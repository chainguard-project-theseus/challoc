## Done

- Per-CPU dispatch (lane percpu-dispatch, 2026-09-21) DONE, commits
  da39e2e (the shape), dd7f9b5 (design 15, tuning row), cc547ed (the
  mode's hit paths frameless), e088459 and 051780d (results). The
  per-CPU mode's dispatch leaves the entry points: a heap in the mode
  never installs a home cache (`Heap::ensure_cache` refuses its home
  arena), so every home header stays `CacheHeader::EMPTY` and
  `malloc`/`free`/`sdallocx` fall through on the `len == 0` / `len ==
  cap` test they already have to `malloc_slow`/`free_slow`, where
  `Heap::alloc`, `alloc_zeroed` and `free` route on `Heap::percpu` to
  `percpu_alloc`/`percpu_free` (shape (a) of the brief). `PUSH_PERCPU`,
  the entry points' `tbnz` and tail calls and the five wrappers are gone;
  `malloc`, `malloc_fast`, `free`, `free_sized_hint`, `Heap::alloc` and
  `Heap::alloc_zeroed` are their pre-4864d4b bodies. Codegen
  (`scripts/codegen-diff.sh` 2078f67 -> tree): aarch64 `malloc` 70 -> 65,
  `free` 149 -> 144, `sdallocx` 192 -> 180, `free_sized` 151 -> 142,
  `calloc` 222 -> 132, `operator new[]` 153 -> 66 instructions,
  `malloc_usable_size` 83 unchanged; x86_64 `malloc` 59 -> 55, `free` 141
  -> 130, `sdallocx` 198 -> 180, `calloc` 195 -> 128. `quick-perf`
  (lane VMs, perf on the host): aarch64 larson1 264.5 -> 255.5
  instructions per operation (the drift file's pre-4864d4b 255.4) and
  72.1..72.3 -> 71.2..71.6 cycles (its pre-4864d4b band 70.9..71.2),
  valkey1 245.2 -> 241..244, valkey1-free 247..249 -> 244, misses
  unchanged; x86_64 larson1 244.4 -> 236.2 instructions, valkey1 234..240
  -> 228..233, cycles within the runs' spread. `quick` GO on both.
  `ab` (5 rounds, same run, old/new/jemalloc plus both under
  `percpu_arena:percpu`): aarch64 `new` ahead of `old` on every row
  (larson1 -0.9% with disjoint sorted lists, larson10 -1.8%, larson16
  -0.9%, xmalloc -0.5%, alloc-test1/N -0.8%, valkey1 -0.7%, valkey1-free
  -0.4%), larson1 6.4% behind jemalloc (the pre-4864d4b band +5.9..+6.6%,
  post +7.5..+8.5%); x86_64 within noise both ways (larson1 -2.9%,
  larson10 -0.6%, larson16 +0.2%, xmalloc -0.6%, alloc-test1 +2.4%,
  alloc-testN -0.4%, valkey1 0.0%, valkey1-free +1.1%). The opt-in mode
  is slower under this shape: aarch64 larson1 311 -> 438 instructions
  and 75.5 -> 119.5 cycles per operation, `ab` 39.8 -> 25.1 M ops/s,
  valkey1 6.63 -> 5.55; x86_64 larson1 42.6 -> 25.2, valkey1 11.6 -> 8.5
  (`bench/results/2026-09-21-{aarch64,x86_64}-percpu-dispatch.md`,
  `-ab.md`, sections 0, 2 and 4). `make test` green on both lane VMs at
  dd7f9b5 and cc547ed (481 + 138 + 26, churn/lazy/pagemap ok,
  `c_percpu_both_ways` with the mode on, ids=16); host `cargo test
  --features std` 375. VMs `challoc-percpu-dispatch-{arm64,amd64}`
  created, used and deleted by the lane.

## Design decisions

- Per-CPU dispatch (lane percpu-dispatch, 2026-09-21), the shape: the
  mode rides the `EMPTY` home header rather than a flag bit, a TLS
  sentinel or an ifunc. Reason: `len == 0` and `len == cap` are the two
  branches the fast paths take anyway, so the default path is byte for
  byte the allocator without the mode (measured: the pre-4864d4b
  instruction counts and cycle band come back on the C4A); the TLS
  sentinel would have needed a second slot for the heap the mode still
  uses (byte counters, rseq area); an ifunc would have needed the
  configuration and the rseq probe readable before relocation and a
  proof on eight loading shapes, for no gain over this on the default
  path. The invariant the shape rests on, "a heap with `percpu` set has
  no home cache", is enforced in one place (`Heap::ensure_cache`) and
  asserted by `percpu_caches_serve_plain_allocations` (`cache(c).is_none()`).
- Per-CPU dispatch (lane percpu-dispatch, 2026-09-21), the mode pays the
  slow-path prologue: in the mode `malloc`/`free` now cost the entry
  point's wasted inlined fast path plus `malloc_slow`/`free_slow`'s
  prologue and the exact `classify_free` before the same section
  (aarch64 larson1 +127 instructions and +58% cycles against the
  tail-call wrapper, valkey1 +16%). Reason: the brief ranks the default
  path's cost first and asks that the mode keep working, which it does
  (tests, campaign script unchanged); the mode is opt-in and was 4..38%
  behind thread caches on every `ab` row before this; the only ways to
  hand the mode a cheaper path are an extra argument on the default
  path's tail calls (a register move every default `free` pays, not
  measured) or the ifunc, both recorded under Notes.
- Per-CPU dispatch (lane percpu-dispatch, 2026-09-21), an explicit
  `MALLOCX_ARENA(home)` request on a heap in the mode is served directly
  from the slab (`ensure_cache` returns null for the home arena) instead
  of through a home cache. Reason: a home cache on such a heap would be
  popped by the entry point's fast path and bypass the CPU caches; the
  request is a corner of a corner (an explicit arena equal to the
  thread's automatic home, under an opt-in mode) and direct is the honest
  answer. `Shared::checked_slot_any` is test-only now (its one production
  caller was the removed push wrapper); `rseq::POP_OFF_MODE` /
  `PUSH_OFF_MODE` are gone with it.

## Notes for the human

- Per-CPU dispatch (lane percpu-dispatch, 2026-09-21): the opt-in mode
  is slower under this shape (numbers above and in the results files).
  If the mode ever gets an adopter, the two ways to give it back its
  +33..47-instruction hit path without touching the default: (b) of the
  brief, ifunc-selected entry points (resolver runs before relocation:
  needs the configuration and the rseq probe read without GOT loads, and
  the loader-order proof on both archs), or a `class`/heap argument on
  the entry points' cold tail calls (a register move on every default
  `free`, to be priced with `quick-perf` before it is taken). Nothing to
  decide for the default.
- Per-CPU dispatch (lane percpu-dispatch, 2026-09-21): design 15 and
  `docs/tuning.md`'s `percpu_arena` row now describe this shape; design
  6.3 needed no change (the `EMPTY` header is already its slow-path
  rule). No packaging change: `percpu_arena:percpu` stays selectable at
  run time in the same binary.
