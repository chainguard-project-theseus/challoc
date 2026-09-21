# The frame of the sized path (lane sized-path, task 2), x86_64

Date 2026-09-21. Lane `sized-path`, lane VM `challoc-sized-path-amd64`
(c4-standard-16, Xeon Platinum 8581C, Emerald Rapids, PMU exposed, kernel
7.0.0-1011-gcp) and a throwaway `matrix lab cpu-c3` cell (c3-standard-8,
Xeon Platinum 8481C, Sapphire Rapids, no PMU; the small-L3 host where the
instruction window binds, `2026-09-19-8481c-valkey1-attribution.md`
section 8). Parent 63bd7ee (task 1 of this lane); the aarch64 file
(`2026-09-21-aarch64-sized-path-frame.md`) has the C4A numbers.

## 1. The mechanism

Phase 2.5 task 4c left challoc's Valkey SET at 344.6 instructions on the
C4 (jemalloc 268.2; target under 330) and named the next term: the inlined
push of `sdallocx` and `free` calls `cached_twice(p)` on a
`double_free_scan` hit, a diverging cold call, and one call in the body
makes the compiler keep a frame the default path never uses (7
instructions per call on x86_64, the `stp`/`ldp` of `x29`, `x30` on
aarch64); two `sdallocx` per SET, 14 instructions. The same shape sits in
`malloc`: the inlined pop calls `pop_compact` for the scheduled pop (design
6.4), a real call, so `malloc` keeps a frame too, two per SET.

The change (`src/heap/mod.rs`, code shape only, no check weakened):

- `Heap::push_cache`: on a scan hit the push declines (`false`) without
  storing the second copy, the same outcome as a full cache; the caller's
  cold slow path (`Heap::free_slow_in`) repeats the compare
  (`Heap::cached_recently`, the four-entry test shared with the push)
  before its `tick` and flush can move an entry, and aborts with the
  same message. `double_free_cached` and `sized_double_free` under
  `double_free_scan:true` still abort at the call with "already in the
  thread cache" (tests/c/negative.c through src/api/ctests.rs);
  `double_free_straddle` and the default `double_free_scan:false` path
  are untouched.
- `Heap::pop_fast`: the scheduled pop (`(allocs + 1) & compact_mask ==
  0`) is declined (`None`, `allocs` not counted) and served by
  `Heap::alloc_slow`, the one place every caller's `None` lands (a
  non-empty home cache there means a declined schedule, not an empty
  cache); `pop_compact` itself and the `pop` the cold paths use are
  unchanged. The compaction tests (`heap::tests::compact*`,
  `scheduled*`, `probe*`) pass unchanged.

Static count of the exported fast paths (`objdump`, the whole function;
frame = a `stp x29, x30` / `push` prologue; calls = `bl`/`call` in the
body, tail jumps excluded), aarch64 in the Mac container, parent 63bd7ee
against the change:

| function | parent insns | frame | call in body | new insns | frame | call in body |
|---|---:|---|---|---:|---|---|
| `malloc` (`je_malloc`) | 70 | yes | `pop_compact` | 62 | no | none |
| `free` (`je_free`) | 149 | yes | `cached_twice` | 145 | no | none |
| `sdallocx` (`je_sdallocx`) | 192 | yes | `cached_twice` | 182 | no | none |
| `challoc_malloc_usable` | 84 | yes | `pop_compact` | 68 | no | none |
| `challoc_free_usable` | 146 | yes | `cached_twice` | 143 | no | none |

(x86_64 static counts: section 2, from the lane VM.)

## 2. x86_64 static shape (the lab cell, `target/quick/{old,new}`, `objdump`)

| function | parent insns | prologue | call in body | new insns | prologue | call in body |
|---|---:|---|---|---:|---|---|
| `malloc` (`je_malloc`) | 59 | `push` x1 | `pop_compact` | 52 | none | none |
| `free` (`je_free`) | 141 | `push` x1 | `cached_twice` | 139 | `push` x1 | none |
| `sdallocx` (`je_sdallocx`) | 198 | `push r15, r14, rbx` | `cached_twice` | 201 | `push r15, r14, rbx` | none |
| `challoc_malloc_usable` | 87 | `push` x3 | `pop_compact` | 57 | none | none |
| `challoc_free_usable` | 143 | `push` x1 | `cached_twice` | 143 | `push` x1 | none |

The finding that corrects task 4c's note: on x86_64 the `sdallocx` frame
was never the call. The three callee-saved registers the parent pushes
hold `Shared::sized_slot`'s temporaries (`r14` = `p - class_base[c]`,
`rbx` = `&CLASSES[c]`, `r15` = the slot mask) while `p`, `size`, the
two words of the `Option<usize>` hint, the flag byte, the heap and the
cache header are live in the nine caller-saved registers; with the call
gone the allocator still needs them, so the pushes stay (a masked diff
of the two functions: the default path is instruction-for-instruction
the same, the +3 is a duplicated `pop; pop; pop; ret` epilogue where the
scan branch used to fall into the abort call). `malloc`, whose body
needs fewer live values, does lose its frame (-7 per call, 14 per SET),
and so does `challoc_malloc_usable` (-30: three pushes and the
`pop_compact` call's argument marshalling). On aarch64 (31 registers) the
frame of all five was the `stp x29, x30` a call requires, and every one
is frameless now (section 1's table). The remaining x86_64 term for the
sized path is therefore register pressure, and the cheapest lever the
4c file already named: the two-register `Option<usize>` hint (a
one-word encoding with `NCLASSES` for "huge" would free one register on
the whole path; it changes `class_for`'s callers in `libc.rs`,
`jemalloc.rs`, `heap/shared.rs` and `heap/percpu.rs`, which the
per-CPU lane is editing, so it is left for after the merge).

## 3. The C3 lab (8481C, 8 vCPU, no PMU): `quick` and the write-phase rate

`challoc-bench matrix lab cpu-c3` (`challoc-sized-path-matrix-cpu-c3`,
c3-standard-8, kernel 7.0.0-1011-gcp), tree synced with the change,
`make quick OLD=HEAD` (old = 63bd7ee, new = the change, jemalloc 5.3.1):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 40.1 | 40.6 | 41.4 | -1.2% | +2.0% |
| larson10 | M ops/s | 149.5 | 154.1 | 167.6 | -3.0% | +8.8% |
| xmalloc | M frees/s | 154.078 | 157.148 | 160.446 | -2.0% | +2.1% |
| alloc-test1 | s | 3.813 | 3.769 | 3.589 | -1.2% | +5.0% |
| valkey1 | M ops/s | 7.973 | 8.188 | 9.898 | -2.6% | +20.9% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6693 (49.7%) | compacts | |

`quick: go` (new within 5% of old on every row, churn compacts; the
`new vs old` column is `quick.sh`'s sign, negative = new ahead).

The write-phase rate, the 4c protocol (`valkey_model -n 850000 -w 8 -d 0
-s`, three interleaved runs per variant, M SET/s):

| run | old (63bd7ee) | new | jemalloc |
|---|---:|---:|---:|
| 1 | 8.266 | 8.446 | 9.809 |
| 2 | 8.348 | 8.064 | 9.687 |
| 3 | 8.272 | 8.344 | 9.822 |
| median | 8.27 | 8.34 | 9.81 |

New +0.9% on the median, inside the spread (the three new readings
straddle the old ones): on this host the change does not move the rate.
Two things the table says beyond this task. First, on x86_64 the change
takes 14 instructions per SET out of `malloc` and none out of `sdallocx`
(section 2), and 14 of about 420 is under the C3's resolution. Second,
this cell reads challoc 15..21% behind jemalloc on the write phase where
`2026-09-19-x86_64-gce-sized-path.md` section 2.4 read (b) at +3.2%
ahead on the same machine type: the C4 `quick-perf` of this lane
(`2026-09-21-x86_64-sized-path-usable.md` section 4) has `valkey1-write`
at 423 instructions per SET for 63bd7ee against 344.6 for (b) then, and
jemalloc at 330.6 against 268.2, so about 60 of the 78 are on the
program side (the dev image was rebuilt on a fresh VM: a newer gcc and
Wolfi jemalloc; the model gained two flag tests per allocation and free
for the `-a`/`-p` rows) and about 16..22 are challoc's own since (b)
(what landed on `main` between a7059bb and 2078f67 on the entry points:
the per-CPU dispatch test and its tail calls, plan Phase 2.5 task 8),
which on this host is the difference between under and over the
instruction-window cliff the 8481C file locates at 350..430. Not this
lane's change, but the coordinator should know the C3 row is back
behind jemalloc; the instruction accounting of the entry points since
task 8 is the place to look.


## 4. `quick-perf` on the C4 (`old` = 63bd7ee, `new` = the change, jemalloc; one run per cell)

Two states were measured. First 97859aa alone (the scan hit and the
scheduled pop decline, `malloc`'s declined arm still going through
`malloc_slow(size)` -> `heap::alloc`):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1-write | old | 12.19 | 276.2 | 423.2 | 4.639 | 0.947 |
| valkey1-write | new | 11.74 | 286.9 | 421.6 | 4.550 | 0.940 |
| valkey1-write | jemalloc | 12.05 | 279.2 | 330.6 | 4.821 | 1.032 |
| valkey1-write-pair | old | 9.52 | 353.7 | 442.6 | 6.961 | 0.987 |
| valkey1-write-pair | new | 10.02 | 335.6 | 429.8 | 6.872 | 0.986 |
| valkey1 | old | 20.27 | 165.8 | 270.9 | 3.269 | 0.898 |
| valkey1 | new | 20.11 | 167.3 | 275.2 | 3.299 | 0.897 |
| valkey1 | jemalloc | 23.56 | 142.7 | 184.0 | 3.333 | 0.952 |
| valkey1-free | old | 19.69 | 171.0 | 274.3 | 4.570 | 0.928 |
| valkey1-free | new | 19.30 | 174.5 | 272.9 | 4.522 | 0.918 |
| valkey1-free | jemalloc | 20.96 | 160.6 | 189.4 | 4.694 | 1.212 |
| larson1 | old | 43.72 | 74.3 | 243.9 | 1.645 | 0.002 |
| larson1 | new | 43.78 | 74.2 | 241.3 | 1.630 | 0.002 |
| larson1 | jemalloc | 44.37 | 72.6 | 200.9 | 2.119 | 0.001 |

The `valkey1-write-pair` row gains 12.8 instructions per SET (the
frameless `challoc_malloc_usable`, -30 static), `larson1` 2.6, but
`valkey1-write` only 1.6 (section 2: `sdallocx` keeps its pushes on
x86_64 and `malloc`'s frame was one push) and the mixed `valkey1` row
pays **+4.3 instructions per operation**: in its delete phase the
compacting mode schedules one pop in eight, and a declined scheduled pop
went `malloc` -> `malloc_slow(size)` -> `class_of` again -> `heap::alloc`
-> a second `pop_fast` (declined again) -> `alloc_slow`, about 35
instructions per scheduled pop. Hence 9113e9c: `malloc` and
`challoc_malloc_usable` tail-call their class-known slow path, which
reaches `Heap::alloc_slow` through `heap::alloc_declined` (a heap
lookup, no second pop). The same rows at 9113e9c:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1-write | old | 12.36 | 272.3 | 423.0 | 4.581 | 0.943 |
| valkey1-write | new | 12.05 | 279.0 | 423.3 | 4.599 | 0.951 |
| valkey1-write | jemalloc | 12.28 | 273.9 | 330.4 | 4.859 | 1.033 |
| valkey1-write-pair | old | 9.63 | 350.2 | 442.4 | 6.956 | 0.983 |
| valkey1-write-pair | new | 10.51 | 320.4 | 429.1 | 6.957 | 0.988 |
| valkey1 | old | 20.50 | 164.0 | 273.9 | 3.285 | 0.899 |
| valkey1 | new | 19.67 | 170.9 | 274.7 | 3.299 | 0.906 |
| valkey1 | jemalloc | 23.56 | 142.7 | 183.8 | 3.327 | 0.949 |
| valkey1-free | old | 19.47 | 173.0 | 273.9 | 4.558 | 0.922 |
| valkey1-free | new | 19.56 | 172.2 | 274.5 | 4.555 | 0.928 |
| valkey1-free | jemalloc | 21.08 | 159.7 | 189.5 | 4.707 | 1.208 |
| larson1 | old | 44.08 | 73.2 | 244.2 | 1.596 | 0.002 |
| larson1 | new | 44.67 | 72.6 | 241.3 | 1.594 | 0.002 |
| larson1 | jemalloc | 41.89 | 77.1 | 200.9 | 2.127 | 0.001 |

`valkey1` is back to +0.8 instructions per operation (the heap lookup
and `alloc_slow`'s two routing tests per scheduled pop, one in eight),
`valkey1-free` +0.6, `larson1` -2.9, `valkey1-write-pair` -13.3,
`valkey1-write` +0.3: on x86_64 the frame change is worth nothing on the
plain and sized `free` (the pushes stay, section 2) and about one
instruction per `malloc`, the tail call's argument having replaced the
one push/pop pair. The cycle columns of the two write rows read `new`
2.5..3.9% slower in both single runs and `valkey1` +0.9% and +4.2%,
`larson1` -0.1..-0.8% faster, `valkey1-free` within 0.5%: the C4's
round-to-round band for these rows is about 5% (`2026-09-19-x86_64-gce-sized-path.md`
section 2.3), and the instruction columns, which are exact, move by at
most 0.3 per SET on the write row; the multi-round tiers below are the
measure.

## 5. `quick` on the C4 (`OLD=63bd7ee`, one round, the gate)

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.9 (17.0 MiB) | 44.9 (17.1 MiB) | 44.7 (17.9 MiB) | -2.2% | -0.4% |
| larson10 | M ops/s | 263.9 (93.9 MiB) | 266.4 (96.1 MiB) | 278.1 (91.3 MiB) | -0.9% | +4.4% |
| xmalloc | M frees/s | 332.085 (52.6 MiB) | 316.616 (52.8 MiB) | 322.426 (91.1 MiB) | +4.9% | +1.8% |
| alloc-test1 | s | 3.436 (14.1 MiB) | 3.404 (14.0 MiB) | 3.236 (13.1 MiB) | -0.9% | +5.2% |
| valkey1 | M ops/s | 11.354 (585.0 MiB) | 12.068 (585.0 MiB) | 12.212 (591.8 MiB) | -5.9% | +1.2% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6655 (50.0%) | new compacts (end occupancy >= 90%) | 20717708 ops/s |

`quick: go` (new within 5% of old on every row; churn compacts;
`quick.sh`'s sign: negative = new ahead). `valkey1` +6.3% ahead and
`xmalloc` 4.9% behind on their one round each, both inside the band the
row shows between rounds; `ab` below has five.

## 6. `ab`, the protocol tier (C4, 5 rounds + warm-up, interleaved, `old=63bd7ee new=9113e9c jemalloc`)

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.5 | 16.8 | 43.7 | 17.0 | 44.3 | 18.0 | +1.8% | +0.5% |
| larson10 | M ops/s | 271.1 | 94.8 | 269.7 | 95.1 | 281.2 | 90.6 | +0.5% | -3.6% |
| larson16 | M ops/s | 297.6 | 116.0 | 296.5 | 120.3 | 327.7 | 126.6 | +0.4% | -9.2% |
| xmalloc | M frees/s | 317.593 | 53.4 | 314.740 | 53.1 | 320.368 | 92.9 | +0.9% | -0.9% |
| alloc-test1 | s | 3.428 | 13.9 | 3.410 | 13.9 | 3.230 | 13.3 | -0.5% | -5.8% |
| alloc-testN | s | 4.727 | 17.3 | 4.669 | 17.3 | 4.346 | 17.0 | -1.2% | -8.1% |
| valkey1 | M ops/s | 12.216 | 585.4 | 12.202 | 585.4 | 12.228 | 592.3 | +0.1% | -0.1% |
| valkey1-free | M ops/s | 11.393 | 585.4 | 11.706 | 585.5 | 10.903 | 592.3 | -2.7% | +4.5% |
| valkey1-write | M ops/s | 12.141 | 585.8 | 12.164 | 585.9 | 12.257 | 593.0 | -0.2% | -0.9% |

(`new vs old` in `ab.sh`'s sign: positive = new worse.) Every row within
2%, the signs mixed: `larson1` +1.8% (one of new's five rounds read 41.6
against a 43.6..44.3 band, the medians 44.5 against 43.7), `larson10`
+0.5%, `larson16` +0.4%, `xmalloc` +0.9%, `alloc-test1` -0.5%,
`alloc-testN` -1.2%, `valkey1` +0.1%, `valkey1-free` -2.7% (new ahead;
old's list carries an 8.96 outlier), `valkey1-write` -0.2%. The sorted
lists:

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 43.8 44.4 44.5 44.5 44.6 | 17244 |
| larson1 | new | 41.6 43.6 43.7 44.3 44.3 | 17388 |
| larson1 | jemalloc | 43.8 44.0 44.3 44.5 44.8 | 18456 |
| larson10 | old | 265.6 269.9 271.1 271.4 279.9 | 97120 |
| larson10 | new | 258.0 267.4 269.7 270.5 271.8 | 97344 |
| larson10 | jemalloc | 277.8 277.9 281.2 285.4 293.2 | 92820 |
| larson16 | old | 292.6 296.2 297.6 301.5 302.7 | 118756 |
| larson16 | new | 294.1 295.1 296.5 299.3 306.4 | 123228 |
| larson16 | jemalloc | 317.6 321.0 327.7 329.8 330.6 | 129612 |
| xmalloc | old | 311.743 315.842 317.593 318.558 323.138 | 54640 |
| xmalloc | new | 308.824 313.963 314.740 318.020 329.124 | 54324 |
| xmalloc | jemalloc | 317.814 318.534 320.368 322.329 323.778 | 95148 |
| alloc-test1 | old | 3.424 3.425 3.428 3.429 3.431 | 14264 |
| alloc-test1 | new | 3.405 3.406 3.410 3.413 3.413 | 14260 |
| alloc-test1 | jemalloc | 3.228 3.229 3.230 3.231 3.245 | 13636 |
| alloc-testN | old | 4.684 4.694 4.727 4.731 4.814 | 17684 |
| alloc-testN | new | 4.645 4.665 4.669 4.683 4.683 | 17736 |
| alloc-testN | jemalloc | 4.316 4.336 4.346 4.426 4.462 | 17376 |
| valkey1 | old | 12.153 12.197 12.216 12.231 12.235 | 599420 |
| valkey1 | new | 12.130 12.132 12.202 12.239 12.278 | 599484 |
| valkey1 | jemalloc | 11.938 11.982 12.228 12.245 12.286 | 606544 |
| valkey1-free | old | 8.961 11.293 11.393 11.451 11.889 | 599472 |
| valkey1-free | new | 11.458 11.629 11.706 11.798 11.922 | 599508 |
| valkey1-free | jemalloc | 10.505 10.885 10.903 11.007 11.186 | 606532 |
| valkey1-write | old | 11.975 12.042 12.141 12.165 12.177 | 599904 |
| valkey1-write | new | 12.075 12.077 12.164 12.219 12.272 | 599944 |
| valkey1-write | jemalloc | 12.164 12.172 12.257 12.286 12.330 | 607280 |

## 7. Conclusion (x86_64)

- The two calls are off the fast paths (`push_cache` declines on a scan
  hit, `pop_fast` declines the scheduled pop, the slow paths repeat the
  compare and serve the pop) with the detection unchanged: the negative
  modes under `double_free_scan:true` abort at the call by name, the
  straddle and the default path are untouched, the compaction tests pass.
- On x86_64 the change is neutral: `malloc` and `challoc_malloc_usable`
  lose their frame and call (52 and 57 instructions static), but the
  `sdallocx` and `free` frames were register pressure and stay (section
  2), so `valkey1-write` reads 423.0 -> 423.3 instructions per SET, the
  cycle rows of `quick-perf` read 2.5..3.9% either way between single
  runs, `quick` is go and `ab`'s nine rows are within 2% with mixed
  signs. The 330-instruction target of task 4c is not approached by this
  change on x86_64; the accounting names what is: the three
  callee-saved temporaries of `sized_slot` (the two-register hint is the
  lever) and, since task 8, the per-CPU dispatch on every entry point
  (section 3, the C3 observation).
- On aarch64 (the other file) every one of the five entry points is
  frameless and the gain is real: `larson1`/`larson10`/`larson16` +1.4%
  with non-overlapping five-round lists, `xmalloc` +2.2%, `valkey1-write`
  -5 instructions per SET.
