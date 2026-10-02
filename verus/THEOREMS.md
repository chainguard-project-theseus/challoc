# Whole-allocator theorems: statements and premises

Reference for the six whole-allocator theorems as the code states them:
the Verus statement, the entry points it covers, and every trusted
premise it rests on, with `file:line`. Read at `b2cad68` (branch
`verus`); line numbers are the `fn`/`axiom` line at that commit.
`verus/TODO.md` ("Whole-allocator theorems") carries the history and the
open work; this file says what the code proves today.

Marked **unconfirmed**: facts read from comments, not checked against
the proof.

## Configuration

| Axis | What is verified |
|---|---|
| Cache mode | thread caches (upstream main has no per-CPU mode) |
| Quantum | `quantum16` (default) and `quantum8`, x86_64 |
| Target | x86_64 (both quanta); aarch64 with `quantum16` only |
| PAC | on or off: `heap::pac_mode()` is uninterpreted |
| Build | `cfg(not(test))`; test-only items (`Meta::new`, `*_t`) excluded |

Because `pac_mode()` (`heap/mod.rs:153`) is uninterpreted, every proof
holds for both values.

`aarch64` + `quantum8` is not run (`verus/README.md` lists three
configurations). On x86_64 `PUSH_PAC` is never set, but the proofs do not
use that.

## Common premises

Every theorem below rests on these. Later sections list only what they
add.

**One process, one `Shared`.**
- `layout::process_regions` (`layout/regions.rs:64`) and
  `heap::pac_mode` (`heap/mod.rs:153`) are uninterpreted;
  `heap::shared::axiom_process_shared` (`heap/shared.rs:390`) defines
  them as the regions and `PUSH_PAC` bit of
  `shared_at(shared_storage())` (`heap/shared.rs:372`, `:378`).
- `api::shared_ref` (`api/mod.rs:207`, `external_body`) ensures
  `r.wf()`; `wf`'s `regions == process_regions()` conjunct
  (`heap/shared.rs:820`) ties it to the process. Its doc comment states
  what is assumed: `init_slow` (`api/mod.rs:319`) runs `init` once
  before the `READY` release store, and nothing writes a field `wf`
  reads afterwards. `shared`, `shared_if_ready` and `shared_unchecked`
  (`api/mod.rs:217`, `:232`, `:261`) inherit it.
- Two more trusted sources of `Shared::wf`: `heap::paths::shared_of`
  (`heap/paths.rs:783`, a heap's `&Shared`) and
  `purge::background::shared_arg` (`purge/background.rs:363`, the
  background thread's argument), both `external_body`.
- `Shared::init` (`heap/shared.rs:653`) proves
  `r ==> shared_at(this).wf()`, using the tracked `Build` token
  (`:404`) and trusted in-place writers: `begin` (`:447`), `put_head`
  (`:459`), `put_flags` (`:484`), `put_table` (`:505`), `tables_of`
  (`:522`), `put_huge` (`:552`), `put_tail` (`:604`) and the claim-free
  `put_guard_test`, `arena0_ptr`, `put_registries`, `put_cgroup`,
  `put_policy` (`:536`-`:592`). Arena 0 is built the same way: `put_head`,
  `put_sets`, `put_class`, `put_tail` (`arena.rs:106`-`:163`) and
  `arena_at` (`arena.rs:515`).
- `api::shared_storage_ptr` (`api/mod.rs:360`): the static's storage is
  `Shared::storage()`.

**The `free` contract.** `class::axiom_free_token`
(`class/mod.rs:3864`) gives the token of the slot a free path is handed
(theorem 5). It is called inside `Heap::free`, `free_fast`,
`free_fast_class`, `free_sized_fast` and `free_direct`, which ensure
`free_target` and `Heap::keeps`. A double free by the program makes the
proof context inconsistent at that call, so it voids theorem 3 for that
free and every theorem for later calls on the heap.

**Every heap a verified path is handed is `Heap::wf`.**
- `Heap::wf` (`heap/mod.rs:2479`): `CachePerms::wf`, `tracks`,
  `zero_ok`, `tokened`, `regions_ok()`, `list_ok`.
- Assumed by `thread::heap_from_slot` (`heap/thread.rs:191`) and
  `tcaches::heap_at` (`heap/tcaches.rs:70`), both `external_body`.
- Heap creation is trusted: `Heap::create` (`heap/mod.rs:3297`),
  `create_detached` (`:3312`), `destroy` (`:3350`).
- Writers the permissions do not follow, assumed to keep `wf`:
  `others::swap_home` (`heap/others.rs:149`), `Heap::free_caches`
  (`heap/mod.rs:2745`, verified, but it writes the untracked `caches`
  field), `node_caches_mut` (`heap/others.rs:126`). `Heap::set_arena`
  (`heap/mod.rs:3132`) ensures nothing; the next `heap_from_slot`
  assumes `wf` again.
- Trusted, but stated over the permissions: `stack_mut`
  (`heap/mod.rs:1309`, the slice over the stack's `PointsTo`s),
  `Heap::cache_ptr` (`heap/cache.rs:302`, on the `malloc` and `free`
  fast paths: the home header for `class` is owned and caches `class`),
  `cached_recently` (`heap/cache.rs:608`), the `others` list readers
  `node_link`, `node_next`, `node_arena`, `node_caches`
  (`heap/others.rs:100`, `:217`, `:238`, `:257`), and
  `Heap::axiom_list_bound` (`heap/mod.rs:2652`).
- `flush::group_by_slab` (`heap/flush.rs:618`, `external_body`): its
  result is a permutation of the batch (the flush's tokens follow it).
- `thread::fast_flags` (`heap/thread.rs:170`): the `PUSH_PAC` bit read
  through a raw pointer is `pac_mode()`.
- TLS asm: `sys::tls::get` (`sys/tls.rs:69`), `get_tp` (aarch64,
  `:118`), `set` (`:140`); they claim nothing.

**Class table.**
- `ClassTable::table` (`layout/mod.rs:1115`) and `Regions::class_entry`
  (`layout/regions.rs:1696`), `external_body`: entries are
  `class_table()`, `wf`, sized `spec_class_size`.
- `vspec::class::axiom_class_table_wf` (`vspec/class.rs:12`), the same
  fact for proofs; `adopt_page_geometry`'s `assume_specification`
  (`vspec/class.rs:30`) claims nothing.

**Kernel `mmap`.**
- `linux::try_reserve` (`layout/regions.rs:1135`): a reservation is
  non-zero and ends below `sys::pac::user_va_limit()`.
- `Reserved::is_disjoint` (`layout/regions.rs:1099`), `into_unbacked`
  (`:1115`): live reservations are apart; the metadata range is minted
  once as `Unbacked`.
- `Unbacked::commit` (`vspec/unbacked.rs:80`, `external_body`): a
  committed range becomes a `PointsToRaw` (the `mprotect` contract); the
  source of every metadata permission (`meta.rs:1566`, `:2173`).
- `sys::pac::axiom_user_va_limit` (`sys/pac.rs:147`): the limit is at
  most `2^48`.

**Locks.** `lock::axiom_acquired` (`lock.rs:177`): an acquired guard's
value satisfies `lock_inv` and `lock_frame`; the other entry is
`Lock::get_unlocked` (`lock.rs:296`). The guard's `deref`, `deref_mut`
and `drop` (`lock.rs:325`, `:337`, `:355`) are `external_body`. Each
lock type's `lock_inv`/`lock_frame` axiom says the lock protects that
type's invariant; the theorems that use one name it. Also:
`class::sets::axiom_set_tables_lock_inv` (`class/sets.rs:377`).

**Class state accessors.** `ClassState::table` (`class/mod.rs:1464`,
`*r == spec_table()`) and `table_ptr` (`class/mod.rs:2427`),
`external_body`.

**`core` and `vstd`.** The `assume_specification`s in `src/vspec/`
(76, mostly `vspec/sys.rs`, `slice.rs` and `core_int.rs`), the one in
`heap/flush.rs:1361` (`seed`), and vstd's specifications of `core`.

**Memory model.** Sequential consistency. Atomic loads return any
value (vstd); the three bitmap word writers (`vspec/word.rs:59`, `:76`,
`:92`) are one atomic step each and ignore the ordering. See
`verus/WEAKMEM.md`.

**Abort stand-ins.** Verus sees a trusted `-> !` call where the build
keeps an abort of its own, a `panic!` or `assert!` or, at
`huge/mod.rs:4711`, a call to `not_a_run` (`#[cfg(verus_only)]`), at:
`class/decay.rs:292`, `purge.rs:638`, `huge/mod.rs:2199`, `:4711`,
`:4744`, `class/table.rs:1764`, `:1882`, `heap/shared.rs:207`, `:1449`,
`huge/table.rs:774`, `layout/regions.rs:1424`. The panic handler
(`lib.rs`) aborts. An abort satisfies every postcondition.

**Termination not proved** (`exec_allows_no_decreases_clause`):
`lock.rs:80` `lock_slow`, `class/mod.rs:2672` `claim_with`,
`heap/shared.rs:1400` `release_routed`, `api/mod.rs:319` `init_slow`,
`api/cxx.rs:172` `oom`, `purge/background.rs:379` `main`,
`sys/mod.rs:533`, `:697`, `:760`, `sys/cgroup.rs:798`. The theorems are
partial correctness: they say what holds if the call returns.

## 1. Region separation

**Statement** (`heap/mod.rs:167`, `:182`):

```rust
pub open spec fn fits(p: *mut u8, size: int, align: int) -> bool {
    p@.addr != 0 ==> {
        &&& regions_ok()
        &&& process_regions().user_block(p@.addr, size)
        &&& (p@.addr as int) % align == 0
    }
}

pub proof fn theorem_region_separation(p: *mut u8, size: int, align: int)
    requires
        fits(p, size, align),
        p@.addr != 0,
        size >= 0,
    ensures
        ({
            let r = process_regions();
            &&& (exists|c: int|
                0 <= c < NCLASSES && #[trigger] r.class_base[c] <= p@.addr
                    && p@.addr + (if size < 1 { 1 } else { size })
                        <= r.class_base[c] + (1usize << r.lg_region))
                || (r.huge_base <= p@.addr
                    && p@.addr + (if size < 1 { 1 } else { size })
                        <= r.huge_base + r.huge_len)
            &&& !(r.meta_base <= p@.addr && p@.addr < r.meta_base + r.meta_len)
            &&& (p@.addr + size <= r.meta_base
                || r.meta_base + r.meta_len <= p@.addr)
        }),
```

(Line breaks added; the code has the ensures on fewer lines.)
`regions_ok()` is `process_regions().wf() && .separated()`
(`heap/mod.rs:158`); `Regions::user_block` (`layout/regions.rs:231`) is
a slot start of a class at least `size` bytes (`is_slot`, `:200`), or a
page start of the huge region with `max(size, 1)` bytes of the region
after it (`is_huge_block`, `:219`). The proof is
`Regions::lemma_user_block_separated` (`layout/regions.rs:876`).

**Entry points.** Each ensures `fits`; the theorem is a separate lemma
over that postcondition, not called by them. Rust-level functions; the
`extern "C"` wrappers (`api/libc.rs:1311`-`:1448`, `api/jemalloc.rs`
`exports`, `api/cxx.rs` `exports`) are in `verus!` with no contract and
only cast.

| Function (`file:line`) | Size, align in `fits` |
|---|---|
| `malloc` (`api/libc.rs:191`) | `size`, `QUANTUM` |
| `calloc`, `calloc_zeroed` (`api/libc.rs:306`, `:319`) | `n * m`, `QUANTUM` |
| `realloc` (`api/libc.rs:646`) | `size`, `QUANTUM`, if `r != p` |
| `reallocarray` (`api/libc.rs:681`) | `n * m`, `QUANTUM`, if `r != p` |
| `aligned_alloc` = `memalign` (`api/libc.rs:1167`) | `size`, `align` (pow2) |
| `valloc`, `pvalloc` (`api/libc.rs:1236`, `:1245`) | `size`, the page |
| `malloc_usable` (`api/libc.rs:746`) | `size`, `QUANTUM` |
| `calloc_usable` (`api/libc.rs:838`) | `n * m`, `QUANTUM` |
| `realloc_usable` (`api/libc.rs:908`) | `size`, `QUANTUM`, if `r != p` |
| `mallocx` (`api/jemalloc.rs:210`) | `size`, `1 << (flags & 0x3f)` |
| `rallocx` (`api/jemalloc.rs:236`) | as `mallocx`, if `r != p` |
| `cxx::new`, `new_nothrow` (`api/cxx.rs:214`, `:226`) | `size`, `QUANTUM` |
| `cxx::new_aligned` (`api/cxx.rs:237`) | `size`, `align` (pow2) |

A kept block (`r == p`) satisfies `fits(..) || kept(..)`
(`api/libc.rs:582`): `kept(r, size, 1)` for `realloc`, `reallocarray`
and `realloc_usable`, `kept(r, size, 1 << (flags & 0x3f))` for
`rallocx`. `kept` is a quantum-aligned slot of a large enough class, or
a huge page start with no size bound.

**Premises beyond the common ones.**
- Slab results: `CachePerms::tracks` (`heap/mod.rs:1458`), part of the
  assumed `Heap::wf`; the cache entries' `is_slot` comes from verified
  refills and frees.
- With PAC: `sys::pac::axiom_auth_sign` (`sys/pac.rs:157`, used at
  `heap/mod.rs:547`): `spec_auth(spec_sign(p, m), m) == p` below the VA
  limit; `sign`, `auth` (`sys/pac.rs:170`, `:205`) are `external_body`
  and equal `spec_sign`/`spec_auth`, one function per process (no
  `PR_PAC_RESET_KEYS`). `sign`/`auth` have no precondition that PAuth is
  present on aarch64.
- Huge results: `vspec::huge::axiom_huge_lock_inv`, `_frame`
  (`vspec/huge.rs:44`, `:52`); `Huge::alloc_run`
  (`huge/mod.rs:3685`) is verified.
- Class lock and commit lock invariants for the claim path:
  `class::axiom_class_lock_inv`, `axiom_class_lock`
  (`class/mod.rs:1288`, `:1296`), `table::axiom_table_lock_inv`,
  `_frame` (`class/table.rs:127`, `:134`), `next_slab_locked`
  (`class/table.rs:1968`), `bits_unchecked` (`class/table.rs:1258`).
  **Unconfirmed** which of these the `is_slot` ensures of
  `ClassState::claim` needs; the slot address comes from
  `claim_with`'s verified `slots_in`.

**Where the statement is weaker than the code.**
- A kept huge block has no `size` bound (`kept`, `api/libc.rs:582`):
  the huge descriptors state no `usable` bound against the region.
- `mallocx` with no alignment flag is stated at alignment 1, not the
  quantum, though with no `MALLOCX_LG_ALIGN` the code returns a class or
  run aligned to at least the quantum.
- `posix_memalign` (`api/libc.rs:1190`) states only its return code:
  `store_out` (`api/libc.rs:1182`) is `external_body` and claims
  nothing about `*out`.
- `fits` says `size` bytes; the code gives the class size or the run's
  usable bytes (theorem 2).

## 2. Fit and alignment

**Statement.** No theorem function: `fits` itself (above). Its
`user_block` conjunct puts `size` bytes in one slot of a class at least
`size` bytes, or in the huge region from a page start; its last conjunct
is `p % align == 0`. Lemmas: `lemma_slot_fits` (`heap/mod.rs:413`),
`lemma_slot_fits_quantum` (`:430`), `lemma_huge_fits` (`:461`),
`Regions::lemma_slot_aligned` (`layout/regions.rs:960`),
`layout::lemma_class_size_quantum`.

**Entry points.** Those of theorem 1, with the alignment column above.

**Premises beyond the common ones.**
- Slot alignment: class bases `MAX_SLAB`-aligned (`Regions::separated`,
  `layout/regions.rs:148`, proved by `reserve_owning`); class sizes are
  multiples of the quantum and slabs powers of two (`Class::wf`, through
  `axiom_class_table_wf`).
- Huge alignment: `Huge::alloc_run`'s placement, under the huge lock
  axioms; small alignments fall back to the page, at least 4 KiB
  (`Regions::separated`).
- `api::page_size` (`api/libc.rs:1211`) is verified from `Shared::wf`.

**Where the statement is weaker than the code.**
- The huge fit is `size` bytes of the region, not the descriptor's
  `usable` bytes (`Huge::spec_usable`, `huge/mod.rs:2400`), which the
  code hands out.
- The slab fit is `size <= class size`, true but not "the class size is
  usable": `malloc_usable_size` (`api/libc.rs:530`) ensures only
  `free_target`, not its result.

## 3. Invalid free

**Statement** (`heap/mod.rs:373`, `:406`):

```rust
pub open spec fn slot_start(p: *mut u8) -> bool {
    exists|c: int| #[trigger] process_regions().is_slot(c, p@.addr)
}

pub open spec fn free_target(p: *mut u8) -> bool {
    ||| slot_start(p)
    ||| process_regions().is_huge_block(p@.addr, 1)
}
```

Each entry point ensures `p@.addr != 0 ==> free_target(p)`: it returns
only for a slot start or a huge page start; anything else aborts.

**Entry points.**

| Function (`file:line`) | Condition on `free_target(p)` |
|---|---|
| `free` (`api/libc.rs:364`) | `p` non-null |
| `free_sized`, `free_aligned_sized` (`api/libc.rs:449`, `:459`) | non-null |
| `realloc`, `realloc_usable` (`api/libc.rs:646`, `:908`) | non-null |
| `reallocarray` (`api/libc.rs:681`) | non-null, `n * m` no overflow |
| `free_usable` (`api/libc.rs:994`) | non-null |
| `malloc_usable_size` (`api/libc.rs:530`) | non-null |
| `dallocx`, `sdallocx` (`api/jemalloc.rs:322`, `:347`) | non-null |
| `rallocx` (`api/jemalloc.rs:236`) | non-null |
| `xallocx`, `sallocx` (`api/jemalloc.rs:301`, `:309`) | always |
| `cxx::delete`, `delete_sized` (`api/cxx.rs:256`, `:267`) | non-null |
| `cxx::delete_sized_aligned` (`api/cxx.rs:278`) | non-null |

Below them, `heap::free` and `free_direct` (`heap/paths.rs:667`, `:682`),
`Heap::free` (`:196`), and the fast paths `free_fast`,
`free_fast_class`, `free_sized_fast` (`:518`, `:548`, `:577`, on their
`true`/`Some` result) ensure it; the cache pushes and
`Shared::free_direct` (`heap/shared.rs:1115`) require `is_slot` of their
class, `Shared::free_huge` (`:1312`) a page index from
`Shared::classify_free` (`:1139`).

**Premises beyond the common ones.**
- Every abort ends in a trusted `-> !` helper (`heap::paths::invalid_free`,
  `heap/paths.rs:724`, and the class and huge abort helpers).
- `Shared::classify_free` and `Regions::classify` are verified; the
  sized fast path's `slot_in_class` and `Regions::slab_slot` read the
  class table through `class_entry` (common premise).

**Where the statement is weaker than the code.**
- Liveness is not stated: a slot start whose bit is clear aborts only by
  the run-time tests (`SlabTable::is_set_unchecked`,
  `class/table.rs:1288`, trusted); the sized fast path pushes untested
  and the flush catches it; a non-live huge page start aborts in
  `Huge::free_run`. That a freed block is live is the C caller's
  contract (`axiom_free_token`, theorem 5).
- A thread's first `free` may create its heap before the abort.

## 4. Metadata integrity

**Statement** (`heap/mod.rs:234`):

```rust
pub(crate) proof fn theorem_metadata_integrity(shared: &Shared, p: *mut u8,
    size: int, align: int)
    requires
        shared.wf(),
        fits(p, size, align),
        p@.addr != 0,
        size >= 0,
    ensures
        ({
            let n = if size < 1 { 1 } else { size };
            let b = p@.addr as int;
            &&& forall|a: int, len: int|
                len >= 0 && #[trigger] shared.meta.holds(a, len)
                ==> disjoint(b, n, a, len)
            &&& forall|c: int, slab: int|
                0 <= c < NCLASSES && 0 <= slab < shared.tables[c].spec_nslabs()
                ==> disjoint(b, n, #[trigger] shared.tables[c].desc_addr(slab),
                             shared.tables[c].spec_desc_stride())
            &&& forall|c: int, g: int|
                0 <= c < NCLASSES && 0 <= g
                && g * shared.tables[c].spec_group()
                    < shared.tables[c].spec_nslabs()
                ==> disjoint(b, n, #[trigger] shared.tables[c].owner_addr(g), 2)
            &&& forall|c: int, bit: int|
                0 <= c < NCLASSES && 0 <= bit < shared.tables[c].spec_nbits()
                && shared.tables[c].spec_nbits() % 64 == 0
                ==> disjoint(b, n,
                    #[trigger] shared.tables[c].bit_word_addr(bit), 8)
            &&& forall|h: Heap, q: *mut CacheHeader|
                h.wf() && #[trigger] h.owns(q)
                ==> disjoint(b, n, q@.addr as int, 64)
            &&& forall|h: Heap, q: *mut CacheHeader, i: int|
                h.wf() && h.owns(q) && 0 <= i < h.hdr_cap(q)
                ==> disjoint(b, n, #[trigger] h.entry_addr(q, i), 8)
            &&& forall|v: crate::huge::Inner, d: *mut crate::huge::HugeDesc|
                v.wf() && #[trigger] v.has_desc(d)
                ==> disjoint(b, n, d@.addr as int, 48)
            &&& forall|v: crate::huge::Inner, i: int|
                v.wf() && 0 <= i < v.table_cap()
                ==> disjoint(b, n, #[trigger] v.table_slot_addr(i), 16)
            &&& forall|v: crate::huge::Inner, j: int|
                v.wf() && 0 <= j < v.free_map().spec_l1_words()
                ==> disjoint(b, n, #[trigger] v.free_map().l1_addr(j), 8)
            &&& forall|v: crate::huge::Inner, w: int|
                v.wf() && 0 <= w < v.free_map().spec_l0_words()
                ==> disjoint(b, n, #[trigger] v.free_map().l0_addr(w), 8)
        }),
```

(Line breaks added.) `disjoint` is `layout/regions.rs:55`. Each
conjunct is "lies in the metadata region" (`Regions::in_meta`,
`layout/regions.rs:912`) plus `lemma_user_block_apart` (`:921`).

**Entry points.** Those of theorem 1, through `fits`. The heap and huge
conjuncts quantify over every well-formed `Heap` and `huge::Inner`
value; they reach the real heaps and the huge lock's value only through
the assumed `Heap::wf` and the huge lock axiom.

**Premises beyond the common ones.**
- `vspec::ptr::axiom_ptr_add_addr` (`vspec/ptr.rs:61`): `p.add(i)` is
  `i` elements on, inside its allocation; used for stack entries
  (`heap/mod.rs:2574`), `others` nodes, huge table slots and bitmap
  words.
- `vspec::ptr::axiom_raw_valid` (`vspec/ptr.rs:98`): a `PointsToRaw`
  makes its bytes valid and in one allocation.
- `vspec::meta::axiom_meta_lock_inv`, `axiom_meta_lock_frame`
  (`vspec/meta.rs:39`, `:52`): the metadata lock protects `Inner::wf`;
  bounds and ghost identities fixed. `Meta::free` and `Meta::commit`
  (`meta.rs:2574`, `:2527`) are `external_body`: each
  assumes its caller's claim (the range was committed or allocated by
  this `Meta`).
- `Shared::put_head` and `put_table` store what they are given
  (common premise); `SlabTable::new` (`class/table.rs:325`) proves
  `meta_ok` from `Lazy::reserve`.
- Trusted class accessors compute the named addresses: `desc_unchecked`
  (`class/table.rs:1225`), `bits_unchecked` (`:1258`),
  `is_set_unchecked` (`:1288`), `owner_unchecked` (`:1316`),
  `owner_word` (`:1986`).
- `TwoLevelBitmap::from_raw`, `from_raw_chunked` (`bitmap.rs:2584`,
  `:2616`): their `place` ensures.
- `heap::axiom_fresh_stack` (`heap/mod.rs:1362`): a stack `Meta::alloc`
  just returned is fresh; `Heap::lemma_caches_in_meta`
  (`heap/mod.rs:2553`) needs `cell_in_meta`, which `ensure_cache`
  (`heap/cache.rs:758`) proves for each stack.
- Huge lock axioms (`vspec/huge.rs:44`, `:52`) for `huge::Inner`.
- `huge::table::axiom_zero_entry` (`huge/table.rs:174`): all-zero bytes
  are an empty descriptor table entry (`Table::storage`).
- User-block writes: `vspec::addr::axiom_fresh_block`,
  `axiom_moved_block`, `axiom_slot_writable`, `axiom_huge_writable`
  (`vspec/addr.rs:104`, `:121`, `:140`, `:153`) grant validity for any
  length; the theorem covers where a block is, not that every write to
  it stays inside it.

**Not covered.**
- The `Shared` static and TLS: outside the metadata region, not in the
  statement.
- Writes through `&` atomics (descriptors, owner words, `AtomicBitmap`
  words) are located, not tied to permissions. Bitmap words now have
  `WordPerm`s inside `bitmap::WordPred` (`bitmap.rs:687`), but the
  theorem does not use them.
- Arenas' slab-set tables (`SetTable::new`, `Set::in_table`,
  `class/sets.rs`): no placement stated.
- A class bitmap whose bit count is not a multiple of 64: the last word
  is excluded (`spec_nbits() % 64 == 0`).
- `Shared::zero_huge` (`heap/shared.rs:1295`) clears `usable` bytes that
  no huge invariant bounds by the run (`axiom_huge_writable`).
- `Meta`'s own link words and reused-object fill are a fact of its lock
  value (`meta::Inner::lemma_links_in_region`), not a conjunct.

## 5. Uniqueness

**Statement** (`heap/mod.rs:1729`, `:1772`; private to `heap`, in
`impl CachePerms`):

```rust
proof fn theorem_uniqueness_cached(tracked perms: &mut CachePerms,
    p1: *mut CacheHeader, i1: int, p2: *mut CacheHeader, i2: int)
    requires
        old(perms).wf(),
        old(perms).tracks(),
        old(perms).tokened(),
        old(perms).hdrs.contains_key(p1),
        old(perms).hdrs.contains_key(p2),
        0 <= i1 < old(perms).hdr(p1).len,
        0 <= i2 < old(perms).hdr(p2).len,
        p1 != p2 || i1 != i2,
        old(perms).cls[p1] == old(perms).cls[p2],
    ensures
        final(perms).hdrs == old(perms).hdrs,
        // ... slots, cls, owner, zero, handed unchanged ...
        final(perms).tokened(),
        plain(old(perms).slots[old(perms).ent(p1, i1)].value(),
              old(perms).hdr(p1).slots)
            != plain(old(perms).slots[old(perms).ent(p2, i2)].value(),
                     old(perms).hdr(p2).slots),

proof fn theorem_uniqueness_held(tracked perms: &CachePerms,
    tracked held: &mut GhostSubset<int>, p: *mut CacheHeader, i: int)
    requires
        perms.tokened(),
        perms.hdrs.contains_key(p),
        0 <= i < perms.hdr(p).len,
        old(held).id() == slot_loc(perms.cls[p]),
    ensures
        final(held).id() == old(held).id(),
        final(held)@ == old(held)@,
        !old(held)@.contains(plain(perms.slots[perms.ent(p, i)].value(),
                                   perms.hdr(p).slots) as int),
```

(Abridged where marked; line breaks added.) Two cached entries of one
class name different slots; a slot whose token any other holder has
(a bitmap word's invariant, another heap, a claim in flight) is not
cached. The rest of the theorem is the token protocol every verified
writer keeps:

- Authority per class: `table::slot_loc` (`class/table.rs:635`),
  minted by `SlabTable::new`.
- Free slots: tokens in each word's `AtomicInvariant`,
  `bitmap::WordPred` (`bitmap.rs:687`): the word's `WordPerm` and the
  tokens of exactly its clear bits.
- Claimed: `AtomicBitmap::claim_word`, `claim_from` (`bitmap.rs:1484`,
  `:1645`) move out the tokens of the bits their `fetch_or` won;
  `ClassState::claim_with` (`class/mod.rs:2672`) returns them by index.
- Cached: `CachePerms::tokened` (`heap/mod.rs:1483`), a conjunct of
  `Heap::wf` and `Heap::keeps`.
- Released: `ClassState::release_owned` (`class/mod.rs:4423`) takes a
  `GhostSubset` of the batch; `AtomicBitmap::release`
  (`bitmap.rs:1700`) hands each word's back, so its double-free abort is
  unreachable under the contract.

**Entry points.** None states uniqueness. `malloc`'s pop
(`Heap::alloc_fast`, `heap/paths.rs:54`) drops the popped token; every
free path takes one from `axiom_free_token`.

**Premises beyond the common ones.**
- `class::axiom_free_token` (`class/mod.rs:3864`): the `free` contract,
  one token per free of a live slot. Called at `heap/paths.rs:207`
  (`Heap::free`), `:533` (`free_fast`), `:563` (`free_fast_class`),
  `:593` (`free_sized_fast`), `:696` (`free_direct`). See "The `free`
  contract" under Common premises.
- `table::axiom_slot_loc` (`class/table.rs:644`, one call `:446`):
  `SlabTable::new` runs once per class per process.
- `table::axiom_fresh_words` (`class/table.rs:655`, one call `:478`):
  a just-reserved table's words read zero, one permission each.
- `vspec::word::word_addr` (`vspec/word.rs:38`) is a word's identity;
  `fetch_or`, `store`, `fetch_and` (`:59`, `:76`, `:92`) are single
  atomic steps whose result is the permission's value (ordering
  ignored).
- `SlabTable::bits_unchecked` (`class/table.rs:1258`): the view's words
  are the class bitmap's words for a slab below the watermark.
- Class and commit lock axioms (`class/mod.rs:1288`, `:1296`,
  `class/table.rs:127`, `:134`), the class lock bridges
  `owner_locked`, `state_locked`, `set_state_locked`, `dirty_locked`,
  `set_dirty_locked` (`class/mod.rs:1332`-`:1433`), `next_slab_locked`
  (`class/table.rs:1968`) and `take_checks` (`class/mod.rs:2240`).
- Heap creation and the unfollowed cache writers (common premise): a new
  heap's caches are taken to be empty and `tokened`.

**Where the statement is weaker than the code.**
- No ghost model of live blocks: the program's blocks hold no tokens, so
  "no slot is handed out twice" is proved only up to the hand-out and
  from the free on.
- The theorems are lemmas inside `heap/mod.rs`; no entry point or global
  invariant across heaps calls them.

## 6. Zeroed memory

**Statement.** The `Zeroed` token (`vspec/zero.rs`): `covers(p, n)` is
`addr() == p@.addr && n <= len()`. Postconditions:

| Function (`file:line`) | Token on a non-null result |
|---|---|
| `calloc_zeroed` (`api/libc.rs:319`) | `covers(r.0, n * m)` |
| `calloc_usable_zeroed` (`api/libc.rs:853`) | `n * m`, if `<= MEDIUM_MAX` |
| `mallocx_zeroed` (`api/jemalloc.rs:220`) | `size`, with `MALLOCX_ZERO` |
| `rallocx_zeroed` (`api/jemalloc.rs:251`) | `size - k` bytes from `r + k` |

`rallocx_zeroed`'s clause, for a moved or new block with
`MALLOCX_ZERO`:

```rust
r.0@.addr != 0 && r.0 != p && (flags as u32) & MALLOCX_ZERO != 0 ==> {
    &&& 0 <= r.1@ <= size
    &&& r.2@.addr() == r.0@.addr + r.1@
    &&& r.2@.len() >= size - r.1@
},
```

`calloc` and `mallocx` are `.0` of the `_zeroed` forms; the exported
symbols carry no token.

**Token sources.**
- `vspec::zero::write_zero` (`vspec/zero.rs:90`, `external_body`):
  `write_bytes(p, 0, n)` makes `n` zero bytes.
- `vspec::sys::axiom_fresh_anon_zero` (`vspec/sys.rs:167`): never-written
  anonymous memory reads zero. Call sites: `ClassState::take`
  (`class/mod.rs:2204`, a `Fresh` slab), `huge::zero_run`
  (`huge/mod.rs:1513`, `:1523`, pages at or past the mark),
  `alloc_zeroed` (`meta.rs:2347`), `reserve_committed_raw`
  (`meta.rs:2423`).
- `vspec::sys::axiom_discarded_zero` (`vspec/sys.rs:191`):
  `MADV_DONTNEED` memory reads zero. Call sites: `ClassState::take`
  (`class/mod.rs:2206`, a `Purged` slab), `huge::zero_run`
  (`huge/mod.rs:1517`, `:1522`).

**Premises beyond the common ones.**
- Each kernel axiom's protocol premise, the caller's: a `Fresh` slab
  was never handed out; pages past the huge mark were never covered by a
  run; a `Purged` slab or extent was discarded in the lock hold that
  set the state (`class/decay.rs:608` sets it whatever the advice) and
  has no allocated slot or run since; and `take` makes a zero claim of a
  `Purged` one only while `purge::purged_is_zero` (`purge.rs:203`, a
  `Relaxed` load of `LAZY_EVER`) reads true under the lock
  (`class/mod.rs:2180`), that is, no batch has used `MADV_FREE`. The
  axioms take any range; the call sites check the recorded state, not
  the history.
- Cache path: `CachePerms::zero_ok` (`heap/zero.rs:49`), part of the
  assumed `Heap::wf`; `Heap::take_handed` (`heap/zero.rs:429`).
- Class and huge lock axioms, for the state `take` reads.
- `axiom_fresh_block`, `axiom_moved_block` (`vspec/addr.rs:104`, `:121`)
  for `write_zero`'s and the copy's validity.
- The only writes verified code makes to a token's bytes are zero fills
  and `rallocx`'s copy into the new block's prefix, which it splits off
  (`api/jemalloc.rs:288`). The token is not a permission; nothing ties
  it to other writers. Stated in `vspec/zero.rs`, not proved.

**Where the statement is weaker than the code.**
- `mallocx_zeroed` states `size` bytes; `alloc_with`
  (`api/jemalloc.rs:158`) zeroes `usable` bytes (the class size or the
  run's usable size, `api/jemalloc.rs:199`) when the block is not
  known zero, and that token covers them; on the known-zero path the
  token comes from `mallocx::alloc` with no length stated as `usable`.
  Its ensures says `size`.
- `rallocx_zeroed`'s `k` is any value in `0..=size`; the code copies
  `min(old, size)` with `old` from `old_size_or_keep`, whose ensures does
  not state `old`.
- `calloc_usable_zeroed` gives no token for huge requests: the huge
  `memset` length (`Huge::usable_size`) is not tied to the run.
