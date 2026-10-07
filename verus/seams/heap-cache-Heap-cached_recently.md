# `Heap::cached_recently`

Seam S112 of `verus/SEAMS.md`: src/heap/cache.rs:721. Kind: external_body. Category: raw pointer (internal).
Discharged by Kani.

## Contract

At commit 4f75a5b7, with the first `requires` added by this record's
package (P3), so that the contract states the bounds condition of the
body's pointer offsets:

```rust
pub(super) unsafe fn cached_recently(slots: *mut *mut u8, len: u32, e: *mut u8, Tracked(perms): Tracked<&Map<*mut *mut u8, PointsTo<*mut u8>>>) -> (r: bool)
    requires
        len == 0 || slot_in(slots, len as int),
        forall|i: int| 0 <= i < len ==> {
            let q = #[trigger] slot_ptr(slots, i);
            perms.contains_key(q) && perms[q].ptr() == q && perms[q].is_init()
        },
    ensures
        // A hit is an entry among the `len` whose address is `e`'s (the
        // compares are of addresses, as `==` on pointers is).
        r ==> exists|i: int| 0 <= i < len && perms[#[trigger] slot_ptr(slots, i)].value()@.addr == e@.addr,
    opens_invariants none
```

`slot_in(slots, len)` is `in_alloc(slots, len * size_of::<*mut u8>())`
(heap/mod.rs): the `8 * len` bytes from `slots` lie inside the
allocation `slots` is derived from, the range possibly ending one past
its last byte. The body Verus does not see, `#[inline(always)]`:
`false` at `len == 0`; otherwise, with `top = slots.add(len)`, the four
compares `*top.sub(k) == e` for `k` in 1..=4 at `len >= 4`, and
`(1..=len).any(|i| *top.sub(i) == e)` below 4 (the iterator is why it
is trusted). Its callers: `Heap::push_cache` (the fast push's
double-free scan, which declines on a hit), and `Heap::scan_or_abort`
(which aborts on a hit), called on the slow free paths
(`free_slow_in` and `rehome_on_free`). Both pass a cache's stack
(`slots`, `len` from its header, `len <= cap`) with the heap's stack
permissions, under `stack_ok`, which gives the first `requires`:
`slot_in` for every entry below `cap` and, since P3, for `cap` itself
when it is not 0 (proved at the stack's one creation,
`Heap::ensure_cache`, from the block's `PointsToRaw` through
`axiom_raw_valid`; no new trusted fact).

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `heap::cache::kani_cache::cached_recently_reads_the_top_entries` (`src/heap/cache.rs`):
for every `len` from 0 to 8, on a stack of exactly `len` entries (an
array: the stack is the whole object the pointers are derived from, so
the `8 * len` bytes from `slots` lie in it and end at its end, the
first `requires`), with every entry and every `e` (`kani::any()`
addresses), the call returns, with no access outside the array and no
out-of-object pointer offset, and its result is whether `e` equals one
of the top `min(4, len)` entries. And at `len == 0` (where the
`requires` names no entry and no range) it returns `false` on any
`slots` pointer, null or dangling.

Reproduce: `sh verus/kani/run.sh --harness heap::cache::kani_cache::cached_recently_reads_the_top_entries` (both quanta;
`--harness` takes the full path).

A control (run by hand, not kept): passing `len + 1` for a stack of
`len` entries, one entry and 8 bytes the precondition does not cover,
fails the harness ("pointer outside object bounds", and the offsets'
"must point to the same allocation").

## Translation

- **`requires len == 0 || slot_in(slots, len)`.** In the harness, the
  array of `len` entries: the `8 * len` bytes from `slots` are the whole
  object, ending at its end, which `in_alloc` allows (the range may end
  one past the last byte). The body's offsets are `slots.add(len)` and
  `top.sub(k)` for `k <= min(4, len)`: each moves within that range or
  to its end, which `core`'s in-bounds rule allows; the range lies in
  one allocation, and an allocation is at most `isize::MAX` bytes
  (the `core::ptr` documentation), so the byte offsets fit in an
  `isize`. Kani reports an offset leaving the object or crossing into
  another: none happens, for every `len` checked. At `len == 0` the
  body returns before any offset.
- **`requires`: the `len` entries from `slots` permitted and written.**
  The harness's array elements, each written. `slot_ptr(slots, i)` is
  `slots.add(i)` (`ptr_add`); a permission with `ptr() == q` is to the
  entry at `q`. The body reads entries `len - 1` down to
  `len - min(4, len)`, all named by the `requires`, and Kani reports
  any read outside the array: none happens.
- **No `ensures`.** Verus learns nothing from the result (the callers'
  proofs do not depend on it: a hit declines the push or aborts, a
  miss pushes or returns, and each branch is verified either way). The
  harness checks, beyond the contract, that the result is the
  top-four membership the doc comment states.
- **`opens_invariants none`**: the body has no ghost code and opens no
  invariant (read from the source).
- **Returns.** The body has no panic site under the `requires` (no
  division, no unchecked index; the iterator's range `1..=len` with
  `len < 4` cannot overflow); the harness reaches the end for every
  input.

Gap: `len` up to 8, which covers both branches and their boundary at
4 (the body reads at most four entries whatever `len` is, so a longer
stack only moves `top`). The entries are addresses without provenance
(`without_provenance_mut`): the body compares them by address and
never dereferences them.

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

Written 2026-10-07 on commit 4f75a5b7 (P3): the contract above is the
ledger's entry with P3's `requires`, and the harness was compared with
each clause. A fresh-context subagent reviewed this record against the
harness and the contract; it found the first version's contract, without
`slot_in`, too weak for the body's offsets (the single allocation was
then a premise read from the callers), which P3 now states and proves;
its other findings are folded in.

Run on commit 4f75a5b7 with P3, 2026-10-07, with Kani 0.68.0 under both
quanta: `heap::cache::kani_cache::cached_recently_reads_the_top_entries`
passes (0 of 580 checks failed in each, 7 unreachable).

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
