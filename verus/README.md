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

[RESULTS.md](RESULTS.md) summarizes what is proved.
[THEOREMS.md](THEOREMS.md) states each whole-allocator theorem as the
code does, with the entry points it covers and every trusted premise it
rests on, by `file:line`.

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
compiled only under `test`, `kani` or `challoc_tls_check` (the debug TLS
check build) is left out (`cfg(test)`, such a name inside `cfg(all(..))`,
or `cfg(any(..))` with only such arguments), and so is a test module's
directory. So is an executable function whose own `cfg` holds only under
those names or `verus_keep_ghost`, such as
`cfg(any(test, verus_keep_ghost))`: Verus verifies it, but no build ships
it. A proof under `verus_keep_ghost` still counts, because Verus uses it.
It reads source and does not run Verus, so "verified" means "under
`verus!` and not trusted"; `cargo verus verify` is what checks it. Shares
are to a tenth of a percent, rounded down.

The counter prints two rows. The first is the verified share: functions
and lines Verus checks. The second is the combined share: verified
functions plus the trusted functions whose seam another tool has
discharged. It reads the seam ledger (Seams, below) and matches each
trusted function to its entry by the `src/FILE:LINE` of its `fn` keyword.
A function counts as discharged when its entry's status is "discharged",
meaning a check outside Verus (a Kani harness, a const assertion, an
Isla run or `make emptyasm`) covers its whole contract, as
[SEAMS.md](SEAMS.md) ("Status") defines it, or "discharged on premises
(Isla: ...; premises: ...)", meaning an Isla run covers it but for the
platform premises (P and K labels) the status names, or "discharged on
premises (Kani: ...; premises: ...)", meaning a Kani harness covers it
but for the C premises and the loader and kernel premises (C and L
labels, [kani/premises.md](kani/premises.md)) the status names. A
function whose status is "trusted (partly checked, ...)" is shown in its
own column and
not counted in the combined share, since part of its contract is still
assumed. Discharged seams that are not functions
(constants, types, `assume_specification`s) do not enter either share.
Only an `external_body`, `external_body proof` or `axiom` seam classes a
function. A trusted function with no such entry is a warning; a function
under `#[verifier::external]`, which the ledger does not list, always
gets one. The run fails when a discharged seam of those kinds matches no
trusted function, so moving its function cannot silently drop its
coverage, when two of them share a line, or when the ledger names a kind
the counter does not know.

`--floor FILE` holds the crate to [FLOOR](FLOOR): the verified share of
functions and of lines (`functions`, `lines`), and the combined share of
each (`combined-functions`, `combined-lines`). The run fails below any
figure, and above one it names the figure to record. A FLOOR without the
combined figures reads them as 0.0, so a base revision from before they
existed still compares. `--base-rev REV` (`make verus
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

## Restated bodies

Four verification-only theorems restate executable code instead of
calling it, because they check its callees in a context the code's own
contract does not give:

- `api::libc::theorem_no_syscall_on_fast_paths` restates the fast paths
  of `malloc_class` (path 0), `free_block` (path 2), `free_sized_block`
  (path 3) and `free_usable_block` (the last path);
- `api::theorem_read_only_never_aborts` restates the calls of
  `jemalloc::nallocx`, `challoc_features` (with `feature_line_ready`'s)
  and `glibc::mallinfo2`;
- `api::mallctl::theorem_mallctl_reads_never_abort` and
  `theorem_mallctlbymib_reads_never_abort` restate the bodies of
  `mallctl` and `mallctlbymib` after their `shared_published`.

Verus checks only the callees a restatement calls, not that it still
matches its body. The tests in `src/api/restated.rs` do:

```sh
cargo test --release --locked restated
```

Each reads the body and its restatement from the source and compares the
names of their exec calls, in source order, with comments, attributes and
ghost code (`proof` blocks, `let tracked`, `let ghost`, `assert`,
`assume` and `reveal`, `Tracked` and `Ghost` paths) left out. A call added to or removed from either side fails
the test until the other side follows. Each case lists the call names it
skips on both sides, with the reason: the slow-path tail calls, the tests
that pick the fast or slow path, the class computation a fast path takes
from its callers, `shared()`, and `mallinfo2`'s arithmetic. The
tests compare names and order only, not arguments, control flow or how
often a call runs, and they cover only the bodies listed above.

## Isla

[ISLA.md](ISLA.md) checks the inline-assembly seams with
[Isla](https://github.com/rems-project/isla) on Sail's Arm and x86
models: the compiled code of each `asm!` block, traced instruction by
instruction and composed by `verus/isla/check.rs`. The seams it
checks, with the contract each run is held to, are in
[isla/seams.md](isla/seams.md); their overrides lines cite `Isla:
verus/isla/seams.md`, in `discharged (...)` or, where part of the
contract rests on premises the runs do not reach, `discharged on
premises (...; premises: ...)`. `make isla`
runs it, with the `isla`, `isla-snapshots`, `sail`, `isla-sail`, `z3` and
`llvm-21` packages.

`make emptyasm` (`verus/emptyasm/run.sh`) covers the four register
hints whose `asm!` template is empty (`verus/emptyasm/blocks.txt`): it
checks that the release build's assembly and object code hold no
instruction for them on the host's architecture, and that a `nop` added
to each template fails the check. It needs `rust-1.98`, `build-base`, `llvm-21` and git.
A core-library block in the build (today the empty one rustc emits for
`core::hint::black_box`) passes only as `verus/emptyasm/core.txt` names it.
That discharges `word_index`; the other three stay trusted, partly
checked (their ledger notes say what the check leaves out).
`ClassLine::guard_word`'s load is also checked by Kani, on a copy
without the `asm!` block: `seams.sh --check` requires the copy's lines
to equal the original's but for the rows `verus/kani/pins.txt` marks
`-only` (today the opening brace and that block).

## Guard tests

Some trusted items are sound only where they are called. An axiom such
as `TlsSlot::entry` or `lock::axiom_entry` states a premise about its
caller (one slot token per top-level call; no allocator lock held) that
Verus cannot check, and a `lock_inv` axiom says that every holder of a
lock leaves its invariant true, which Verus does not check when a guard
drops. Each such item's doc names its call sites or holders and argues
the premise for each one. Nothing in Verus ties that list to the code, so
unit tests (`cargo test`) scan `src/` and fail when a call site or holder
appears that the list does not name, or a listed one goes away. Most use
`testutil::assert_only_at(call, &[(file, fn, count)])`, and most pinned
items' docs name the test that checks them. A separate guard,
`no_assume_or_admit_in_verified_code`, keeps shipped code free of
`assume`/`admit` and of verifier attributes the seam ledger does not know.

| test | what it pins |
|---|---|
| `tests::no_assume_or_admit_in_verified_code` | no `assume(..)` or `admit()` in shipped code; verifier attributes spelled `#[verifier::name]`, `name` from a fixed list |
| `sys::tls::tests::tls_entry_only_at_listed_users` | `TlsSlot::entry` once in each slot user `TlsSlot`'s doc lists (twice in `heap::mallocx::alloc`, on match arms that exclude each other) |
| `api::tests::c_call_tokens_only_in_export_bodies` | `axiom_c_call`, `axiom_c_out` and `errno::lend` at most once per export body (`extern "C" fn`, `verified_export_pair!` arm, or a helper one arm calls), each token for the call's own arguments (every `CEntry` field and out-pointer a parameter of the minting function, none twice); the 4 `axiom_c_out` mints (the three allocating `challoc_*_usable` exports and `memptr_out`) and the one `memptr_out` call each in the spelling its hand-back names |
| `api::tests::exports_return_their_callees_result` | `vspec::errno::axiom_errno_at_return` only right after the call in the export bodies that lent the token; `axiom_out_at_return` only right after the call in the 4 export bodies that minted an out-pointer token |
| `tests::trusted_mint_sites_are_pinned` | the stores to and takes from `Heap::head` (`heap_new`, `Heap::destroy`), `HeapPerm::assemble` (`heap_new`), `ZeroRaw::axiom_new`, the refill's zero and fresh-slab axioms and `purged_zero`, `axiom_shared_regions` and `axiom_shared_meta`, `TlsCell::note_create`: each at the callers its doc names |
| `lock::tests::entry_lock_order_uses_are_listed` | `axiom_entry` in the functions its doc lists |
| `api::tests::lock_holders_match_inv_docs` | the functions that take each lock's guard, against the holders its `lock_inv` doc names, copied into the test by hand; raw `lock_held`/`unlock_held` only in the fork handlers; `value_held_mut` once |
| `lock::tests::permission_axioms_are_called_only_at_acquisitions` | the lock-cell permission axioms at the acquisitions |
| `sys::tests::{syscall,abort}_grants_only_at_roots`, `user_read_grants_only_in_listed_callers`, `user_read_capability_ensured_only_at_grants`, `sys::abort::tests::abort_relabels_only_in_the_sinks` | the syscall, abort and user-read grants; the abort relabel only in the three narrowed sinks; `may_read_user` in an `ensures` only at `grant_user_reads` and `purge::check_zero` |
| `heap::tcaches::tests::heap_use_axiom_only_at_listed_mint_sites` | `axiom_heap_use` at its 8 listed callers |
| `api::tests::fork_hold_is_resumed_once`, `fork_child_resets_are_child_only`, `fork_lock_list_matches_the_source_tree` | the fork handlers' premises and lock list |
| `huge::tests::zero_axioms_called_from_listed_sites`, `purge::tests::state_lock_axiom_names_the_one_state_lock` | the huge zero axioms (in `zero_run`, and `axiom_fresh_anon_zero` in `SlabTable::new`); the one state lock |
| `lock::tests::value_ptr_called_only_by_put_stats` | `Lock::value_ptr` once, in `Arena::put_stats`, on the pointer its permission names, at a `T` the Kani harness runs |
| `arena::array_layout_tests::array_layout_only_at_asserted_instances` | `axiom_array_layout` only at the instances the build's `const` assertions state, and generically in `split_array`, which is called only on fields of those types |
| `api::mallctl::tests::c_buffer_reads_are_listed` | `from_raw_parts` and `read_unaligned` in verified code only in `mallctl`'s `name_bytes`, `load`, `query_ptr` and `mib_in`; those helpers used only in the functions that read the C program's name, `newp` and MIB, and keeping the `requires` that make what they read the caller's buffer (premise C2, [kani/premises.md](kani/premises.md)); the `callers:` test of the `from_raw_parts` and `read_unaligned` seams. Mutants killed: a use in another file (`config` `cstr`), in another fn (`old_len`), in method form (`name_byte`), through a `use .. as` inside a fn, through a turbofish (`load::<u8>`) and a `self::` path, a second use and a removed use in `mib_in`, `load` called from a fn not listed for it (`batch_query`), a dropped `c_readable` requires (`load`), a `&'static` return on `name_bytes`, and a one-line `#[inline(always)] fn` (12 mutants; uses in an `external_body` fn, in a comment and of another module's `load` pass, as they should). Known escapes: a rename through a `use .. as` outside `verus!` (its later uses are not seen), a use a `macro_rules!` defined outside `verus!` expands to (the scan reads source lines, not expansions), and a helper's kept clause moved or weakened inside its item (the clauses are matched as text from the `fn` line to the body's `{`, so one moved to `ensures` or guarded by `false ==>` passes the scan) |
| `vspec::ptr::array_view_tests::array_view_only_at_the_checked_instance` | `axiom_array_view` once, in `heap::others::node_view`, at the `[CacheHeader; NCLASSES]` its Kani harness checks |
| `rseq::tests::probe_reads_are_listed` | `rseq::read_global` and `rseq::read_cpu_id` called only in `rseq::probe` (and their Kani harnesses), on the `symbols` results and `area` of the offset read, with `probe`'s and `area`'s code line for line, and `symbols`' signature, `Probe`'s `size`, `flags` and `offset` and `CPU_ID_OFFSET` as listed (premises L1 and L2, [kani/premises.md](kani/premises.md)); the `callers:` test of the `read_global` and `read_cpu_id` seams. Mutants killed: a dropped `ss` null check, `area(offset + 8)`, `area(0)`, a negated offset in `area`, a shadowing `let offset`, a fourth `read_global` call in `probe`, a third in its harness, a call in another fn (`reread`), a function pointer through a turbofish (`read_global::<u32>`), a `use .. as` rename of `read_cpu_id`, `flags` read through `ss`, a size guard without its `+ 4`, and `CPU_ID_OFFSET` changed (13 mutants). Known escapes: a call a macro assembles from parts (`paste!`, `concat_idents!`), and code `include!`d from outside `src/` (the scan reads the source lines under `src/`) |

The tests check where a premise is used, not that it holds: a new site
fails a test so that a person reads its argument. The call-site scans
(`assert_only_at`) match exact spellings on one line (a call through a
`use` alias or `Self::` is not seen), and they cannot tell that two calls
in one function sit on arms that exclude each other (the two in
`heap::mallocx::alloc` and in
`api::libc::theorem_no_syscall_on_fast_paths`): a reviewer checks that.
For a reviewer:

- A pull request that adds a call to a pinned axiom, or a new holder of
  a lock, updates the axiom's doc list (with why the premise holds at the
  new site) and the test's list in the same change. Ask why the premise
  holds there. A test list edited with no matching doc change is the
  thing to question.
- A failing guard test is fixed by arguing the new site in the doc (or
  removing the call), never by editing the test alone.
- A new trusted item whose premise is about its caller gets a doc list
  and a row in a guard test (`trusted_mint_sites_are_pinned` takes one).
- A new `#[verifier::name]` that trusts code becomes a seam kind in
  `verus/seams.sh` before it goes on the list in
  `no_assume_or_admit_in_verified_code`.

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

## Export boundary

Each `extern "C"` export in `src/api/libc.rs`, `cxx.rs` and `jemalloc.rs`
is a wrapper: it lends the call's `errno` token and the C caller's
tokens, calls its Rust entry point, and returns that call's result. Its
`ensures` restate the entry point's in the export's own arguments and
result, so what a C caller is promised is what the entry point proves.

An entry point's claims about `errno` name the token the wrapper lent,
which the export's `ensures` cannot name. One trusted axiom carries
them across: `vspec::errno::axiom_errno_at_return` uses the token up
after the call and states that, under a `failed` condition the export
passes, its value is the `errno` the C caller reads on return
(`vspec::errno::c_errno`). With it, `malloc` ensures
`r@.addr == 0 ==> c_errno() == ENOMEM`, and so do `calloc`,
`aligned_alloc`, `memalign`, `valloc`, `pvalloc`, their `je_` twins,
`challoc_malloc_usable` and `challoc_calloc_usable` (with `EINVAL` for
a bad alignment). `realloc`, `reallocarray` and `challoc_realloc_usable`
ensure it for every null result except a size-0 `realloc` of a block,
and `posix_memalign` ensures `r == ENOMEM ==> c_errno() == ENOMEM`.
The claim is true only when no write to `errno` follows challoc's last
write in the call, so `failed` names the results the entry points return
right after that write: a null result other than a `realloc` of a block
to size 0, and `posix_memalign`'s `ENOMEM`. A `realloc` to size 0 frees
the block, and the free may create the thread's heap, where a failing
`pthread_setspecific` writes `errno`. Verus proves a `c_errno()` clause
only where `failed` holds, so it rejects a clause about a success path.
It does not check `failed` itself (`true` would pass); the shape test
requires each export's own condition. The ledger lists the axiom as a
C-program contract.

Out-pointer results cross the same way. An export that takes an
out-pointer mints the call's token for it (`axiom_c_out` in
`src/api/libc.rs`), which holds what the caller left there
(`vspec::cbuf::c_out_entry`), and lends it `&mut` to its entry point;
after the call, `axiom_out_at_return` uses the token up and states that
what it holds is what the C caller reads (`vspec::cbuf::c_out`). With
it, `posix_memalign` ensures that on a 0 return `*out` holds a non-null
block that fits the request, and that on `EINVAL` or `ENOMEM` `*out` is
what the caller left there, as in glibc. `challoc_malloc_usable`,
`challoc_calloc_usable` and `challoc_realloc_usable` ensure that a
non-null `usable` holds the block's usable size, or 0 for a null result
(`api::libc::reports_usable`). This axiom takes no `failed` condition:
the token holds the only permission to `*out` for the call, and the C
contract keeps `out` apart from challoc's memory, libc's, and the blocks
the call frees or hands out, so nothing else writes it. The ledger
lists it as a C-program contract.

With the `errno` axiom, Verus rejects a `malloc` export that drops its
entry point's block and returns null, as long as the export does not
also write or read the `errno` token itself; the shape test below rules
that out. For `operator new`'s `nothrow_t` forms, `mallocx` and
`rallocx`, Verus cannot tell a null result from a dropped block at all:
their contracts allow null at any size and say nothing about `errno`
(the C++ `new_handler` may write it; jemalloc's `*allocx` leave it
alone). Two tests cover every export:

- `api::tests::exports_return_their_callees_result` reads the export
  modules' source and fails unless each export's body is the setup lines,
  one call of a `super::` entry point, and that call's result (as the
  tail, or as `r` after the axiom on the lent token with the export's own
  `failed` condition, and the hand-back of the out-pointer token the
  body minted, if any); unless the axioms are named only there, and
  tokens are lent only by exports, one each; and unless no code names an
  `exports` module, so no verified code calls an export.
- `c_export_results` (`tests/c/export_results.c`) calls the C exports
  under the preload: an impossible size returns null with the entry
  point's `errno`, and a small one returns a block. `cxx_new`
  (`tests/c/cxx_new.cpp`) checks that each of the four `nothrow_t` `new`
  forms returns a block for a small size.

Reviewer checklist: an export returns its callee's result; the `errno`
and out-pointer axioms are called only at the export boundary, after
the call, on the export's own tokens; and each export's `failed`
condition (`FAILED` and `failed_for` in `src/api/mod.rs`) holds only on
a result that challoc's own `errno` write precedes.
`exports_return_their_callees_result` and `c_export_results` enforce
the first two. For the third, Verus proves a `c_errno()` clause only
under the export's `failed`, and the shape test rejects a `failed` other
than the one `failed_for` gives that export, so what is left to review
is the 4 conditions and which exports get each.

## Gate history

Main has no merge commits. After the 6 commits that imported the project, every commit on main came from a pull request. [#136](https://github.com/wolfi-dev/challoc/pull/136) landed rebased, as `7a12536a` and `75e665c6`, with the user who merged it as committer, after its head passed `Chainguard Checks (presubmit)`; every other pull request commit has GitHub as committer. [#132](https://github.com/wolfi-dev/challoc/pull/132) moved CI to Chainguard Checks on 2026-10-12. A repository ruleset, created on 2026-10-12 and last changed on 2026-10-29, requires the aggregate `Chainguard Checks (presubmit)` status before a change lands on main; these are its settings as read on 2026-11-02, and its settings before 2026-10-29 are not recorded. It does not require a branch to be up to date with main, so 2 pull requests that each pass presubmit alone can fail together. The postsubmit checks rerun after every landing to catch that; they cannot block a landing, and a failure points at the commit that landed. `codegen` and `codegen-arm64` are advisory, because an executable change is expected to change some functions; review reads their output against the hot set.

The verification checks were added in this order (merge dates in UTC):

| Merged | PR | Checks added |
| --- | --- | --- |
| 2026-10-25 | [#141](https://github.com/wolfi-dev/challoc/pull/141) | `verus` presubmit on x86_64 (counter tests, proofs and coverage floor) and `verus-main` postsubmit |
| 2026-10-25 | [#142](https://github.com/wolfi-dev/challoc/pull/142) | `sh verus/seams.sh --check` as the first step of `make verus` |
| 2026-10-26 | [#157](https://github.com/wolfi-dev/challoc/pull/157) | `verus-arm64`, natively on arm64 |
| 2026-10-28 | [#264](https://github.com/wolfi-dev/challoc/pull/264) | quantum 8 proofs in the Verus checks; advisory `codegen` and `codegen-arm64` |
| 2026-10-30 | [#329](https://github.com/wolfi-dev/challoc/pull/329) | `kani` presubmit on listed paths |
| 2026-10-31 | [#338](https://github.com/wolfi-dev/challoc/pull/338) | `kani-main` postsubmit over every harness |
| 2026-11-01 | [#364](https://github.com/wolfi-dev/challoc/pull/364) | `kani-boolcmp` presubmit lint and `kani-ignoring` postsubmit |
| 2026-11-01 | [#414](https://github.com/wolfi-dev/challoc/pull/414) | `isla` presubmit on aarch64 for listed paths, and `isla-main` postsubmit on arm64 |
| 2026-11-01 | [#415](https://github.com/wolfi-dev/challoc/pull/415) | x86_64 legs in `isla-main`, which runs them only after a landing |
| 2026-11-02 | [#459](https://github.com/wolfi-dev/challoc/pull/459) | `kani-main` dropped; `kani-ignoring` runs every harness after every landing |
| 2026-11-02 | [#423](https://github.com/wolfi-dev/challoc/pull/423) | `perf-budget`: fast-path cost bounds on the x86_64 release binary at both quanta (`scripts/perf-budget.sh`), which checks theorem 45 |
| 2026-11-03 | [#465](https://github.com/wolfi-dev/challoc/pull/465) | `emptyasm` legs in `.chainguard/ci/isla.yaml` (`make emptyasm`): the empty-template `asm!` register hints add no instruction |

A check could not stop a change that landed before it was added. The `kani` and `isla` presubmit checks run only on pull requests that change their listed paths; `kani-ignoring` and `isla-main` cover the rest after the landing.
