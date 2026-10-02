# Verus proof TODO

## Where things stand

Checked October 2, 2026, on branch `verus` at `e790f43` plus the `THEOREMS.md` commit, based on upstream main `0f6ae74`:

- `verus-count` skips `#[cfg(test)]` modules. It reports 927 of 1,164 non-test functions verified (79.6%) and 19,879 of 21,894 function lines (90.8%). Another 200 functions inside `verus!` are trusted.
- `quantum16`, `quantum8`, and `--target aarch64-unknown-linux-gnu` verify with zero errors. `.github/workflows/verus.yaml` runs all three.
- The hot set — `malloc`, `free`, `calloc`, `realloc`, `sdallocx`, `free_sized`, and everything inlined into them — compiles to the same machine code as upstream on x86_64 and aarch64. The codegen check lists the functions that differ; all are cold (78 on x86_64).
- All six whole-allocator theorems hold for the thread-cache configuration. [THEOREMS.md](THEOREMS.md) gives their exact statements and premises.

The proofs began on the pre-cleanup tree (`verus-prototype` at `641d01c` and the `verus-prototype-*` branches), then moved to the rewritten upstream. Upstream removed the per-CPU mode, prefault, and group handoff that those proofs also covered.

Verification found these bugs:

- [#1](https://github.com/chainguard-dev/challoc/pull/1), a batch query length wrap and out-of-bounds write; [#2](https://github.com/chainguard-dev/challoc/pull/2), an abort sink UTF-8 split; [#3](https://github.com/chainguard-dev/challoc/pull/3), a cpuset count overflow; and [#4](https://github.com/chainguard-dev/challoc/pull/4), a per-CPU `push_batch` zero-prefix error, were merged.
- [#5](https://github.com/chainguard-dev/challoc/pull/5), an x86_64 pop that read the zero prefix outside its rseq section, and [#6](https://github.com/chainguard-dev/challoc/pull/6), its model, were approved and then closed when upstream removed the per-CPU mode.
- [#71](https://github.com/chainguard-dev/challoc/pull/71), a report sink UTF-8 split, remains a draft.

A background purge wake-up can also be lost at startup. In `purge/background.rs`, `start` publishes `PRESENT` after the thread’s first scan. A free in that window does not kick the thread, which can then sleep without a timeout. Reading the code confirms the path; it has not yet been reported.

## Coverage ceiling

About 1,250 unverified function lines are beyond Verus’s current reach: inline assembly (~330), FFI, syscalls, and function pointers (~230), `UnsafeCell`, volatile access, `transmute`, and `from_ne_bytes` (~330), `panic!` aborts (~280, because vstd’s `panic!` requires `false`), atomics and the panic handler (~100), and raw pointers from C callers (~100). These categories overlap. Keeping upstream’s exact code, identical hot-path codegen, and no new soundness risk puts coverage near a soft ceiling.

Possible relaxations and rough gains:

- Change `panic!` aborts to `abort_fmt`: ~50 functions and ~280 lines. Aborts would lose their file and line.
- Allow codegen drift for cold raw-pointer permissions: ~40 functions and ~350 lines.
- Rewrite cold iterators: ~5 functions and ~80 lines.
- Allow hot-path codegen changes after Dan’s benchmarks: ~10 functions and ~80 lines.
- Trust per-call permissions for pointers from C callers: ~15 functions and ~100 lines. This adds soundness risk and is not recommended.

Other tools could cover much of the remainder: Islaris for inline assembly, RefinedRust/Iris for `UnsafeCell` and raw pointers, Kani for function pointers, and GenMC/loom for weak memory. Connecting their results to the Verus contracts would require documented, reviewed translations.

## Open work

### Near-term proof work

- Cap `MAX_NSLOTS` at 65,535 so `flag_sort` meets `len <= u16::MAX`, then verify `group_by_slab`. Get Dan’s agreement before changing the cap: a full 65,536-slot flush currently aborts in debug builds, while release builds grouped it correctly in a 600-run harness.
- Verify the remaining cold iterator chains, including `heap::others::nodes`. Keep any rewrite’s codegen effect visible in the codegen check.
- Prove termination where possible. `ClassState::claim_with` verifies its functional contract; its retry loop’s termination remains assumed. Ten functions use `exec_allows_no_decreases_clause`. Lock waits and retries that depend on other threads need a liveness-capable model.

### Permissions and trusted boundaries

- Thread `Meta`’s `PointsToRaw` permissions through heap, node, and cache-stack allocations and through `Arenas::build`. This could replace trusted raw-pointer helpers and the block-validity axioms used by `write_bytes` and `copy_nonoverlapping`. `src/huge/table.rs`, `src/meta.rs`, and `src/heap/` show the existing permission pattern.
- Reduce the class and commit lock axioms’ reliance on trusted bridges: `owner_locked`, `state_locked`, `set_state_locked`, `dirty_locked`, and `set_dirty_locked` in `class/mod.rs`, and the bound on `SlabTable::committed_slabs`.
- Reduce the trusted construction boundary around heap creation, `Arenas::build`, in-place field access, and publication of `Shared`. `Shared::init` verifies using the token from `Regions::reserve_owning` and `Meta::owning`; `Meta::new` is test-only. Heap creation still writes headers through raw pointers, and heap accessors assume `Heap::wf`.
- Revisit `config::TABLE`, mallctl readers, print sinks, and `fmt::Display` implementations when Verus supports their function-pointer and formatting patterns. The report sink split is tracked in draft [#71](https://github.com/chainguard-dev/challoc/pull/71).

### Theorem premises

All six theorems are in the code. [THEOREMS.md](THEOREMS.md) records where each statement is narrower than the allocator’s behavior and which trusted premises it uses. The active lanes are:

- **Uniqueness:** `create_heap_for_thread` proves a new heap’s `wf`, including `tokened` and `home_ok`; `Heap::create` and `create_detached` are verified using `Meta::alloc_raw` and typed permissions. Verified `swap_hdrs` and `free_caches_in` replace stale-permission writers. `cache_ptr` is verified except for `home_ptr_at`’s address computation. `Heap::theorem_uniqueness_held`, `theorem_uniqueness`, and `theorem_uniqueness_apart` state uniqueness for held slots, one heap, and two heaps. Existing-heap accessors still assume `wf` because ghost state does not survive between calls. Trust also remains in `destroy`, whose verification shifts LTO inlining of `Meta::free`; `init_fields`, `mark_bound`, `node_set_arena`, `axiom_fresh_stack`, `axiom_list_bound`, `node_next`’s chain, and `class::axiom_free_token`. Next, model live blocks so `malloc` hands a slot token to the caller and the C free contract no longer needs `axiom_free_token`.
- **Zeroed memory:** `mallocx_zeroed` and `alloc_with` now cover the whole usable size; `rallocx_zeroed` ties the copied prefix `k` to a slot’s class size and covers the new block’s usable tail. `vspec::zero::theorem_zeroed_memory` states that every byte from `p + from` to the usable end reads zero. `Meta` gets zero tokens from `Unbacked::commit` and `fresh_zero`; slab zero picks use `axiom_fresh_slots_zero` and `axiom_discarded_slots_zero`, which require the slab’s `Fresh` or `Purged` state under the class lock and its slots’ uniqueness tokens. Still open: tie `k` to `huge_run_usable` for huge blocks through `old_size_or_keep`, use an `Unbacked` token to establish that pages past the huge mark were never written, and have `purge::Batch::flush` return zero tokens from eager `MADV_DONTNEED` to discharge the `Purged` premises.
- **Metadata integrity:** `heap::theorem_metadata_integrity` now separates user blocks from `Meta` free-list link words and the `Shared` static, using the trusted `Reserved::is_apart_from_static` fact for the static. TLS can occupy a malloc-allocated thread stack, so the theorem does not exclude it. Still open: use `WordPerm`s to place bitmap words in `SlabTable::meta_ok`, cover the partial last word as a whole word in metadata, prove slab-set placement through `SetTable::new` and `Set::in_table`, and add permission-carrying atomic wrappers for descriptor and owner-word writes.
- **PAC:** Done. `sign`, `auth`, `strip`, and `mask` require `pac_available()`, meaning every CPU the process runs on implements FEAT_PAuth. `pac::available()` establishes it; `Shared::init` proves the `Shared::wf` premise `pac_mode() ==> pac_available()`, which hot paths carry as ghost-only requirements. One trusted fact in `sys/auxv.rs` links both `HWCAP_PACA` and `HWCAP_PACG` in `getauxval(AT_HWCAP)` to system-wide PAuth availability on aarch64. Machine code is unchanged.

### Weak memory and external models

- Carry out the weak-memory checks in [WEAKMEM.md](WEAKMEM.md). Verus assumes sequential consistency and does not establish that the shipped `Relaxed`, `Acquire`, and `Release` orderings suffice. The background purge wake-up needs a reproduced check and a fix.
- Model the kernel and CPU boundary before reducing trust in assembly and syscall wrappers, including `sys/raw.rs`’s syscall functions, TLS, PAC, and CPU instructions. The kernel contracts for `mmap`, `madvise`, and `mprotect` remain premises.
- Use a liveness-capable model for lock waits and `claim_with`. The Verus theorems state partial correctness: they describe calls that return.

### Uniqueness: slot tokens

`SlabTable::new` creates one `GhostSetAuth<int>` per class table. Its elements are slot addresses; bitmap padding bits have no element. A `GhostSubset<int>` or `GhostSingleton<int>` token identifies a held slot, and two tokens from the same authority cannot contain the same address.

A separate marker authority records whether each bitmap bit is fresh or initialized. The commit lock holds initialization markers for slabs it has not handed out. `AtomicBitmap::init` exchanges a slab’s markers as it initializes its words and padding bits. A claim needs the resulting persistent markers, which establish that it is claiming initialized slots.

A slot’s token moves with its ownership:

- A clear bitmap bit’s token sits in its word’s `AtomicInvariant`, alongside the word permission.
- A successful claim moves the token to the claimer. `finish` returns tokens keyed to the pointers it writes.
- A cached slot’s token sits in `CachePerms::toks` under its stack entry. Verified refill, pop, push, flush, and rebuild paths move tokens with their slots.
- A live slot belongs to the caller. C callers carry no ghost state, so `class::axiom_free_token` currently supplies the token when they free it.
- `ClassState::release_owned` returns a batch’s distinct tokens to the bitmap words. A batch containing the same slot twice cannot supply the required tokens.

The bitmap word wrappers model each `fetch_or`, `fetch_and`, and `store` as one atomic step and preserve the caller’s machine-code ordering. Their proof specifications ignore that ordering and assume sequential consistency. `table::axiom_fresh_words` supplies the initial zero words, and `table::axiom_slot_loc` assumes one authority per class per process.

The chain still stops at heap creation, unfollowed cache writers, and the C caller boundary. `Heap::create` and `create_detached` write empty headers through raw pointers; trusted accessors then assume `Heap::wf`. `malloc` drops the popped token, and `free` gets one from `class::axiom_free_token`. A live-block ghost model and a proof of empty caches at creation would connect those ends.
