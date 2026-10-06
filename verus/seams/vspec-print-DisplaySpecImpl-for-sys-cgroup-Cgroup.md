# `DisplaySpecImpl for sys::cgroup::Cgroup`

Seam S337 of `verus/SEAMS.md`: src/vspec/print.rs:45. Kind: spec_impl. Category: core library.
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
impl vstd::std_specs::fmt::DisplaySpecImpl for crate::sys::cgroup::Cgroup {
    open spec fn fmt_req(&self, f: &core::fmt::Formatter<'_>) -> bool {
        true
    }
}
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `sys::cgroup::kani_fmt::display_cgroup` (`src/sys/cgroup.rs`):
`Cgroup`'s `fmt` returns without a panic or undefined behaviour, under
the plain formatter and a padded one (`{:>24}`) and whatever the writer
returns (`kani_proofs::fmt::formats`), for every `State`, every value
of the seven figures it prints (`limit`, `high`, `current`, the two
event counts and the two rise counts), and every directory of up to 3
bytes (every byte value) with every mount prefix length `mount_len <=
dir_len`.

Reproduce: `sh verus/kani/run.sh --harness sys::cgroup::kani_fmt::display_cgroup` (both quanta;
`--harness` takes the full path).

## Translation

- `fmt_req` is `true`: `fmt` may be called on any `Cgroup` with any
  formatter, and Verus assumes the call returns without a panic. A
  `Cgroup` that Verus code holds satisfies its type invariant
  (`dir_len + FILE_ROOM <= PATH_CAP && mount_len <= dir_len`), which
  the harness's values meet; `mount()` and `path()` slice by it.
- Nothing is claimed about what it writes or returns.

Gap: directories of at most 3 bytes, where the invariant allows up to
`PATH_CAP - FILE_ROOM` (2016). The directory, mount and path are
printed by one loop over their bytes whose body depends only on the
byte (a printable byte indexes the 95-byte table at `b - b' '`, any
other prints `?`), and every byte value is checked; longer directories
run more iterations of the same body. The fields `fmt` does not print
(`has_*`, the timestamps, the other event counts) keep their
`untracked` values.

Common to every Kani record: Kani checks the MIR of a debug build
(overflow checks and debug assertions on) for x86_64, with CBMC's
bit-precise semantics for the integer operations and Kani's own models
of the `core` intrinsics it lowers. The shipped library is a release
build: it drops the overflow checks and debug assertions (where a
harness shows they never fire under the contract, the two builds
agree), and it is also built for aarch64, whose layouts and pointer
width match x86_64's for every type here (both are LP64 targets;
the layout records name the one check that runs per target). The
`core` Kani checks is its own nightly's (2026-08-21); the shipped
library is built with rustc 1.98, whose `core` functions here are the
same unless they changed between the two, which a rerun with a newer
Kani would show. Policy for every Kani record: a trusted body must
return under its `requires` without a panic, except an abort seam
(whose contract is that it does not return) and the overflow aborts
a contract's comment declares (`fig_*`, `stats_allocated`); a panic
in challoc aborts the process (`panic = "abort"`), so it never yields
a wrong result, but the proofs' "no panic" claim rests on this policy.

## Review

Fresh-context subagent, 2026-10-04: the harness against the contract
and this translation, looking for claims the harness does not support;
its findings are folded in.

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
