# `<[T]>::rotate_left`

Seam S344 of `verus/SEAMS.md`: src/vspec/slice.rs:156. Kind: assume_spec. Category: core library.
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
pub assume_specification<T>[ <[T]>::rotate_left ](s: &mut [T], mid: usize)
    requires
        mid <= old(s)@.len(),
    ensures
        final(s)@ == old(s)@.subrange(mid as int, old(s)@.len() as int) + old(s)@.subrange(0, mid as int),
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::slice::slice_rotate_left`, `kani_proofs::slice::slice_rotate_left_long`, `kani_proofs::slice::slice_rotate_left_cycles`, `kani_proofs::slice::slice_rotate_left_quads` (`src/kani/slice.rs`):
`s[..n].rotate_left(mid)` on an array of `L` elements whose words all
carry distinct labels leaves element `k < n` holding the labels of
`(k + mid) % n` and every element from `n` on unchanged, for: 8-byte
elements (`u64`) at every `n <= 8` and every `mid <= n`
(`slice_rotate_left`) and at `n == 70` with `mid` from 31 to 39
(`slice_rotate_left_long`); 40-byte elements (`[u64; 5]`) at `n ==
16`, every `mid` (`slice_rotate_left_cycles`); 32-byte elements
(`[u64; 4]`) at `n == 20` and `n == 26`, every `mid`
(`slice_rotate_left_quads`).

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::slice::slice_rotate_left --harness kani_proofs::slice::slice_rotate_left_long --harness kani_proofs::slice::slice_rotate_left_cycles --harness kani_proofs::slice::slice_rotate_left_quads` (both quanta;
`--harness` takes the full path).

## Translation

- `requires mid <= old(s)@.len()`: every `(n, mid)` pair the harnesses
  run (as constants) meets it.
- `ensures final(s)@ == old(s)@.subrange(mid, len) +
  old(s)@.subrange(0, mid)`: element `k` of the right side is old
  element `mid + k` for `k < len - mid`, else old element `k - (len -
  mid)`, which is `(k + mid) % len`: the assertion.

Why labels suffice: `rotate_left` has no bound on `T`, so it cannot
look at the values: its control flow depends only on the length, `mid`
and `T`'s size and alignment (parametricity), and every copy, chunked
swap or typed move in it runs between element-aligned offsets over
whole elements. With every word labelled distinctly, one run per
`(len, mid)` shows where every word goes, so the result for every
content of a `T` of that size and alignment. `len` and `mid` run as
constants (with a symbolic count Kani 0.68 copies one element too few
into an array of 8-byte elements; the module comment of
`src/kani/ptr.rs`).

Gap: bounded. `core` (`core/src/slice/rotate.rs`) picks one of three
algorithms: through a 256-byte stack buffer when the shorter side
`min(mid, len - mid)` fits in it; else by cycles (gcd) when `len < 24`
or `T` is over 32 bytes; else by block swaps. The proofs' one caller
(`heap::compact`'s refill) rotates 8-byte slot words in caches of up
to 65,536 entries. For 8-byte elements the buffer holds 32; the
harnesses run the buffer algorithm at shorter sides 0 to 4 (every
`len <= 8`) and 31 and 32 (`len` 70), and the block swaps at shorter
sides 33 to 35 (`len` 70), both branches of their loop (`mid` 33, the
left side shorter, and 37, the right side shorter). Shorter sides 5 to
30 on 8-byte elements run the buffer algorithm's same three copies
with other counts, and are not run. 8-byte elements never take the
cycle algorithm, which is run on 40-byte elements (shorter sides 7 and
8 at `len` 16) and 32-byte elements (shorter sides 9 and 10 at `len`
20), including `mid`s whose gcd with `len` exceeds 1 (`mid` 8 at 16,
`mid` 10 at 20), which take its second loop; the 32-byte block swaps
run at `len` 26. All 71 `mid`s at `len` 70 did not finish in 90
minutes of CBMC, hence the window 31 to 39. Longer slices run the same
code with more iterations.

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
