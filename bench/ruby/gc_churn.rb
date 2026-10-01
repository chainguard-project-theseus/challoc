# bench/ruby/gc_churn.rb: Ruby's allocation shape as the C allocator sees it
# (the ruby-gc-churn row of bench/rows.toml).
#
# Ruby 3.4 keeps objects up to 640 bytes inside its own mmap'ed heap pages
# (variable-width slots), so what reaches malloc is the buffers that do not
# fit a slot: strings over ~600 bytes, arrays over ~78 elements, st_tables
# (hashes over 8 entries), plus the interpreter's own structures. The GC frees
# those buffers in bursts during sweep. This script builds a long-lived set of
# such medium objects, runs ROUNDS bursts of short-lived ones, then forces
# FORCED full GC cycles and prints GC.stat counters and the process RSS before
# and after the forced cycles (Ruby's GC calls malloc_trim after a full GC in
# the glibc build; the jemalloc build does not). Output is one key=value line
# ruby-bench.sh parses.
rounds = (ARGV[0] || 10).to_i
forced = (ARGV[1] || 10).to_i

def rss_kb
  st = File.read("/proc/self/status")
  [st[/^VmRSS:\s+(\d+)/, 1].to_i, st[/^VmHWM:\s+(\d+)/, 1].to_i]
end

# Long-lived medium objects: 20k strings of 1..4 KiB and 20k hashes of 12
# entries, about 50 MiB of malloc'ed buffers that survive every GC.
live_strings = Array.new(20_000) { |i| "x" * (1024 + (i * 613) % 3072) }
live_hashes = Array.new(20_000) { |i| h = {}; 12.times { |k| h[k] = i + k }; h }

rounds.times do
  # A burst of short-lived buffers: 100-element arrays (800-byte buffers),
  # 1 KiB strings and 10-entry hashes; all garbage at the end of the round.
  tmp_a = Array.new(100_000) { |i| Array.new(100, i) }
  tmp_s = Array.new(50_000) { "y" * 1024 }
  tmp_h = Array.new(50_000) { |i| h = {}; 10.times { |k| h[k] = i }; h }
  tmp_a = tmp_s = tmp_h = nil
end

st0 = GC.stat
rss0, hwm0 = rss_kb
forced.times { GC.start(full_mark: true, immediate_sweep: true) }
rss1, hwm1 = rss_kb
st1 = GC.stat

puts "gc_count=#{st1[:count]} major=#{st1[:major_gc_count]} minor=#{st1[:minor_gc_count]} " \
     "forced=#{st1[:count] - st0[:count]} " \
     "allocated_objects=#{st1[:total_allocated_objects]} freed_objects=#{st1[:total_freed_objects]} " \
     "heap_live_slots=#{st1[:heap_live_slots]} heap_pages=#{st1[:heap_allocated_pages]} " \
     "malloc_increase_bytes=#{st1[:malloc_increase_bytes]} oldmalloc_increase_bytes=#{st1[:oldmalloc_increase_bytes]} " \
     "rss_before_kb=#{rss0} rss_after_kb=#{rss1} hwm_kb=#{hwm1}"
# Keep the long-lived set reachable to the end.
exit(live_strings.size + live_hashes.size == 40_000 ? 0 : 1)
