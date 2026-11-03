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
- a `PartialEqSpecImpl` impl (its `obeys_eq_spec()` and `eq_spec`, a
  fact about the `PartialEq` impl that Verus does not check);
- a `DisplaySpecImpl` or other fmt-trait spec impl whose `Display` impl
  is outside `verus!` or in another file (its `fmt_req`, unchecked against
  that `fmt`); one in the same file and inside `verus!` is no seam,
  since Verus checks that `fmt` under `fmt_req` and every
  `format_args!` site proves it;
- an `exec_allows_no_decreases_clause` function (termination assumed);
- a `Tracked::assume_new()` or `Ghost::assume_new()` inside `verus!`,
  outside tests and outside `external_body` bodies (an entry notes the
  ones inside; code outside `verus!` is not verified, so its own are
  not seams).

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

A seam is "discharged" when a check outside Verus covers its whole
contract: under every input its `requires` admits, the call is defined
and every `ensures` clause holds when it returns, within the bounds and
at the type instances the check's comment states, running the item's
own body and the code it calls (no stub but of verified code). A
clause about ghost state counts as covered when the check shows the
fact that state stands for (a `Zeroed` token's bytes read zero) or
when the item's body cannot change it (a token it does not touch,
an invariant it cannot open, the contents of an immutable static);
one about a real pointer's provenance, which Kani cannot observe, does
not. `discharged (Kani: <harness path>)` names the Kani harnesses that
do so, which `.chainguard/ci/kani.yaml` runs at both quanta (`kani-ignoring`
runs those in the hot files `src/kani/mod.rs` lists, after a change
lands; `seams.sh --check` fails when a path names no `#[kani::proof]`
fn). `discharged (const assert: <label> in <file>)` names a top-level
`const _: () = assert!(..)` in `<file>`, the one whose `//` comment
block starts with `<label>` (a name in backquotes, and as many of the
words after it as it takes to single out one assertion). The compiler
evaluates the assertion in every build, at each quantum and on each
target, and a false one is a compile error; `seams.sh --check` fails
unless exactly one assertion in `<file>` carries the label. For a
struct layout axiom the assertion states every `ensures` clause, its
`field_fits` ones through `arena::fits`, which Verus verifies against
`field_fits`; for an `offset_of!` constant, whose value Verus does
not see, it checks every clause of the layout axiom that names the
constant.
`discharged (Isla: verus/isla/seams.md)` names the Isla runs of
`verus/ISLA.md`, which trace the compiled code of the item's `asm!`
blocks on a Sail model and check each against the contract
that file quotes; `verus/isla/run.sh` fails when that file's seams,
contracts or architectures differ from the ledger's. `seams.sh
--check` fails unless the file has a heading for the seam, or when an
`asm!` call in the seam's item is not a line of the `templates.txt`
next to that file (or a line there matches no call, or a string literal
in the item names a trapping or system-register instruction), so a
changed template fails without the Isla tools. A trace that ends at an
exception the kernel handles (a system call) checks nothing the kernel
does after it, so such a seam is at most partly checked. The bounds of an
Isla check are the build, link and platform residuals `verus/isla/seams.md`
states for its runs (the static library's code and register assignments,
the Linux user state the runs start from, the CPU configurations
checked), as a Kani harness's are the ones its comment states.
`discharged (emptyasm: <fn> in <file>)` names the row of
`verus/emptyasm/blocks.txt` for an `asm!` block with an empty template
in `<fn>` of `<file>`, the seam's own fn and file: `make emptyasm`
(`verus/emptyasm/run.sh`, `.chainguard/ci/isla.yaml`) checks that the
release build holds no instruction for it on the row's architecture.
That discharges a seam only when the item's body is that block alone and
its contract is that the operand comes out unchanged; otherwise the check
covers part of it.
`seams.sh --check` fails unless exactly one row names that fn and file
and the seam's item holds one `asm!` call, whose template is the empty
`"/* {0} */"`, so a template that gains an instruction fails without
the build. Where a Kani harness runs a copy of source it cannot run (a
body around an `asm!` block), `verus/kani/pins.txt` lists the
original's lines and the copy's, and `seams.sh --check` fails unless
they are in place; where rows mark the two sides (`NAME:orig`,
`NAME:copy` and their `-only` rows), it also fails unless the copy's
rows equal the original's but for the `-only` ones.
A check that covers only part of the contract leaves the
status "trusted", with a note that names the check and what it does
not cover: `trusted (partly checked, <check>; not checked: <what>)`,
where `<check>` is `Kani: <harness path>`, `const assert: <label> in
<file>`, `Isla: <file>`, `emptyasm: <fn> in <file>`, or several joined
by `, and`. A list separates
its items with `, ` or ` and `, and ends at `;`, `)` or the next tag.
