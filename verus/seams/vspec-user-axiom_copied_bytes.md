# `axiom_copied_bytes`

Seam S414 of `verus/SEAMS.md`: src/vspec/user.rs:285. Kind: axiom. Category: core library.
Evidence from Kani: the harness settles the memory half (what
`copy_nonoverlapping` does to memory); that the ghost byte maps of
`UserMem` agree with memory before the copy is a premise the harness
cannot check, so the axiom stays trusted on it.

## Contract

At commit c76d91a7 (written at a9be5831, which added the axiom with
`copy_user`):

```rust
axiom fn axiom_copied_bytes(tracked s: &UserMem, tracked d: &mut UserMem, src: int, dst: int, n: int)
    requires
        0 <= n,
        forall|i: int| src <= i < src + n ==> #[trigger] s.dom().contains(i),
        forall|i: int| dst <= i < dst + n ==> #[trigger] old(d).dom().contains(i),
        s.dom().disjoint(old(d).dom()),
    ensures
        final(d).frame(*old(d), dst, n),
        final(d).reads_as(dst, *s, src, n),
;
```

Its one caller is `copy_user` (the axiom is private to
`vspec/user.rs`), right after `core::ptr::copy_nonoverlapping(src, dst,
n)` on `*mut u8`, with the permissions `s` and `d` that call was made
with and `src@.addr`, `dst@.addr`, `n` as the arguments:

```text
unsafe { core::ptr::copy_nonoverlapping(src, dst, n) };
proof {
    axiom_copied_bytes(s, d, src@.addr as int, dst@.addr as int, n as int);
}
```

`copy_user`'s `requires` give the axiom's (the two ranges in the two
permissions' domains, which share no byte) and the two pointers'
provenances; `copy_user` is called once, by `libc::copy_moved`
(`realloc`'s copy into the new block).

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness
`kani_proofs::ptr::ptr_copy_nonoverlapping` (`src/kani/ptr.rs`), the one
the record of `core::ptr::copy_nonoverlapping::<T>` (S259,
`verus/seams/vspec-addr-ptr-copy_nonoverlapping.md`) cites. Its part
at `T = u8`, the instance `copy_user` calls,
`copy_nonoverlapping_on::<u8, 5>`: for every `i` and `j` in `0..=5` and
every `count` with `i + count <= 5` and `j + count <= 5`,

- between two arrays of 5 bytes with arbitrary contents,
  `copy_nonoverlapping(src + i, dst + j, count)` is defined and
  afterwards `dst[k] == src[i + (k - j)]` for `j <= k < j + count` and
  `dst[k]` is its old value for every other `k` in the array;
- within one array of 5 bytes, when `i + count <= j || j + count <= i`,
  the same, with the array's old contents on both sides.

It does the same at `u64` (`copy_nonoverlapping_on::<u64, 4>`) and with
a zero-sized `T`, and checks the empty copy on null and dangling
aligned pointers.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::ptr::ptr_copy_nonoverlapping`
(both quanta; `--harness` takes the full path).

## Translation

Read `s.bytes()[x]` as "the byte memory holds at address `x`" for each
`x` in `s.dom()`, and the same for `d` before and after the copy. That
reading is the premise (below); under it:

- **`requires 0 <= n`, the two ranges in the two domains, the domains
  disjoint**: the copy's `n` source bytes are `s`'s and its `n`
  destination bytes `d`'s, and no byte is both: the harness's `count`,
  its in-range `i + count`, `j + count`, and its two arrays (or the two
  ranges apart within one array).
- **`ensures final(d).reads_as(dst, *s, src, n)`**: for `dst <= x < dst
  + n`, `final(d).bytes()[x] == s.bytes()[src + (x - dst)]`: the
  harness's `dst[k] == src[i + (k - j)]` at `k = x`, with addresses for
  indices. The source is read before any write to it could happen (no
  destination byte is a source byte), so the old source bytes, which
  `s` records, are what is copied.
- **`ensures final(d).frame(*old(d), dst, n)`**: its fourth conjunct,
  every `i` outside `[dst, dst + n)` keeps its value. For `i` in
  `old(d).dom()`: the harness's "every other element keeps its old
  value", over the destination array; Kani also fails the harness on
  a write outside the object a pointer is derived from, so the copy
  writes nothing past the destination range. For `i` outside the
  domain the map's values are ghost-only (no byte of memory is read
  through them) and kept by definition. Its other conjuncts (the same domain, provenance
  and `wf`) are about the ghost record alone: the copy does not change
  which bytes the permission covers, and memory has no counterpart of
  them, so they are definitional, not a memory fact.
- `s` is borrowed, so the axiom leaves it as it was: the copy does not
  write a source byte (the within-one-array case shows the bytes
  outside the destination range, the source among them, unchanged).

**Why evidence: the premise.** The axiom states its effect on the ghost
byte maps, not on memory. That `s.bytes()` and `old(d).bytes()` are
what memory holds at their addresses when the copy runs is the
invariant `UserMem`'s doc comment states: every write verified code
makes to user memory takes the permission and updates its byte state
(`zero_user`, `copy_user`), and the program, which writes the bytes as
it likes, holds the permission (in the block's token) while it does;
where a permission is minted its bytes are any values. Kani checks the
copy on memory (of arbitrary contents), not that agreement, and no other tool here
checks it: it is a property of the whole program (of every writer of
user memory, verified or not, and of the C program between calls). So
the harness settles the memory-level effect, and the agreement
between the ghost maps and memory stays a premise; the status is
evidence, not discharged. A second premise, of the axiom's placement:
it is private and its one call sits right after the copy, with that
copy's permissions and arguments; a call with no copy would set a false
byte state (the axiom's doc comment). That placement is checked by
review of `vspec/user.rs`, not by a tool.

**Gaps (model).** Arrays of up to 5 bytes, every in-range `(i, j,
count)`, run as constants (with a symbolic count Kani 0.68 copies one
element too few into an array of 8-byte elements; the module comment
of `src/kani/ptr.rs`); `copy_nonoverlapping` is one copy whatever the
length, so the bound only limits the sizes. The pointers are derived
from local arrays, which Kani tracks as objects; challoc's are
integer-to-pointer casts of mapped addresses (the S259 record's
"Provenance" paragraph). Kani lowers the call to its own model of the
`copy_nonoverlapping` intrinsic; the release build calls `memcpy` (or
inlines a copy), whose agreement with that model is assumed.

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

Written on 2026-10-07 at c76d91a7, as evidence rather than a
discharge (the premise above). Run then with Kani 0.68.0 under both quanta:
`kani_proofs::ptr::ptr_copy_nonoverlapping` passes (0 of 186 checks
failed in each, 4 unreachable; about 200 s per quantum).

Fresh-context subagent, 2026-10-07: this record against the harness,
the axiom, `copy_user` and `UserMem`'s definitions. It found the
statement, the `requires`/`reads_as` mapping, the premise and the
evidence status right; its findings (the `frame` conjunct for bytes
outside the domain, a placeholder, wording, and stale seam numbers in
the S259 record this one cites) are folded in.

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the harness
under both quanta: a `core` whose `copy_nonoverlapping` copies other
bytes, or writes outside the destination, fails it. The script checks
the contract above against the ledger only for discharged records, so
a changed contract of this evidence seam is caught by review, not by
the script. Nothing checks the premise. The script is run by hand (it
needs Kani's own toolchain, `verus/kani/setup.sh`); no CI job runs it
yet.
