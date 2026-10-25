# Verification map at `d5a006fa`

## What we did

This branch verifies challoc's own Rust source in place with Verus. Verus checks executable bodies against their contracts and erases ghost code from ordinary builds. The shipped code is unchanged: at every integration step, per-function x86_64 and aarch64 disassembly was compared for the hot set of `malloc`, `free`, `calloc`, `realloc`, `sdallocx`, and `free_sized`, including functions inlined into them. Read [the Verus introduction](verus/README.md) for the scope of a verified function.

- Verified challoc's own source in place with Verus under quantum16, quantum8, and aarch64.
- Stated and proved 46 whole-allocator theorems, with their premises in `verus/THEOREMS.md`.
- Built a ledger of all 424 seams and discharged 223 with Kani, loom, Isla, the empty-asm check, and RefinedRust; checked records link each discharge to its source contract.
- Bounded the fast path's machine-level cost in theorem 45.
- Kept the shipped machine code of the hot set unchanged across integration steps on x86_64 and aarch64.
- Recorded the 199 seams that remain trusted and why.

## Results

| Measure | x86_64 | aarch64 |
| --- | ---: | ---: |
| Combined coverage, Verus plus discharged seams: function lines | 35,580 / 35,938 (99.0%) | 35,606 / 35,964 (99.0%) |
| Combined coverage: functions | 1,407 / 1,446 (97.3%) | 1,407 / 1,446 (97.3%) |
| Verus alone: function lines | 34,498 / 35,938 (96.0%) | 34,494 / 35,964 (95.9%) |
| Verus alone: functions | 1,315 / 1,446 (90.9%) | 1,313 / 1,446 (90.8%) |
| Verus verification items, errors | 3,596 (quantum16), 3,596 (quantum8); 0 errors | 3,597; 0 errors |

| Artifact | Count |
| --- | ---: |
| Whole-allocator theorems | 46 |
| Seam ledger entries | 424: 223 discharged, 2 evidence, 199 trusted |
| Discharged seams by tool | Kani 148; loom 41; Isla 31; RefinedRust 3 |
| Interface matrix cells | 62 `P`; 28 `p` (partial); 6 `-` (missing); 163 `.` (not applicable) |

A function counts as verified when it is inside `verus!`, has neither `external_body` nor `external`, and has no `assume` or `admit` in its body. Combined coverage also counts a seam only when [the seam ledger](verus/SEAMS.md) marks it discharged and names a record. See [“What counts as verified”](verus/README.md#what-counts-as-verified) for the base rule. We measured these numbers at `d5a006fa` with a line counter over the `verus!` blocks and the seam ledger; the counter is not part of this repository.

## How we did it

### Verus proofs

Verus checks contracts on functions, loop invariants and decreases clauses, and proof functions for lemmas. `Tracked` ghost permissions model memory, locks, and atomics through `PointsTo`, atomic invariants, and lock tokens. Z3 discharges the resulting obligations. Near-limit proofs are split into helper proof functions or given a raised resource limit; each must verify under quantum16, quantum8, and aarch64. Verus uses a resource-count limit, not a time limit.

### Trusted seams

When Verus cannot inspect an item, its contract becomes a trusted seam in `verus/SEAMS.md`. A seam leaves the trusted set when Verus verifies it, or when another tool checks its contract and a record in `verus/seams/` links the check to the source. Each record states what its check does not cover. The ledger's `evidence` status does not discharge a seam.

| Tool | Scope of a result |
| --- | --- |
| Kani | Every input of a harness for its instantiated types, subject to the harness's stated bounds. |
| loom | C11 interleavings in its model and, where set, its preemption bound. |
| Isla | Instruction semantics of the compiled inline assembly against the selected ISA models. |
| Empty-asm check | Empty-template `asm!` blocks add no instruction to the compiled code. |
| RefinedRust | Refinement of the specified Rust body. |
| Miri | Evidence only; no seam discharge. |

### Integration gate and history

Each change passes Verus in three configurations, a release build, tests, formatting, `sh verus/seams.sh --check`, the source seam-reference check, and the Kani, loom, Isla, and RefinedRust record checks. The gate also compares per-function disassembly on x86_64 and aarch64 with the preceding build, requires the hot set to match, and runs `scripts/perf-budget.sh`. A tool reruns when a record it checks changes. The branch is a linear sequence of commits, each of which passed the gate, one per package or theorem.

## What is trusted

| Group of trusted seams | Count |
| --- | ---: |
| libc, kernel, and C++ calls | 40 |
| Rust core library items that vstd does not specify | 43 |
| Allocator-state invariants | 41 |
| Raw-pointer arithmetic and mapped-memory permissions | 27 |
| The C caller's promises | 24 |
| Cells, kernel waits, and one panic-message axiom | 24 |
| **Total** | **199** |

The remaining trusted set includes slice constructors (31 lines), `Lock::cell_ref` and `Lock::cell_mut` (22 lines; verified versions changed hot-path codegen), and addresses of statics (44 lines). The proof model also treats a reference's address (`shared_ptr`, `shared_arg`) and the heap as one permission (`slot_heap_ref`, `heap_at`) as limits. The toolchain is part of the trusted base: Verus, Z3, rustc 1.95.0 and LLVM, Kani/CBMC, loom, Isla models, and RefinedRust/Rocq/Iris. No tool here checks Rust's Tree Borrows aliasing model.

## Map of the repository

| Path | What it holds |
| --- | --- |
| `src/` | 93 Rust files with `verus!` blocks: `src/vspec/` 34, `src/heap/` 13, `src/` top level 12, `src/sys/` 11, `src/class/` 8, `src/api/` 7, `src/purge/` 2, and `src/layout/` 2. Other directories account for the remaining 4. |
| `src/vspec/` | Verus wrappers and specifications for atomics, pointers, cells, slices, errno, user memory, and zeroing. |
| `src/kani/`, `src/loom_tests/` | Kani harnesses under `cfg(kani)` and loom models under `cfg(loom)`. |
| `verus/README.md` | How to run Verus and what counts as verified. |
| `verus/THEOREMS.md` | 46 theorem statements, premises, proof locations, and the interface matrix. |
| `verus/SEAMS.md` | Generated ledger: seam number, source location, kind, category, tool, status, and contract. |
| `verus/seams/` | Per-seam records that connect a discharged or evidence result to its source contract. At this tip, the directory has 227 Markdown records. |
| `verus/seams.sh`, `verus/seams.overrides` | Generate and check the seam ledger. |
| `verus/TODO.md` | Open work and known gaps. |
| `verus/WEAKMEM.md` | Weak-memory scope and loom results. GenMC was not installed; every reported result is from loom. |
| `verus/ISLA.md`, `verus/isla/` | Scope, models, and instruction-level checks of inline assembly. |
| `verus/emptyasm/` | `run.sh` and `blocks.txt`: check that empty-template `asm!` blocks emit no instruction. |
| `verus/kani/`, `verus/loom/` | `run.sh` and `pins.txt` for each tool; `verus/loom/` also holds the loom DPOR patch. |
| `verus/refinedrust/` | RefinedRust proofs and `run.sh`. |
| `verus/miri/`, `verus/MIRI.md` | Evidence only. |
| `verus/audit/` | Hand audits of the C boundary, invariants, library control, and memory; historical S-numbers. |
| `verus/perf-budget.txt`, `verus/perf-budget-q8.txt`, `scripts/perf-budget.sh`, `scripts/perf-budget.rs` | Theorem 45: fast-path instruction, load, store, branch, and cache-line bounds for pinned x86_64 release builds. |
| `scripts/codegen-diff.sh` | Per-function machine-code comparison. |

## How to re-check

Run these commands on Linux with Verus `0.2026.10.18.7d82994` and `vstd =0.0.0-2026-10-18-0147`, as specified in [`verus/README.md`](verus/README.md#running). Each clean Verus run takes about four minutes on eight cores; with the same source and tool versions, its resource-count result is deterministic.

```sh
cargo verus verify
cargo verus verify --no-default-features --features quantum8
cargo verus verify --target aarch64-unknown-linux-gnu
sh verus/seams.sh --check
sh verus/kani/run.sh
sh verus/loom/run.sh
sh verus/isla/run.sh
sh verus/emptyasm/run.sh
sh verus/refinedrust/run.sh
sh scripts/perf-budget.sh
cargo +1.95.0 build --release
cargo +1.95.0 test
```

The tool runners document their dependencies, pin files, record checks, and limits. For a before-and-after codegen comparison, use `sh scripts/codegen-diff.sh capture` on the base build and `sh scripts/codegen-diff.sh compare` on the candidate build on each architecture.

## Where to start reading

- Read the introduction to [`verus/THEOREMS.md`](verus/THEOREMS.md), then one theorem's statement, premises, and proof location.
- Read one [discharged pointer record](verus/seams/vspec-ptr-ref_ptr.md), including its translation and limits.
- Use the [seam index](verus/SEAMS.md#index) to find every contract a theorem takes on trust.
- Read [`src/heap/`](src/heap/) for the core allocator proofs.
- Read [`verus/TODO.md`](verus/TODO.md) for open gaps.
