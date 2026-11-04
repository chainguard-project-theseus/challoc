# Seam audit: memory (raw pointer, UnsafeCell/volatile/transmute, atomics), the audited seams still trusted at `26f535a9`

*An adversarial audit of the Verus seams in the categories raw pointer
(internal), UnsafeCell/volatile/transmute and atomics primitive, together
with the core-library pointer and atomic specifications they rest on,
made during [#138](https://github.com/wolfi-dev/challoc/pull/138). There it covered 93 seams: 76 sound, 10 fixed,
1 weakened, 5 removed, and 1 unsound-open that was fixed after the
audit. It machine-checked two derivations of `false`, through
`from_raw` and `Lock::get_unlocked`, whose contracts it fixed.*

*This file lists the 37 of those seams that the ledger at `26f535a9`
still lists. The others are verified at `26f535a9` or no longer exist,
so their rows are left out; among them are `Heap::destroy`,
`LockGuard::deref_mut`, `SlabDesc::at`, `Set::ensure`, `Arenas::init`,
`save_sigmask`, `from_raw` and `Lock::get_unlocked`, all verified there.
The audit's `write_zero` was `vspec/zero.rs`'s, which no longer exists;
`huge::write_zero`, a seam of this scope at `26f535a9`, was not audited,
and no other unaudited seam of the scope is listed either. Each row names
the seam by its item and its ledger file, `verus/ledger/src/<file>.md`,
which gives the seam's contract, category, tool and status.*

The verdicts are the audit's, from #138; none was re-audited at
`26f535a9`. The ledger file and status columns are `26f535a9`'s ledger,
checked by script against `verus/ledger/src/`. A reason may describe the
contract as it stands at `26f535a9`, but its verdict was not judged
again.

For each seam the audit asks four questions. Can its contract prove
`false` when instantiated at equal pointers, by a second call, or at
empty and maximal sizes? Does the `ensures` hold of the body for every
input that meets the `requires`, at both quanta and on both aarch64 and
x86_64? Is the seam still needed? Is it stronger than its callers need?

Verdicts: **sound** means no derivation of `false` was found and the
contract holds of the body. Where that truth rests on an unchecked
`# Safety` duty, the reason says which one. **fixed** means the audit
found the contract false or unsound and corrected it; the reason gives
the corrected contract. Remaining seam work is tracked in
[wolfi-dev/challoc#397](https://github.com/wolfi-dev/challoc/issues/397).

Totals for the 37 seams listed: 33 sound, 4 fixed.

| seam | ledger file | status | verdict | reason |
|---|---|---|---|---|
| `shared_ptr` | `api/mod.rs` | trusted (partly checked) | sound | Ensures `r == Shared::storage()`; the body returns the static's address, which a Kani harness compares with `shared_storage_ptr`'s. |
| `shared_storage_ptr` | `api/mod.rs` | discharged | sound | `Shared::storage()` is one uninterpreted constant; the body returns the static's address. |
| `axiom_arena_assemble` | `arena.rs` | trusted | fixed | Consumes the field permissions and the padding (`pad_covers`), so the assembled `PointsTo<Arena>` covers exactly the bytes it is given (see details). |
| `Heap::home_ptr_at` | `heap/cache.rs` | trusted (partly checked) | sound | Ensures the home header's address, `heap + 128 + 64 * class`. True while the heap's ghost record names the headers at its own address (`Heap::home_at`); the Kani harness checks the arithmetic, not that premise. |
| `addr_mix` | `heap/flush.rs` | discharged | sound | No `ensures`. |
| `Heap::arena_volatile` | `heap/mod.rs` | discharged | sound | No `ensures`. |
| `axiom_shared_built` | `heap/shared.rs` | trusted | sound | Consumes its permissions and returns none, so padding does not matter. A second call needs a second set of field permissions, which only `Shared::begin` mints, once per process. |
| `axiom_shared_regions` | `heap/shared.rs` | trusted | sound | The permission is frozen behind `&'static`; two calls give the same value. |
| `heap_at` | `heap/tcaches.rs` | trusted | sound | Ensures `wf` and `home_at(p)` for a heap that `Heap::create` or `Heap::create_detached` returned and nothing destroyed (the `# Safety` duty). The result borrows the `HeapUse` token mutably, so two results for one pointer cannot coexist. |
| `axiom_zero_entry` | `huge/table.rs` | discharged | sound | Consumes the uninitialised permission and a 16-byte zero token; all-zero bytes are an empty entry (null `desc`). |
| `ClassTable` | `layout/mod.rs` | trusted | sound | Opaque type with one private constructor. |
| `ClassTable::new` | `layout/mod.rs` | discharged | sound | No contract. |
| `ClassTable::table` | `layout/mod.rs` | trusted (partly checked) | sound | One spec constant over memory that `adopt_page_geometry` rewrites once at init. No verified read of the geometry precedes the rewrite, and `wf` and the sizes hold before and after. |
| `LockGuard::drop` | `lock.rs` | trusted (partly checked) | sound | No `ensures`. Its futex wake is a syscall under no `may_syscall`; no fast path drops a guard. |
| `Explicit::drop` | `purge.rs` | discharged | sound | No `ensures`. |
| `shared_arg` | `purge/background.rs` | trusted | sound | Ensures `wf` for any argument. True for its one caller: the thread `spawn` starts with the process's `Shared`, a C-program contract on `pthread_create`. |
| `read_global` | `rseq.rs` | trusted (partly checked) | sound | No `ensures`; instantiated at `isize` and `u32`, which have no type invariant. |
| `read_cpu_id` | `rseq.rs` | trusted (partly checked) | sound | No `ensures`; the caller checks the area size, which the `requires` names. |
| `addr_to_ptr` | `vspec/addr.rs` | trusted (partly checked) | sound | `addr_ptr` and `cast_provenance` are uninterpreted, one value per address. The `ensures` pins the address, that value and the shared cast provenance. |
| `core::ptr::write_bytes::<T>` | `vspec/ptr.rs` | discharged | sound | `core`'s safety contract, for a `T` that is not zero-sized. Its effect is unstated (see the model notes). |
| `core::ptr::copy_nonoverlapping::<T>` | `vspec/addr.rs` | discharged | sound | `core`'s safety contract, overlap included. |
| `<*mut T>::is_null` | `vspec/addr.rs` | discharged | sound | The data address is zero, wide pointers included. |
| `<*const T>::is_null` | `vspec/addr.rs` | discharged | sound | As `<*mut T>::is_null`. |
| `Atomic::<*mut T>::load` | `vspec/arena.rs` | discharged | fixed | Requires away `Release` and `AcqRel`, the orderings for which `core` panics. |
| `Atomic::<*mut T>::store` | `vspec/arena.rs` | discharged | sound | Requires away `Acquire` and `AcqRel`. |
| `Atomic::<*mut T>::new` | `vspec/arena.rs` | discharged | sound | No contract. |
| `Atomic::<*mut T>::swap` | `vspec/arena.rs` | discharged | sound | Every ordering is valid; any result. |
| `<*const T>::cast::<U>` | `vspec/config.rs` | discharged | sound | Nothing is claimed about the result. |
| `Atomic::<u32>::as_ptr` | `vspec/misc.rs` | discharged | sound | No claim. |
| `<*mut T>::add` | `vspec/ptr.rs` | trusted (partly checked) | sound | `core`'s contract; the result is abstract (`ptr_add`). |
| `core::ptr::from_ref` | `vspec/sys.rs` | discharged | sound | No claim. |
| `core::ptr::from_mut` | `vspec/sys.rs` | discharged | sound | No claim. |
| `WordPerm` | `vspec/word.rs` | trusted (partly checked) | sound | Unique per word because `axiom_fresh_words` (`class/table.rs`) mints one set per table, against the table's one-shot token. |
| `fetch_or` | `vspec/word.rs` | trusted (partly checked) | sound | Every ordering is valid; the value follows the permission. |
| `store` | `vspec/word.rs` | trusted (partly checked) | fixed | Requires away `Acquire` and `AcqRel`, the orderings for which `core` panics. |
| `load` | `vspec/word.rs` | trusted (partly checked) | fixed | Requires away `Release` and `AcqRel`. |
| `fetch_and` | `vspec/word.rs` | trusted (partly checked) | sound | As `fetch_or`. |

## Details

### `axiom_arena_assemble`: padding

`index: u16` is followed by `decay_ns: AtomicU64`, so `axiom_arena_layout`
(with `AtomicU64`'s alignment 8) forces a byte that no field covers. An
axiom that returned a permission to all `size_of::<Arena>()` bytes while
consuming only the field permissions would let a caller that kept that
padding byte's permission hold two permissions for one byte, which
`is_disjoint` turns into `false`.

The contract rules this out. `take_field` ensures the exact domain of
what it leaves; `Arena::split` returns that leftover (the padding) with
`pad_covers`, which says every byte of the `Arena` is the leftover's or a
field's; and `axiom_arena_assemble` requires and consumes it. The
assembled permission is exactly the bytes it is given.

Of its `ensures`, its one caller (`Arena::init`) uses `ptr`, `is_init`
and `index`; the other field equalities are true and unused.

### Atomic orderings

`core` panics for a load with `Release` or `AcqRel` and for a store with
`Acquire` or `AcqRel`. `AtomicPtr::load` and `store`, and
`vspec::word::load` and `store`, each require their panicking orderings
away, so verified code that passes one fails to verify instead of
keeping a "no panic" claim it does not have. Every caller passes a
constant ordering that meets the requirement. vstd's own `core` atomic
specifications (`std_specs/atomic.rs`) leave the same orderings allowed
for the integer atomics and for `compare_exchange`'s failure ordering;
those are vstd's, not challoc seams.

## Model notes

These verdicts are sound; each rests on an assumption worth knowing.

- **`valid_writes` and `in_alloc` are timeless.** `rw_len` and
  `alloc_len` (`vspec/ptr.rs`) have no memory state, so a bound
  established from a permission (`axiom_raw_readable`) or by an
  allocator's trusted function outlives the permission. And
  `write_bytes` and `copy_nonoverlapping` state no effect, so Verus
  assumes they change no memory a ghost permission covers. Together,
  verified code could write through a bound it kept after giving the
  bytes' permission away, or zero bytes under a `PointsTo` it holds, and
  keep believing the old value. No derivation of `false` was found (no
  axiom combines a `Zeroed` token with an initialised permission), and
  every call uses its bound at once, on memory no permission covers, as
  `vspec/ptr.rs` documents.
- **Truth under `# Safety`.** `heap_at` and `shared_arg` claim `wf` of
  their result for the pointers their callers pass by the `# Safety`
  contract; nothing checks that duty. Neither relates two results, so
  neither proves `false`.
- **`class_table()` across `adopt_page_geometry`.** One constant names
  the table before and after the init-time rewrite (`ClassTable`,
  `ClassTable::new`, `ClassTable::table`). No verified code reads the
  geometry fields before the rewrite, and the rewrite keeps every entry
  `wf` with its size, so the constant is the table's value at every
  verified read.
- **Sequential consistency.** `vspec::word::load` returns the
  permission's value for a `Relaxed` load; the proofs assume sequential
  consistency (`vspec/word.rs`, `verus/WEAKMEM.md`).
