# challoc tuning: the operator's guide

This is the operator's view of challoc's tunables and of its cgroup and
THP behaviour. It describes the code at commit 483ec7c. `src/config.rs`
(`KEYS`, `Config::DEFAULT`, `Config::set`) is the source of truth for the
key list, the defaults and the value grammar; design section 12
(`docs/design.md`) is the design's table. Where a sentence here cites a
number, the number comes from the results file named next to it in
`bench/results/`.

Rule of thumb first: the defaults are the measured configuration. Every
default below was either kept because the alternative lost a recorded
row, or was decided by the human with the numbers in front of them
(PROGRESS.md, "Phase 2.5 defaults, the human's decisions (2026-09-20)").
Section 7 lists the keys not to touch and why.

## 1. Setting keys

challoc reads its configuration once, at first use, from these sources
in increasing priority (`config::load`); a later source overrides an
earlier one key by key:

| order | source | notes |
|---|---|---|
| 1 | compiled defaults | `Config::DEFAULT` |
| 2 | the `malloc_conf` global | the library defines a null `malloc_conf` slot; a program that defines its own `const char *malloc_conf` interposes it, jemalloc style (Ruby's `--with-jemalloc` build reads `malloc_conf` through the header) |
| 3 | the `je_malloc_conf` global | weak; Redis and Valkey define it |
| 4 | `/etc/challoc.conf` | first line only, up to 4096 bytes, a trailing `\r` stripped; read with raw syscalls. The file is `challoc.conf`, not jemalloc's `/etc/malloc.conf` symlink |
| 5 | `MALLOC_CONF` | environment; read from `environ`, never through `getenv` |
| 6 | `CHALLOC_CONF` | environment, after `MALLOC_CONF`, so it wins over a `MALLOC_CONF` written for jemalloc |

The two environment variables are skipped when the process is
`AT_SECURE` (setuid, setgid or capability-elevated), glibc's
`secure_getenv` rule. The two globals and the file are always read.

Format: jemalloc's `key:value,key:value`. A value may be wrapped in
double quotes (and may then contain commas). Empty pairs (`a,,b`, a
trailing comma) are ignored. Booleans are `true` and `false` only.
Integers are decimal, `0x` hexadecimal or leading-zero octal, with an
optional sign. An out-of-range integer is clamped to the key's range and
reported. `guard_slab_interval` must be a power of two: anything else is
rounded up and reported.

Reporting: every problem is printed on stderr as `challoc: <source>:
<issue>` (`unknown conf key "x"`, `invalid conf value "x" for k`,
`out-of-range conf value "x" for k (clamped)`, `conf value "x" for k is
not a power of two (rounded up to N)`, `conf key k is accepted but has no
effect (not implemented)`, `conf value "x" for spin_hint is not available
on this CPU; using the default`) and the offending pair is skipped
(clamped or rounded for the numeric cases); parsing continues.

`abort_conf:true` turns the report into an abort. It is evaluated after
every source has been read, so it covers keys that came before it in the
same or an earlier source, and it refuses: unknown keys, bad values,
out-of-range and rounded numbers, inert keys (below), and a `spin_hint`
this CPU cannot run. It does not refuse a runtime probe that fails for a
valid key: `slice_ext:on` on a kernel without the extension,
`percpu_arena:percpu` without `mm_cid`, `thp_process:advised` before
6.18, `thp:auto` on a host with no sub-PMD folio size, `thp:always` under
a cgroup limit below 1 GiB. Those are the documented fallbacks, and the
`malloc_stats_print` report says which one applies (section 6). Use
`abort_conf:true` in a package test: `MALLOC_CONF=abort_conf:true,no_such_key:1
ruby -e 1` aborts with challoc's message
(`bench/results/2026-09-20-phase3-task3-ruby-package.md`).

Inert keys: `stats_print`. It parses as a boolean and is reported as
inert every time it is set, so a program cannot believe it turned
something on; nothing prints at exit. `malloc_stats_print` lists it as
`opt.stats_print: accepted, no effect`, and `mallctl("opt.stats_print")`
returns `ENOENT`. To get the report at exit from a program that cannot
call `malloc_stats_print` itself, preload a hook (the benchmark harness
uses `bench/probes/statsdump.so`, an `atexit` handler that `dlsym`s
`malloc_stats_print`).

Ignored jemalloc keys, accepted with any value and never validated:
`junk`, `zero`, `retain`, `oversize_threshold`, `metadata_thp`,
`confirm_conf`. They exist so a `MALLOC_CONF` written for jemalloc parses
clean under `abort_conf`. `opt.<key>` returns `ENOENT` for them.

Reading back: `mallctl("opt.<key>")` returns the value in force for
every key of the table below (a bool, an integer, or a string for the
enumerated keys). `malloc_stats_print` prints them all under `Run-time
option settings` as `opt.<key>: <value>`. Two annotations can follow a
value: `(accepted, no effect)` for an inert key, and for `thp`
`(accepted, no effect; forced to never by a cgroup limit under 1 GiB)`
when section 4's rule applied.

At run time, through `mallctl`: `background_thread` (a write of `true`
creates the thread synchronously; Valkey does this at startup),
`arena.<i>.dirty_decay_ms`, `arena.<i>.purge` and `arena.<i>.decay`,
`thread.tcache.enabled`, `thread.tcache.flush`, and `malloc_trim(0)`
(every arena purged, eagerly). One gate is read only at init:
`prefault` is active only if `background_thread:true` was in the
configuration; a thread started later through `mallctl` does not turn
the prefault pass on (section 3, Valkey).

## 2. The keys

One row per key, in `KEYS` order. "Values" is the grammar
`Config::set` accepts; ranges are inclusive. "Default" is
`Config::DEFAULT`. The reasons behind the defaults follow the table.

| key | values | default | what it does | change it when | cost or risk |
|---|---|---|---|---|---|
| `background_thread` | `true`, `false` | `false` | Runs decay (and the PSI trigger, and the prefault pass) on a background thread instead of only from allocation slow paths. Created on the first periodic tick after the request, or synchronously by `mallctl("background_thread")`. | The process goes idle for long stretches with freed memory still dirty: without the thread an idle process never purges (as jemalloc). Valkey turns it on itself. | One thread, one futex or `ppoll` sleep; its stack is one metadata reservation. On the Ruby soak it changed nothing (-0.7..+0.3% RSS on every row, `bench/results/2026-09-19-aarch64-ruby-soak.md`). |
| `prefault` | `true`, `false` | `false` | With the background thread, commits and populates (`MADV_POPULATE_WRITE`, 5.14+, probed; else a touch per page) the next fresh slab of every class that refilled since the last pass, one pipeline per `(arena, class)`, only while the cgroup policy shows headroom, so the allocating thread's first write into a fresh slab takes no page fault. Inert without `background_thread:true` (the report says `Prefault: off (prefault:true without background_thread)`). | A latency-sensitive main thread fills a large heap in bursts (Valkey's shape). See section 3. | RSS: one pipeline of resident slabs per refilling `(arena, class)`: +4.2..4.6 MiB (+0.7..0.8%) at Valkey's 580 MiB peak, +4..6 MiB on a 16 MiB larson heap, +25..34 MiB over ten arenas on larson10 (`bench/results/2026-09-20-aarch64-task11-prefault-claim.md`, section 8). |
| `dirty_decay_ms` | `-1`..`1000000000000` | `10000` | The window of the decay curve: freed slabs and huge extents go from dirty to purged over this many milliseconds along a smootherstep (about half by mid-window, all by the end). `-1` never purges. `0` purges at the next pass. | Lower it (1000..5000) when RSS after a burst matters more than refaults; raise it or set `-1` for a process that reuses everything it frees and never idles. | Purging costs `madvise` calls and refaults on reuse; never purging keeps every peak resident. Under a cgroup limit the policy scales this value down (section 4); `-1` is left alone in every band. |
| `muzzy_decay_ms` | `-1`..`1000000000000` | `0` | Accepted for jemalloc compatibility, stored, readable as `opt.muzzy_decay_ms`; nothing reads it. There is no muzzy state: the two-advice purge (section 4) replaces it. | Never. | None. |
| `narenas` | `0`..`4294967295` | `0` | Automatic arenas. `0` means `min(4 * ncpus, 64)` (`ncpus` is the affinity mask's width). Each heap is bound to the least-loaded automatic arena, so a program with `t` threads touches `min(t, narenas)` arenas; each arena has its own partial slabs per class. | A mostly idle many-thread process holds too much RSS in per-arena slab sets: `narenas:1` on the 16-thread Ruby soak read 170.9 MiB peak against 401 default (jemalloc 200.8) and 35.9 MiB against 279 after the first full GC (`2026-09-19-aarch64-ruby-soak.md`). | Fewer arenas means more threads share each arena's class locks on the slow path (refill and flush); the fast paths are unaffected. Not measured under contention here. |
| `percpu_arena` | `auto`, `percpu`, `off` (`phycpu` = `percpu`, `disabled` = `off`) | `auto` | Per-CPU caches and arenas keyed by the rseq `mm_cid` of the running thread instead of by thread (design 15). `auto` is thread caches (`heap::PercpuState::OffAuto`, "opt in with `percpu_arena:percpu`"). `percpu` turns the mode on where the probe passes (glibc's registered `__rseq_size >= 28`, kernel 6.3+); where it fails the report notes the request and thread caches stay. `off` is thread caches whatever the probe says. `double_free_scan:true` also keeps thread caches. | A process with far more threads than CPUs whose RSS is dominated by per-thread caches, and after measuring. | Against thread caches the mode costs larson1 +5.3%, xmalloc +18.9%, valkey1 +4.7% on aarch64, xmalloc +16.1% and valkey1 +37.8% on x86_64 (`bench/results/2026-09-19-{aarch64,x86_64}-task8-percpu-fastpath-ab.md`). |
| `tcache` | `true`, `false` | `true` | Thread caches on the `malloc`/`free` fast paths. `false` sends every operation to the slabs under the class lock. | Never in production; a diagnostic. | Every operation becomes a slow path. |
| `tcache_max` | `0`..`262144` | `32768` | Largest class size (bytes) whose slots are cached (`opt.tcache_max` reads as a `size_t`, like jemalloc's); classes above it are served from arena 0's slabs directly (shared classes), so one partial slab per class serves every thread. | Never. | Set from the RSS acceptance probe: 8 cached 40 KiB slots per `(thread, arena)` held 70 MiB of xmalloc-test's RSS (`bench/results/2026-09-18-aarch64-gce-arena-rss.md`). |
| `tcache_nslots_small_max` | `8`..`65536` | `1024` | Cache capacity for classes up to 128 bytes; the capacity halves per doubling of the class size above that (the ladder), down to a floor of 8. | To trade cache RSS for refill frequency: `512` halves every cache. | More refills and flushes (slow paths) per operation. |
| `lg_tcache_nslots_mul` | `-16`..`16` | `1` | Shifts the whole ladder by `lg_tcache_nslots_mul - 1` doublings (jemalloc's key; Redis's `je_malloc_conf` sets `3`). | As `tcache_nslots_small_max`. | As above; the result is clamped to `[8, tcache_nslots_small_max]`. |
| `tcache_gc_interval_ms` | `0`..`1000000000000` | `10000` | Period of the cache GC (trimming idle caches), driven from the periodic tick. Above 90% of a cgroup limit the GC runs at every tick regardless. | Never. | None measured. |
| `tcache_compact_shift` | `0`..`20` | `3` | Every `2^shift` pops of a class are served from the lowest-addressed cached slot instead of the top of the stack, so long-lived objects gather in low slabs and the high slabs empty (design 6.4). `0` turns the schedule off. | Never. | The compaction the defragmentation story and the `churn` row rely on. `slot_randomize:true` fights it. |
| `thp` | `never` (`default` is `never`), `auto`, `always` | `never` | Transparent huge pages for challoc's regions. `never`: `MADV_NOHUGEPAGE` on the class superblock and the metadata region; the huge region (allocations above the largest class) follows the host's policy. `auto`: `MADV_HUGEPAGE` for sub-PMD folios (64 KiB) on slab groups and metadata, never a 2 MiB page; inert where no such size is enabled or the 2 MiB size is `madvise`-gated. `always`: 2 MiB chunks over the class regions, huge runs and metadata (jemalloc's `thp:always`); forced to `never` under a cgroup limit below 1 GiB. Section 5. | `always` on a THP=`always` host when throughput matters more than RSS and the process does not fork. `auto` on an aarch64 host with `hugepages-64kB` enabled. | `always`: valkey1 at parity with jemalloc for +9.5% peak RSS on a 585 MiB heap and +39.9% on larson16's (`bench/results/2026-09-20-aarch64-cgos-thp-decision.md`); `auto`: larson1 +1.7% and 15.9 -> 18.1 MiB RSS (`2026-09-19-lane-b-mthp.md`). |
| `thp_process` | `default`, `advised` | `default` | `advised` issues `prctl(PR_SET_THP_DISABLE, 1, PR_THP_DISABLE_EXCEPT_ADVISED)` once at init (kernel 6.18+, probed by the call's return), so the whole process gets huge pages only where a range was `MADV_HUGEPAGE`d; `default` issues nothing. Independent of `thp`. | "No huge page anywhere in this process" on an `always` host, including libc's and the program's own mappings, which `thp:never` does not cover. | Process-wide, hence opt-in. On the Valkey model it takes the huge runs' 2 MiB pages away too: +12.2% behind jemalloc on valkey1 against +5.1% for the default (`2026-09-20-aarch64-cgos-thp-ab.md` row (d) and `-thp-decision.md`). |
| `spin_hint` | `auto`, `baseline`, `umwait`, `wfe` | `auto` | What a contended lock spins with. `auto` is the CPU probe's choice: `umwait` where WAITPKG is exposed, else the baseline (`pause`/`isb`). `umwait` without WAITPKG and `wfe` without the timer event stream are refused (reported, an `abort_conf` issue) and left at `auto`. `auto` never picks `wfe`. | Never in production. `wfe` for a power-saving experiment. | `wfe` lost xmalloc by 3.5..3.6% with RSS 55.7 -> 66.9 MiB on the T2A (`bench/results/2026-09-19-lane-c-spin-and-geometry.md`). No GCE guest exposes WAITPKG, so `umwait` has never run. |
| `slice_ext` | `auto`, `on`, `off` | `auto` | A lock holder asks the kernel, through the rseq `slice_ctrl` bytes, to defer a preemption until the release (kernel 7.0 with `CONFIG_RSEQ_SLICE_EXTENSION`, x86_64 only so far, probed). `auto` and `on` follow the probe; `on` additionally makes the report note a refusal; `off` never requests. | Never; `off` to rule it out when comparing kernels. | One relaxed load per hold when off. The reference GCE kernels compile the option out (`slice_ext=kernel-disabled`). |
| `lazy_free` | `true`, `false` | `false` | `MADV_FREE` for every purge, in every band, explicit purges included: the override of the two-advice purge (section 4). | Never in a container that watches RSS: lazily freed pages stay resident and charged until the kernel reclaims them. | RSS and `memory.current` do not fall after a free; Valkey's `allocator_frag_ratio` and autoscalers read the stale figure. |
| `lazy_band` | `true`, `false` | `true` | `MADV_FREE` for purges made under 70% of a known cgroup limit (the headroom band); `false` makes every purge `MADV_DONTNEED` unless `lazy_free`. | `false` when the container's RSS figure must track live memory exactly even with headroom (an autoscaler, a fragmentation ratio). | Refaults on reuse: allocating 200 MiB again after a lazy purge took 5 ms against 44 ms after an eager one on the C4A (`bench/results/2026-09-19-lane-b-two-advice-purge.md`). |
| `guards` | `true`, `false` | `true` | Guard slabs between slabs (kernel 6.13+, `MADV_GUARD_INSTALL`) and a guard after every huge run (a `PROT_NONE` page on older kernels). | Never. | `false` removes an overflow barrier and makes the sized-free guard test a no-op. |
| `guard_slab_interval` | `0`, or `2`..`4194304`, a power of two | `8` | One guard slab per N x 64 KiB of slabs (every Nth slab for the 64 KiB-slab classes; the same span in bytes for the classes with smaller slabs). `0` is none; `1` is out of range and clamps to `2`; a non-power of two rounds up and is reported. | To trade address space and slab count for guard density. | A denser interval costs slabs; it never costs a fast-path instruction (the test is a mask). No guard slabs exist before kernel 6.13 whatever the value. |
| `huge_retain_max` | `0`..`1099511627776` | `67108864` | Freed huge runs (allocations above the largest class) with more usable bytes than this (`opt.huge_retain_max` reads as a `size_t`) are purged the moment they are freed instead of waiting for decay; smaller ones are retained for reuse and decay with `dirty_decay_ms`. | A process churns large buffers and their retention shows in RSS between bursts: Node's 1 MiB `Buffer.allocUnsafe` bodies held 62 MiB of `challoc:huge` at the end of the load (`bench/results/2026-09-20-x86_64-node.md`). Not measured with a lower value. | A lower value refaults every reused large buffer. |
| `huge_dirty_decay_ms` | `-1`..`1000000000000` | follows `dirty_decay_ms` (`opt.huge_dirty_decay_ms` reads `10000`) | The huge tier's own decay window (design 7): freed huge runs at or under `huge_retain_max` go from dirty to purged over this many milliseconds on their own curve, while the slabs keep `dirty_decay_ms`. Unset it follows arena 0's period (and `arena.0.dirty_decay_ms` moves both). `0` purges a huge run the moment it is freed, whatever its size; `-1` never purges one by decay (`malloc_trim(0)` still does). The report's `Huge:` line names the period when it differs. | A process churns buffers above the largest class (256 KiB) in bursts and holds them through its lulls: the JVM's direct `ByteBuffer`s (section 3, JVM) kept 222 MiB of empty huge extents inside the 10 s window at the end of every lull of the mixed-size soak; the measured table is in the JVM section. | A shorter window refaults every reused huge run, one `madvise` and `n` page faults per run; the slab classes are untouched, so the Valkey guard rows (same file, the soak at `0`) read unchanged on x86_64 and within one run's spread on aarch64. |
| `zero_on_free` | `true`, `false` | `false` | Clears a freed block (a slot's class-size bytes, a huge run's usable bytes) before it is cached or released. | A hardening requirement that outweighs the cost. | A `memset` per free: the Valkey model +84% (aarch64), larson1 +145%, larson10 +237%, redis +11.2% (`bench/results/2026-09-18-aarch64-gce-tunables.md`). |
| `pac_pointers` | `true`, `false` | `false` | On arm64 with pointer authentication (`pac=yes` in the feature line: Axion, Apple silicon; not Altra, not x86_64), signs every cached slot pointer and the TLS heap pointer with `pacda` and authenticates on pop, so a metadata write cannot make `malloc` return a chosen address. Inert elsewhere. | A hardening requirement on a host with `pac=yes`. | xmalloc-test's cross-thread frees +11.0..15.8%, larson +1.2..1.9% on Axion (`bench/results/2026-09-19-lane-a-pac-pointers.md`). |
| `slot_randomize` | `true`, `false` | `false` | Cache refills claim from a random bitmap word and shuffle the batch, so slot order is not predictable from previous allocations. Slow paths only. | A hardening requirement. | Works against `tcache_compact_shift`'s compaction: +7..12% RSS on larson (`2026-09-18-aarch64-gce-tunables.md`, section 6). |
| `double_free_scan` | `true`, `false` | `false` | Every free compares the slot against the four most recently cached entries and aborts on a match, catching the one double-free window the default leaves (a second free while the slot is still cached, popped before a flush). Also keeps thread caches (`percpu_arena` off). | Debugging a suspected double free. | valkey1 14.828 -> 13.004 M ops/s on x86_64 (`bench/results/2026-09-18-x86_64-gce-detection-gaps.md`). |
| `abort_conf` | `true`, `false` | `false` | Abort after parsing if any source reported a problem (section 1). | Package tests and any deployment that must not run on a misspelled key. | None at run time. |
| `cgroup` | `true`, `false` | `true` | Read the memory cgroup at init and sample it: effective limits, `memory.current`, `memory.events`, the PSI trigger; apply the policy of section 4. `false` reads nothing, applies nothing, reports `cgroup: not tracked (cgroup:false)`. | `false` only as the control in a measurement. | With `cgroup:false` the container churn test keeps 319 MiB of a freed 400 MiB at 2 s where the default is at 13 MiB within 50 ms (`bench/results/2026-09-18-aarch64-gce-cgroup-container-tests.md`). |
| `check_zero` | `true`, `false` | `false` | Debug: every slot or page handed out as known zero is read at its first and last word under the class or huge lock; a non-zero word aborts with the slab's prior state. Slow paths only. | Diagnosing a `calloc` zero bug. | Extra reads on every refill from `Fresh` or `Purged` memory. |
| `group_handoff` | `true`, `false` | `false` | An arena whose slab group of a class is used up takes an idle group of that class from another arena before committing a fresh one. Slow paths only. | A many-arena process whose RSS is idle groups: it takes a fifth off xmalloc-test's RSS. | It moved nothing on the 16-arena mstress row it was built for and cost that row 2.4% of RSS on aarch64 (`bench/results/2026-09-18-aarch64-gce-group-handoff.md`). |

### Why the defaults are what they are

The three decided by the human on 2026-09-20 (PROGRESS.md, "Phase 2.5
defaults, the human's decisions"):

- `thp:never`, in its current form (option (d) of
  `bench/results/2026-09-20-aarch64-cgos-thp-ab.md`). Before that day
  `never` also advised the huge region, and on Chainguard OS (THP
  `always`) that read valkey1 +16.1% behind jemalloc; letting the huge
  region follow the host brought the 32 MiB of huge runs back onto 2 MiB
  pages for no RSS change and the row to +5.1% (the re-measurement,
  `2026-09-20-aarch64-cgos-thp-decision.md`). `thp:always` would buy
  parity (-0.2%) for +9.5% RSS on that heap and +39.9% on larson16's. RSS
  first is the Valkey soak's criterion, so `never` stays. Section 5.
- `percpu_arena:auto` = thread caches. The mode costs 4..38% on the
  `ab` rows (table above); no row in the suite shows a win yet. The mode
  stays available through `percpu_arena:percpu`.
- `prefault:false`. The mechanism works (main-thread page faults on the
  Valkey soak's write phase -83% on both architectures, p99 unmoved at
  the tools' resolution) but a default `prefault:true` is also a default
  background thread for every process and +4..6 MiB on a 16 MiB heap, so
  it is documented as the Valkey opt-in instead (section 3;
  `bench/results/2026-09-20-aarch64-task11-prefault-claim.md`, section 8).

The rest are measured refusals: `zero_on_free`, `slot_randomize`,
`pac_pointers`, `double_free_scan` are hardening opt-ins whose price is
over the 3% budget for a default-on hardening (rows in the table);
`group_handoff` did not move its target row; `spin_hint:auto` never
chooses `wfe`; `tcache_max` is the RSS probe's finding; `lazy_band:true`
with no-limit eager is the two-advice purge, chosen so RSS never lies
where nothing knows the headroom.

## 3. Consumers

### Valkey and Redis

The package (`contrib/wolfi/valkey/valkey-9.1.yaml`, design 13.6)
builds a private `quantum8` challoc into `/usr/lib/valkey/libchalloc.so.0`
and links `valkey-server` to it through `DT_RPATH`. The system
`/usr/lib/libchalloc.so.0` is a `quantum16` build; Valkey's
`allocator_defrag.c` asserts `arenas.quantum == 8` at start, so a server
forced onto the system library (`LD_PRELOAD`) dies in
`allocatorDefragInit` before it logs anything
(`bench/results/2026-09-20-phase3-task2-valkey-package.md`). Do not
"fix" this by pointing `LD_LIBRARY_PATH` at `/usr/lib`; the rpath wins
anyway.

What Valkey sets itself: `jemalloc-bg-thread yes` (the default) calls
`mallctl("background_thread", true)` at startup, so the server has the
decay thread without any key; `MEMORY PURGE` is `arena.<i>.purge`
(eager, whatever the band); `MEMORY MALLOC-STATS` is `malloc_stats_print`
(section 6). Redis defines `je_malloc_conf` with
`lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000`, which challoc
reads as a source (section 1); `MALLOC_CONF` overrides it.

The documented opt-in, for a server that fills a large keyspace in
bursts on a latency-sensitive main thread:

```
MALLOC_CONF=prefault:true,background_thread:true
```

Both keys, because the prefault gate is read at init and Valkey's own
`mallctl` comes later (PROGRESS.md, Phase 2.5 task 11, lane A's note).
Measured on the Valkey soak (write phase 300 s, `-d 512 -r 500000 -P 16`
and memtier SET-only, activedefrag on; `bench/results/2026-09-20-aarch64-lane-b-task11-prefault.md`
section 4 and `2026-09-20-aarch64-task11-prefault-claim.md` sections 6
and 8):

| measure | default | `prefault:true,background_thread:true` |
|---|---:|---:|
| main thread page faults, write phase (both architectures, DURATION 1200) | 157538 | 27047 (-83%) |
| process fault exceptions (`perf page-faults`), write phase | 66172 | 9540 (-86%) |
| valkey-benchmark write req/s (lane B's DURATION 600 run) | 925268 | 924518 (-0.1%) |
| valkey-benchmark write p99 ms | 1.063 | 1.063 |
| peak RSS (VmHWM) | 578.4 MiB | 582.3 MiB (+0.7%) |
| RSS after the settle | 129.3 MiB | 131.4 MiB (+1.6%) |

The faults are the same pages installed by `challoc-bg` ahead of the
refill (the two threads' `minflt` sum within 1.5% of the default's). The
p99 does not move at the tools' resolution because 130 k faults of a few
microseconds spread over 300 s of a 920 k req/s stream are tens of
microseconds per second of CPU. What it costs is the RSS above and the
background thread, which Valkey already runs. The pass does not touch
the huge runs (Valkey's dict tables and client buffers), which is where
the remaining main-thread faults are.

Active defragmentation: `CONFIG SET activedefrag yes` works on the
`quantum8` build (the package test asserts it). challoc answers
`experimental.utilization.batch_query` and `stats.arenas.<i>.bins.<j>.*`
exactly; one difference to know is that `nonfull_slabs` includes the
slab the next claim takes from, where jemalloc's excludes it, so for the
same heap challoc reports one more non-full slab and moves slightly more
(design 13.4). The contrib patch changes one Valkey test threshold
(`memefficiency.tcl`, `ignore_bytes` 5000kb for the Lua script test:
challoc replaces libc `malloc` too, so the Lua heap sits in its bins where
the bundled jemalloc's `je_` prefix kept it invisible). Nothing about
defrag is tunable on the challoc side; the decision rule is Valkey's.

RSS-derived metrics under a cgroup limit: below 70% of the limit purges
are `MADV_FREE` (section 4), so `allocator_resident`, RSS and therefore
`allocator_frag_ratio` include lazily freed pages until the kernel
reclaims them. If those figures drive alerting or an autoscaler, set
`lazy_band:false` (every purge eager, RSS tracks live memory at the cost
of refaults on reuse). With no limit at all the default is already eager.

THP: Valkey warns about THP `always` because of fork copy-on-write cost.
challoc's `never` keeps the class and metadata regions off huge pages on
every host; the huge runs take what the host gives (section 5).
`thp_process:advised` (kernel 6.18+) is the key for no huge page anywhere
in the process, at the throughput cost in the table above.

### Ruby

The package (`contrib/wolfi/ruby/ruby-3.4.yaml`) builds Ruby
`--with-jemalloc` against `challoc-jemalloc-compat-dev` and runs on
`challoc-jemalloc-compat`'s `/usr/lib/libjemalloc.so.2` (a `quantum16`
build with the jemalloc soname). Nothing is set: Ruby's `malloc_conf`
path through the header and `MALLOC_CONF` both work, and the package
test proves the allocator from inside the interpreter through Fiddle
(`bench/results/2026-09-20-phase3-task3-ruby-package.md`).

What was measured for Ruby-shaped processes (the 16-thread soak,
`bench/results/2026-09-19-aarch64-ruby-soak.md`): `background_thread:true`
has no effect (-0.7..+0.3% on every row; the slow-path ticks already
drive the decay). The lever is the automatic arena count: with 17
arenas (one per thread) challoc read 401 MiB peak against jemalloc's
200.8; with `narenas:1` 170.9 MiB peak, 35.9 MiB against jemalloc's 93.0
after the first forced full GC and +4.4% against jemalloc at t=600. This
is a packaging decision not taken (PROGRESS.md, prerequisite (c)): the
package ships the defaults. An operator running a mostly idle
many-thread Ruby (Puma, Sidekiq) who sees RSS well above jemalloc's can
try `MALLOC_CONF=narenas:1` (or a small count) and read `Arenas:` in
the report; the contention cost of one arena was not measured on that
soak.

### JVM

`bench/results/2026-09-20-{aarch64,x86_64}-jvm.md` (Wolfi `openjdk-21`,
`-Xms512m -Xmx512m -XX:+AlwaysPreTouch`, a 200-thread HTTP server for
300 s, challoc against glibc, glibc with `MALLOC_ARENA_MAX=2`, jemalloc,
mimalloc, tcmalloc). Nothing to set; the defaults are the measured
configuration. What the rows found:

- Startup (`java -version`): +9.1% (aarch64) / +7.4% (x86_64) against
  glibc, a 2 ms constant on a 22..27 ms process; attributed to about
  720..1270 more first-touch page faults (one fresh slab page per class
  each of the JVM's 15 startup threads touches, plus a heap page per
  thread) and jemalloc pays the same count. Max RSS 39.1 MiB on aarch64,
  the smallest of the six (glibc 67.1, its per-thread arenas).
- Server: peak RSS +0.2% (aarch64) / -1.4% (x86_64) against glibc and
  -13.8% / -17.0% against jemalloc; req/s within 1% of glibc on aarch64
  and 3.2% fewer on x86_64 at the same CPU per request (-0.5%); p99 2 ms
  for every allocator but glibc-arena2 on aarch64 (4 ms). Against
  `MALLOC_ARENA_MAX=2`, the fleet's usual glibc mitigation: peak RSS
  +5.9% / +2.1%, and 2.6% more req/s at half the p99 on aarch64.
- The x86_64 3.2% req/s gap is inside the 10% band and has no mechanism
  named; if chased, the place is the cross-thread free (every 64 KiB
  direct buffer is allocated by an executor thread and freed by the
  Cleaner thread).

The JVM's own knobs (`-Xmx`, `-XX:MaxDirectMemorySize`) decide the native
churn; `narenas` is the analogue of `MALLOC_ARENA_MAX` and was not
needed on these rows.

The mixed-size soak (`bench/results/2026-09-20-{aarch64,x86_64}-jvm-soak.md`:
a 16..400-thread pool, direct buffers of 256 B..1 MiB, 60 s windows
alternating 256 and 8 connections for 1200 s) is where challoc trails the
tuned glibc variants: 13..22% above `MALLOC_ARENA_MAX=2` and
`-XX:TrimNativeHeapInterval=5000` in the tail-mean RSS at equal req/s,
about 222 MiB of it the huge tier's empty extents at the end of each
lull. `huge_dirty_decay_ms` was built for that and measured at 2000, 500
and 0 against the same two glibc variants on the same day
(`bench/results/2026-09-20-{aarch64,x86_64}-jvm-huge-decay.md`; tail mean
Rss over the last 600 s in MiB, CPU us per request in the high windows,
challoc's default = the slab period, 10 s):

| variant | aarch64 tail mean | aarch64 CPU us/req | x86_64 tail mean | x86_64 CPU us/req |
|---|---:|---:|---:|---:|
| challoc, default | 1185 | 53.2 | 1121 | 68.8 |
| `huge_dirty_decay_ms:2000` | 1181 (-0.4%) | 54.9 (+3%) | 1105 (-1.4%) | 71.5 (+4%) |
| `huge_dirty_decay_ms:500` | 1169 (-1.4%) | 58.1 (+9%) | 1093 (-2.5%) | 75.4 (+10%) |
| `huge_dirty_decay_ms:0` | 1088 (-8.2%) | 67.2 (+26%) | 1050 (-6.3%) | 83.7 (+22%) |
| glibc, `MALLOC_ARENA_MAX=2` | 981 | 49.4 | 986 | 64.2 |
| glibc, `TrimNativeHeapInterval=5000` | 969 | 50.8 | 865 | 63.9 |

The window does not buy the RSS back: the buffers are not idle in the
lulls. At 8 connections the JVM still moves about 4 GiB/s of direct
buffers, its direct-memory limit forces a GC every ~120 ms, the Cleaner
frees one cycle's ~200 MiB of huge runs in a burst and the next cycle
takes them back out of the empty list before any age-based window has a
step to purge; the "222 MiB inside the window" was one such burst caught
by the exit snapshot. A window shorter than the cycle (500, 0) purges
every burst and refaults it (`huge_dirty_decay_ms:0`: 7.5 M eager purges,
4.2 TB over 1200 s on aarch64), so the RSS bought is linear in the CPU
spent and the low windows lose 8..16% of their blob req/s. glibc-trim gets
its lower tail mean for less CPU because `malloc_trim(0)` every 5 s
purges whatever is free at that instant regardless of age and then
nothing for 5 s. Leave `huge_dirty_decay_ms` unset for a JVM; it is the
right knob only where huge runs are freed and not re-requested for longer
than the window (a process that keeps a large buffer for a phase and
drops it), and the same file's Valkey guard rows show it is inert for
Valkey (no huge frees). The periodic-trim shape is the direction the
results file names for the human (`challoc-trim` in the companion file
`2026-09-20-{aarch64,x86_64}-jvm-huge-decay-trim.md`).

### Node.js

`bench/results/2026-09-20-{aarch64,x86_64}-node.md` (Wolfi `nodejs-22`,
`--max-old-space-size=256`, an HTTP server streaming 1 MiB
`Buffer.allocUnsafe` bodies for 300 s). Nothing to set. What the rows
found:

- Startup (`node -e 0`): +10.5% (aarch64) / +4.3% (x86_64) against
  glibc, 2 ms of system time on a 20 ms process: about 270 more
  `mprotect` calls and 880 more minor faults from committing the first
  slab of each of the ~40 size classes V8's snapshot touches over the
  `PROT_NONE` class reservations (attributed in the aarch64 file). Max
  RSS 11.9% (aarch64) below glibc.
- `json_churn`: within 0.5% of glibc and jemalloc on aarch64, 7.1% faster
  than glibc on x86_64; RSS -10.3% against glibc on aarch64.
- Server: req/s within 2.2% of glibc on aarch64 and 5.5% above it on
  x86_64 with a 31% lower p99; peak RSS below every allocator on aarch64,
  +0.7% against glibc on x86_64. The one figure near the band is the
  x86_64 final RSS, +9.8% against glibc (138.8 vs 126.3 MiB): 62 MiB of
  `challoc:huge`, the freed 1 MiB bodies retained for reuse up to
  `huge_retain_max` (64 MiB) and decaying over `dirty_decay_ms`, where
  glibc and jemalloc trim between bursts.

`huge_retain_max` is the knob that governs that retention for a process
churning megabyte buffers; a lower value was not measured on this row.

### PostgreSQL

`bench/results/2026-09-20-{aarch64,x86_64}-postgres.md` (Wolfi
`postgresql-17`, pgbench scale 50, `shared_buffers` 1 GB, five allocators,
5 rounds) and, after the metadata region's small tables moved into the
heaps' mapping (`meta::EAGER_MAX`, design 2),
`2026-09-20-{aarch64,x86_64}-postgres-vmas.md` with a same-VM challoc
before/after in `-postgres-vmas-main.md`. Nothing to set. What the rows
found:

- Steady state (`select100`, `tpcb100`, `select500`): within a few percent
  of glibc and jemalloc on both architectures, the backends spending the
  same instructions and cache misses per transaction under either
  allocator (the attribution in the aarch64 file).
- Connection setup (`connect20`, a fork, a backend start-up, one TPC-B
  transaction and an exit per client): the row where challoc is behind,
  glibc +53% (aarch64) / +64% (x86_64) in tps on the first files, +43% /
  +59% on the lane VMs a day later (connection 2.5-2.6 ms against glibc's
  1.6-1.7). A fresh backend inherits every mapping of the postmaster, and
  challoc's heap is hundreds of them (one `rw-p` slab run and one `---p`
  tail per class region, and, before the change, one pair per metadata
  table) where glibc's is one `[heap]`; `fork` copies the VMAs and the
  page tables under the touched pages, the child faults its first slab of
  every class it uses, and exit tears the sparse page tables down. Moving
  the small tables into one mapping took a backend's metadata region from
  251 lines to 173 (its 464 mappings to 386) and the same-VM `connect20`
  by +2.4% tps / -2.5% connection time on aarch64, +1.5% / -1.5% on
  x86_64; what remains is the page-table sparsity of the class regions and
  the large tables (VmPTE 1.3 MiB per backend against glibc's 120 KiB, the
  same at every cap the lane measured) and the child's 58 slab commits,
  neither of which the metadata layout reaches.
- RSS: per-backend Pss_Anon 1.6-1.8 MiB against glibc's 1.2-1.3 (x86_64)
  or 2.5 (aarch64, where glibc's arenas retain more) and jemalloc's
  1.3-1.5: one committed slab per size class the backend touched plus a
  page per metadata table head, the same before and after the change. At
  500 backends that is +15% of the Pss sum against glibc on x86_64 and
  -19% on aarch64.

Connection cost is what a pooler is for. PostgreSQL's own advice for a
workload that opens a connection per transaction is PgBouncer (or the
application's pool) in front of the server, whatever the allocator: even
glibc's backend costs 1.6-1.8 ms to fork and start and a first transaction
several times a steady-state one, and with a pool the challoc backend's
extra fork cost is paid once per pooled connection, not per transaction.
Under a pool the rows that matter are the steady-state ones above.

## 4. The cgroup policy

Everything here is behind `cgroup:true` (the default). The measurement
layer is `src/sys/cgroup.rs`, the policy `src/policy.rs`, the advice
`src/purge.rs` (design 8 and 11).

Discovery, once at init: the cgroup path is the `0::` line of
`/proc/self/cgroup` (`/` inside a cgroup namespace); the cgroup2 mount is
the first `cgroup2` line of `/proc/self/mountinfo` (normally
`/sys/fs/cgroup`). Without either (a v1-only host) nothing is tracked and
the report says why. The effective limit is the minimum of `memory.max`
and of `memory.high` read at every level from the leaf up to the mount;
`max` or an absent file is unlimited. The walk is repeated at most once
per second, so `docker update --memory` is seen. `memory.current` and
`memory.events` (`high`, `max`) are sampled at most every 10 ms, from
allocation slow paths (every 64 slow-path entries of a heap), from the
background thread, from a `stats.cgroup.current` read and from
`malloc_stats_print`. The fast paths never see any of this.

The PSI trigger: only the background thread arms one. It writes
`some 150000 1000000` (a `some` stall of 150 ms in 1 s) to
`memory.pressure`; if the kernel answers `EINVAL` it tries the
unprivileged `some 300000 2000000` (windows under 2 s need
`CAP_SYS_RESOURCE`; unprivileged triggers exist since 6.4). A fired
trigger takes a sample, re-evaluates the policy and runs a decay pass at
once. Two container facts: Docker's default profile mounts the container's
cgroup read-only, so the trigger reads `psi unavailable (EROFS)` there on
every kernel; and on a host whose cgroup2 is mounted `nsdelegate`
(systemd hosts) a container's root cgroup refuses the write (`EPERM`)
even under `--privileged`. The fallback is the same: the tick's sample.

The policy. Usage is the last `memory.current` sample against the
effective limit `min(memory.high, memory.max)`:

| usage | decay period | retention | cache GC | purge advice |
|---|---|---|---|---|
| no limit | as configured | as configured | as configured | eager (`MADV_DONTNEED`) |
| under 70% | as configured | capped at 5% of the limit | as configured | lazy (`MADV_FREE`) |
| 70% to 90% | `dirty_decay_ms` scaled linearly down to 1000 ms at 90% | capped at 5% | as configured | eager |
| 90% and above | 0: everything `Empty` purged at the next pass | (everything goes) | every periodic tick | eager |
| `memory.events` `high` rose | one full purge at the next decay pass | | | eager |
| PSI trigger fired | a sample and a decay pass at once | | | as the band |

*Retention* is the memory the allocator holds that costs RSS without
serving anything: freed slabs and huge extents not yet purged (`Empty`).
The cap is applied inside the decay walks as a keep ratio, so the curve
still decides which slabs go first. A period already at or under 1000 ms
is left alone in the scaled band, and `dirty_decay_ms:-1` is left alone
in every band. A limit that goes away resets the outcome to the
defaults. The `high` baseline is the count at init, so a container that
already had `high` events does not start with a full purge.

The two-advice purge. A purge is `MADV_DONTNEED` (the pages leave RSS
and the charge at once; the next touch faults in a zero page) except
under 70% of a known limit, where it is `MADV_FREE` (the pages stay
mapped and charged until the kernel wants them, which it does before
anything else as the cgroup nears its limit, and a reuse before that
costs no fault). With no limit the advice is eager on purpose: lazily
freed pages would stay in RSS and mislead a fragmentation ratio or an
autoscaler. Every explicit purge (`malloc_trim(0)`, `arena.<i>.purge`,
the `high`-rise full purge) is eager, and so is the whole return of an
excess over the retention cap first seen at 70% or above, even after the
first eager walks bring usage back under 70% (the cap is a promise about
charged memory). Measured (`tests/c/cgroup_lazy.c`,
`bench/results/2026-09-19-lane-b-two-advice-purge.md`): 200 MiB freed at
40% of a 512 MiB container is purged lazily 80 ms later, RSS and
`memory.current` stay at 202 and 205 MiB while `memory.stat` `pglazyfree`
rises by 188 MiB; allocating the same 200 MiB again takes 5 ms against 44
ms after an eager purge; under pressure from a fresh 380 MiB set the
kernel reclaims the lazy pages and the process fits in the limit. One
consequence: lazily freed pages count in `memory.current`, so the band
moves toward 70% sooner than live memory alone would, which flips the
advice to eager early. That is an error in the safe direction.

What happens near the limit. The policy discharges retention, not live
memory. At the top of a climb the allocator's `empty_bytes` is 0 on a
4 KiB kernel (`bench/results/2026-09-19-matrix.md`, F1): the peak is
`allocated` plus the slack of partially used slabs plus metadata,
and no band, cap or sample interval can take those back. The kernel's
`memory.high` throttling and the OOM killer are the kernel's; what
challoc guarantees is that freed memory is never what stands between the
program and its limit.

The container tests that prove it (`make test-cgroup`, one
`docker run --memory=L --memory-swap=L` container per run,
`scripts/cgroup-tests.sh`; `bench/results/2026-09-18-aarch64-gce-cgroup-container-tests.md`):

- `tests/c/cgroup_churn.c` (`make test-cgroup-churn`, in `make test`):
  400 MiB allocated, freed and idled at about 80% of 512 MiB. RSS 402 ->
  13 MiB, under 100 MiB after 48 ms; with `MALLOC_CONF=cgroup:false` (the
  control) 319 MiB is still resident at 2 s. In the same container
  jemalloc 5.3.1 keeps 190 MiB and mimalloc 3.5.2 returns nothing (407).
  The same at 256 MiB with the sizes scaled.
- `tests/c/cgroup_lazy.c` (`make test-cgroup-lazy`, in `make test`): the
  two-advice measurement above, both ways (`lazy_band:false` as the
  eager control).
- `tests/c/cgroup_oscillate.c` (`make test-cgroup-oscillate`, opt-in,
  5 minutes): live memory swinging between 200 and 480 MiB in 512 MiB is
  not OOM-killed; jemalloc is killed at the fifth climb (12 s) and
  mimalloc at the second (6 s) in the same container.
- `tests/c/cgroup_policy.c high` (by hand, needs `--privileged
  --cgroupns=host` and a sibling cgroup with `memory.high`): the `high`
  rise row.

The idle phase of these tests is an event loop (a burst of small
allocations every 10 ms), because without a background thread the policy
runs from the periodic tick: a process that frees 400 MiB and then does
nothing at all keeps it under every setting. Set `background_thread:true`
for a process that idles completely after a burst.

The knobs that move the policy:

| key | effect on the policy |
|---|---|
| `cgroup:false` | nothing is read or applied; every row is "no limit" |
| `dirty_decay_ms` | the configured period the scaled band starts from; `-1` is never touched by any band |
| `lazy_band:false` | every band eager |
| `lazy_free:true` | every band lazy, explicit purges included |
| `background_thread:true` | the PSI trigger exists, and decay runs while the process idles |
| `huge_retain_max` | how much freed huge memory waits for decay rather than going at once (retention the cap then counts) |
| `tcache_gc_interval_ms` | the cache GC period the immediate band overrides with "every tick" |
| `thp:always` | forced to `never` when the effective limit at init is under 1 GiB; `auto` is not forced |
| `prefault` | the pass runs only with headroom (no limit, or under 70% with no eager return in progress) |

Observability: `stats.cgroup.limit`, `.high` (`SIZE_MAX` when unlimited),
`.current`, `.pressure_events`, `.decay_ms` (arena 0's effective period),
`.retention_cap`, `.empty_bytes`, `.full_purges` (`ENOENT` when nothing is
tracked), and, readable whether or not a cgroup is tracked,
`stats.cgroup.purge_advice` (1 lazy, 0 eager), `.lazy_purges`,
`.eager_purges`. The report's `cgroup:`, `cgroup policy:` and `purge
advice:` lines carry the same (section 6).

## 5. THP on `always` hosts (Chainguard OS)

Chainguard OS ships `/sys/kernel/mm/transparent_hugepage/enabled` at
`always` with the 2 MiB size on `inherit` and every mTHP size
(16 kB..1024 kB) at `never` (`bench/results/2026-09-20-aarch64-cgos-thp-decision.md`,
section 1). The reference GCE kernels ship `madvise`, where nothing in
this section differs from a plain `never`.

What the default does now (`thp:never`, since 2026-09-20): at
reservation the class superblock and the metadata region get
`MADV_NOHUGEPAGE`, a VMA flag every later split inherits, so no slab or
descriptor table ever sits on a 2 MiB page whatever the host's mode (a
2 MiB page under a few touched slabs is RSS the program never asked for,
and the fork copy-on-write cost Redis and Valkey forbid). The huge region
is not advised: a huge run is a whole multi-page allocation the program
asked for and takes what the host gives, as jemalloc's large extents do.
On Chainguard OS the report reads (verbatim from the decision file):

```
thp: never -> off (thp:never) nohuge 2 (classes, metadata; huge follows host) class_ranges 0 (0 bytes, 0 chunks) huge_ranges 0 meta_ranges 0 failures 0; host always process default (nothing issued)
```

and `/proc/<pid>/smaps` shows `AnonHugePages 0 kB` on the class region
and 2 MiB pages on the huge runs (8192 kB on an 8 MiB run in
`tests/c/thp.c never`). Expect `AnonHugePages` on the huge region there;
it is by design.

The measured alternatives, same cell (c4a-standard-8 Axion, kernel
6.18.49, 7 interleaved rounds, medians; positive = challoc behind
jemalloc; `2026-09-20-aarch64-cgos-thp-decision.md` section 3):

| setting | valkey1 vs jemalloc | valkey1-free | larson16 | peak RSS valkey1 | peak RSS larson16 | AnonHugePages at valkey1's peak |
|---|---:|---:|---:|---:|---:|---:|
| `thp:never` (the default) | +5.1% | +6.4% | +6.0% | 584.8 MiB | 173.7 MiB | 32 MiB (the huge runs) |
| `thp:auto` | +3.8% | +5.8% | +5.8% | 584.8 MiB | 176.8 MiB | 32 MiB |
| `thp:always` | -0.2% | +0.8% | +5.9% | 640.2 MiB (+9.5%) | 243.0 MiB (+39.9%) | 636 MiB |
| jemalloc | | | | 594.7 MiB | 263.5 MiB | 584 MiB |

The morning's run on the same host at the previous tree
(`2026-09-20-aarch64-cgos-thp-ab.md`, copied into the decision file's
table) had `never` advising the huge region too: valkey1 +16.1%,
`AnonHugePages` 0; and `never,thp_process:advised` +12.2% at 0 kB. So
the trade on an `always` host is:

- `thp:never` (default): RSS discipline on the slab regions; the huge
  runs on 2 MiB pages; about 5% behind jemalloc on the Valkey model.
- `thp:auto`: inert on Chainguard OS today (`auto -> off (no folio size
  below the PMD is enabled in sysfs)`): every sub-PMD size is `never`,
  and `auto` refuses to advise where the advice would enable 2 MiB pages.
  Its rows above are within the default's spread; it has nothing to buy
  there. It needs `hugepages-64kB/enabled` at `madvise` or `always` with
  `hugepages-2048kB/enabled` at `never` or `always` (root-only knobs).
- `thp:always`: parity with jemalloc on the Valkey model for +9.5% RSS
  on the 585 MiB heap and +39.9% on larson16's retained memory, with 281
  deferred page splits per run (a purge inside a 2 MiB page splits it).
  Guard slabs inside a chunk are left unguarded (a guard would split the
  page). Forced to `never` under a cgroup limit below 1 GiB.
- `thp_process:advised` on top of the default: no huge page anywhere in
  the process, libc's and the program's mappings included; the huge runs
  lose their pages too (+12.2% on valkey1 in the morning's row). Kernel
  6.18+ (`thp_except_advised=yes` in the feature line); below that the
  report says `advised: skipped, the kernel refuses ...` and nothing
  changes. A process whose parent already set a THP policy keeps it
  (`inherited`).

Fork: `never` keeps the slab regions on small pages on every host, so the
copy-on-write cost a `BGSAVE` child imposes is bounded to the huge runs
under the default and to nothing under `thp_process:advised`. The fork
rows are in `bench/results/2026-09-19-lane-b-mthp.md`.

The `thp:` line's last words name the host's global mode and the
process policy in force, so one report tells which of the four states the
process is in.

## 6. Reading `malloc_stats_print` and the feature report

Ways to get it: `malloc_stats_print(NULL, NULL, NULL)` (or with `"J"` for
JSON), Valkey's `MEMORY MALLOC-STATS`, `malloc_info(0, stream)` (glibc's
XML shape), Fiddle or `ctypes` in an interpreter, or the `atexit` hook
`bench/probes/statsdump.so`. The banner is jemalloc's (`___ Begin
jemalloc statistics ___`; consumers key on it); `Version: "5.3.0-0-gchalloc"`
says which library wrote it. `challoc_version()` (`0.1.0`) is the `dlsym`
probe for "is challoc loaded"; `challoc_features()` returns the feature
line alone.

The lines an operator reads first, in the order they appear:

| line | what to look for |
|---|---|
| `Run-time option settings` / `opt.<key>` | the configuration in force after every source; `(accepted, no effect)` marks an inert key; `thp` carries the forced-never note when a small cgroup limit applied |
| `Kernel and CPU features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no populate_write=yes thp=always mthp=2M collapse=unprobed thp_except_advised=yes rseq=yes rseq_size=28 mm_cid=yes slice_ext=... mte=no ... cacheline=64 l1d=64K l2=2M` | `guard_install=no` means a kernel before 6.13: no guard slabs. `process_madvise=no` means purges loop over `madvise` (Docker's default seccomp hides the syscall). `anon_name=no` means `/proc/<pid>/smaps` has no `challoc:*` names (the Chainguard OS server image, `bench/results/2026-09-20-chainguard-os-cells.md`). `thp=` is the host's global mode. `populate_write=no` means the prefault pass touches pages instead. `mm_cid=no` means `percpu_arena:percpu` would fall back. The keys are only ever added, never renamed |
| `Sealed ranges: N of M` | `0 of M` under Docker's default seccomp (mseal filtered) or before 6.10 |
| `Background thread: present` / `absent` | whether decay runs while idle; Valkey shows `present` |
| `Prefault: off (prefault:false)` / `off (prefault:true without background_thread)` / `on (MADV_POPULATE_WRITE) passes N (active N) slabs N (B bytes) skipped_no_headroom N fallbacks N wakes N taken N` | the second form is the misconfiguration to catch (section 3); `skipped_no_headroom` rising means the cgroup band vetoed the pass |
| `Per-CPU caches: off (auto: opt in with percpu_arena:percpu) (rseq usable, size 28)` / `on (N ids, ...)` / `off (rseq) (rseq no-mm_cid, size 20); percpu_arena:percpu requested, thread caches in use` | the mode in force and why; the probe word is `usable`, `no-mm_cid`, `unregistered`, `thread-unregistered`, `flags` or `no-symbols` |
| `Slice extension: off (kernel-disabled)` / `on (N threads enabled, N grants yielded via ...)` | the kernel's answer for the time slice extension |
| `Arenas: N (M automatic), explicit caches: K` | `N` is the arenas built so far (one per live thread up to the cap): 17 on the 16-thread Ruby soak, 64 on the 200-thread JVM server. This is the figure `narenas` moves |
| `Quantum size: 8` / `16` | `8` is Valkey's private build, `16` the system library |
| `Page size`, `Page-sized slabs:` | on a 16 or 64 KiB kernel the 512..1024-byte classes take the page as their slab |
| `Allocated: A, active: B, metadata: C, resident: D, mapped: E, retained: F` | `A` live bytes; `B` committed slabs and runs in use (`B/A` is the slack of partially used slabs, 1.94 on the JVM's 64 KiB direct buffers); `D - B` is dirty retention waiting for decay (what the cgroup cap bounds); `F` is purged address space, not RSS |
| `Huge: N runs (P pages), N empty extents (P pages), N purged extents (P pages), high-water page H` | the huge tier: `empty` is retained freed runs (`huge_retain_max`) |
| `Metadata: reserved N, committed N, writable N, allocated N, bumped N, tables N, small tables N` | descriptor tables, bitmaps and heaps; `writable` is the one mapping the small tables share, `small tables` how many of `tables` live in it (design 2) |
| `cgroup: <dir> (mount <m>, path <p>) limit L high H current C events high N (+r) max N (+r) psi <state>` or `cgroup: not tracked (<reason>)` | `limit`/`high` are the effective values up the hierarchy (`max` when unlimited); `psi armed (some ...)`, `unavailable (EROFS)` (Docker), `not armed (no background thread)` |
| `cgroup policy: defaults (no limit)` / `under 70% (defaults, retention capped)` / `70..90% (decay scaled, N% into the band)` / `over 90% (purge at once, cache GC every tick)` `decay_ms N retention_cap N keep N% empty_bytes N full_purges N thp as configured` | the band as last evaluated and what it gave arena 0; `empty_bytes` is the retention the cap acts on |
| `purge advice: lazy MADV_FREE (under 70% of the limit)` / `eager MADV_DONTNEED (no headroom known)` `lazy_purges N (B bytes) eager_purges N (B bytes) purged_is_zero true|false lazy via process_madvise|madvise loop` | which advice the next purge issues and why (the reasons are `lazy_free`, `explicit purge`, `lazy_band:false`, `under 70% of the limit`, `returning the excess over the cap`, `no headroom known`); `purged_is_zero false` means a lazy batch has happened and `calloc` now clears reused purged memory |
| `thp: <key> -> off|folios|chunks (<reason>) [nohuge N (classes, metadata; huge follows host)] [slab_min S pmd P meta M huge on|off] class_ranges N (B bytes, C chunks) huge_ranges N meta_ranges N failures N; host <mode> process <text>` | section 5; the reason text says why `auto` or `always` is inert |
| `arenas[N]:` blocks | `dirty_decay_ms` per arena (`arena.<i>.dirty_decay_ms`), `pactive/pdirty`, the `allocated active dirty retained` totals, the bins table (`size nregs curslabs curregs`, the compaction counters `refilled`, `compacted`) and the slab occupancy histogram (non-empty slabs per eighth of `nslots`), which shows where the live data sits after a delete phase |

The heads of these lines are spelled once, in `src/api/print.rs`
(`head!`), and a test (`print::tests::quoted_needles_match_the_report`)
checks that every line quoted in this section, in design 16 and in the C
tests is still one the report writes, so quote them exactly; `N` and
`<...>` stand for figures and variable text.

RSS by region, outside the report: with `anon_name=yes` the mappings are
named `[anon:challoc:classes]`, `[anon:challoc:huge]` and
`[anon:challoc:meta]` in `/proc/<pid>/smaps`, so `Rss` summed per name
places a gap without a second run (the JVM and Node rows do this:
`challoc:classes=187.9 challoc:meta=21.1 challoc:huge=13.8` MiB at the
end of the aarch64 JVM load). `AnonHugePages` per mapping shows where
huge pages landed (section 5).

## 7. What not to tune, and why

The fast paths of `malloc` and `free` are a thread-local load, a pop or
push, and a bounds check; none of the keys below changes an instruction
on them except by turning a feature on that costs one, and the design
fixes their values by measurement:

- `tcache`, `tcache_nslots_small_max`, `lg_tcache_nslots_mul`,
  `tcache_max`, `tcache_compact_shift`, `tcache_gc_interval_ms`: the cache
  geometry. The ladder is design 6.2's table; an L2-derived smaller
  ladder was priced and dropped
  (`bench/results/2026-09-19-lane-c-spin-and-geometry.md`); `tcache_max`
  is the RSS probe's finding; the compaction schedule is what keeps a
  long-running heap dense and Valkey's defragmentation effective.
  `tcache_nslots_small_max:512` is the measured RSS trade if a
  deployment wants a smaller cache.
- `guards`, `guard_slab_interval`: the hardening the default is measured
  with. The sized free's guard test is a mask, so the interval costs no
  fast-path time at any power of two.
- `zero_on_free`, `slot_randomize`, `pac_pointers`, `double_free_scan`:
  hardening opt-ins whose prices are in section 2's table. Turn one on
  for a requirement, not for a hunch, and re-run the consumer's row.
- `percpu_arena:percpu`: 4..38% behind thread caches on every `ab` row;
  no workload in the suite has shown the win it is designed for.
- `spin_hint`: `auto` is the probe's choice; `wfe` lost; `umwait` has
  never run on any host the project reached.
- `slice_ext`: `auto` follows the kernel; there is nothing to gain by
  forcing it.
- `lazy_free:true`: makes RSS lie everywhere, including with no limit.
- `cgroup:false`: only as a control.
- `group_handoff`, `check_zero`: an opt-in that did not move its row, and
  a debug mode.
- `narenas` for Valkey: the server is one main thread plus io-threads;
  the default gives it `min(threads, 4 * ncpus)` arenas and the soak
  criteria are met with it.

Change `dirty_decay_ms`, `background_thread`, `narenas` (many-thread
idle processes), `huge_retain_max` and `huge_dirty_decay_ms` (large-buffer
churn), `lazy_band` (RSS-derived metrics under a limit) and the `thp` pair
(section 5) with a row in hand; leave the rest.
