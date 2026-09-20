## Done

- Phase 2.5 task 16 (lane B, follow-up: the human's decision on the `thp`
  default, PROGRESS.md "Phase 2.5 defaults, the human's decisions
  (2026-09-20", item 1; option (d) of
  `bench/results/2026-09-20-aarch64-cgos-thp-ab.md`): done (4678fcd).
  `thp:never` (and a forced `never`) issues `MADV_NOHUGEPAGE` on the class
  superblock and the metadata region only; the huge region is neither
  advised nor recorded for `force_never`, so it follows the host's THP
  policy on every path. `thp::Reservation { Classes, Huge, Metadata }`
  names the mapping at the one call site (`layout::Regions::reserve`, task
  16's own loop; the only change outside lane B's partition in `src/`
  besides the report line in `src/api/print.rs`, the `stats.thp.
  nohuge_ranges` leaf in `src/api/mallctl.rs` and a doc comment in
  `src/api/ctests.rs`), `thp::is_never()` is the one predicate, the
  `thp:` report line reads `nohuge 2 (classes, metadata; huge follows
  host)` under `never` (the JSON object gains `"nohuge_ranges"`),
  `stats.thp.nohuge_ranges` is readable, and `tests/c/thp.c never` asserts
  `nh` on the class region, no `nh` on the 8 MiB huge run, `nohuge_ranges`
  2 and, on an `always` host, `AnonHugePages` 0 on the class region with
  `THPeligible` 1 and at least 2 MiB of `AnonHugePages` on the huge run
  (the Mac dev container's kernel runs `always`, so that row ran both ways
  here: class 0 kB, huge run 8192 kB). `auto` and `always` unchanged;
  nothing on a fast path (one `madvise` fewer at init). Tests: host
  `cargo test --features std` 319 passed; Mac container `make test` 411 +
  120 + 23 passed, 58 C-test `ok` lines; `make test` on
  `challoc-lane-b-arm64` and `challoc-lane-b-amd64` (Ubuntu 24.04 hosts,
  global mode `madvise`: the `never` test there is the `nh` / no-`nh`
  row): 411 + 120 + 23 passed on each, 59 C-test `ok` lines each. `quick arm64` against `main` 4a7d30c: go, every row
  within 1% (`valkey1` +1.0% on one round, RSS identical everywhere), in
  `bench/results/2026-09-20-aarch64-lane-b-thp-decision-quick.md`.
  Documents: `docs/design.md` sections 11, 12 (the `thp` row), 16 (the
  report line and the leaf) and 17 (the THP row), README's THP row,
  `docs/plan.md` task 16 (a parenthetical narrowing the acceptance to the
  slab regions), the module doc of `src/thp.rs` and the header of
  `tests/c/thp.c`.

## Design decisions

- Phase 2.5 task 16 (lane B, 2026-09-20): `never` is the slab regions' RSS
  discipline; the huge tier follows the host, as jemalloc's does. The
  class superblock and the metadata region are committed a few slabs or
  pages at a time, so a 2 MiB page under them is RSS the program never
  asked for (and the fork copy-on-write cost Redis and Valkey forbid),
  which is what `MADV_NOHUGEPAGE` there prevents on an `always` host. A
  huge run is a whole allocation of a page or more the program asked for;
  jemalloc's `never` advises its slab extents and lets its large extents
  take what the host gives, and challoc did the same before task 16: the
  32 MiB of tables the Valkey model keeps in huge runs took 2 MiB pages
  and their loss was 10 points of `valkey1` on Chainguard OS for no RSS
  gain (585 MiB either way). So the huge reservation is skipped by kind,
  not recorded for `force_never` (a forced `never` behaves as a configured
  one), and the report line names what `never` covers so a reader on an
  `always` host is not surprised by `AnonHugePages` on the huge region.
  The alternatives and why not: `thp:always` as the default on `always`
  hosts buys parity with jemalloc for +9.5% RSS on the 585 MiB heap and
  +40% on larson's retained memory; a size-banded `always` adds a
  threshold with no row behind it; and `thp_process:advised` stays the
  key for "no huge page anywhere in the process", which `never` no longer
  claims for the huge region.

## Notes for the human

- Phase 2.5 task 16 (lane B): lane E's next cgos iteration, launched with
  4678fcd (this task's code commit), is the `always`-host measurement; the expectation to
  check against `2026-09-20-aarch64-cgos-thp-ab.md` is that `valkey1` on
  `cgos-arm64` under the shipped default reads as that file's option (b)
  (`thp:auto`, no advice either way): about +6% behind jemalloc instead of
  the +16% of option (a), `valkey1-free` about +1%, larson16 unmoved (+5..6%
  with 175 MiB), with `AnonHugePages` about 32 MiB at the Valkey model's
  peak (the tables in huge runs; `thp_fault_alloc` about 32 per run, up
  from 12) and 0 on larson16 (no huge runs; the 2..4 MiB the (a) row showed
  there are the program's own mappings), peak RSS 585 MiB on the Valkey
  rows as before. `tests/c/thp.c never` on that host must print
  `AnonHugePages 0 kB (class region), 8192 kB (huge run)`; the Mac
  container's `always` kernel already printed exactly that.
