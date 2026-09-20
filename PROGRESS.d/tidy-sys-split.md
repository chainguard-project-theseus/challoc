# tidy-sys, batch two: sys/abort.rs and the constants hoist

## Done

- Maintainability pass, batch two (lane tidy-sys, 2026-09-20): code motion
  in `src/sys/` with no machine-code change. `src/sys/abort.rs` (new)
  holds `Stderr`, `abort`, `abort_fmt`, `die`, the `abort!` macro and the
  private `KernelSigaction` only `die` uses, moved verbatim from
  `mod linux` in `src/sys/mod.rs` with their six tests (the child hook is
  now `sys::abort::tests::abort_child_hook`); `sys/mod.rs` re-exports it
  with `pub use abort::*`, so `sys::abort_fmt`, `sys::Stderr`,
  `sys::abort` and `crate::abort!` resolve as before (lib.rs's panic
  handler, api/print.rs, config.rs untouched). The constants block
  (`PROT_*`, `MAP_*`, `MADV_*`, `PR_*`, futex, clock, `O_*`, `EFD_*`,
  `POLL*`, `AT_FDCWD`, `PIDFD_SELF`, signals, `STDERR_FILENO`) moved out of
  `mod linux` to the top of `sys/mod.rs`, values and the batch-one
  `#[cfg(test)]` gates unchanged, so host builds see them; `mod linux`
  imports the ones it uses by name. Codegen (`scripts/codegen-diff.sh
  compare .tmp-codegen`, baseline 0f5dee4 at ea9532a): q16 and q8 both
  `exact functions 435, reloc-only 0, changed 0`, `.text=03bea0` (q16) and
  `.text=03c1c0` (q8) identical to the baseline, every other section size
  identical too; only the `.text` hash differs (symbol order). Host `cargo
  test --features std`: 341 passed. `make test` in the container: 442 +
  123 + 25 passed, cgroup and pagemap-guard steps `ok`, zero warnings.
  Commits 5fe3f1b (constants), 777dbc1 (abort.rs).

## Notes for the human

- `crate::sys::PIDFD_SELF` is now a plain `pub const` at the top of
  `src/sys/mod.rs`, visible to host builds, so the `PIDFD_SELF` mirror and
  its `const _` assertion in `src/purge.rs` (lines 180-184) can go and
  `configure` can use `crate::sys::PIDFD_SELF` directly. The doc comment
  on `sys::PIDFD_SELF` still says "`purge.rs` mirrors the value"; drop that
  clause with the mirror.
- One deviation from "no new attributes": the four private signal
  constants (`SIG_BLOCK`, `SIG_UNBLOCK`, `SIG_SETMASK`,
  `SIGSET_WORDS_BYTES`) carry `#[cfg(target_os = "linux")]` after the
  hoist. They are private, only Linux code reads them, and the crate is
  `#![deny(warnings)]`, so an ungated hoist fails the host build on
  `dead_code`. The alternative (leaving them inside `mod linux`) would
  have kept `die` from seeing them from `sys/abort.rs`.
- Design 19 (`docs/design.md`, the source-layout table) needs one new
  line and one edit; the lane may not touch that file:

      src/sys/abort.rs      abort (`challoc: <msg>` on stderr, SIGABRT the way glibc delivers it), the stderr sink, the `abort!` macro

  and `src/sys/mod.rs`'s line loses its trailing ", abort, stderr sink"
  and gains "the kernel constants".
- Nothing else skipped. `use core::fmt;` left `mod linux` with the abort
  code (it was its only user); the two `// ---- abort ----` section
  headers were not carried over, the module doc of `abort.rs` covers them.
