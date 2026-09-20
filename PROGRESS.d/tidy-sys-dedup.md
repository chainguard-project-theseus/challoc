# tidy-sys, batch three: deduplication in sys/, rseq.rs and slice.rs

## Done

- Maintainability pass, batch three (lane tidy-sys, 2026-09-20):
  deduplication on the cold paths of `src/sys/`, `src/rseq.rs`,
  `src/slice.rs` and the body of `api::init`, with the codegen proof and
  a test per behaviour change. (1) `features::Rseq`, `probe_rseq`, the
  `weak_addr!` macro with its three `#[inline(never)]` GOT readers,
  `thread_pointer` and the duplicated `MM_CID_OFFSET`/`CPU_ID_OFFSET`/
  `MM_CID_END` are deleted; `Features.rseq` is `rseq::Probe` and
  `features::rseq_words` derives `rseq=`, `rseq_size=`, `mm_cid=` from it
  (`rseq::BASE_SIZE`, 20, added for the `rseq=` gate), the line
  byte-identical; the dlsym test
  (`rseq_words_match_glibc_symbols_and_the_feature_size`) checks the words
  against glibc's symbols and `AT_RSEQ_FEATURE_SIZE` and that the line's
  probe is the mode's. (2) `src/sys/auxv.rs` (new): the `getauxval` extern
  and `AT_NULL`, `AT_PAGESZ`, `AT_HWCAP`, `AT_HWCAP2` (+
  `AT_RSEQ_FEATURE_SIZE` for tests), replacing the externs and constants in
  features.rs, cpu.rs and pac.rs; test `keys_match_libc_and_the_vector`.
  (3) `sys::read_small_file` retires `features::read_sysfs`,
  `cpu::read_small`'s loop (a two-line slice adapter of that name stays)
  and `cgroup::read_file`; `rseq::possible_cpus` reads through it, so rseq
  no longer imports the cgroup module; `sys::yn` is the one `yes`/`no`;
  `sys::parse_decimal(bytes, Trailing::{Ignore, Newlines})` is the one
  digit loop behind `features::parse_decimal`, `cgroup::parse_u64_prefix`
  (now private) and `cpu::parse_sysfs_dec` (kept as the `u32` narrowing
  over it), every caller's result unchanged (tests per caller kept, plus
  `decimal_prefixes_and_trailing_bytes`,
  `small_files_read_whole_cut_or_not_at_all`). (4) `Features::words`,
  `Thp::words`, `Cpu::words` yield `(key, Value)` pairs; `Display`,
  `FeatureLine` and the tests iterate them (`words_are_the_key_table` pins
  the key list of `tests/c/features.c`; `feature_line_fits` sums the
  words); output identical. (5) `Cpu::probe` and `Features::probe` are
  pure: `lock::suggest_spin_hint(cpu::spin_hint(..))` and
  `cldemote::configure(..)` moved into `api::init` right after
  `Features::probe` and before `config::load` (same order as before, so
  the configured key still wins); `SPIN_TEST_LOCK` stays (config.rs's
  test and the new ones install hints under it); tests
  `probe_has_no_side_effects` and `api::init_installs_the_spin_hint_and_cldemote`.
  (6) `rseq_enter!()`/`rseq_abort!()`: the ten asm blocks' identical
  descriptor prologue and per-arch abort handler as `concat!` strings;
  every rseq function exact or reloc-only in the compare. (7) Behaviour
  fix asked for by the human: `Cgroup::events()` returns all five
  `memory.events` counters (`Cgroup::note_events` split out of
  `sample_now`); `Events` documents the format (`oom_kill` absent before
  4.13 reads 0, `oom_group_kill` ignored); tests `events_files` (pre-4.13
  fixture), `sampled_events_carry_every_counter`, and the discover test
  cross-checks each counter against std. (8) `slice.rs`'s three syscall
  numbers are `nr::SCHED_YIELD` (24/124) and `nr::RSEQ_SLICE_YIELD` (471)
  in `TABLE` and `tests/c/sysnr.c`; `sys::sched_yield` and
  `sys::rseq_slice_yield` wrap `raw::syscall0`; the two `asm!` blocks in
  slice.rs are gone; test `yields_answer_zero_or_enosys`.
  Codegen (`scripts/codegen-diff.sh compare .tmp-codegen`, baseline
  captured at 29c0eda): q16 and q8 both `functions old 435 new 427: exact
  41, reloc-only 378, changed 8`, the same eight in both variants, every
  one in the lane's files or `api::init`: `sys::cpu::linux::Cpu::probe`
  (the `suggest_spin_hint` call gone, task 5),
  `sys::cgroup::linux::Cgroup::sample_now` (five counters stored, task
  7), `api::init` (the two installs added, task 5),
  `sys::cgroup::linux::read_limit` (the shared `parse_decimal` loop
  inlined, task 3), `<Features as Display>::fmt` and `<Value as
  Display>::fmt` (the key table, task 4), `<Cgroup as Display>::fmt`
  (`events()` loads five atomics, task 7), and `<bool as Display>::fmt`
  (one trailing alignment `nop`, no instruction changed). Eight functions
  gone to dead-code removal (the three GOT readers, `probe_rseq`, the
  `Display`s of `Rseq`, `ThpDisable`, `SliceExt`, `cpu::Size`, `Thp`, `Cpu`
  now unused in the library). Host `cargo test --features std`: 345
  passed (341 before). `make test` in the container: 449 + 134 + 26
  passed (441 + 133 + 25 on main; the extra cgroup-step test is
  `sampled_events_carry_every_counter`), the cgroup churn, lazy and
  pagemap-guard steps `ok`, zero warnings.
  Commits abe7d13 (sys helpers, auxv, nr rows), 34b1d20 (slice), 9f1b60c
  and b731cfb (cgroup), ee5f60b (key table, Rseq deleted), 1b2353a (pure
  probes, init), 92a5a27 (rseq macros).

## Notes for the human

- Dropped from task 6: passing `pac` to the x86_64 rseq blocks to lose
  the `let _ = pac` lines. An `in(reg)` input the block never reads still
  costs a register and a move, so the x86_64 sections would not stay
  exact; the three lines stay.
- `config.rs` (another lane's file) still declares its own `getauxval`
  extern and `AT_SECURE` (lines 1050-1051, 1076); with `sys::auxv` merged
  it can `use crate::sys::auxv::getauxval` and take `AT_SECURE` (23) as a
  constant there. Its `spin_hint_key_installs_where_available` test still
  ends with a `Cpu::probe` "kept against the probe's suggestion" check
  that is now trivially true (the probe suggests nothing); it can go.
- One documented semantic edge in the `rseq=` word: a `cpu_id` of -2
  (`RSEQ_CPU_ID_REGISTRATION_FAILED`) read as unregistered before and
  reads as registered now, the way `rseq::Probe::registered` already
  treated it for the mode. glibc sets `__rseq_size` to 0 when its
  registration fails, so no real line changes; noted in `rseq_words`'s
  doc.
- `docs/design.md` (not the lane's file) names `sys::features::Rseq` at
  lines 2089 and 2209; both should now read `sys::features::rseq_words`
  over `rseq::probe` ("one probe, read once at init, for the report and
  the mode"). Design 19 needs one new line:

      src/sys/auxv.rs       getauxval and the AT_* keys (AT_PAGESZ, AT_HWCAP, AT_HWCAP2)

  and the `src/sys/mod.rs` line could add "read_small_file, parse_decimal,
  yn, sched_yield, rseq_slice_yield".
- `init_installs_the_spin_hint_and_cldemote` lives in a module named
  `api` inside `sys::cpu::linux_tests` so the Makefile's `--skip api::`
  filter runs it in the second test process (the one that initialises the
  real allocator). If the Makefile's split ever changes shape, that test's
  module name is the coupling to update.
