# Verifying challoc with Verus

challoc's own source is verified in place: a function inside a
`verus! { ... }` block is checked by [Verus](https://github.com/verus-lang/verus)
for every operation it performs (no arithmetic overflow, no division by
zero, no out-of-bounds index, no reachable panic, termination of every loop
and recursion) and against the `requires`/`ensures` contract it states.
Ghost code (`spec`, `proof`, `requires`, `ensures`, invariants) is erased
from ordinary builds, so the shipped library is unchanged by it.

Verified code sits in `verus!` blocks across `src/`; `make verus` prints
the current counts (Coverage, below). Everything else sits outside
`verus!`, and Verus does not look at it.

## Running

```sh
make verus     # the seam ledger, the proofs, then the coverage floor
```

`make verus` runs `sh verus/seams.sh --check` (Seams, below), then
`cargo verus verify` (the default `quantum16` feature), then
`verus/count --floor ../FLOOR` (Coverage, below). Unlike the other
targets it runs where it is invoked, not in the dev container. The
`verus` check (`.chainguard/ci/verus.yaml`) runs it on every pull
request, against the pull request's base, and `verus-main` on main,
against the commit before, so a change that leaves the seam ledger
stale, breaks a proof or lowers the verified share fails CI.

The check gets Verus from the `verus` package, release
`0.2026.10.18.7d82994`, which matches Cargo.toml's
`vstd = "=0.0.0-2026-10-18-0147"` and brings the toolchain it runs with
(the `rust-1.99` stream, 1.99.1) and z3. The package has no aarch64 cross
target, so `--target aarch64-unknown-linux-gnu` cannot be verified from
an x86_64 host; a second check, `verus-arm64`, runs the same steps
natively on an arm64 runner with the package's arm64 build. To run it on
this machine instead:

1. Download the release for this machine from
   <https://github.com/verus-lang/verus/releases/tag/release/0.2026.10.18.7d82994>
   (the release's Linux build is x86-64 only) and put its directory on `PATH`.
2. `rustup toolchain install 1.99.1`.

Cargo keeps a verified crate's result: when nothing changed, `make verus`
prints no results line for it. `cargo clean -p challoc` forces a full run.

Ordinary builds and tests are unchanged (`make build`, `make test`).

## What counts as verified

A function counts when it sits inside `verus!`, is not
`#[verifier::external_body]` or `#[verifier::external]`, and its body has no
`assume(...)`, `admit()` or `Tracked::assume_new()`. Everything a verified
function relies on without proof is a seam: vstd's specifications of
`core`, the `assume_specification` contracts for `core` functions vstd
does not specify (in `src/vspec/`), and `external_body` functions. The
`global size_of usize == 8` directive in `src/vspec/mod.rs` is not a seam:
Verus checks it when it compiles the crate.
The seam ledger lists every seam but vstd's own (Seams, below).

## Coverage

```sh
cd verus/count && cargo run --release -- ../..          # the crate
cd verus/count && cargo run --release -- --list ../..   # per function
```

`verus/count` parses every file under `src/`, `verus!` blocks included,
with `verus_syn`, and counts executable functions and their lines as
verified, trusted (inside `verus!` but `external_body`, `external`, or
calling `assume`, `admit` or `Tracked::assume_new`) or outside `verus!`.
Ghost functions (`spec`, `proof`) are counted apart, except a proof taken
on trust (an `axiom fn`, or a `proof fn` that is `external_body` or calls
`assume`, `admit` or `Tracked::assume_new`), which counts as trusted. Code
compiled only under `test` (`cfg(test)`, `test` inside `cfg(all(..))`, or
`cfg(any(..))` with only such arguments) is left out, and so is a test
module's directory. So is an executable function whose own `cfg` holds
only under `test` or `verus_keep_ghost`, such as
`cfg(any(test, verus_keep_ghost))`: Verus verifies it, but no build ships
it. A proof under `verus_keep_ghost` still counts, because Verus uses it.
It reads source and does not run Verus, so "verified" means "under
`verus!` and not trusted"; `cargo verus verify` is what checks it. Shares
are to a tenth of a percent, rounded down.

`--floor FILE` holds the crate to [FLOOR](FLOOR): the verified share of
functions and of lines. The run fails below either figure, and above one
it names the figure to record. `--base-rev REV` (`make verus
VERUS_BASE=REV`; the check passes the pull request's base) also reads
`REV:verus/FLOOR` and fails a figure set below it, unless FLOOR has a
`lowered FIGURE P REASON` line for that figure and value that REV's copy
does not have. A REV that does not resolve to a commit fails the run.
FLOOR rises in its own pull requests between merges, to the figures the
check names, so parallel pull requests do not conflict on it; the floor is
lowered only on purpose, with the reason in FLOOR.

The [README badge](../README.md) shows a snapshot of the verified shares,
as `verus-main` reported them on main. The repository is private,
so shields.io cannot read the counts; the badge carries them in its
URL. It changes in the pull requests that raise FLOOR, to the figures
the check names, so parallel pull requests do not conflict on it. Each
`verus-main` log has the current figures.

The counter comes from pcre3's `verus/count`, cut to challoc's single
crate.

## Seams

```sh
sh verus/seams.sh           # print the totals
sh verus/seams.sh --write   # regenerate the ledger under verus/ledger/
sh verus/seams.sh --check   # fail when it is stale (`make verus` runs this)
```

The seam ledger is generated from `src/`, one file per source file
(`verus/ledger/src/a/b.rs.md` for `src/a/b.rs`), so changes to different
source files touch different ledger files. It lists every item Verus
takes on trust, with its place, the contract Verus assumes, a category, a
proposed tool to discharge it and a status ("trusted" until another tool's result discharges it, SEAMS.md). Regenerate it in the change that adds, moves or edits a seam.
`seams.sh` picks the category and tool from the item's kind, path and
body; a line in the source file's overrides file
(`verus/ledger/src/a/b.rs.overrides`) corrects them where the guess is
wrong, and the run fails on a seam it cannot classify. [SEAMS.md](SEAMS.md)
explains the kinds, categories, tools and statuses.

## Kani

`src/kani/` holds Kani harnesses (`cfg(kani)` only, so no ordinary build,
test or Verus run sees them) for seams of the ledger, mostly those whose
proposed tool is Kani: each checks a trusted item's body, or the `core`
function an `assume_specification` describes, against the contract Verus
assumes. The harnesses of items with private parts sit next to them, in a
`kani_*` module of the item's own file (`config::kani_config`, for one).
Each harness's comment names its seam and its bounds. A harness is
evidence; the ledger status stays "trusted" until an overrides line cites
it. The `kani` check (`.chainguard/ci/kani.yaml`) runs them under
both quanta; locally, with Kani 0.68.0 and CBMC 6.11.0 (Wolfi's `kani` and
`cbmc` packages):

```sh
cargo kani -Z unstable-options -Z stubbing -Z mem-predicates \
    --ignore-global-asm \
    --no-default-features --features quantum16      # or quantum8
cargo kani ... --harness kani_proofs::stats::fig_add_exact   # one harness
cargo kani ... --harness config::kani_config::get_contract   # one in a file
```

## Conventions

- Keep the executable code as it is. A rewrite needed only to get through
  Verus must behave the same, and must leave the release library's machine
  code for the hot set (`malloc`, `free`, `calloc`, `realloc`, `sdallocx`,
  `free_sized` and everything inlined into them) unchanged:
  `scripts/codegen-diff.sh compare` must report `changed 0` there
  (CONTRIBUTING.md, "Performance rules").
- A `static` computed by a `const fn` inside `verus!` is an `exec static`.
- Every loop needs `invariant` and `decreases`. Quantifiers name their
  triggers.
- A new `core` contract goes in `src/vspec/`, in the module for its type
  (`core_int.rs` for integers, `layout.rs` for `Layout`), checked against
  the `core` documentation. Verus rejects a second contract for the same
  item, so check the module first, and drop a contract once vstd states
  it.
- Imports of ghost-only items (vstd's lemmas and spec functions) carry
  `#[cfg(verus_keep_ghost)]`: ordinary builds do not have them.
- A specification change gets a mutation check: break the code the way the
  specification is meant to catch and confirm that Verus rejects it.
