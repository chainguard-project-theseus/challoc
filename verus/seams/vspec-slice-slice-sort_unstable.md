# `<[T]>::sort_unstable`

Seam S378 of `verus/SEAMS.md`: src/vspec/slice.rs:139. Kind: assume_spec. Category: core library.
Discharged by Kani, for short slices (the gap below).

## Contract

At commit c704a90e (the record was written there):

```rust
pub assume_specification<T: Ord>[ <[T]>::sort_unstable ](s: &mut [T])
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] final(s)@[i]),
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::slice::slice_sort_unstable` (`src/kani/slice.rs`):
for every length `L` from 0 to 12 (`SORT_MAX`, each a constant) and
every slice of `L` words whose values are below `L`,
`s.sort_unstable()` returns without a panic or undefined behaviour,
`s` keeps its length, an arbitrary index `i < L` names a value that
occurs as often in `s` after the call as before, both for the old
entry `old[i]` and for the new entry `s[i]`, and `s` ends in
ascending order (beyond the contract).

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::slice::slice_sort_unstable` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`: the call returns, which the harness shows.
- `final(s)@.len() == old(s)@.len()`: the length assertion.
- `final(s)@.to_multiset() == old(s)@.to_multiset()`: two sequences
  have the same multiset when every value of either occurs as often in
  both. Every value of `old` is some `old[i]` and every value of the
  result some `s[i]`, and `i` is arbitrary, so the two count
  assertions give exactly that.
- `forall i. old(s)@.contains(final(s)@[i])`: follows from the
  multiset equation (a value occurring in the result occurs in `old`
  as often, so at least once).

Why words below `L` suffice: `sort_unstable` (`T: Ord`) looks at its
elements only through `T::lt` (`core/src/slice/sort/unstable`: the
choice of algorithm depends on the length and `T`'s type only, and
every branch on the values inside one, such as `ipnsort`'s run search
and pivot choices, is a `lt` result), and otherwise only moves whole
elements. Map any `L` words
to their dense ranks (the number of distinct smaller values among
them), a strictly increasing map onto values below `L`. Every
comparison the sort makes has the same outcome on the ranks as on the
words, so it makes the same moves, and its result on the words is the
inverse map of its result on the ranks, element by element. A strictly
increasing map preserves the counts of each value and the ascending
order, so what the harness shows of the ranks holds of the words.
Without the restriction a length of 8 did not finish in 25 minutes of
CBMC (6 took 218 seconds); with it 8 takes 8 seconds and 12 about five
minutes alone, 16 did not finish in 22 minutes, and the harness (every
length to 12 in one run) takes about 28 minutes per quantum.

- Gap: `T = u64`. The proofs call it on `*mut u8` (the cache flush's
  kept half and `group_by_slab`'s fallback, `heap/flush.rs`), whose
  `Ord` compares the addresses as `usize`s and whose moves are 8-byte
  copies, as `u64`'s. CBMC itself cannot stand in: it reports
  comparisons of pointers to different objects as a "same object
  violation", which a harness on `*mut u8` from integer addresses hits.
- Gap: bounded, and the bound reaches one of `core`'s algorithms. For
  2 to 20 elements `sort_unstable` is insertion sort
  (`insertion_sort_shift_left`); the harness runs it at every length
  to 12, and 13 to 20 run the same loop longer. Past 20 it is
  `ipnsort`: a search for an existing ascending or descending run, a
  small sort (`smallsort.rs`) below a threshold that depends on the
  element type, and a quicksort with a heapsort fallback above, none of
  which the harness reaches. The proofs sort caches of up to 65,536
  entries, so the long paths, which the flush takes, rest on `core`'s
  own tests and on this contract being the documented one ("sorts the
  slice", the same elements); only the short path is checked here.

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

Fresh-context subagent, 2026-10-07: the harness against the contract
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
