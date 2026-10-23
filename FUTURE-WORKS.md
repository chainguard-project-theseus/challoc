# Future verification work

challoc has 99.0% combined coverage on x86_64 and aarch64; [VERIFIER-README.md](VERIFIER-README.md) defines that measure. This file lists what remains for challoc and what we assessed for the next library.

## challoc: what is left

| Item | Why it is open | First step |
| --- | --- | --- |
| `Heap::home_ptr_at` ([S111](verus/SEAMS.md), 12 lines) | Commit `926af8ac` on a work branch discharges it with Kani, but it is not on `verus`. With it, the projected reachable ceiling is about 99.07%. | Bring the commit through the [integration gate](VERIFIER-README.md#integration-gate-and-history) before it lands. |
| Theorem 44's pending checks | We have not proved that the abort paths in `ClassState::walk_empty` and `unbind_unbound` are unreachable. [Theorem 44](verus/THEOREMS.md) names the missing invariants. | Relate the empty set to slab state and live counts; bound homed heaps against the `u32` ticket count. |
| Theorems 10, 16, 22, and 37 | Theorem 10 applies when the thread destructor runs. Theorem 16 partly trusts `axiom_unlocked`. Theorems 22 and 37 hold at quiescence. | Prove each premise in the source path that establishes it, then strengthen the theorem statement. |
| Interface matrix | Of 259 cells, 6 are missing (`-`) and 28 are partial (`p`) in [the matrix](verus/THEOREMS.md#interface-contract). | Take one missing cell, name its contract and premises, and add a proof or a recorded seam check. |
| Rust aliasing | No tool here checks the Tree Borrows model. Miri cannot follow the C programs that call challoc. | Report the BorrowSanitizer run described below, retry on a later release, or select tests that do not build a full allocator. |
| Weak memory | The 41 seams marked loom/GenMC were checked with loom only; GenMC was never installed ([WEAKMEM.md](verus/WEAKMEM.md)). | Run GenMC models of the same protocols and record behaviors outside loom's model. |
| aarch64 cost bounds | [Theorem 45](verus/THEOREMS.md#45-fast-path-cost-bounds-machine-level-pinned-build) covers only a pinned x86_64 release build. | Extend the cost checker and pinned table to an aarch64 release build. |
| 199 trusted seams | Allocator-state invariants (41) and Rust core items absent from vstd (43) are the largest groups we can reduce. libc, kernel, and C++ calls (40) and C caller promises (24) remain external premises. | Move allocator facts into ghost state; contribute core specifications to vstd where their contracts can be checked. |
| Slice constructors, lock cells, static addresses | Slice constructors cover 31 lines and need a Verus constructor without a compile-time length. `Lock::cell_ref` and `Lock::cell_mut` cover 22 lines; verified versions changed hot-path codegen, while the other route needs a raised compiler recursion limit. Static-address identities cover 44 lines. | Revisit the constructor and compiler limits, then run the codegen gate before replacing each trusted contract. |
| Proof-model limits | `heap::paths::shared_ptr` and `purge::background::shared_arg` rely on a reference's address. `slot_heap_ref` and `heap_at` treat the heap as one permission. Verus and vstd lack the needed features; a reference-address specification would be unsound today. | Wait for a sound reference-address model and a persistent heap permission, then replace the corresponding seams. |
| Trust in the tools | The result trusts Verus and its translation to Z3, among other tools named in [VERIFIER-README.md](VERIFIER-README.md#what-is-trusted). | Evaluate an independent check of Verus's output, such as translating its intermediate representation to Lean. |
| Keeping up with challoc | The main branch continues to change. This branch has ported changes through `a8baf658`. | Port each subsequent change and run the full integration gate again. |

We tried BorrowSanitizer 0.1.0 (`ghcr.io/borrowsanitizer/bsan`, 2026-10-21) on the unit tests. It built challoc and passed 4 tests. During `api::jemalloc::tests::get_defrag_hint_follows_the_utilization_data`, resident memory rose from 0.2 GB to 96.6 GB over 33 minutes before the kernel killed it; virtual size stayed near 112 TiB of unbacked reservations. The resident growth came from the tool's metadata, so this run did not finish an aliasing check of the suite.

## The next library: what we assessed

We assessed candidates on 2026-10-08 and later using the runtime reverse-dependency closure of the public Wolfi x86_64 APKINDEX. We counted `so:` providers and excluded `-dev` packages. Package YAML build dependencies connect nearly everything, giving every library 9,271 dependents, while YAML runtime lists miss the `so:` dependencies melange adds; map providers that have multiple atoms.

| Library | Direct runtime dependents | Transitive runtime dependents |
| --- | ---: | ---: |
| zlib | 637 | 8,747 |
| bzip2 | 64 | 7,742 |
| libcrypt | 102 | 6,382 |
| expat | 59 | 5,664 |
| xz | 62 | 5,348 |
| pcre2 | 122 | 1,355 |
| zstd | 90 | 1,183 |
| libpng | 71 | 1,029 |
| libseccomp | 15 | 944 |
| libxml2 | 155 | 697 |
| c-ares | 112 | 486 |
| libtasn1 | 6 | 345 |
| lz4 | 32 | 286 |

### Ranking

1. **zlib through zlib-rs:** 637 direct and 8,747 transitive dependents; RFC 1951 specifies inflate, and the Rust port leaves verification and integration rather than a port.
2. **expat:** 5,664 transitive dependents; XML 1.0 specifies the format, but expat returns events, so we must scope callback and entity behavior before claiming “one tree or error.”
3. **bzip2 through libbzip2-rs:** 7,742 transitive dependents and an existing Rust implementation; this is the alternative second library for testing whether the zlib approach repeats.
4. **libxcrypt:** 6,382 transitive dependents; bound the queued Kani no-panic and Verus SHA-2 work, and price a Frama-C trial on the C against a Rust port.
5. **xz, lz4, and libtasn1:** Format specifications exist; their transitive reach is 5,348, 286, and 345, respectively.

Defer libxml2 (697 transitive dependents; XML with DTDs and external entities needs a larger contract) and pcre2 (no formal semantics; last).

Our queued plan put libxcrypt first. A business review on 2026-10-08 recommended zlib-rs and expat, with one reviewer proposing bzip2 in expat's place, and skipping pcre2 and libxml2. This ranking follows that review and keeps libxcrypt as a bounded experiment. A later review round on 2026-10-20 suggested a separate pilot outside verification: data-only builds of managed-language parsers, such as SnakeYAML, that remove input-selected code execution; that pilot is outside this file's scope.

A formal specification affects proof difficulty more than source line count. Candidate specifications include RFC 1951 for zlib inflate, XML 1.0 for expat, and formats for lz4, xz, ASN.1/DER in libtasn1, and bzip2.

| Candidate | Scope and obstacle | First step |
| --- | --- | --- |
| zlib through zlib-rs | The Rust port exists; RFC 1951 specifies inflate. | State the inflate contract, verify the decoder, and integrate the result. |
| expat | XML 1.0 specifies the format; expat emits callback events and handles entities. | Scope the callback and entity contract before making a tree-or-error claim. |
| bzip2 through libbzip2-rs | A Rust implementation exists. | Test whether the zlib verification and integration approach repeats. |
| libxcrypt, the queued plan | Port `lib/`: 18,557 lines including generated tables (`alg-des-tables.c` 3,858; GOST precalculation 1,426). `yescrypt-opt` has 1,568 lines and bcrypt 1,061. | Use Kani for no-panic on bounded setting and passphrase parsing, then prove Verus SHA-2 against FIPS 180-4. Price a Frama-C trial on the existing C first to compare verification of C with a Rust port. |
| xz, lz4, and libtasn1 | Format specifications exist for all three. | Pick one format and state the decoder contract before choosing a proof target. |
| libxml2, deferred | XML with DTDs and external entities needs a larger contract. | Define a bounded contract before reconsidering it. |
| pcre2, last in our candidate order | About 48k core C lines excluding tools (22.1k), JIT/sljit (22.1k), and tables (7.8k). `pcre2_compile.c` has 12,107 lines, `pcre2_match.c` 9,020 with heap-frame backtracking, and the DFA matcher 4,580. It has 174 opcodes and 41k lines of test input. Warblre's Coq specification covers ECMAScript regexes, not PCRE recursion, callouts, or backtracking verbs. | Keep JIT out of scope. Preserve a well-formed compiled-bytecode invariant from compile to match. Test a subset that rejects unsupported syntax rather than diverging, with Kani bounded equivalence against a naive backtracker. |
| uutils/coreutils | Already Rust, 151.5k lines. Whole-library Verus work would span many commands. | Add `cfg(kani)` harnesses upstream and use Verus on parsing kernels. |

## What to reuse from challoc

- Keep the [seam ledger](verus/SEAMS.md), per-seam records, and checks that a record still matches its source contract.
- Run the [integration gate](VERIFIER-README.md#integration-gate-and-history): three Verus configurations, codegen diffs for the hot set, and record checks.
- Keep one package or theorem per commit.
- State each theorem as narrowly as its proof supports, with its premises.
- Run provers in disposable cluster VMs. A 28 GB Lean process once took down a workstation.
- Bring a Z3 binary with prover jobs; apk does not package it.
- Limit the public claim to the proved statement and its premises.
