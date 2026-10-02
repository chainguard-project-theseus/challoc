# Whole-allocator theorems: statements and premises

Reference for the seven whole-allocator theorems as the code states them:
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
`:92`) are one atomic step each and ignore the ordering, as is the one
word load that returns its value (`vspec::word::load`, theorem 7). See
`verus/WEAKMEM.md`.

**Abort stand-in.** One site remains where Verus sees a trusted `-> !`
call in place of the build's abort (`#[cfg(verus_only)]`):
`huge/mod.rs:4786`, where the build calls `not_a_run(self.addr(page))`
and the address of an unvalidated page cannot be bounded. Every other
abort helper verifies in place and ends in `sys::abort_fmt`, which
writes the message and raises `SIGABRT` (`sys/abort.rs`). The panic
handler (`lib.rs`) aborts. An abort satisfies every postcondition.

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
  (`class/table.rs:1968`); `bits_unchecked` is verified, through the
  word permissions of `table::axiom_init_words` (`class/table.rs:996`).
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
  usable": `fits` does not say it. The size-returning entry points do
  (`vspec::usable::usable_is` and `usable_of`, the U column of the
  interface contract below).

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
only for a slot start or a huge page start; anything else aborts. Since
theorem 7, `is_slot` excludes guard slabs, so a pointer into one aborts
too (`theorem_guard_free`).

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
- Every abort ends in `sys::abort_fmt`, which never returns: the abort
  helpers (`heap::paths::invalid_free`, `heap/paths.rs:729`, and the
  class and huge abort helpers) verify and call it.
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
  `meta_ok` from `Lazy::reserve_raw`.
- The class table accessors `desc_unchecked`, `bits_unchecked`,
  `is_set_unchecked`, `owner_word` and `owner_at` read through
  permissions carved from the three reservations
  (`SlabTable::perms_ok`): `sets::axiom_table_bytes`
  (`class/sets.rs:166`, the table's bytes; an eager table's copy also
  stays in `meta::Inner::handed`, never handed out),
  `table::axiom_init_descs`, `_owners`, `_words` (`class/table.rs:974`,
  `:984`, `:996`, the values are initialised), `words_ref` (`:1013`, a
  slice over consecutive word permissions) and `watermark` (`:1580`, at
  most the slab count). Premise: only committed pages are read (the
  accessors' `# Safety`). `owner_unchecked` (`:1835`) is trusted for
  `slab` below the slab count, which its one outside caller tests.
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
- `SlabTable::bits_unchecked` is verified: the view's words are the
  class bitmap's through the permissions of `table::axiom_init_words`
  (`class/table.rs:996`), whose `word_addr` ties them to the word
  invariants.
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
| `calloc_usable_zeroed` (`api/libc.rs:853`) | `n * m`, and `covers_usable(.., 0, n * m)` |
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
- `calloc_usable_zeroed`'s huge `memset` length (`Huge::usable_size`)
  is tied to the run only through `vspec::huge::axiom_owned_run`
  (`r == huge_usable(size)`), from which `lemma_covers_huge` gives
  `covers_usable` for its whole usable size (row 13 of
  the interface contract).

## 7. Guard pages

Read at this section's commit (the `file:line`s are the `fn`/`axiom`
line there).

**Statement** (`heap/mod.rs:212`, `:243`; `layout/regions.rs:110`,
`:284`):

```rust
pub open spec fn guard_slab(c: int, slab: int) -> bool {
    guard_of(guard_geom(c).0, guard_geom(c).1, slab as usize)
}

pub open spec fn is_slot(self, c: int, p: usize) -> bool {
    &&& self.is_slot_addr(c, p)
    &&& !guard_slab(c, self.slab_index(c, p))
}

pub proof fn theorem_guard_pages(p: *mut u8, size: int, align: int, c: int, slab: int)
    requires
        fits(p, size, align),
        p@.addr != 0,
        size >= 0,
        0 <= c < NCLASSES,
        0 <= slab < process_regions().spec_nslabs(c),
        guard_slab(c, slab),
    ensures
        disjoint(p@.addr as int, if size < 1 { 1 } else { size },
            process_regions().slab_span(c, slab).0,
            process_regions().slab_span(c, slab).1),

pub proof fn theorem_guard_free(p: *mut u8, c: int, slab: int)
    requires
        regions_ok(),
        free_target(p),
        0 <= c < NCLASSES,
        0 <= slab < process_regions().spec_nslabs(c),
        guard_slab(c, slab),
    ensures
        disjoint(p@.addr as int, 1,
            process_regions().slab_span(c, slab).0,
            process_regions().slab_span(c, slab).1),
```

(Line breaks added.) No byte of a non-null block an allocation returns
lies in a guard slab of any class region, and a free that returns acted
on no address in one. A guard slab is what `SlabTable::is_guard_slab`
(`class/table.rs:1529`) names: every `guard_interval`-th page group,
counted from 1. `commit_slab` (`class/table.rs:2232`) puts the guard on
it: `MADV_GUARD_INSTALL` on the page group's first page, or the whole
slab or page group left `PROT_NONE` when the advice fails, or (under
`thp:always` with PMD chunks) no marker, the slab left unused. Either
way the guard page is inside the guard slabs, so the theorem covers it.
With guards off the mask is all ones and no slab is a guard slab.

`guard_of` (`layout/regions.rs:96`) is `is_guard_slab`'s formula;
`guard_geom(c)` (`:105`) is the process's class `c` table's
`(lg_slabs_per_page, guard_mask)`. `is_slot_addr` (`:266`) is the old
`is_slot` (slot `slot < nslots` of slab `slab < nslabs`, guard slabs
included); `slab_index` and `guard_of` are opaque, revealed only in
`Regions::lemma_slab_index` and the two guard tests. `is_slot` is in
`user_block`, so in `fits` (theorem 1), and in `slot_start`, so in
`free_target` (theorem 3): every row of the interface contract whose R
or B cell cites them now carries the guard exclusion, with no change to
any entry point's `ensures`. The proofs are
`Regions::lemma_user_block_not_guard` (`layout/regions.rs:1080`): a slot
lies inside its own slab, which `is_slot` puts outside the guard slabs,
and inside its class region, apart from every other class region and
from the huge region (`separated`); a huge block lies in the huge
region.

**Where `is_slot` is established.**
- Claims: `lemma_real_is_slot` (`class/mod.rs:3760`) takes the bit's
  slab from `SlotToks::entry` (`:3419`), which now records that the slab
  is not a guard slab. `finish` gets it from `picks_marks` (`:3864`),
  which `take` ensures from the class lock's `ready` (`:855`:
  `!spec_is_guard`). The zero picks' `slot_ptr`
  (`class/table.rs`) states `is_slot` under the same premise.
- Unsized frees: `SlabTable::is_set_unchecked` (`class/table.rs:1826`)
  ensures `r ==> !guard_slab(class, slab)`. It now reads the word through
  `vspec::word::load` (`vspec/word.rs:96`) inside the word's invariant,
  where `bitmap::guards_ok` (`bitmap.rs:697`) holds: a guard slab's bits
  keep their fresh markers, so `marks_ok` makes them clear
  (`lemma_guard_clear`, `:722`). `check_live` and `check_located`
  (`class/table.rs:2169`, `:2135`) re-export it; `Shared::classify_free`,
  `set_slot` (so `checked_slot`, the `free` fast path) and the three
  `api/libc.rs` `check_live` callers (`usable_size`,
  `old_size_or_keep`, `realloc_old`) turn it into `is_slot`.
- Sized frees: `Shared::sized_slot` (`heap/shared.rs:1806`) through
  `GuardTest::is_guard`, which now ensures its formula;
  `GuardTest::lemma_is_guard` (`:175`, `bit_vector`) proves the folded
  test (`(slab + spp) & (mask << lg) == 0`) equal to `guard_of` for
  every slab of a region (`slab < 2^26`, `lg <= 26`), and `Shared::wf`
  (`:1416`) now states that `guard_test[c]` tests the class's geometry
  (`GuardTest::of`, `:204`, which `init` stores).
- Cached slots: the cache invariant already holds `is_slot` of every
  entry, so pops, refills, flushes and rebuilds carry the guard
  exclusion unchanged.

`bitmap::guards_ok` is kept by every word writer: `fresh_word_invs`
starts every bit fresh, claims and releases leave the markers alone,
and `AtomicBitmap::init` (`bitmap.rs:1233`) now requires a slab that is
not a guard slab (`lemma_guards_move`, `:922`); `take_group` calls it
only past the `is_guard_slab` test.

**Huge runs.** Each run's guard follows its usable pages inside its
extent, and live extents are pairwise apart (`huge::Inner::wf`).
`Inner::theorem_run_guards` (`huge/mod.rs:941`) states the consequence
for every value the huge lock admits: the usable pages of a live run
share no page with the guard pages of any live run, its own included.

**Premises beyond the common ones.**
- `table::axiom_slot_loc` (`class/table.rs:902`) now also fixes
  `guard_geom(c)` to the table's geometry: the same one-table-per-class
  assumption that names the slot tokens' authority, no new kind of
  premise.
- `heap::shared::axiom_shared_built` (`heap/shared.rs:664`) now also
  states the stored guard tests, as it states the tables.
- `vspec::word::load` (`external_body`, one atomic load of the word, as
  the three writers are one atomic step each); its result is the
  permission's value under sequential consistency.
- Class lock axioms for `ready`, and `api::shared_ref`'s `Shared::wf`
  for the guard tests and the table geometry the free paths read.

**Where the statement is weaker than the code.**
- Huge runs are covered only at the lock invariant: `alloc_run` does not
  tie its result to a descriptor, so no entry point states that a
  returned huge block avoids other runs' guard pages. In fallback mode
  (no `MADV_GUARD_INSTALL`) a reused extent may hold a stale `PROT_NONE`
  guard that `take` re-commits; that commit is not modelled.
- `theorem_guard_free` says that a pointer into a guard slab aborts,
  not which message it prints (`guard_free` for a handed-out guard slab
  through `check_live`'s `not_live`, `never_committed` past the
  watermark, `invalid_free` for a mid-slot address).
- The kernel's guard semantics (a fault on access) are not modelled: the
  theorem is about where blocks lie, not about what the guard does.

## Interface contract

The seven theorems above checked against challoc's exported C ABI: one
row per exported symbol (`je_` twins and C++ array forms share their
row), one column per interface property. Read from the `ensures` of
each entry point's Rust function at `ccbf1dd`; the `file:line`s in
this section are the `fn` line at that commit (the sections above are
at `b2cad68`). The U column, row 13's Z and row 22's F were revised
later, by the usable-size work (gap 3 below); items it added are cited
by name. All the trusted premises listed above apply to every cell. Every `extern "C"` wrapper is in `verus!` with no contract of its
own and only casts, except `malloc_stats_print`. So the Rust function's
`ensures` is the symbol's contract, and a verified body cannot panic or
overflow. Where that function is `.0` of a `_zeroed` twin (`calloc`,
`challoc_calloc_usable`, `mallocx`), the Z cell rests on the twin's
`ensures`: the zero token is ghost, so the exported function returns
the same block. Likewise the U cells of rows 12 and 14 rest on the
`_stored` twins' `ensures` (`malloc_usable_stored`,
`realloc_usable_stored`) and row 13's on `calloc_usable_zeroed`'s third
result: `malloc_usable` and `realloc_usable` themselves state nothing
about `*usable`.

Properties:

- **R**: the result fits and is aligned (`heap::fits`, `heap/mod.rs:167`);
  since theorem 7 a slab result is never in a guard slab.
- **D**: the result is disjoint from every other live block.
- **Z**: the result is zeroed where the API promises it.
- **C**: contents are preserved (realloc family).
- **F**: failure semantics: NULL, `errno`, return code, the old block
  intact after a failed realloc, overflow in `calloc`/`reallocarray`.
- **B**: bad input (invalid or double free, bad alignment) aborts or is
  a documented no-op. It never corrupts silently. Since theorem 7 a
  free target is never in a guard slab, so a free into one aborts.
- **U**: size consistency: `nallocx` equals `mallocx`'s usable size;
  `sallocx` and `malloc_usable_size` give the block's real usable size.

Codes: `P` proved, `p` partial, `-` missing, `.` not applicable.

| #  | Entry point                                 | R | D | Z | C | F | B | U |
|----|---------------------------------------------|---|---|---|---|---|---|---|
| 1  | `malloc`                                    | P | p | . | . | p | . | . |
| 2  | `calloc`                                    | P | p | P | . | p | . | . |
| 3  | `realloc`                                   | p | p | . | - | p | p | . |
| 4  | `reallocarray`                              | p | p | . | - | p | p | . |
| 5  | `aligned_alloc`, `memalign`                 | P | p | . | . | p | P | . |
| 6  | `posix_memalign`                            | p | p | . | . | p | P | . |
| 7  | `valloc`                                    | P | p | . | . | p | . | . |
| 8  | `pvalloc`                                   | p | p | . | . | p | . | . |
| 9  | `malloc_usable_size`                        | . | . | . | . | P | p | p |
| 10 | `free`                                      | . | . | . | . | - | p | . |
| 11 | `free_sized`, `free_aligned_sized`          | . | . | . | . | - | p | . |
| 12 | `challoc_malloc_usable`                     | P | p | . | . | p | . | P |
| 13 | `challoc_calloc_usable`                     | P | p | P | . | p | . | P |
| 14 | `challoc_realloc_usable`                    | p | p | . | - | p | p | P |
| 15 | `challoc_free_usable`                       | . | . | . | . | P | p | p |
| 16 | `mallocx`                                   | P | p | P | . | p | - | . |
| 17 | `rallocx`                                   | p | p | p | - | p | p | . |
| 18 | `xallocx`                                   | . | . | . | . | . | p | p |
| 19 | `sallocx`                                   | . | . | . | . | . | p | p |
| 20 | `dallocx`                                   | . | . | . | . | . | p | . |
| 21 | `sdallocx`                                  | . | . | . | . | . | p | . |
| 22 | `nallocx`                                   | . | . | . | . | P | . | P |
| 23 | `operator new`, `new[]`                     | P | p | . | . | p | . | . |
| 24 | `new`, `new[]` (`nothrow_t`)                | P | p | . | . | p | . | . |
| 25 | `new`, `new[]` (`align_val_t`, 4 forms)     | P | p | . | . | p | - | . |
| 26 | `delete`, `delete[]` (unsized, 8 forms)     | . | . | . | . | . | p | . |
| 27 | `delete`, `delete[]` (sized)                | . | . | . | . | . | p | . |
| 28 | `delete`, `delete[]` (sized, aligned)       | . | . | . | . | . | p | . |
| 29 | `malloc_trim`                               | . | . | . | . | P | . | . |
| 30 | `mallopt`                                   | . | . | . | . | P | . | . |
| 31 | `mallinfo`, `mallinfo2`                     | . | . | . | . | . | . | . |
| 32 | `malloc_stats`, `malloc_info`               | . | . | . | . | - | . | . |
| 33 | `malloc_stats_print`                        | . | . | . | . | . | - | . |
| 34 | `mallctl`, `mallctlnametomib`, `..bymib`    | . | . | . | . | - | - | . |
| 35 | `get_defrag_hint`                           | . | . | . | . | P | - | . |
| 36 | `challoc_version`, `challoc_features`       | . | . | . | . | . | . | . |
| 37 | `malloc_conf`, `malloc_message`, `je_..`    | . | . | . | . | . | . | . |

Notes that recur below:

- *(D)*: no entry point states disjointness. Theorem 5
  (`Heap::theorem_uniqueness_held`, `theorem_uniqueness`,
  `theorem_uniqueness_apart`, `heap/mod.rs:2883`, `:2908`, `:2935`)
  proves that a cached slot is held by no other cache,
  bitmap word or claim. But `Heap::alloc_fast` (`heap/paths.rs:54`) drops
  the slot's token at hand-out, so nothing states that the result is
  apart from the program's live blocks. Huge runs have no token:
  `huge::allocated` (`huge/mod.rs:622`) is uninterpreted and minted by a
  trusted lemma. Hence `p` for every allocating row.
- *(errno)*: `set_errno` (`api/libc.rs:83`, `api/print.rs:1045`) is
  `external_body` with no `ensures`, and no spec models `errno`. A
  "NULL with `ENOMEM`" contract is proved only for its NULL half
  (`enomem`, `einval`, `api/libc.rs:91`, `:102`, ensure `r@.addr == 0`).
  Nothing proves that the failure paths are the only NULL returns, or
  that `free` preserves `errno`.
- *(live)*: `free_target` (`heap/mod.rs:432`, theorem 3) is proved. A
  pointer that is not a slot start or a huge page start aborts. Liveness
  (bit clear: a double free, a stale pointer, a never-allocated slot) is
  caught at run time by `SlabTable::check_live` (`class/table.rs:1540`)
  through the trusted `is_set_unchecked` (`:1317`), or by
  `Huge::free_run` (`huge/mod.rs:4779`) for runs (**unconfirmed**:
  read from its doc comment; its `ensures` states only `r.pages`). No
  `ensures` says that a non-live pointer aborts. Under the free contract
  (`axiom_free_token`), a double free makes the proof context
  inconsistent.
- *(U)*: the size specs are in `vspec/usable.rs`. `usable_is(p, size,
  n)` is the usable size of a block an allocation of `size` bytes
  returned: the class's size for a slot, `huge_usable(size)` (`size`
  rounded up to whole pages) for a huge page start, the ends
  `covers_usable` names. `usable_of(p, n)` is what a query reads for a
  live block: the class's size for a slot; for a huge page start only
  `run_bytes(p, n)`, the descriptor's shape (whole pages, at least one,
  guard inside the region). `usable_size` ensures `usable_of`, and
  `malloc_usable_size`, `sallocx`, `xallocx` and `free_usable` re-export
  it. So a query of a slot is exact, and
  `vspec::usable::theorem_usable_consistent` proves that it reads what
  the allocation stated (`r == n` for a slot). A query of a huge run is
  not tied to the request: the run's request size lives in no ghost
  state a C caller hands back, so that needs gap 1's run token. Hence
  `p` for rows 9, 15, 18 and 19. The allocating `_usable` entry points
  are `.0` of `_stored` twins (`malloc_usable_stored`,
  `realloc_usable_stored`; `calloc_usable_zeroed`'s third result) that
  return the `Stored` token of `store_usable` (`external_body`, its
  ensures `s@.ptr() == usable && s@.val() == n`), the last store through
  `usable`. They state `usable_is(r, size, s@.val())` for a non-null
  result. For a huge run they read the size back with `usable_size` right
  after `alloc_huge`, and `vspec::huge::axiom_owned_run`, which states
  `r == huge_usable(size)` (the descriptor `Huge::take` wrote: it
  takes `usable_pages(size)` pages plus the guard), makes it the
  request's page-rounded size.

Per row (cells marked `P` without a note are the cited `ensures`
alone):

1. `malloc` (`api/libc.rs:213`). R: `fits(r, size, QUANTUM)`. D: *(D)*.
   F: *(errno)*. Not stated: NULL only on exhaustion; `malloc(0)` is
   non-NULL (the module doc promises a distinct slot).
2. `calloc` (`api/libc.rs:328`, `.0` of `calloc_zeroed`, `:341`). R:
   `fits(r, n * m, QUANTUM)`. Z: `r.1@.covers(r.0, n * m)` (theorem 6).
   F: `n * m > usize::MAX ==> r@.addr == 0` is proved; the rest is
   *(errno)*.
3. `realloc` (`api/libc.rs:698`). R: `r != p ==> fits(r, size, QUANTUM)`
   and `fits(..) || kept(r, size, 1)` (`kept`, `:617`). A kept huge block
   has no size bound. D: *(D)*. Also, `realloc`'s move takes its
   disjointness from the trusted `axiom_moved_block`
   (`vspec/addr.rs:121`). C: the code copies `min(old, size)` bytes, with
   `old` the class size for a slot (`old_size_or_keep`, `:635`).
   `ptr::copy_nonoverlapping` has no byte-level spec, so contents are not
   modelled. F: `p != 0 && size == 0 ==> r@.addr == 0` is proved. Not
   stated: on a NULL from the move, `p` is still live; the code returns
   before `heap::free`. B: `p != 0 ==> free_target(p)`, *(live)*.
4. `reallocarray` (`api/libc.rs:733`). As `realloc` for `n * m`. F also
   proves `n * m > usize::MAX ==> r@.addr == 0`. B: `free_target(p)` only
   when `n * m` does not overflow (an overflowing call returns NULL
   without looking at `p`).
5. `aligned_alloc`, `memalign` (`api/libc.rs:1228`). R:
   `is_pow2(align) ==> fits(r, size, align)`. B:
   `!is_pow2(align) ==> r@.addr == 0` is proved; `EINVAL` is *(errno)*.
   challoc's `memalign` rejects a non-power-of-two alignment, unlike
   glibc's, which rounds the alignment up (the module doc names the
   `EINVAL`). F: *(errno)*.
6. `posix_memalign` (`api/libc.rs:1251`). R: `alloc_aligned`
   (`:1146`) ensures `fits(p, size, align)`, but `store_out` (`:1243`,
   `external_body`) claims nothing about `*out`. F: the return code is
   `0`, `ENOMEM` or `EINVAL`. Not stated: `0` iff an allocation was
   stored, and `*out` untouched on failure. B:
   `r == EINVAL <==> !(is_pow2(align) && align % 8 == 0)`.
7. `valloc` (`api/libc.rs:1297`). R: `fits` at the page
   (`1 << lg_page`). F: *(errno)*.
8. `pvalloc` (`api/libc.rs:1306`). R: `fits(r, size, page)`. The
   page-rounded size the code allocates is not stated. F: the
   `checked_add` overflow returns `enomem()`, which is not in the
   `ensures`; the rest is *(errno)*.
9. `malloc_usable_size` (`api/libc.rs:565`). F: `p == 0 ==> r == 0`. B:
   `p != 0 ==> free_target(p)`, *(live)*. Before init, a non-NULL `p`
   aborts (`free_before_init`, `:508`). U: `p != 0 ==> usable_of(p, r)`,
   exact for a slot, the descriptor's shape for a huge run, *(U)*.
10. `free` (`api/libc.rs:386`). B: `p != 0 ==> free_target(p)`, *(live)*;
    NULL is a no-op (early return; not in the `ensures`). F: `errno`
    preservation (the module doc's promise) is *(errno)*. A thread's
    first `free` may create its heap before the abort (theorem 3).
11. `free_sized`, `free_aligned_sized` (`api/libc.rs:471`, `:481`). B:
    `free_target(p)`, *(live)*. The size hint is checked against the slot's
    class only on the slow path (`Shared::check_size_hint`,
    `heap/shared.rs:1363`, verified, no `ensures`). On the fast path
    (`free_sized_fast`, `heap/paths.rs:583`) a matching-class slot is
    pushed untested; its liveness is checked when the cache flushes. A
    non-power-of-two alignment gives no hint and aborts on a slab pointer.
    F: as `free`.
12. `challoc_malloc_usable` (`api/libc.rs:798`, `.0` of
    `malloc_usable_stored`). R, D, F: as `malloc`. U: the `Stored` token
    of the last store through `usable` has `ptr() == usable` and, for a
    non-null result, `usable_is(r, size, val())`: the class's size, or
    `size` rounded up to whole pages for a huge run (*(U)*, through
    `axiom_owned_run`). That the class is `nallocx(size, 0)`'s (the
    smallest that holds `size` bytes) is true of `class_index` but not
    exported here. Not stated either: the 0 stored for a null result
    (`Heap::alloc_fast`'s pop states a slot, and the regions' spec does
    not rule out a slot at address 0).
13. `challoc_calloc_usable` (`api/libc.rs:890`, `calloc_usable_zeroed`,
    `:905`). R, F: as `calloc`. Z: the token covers `n * m` bytes and
    `covers_usable(r.1, r.0, 0, n * m)`, the whole usable size the doc
    promises and the code zeroes (`write_zero(p, u)`, or the known-zero
    token of `heap::alloc_zeroed` or `alloc_huge`). For huge runs the
    `memset` length rests on `axiom_owned_run` (`vspec/huge.rs:77`). U:
    as row 12, for `n * m`, and the null result's 0 is stated.
14. `challoc_realloc_usable` (`api/libc.rs:969`, `.0` of
    `realloc_usable_stored`). R, D, C, F, B: as `realloc` (the keep
    decision is `realloc_old`, `:1010`). U: as row 12. A kept block's
    size is `realloc_old`'s, which ensures `usable_of(p, old)` and, when
    kept, `usable_is(p, size, old)` (for a run, `usable_for(size) ==
    Some(old)` is the keep test); a moved block's is
    `malloc_usable_stored`'s.
15. `challoc_free_usable` (`api/libc.rs:1055`). F: `p == 0 ==> r == 0`.
    B: as `free`. U: `p != 0 ==> usable_of(p, r)`, *(U)*: the class size
    from `heap::free_fast_class`, which ensures `is_slot(c, p)`, or
    from `free_usable_slow`'s classification (`:1081`), or
    `Huge::usable_size` there.
16. `mallocx` (`api/jemalloc.rs:231`, `.0` of `mallocx_zeroed`, `:243`).
    R: `fits(r, size, 1 << (flags & 0x3f))`. With no `MALLOCX_LG_ALIGN`
    this is alignment 1, weaker than the quantum the code gives. Z: with
    `MALLOCX_ZERO`, `covers(r, size)` and `covers_usable(.., 0, size)`
    (`vspec/zero.rs:109`, the whole usable size; theorem
    `vspec::zero::theorem_zeroed_memory`, `:240`). Also, for a non-NULL
    result, `usable_is(r, size, nalloc_usable(size, flags & 0x3f))` (row
    22). F: NULL on failure
    with `errno` untouched (`alloc_with`'s doc) is not stated beyond
    `fits` allowing NULL. B: an arena or explicit cache that does not
    exist returns NULL (doc of `alloc_with`, `:160`); not stated.
17. `rallocx` (`api/jemalloc.rs:260`, `rallocx_zeroed`, `:278`). R: as
    `realloc` at `1 << (flags & 0x3f)`, `kept` at that alignment. Z: for a
    new or moved block with `MALLOCX_ZERO`, a token from offset `k` to
    the usable end (`covers_usable`). `k == 0` for a NULL `p` and
    `min(class size, size)` for a slot `p`. For a huge `p`, `k` is only
    `0 <= k <= size`: `old_size_or_keep` does not state `old` for a run.
    A kept block gets no token; jemalloc zeroes only past the old real
    size, which a kept block keeps. C, F, B: as `realloc`; NULL `p`
    allocates.
18. `xallocx` (`api/jemalloc.rs:332`). Never resizes; returns
    `sallocx(p, 0)`. B: `free_target(p)` unconditionally, so NULL aborts.
    U: `usable_of(p, r)`, as row 19.
19. `sallocx` (`api/jemalloc.rs:340`). B: `free_target(p)`
    unconditionally; NULL aborts (`not_an_allocation`, `:135`); *(live)*.
    U: `usable_of(p, r)`, *(U)*. For a slot `mallocx(size, flags)`
    returned, `theorem_usable_consistent` gives `r == nallocx(size,
    flags)`, jemalloc's contract.
20. `dallocx` (`api/jemalloc.rs:353`). B: `p != 0 ==> free_target(p)`,
    *(live)*. With `MALLOCX_TCACHE_NONE` the release tests the bit
    immediately (**unconfirmed**, from the doc comment).
21. `sdallocx` (`api/jemalloc.rs:378`). B: `p != 0 ==> free_target(p)`,
    *(live)*. jemalloc's `nallocx(size, flags) == sallocx(p)` contract
    is checked as in row 11: `flags == 0` is `free_sized`, and
    `sdallocx_flags`/`sdallocx_routed` (`:391`, `:409`) hint
    `class_for(size, align)`.
22. `nallocx` (`api/jemalloc.rs:432`). F, U: `r == 0 || size <= r` and
    `r == nalloc_usable(size, flags & 0x3f)` (`api/jemalloc.rs`), the
    size of `spec_class_for(size, 1 << (flags & 0x3f))`, or
    `huge_usable(size)`, or 0 when the page rounding overflows. `mallocx`
    and `mallocx_zeroed` (through `alloc_with`) ensure
    `r != 0 ==> usable_is(r, size, nalloc_usable(size, flags & 0x3f))`,
    so `nallocx(size, flags)` is the usable size `mallocx(size, flags)`
    gives. The two calls agree because `class_for` ensures
    `r == spec_class_for(size, align)` for a power-of-two `align`, the
    smallest class that serves the request (`least_class`, from
    `class_of`'s and `class_index`'s new minimality: no smaller class
    holds `size` bytes), and `Huge::usable_for` is `Huge::spec_usable`.
    F: `r == 0` exactly when no class serves the request and `size`
    rounded up to a page overflows (class sizes and `huge_usable` are
    positive); `mallocx` then returns NULL (`alloc_huge` ensures
    `huge_ok(size)` of a non-NULL run).
23. `operator new`, `new[]` (`api/cxx.rs:214`). R:
    `fits(r, size, QUANTUM)`. F: the C++ contract (never NULL; throws
    `std::bad_alloc` after the `new_handler` loop) is not stated. `oom`
    (`:172`) returns only a non-NULL pointer when `nothrow` is false, and
    `throw_bad_alloc` (`:131`) is `-> !`, but no `ensures` says
    `r@.addr != 0`. `oom` has no `decreases` (it waits on the program's
    handler).
24. `new` `nothrow_t` (`api/cxx.rs:226`). R: as row 23. F: NULL after
    the handler loop; not stated beyond `fits`.
25. `new` `align_val_t` (`api/cxx.rs:237`). R: `is_pow2(align) ==>
    fits(r, size, align)`. F: as rows 23 and 24 by `nothrow`. B: a
    non-power-of-two alignment (undefined in C++) makes `aligned_alloc`
    return NULL, so the handler loop runs; nothing is stated.
26. Unsized `delete` forms, with or without `align_val_t` or `nothrow_t`
    (`cxx::delete`, `api/cxx.rs:256`). B: as `free`; the alignment is
    ignored.
27. Sized `delete` (`cxx::delete_sized`, `api/cxx.rs:267`). B: as
    `free_sized`.
28. Sized aligned `delete` (`cxx::delete_sized_aligned`,
    `api/cxx.rs:278`). B: as `free_aligned_sized`.
29. `malloc_trim` (`api/libc.rs:1350`). F: `r == 0 || r == 1`. Not stated:
    whether anything was purged.
30. `mallopt` (`api/glibc.rs:72`). F: `r == 1` (accepted and ignored).
31. `mallinfo2` (`api/glibc.rs:82`) ensures `fordblks <= arena` and the
    three zero fields; `mallinfo` (`:115`) is verified with no contract.
    The figures are unbounded counters (`fig_add`, `fig_mul`).
32. `malloc_stats`, `malloc_info` (`api/print.rs:1067`, `:1053`):
    verified bodies, no contract. F: `malloc_info`'s `-1` with `EINVAL`
    for bad arguments is *(errno)* and not in an `ensures`. The sinks
    (`file_target`, `stderr_target`, `:1108`, `:1115`) are
    `external_body`.
33. `malloc_stats_print` (`api/print.rs:947`): outside `verus!`, with its
    `export_pair!` wrapper (`:1131`); unverified. B: the program's
    `opts` string and callback are read through trusted code.
34. `mallctl`, `mallctlnametomib`, `mallctlbymib` (`api/mallctl.rs:1585`,
    `:1646`, `:1685`): verified bodies, no `ensures` on the return code
    (`result`, `:1573`, maps `Err(e)` to `e`). Every access to the
    caller's memory is `external_body` and claims at most bounds:
    `name_byte`, `name_bytes`, `old_len`, `copy_prefix`, `store`, `load`,
    `query_ptr`, `store_triple`, `mib_cap`, `mib_out`, `mib_in` (`:803`
    to `:1676`). So B (a bad `oldlenp` or `newlen`, an unknown name) is
    not stated; the batch-query length check (bug #1) is verified inside
    the body.
35. `get_defrag_hint` (`api/jemalloc.rs:465`). F: `r == 0 || r == 1`. B:
    "never aborts" for a foreign pointer cannot be stated, because an
    abort satisfies every postcondition. The body is verified.
36. `challoc_version` (`api/mod.rs:123`), `challoc_features` (`:296`):
    verified, no contract.
37. Data symbols `malloc_conf`, `malloc_message`, `je_malloc_message`
    (`api/jemalloc.rs:76`-`:90`): no code. `message_hook` and
    `malloc_write`, which read the hooks, are unverified.

### Internal properties under every row

| Property                                 | Code | Where                  |
|------------------------------------------|------|------------------------|
| I1 region separation                     | P    | th. 1, heap/mod.rs:182 |
| I2 metadata integrity                    | P    | th. 4, heap/mod.rs:253 |
| I3 uniqueness (no slot handed out twice) | p    | theorem 5              |
| I4 guard pages never handed out (slabs)  | P    | th. 7 (section 7)      |
| I4 guard pages never handed out (huge)   | p    | th. 7 (section 7)      |
| I5 purge never discards live data        | -    | below                  |

- I1, I2: for every result an entry point states `fits` for, with the
  premises and the "not covered" lists of sections 1 and 4.
- I3: a slot is unique while it is cached, held by a bitmap word or in
  flight (*(D)* above). There is no model of live blocks, and the `free`
  side rests on `axiom_free_token` (`class/mod.rs:3954`).
- I4: theorem 7 (lines as in section 7). For slabs, `Regions::is_slot`
  excludes the guard slabs, so every `fits` (the R column) and every
  `free_target` (the B column) states it per result: `theorem_guard_pages` and
  `theorem_guard_free` (`heap/mod.rs:212`, `:243`). For huge runs it is
  stated only of the huge lock's values (`Inner::theorem_run_guards`,
  `huge/mod.rs:941`): no entry point ties its huge result to a live
  descriptor, and fallback-mode stale `PROT_NONE` guards are not
  modelled.
- I5: class purges run only on slabs in the `empty` set, and
  `walk_empty` (`class/decay.rs:265`) checks at run time that the
  descriptor says `Empty` with `live() == 0`, aborting otherwise
  (`not_empty`, `:315`). `page_group_is_idle` (`:207`) is trusted. No
  invariant ties `live() == 0` to "no slot of the slab is allocated or
  cached". `purge::Batch`'s "ranges are free" (`purge.rs:218`) is a doc
  comment; `Batch::wf` states only bounds. Theorem 6's
  `axiom_discarded_zero` premises lean on the same unproved fact.

### Gaps, by cells turned to `P`

1. **Live-block ghost model**: `malloc` and friends hand the caller the
   slot's token (or a run token in place of the uninterpreted
   `huge::allocated`); every free consumes one. This turns the D column
   (16 rows) and I3 to `P`, removes `axiom_free_token`, and turns
   *(live)* B cells (15 rows) to `P`: a non-live free has no token
   and the run-time check becomes provably reached. It also derives
   `axiom_moved_block`'s disjointness. Largest gain, largest effort.
2. **`errno` as ghost state**: give `set_errno` an `ensures` over a
   ghost per-thread `errno`, and add `r@.addr == 0 <==> failure` to the
   allocating entry points. This turns the F cells of rows 1-8, 12-14
   and 32 (and `free`'s preservation, rows 10, 11) to `P` or nearly so.
   Mostly spec work; the hot paths keep their code.
3. **Usable-size contract**: done for rows 12, 13, 14 and 22 (U to `P`)
   and row 13's Z, *(U)*. `usable_size` states both halves
   (`usable_of`), and `malloc_usable_size`, `sallocx`, `xallocx` and
   `challoc_free_usable` re-export it; `*usable` is a `Stored` token from
   `store_usable`; `nallocx` and `mallocx` share `nalloc_usable`. Left:
   the huge half of a query (rows 9, 15, 18, 19 stay `p`), which needs
   the run's request size in ghost state the caller holds (gap 1's run
   token), and the 0 that `challoc_malloc_usable` and
   `challoc_realloc_usable` store for NULL (a slot at address 0 is not
   ruled out by `Regions::wf`).
4. **Failure-path contracts**: `r@.addr == 0 ==> p` still live for the
   realloc family (needs 1, or a ghost "not freed" result);
   `posix_memalign`'s `*out` through a `PointsTo` for `store_out`;
   `!nothrow ==> r@.addr != 0` for `operator new` (provable from `oom`'s
   loop today). Rows 3, 4, 6, 14, 17, 23-25.
5. **Byte contents**: a `PointsToRaw`-level spec of user blocks and of
   `copy_nonoverlapping` would turn the C column (4 rows) to `P`. It
   needs the permissions threading that `TODO.md` lists.
6. **Huge `kept` bound**: state a run's usable bytes against the region
   in the huge descriptors (theorem 1's "weaker than the code"). Turns R
   of rows 3, 4, 14 and 17 to `P`, and with it `rallocx`'s `k` for huge
   blocks (Z of row 17).
7. **Purge and guards**: tie the empty set and `purge::Batch` ranges to
   slot and extent tokens (I5, and theorem 6's purge premise). The guard
   half is done for slabs (`!guard_slab` is in `is_slot`, theorem 7);
   for huge runs, tie `Huge::alloc_run`'s result to its live descriptor
   so `theorem_run_guards` holds per result (I4, huge row).
