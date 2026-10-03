# challoc tuning: the operator's guide

This is the operator's view of challoc's tunables and of its cgroup and
THP behaviour. `src/config.rs` (`TABLE`, `Config::DEFAULT`) is the source
of truth for the key list, the defaults and the value grammar; the design
behind each key is in [design.md](design.md) (cited as "design N").

Rule of thumb first: the defaults are the measured configuration. Where
the trade is between throughput and memory, RSS comes first. Section 7
lists the keys not to touch and why; section 8 lists the known
limitations.

## 1. Setting keys

challoc reads its configuration once, at first use, from these sources
in increasing priority (`config::load`); a later source overrides an
earlier one key by key:

| order | source | notes |
|---|---|---|
| 1 | compiled defaults | `Config::DEFAULT` |
| 2 | the `malloc_conf` global | the library defines a null `malloc_conf`; a program that defines its own `const char *malloc_conf` interposes it, jemalloc style (Ruby's `--with-jemalloc` build does) |
| 3 | the `je_malloc_conf` global | weak; Redis and Valkey define it |
| 4 | `/etc/challoc.conf` | first line only, up to 4096 bytes, a trailing `\r` stripped; read with raw syscalls. The file is `challoc.conf`, not jemalloc's `/etc/malloc.conf` symlink |
| 5 | `MALLOC_CONF` | environment; read from `environ`, never through `getenv` |
| 6 | `CHALLOC_CONF` | environment, after `MALLOC_CONF`, so it wins over a `MALLOC_CONF` written for jemalloc |

The two environment variables are skipped when the process is
`AT_SECURE` (setuid, setgid or capability-elevated), glibc's
`secure_getenv` rule. The two globals and the file are always read.

Format: jemalloc's `key:value,key:value`. A value may be wrapped in
double quotes (and may then contain commas). A value that opens a quote
and does not end with one is reported as malformed; a value with an
unclosed quote ends at the next comma. Empty pairs (`a,,b`, a trailing
comma) are ignored. Booleans are `true` and `false` only. Integers are
decimal, `0x` hexadecimal or leading-zero octal, with an optional sign.
An out-of-range integer is clamped to the key's range and reported.
`guard_slab_interval` must be a power of two: anything else is rounded
up and reported.

Reporting: every problem is printed on stderr as `challoc: <source>:
<issue>` (`unknown conf key "x"`, `invalid conf value "x" for k`,
`out-of-range conf value "x" for k (clamped)`, `conf value "x" for k is
not a power of two (rounded up to N)`, `conf key k is accepted but has no
effect (not implemented)`) and the offending pair is skipped (clamped or
rounded for the numeric cases); parsing continues.

Each source is read up to 4096 bytes (for an environment variable, its
value). A longer source is cut there and reported as `conf text longer
than 4096 bytes (ignored after the last complete pair)`: the pairs before
the cut apply, and the pair the cut splits is dropped rather than applied
with a shortened value.

`abort_conf:true` turns the report into an abort. It is evaluated after
every source has been read, so it covers keys that came before it, and it
refuses unknown keys, bad values, out-of-range and rounded numbers,
inert keys, and sources longer than 4096 bytes. It does not refuse a
documented runtime fallback for a valid key (`thp:auto` on a host with
no sub-PMD folio size, `thp:always` under a cgroup limit below 1 GiB);
the `malloc_stats_print` report says which applies (section 6). Use it in package tests:
`MALLOC_CONF=abort_conf:true,no_such_key:1 ruby -e 1` aborts with
challoc's message.

Keys outside the table:

- `stats_print` is *inert*: it parses as a boolean and is reported every
  time it is set, so a program cannot believe it turned something on;
  nothing prints at exit (#62). `malloc_stats_print` lists it as
  `opt.stats_print: accepted, no effect`, and `mallctl("opt.stats_print")`
  returns `ENOENT`. To get the report at exit from a program that cannot
  call `malloc_stats_print` itself, preload an `atexit` hook that calls it.
- `junk`, `zero`, `retain`, `oversize_threshold`, `metadata_thp` and
  `confirm_conf` are jemalloc keys accepted with any value and ignored, so
  a `MALLOC_CONF` written for jemalloc parses clean under `abort_conf`.
  `opt.<key>` returns `ENOENT` for them.

Reading back: `mallctl("opt.<key>")` returns the value in force for every
key of section 2 (a bool, an integer, or a string for the enumerated
keys). `malloc_stats_print` prints them under `Run-time option settings`
as `opt.<key>: <value>`, annotated `(accepted, no effect; caches are per
thread)` for `percpu_arena:percpu` or `phycpu`, and `(accepted, no
effect; forced to never by a cgroup limit under 1 GiB)` for `thp` when
section 4's rule applied.

At run time, through `mallctl`: `background_thread` (a write of `true`
creates the thread synchronously; Valkey does this at startup),
`arena.<i>.dirty_decay_ms`, `arena.<i>.purge` and `arena.<i>.decay`,
`thread.tcache.enabled`, `thread.tcache.flush`; and `malloc_trim(0)`
(every arena purged, eagerly).

## 2. The keys

One row per key, in `TABLE` order. "Values" is the grammar
`Config::set` accepts; ranges are inclusive.

| key | values | default | what it does | change it when | cost or risk |
|---|---|---|---|---|---|
| `background_thread` | `true`, `false` | `false` | Runs decay (and the cgroup sampling) on a background thread as well as from allocation slow paths. Created on the first periodic tick after the request, or synchronously by `mallctl("background_thread")`. | The process goes idle for long stretches with freed memory still dirty: without the thread an idle process never purges (as jemalloc). Valkey turns it on itself. | One thread, one futex sleep; its stack is one metadata reservation. No measurable effect on a process that keeps allocating. |
| `dirty_decay_ms` | `-1`..`1000000000000` | `10000` | The window of the decay curve: freed slabs and huge extents go from dirty to purged over this many milliseconds along a smootherstep (about half by mid-window, all by the end). `-1` never purges; `0` purges at the next pass. | Lower it (1000..5000) when RSS after a burst matters more than refaults; raise it or set `-1` for a process that reuses everything it frees and never idles. | Purging costs `madvise` calls and refaults on reuse; never purging keeps every peak resident. Under a cgroup limit the policy scales this value down (section 4); `-1` is left alone in every band. |
| `muzzy_decay_ms` | `-1`..`1000000000000` | `0` | Accepted for jemalloc compatibility, stored, readable as `opt.muzzy_decay_ms`; nothing reads it. There is no muzzy state: the two-advice purge (section 4) replaces it. | Never. | None. |
| `narenas` | `0`..`4294967295` | `0` | Automatic arenas. `0` means `min(4 * ncpus, 64)` (`ncpus` is the affinity mask's width). Each new thread heap is bound to the least-loaded automatic arena, so a program with `t` live threads touches `min(t, narenas)` arenas; each arena has its own partial slabs per class. | A many-threaded process that wants one shared slab set for all its threads, the lowest RSS floor (`narenas:1`). | Fewer arenas means more threads share each arena's class locks on the slow path (refill and flush) and interleave their slots in the same slabs (about one L1D miss per operation on 10-thread `larson` under one arena); the fast paths are unaffected. |
| `percpu_arena` | `disabled`, `percpu`, `phycpu` | `disabled` | jemalloc's key, accepted with jemalloc's values so a jemalloc conf string parses under `abort_conf` and reads back through `opt.percpu_arena`. It changes nothing: caches are per thread whatever the value (design 15). | Never. | None. |
| `tcache` | `true`, `false` | `true` | Thread caches on the `malloc`/`free` fast paths. `false` sends every operation to the slabs under the class lock. | Never in production; a diagnostic. | Every operation becomes a slow path. |
| `tcache_max` | `0`..`262144` | `32768` | Largest class size (bytes) whose slots are cached (`opt.tcache_max` reads as a `size_t`, like jemalloc's); classes above it are served from arena 0's slabs directly, so one partial slab per class serves every thread. | Never. | Raising it multiplies cached large slots by threads and arenas: 8 cached 40 KiB slots per (thread, arena) held 70 MiB of a 16-thread benchmark's RSS. |
| `tcache_nslots_small_max` | `8`..`65536` | `1024` | Cache capacity for classes up to 128 bytes; the capacity halves per doubling of the class size above that (the ladder), down to a floor of 8. | To trade cache RSS for refill frequency: `512` halves every cache. | More refills and flushes (slow paths) per operation. |
| `lg_tcache_nslots_mul` | `-16`..`16` | `1` | Shifts the whole ladder by `lg_tcache_nslots_mul - 1` doublings (jemalloc's key; Redis's `je_malloc_conf` sets `3`). | As `tcache_nslots_small_max`. | As above; the result is clamped to `[8, tcache_nslots_small_max]`. |
| `thp` | `never` (alias `default`), `auto`, `always` | `never` | Transparent huge pages for challoc's regions (section 5). `never`: `MADV_NOHUGEPAGE` on the class superblock and the metadata region; the huge region (allocations above 256 KiB) follows the host's policy. `auto`: `MADV_HUGEPAGE` for sub-PMD folios (64 KiB) on slab groups and metadata, never a 2 MiB page; inert where no such size is enabled or the 2 MiB size is `madvise`-gated. `always`: 2 MiB chunks over the class regions, huge runs and metadata (jemalloc's `thp:always`); forced to `never` under a cgroup limit below 1 GiB. | `always` for a single-tenant, cache-shaped heap with RSS to spare that does not fork. `auto` on an aarch64 host with `hugepages-64kB` enabled and dTLB-bound rows. | `always`: the Valkey model's `SET` loop 5..18% faster for about +10% peak RSS on that heap and +30..66% on allocation microbenchmarks (#20). `auto`: about 1.6% slower on `larson` and 7..19% more RSS on small heaps. |
| `lazy_band` | `true`, `false` | `true` | `MADV_FREE` for purges made under 70% of a known cgroup limit (the headroom band); `false` makes every purge `MADV_DONTNEED`. | `false` when the container's RSS figure must track live memory exactly even with headroom (an autoscaler, a fragmentation ratio). | Refaults on reuse: re-allocating a lazily purged range costs a small fraction of refaulting an eagerly purged one. |
| `guards` | `true`, `false` | `true` | Guard slabs between slabs (kernel 6.13+, `MADV_GUARD_INSTALL`) and a guard after every huge run (a `PROT_NONE` page on older kernels). | Never. | `false` removes an overflow barrier. |
| `guard_slab_interval` | `0`, or `2`..`4194304`, a power of two | `8` | One guard slab per N x 64 KiB of slabs (every Nth slab for the 64 KiB-slab classes; the same span in bytes for the classes with smaller slabs). `0` is none; `1` is out of range and clamps to `2`; a non-power of two rounds up and is reported. | To trade address space and slab count for guard density. | A denser interval costs slabs; it never costs a fast-path instruction (the test is a mask). No guard slabs exist before kernel 6.13 whatever the value. |
| `huge_retain_max` | `0`..`1099511627776` | `67108864` | Freed huge runs (allocations above 256 KiB) with more usable bytes than this are purged the moment they are freed instead of waiting for decay; smaller ones are retained for reuse and decay (`opt.huge_retain_max` reads as a `size_t`). | A process churns large buffers and their retention shows in RSS between bursts (Node's 1 MiB `Buffer` bodies kept tens of MiB of `challoc:huge` at the end of a load). | A lower value refaults every reused large buffer. |
| `huge_dirty_decay_ms` | `-1`..`1000000000000` | unset: follows `dirty_decay_ms` (`opt.huge_dirty_decay_ms` reads the effective value) | The huge tier's own decay window (design 7): freed huge runs at or under `huge_retain_max` decay on their own curve while the slabs keep `dirty_decay_ms`. Unset it follows arena 0's period (and `arena.0.dirty_decay_ms` moves both). `0` purges a huge run the moment it is freed, whatever its size; `-1` never purges one by decay (`malloc_trim(0)` still does). | Huge runs are freed and not re-requested for longer than the window (a process that keeps a large buffer for a phase and drops it). Not for a JVM: its direct buffers are freed and re-requested every GC cycle. | A shorter window refaults every reused huge run; on the JVM's direct buffers the RSS bought was linear in the CPU spent (`0`: about -7% RSS for +22..26% CPU per request). |
| `zero_on_free` | `true`, `false` | `false` | Clears a freed block (a slot's class-size bytes, a huge run's usable bytes) before it is cached or released. | A hardening requirement that outweighs the cost. | A `memset` per free: on Valkey's 1 KiB values far over a 3% budget, and 2.5..3.4x on `larson`. |
| `pac_pointers` | `true`, `false` | `false` | On arm64 with pointer authentication (`pac=yes` in the feature line: Axion, Apple silicon; not Altra, not x86_64), signs every cached slot pointer and the TLS heap pointer with `pacda` and authenticates on pop, so a metadata write cannot make `malloc` return a chosen address. Inert elsewhere. | A hardening requirement on a host with `pac=yes`. | About 11% on cross-thread frees (`xmalloc-test`), 1..2% on `larson`. |
| `slot_randomize` | `true`, `false` | `false` | Cache refills claim from a random bitmap word and shuffle the batch, so slot order is not predictable from previous allocations. Slow paths only. | A hardening requirement. | Works against the compacting cache (design 6.4): more RSS on churn-heavy workloads. |
| `double_free_scan` | `true`, `false` | `false` | Every free compares the slot against the four most recently cached entries and aborts on a match, catching the one double-free window the default leaves (a second free while the slot is still cached, popped before a flush). | Debugging a suspected double free. | About 3..4% on `larson1`, up to 14% on the Valkey model on x86_64. |
| `abort_conf` | `true`, `false` | `false` | Abort after parsing if any source reported a problem (section 1). | Package tests and any deployment that must not run on a misspelled key. | None at run time. |
| `cgroup` | `true`, `false` | `true` | Read the memory cgroup at init and sample it: effective limits, `memory.current`, `memory.events`; apply the policy of section 4. `false` reads nothing, applies nothing, reports `cgroup: not tracked (cgroup:false)`. | `false` only as the control in a measurement. | With `cgroup:false` a container that frees 400 MiB of 512 MiB keeps most of it resident for the whole decay window; with the default it is back to about 13 MiB within 50 ms. |
| `check_zero` | `true`, `false` | `false` | Debug: every slot or page handed out as known zero is read at its first and last word under the class or huge lock; a non-zero word aborts with the slab's prior state. Slow paths only. | Diagnosing a `calloc` zero bug. | Extra reads on every refill from `Fresh` or `Purged` memory. |

Why the close call went the way it did: `thp:never` is the default
because RSS comes first. On a host whose THP mode is `always`
(Chainguard OS), `thp:always` buys the Valkey model's hot loop parity
with jemalloc but costs about 10% more RSS on that heap and far more on
smaller ones, and does not meet the budget a shared or memory-limited
host needs (section 5). The hardening opt-ins (`zero_on_free`,
`slot_randomize`, `pac_pointers`, `double_free_scan`) each cost more than
a default-on hardening may; `lazy_band:true` with eager purges when no
limit is known is chosen so RSS never lies where nothing knows the
headroom.

## 3. Consumers

### Valkey and Redis

The Wolfi package (`contrib/wolfi/valkey/valkey-9.1.yaml`, design 13.6)
builds a private `quantum8` challoc into `/usr/lib/valkey/libchalloc.so.0`
and links `valkey-server` to it through `DT_RPATH`. The system
`/usr/lib/libchalloc.so.0` is a `quantum16` build; Valkey's
`allocator_defrag.c` asserts `arenas.quantum == 8` at start, so a server
forced onto the system library (`LD_PRELOAD`) dies in
`allocatorDefragInit` before it logs anything. Do not "fix" this by
pointing `LD_LIBRARY_PATH` at `/usr/lib`; the rpath wins anyway.

The contrib patch also switches Valkey's accounting onto challoc's
size-returning entry points (`<challoc/challoc.h>`, design 13.7): under
`USE_CHALLOC`, `zmalloc`/`zcalloc`/`zrealloc` get the usable size back
from `challoc_malloc_usable`, `challoc_calloc_usable` and
`challoc_realloc_usable` instead of asking `zmalloc_size` after the call,
and the unsized `zfree(ptr)` is one `challoc_free_usable`. Nothing to
configure; `INFO memory`'s `used_memory` is the same figure.

What Valkey sets itself: `jemalloc-bg-thread yes` (the default) calls
`mallctl("background_thread", true)` at startup, so the server has the
decay thread without any key; `MEMORY PURGE` is `arena.<i>.purge`
(eager, whatever the band); `MEMORY MALLOC-STATS` is `malloc_stats_print`
(section 6). Redis defines `je_malloc_conf` with
`lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000`, which challoc
reads as a source (section 1); `MALLOC_CONF` overrides it.

Active defragmentation: `CONFIG SET activedefrag yes` works on the
`quantum8` build (the package test asserts it). challoc answers
`experimental.utilization.batch_query` and `stats.arenas.<i>.bins.<j>.*`
exactly; `nonfull_slabs` includes the slab the next claim takes from,
where jemalloc's excludes it, so for the same heap challoc reports one
more non-full slab and moves slightly more (design 13.4). The contrib
patch changes one Valkey test threshold (`memefficiency.tcl`,
`ignore_bytes` for the Lua script test: challoc replaces libc `malloc`
too, so the Lua heap sits in its bins). Nothing about defrag is tunable
on the challoc side; the decision rule is Valkey's.

RSS-derived metrics under a cgroup limit: below 70% of the limit purges
are `MADV_FREE` (section 4), so `allocator_resident`, RSS and therefore
`allocator_frag_ratio` include lazily freed pages until the kernel
reclaims them. If those figures drive alerting or an autoscaler, set
`lazy_band:false`. With no limit at all the default is already eager.

THP: Valkey warns about THP `always` because of fork copy-on-write cost.
challoc's default `never` keeps the class and metadata regions off huge
pages on every host; the huge runs take what the host gives (section 5).
Nothing else needs setting for Valkey: the server is one main thread plus
io-threads, and the default arena count fits it.

### Ruby

The package (`contrib/wolfi/ruby/ruby-3.4.yaml`) builds Ruby
`--with-jemalloc` against `challoc-jemalloc-compat-dev` and runs on
`challoc-jemalloc-compat`'s `/usr/lib/libjemalloc.so.2` (a `quantum16`
build with the jemalloc soname). Nothing is set: Ruby's `malloc_conf`
path through the header and `MALLOC_CONF` both work, and the package test
proves the allocator from inside the interpreter through Fiddle.
`background_thread:true` has no measurable effect on a busy Ruby process
(the slow-path ticks already drive the decay). Ruby's GC sweep frees every
thread's garbage from whichever thread runs it; those frees go through
one flush-only cache per class on the freeing thread (design 6.3), so
they do not pile up. `narenas:1` reads lower at the peak of a
many-threaded process at the price of one arena's class locks for every
thread; see section 8 for the x86_64 RSS gap.

### JVM

Nothing to set. The JVM's own knobs (`-Xmx`, `-XX:MaxDirectMemorySize`)
decide the native churn; `narenas` is the analogue of glibc's
`MALLOC_ARENA_MAX` and was not needed. Leave `huge_dirty_decay_ms` unset:
the JVM's direct buffers are freed in bursts by the Cleaner and taken back
out of the empty list by the next GC cycle before any age-based window
has a step to purge, so a shorter window purges and refaults every burst
for an RSS saving linear in the CPU it costs (row in section 2). A
periodic `malloc_trim(0)`, which purges whatever is free at that instant,
is the better shape for a process that wants that RSS back.

### Node.js

Nothing to set. A server streaming 1 MiB `Buffer`s retains the freed
bodies in the huge tier up to `huge_retain_max` (64 MiB), decaying over
`dirty_decay_ms`, where glibc trims between bursts; lower
`huge_retain_max` if that retention matters more than refaults.

### PostgreSQL

Nothing to set; steady-state transactions are within a few percent of
glibc and jemalloc. Connection setup costs more than under glibc (section
8): PostgreSQL forks a backend per connection, and challoc's heap is many
mappings where glibc's is one. A connection-per-transaction workload
belongs behind a pooler (PgBouncer or the application's pool) whatever
the allocator; under a pool the extra fork cost is paid once per pooled
connection.

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

The policy. Usage is the last `memory.current` sample against
`min(memory.high, memory.max)` at every level that has a limit, each
level's own usage against its own limit, and the most severe row wins:
a parent limited to 1000 MiB that holds 950 MiB puts the process at 95%
even when its own cgroup holds 100 MiB, because the kernel reclaims and
OOM-kills against the parent's usage. Each such ancestor costs one more
read per sample.

| usage | decay period | retention | cache GC | purge advice |
|---|---|---|---|---|
| no limit | as configured | as configured | as configured | eager (`MADV_DONTNEED`) |
| under 70% | as configured | capped at 5% of the limit | as configured | lazy (`MADV_FREE`) |
| 70% to 90% | `dirty_decay_ms` scaled linearly down to 1000 ms at 90% | capped at 5% | as configured | eager |
| 90% and above | 0: everything `Empty` purged at the next pass | (everything goes) | every periodic tick | eager |
| `memory.events` `high` rose | one full purge at the next decay pass | | | eager |

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
mapped until the kernel wants them, which it does before anything else
as the cgroup nears its limit, and a reuse before that costs no fault).
With no limit the advice is eager on purpose: lazily freed pages would
stay in RSS and mislead a fragmentation ratio or an autoscaler. Every
explicit purge (`malloc_trim(0)`, `arena.<i>.purge`, the `high`-rise full
purge) is eager, and so is the whole return of an excess over the
retention cap first seen at 70% or above, even after the first eager
walks bring usage back under 70% (the cap is a promise about charged
memory).

`MADV_FREE`d pages stay charged to `memory.current` until the kernel
reclaims them, so the band moves toward 70% sooner than live memory alone
would, which flips the advice to eager early. That is an error in the safe
direction, and the lazy pages are the first thing the kernel reclaims near
`memory.high` or `memory.max`.

What happens near the limit. The policy discharges retention, not live
memory: at the top of a climb the peak is `allocated` plus the slack of
partially used slabs plus metadata, and no band, cap or sample interval
can take those back. The kernel's `memory.high` throttling and the OOM
killer are the kernel's; what challoc guarantees is that freed memory is
never what stands between the program and its limit. In a 512 MiB
container a live set swinging between 200 and 480 MiB runs for 5 minutes
under challoc, and 400 MiB freed at about 80% of the limit is back to
about 13 MiB of RSS within 50 ms (jemalloc keeps 190 MiB).

The container tests (`make test-cgroup`, one `docker run --memory=L
--memory-swap=L` container per run, `scripts/cgroup-tests.sh`):
`tests/c/cgroup_churn.c` (allocate, free and idle near the limit; with
`cgroup:false` as the control), `tests/c/cgroup_lazy.c` (the two-advice
purge both ways), `tests/c/cgroup_oscillate.c` (the 5-minute swing,
opt-in) and `tests/c/cgroup_policy.c high` (by hand, needs `--privileged
--cgroupns=host` and a sibling cgroup with `memory.high`: the `high`-rise
row).

Without a background thread the policy runs from the periodic tick: a
process that frees 400 MiB and then does nothing at all keeps it under
every setting. Set `background_thread:true` for a process that idles
completely after a burst.

The knobs that move the policy:

| key | effect on the policy |
|---|---|
| `cgroup:false` | nothing is read or applied; every row is "no limit" |
| `dirty_decay_ms:N` | the configured period the scaled band starts from; `-1` is never touched by any band |
| `lazy_band:false` | every band eager |
| `background_thread:true` | sampling and decay run while the process idles |
| `huge_retain_max:N` | how much freed huge memory waits for decay rather than going at once (retention the cap then counts) |
| `thp:always` | forced to `never` when the effective limit at init is under 1 GiB; `auto` is not forced |

Observability: `stats.cgroup.limit`, `.high` (`SIZE_MAX` when unlimited),
`.current`, `.decay_ms` (arena 0's effective period), `.retention_cap`,
`.empty_bytes`, `.full_purges` (`ENOENT` when nothing is tracked), and,
readable whether or not a cgroup is tracked, `stats.cgroup.purge_advice`
(1 lazy, 0 eager), `.lazy_purges`, `.eager_purges`. The report's
`cgroup:`, `cgroup policy:` and `purge advice:` lines carry the same
(section 6).

## 5. THP on `always` hosts (Chainguard OS)

Chainguard OS ships `/sys/kernel/mm/transparent_hugepage/enabled` at
`always` with the 2 MiB size on `inherit` and every smaller size at
`never`. On a `madvise` host (most distributions' default) nothing in this
section differs from a plain `never`.

What the default does (`thp:never`): at reservation the class superblock
and the metadata region get `MADV_NOHUGEPAGE`, a VMA flag every later
split inherits, so no slab or descriptor table ever sits on a 2 MiB page
whatever the host's mode (a 2 MiB page under a few touched slabs is RSS
the program never asked for, and the fork copy-on-write cost Redis and
Valkey forbid). The huge region is not advised: a huge run is a whole
multi-page allocation the program asked for and takes what the host
gives, as jemalloc's large extents do. On Chainguard OS the report reads:

```
thp: never -> off (thp:never) nohuge 2 (classes, metadata; huge follows host) class_ranges 0 (0 bytes, 0 chunks) huge_ranges 0 meta_ranges 0 failures 0; host always process default (nothing issued)
```

and `/proc/<pid>/smaps` shows `AnonHugePages 0 kB` on the class region
and 2 MiB pages on the huge runs. Expect `AnonHugePages` on the huge
region there; it is by design.

The alternatives on an `always` host:

- `thp:never` (default): RSS discipline on the slab regions, the huge
  runs on 2 MiB pages; about 5% behind jemalloc on the Valkey model's hot
  loop, where jemalloc's whole heap sits on 2 MiB pages.
- `thp:auto`: inert on Chainguard OS today (`auto -> off (no folio size
  below the PMD is enabled in sysfs)`), because every sub-PMD size is
  `never` and `auto` refuses to advise where the advice would enable
  2 MiB pages. It needs `hugepages-64kB/enabled` at `madvise` or `always`
  with `hugepages-2048kB/enabled` at `never` or `always` (root-only
  knobs).
- `thp:always`: parity with jemalloc on the Valkey model's hot loop
  (5..10% faster than `never` on arm64, 12..18% on amd64). Guard slabs
  inside a chunk are left unguarded (a guard would split the page).
  Forced to `never` under a cgroup limit below 1 GiB.

The RSS cost of opting into `thp:always`, measured against the default
on Chainguard OS 6.18 (`always` hosts) and on 7.x `madvise` hosts on both
architectures (#20):

- No row but the Valkey model's hot loop gains: `larson`, `xmalloc` and
  `mstress` are flat, and the Valkey soak shows no throughput gain.
- Peak RSS: +9.5% on the Valkey model; `mstress` +56..62% (about
  +190 MiB), `larson10` +53..66%, `xmalloc` +30..39%.
- Valkey soak: RSS after the settle phase up to 5x on the `madvise` hosts
  (+13..23% on the `always` hosts). About three quarters of the 2 MiB
  folios `always` faults in are queued for deferred split by the end of a
  run: partial purges split them, and the split folios stay charged, so
  the purges do not return the memory.
- Under a 1 GiB cgroup (not small enough for the forced `never`) it is
  slower too: `mstress` 2.6..7.2% on most cells.

So `thp:always` is an opt-in for a single-tenant, cache-shaped heap that
has the RSS to spare and does not fork; it does not meet the RSS budget a
shared or memory-limited host needs. Under the default the copy-on-write
cost a `BGSAVE` child imposes is bounded to the huge runs. The `thp:`
line's last words name the host's global mode.

## 6. Reading `malloc_stats_print` and the feature report

Ways to get it: `malloc_stats_print(NULL, NULL, NULL)` (or with `"J"` for
JSON), Valkey's `MEMORY MALLOC-STATS`, `malloc_info(0, stream)` (glibc's
XML shape), Fiddle or `ctypes` in an interpreter, or an `atexit` hook
preloaded into the process. The banner is jemalloc's (`___ Begin
jemalloc statistics ___`; consumers key on it); `Version:
"5.3.0-0-gchalloc"` says which library wrote it. `challoc_version()`
(`0.1.0`) is the `dlsym` probe for "is challoc loaded";
`challoc_features()` returns the feature line alone.

The lines an operator reads first, in the order they appear:

| line | what to look for |
|---|---|
| `Run-time option settings` / `opt.<key>` | the configuration in force after every source; `(accepted, no effect)` marks an inert key; `thp` carries the forced-never note when a small cgroup limit applied |
| `Kernel and CPU features: page_size=4096 guard_install=yes mseal=yes anon_name=no thp=always mthp=2M rseq=yes rseq_size=28 pac=yes` | `guard_install=no` means a kernel before 6.13: no guard slabs. `anon_name=no` means `/proc/<pid>/smaps` has no `challoc:*` names (a kernel without `CONFIG_ANON_VMA_NAME`). `thp=` is the host's global mode. The keys are never renamed |
| `Sealed ranges: N of M` | `0 of M` under Docker's default seccomp (mseal filtered) or before 6.10 |
| `Background thread: present` / `absent` | whether decay runs while idle; Valkey shows `present` |
| `Arenas: N (M automatic), explicit caches: K` | `N` is the arenas built so far (one per live thread up to the cap). This is the figure `narenas` moves |
| `Quantum size: 8` / `16` | `8` is Valkey's private build, `16` the system library |
| `Page size`, `Page-sized slabs:` | on a 16 or 64 KiB kernel the 512..1024-byte classes take the page as their slab |
| `Allocated: A, active: B, metadata: C, resident: D, mapped: E, retained: F` | `A` live bytes; `B` committed slabs and runs in use (`B/A` is the slack of partially used slabs); `D - B` is dirty retention waiting for decay plus metadata (what the cgroup cap bounds); `F` is purged address space, not RSS |
| `Huge: N runs (P pages), N empty extents (P pages), N purged extents (P pages), high-water page H` | the huge tier: `empty` is retained freed runs (`huge_retain_max`) |
| `Metadata: reserved N, committed N, writable N, allocated N, bumped N, tables N, small tables N` | descriptor tables, bitmaps and heaps; `writable` is the one mapping the small tables share, `small tables` how many of `tables` live in it (design 2) |
| `cgroup: <dir> (mount <m>, path <p>) limit L high H current C events high N (+r) max N (+r)` or `cgroup: not tracked (<reason>)` | `limit`/`high` are the effective values up the hierarchy (`max` when unlimited) |
| `cgroup policy: defaults (no limit)` / `under 70% (defaults, retention capped)` / `70..90% (decay scaled, N% into the band)` / `over 90% (purge at once, cache GC every tick)` `decay_ms N retention_cap N keep N% empty_bytes N full_purges N thp as configured` | the band as last evaluated and what it gave arena 0; `empty_bytes` is the retention the cap acts on. The band is the most severe of the leaf and each limited ancestor against its own limit, so it can exceed what the `cgroup:` line's `current` and `limit` imply |
| `purge advice: lazy MADV_FREE (under 70% of the limit)` / `eager MADV_DONTNEED (no headroom known)` `lazy_purges N (B bytes) eager_purges N (B bytes) purged_is_zero true\|false` | which advice the next purge issues and why (the reasons are `explicit purge`, `lazy_band:false`, `under 70% of the limit`, `returning the excess over the cap`, `no headroom known`); `purged_is_zero false` means a lazy batch has happened and `calloc` now clears reused purged memory |
| `thp: <key> -> off\|folios\|chunks (<reason>) [nohuge N (classes, metadata; huge follows host)] [slab_min S pmd P meta M huge on\|off] class_ranges N (B bytes, C chunks) huge_ranges N meta_ranges N failures N; host <mode>` | section 5; the reason text says why `auto` or `always` is inert |
| `arenas[N]:` blocks | `dirty_decay_ms` per arena (`arena.<i>.dirty_decay_ms`), `pactive/pdirty`, the `allocated active dirty retained` totals, the bins table (`size nregs curslabs curregs`, the compaction counters `refilled`, `compacted`) and the slab occupancy histogram (non-empty slabs per eighth of `nslots`), which shows where the live data sits after a delete phase |

A test (`print::tests::text_report_line_set_is_pinned`) pins the heads
of the report's lines, so a renamed or dropped line fails the suite;
quote them exactly. `N` and `<...>` stand for figures and variable text.

RSS by region, outside the report: with `anon_name=yes` the mappings are
named `[anon:challoc:classes]`, `[anon:challoc:huge]` and
`[anon:challoc:meta]` in `/proc/<pid>/smaps`, so `Rss` summed per name
places a gap without a second run. `AnonHugePages` per mapping shows
where huge pages landed (section 5).

## 7. What not to tune, and why

The fast paths of `malloc` and `free` are a thread-local load, a pop or
push, and a bounds check; none of the keys below changes an instruction
on them except by turning on a feature that costs one, and their values
are fixed by measurement:

- `tcache`, `tcache_nslots_small_max`, `lg_tcache_nslots_mul`,
  `tcache_max`: the cache geometry (design 6.2). A smaller, L2-derived
  ladder for small-cache CPUs was priced and did not help the rows it
  targeted. `tcache_nslots_small_max:512` is the measured RSS trade if a
  deployment wants a smaller cache.
- `guards`, `guard_slab_interval`: the hardening the default is measured
  with. The sized free's guard test is a mask, so the interval costs no
  fast-path time at any power of two.
- `zero_on_free`, `slot_randomize`, `pac_pointers`, `double_free_scan`:
  hardening opt-ins whose prices are in section 2. Turn one on for a
  requirement, not for a hunch, and re-measure the consumer.
- `muzzy_decay_ms`, `percpu_arena`: accepted for compatibility, no effect.
- `cgroup:false`: only as a control.
- `check_zero`: a debug mode.

Change `dirty_decay_ms`, `background_thread`, `narenas` (many-thread
processes), `huge_retain_max` and `huge_dirty_decay_ms` (large-buffer
churn), `lazy_band` (RSS-derived metrics under a limit) and `thp`
(section 5) with a measurement in hand; leave the rest.

## 8. Known limitations

Each has an issue with the measurements and the candidate fixes.

- **Ruby threaded RSS on x86_64** (#43): on a 16-thread Ruby soak, peak
  RSS is about 13% above jemalloc's on x86_64 (6% below it on aarch64),
  from 64 KiB-slab holes across many arenas and `Empty` slabs left in
  arenas already purged. `narenas:1` gives the lower peak.
- **PostgreSQL `select500` on aarch64** (#44): pgbench select-only at 500
  clients reads up to 9% behind glibc on the C4A, in a run bound by the
  client's threads sharing the CPUs with the backends; instructions and
  cache misses per transaction are equal. It is ahead on x86_64. To be
  re-measured server-bound.
- **Fork cost of per-class regions** (#45): a fresh process owns dozens
  of class and metadata mappings, so a forked child (a PostgreSQL backend
  per connection) pays more for `fork`, first-touch faults and page-table
  teardown than under glibc: about 40..60% more on a connection-setup
  benchmark. Use a connection pooler (section 3).
- **Valkey write phase on a small-L3 Xeon** (#46): on Sapphire Rapids
  8481C (GCE C3) the Valkey model's write phase has read 15..21% behind
  jemalloc, attributed to the instruction-window limit of an MLP-bound
  loop; two of the named suspects have since been removed, and the gap is
  to be re-measured.
- **Post-delete RSS on 64 KiB-page aarch64** (#47): on a 64 KiB-page
  kernel the Valkey soak's post-delete RSS is about 20% above jemalloc's
  (write phase and peak at parity). The gap is Valkey's defragmenter
  stopping on its absolute `active-defrag-ignore-bytes` threshold, which
  jemalloc's larger slab waste crosses one more time; nothing on the
  challoc side is tunable for it.
