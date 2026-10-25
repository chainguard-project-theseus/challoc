# Seams: what the Verus proofs take on trust

Every verified function is conditional: Verus checks it against the
contracts of the items it cannot see into and assumes those contracts
hold. A *seam* is one such item, the place where a result from another
tool (or a stated assumption) plugs in:

- an `external_body` function, proof function, type or constant (its body
  is not checked; its signature, `requires` and `ensures` are assumed);
- an `axiom fn` or `axiom_*` proof function (its `ensures` is assumed);
- an `assume_specification`, `external_type_specification` or
  `external_trait_specification` (a contract for code outside the crate);
- an `exec_allows_no_decreases_clause` function (termination assumed);
- a `Tracked::assume_new()` or `Ghost::assume_new()` outside tests and
  outside `external_body` bodies (an entry notes the ones inside).

## The ledger

`sh verus/seams.sh --write` generates the ledger from `src/`, one file
per source file with seams: the seams of `src/a/b.rs` are in
`verus/ledger/src/a/b.rs.md`. `sh verus/seams.sh --check` (part of
`make verus`) fails when a ledger file is stale, missing or left over.
`sh verus/seams.sh` prints the totals. Do not edit ledger files by hand.

Each entry gives the place, the kind, the contract Verus assumes (copied
verbatim from the source: the signature with its `requires` and
`ensures`), a category (why Verus cannot check it), a proposed tool and a
status. An entry is named by its item, with `#N` for the Nth of several
seams that share a name in one file.

## Categories and tools

The categories are inline asm, FFI/syscall, function pointer,
UnsafeCell/volatile/transmute, raw pointer (internal), raw pointer from C
caller, panic/abort, atomics primitive, platform/kernel fact, C-program
contract and termination, plus two this ledger adds: core library (a
specification of a `core` function or type, checkable against `core`
itself) and allocator invariant (a ghost fact about challoc's own
protocol whose producer is unverified code). The tools are Islaris/Isla,
RefinedRust, Kani, loom/GenMC, Verus (the fact is provable in Verus
itself, once its producer states it or a lemma derives it), Miri
(evidence only) and none (assumption).

`seams.sh` picks a category from the kind, the path and the body. The
seams of `src/a/b.rs` may have an overrides file,
`verus/ledger/src/a/b.rs.overrides`, with one
`name<TAB>category<TAB>tool[<TAB>status]` line per seam it corrects
(`#` starts a comment; `name` as the ledger file prints it). Category and
tool are spelled as above. A line that matches no seam is an error, and
so is a seam with no category.

## Status

The tool is a proposal. A seam's status stays "trusted" until the
fourth column of its overrides line names another status, citing the
other tool's result.
