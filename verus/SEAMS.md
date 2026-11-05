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
seams that share a name in one file. Its hashes, `contract C, body B`,
are the POSIX `cksum` CRCs of its contract (the item's attributes and
its signature with its clauses) and of its body (`body none` for an item
without one), with `//` comments dropped and each run of whitespace made
one space, string literals included, so moving, reindenting or
recommenting the item keeps them. Splitting a line where there was no
space (a signature reflowed one parameter per line) changes them, and a
`/* */` comment counts as code. A method's hashes do not cover its
`impl` line (the impl's generics and `where` bounds).

## Categories and tools

The categories are inline asm, FFI/syscall, function pointer,
UnsafeCell/volatile/transmute, raw pointer (internal), raw pointer from C
caller, panic/abort, atomics primitive, platform/kernel fact, C-program
contract and termination, plus two this ledger adds: core library (a
specification of a `core` function or type, checkable against `core`
itself) and allocator invariant (a ghost fact about challoc's own
protocol whose producer is unverified code). The tools are Islaris/Isla,
RefinedRust, Kani, loom/GenMC, Verus (the fact is provable in Verus
itself, once its producer states it or a lemma derives it) and none
(assumption).

`seams.sh` picks a category from the kind, the path and the body. The
seams of `src/a/b.rs` may have an overrides file,
`verus/ledger/src/a/b.rs.overrides`, with one
`name<TAB>category<TAB>tool[<TAB>status[<TAB>hashes]]` line per seam it corrects
(`#` starts a comment; `name` as the ledger file prints it). Category and
tool are spelled as above. A line that matches no seam is an error, and
so is a seam with no category.

## Status

The tool is a proposal. A seam's status stays "trusted" until the
fourth column of its overrides line names another status, citing the
other tool's result.

A status that cites a check (discharged, discharged on premises, or
trusted (partly checked, ...)) also records, in a fifth column, the
hashes of the item the check covered, as its ledger entry shows them:
`contract C, body B`. `seams.sh --check` fails when they differ from
the item's, so an edit to a checked seam's attributes, signature,
`requires`, `ensures` or body fails the check, even after `--write`, until the
status is revisited and the hashes copied again from the ledger entry
(the failure also prints them). It also fails on such a status with no
hashes, and on hashes after any other status, including `trusted
(requires, checked at every caller: ...)`, whose body no check runs.
The hashes cover the seam's own item, not the code it calls or the
harness or model it cites. `sh verus/seams-mutants.sh` checks that
`--check`'s self-tests catch weakened forms of this rule, and that, in a
copy of the tree, an edit to a discharged seam's attribute, `requires`,
`ensures` or body fails
`--check` after `--write` while a comment, a reindent or a line shift
does not.

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
does after it, so such a seam is at most discharged on premises (below).
`discharged on premises (Isla: verus/isla/seams.md; premises: <what>)`
is the status of an Isla check that covers the whole contract except
for platform premises: the P and K premises of `verus/isla/seams.md`
("Platform premises"), which `<what>` lists by label, each label
optionally followed by a gloss in parentheses, as a partly checked
status lists what it leaves unchecked. The premises stay assumed, and
`verus/count` counts the seam as discharged. A contract that also rests
on a premise of Verus's own model (a trusted seam such as
`TlsSlot::entry`, or a ghost cell such as `TlsCell::tp`) is
`trusted (partly checked, ...)`.
`discharged on premises (Kani: <harness path>; premises: <what>)` is
the same status for Kani harnesses that run the item's own body and
cover its whole contract, as for `discharged (Kani: ...)`, except for
facts about what the C program stores in, or passes through, challoc's
exported C symbols, which no harness reaches: the C premises of
`verus/kani/premises.md` ("Premises"), which `<what>` lists by label,
each label optionally followed by a gloss in parentheses, as for Isla.
`seams.sh --check` resolves its harness paths as it does those of a
`discharged (Kani: ...)` status. The premises stay assumed, and
`verus/count` counts the seam as discharged. A contract that also rests
on anything else is `trusted (partly checked, ...)`: a premise of
Verus's own model, as for Isla, and also two things a harness can leave
out that an Isla run cannot: an obligation on Rust callers inside
challoc that the item's `requires` does not state (an `unsafe fn`'s
`# Safety` premise), and a pointer's provenance.
The one exception is the caller obligation, and only when the status
names a guard test (`verus/README.md`, "Guard tests") that confines it:
`discharged on premises (Kani: <harness path>; premises: <what>;
callers: <test path>)`. The test must list every caller of the item, or
confine every use of it, together with the clause each caller must keep
for the obligation to reduce to the named premises; its row of the
guard-test table must record its mutation evidence, the mutants it
kills, in a `Mutants killed:` part; and the row must state the test's
known escapes, the uses it cannot see, in a `Known escapes:` part.
`seams.sh --check` fails unless `<test path>` names a `#[test]` fn
(resolved as a harness path is, with `#[test]` for `#[kani::proof]`)
and the guard-test table has a row whose first cell names that path in
full with both parts; `verus/count` fails on a `callers:` clause
anywhere but in a Kani `discharged on premises` status, or on one that
names no `#[test]` fn under `src/`. A contract that rests on any other
caller obligation, or on one no such test confines, stays `trusted
(partly checked, ...)`.
Only Isla's and Kani's checks take this status: `verus/count` fails on
one that cites another check or both, names no premise, or names
anything but labels the cited check's file defines: it reads the
`- **LABEL, ...**` bullets of `verus/isla/seams.md`'s "Platform
premises" section (P and K labels) for Isla and of
`verus/kani/premises.md`'s "Premises" section (C labels) for Kani, and
resolves each label against them. The bounds of an
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
`loom: <mod>::<test>` names a loom model, a `#[test]` fn of
`tests/loom/src/<mod>.rs`, which the `loom` and `loom-count` checks
(`.chainguard/ci/lint.yaml`) run under every interleaving loom reaches.
A model runs a copy of the code it checks (challoc's statics are built
in `const` context, which loom cannot construct;
`tests/loom/tests/pins.rs` keeps the copied lines in place), not the
item's own body, so a seam that cites one is at most partly checked, and
`seams.sh --check` fails on a "discharged" status that cites `loom:`. It
also fails unless the model is no `#[should_panic]` control, its doc
comment or body names the seam's function in backquotes (`Type::fn` for
a method, else `m::fn` with `m` the source file's module, after any
path), and its file names the seam's source file (`src/a/b.rs`, or
`a/b.rs` in a directory). A model may be `#[ignore]`d: the `loom-count`
check runs those (`--ignored`) on changes to the loom crate or the code
they copy.
`sh verus/seams-mutants.sh` checks that `--check`'s self-tests catch
weakened forms of this rule.
The weak-memory argument a loom citation rests on (C++20's coherence
and read-modify-write rules for one atomic, and the release/acquire
edges the models check) is the comment above the seam's overrides line,
with `verus/WEAKMEM.md`; `tests/loom/tests/access.rs` checks, by
matching source text, the source-wide facts those arguments state
(atomic writes only through the wrappers, one permission per atomic).
A check that covers only part of the contract leaves the
status "trusted", with a note that names the check and what it does
not cover: `trusted (partly checked, <check>; not checked: <what>)`,
where `<check>` is `Kani: <harness path>`, `const assert: <label> in
<file>`, `Isla: <file>`, `emptyasm: <fn> in <file>`, `loom:
<mod>::<test>`, or several joined by `, and`. A list separates
its items with `, ` or ` and `, and ends at `;`, `)` or the next tag.
A trusted body whose `requires` Verus checks at every caller (no tool
checks the body) keeps the status "trusted", with a note that names
what the `requires` states and what it leaves out: `trusted (requires,
checked at every caller: <what>; not checked: <what>)`.
