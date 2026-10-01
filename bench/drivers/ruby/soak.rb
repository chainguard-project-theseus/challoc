# bench/ruby/soak.rb: a Sidekiq-shaped Ruby process for the Ruby soak
# (bench/drivers/ruby-soak.sh runs it).
#
# THREADS worker threads (the GVL serialises them, which is the point: many
# mostly idle threads, each with its own allocation history in the C
# allocator's thread cache) each loop over "jobs": a burst of short-lived
# strings (1..4 KiB, so they reach malloc rather than Ruby's slots), arrays
# of 100..400 elements and a few hashes, then an idle sleep of 20..60 ms.
# Every fourth job also appends one string and one array to the thread's
# long-lived set, so the live heap grows slowly for the whole run (tens of
# MiB over 10 minutes at the default rate). The main thread is the
# sampler: every INTERVAL seconds it appends one CSV row with the process
# RSS (/proc/self/status VmRSS and VmHWM), GC.stat counters and, when the
# allocator exports mallctl (challoc, jemalloc), stats.allocated / active /
# resident / retained / mapped after an epoch bump, read through fiddle. It
# forces a full GC every GC_EVERY seconds, runs GC.compact once at
# COMPACT_AT seconds, and ends with one more full GC and a final sample
# ("after the last full GC"), then prints the allocator's own statistics
# (malloc_stats_print, when exported) to stderr. The workload is seeded
# (SEED + thread index), so every allocator sees the same job stream.
#
#   ruby soak.rb DURATION CSV [SEED] [THREADS] [INTERVAL] [GC_EVERY] [COMPACT_AT] [IDLE_THREADS]
#
# IDLE_THREADS (default 0; the "many idle threads" shape)
# adds that many threads that touch the allocator once at start (one small
# burst, so each owns a thread cache and, under per-thread arenas, an arena)
# and then sleep, waking every 5..10 s for one tiny burst (20 strings, 10
# arrays): the shape of a Sidekiq process whose concurrency is far above
# its offered load: with an arena per thread the process's resident set
# grows with the thread count. The workers and the sampler are
# unchanged; the summary line gains `idle_threads=`.
#
# CSV columns: t_s, event, vmrss_kb, vmhwm_kb, heap_pages, heap_live_slots,
# gc_count, major, minor, malloc_increase, oldmalloc_increase, jobs,
# live_objects, alloc_allocated, alloc_active, alloc_resident,
# alloc_retained, alloc_mapped (the alloc_* columns are "-" without mallctl),
# cpu_s (the process's user+system CPU seconds so far, Process.times: the
# cost column of a variant), trimmed (samples so far at which malloc_trim(0)
# purged something; SOAK_TRIM=1 calls it through fiddle right before every
# sample, the attribution of dirty-but-unpurged memory: RSS then reads
# Active plus metadata; 0 when not requested or not exported),
# cgroup_peak_kb (the cgroup's memory.peak since
# the last reset, read through one file description held for the run; the
# 6.12 reset is scoped to the writer's description, so the same descriptor
# writes the reset at start and right after the GC.compact sample and reads
# every value; "-" where the write is not accepted: a read-only cgroup
# tree, a kernel before 6.12, the nsdelegate boundary; ruby-soak.sh passes
# the reason on). The last line on stdout is a key=value summary
# ruby-soak.sh parses; it carries cgroup_peak=yes|no(reason),
# cgroup_peak_before_compact_kb and cgroup_peak_after_compact_kb.

require "etc"
duration   = (ARGV[0] || 600).to_i
csv_path   = ARGV[1] || "soak.csv"
seed       = (ARGV[2] || 4141).to_i
nthreads   = (ARGV[3] || 16).to_i
interval   = (ARGV[4] || 5).to_i
gc_every   = (ARGV[5] || 30).to_i
compact_at = (ARGV[6] || 300).to_i
idle_threads = (ARGV[7] || 0).to_i

def rss_kb
  st = File.read("/proc/self/status")
  [st[/^VmRSS:\s+(\d+)/, 1].to_i, st[/^VmHWM:\s+(\d+)/, 1].to_i]
end

# ---- the cgroup's memory.peak, reset per phase ----
# One descriptor for the run: the reset (a write) is scoped to it, the
# reads go through it (pread at 0). The walk is scripts/cgroup-tests.sh's.
def cgroup_peak_path
  mount = File.readlines("/proc/self/mountinfo").grep(/ - cgroup2 /).map { |l| l.split[4] }.first
  return nil unless mount
  path = File.read("/proc/self/cgroup")[/^0::(.*)$/, 1]
  return nil unless path
  path = "" if path == "/"
  "#{mount}#{path}/memory.peak"
end
$cgpeak = nil
$cgpeak_reason = nil
begin
  cp = cgroup_peak_path
  if cp.nil?
    $cgpeak_reason = "no cgroup2 memory controller visible"
  else
    f = File.open(cp, "r+")
    begin
      f.syswrite("reset")
      $cgpeak = f
    rescue Errno::EINVAL
      $cgpeak_reason = "the kernel #{File.read("/proc/sys/kernel/osrelease").strip} does not accept a write to memory.peak (the reset is 6.12+)"
      f.close
    rescue Errno::EPERM, Errno::EACCES
      $cgpeak_reason = "the write to #{cp} is refused from inside the cgroup namespace (nsdelegate): pass CGROUP_PEAK=1 to make"
      f.close
    end
  end
rescue Errno::EROFS
  $cgpeak_reason = "the container's cgroup tree is read-only (Docker's default): pass CGROUP_PEAK=1 to make"
rescue SystemCallError => e
  $cgpeak_reason = "memory.peak: #{e.message}"
end
def cgroup_peak_kb
  return "-" unless $cgpeak
  $cgpeak.pread(64, 0).to_i / 1024
end
def cgroup_peak_reset
  $cgpeak&.syswrite("reset")
end
$stderr.puts($cgpeak ? "cgroup memory.peak: #{cgroup_peak_path} accepts a reset; the CSV's cgroup_peak_kb is the cgroup's peak since the last reset (start, then right after GC.compact)" : "cgroup memory.peak: not reset per phase, VmHWM only: #{$cgpeak_reason}")

# ---- allocator stats through mallctl, when the allocator has one ----------
mallctl = nil
begin
  require "fiddle"
  sym = nil
  %w[mallctl je_mallctl].each do |name|
    begin
      sym = Fiddle::Handle::DEFAULT[name]
      break
    rescue Fiddle::DLError
      sym = nil
    end
  end
  if sym
    mallctl = Fiddle::Function.new(sym,
      [Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_SIZE_T],
      Fiddle::TYPE_INT)
  end
  # SOAK_TRIM=1 (ruby-soak.sh TRIM=1): malloc_trim(0) right before every
  # sample, so the sample reads what is Active plus metadata and nothing
  # the decay has not got to yet (the attribution of dirty inventory).
  trim = nil
  if ENV["SOAK_TRIM"] == "1"
    begin
      trim = Fiddle::Function.new(Fiddle::Handle::DEFAULT["malloc_trim"], [Fiddle::TYPE_SIZE_T], Fiddle::TYPE_INT)
    rescue Fiddle::DLError
      trim = nil
    end
  end
  stats_print = nil
  %w[malloc_stats_print je_malloc_stats_print].each do |name|
    begin
      stats_print = Fiddle::Function.new(Fiddle::Handle::DEFAULT[name],
        [Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP], Fiddle::TYPE_VOID)
      break
    rescue Fiddle::DLError
      stats_print = nil
    end
  end
rescue LoadError
  mallctl = nil
  stats_print = nil
  trim = nil
end
$stderr.puts "malloc_trim before every sample: #{trim ? 'yes' : (ENV['SOAK_TRIM'] == '1' ? 'requested, symbol missing' : 'no')}"
# The buffers are allocated once, so the sampler itself adds nothing to the
# allocator's churn beyond the CSV line.
ALLOC_KEYS = %w[stats.allocated stats.active stats.resident stats.retained stats.mapped]
if mallctl
  $u64 = Fiddle::Pointer.malloc(8, Fiddle::RUBY_FREE)
  $len = Fiddle::Pointer.malloc(8, Fiddle::RUBY_FREE)
  $epoch = Fiddle::Pointer.malloc(8, Fiddle::RUBY_FREE)
  $epoch[0, 8] = [1].pack("Q")
end
def alloc_stats(mallctl)
  return ["-"] * ALLOC_KEYS.size unless mallctl
  $len[0, 8] = [8].pack("Q")
  mallctl.call("epoch", nil, nil, $epoch, 8)
  ALLOC_KEYS.map do |k|
    $len[0, 8] = [8].pack("Q")
    rc = mallctl.call(k, $u64, $len, nil, 0)
    rc == 0 ? $u64[0, 8].unpack1("Q") : "-"
  end
end

# ---- workers ----------------------------------------------------------------
$jobs = Array.new(nthreads, 0)
$stop = false
live_sets = Array.new(nthreads) { [] }
workers = nthreads.times.map do |i|
  Thread.new(i) do |ti|
    rng = Random.new(seed + ti)
    mine = live_sets[ti]
    until $stop
      # One job: the short-lived burst. 300 strings of 1..4 KiB (malloc'ed
      # buffers), 150 arrays of 100..400 Integers (800..3200-byte buffers),
      # 40 hashes of 16 entries (st_tables), a join and a sort over them.
      strs = Array.new(300) { "s" * (1024 + rng.rand(3072)) }
      arrs = Array.new(150) { Array.new(100 + rng.rand(300)) { rng.rand(1 << 20) } }
      hs = Array.new(40) { |k| h = {}; 16.times { |j| h["k#{k}-#{j}"] = strs[(k + j) % 300] }; h }
      joined = strs.first(20).join(",")
      sorted = arrs.first(10).map(&:sort)
      # The long-lived growth: one 1..2 KiB string and one 50..100-element
      # array per four jobs survive to the end of the run.
      if ($jobs[ti] % 4).zero?
        mine << ("l" * (1024 + rng.rand(1024)))
        mine << Array.new(50 + rng.rand(50)) { rng.rand(1 << 16) }
      end
      strs = arrs = hs = joined = sorted = nil
      $jobs[ti] += 1
      sleep(0.020 + rng.rand * 0.040)
    end
  end
end

# The idle threads: one burst at start, then a tiny burst every 5..10 s.
$idle_bursts = 0
idlers = idle_threads.times.map do |i|
  Thread.new(i) do |ti|
    rng = Random.new(seed + 100_000 + ti)
    first = true
    until $stop
      strs = Array.new(first ? 300 : 20) { "i" * (1024 + rng.rand(3072)) }
      arrs = Array.new(first ? 150 : 10) { Array.new(100 + rng.rand(300)) { rng.rand(1 << 20) } }
      strs = arrs = nil
      $idle_bursts += 1
      first = false
      slept = 0.0
      while slept < 5.0 + rng.rand * 5.0 && !$stop
        sleep 0.25
        slept += 0.25
      end
    end
  end
end

# ---- sampler -----------------------------------------------------------------
csv = File.open(csv_path, "w")
csv.puts "t_s,event,vmrss_kb,vmhwm_kb,heap_pages,heap_live_slots,gc_count,major,minor,malloc_increase,oldmalloc_increase,jobs,live_objects,alloc_allocated,alloc_active,alloc_resident,alloc_retained,alloc_mapped,cgroup_peak_kb,cpu_s,trimmed"
csv.sync = true
t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
elapsed = -> { Process.clock_gettime(Process::CLOCK_MONOTONIC) - t0 }
$trimmed = 0
def cpu_s
  t = Process.times
  (t.utime + t.stime).round(2)
end
sample = lambda do |event|
  if trim
    $trimmed += trim.call(0)
  end
  rss, hwm = rss_kb
  st = GC.stat
  live = live_sets.sum(&:size)
  row = [elapsed.call.round, event, rss, hwm, st[:heap_allocated_pages], st[:heap_live_slots],
         st[:count], st[:major_gc_count], st[:minor_gc_count],
         st[:malloc_increase_bytes], st[:oldmalloc_increase_bytes], $jobs.sum, live] + alloc_stats(mallctl) + [cgroup_peak_kb, cpu_s, $trimmed]
  csv.puts row.join(",")
end
$cgpeak_before_compact = "-"

sample.call("start")
next_sample = interval
next_gc = gc_every
compacted = false
while (t = elapsed.call) < duration
  sleep([next_sample - t, 0.05].max)
  t = elapsed.call
  next if t < next_sample
  event = "sample"
  if !compacted && t >= compact_at
    GC.compact
    compacted = true
    event = "compact"
    # The phase boundary: the sample below records the start..compact peak,
    # then the peak starts again for compact..end.
    sample.call(event)
    $cgpeak_before_compact = cgroup_peak_kb
    cgroup_peak_reset
    next_sample += interval
    next
  elsif t >= next_gc
    GC.start(full_mark: true, immediate_sweep: true)
    next_gc += gc_every
    event = "fullgc"
  end
  sample.call(event)
  next_sample += interval
end
$stop = true
workers.each(&:join)
idlers.each(&:join)
GC.start(full_mark: true, immediate_sweep: true)
sample.call("final-fullgc")
rss, hwm = rss_kb
st = GC.stat
al = alloc_stats(mallctl)
cgpeak_after = cgroup_peak_kb
csv.close
# The allocator's own report (per-class slabs and regions, decay state) to
# stderr; ruby-soak.sh keeps it beside the CSV.
if stats_print
  $stderr.puts "=== malloc_stats_print at the end (t=#{elapsed.call.round}) ==="
  $stderr.flush
  stats_print.call(nil, nil, nil)
end
puts "jobs=#{$jobs.sum} live_objects=#{live_sets.sum(&:size)} gc_count=#{st[:count]} major=#{st[:major_gc_count]} " \
     "minor=#{st[:minor_gc_count]} heap_pages=#{st[:heap_allocated_pages]} rss_final_kb=#{rss} hwm_kb=#{hwm} " \
     "mallctl=#{mallctl ? 'yes' : 'no'} alloc_allocated=#{al[0]} alloc_active=#{al[1]} alloc_resident=#{al[2]} " \
     "alloc_retained=#{al[3]} alloc_mapped=#{al[4]} compacted=#{compacted} " \
     "threads=#{nthreads} idle_threads=#{idle_threads} idle_bursts=#{$idle_bursts} nproc=#{Etc.nprocessors} " \
     "cpu_s=#{cpu_s} trim=#{trim ? 'yes' : 'no'} trimmed=#{$trimmed} " \
     "cgroup_peak=#{$cgpeak ? 'yes' : "no(#{$cgpeak_reason.to_s.tr(' ', '_')})"} " \
     "cgroup_peak_before_compact_kb=#{$cgpeak_before_compact} cgroup_peak_after_compact_kb=#{cgpeak_after}"
