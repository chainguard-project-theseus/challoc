# `axiom_zero_entry`

Seam S168 of `verus/SEAMS.md`: src/huge/table.rs:242. Kind: external_body proof. Category: raw pointer (internal).
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
proof fn axiom_zero_entry(tracked perm: PointsTo<Entry>, tracked z: Zeroed) -> (tracked r: PointsTo<Entry>)
    requires
        perm.is_uninit(),
        z.addr() == perm.ptr()@.addr,
        z.len() == 16,
    ensures
        r.ptr() == perm.ptr(),
        r.is_init(),
        !occupied(r.value()),
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `huge::table::kani_table::zero_entry_is_empty` (`src/huge/table.rs`):
an `Entry` is 16 bytes, and 16 zero bytes, aligned for an `Entry`, read
as an `Entry` (a defined read: Kani checks it) give an entry whose
`desc` has address 0 (and whose `key` is 0).

Reproduce: `sh verus/kani/run.sh --harness huge::table::kani_table::zero_entry_is_empty` (both quanta;
`--harness` takes the full path).

## Translation

- `requires perm.is_uninit(), z.addr() == perm.ptr()@.addr, z.len()
  == 16`: the `Zeroed` token says the 16 bytes at the entry's address
  read as zero, the harness's starting bytes; the permission is ghost
  and says the bytes have no typed value yet, which the harness's read
  gives them.
- `ensures r.ptr() == perm.ptr(), r.is_init()`: ghost facts about the
  permission, which keeps its pointer and now holds a value: what a
  read of initialised bytes as an `Entry` yields, which the harness
  shows is defined for these bytes (an `Entry` is a `usize` and a raw
  pointer, which accept every bit pattern).
- `ensures !occupied(r.value())`: `occupied(e)` is `e.desc@.addr != 0`;
  the harness asserts `e.desc as usize == 0`.

Gap: the proof function is ghost (its body is never run); the harness
checks the executable fact its contract stands for: the bytes the
token describes, read as the value the permission then holds. That the
token's bytes are zero is the token's own trust (its sources, other
seams); that Verus's `PointsTo` value for initialised bytes is the
value a read of them gives is Verus's model of memory.

Alignment: the contract does not require the entry's address to be
aligned, while the harness reads from an aligned buffer; an unaligned
read is outside what the harness shows. The proofs' one caller,
`entry_slots`, types the table storage at `Entry`'s alignment (8, the
`global layout` line).

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
