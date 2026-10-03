# Whole-allocator theorems: statements and premises

Reference for the whole-allocator theorems (sections 1-7 and 10) and the
cgroup policy's theorem as the code states them: the Verus statement,
the entry points it covers, and every trusted premise it rests on, with
`file:line`. Read at `b2cad68` (branch `verus`); line numbers are the
`fn`/`axiom` line at that commit.
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

**The `free` contract.** `heap::live::axiom_c_claim`
(`heap/live.rs:489`) is the one trusted step at the C ABI boundary: for
a pointer C passes to a free or a realloc, it gives a `Live` token that
claims it (`Live::claims`, `:146`): the slot's token if the pointer is a
slot start, the token of the run that starts there if it is a huge page
start, nothing otherwise. The allocations hand C the token of every
block they return (`Live::gives`, `:382`), so C, which holds the block,
holds the token. It is called once per C call, by the Rust entry point
the export calls, before anything else: `libc::free`, `free_sized_hint`
(`free_sized`, `free_aligned_sized`, the sized `delete`s),
`realloc`, `realloc_usable`, `free_usable`, and `jemalloc::rallocx`,
`dallocx`, `sdallocx` (`api/libc.rs:432`, `:477`, `:812`, `:1134`,
`:1256`, `api/jemalloc.rs:274`, `:389`, `:428`); every verified core
below them takes the token as a parameter. A double free, or a free of a
slot or huge page start that is not a live block, gives C a token it
does not hold, which makes the proof context inconsistent at that call:
it voids theorem 3 for that free and every theorem for later calls. A
pointer that is neither a slot start nor a huge page start gets no token
and aborts at run time (theorem 3 is not voided for it).

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
- TLS asm: `sys::tls::get` (`sys/tls.rs:125`), `get_tp` (aarch64,
  `:180`), `set` (`:205`); a read returns the `TlsSlot` token's value
  and `set` makes the written value the token's (theorem 8), nothing
  more.

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
`api/mod.rs:319` `init_slow`,
`api/cxx.rs:172` `oom`, `purge/background.rs:379` `main`,
`sys/mod.rs:533`, `:697`, `:760`, `sys/cgroup.rs:798`. The theorems are
partial correctness: they say what holds if the call returns. None of
these is on the thread-cache fast paths, which terminate and take no
lock (theorem 9).

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
| `realloc` (`api/libc.rs:646`) | `size`, `QUANTUM` |
| `reallocarray` (`api/libc.rs:681`) | `n * m`, `QUANTUM` |
| `aligned_alloc` = `memalign` (`api/libc.rs:1167`) | `size`, `align` (pow2) |
| `valloc`, `pvalloc` (`api/libc.rs:1236`, `:1245`) | `size`, the page |
| `malloc_usable` (`api/libc.rs:746`) | `size`, `QUANTUM` |
| `calloc_usable` (`api/libc.rs:838`) | `n * m`, `QUANTUM` |
| `realloc_usable` (`api/libc.rs:908`) | `size`, `QUANTUM` |
| `mallocx` (`api/jemalloc.rs:210`) | `size`, `1 << (flags & 0x3f)` |
| `rallocx` (`api/jemalloc.rs:236`) | as `mallocx` |
| `cxx::new`, `new_nothrow` (`api/cxx.rs:214`, `:226`) | `size`, `QUANTUM` |
| `cxx::new_aligned` (`api/cxx.rs:237`) | `size`, `align` (pow2) |

A kept block (`r == p`) is included: the realloc family ensures `fits`
for every result (revised by gap 6 below). The keep decision
(`old_size_or_keep`, `realloc_old`) ensures `kept(p, size, align)`
(`api/libc.rs`), a quantum-aligned slot of a large enough class, or a
huge page start with `max(size, 1)` bytes of the region from it, which
unfolds to `fits` at `align` and at the quantum (`lemma_kept_fits`
states it). The huge
half's bound: the keep test is `usable_for(size) == Some(old)`, so
`size <= old`, and `old`, the usable bytes `Huge::usable_size` reads
from the run's descriptor, ensures `spec_run_usable` (the usable pages
and the guard inside the region); `vspec::usable::lemma_run_bytes_block`
turns that into `is_huge_block(p, max(size, 1))`.

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
  (`huge/mod.rs:3685`) is verified. A kept run's bound rests on the
  same axioms: `Huge::usable_size` reads its `spec_run_usable` from a
  live `Active` descriptor under the huge lock invariant (`run_ok`,
  `desc_ok`).
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
- Liveness is a precondition, not a run-time result: every verified
  free core takes the caller's token for the pointer (`Live::claims`)
  and ensures that what it freed was the live block of that token
  (`p@.addr != 0 ==> blk.block(p)`: `libc::free_block`,
  `free_sized_block`, `free_usable_block`, `realloc_block`,
  `jemalloc::dallocx_block`, `sdallocx_block`, `rallocx_zeroed`,
  `heap::free`, `Heap::free`). A slot start whose bit is clear still
  aborts by the run-time tests (`SlabTable::is_set_unchecked`,
  `class/table.rs:1288`, trusted), the sized fast path pushes untested
  and the flush catches it, and a non-live huge page start aborts in
  `Huge::free_run`; but for such a pointer C has no token, and the
  boundary's `axiom_c_claim` (the `free` contract, common premises)
  makes the context inconsistent instead of proving the abort.
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
- Live: the program's. A `heap::live::Live` token (`heap/live.rs:42`)
  holds the slot's token while the program holds the block: the
  allocation paths move the popped, refilled, compacted or directly
  claimed slot's token into it (`heap::hand_out`, `heap/mod.rs:549`;
  `Heap::pop_compact_run`; `Shared::alloc_direct`), and the free paths
  take it back out (`Live::claim_slot`, `take_slot`) for the cache push
  or the release. Huge runs have the same shape: a `huge::RunToken`
  (`huge/mod.rs:652`) holds the run's extent, a singleton of the
  extents' authority, and its pages, a subset of the pages' authority;
  `Huge::take` inserts both into the authorities `Inner::wf` holds
  (`huge/mod.rs:1114`: their sets are the `Active` descriptors'
  extents and pages, `active_runs`, `active_pages`, `:827`, `:835`), and
  `Huge::free_run` deletes them, after `agree` shows the token's extent
  is the descriptor's (`lemma_token_extent`, `:5106`).

**Live blocks (the D column).** `Live::theorem_apart`
(`heap/live.rs:423`): the usable bytes of two live tokens, wherever
they are held, share no byte. Two slots of one class are two tokens of
one authority, so different slots, a class size or more apart; slots of
two classes lie in two class regions; a slot and a run lie in the
superblock and the huge region; two runs hold disjoint page subsets.
`Heap::theorem_uniqueness_live` (`heap/mod.rs:3021`): a slot a
well-formed heap caches is no live block of its class.

**Entry points.** The allocating entry points' cores return the token
of the block they hand out, with at least the request's usable bytes
(`Live::gives`, `heap/live.rs:382`): `malloc_block`, `calloc_zeroed`,
`alloc_aligned`, `aligned_alloc_block`, `posix_memalign_block`,
`valloc_block`, `pvalloc_block`, `malloc_usable_stored`,
`calloc_usable_zeroed`, `realloc_block`, `realloc_usable_stored`
(`api/libc.rs`), `mallocx_zeroed`, `rallocx_zeroed`
(`api/jemalloc.rs`), `new_block`, `new_aligned_block` (`api/cxx.rs`).
The exported functions call them and hand the token to C with the
block; the free cores take the token C holds (common premises, "The
`free` contract").

**Premises beyond the common ones.**
- `heap::live::axiom_c_claim` (`heap/live.rs:489`): the `free`
  contract at the C ABI boundary, one call per C call. See "The `free`
  contract" under Common premises.
- `table::axiom_slot_loc` (`class/table.rs:644`, one call `:446`):
  `SlabTable::new` runs once per class per process.
- `huge::axiom_run_locs` (`huge/mod.rs:636`, one call in `Huge::new`):
  `Huge::new` runs once per process, so its run authorities are the
  process's.
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
- The live model ends at the C boundary: C carries no ghost state, so
  the token of a block handed to C is dropped at the export and the one
  C frees comes from `axiom_c_claim`. "No block is handed out twice"
  holds of the tokens the verified code holds and, under the `free`
  contract, of C's blocks.
- The theorems are lemmas; no global invariant across heaps calls them,
  and `theorem_apart` relates two tokens held at the same time.

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
    // ...
},
```

(Excerpt: the clause also states `covers_usable`, `k`'s value for a
null `p`, and `k == min(o, size)` for an `o` with `usable_of(p, o)`.)

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
- `rallocx_zeroed`'s `k` is `min(old, size)` for some `old` with
  `usable_of(p, old)` (revised by gap 6: `old_size_or_keep` ensures
  `usable_of(p, old)` of the `old` it returns, and the code copies
  `min(old, size)`). That is exact for a slot `p` (the class's size). For
  a huge `p`, `usable_of` states only the shape of `old` (`run_bytes`:
  whole pages, at least one, inside the region), not the descriptor's
  value; the value comes from `p`'s token: `old_size_or_keep` also
  ensures `old == blk.len()` (gap 1), and `rallocx_zeroed` ensures
  `k == min(blk.len(), size)`. That token is the one `axiom_c_claim`
  gives for C's pointer, so for a huge `p` `k` is exact under the `free`
  contract only.

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
- Huge runs are covered only at the lock invariant: `alloc_run` now
  returns its run's token (gap 1's `RunToken`, an `Active` descriptor's
  extent in the authorities `Inner::wf` holds), but no lemma turns
  `theorem_run_guards` into a per-result statement over the token, so no
  entry point states that a returned huge block avoids other runs' guard
  pages. In fallback mode
  (no `MADV_GUARD_INSTALL`) a reused extent may hold a stale `PROT_NONE`
  guard that `take` re-commits; that commit is not modelled.
- `theorem_guard_free` says that a pointer into a guard slab aborts,
  not which message it prints (`guard_free` for a handed-out guard slab
  through `check_live`'s `not_live`, `never_committed` past the
  watermark, `invalid_free` for a mid-slot address).
- The kernel's guard semantics (a fault on access) are not modelled: the
  theorem is about where blocks lie, not about what the guard does.

## 9. The fast path never hangs

Read at this section's commit (the `file:line`s are the `fn` line there).

**Statement.** No theorem function: the statement is the invariant mask
each fast-path function declares, which Verus checks at every call. A
function may call only functions whose mask is a subset of its own
(`opens_invariants`). An unannotated exec function has Verus's default
mask, `any`, and so do most `core` functions vstd specifies, among them
`Option`'s `?`, `Option::map`, the array-to-slice coercion and the
`core` atomics. The two lock acquisitions declare `any`:

```rust
pub fn lock(&self)                                  // RawLock, lock.rs:81
    opens_invariants any

pub fn lock(&self) -> (g: LockGuard<'_, T>)         // Lock<T>, lock.rs:276
    ensures lock_inv(g@), lock_frame(lock_init(*self), g@),
    opens_invariants any
```

and so, by default, do the other acquiring functions (`try_acquire`,
`lock_slow`, both `try_lock`s, `lock.rs:63`, `:93`, `:72`, `:293`). The
fast paths declare `none`, or only the bitmap words' namespace for the
unsized free (`bitmap::word_ns()`, the one `AtomicInvariant`
`is_set_unchecked` opens for its bit test):

```rust
pub(super) fn malloc_fast(size: usize, blk) -> (r: Option<*mut u8>)   // api/libc.rs:315
    opens_invariants none
pub fn alloc_fast(&mut self, class: usize, flags: u8, blk) -> ...    // heap/paths.rs:59
    opens_invariants none
pub fn free_fast(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8, blk) -> (r: bool)   // heap/paths.rs:573
    opens_invariants [crate::bitmap::word_ns()]
pub fn free_fast_class(...) -> (r: Option<usize>)                    // heap/paths.rs:614
    opens_invariants [crate::bitmap::word_ns()]
pub fn free_sized_fast(...) -> (r: bool)                             // heap/paths.rs:654
    opens_invariants none
pub fn existing_heap(shared: *const Shared) -> ...                   // heap/thread.rs:148
    opens_invariants none
pub unsafe fn shared_unchecked() -> (r: &'static Shared)            // api/mod.rs:262
    opens_invariants none
```

(Signatures shortened.) `any` is no subset of either mask, so no call
chain from a masked function reaches a lock acquisition: **no function
the `malloc` and `free` fast branches call can acquire a lock**. The
fast branches call nothing else (read off the entry points, below). The
masks also rule out every other unannotated function, so each function
the masked ones can reach declares a mask itself; the closure is the
list below, and Verus checks every call in it.

**Bounded.** Every verified function in the closure has no
`exec_allows_no_decreases_clause`, so Verus proves it terminates, and
none contains a loop or a recursive call. The trusted ones contain no
loop except `cached_recently` (`heap/cache.rs:650`, the
`double_free_scan` compare): `1..=len` with `len < 4`, at most three
compares. Each fast path is straight-line code with branches, so it runs
a bounded number of instructions whatever the heap holds. "Bounded" is
read off the code; Verus proves termination, not a step count.

**Scope: which paths.** The entry points' fast branches. The entry
points are not masked: their other branch is a tail call into the slow
path. Each C entry point mints or claims its block's token (theorem 5's
`Live`) and calls its `_block` core (`malloc_block`, `free_block`,
`free_sized_block`, `free_usable_block`), which holds the fast branch;
the cores are unmasked for the same reason. Their fast branches call
only the functions named here:
- `malloc` (`malloc_block`, `api/libc.rs`): `class_index`, `api::shared_ptr`,
  `existing_heap`, `fast_flags`, `Heap::alloc_fast`. `malloc_fast` is
  the same branch as one masked function; it is also the C++
  `operator new` family's fast path. A declined pop tail-calls
  `malloc_slow_class`.
- `free` (`free_block`, `api/libc.rs`): `existing_heap`, `shared_unchecked`,
  `Shared::push_flags`, `free_fast`; then `free_slow`.
- `free_sized` and `sdallocx` with `flags == 0`: `class_index`, then
  `free_sized_block`'s `existing_heap`, `shared_unchecked`, `push_flags`,
  `free_sized_fast`; then `free_sized_slow`.
- `challoc_free_usable` (`free_usable_block`): its `free_fast_class` call on the same heap
  lookup. The class size it then returns comes from the unmasked
  `class_size`, outside the theorem.

**The closure.** These are the functions with a declared mask on these
paths, verified unless marked trusted:
- **Size and process state:** `class_index`, `layout::class_of`;
  `api::shared_ptr` (trusted), `shared_unchecked`, `shared_ref`
  (trusted).
- **Heap lookup:** `existing_heap` (on a ghost `TlsSlot::entry()`),
  `existing_heap_in`, `tls_heap_slot`, `sys::tls::get` and
  the aarch64 `get_tp` (trusted asm), the x86_64 `get_tp`, `fast_flags`,
  `heap_from_slot` (trusted).
- **Pop:** `Heap::alloc_fast`, `pop_fast`, `cache_ptr`, `home_ptr_at`
  (trusted), `pop_top`, `CacheHeader::slots`.
- **Push:** `free_fast`, `free_fast_class`, `free_sized_fast`,
  `Heap::push_fast`, `push_cache_flags`, `push_cache`, `cached_recently`
  (trusted).
- **Free checks:** `Shared::push_flags`, `multi_arena`, `checked_slot`,
  `set_slot`, `sized_slot`, `GuardTest::is_guard`,
  `Heap::arena_volatile` (trusted); `SlabTable::watermark` (trusted),
  `is_handed_out`, `is_set_unchecked`, `owner_unchecked` (trusted),
  `owner_at`, `load_owner` (trusted).
- **Layout:** `Regions::slab_slot`, `slot_in_class`, `region_len`, the
  free function `layout::regions::class_entry` (trusted);
  `Class::slab_size`, `slab_mask`, `slot_of_offset`.
- **Primitives:** `sys::pac::sign` and `auth` (trusted asm),
  `vspec::addr::addr_to_ptr` (trusted), and the `vspec` items under
  "Premises".

**Code changes.** A masked function cannot use what vstd specifies with
the default mask. So the eight `?`s on these paths are now
`let Some(..) = .. else { return None }`, and `alloc_fast`'s `map` is a
`match`. The four array reads (`class_base` twice, `tables`,
`guard_test`) and the `multi_arena` load in `checked_slot`'s path go
through the wrappers below; `owner_at`'s load is arena isolation's
trusted `load_owner` (theorem 14), which declares the mask. The release
library's machine code is identical to the previous commit's on x86_64.

**Premises beyond the common ones.**
- **Trusted masks.** Each trusted function's mask is a declaration that
  Verus takes on trust. Their bodies were read, and none takes a lock or
  waits:
  - `shared_ptr`, `shared_ref` and the `Global::get` it calls
    (`api/mod.rs:153`, `assume_init_ref`).
  - `tls::get` and `get_tp` (asm), `fast_flags` (a field read),
    `heap_from_slot` (calls `pac::auth`), `home_ptr_at`.
  - `cached_recently` (the loop above), `arena_volatile`,
    `owner_unchecked` (calls `owner_at`), `watermark`, `class_entry`.
  - `pac::sign` and `auth` (asm; under `cfg(test)` a counter
    increment), `addr_to_ptr`.
- **New wrappers.** `vspec::atomic::load_bool`: the `core`
  atomic load, declared `opens_invariants none`; `load_owner`
  (`class/table.rs`, a `Relaxed` load) declares the same mask.
  `vspec::slice::array_get_unchecked` stands for `get_unchecked` on an
  array.
- **Masks on existing specifications.**
  - Added here: `opens_invariants none` on the `vspec` specifications the
    paths use: `usize::leading_zeros` and `u32::rotate_right`
    (`vspec/core_int.rs`), and `<*mut T>::add` (`vspec/ptr.rs`).
  - Already declared: `vspec::word::load` (`vspec/word.rs:96`, the bit
    test's load), `<*mut T>::is_null` (`vspec/addr.rs:64`), and vstd's
    `ptr_ref`, `ptr_mut_ref`, `ptr_mut_write` and `wrapping_*`.
- **Destructors.** Verus requires every `Drop` impl to declare
  `opens_invariants none`, so the masks cannot exclude a lock taken in a
  destructor. No value on these paths has a `Drop` type (the only guard
  type, `LockGuard`, comes only from `Lock::lock`).
- **Ghost calls.** The live-block steps (`heap::live::axiom_c_claim` at
  the C entry, `Live::claim_slot` and `put_slot` through `lend_slot` on
  the free paths, `hand_out` on the pop) are proof functions and run
  nothing.

**What stays exempt, and why.**
- **The entry points themselves**, as above. The slow path they
  tail-call creates heaps, refills and flushes caches, and takes the
  class, commit, huge and metadata locks. Slow paths are outside the
  theorem.
- **`calloc` and `realloc`.** `calloc` reads `api::shared()`, which can
  run `init_slow` (a wait on another thread's init), and
  `heap::alloc_zeroed` goes through `heap_for_thread`, which can create
  the heap. Their pop is `pop_fast`, which is in the closure.
- **`free_aligned_sized` and `sdallocx` with flags.** Their hint comes
  from `class_for`, which is unmasked (it uses `?` and has a loop bounded
  by `NCLASSES`).
- **Declined fast-path operations.** The scheduled pop (`compact_mask`),
  the full-cache push, the `zero_on_free` push and a scan hit decline to
  the slow path, so they are outside the fast path by construction.
- **The ten `exec_allows_no_decreases_clause` functions** (common
  premises) are all off these paths: lock waits, `claim_with`'s retry,
  `release_routed`, `init_slow`, the OOM handler loop, the purge thread,
  and the kernel retry loops in `sys/`. They need a liveness model.

**Where the statement is weaker than the code.**
- **Composition.** Verus checks the masked functions, not the entry
  points. That `malloc`, `free`, `free_sized` and `sdallocx` call only
  masked functions on their fast branch is read off their bodies.
  `malloc`'s branch is an inlined copy of `malloc_fast`'s calls, not a
  call to it.
- **Step count.** Termination is proved; the step bound is not, because
  Verus has no cost model. The bound rests on the closure having no loop
  but `cached_recently`'s.
- **Other waits.** The masks prove that no lock acquisition is called,
  not that nothing waits. The fast paths contain no other wait either (no
  atomic read-modify-write, no spin), but that, too, is read off the
  code.
- **What Verus sees.** The theorem covers the code Verus sees, and the
  build compiles the same code: no `#[cfg(verus_only)]` swap is on these
  paths.
- **Weak memory.** A load may return a stale value, but every branch on
  one still ends, so the bound holds.

## 10. No leak on thread exit

Read at this section's commit (the `file:line`s are the `fn` line
there).

**Statement** (`heap/mod.rs:3891`, `Heap::flush_all`, which
`Heap::destroy` runs first):

```rust
pub fn flush_all(&mut self, shared: &Shared)
    requires
        shared.wf(),
        old(self).wf(),
    ensures
        final(self).keeps(*old(self)),
        final(self).same_shape(*old(self)),
        final(self).drained(),
        log_adds(final(self).released(), old(self).released(), old(self).held_all()),
```

with (`heap/mod.rs:672`-`:703`, `:3037`-`:3085`; abridged)

```rust
pub open spec fn log_adds(new: Seq<(int, int)>, old: Seq<(int, int)>, s: Set<(int, int)>) -> bool {
    &&& old.len() <= new.len()
    &&& new.subrange(0, old.len() as int) == old
    &&& new.subrange(old.len() as int, new.len() as int).to_set() == s
}

// What owned header p caches: (class, slot address) per entry below len.
pub closed spec fn held(&self, p: *mut CacheHeader) -> Seq<(int, int)> {
    tagged(self.hdr_class(p), Seq::new(self.cached_len(p) as nat, |i: int| self.cached(p, i) as int))
}
// held_all: the union of held(p) over the home headers and the headers
// of every node on the `others` list (held_upto, array_held).
// drained: every one of those headers has len == 0.
```

When a heap's flush of everything returns, no home header and no
header of a node on its `others` list caches a slot, and the heap's
released log has gained exactly the `(class, address)` pairs those
headers cached at entry: no cached slot is lost. Those headers are the
cache memory `destroy` frees next (`free_caches` over the home array and
each node), so the thread-exit destructor (`heap_destructor`,
`heap/thread.rs:39`, through `Heap::destroy`, `heap/mod.rs:4707`)
strands no cached block. `tcache.flush` (`Tcaches::flush`,
`heap/tcaches.rs:163`), `malloc_trim` (`api/libc.rs:1609`), the mallctl
purge's `flush_caller` (`api/mallctl.rs:1371`) and
`thread.tcache.enabled` off (`Heap::set_tcache_enabled`,
`heap/mod.rs:4203`) run the same function.

The released log (`CachePerms::released`, `heap/mod.rs:821`, ghost) is
appended to by `Heap::flush` alone (by inspection: no spec keeps it
fixed elsewhere; the theorem's `ensures` are relative, so this does not
weaken them), with the batch it hands to `Shared::release_routed` and
the same tokens. The chain that makes an entry of the log a release:

- `Heap::flush` (`heap/flush.rs:307`) also ensures, for any `n`:
  every other owned header holds what it held, the cache holds a subset
  of what it held, and `log_adds(final.released(), old.released(),
  old.held(cache).to_set() - final.held(cache).to_set())`: the slots
  that left the cache are the log's new entries. `lemma_flush_split`
  (`heap/flush.rs:612`) proves the split by counting (the cached,
  kept and released addresses are distinct, the kept are cached, the
  released are cached and not kept, and their numbers add up), and
  `Heap::lemma_flush_accounting` (`heap/mod.rs:3416`) lifts it to the
  pairs.
- `Shared::release_routed` (`heap/shared.rs:2036`) takes the bag of the
  batch's tokens by `&mut` and ensures `final(bag)@ == old(bag)@ -
  aset(ptr_addrs(ptrs@), 0, ptrs@.len())`: the bag lost exactly the
  batch's tokens. That each went to a `ClassState::release_owned` call
  (`class/mod.rs:4535`), which passes it, slab by slab, through
  `release_slab` to `AtomicBitmap::release` (`bitmap.rs:1762`), whose
  word invariant (`bitmap::WordPred`, `bitmap.rs:706`) must take the
  token of every bit it clears, is read from the verified bodies, not
  stated by an `ensures`. The `FOREIGN` cache's and a home cache's
  other-arena slots are released the same way through the owner arena's
  class (`release_routed`'s routing); there is no other "remote" path.
- `Heap::flush_arena` (`heap/mod.rs:3774`) flushes one array whole and
  ensures its headers empty, the log gained what they held, and every
  other header unchanged; `flush_all` composes the home array and every
  node, using `lemma_home_apart` and `lemma_nodes_apart`
  (`heap/mod.rs:4031`, `:4045`) for the headers' distinctness.

**Premises beyond the common ones.**
- `Heap::destroy` is `external_body` (its verification shifts LTO
  inlining of `Meta::free`, `TODO.md`): that it calls `flush_all` first,
  on a heap that is `Heap::wf` and no longer in use, is read from its
  body. The heap `heap_destructor` hands it is the TLS slot's heap, whose
  `wf` `heap_at` assumes (common premise). After the flush its
  `free_caches` aborts on a non-empty header (`assert_flushed`); the
  theorem says that abort is not reached through the permissions, but
  `free_caches` reads the plain `caches` field, which Verus models apart
  from them.
- Slot uniqueness (theorem 5): each cached slot's token is held once,
  which is what makes a log entry stand for one token.

**Where the statement is weaker than the code.**
- The accounting is over the slots under each header's first `len`
  entries (`CachePerms::tokened`). That `toks` holds no token beyond
  them is not an invariant (every verified writer moves tokens with
  their entries, by inspection), and a header the permissions own that
  is neither a home header nor on the `others` list is not ruled out
  (only `create`, `create_detached` and `other_caches` add headers, all
  of those kinds); the statement names the home and node headers.
- Tracked tokens are affine: no `ensures` states that a token leaving
  `release_routed`'s bag reaches a bitmap word; that rests on the
  bodies of `release_routed`, `release_owned` and `release_slab` (above).
- No `no_duplicates` on the log's new entries: two headers of one class
  cannot hold one slot (theorem 5), but the statement is the set, not
  the count.
- Partial correctness, as every theorem: `release_routed`'s loop has no
  termination proof (`exec_allows_no_decreases_clause`).
- The arena-unbind and exit purge that `destroy` runs after the flush
  are not covered: they return no cached slot.

## Shuffle is a permutation

The coordinator assigns this theorem its number at merge. The
`file:line`s are the `fn` line at this section's commit.

**Statement** (`class/mod.rs:308`, `:334`, and the `ensures` of
`shuffle_zero`, `:198`):

```rust
pub(crate) open spec fn permutes(s: Seq<*mut u8>, o: Seq<*mut u8>) -> bool {
    s.to_multiset() == o.to_multiset()
}

pub(crate) fn shuffle_zero(out: &mut [*mut u8], rng: &mut u64, /* ghost: zero and slot tokens */)
    ensures
        final(out)@.len() == old(out)@.len(),
        permutes(final(out)@, old(out)@),
        // ... every entry comes from the input (`from_seq`); the zero
        // and slot tokens follow their entries ...

pub(crate) proof fn theorem_shuffle_permutation(s: Seq<*mut u8>, o: Seq<*mut u8>, p: *mut u8)
    requires
        permutes(s, o),
    ensures
        s.len() == o.len(),
        s.to_multiset().count(p) == o.to_multiset().count(p),
        s.contains(p) == o.contains(p),
        s.no_duplicates() == o.no_duplicates(),
```

(Abridged where marked.) The `slot_randomize` shuffle loses no slot and
duplicates none: after it, every pointer occurs in the batch exactly as
many times as before, whatever `rng` holds. `theorem_shuffle_permutation`
spells out the consequences: same length, same count of every pointer,
same membership, and no duplicates after exactly when none before.
`shuffle` (`:178`, test-only) has the same `ensures`.

**Proof.** The Fisher-Yates loop (`for i in (1..len).rev()`, swap `i`
with `j = xorshift(rng) % (i + 1)`) keeps `permutes(out@, old(out)@)` as
an invariant; `lemma_swap_permutes` (`:313`) shows one `<[T]>::swap`
leaves the multiset unchanged (the update-twice spec of `swap` in
`vspec/slice.rs`, with vstd's `to_multiset_update`). `j <= i < len` is
also checked (the `swap` precondition and the `%`), so the swap is in bounds and the remainder by `i + 1` never
divides by zero. Nothing about `xorshift`'s output is used.

**Where it applies.** `ClassState::claim_random_zero` (`:2662`) is the
only caller outside tests: with `slot_randomize`, `Heap::claim`
(`heap/flush.rs:47`) calls it for every refill and run rebuild, and it
shuffles `out[..n]` right after `claim_with` writes the batch. Inside
it, a verified assertion states `permutes(out[..n], mid[..n])`: the
batch handed out is the batch `claim_with` claimed, reordered. The slot
tokens (theorem 5) and zero tokens (theorem 6) of each entry move with
it, so `claim_random_zero` ensures the same token postconditions as
`claim_tokens`. That the batch has no duplicates in the first place
follows from its slot tokens (`keyed`: one token of the class's
authority per entry, and two such tokens are disjoint), but no lemma
states it for the claimed batch; `class::gather` derives it when a
batch's tokens are collected.

**Premises beyond the common ones.** None. The proof uses only the
`assume_specification` of `<[T]>::swap` (`vspec/slice.rs:39`) and
vstd's multiset lemmas.

**Scope.**
- Randomness is out of scope: nothing is claimed about the distribution
  of the permutation, the quality of `xorshift`, or the seed from
  `Heap::rng` (`heap/flush.rs:89`; `seed` is a trusted `getrandom`
  read, or a clock read when that fails). The theorem holds for every `rng` value, including a stuck or
  adversarial one.
- The random start word of each pick that scans the bitmap in `finish`
  (not a `Fresh` or `Purged` prefix; `(xorshift(r) as usize) % words`)
  changes which free slots are claimed; the claim contract (slot tokens,
  `is_slot`, theorem 5's token protocol) is proved for every start word.

**Where the statement is weaker than the code.**
- `permutes` is stated on `shuffle_zero`'s argument and, inside
  `claim_random_zero`, on the claimed batch; no entry point's `ensures`
  says the batch was shuffled from `claim_with`'s output, because the
  pre-shuffle batch has no name outside the function (exposing it would
  need a ghost return value, a signature change). Callers see only the
  token postconditions, which say nothing about order.
- The flush's grouping (`flush::group_by_slab`, `external_body`) is a
  different reordering, assumed to be a permutation (common premises),
  not covered here.

## Fork safety

Numbered by the coordinator at merge (lanes are adding 8, 9 and 10).
Read at this section's commit (the `file:line`s are the `fn`/`axiom`
line there).

**Statement** (`api/mod.rs:517`, `:609`, `:618`, `:627`; asserted at
`:818` and `:960`):

```rust
pub open spec fn listed(k: ForkLock, a: Set<int>) -> bool {
    ||| k == ForkLock::Tcaches
    ||| k == ForkLock::Arenas
    ||| arena_part(k, a)      // Class(i, c), c < NCLASSES, and Sets(i), i in a
    ||| table_part(k)         // Table(c), c < NCLASSES
    ||| k == ForkLock::Huge
    ||| k == ForkLock::Meta
    ||| k == ForkLock::Stats
    ||| stats_part(k, a)      // ArenaStats(i), i in a
}

pub(crate) open spec fn wf(self, s: Shared) -> bool {      // ForkHeld
    &&& forall|i: int| self.arenas().contains_key(i) ==> 0 <= i < self.n()
    &&& forall|k: ForkLock| self.keys().contains(k)
            ==> self.lock(k) == lock_at(s, self.arenas(), k)
}

pub open spec fn all(self) -> bool {                       // ForkHeld
    forall|k: ForkLock| self.keys().contains(k) <==> listed(k, self.arenas().dom())
}

pub(crate) open spec fn fork_holds(s: Shared, held: ForkHeld, state: LockHeld) -> bool {
    &&& held.wf(s)
    &&& held.all()
    &&& state.held() == purge::background::state_lock()
}

// end of fork_prepare (api/mod.rs:818):
assert(fork_holds(*s, held, state));
// end of fork_release (api/mod.rs:960), after every unlock:
assert(held.keys() =~= Set::empty());
```

(Triggers dropped.) When `fork_prepare` returns, the calling thread
holds one `LockHeld` token per lock of the fork lock list and nothing
else. The list is the explicit-cache table lock, the arena creation
lock, every class lock and the slab-set lock of every arena
`fork_prepare` found (indices below the count it read where
`get_existing` returned an arena), every slab table lock, the huge,
metadata and process statistics locks, every such arena's statistics
lock, and the background thread's state lock. Each token holds the lock
its key names in `s` (`lock_at`, `:530`): the value the accessor path
through `Shared` reaches, with `arenas[i]` standing for the arena that
`get_existing(i)` returned.

`fork_release` is the body of `fork_parent` and `fork_child`. It starts
from that hold and hands every token back through
`RawLock::unlock_held` (`lock.rs:126`), which requires the token of the
lock it unlocks and consumes it. A token is not `Copy`, and the map is
empty at the end, so each listed lock is released once, by its holder.
`fork_child` first rewrites the background state through
`Lock::get_unlocked`, which requires the state lock's token
(`lock.rs:382`); `purge::background::fork_child`
(`purge/background.rs:502`) proves the state's lock invariant on the
value it leaves (`:520`). After either handler no listed lock is held.
In the child, no lock is owned by a thread that no longer exists: at
the fork the forking thread held every listed lock (the mutex premise
below excludes a second holder), and the child's one thread is that
thread's copy, which releases them.

The tokens: `RawLock::lock_held` (`lock.rs:116`) is `lock` plus a
`LockHeld` whose `held()` is the lock. The field is private to
`lock.rs`, so outside it only `lock_held` builds a token; the handoff
axioms below are the other source.

**Premises beyond the common ones.**
- *The mutex.* `lock` returns only once this thread holds the lock, and
  no other thread holds it until `unlock`. This is `RawLock`'s futex
  contract; Verus does not check it (atomics return any value). The
  token records the calls; the mutex turns them into exclusive holds.
- *The handoff* (`axiom_fork_resumed_state`, `api/mod.rs:647`, and
  `axiom_fork_resumed`, `:656`; axioms). Ghost state does not cross the
  C calls, so the hold `fork_prepare` ends with is restated where the
  release begins: `axiom_fork_resumed` ensures `fork_holds` at the
  `&Shared` the release reads (`shared` returns the same static). The
  premise includes that the bundle's `n` and `arenas` are what
  `fork_prepare` read, which the ensures does not name. glibc runs the
  parent and child handlers on the forking thread (in the child, its
  copy) after `_Fork`, and nothing releases a lock in between: signals
  are blocked from the prepare handler's first step to the release's
  last, and the atfork handlers registered before ours (whose prepare
  runs after ours and whose parent and child handlers run before ours)
  take no allocator lock. Each handler calls each axiom once; nothing
  enforces that.
- *A frozen arena table* (`arena_held`, `api/mod.rs:671`, and
  `count_held`, `:687`; `external_body`). While this thread holds the
  arena creation lock, `Arenas::count` and `get_existing(i)` return what
  `fork_prepare`'s first reads returned. `Arenas::create` and `build`
  write the count and the table only under that lock, and a published
  arena is never freed or moved. That the release, and `fork_prepare`'s
  second arena scan, cover the same arenas as its first scan rests on
  these two and the handoff. They take a bundle (`ForkHeld`) and trust
  its `n` and `arenas`; its fields are private to `api`, and by
  convention only `fork_prepare` and `axiom_fork_resumed` build one
  (`api`'s submodules could too).
- `purge::background::state` (`purge/background.rs:462`,
  `external_body`): the static `STATE`'s raw lock is `state_lock()`
  (Verus does not read statics in specifications).
- The background state's invariant in the child also rests on
  `axiom_state_lock_inv` (`purge/background.rs:52`) and on
  `get_unlocked`'s trusted ensures (the value a holder left).
- *Completeness of the list.* That `listed` names every `Lock` in the
  crate is not proved. The test `fork_lock_list_matches_the_source_tree`
  checks the `Lock` fields and statics in the source tree against the
  test's `FORK_LOCKS` (`api/mod.rs:1165`); that `FORK_LOCKS`, `ForkLock`
  and the handlers' bodies name the same locks is by inspection.

**Where the statement is weaker than the code.**
- Lock identity is by value along the accessor path (Verus has no
  addresses of references). The proof checks that every lock and unlock
  goes through the accessor path its key names (two locks' values cannot
  be proved equal, so a swapped token fails), not that the paths reach
  distinct addresses.
- The arenas are those `fork_prepare` found, not proved to be every
  arena that exists: `get_existing` has no specification.
- "Released once" holds given one handoff per handler: a second call of
  either axiom would hand out a second token for a held lock and still
  verify (it does not prove false, since no lemma says two tokens of one
  lock are apart).
- The order is not proved: the statement says which locks are held, not
  that `fork_prepare` takes them in the nesting order (design 9) that
  rules out a deadlock with a thread mid-operation. The release order
  is unstated too.
- Partial correctness: a `lock` that never returns (a deadlock in
  prepare) satisfies everything. A prepare handler registered before
  challoc's (one that runs after ours) that allocates on a slow path
  would deadlock on its own thread; the handoff premise excludes it.
- Consistency in the child covers the one value the handlers write (the
  background state). The handlers do not touch the other locked values,
  which keep the invariant their last holder left, as the lock axioms
  assume for every acquire.
- The signal mask's save and restore (`save_sigmask`, `saved_sigmask`,
  `external_body`) are not part of the statement.

## N. Arena isolation

Number assigned at merge (other lanes add 8, 9 and 10). Read at this
section's commit (the `file:line`s are the `fn`/`axiom` line there).

**Statement.** A slab slot belongs to one arena for the life of the
process: the arena `take_group` handed its slab's group to,
`slab_owner(c, slab)` (`class/table.rs:174`), and for an address
`slot_owner(c, p) = slab_owner(c, slab_index(c, p))` (`:179`). The
allocation side and the release side are both stated against it: a
slot is released only into the arena that owns its slab, the arena whose
`plan` reserved in that slab, or the free aborts:

```rust
// class/mod.rs:1991, ClassState::plan (the claim's lock hold): every
// slab a refill or claim reserves slots in is this arena's.
ensures picks_owned(self.spec_class() as int, self.spec_arena(), final(picks)@, 0, np as int),

// class/mod.rs:3899
spec fn picks_owned(c: int, a: u16, p: Seq<Pick>, lo: int, hi: int) -> bool {
    forall|j: int| lo <= j < hi ==> slab_owner(c, (#[trigger] p[j]).slab as int) == a
}

// class/mod.rs:4581, ClassState::release_owned (the one function that
// releases slots into an arena's class state).
ensures
    forall|i: int| 0 <= i < r ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[i]@.addr) == self.spec_arena(),
    r < ptrs@.len() ==> slot_owner(self.spec_class() as int, ptrs@[r as int]@.addr) != self.spec_arena(),

// class/mod.rs:4528, ClassState::release_with: all or abort (`wrong_arena`).
ensures
    forall|i: int| 0 <= i < ptrs@.len() ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[i]@.addr) == self.spec_arena(),
```

`spec_arena` (`class/mod.rs:1190`) is the class state's `arena` field;
`Arena::inv` (`arena.rs:893`) now states that `classes[c]` has arena
`index`, so `Arena::class` (`:1029`) ensures `r.spec_arena() ==
self.spec_index()`, and the lookups by index ensure the arena they
return has that index: `Shared::owner` and `Shared::arena`
(`heap/shared.rs:1677`, `:1658`), `Arenas::get`, `materialise`, `build`
and `get_existing` (`arena.rs`). `Shared::wf` gains `arena0.spec_index()
== 0`, proved by `Shared::init` from `Arena::init`'s new postcondition.

**Per free path.** Every slab free reaches an arena's class state only
through `release_owned`, from one of two places:

| Path | What is proved |
|---|---|
| `Shared::free_direct(arena, class, p)` (`heap/shared.rs:1738`), from `heap::free_direct` (`heap/paths.rs:689`: `MALLOCX_TCACHE_NONE`, a thread without a heap) and `Heap::free_slow_in` (`:451`, no cache for the class) | ensures `self.wf() ==> slot_owner(class, p) == arena`: it releases through `owner(arena).class(class)`, of arena `arena`, and `release_with` aborts (`wrong_arena`) otherwise. Both callers pass the owner: `heap::free_direct` the arena of `classify_free`, which now ensures it is `slot_owner(c, p)` (`heap/shared.rs:1766`, from `check_located`, `class/table.rs:2205`), and `free_slow_in` that of `arena_of_slot` (`heap/shared.rs:1988`); so by their contracts `wrong_arena` cannot fire from them (Verus does not state it at the abort site). |
| `Shared::release_routed(arena, class, ptrs)` (`heap/shared.rs:2061`), from `Heap::flush` (`heap/flush.rs:509`): every cache, home or `FOREIGN` | each run goes through `owner(a).class(class)` (arena `a`), whose `release_owned` releases only `a`'s slots and stops at the first slot of another arena; the next run starts at `arena_of_slot` of that slot, which ensures `slot_owner` (`:1988`). Termination is now proved (`decreases 2 * left + [next slot not a's]`), which removes `release_routed`'s `exec_allows_no_decreases_clause`: with owners fixed, the `changed arena during a flush` abort is unreachable, which its branch now asserts (`assert(false)` in a proof block). |

Upstream of those, `Heap::free` (`heap/paths.rs:198`) pushes a slot onto
the home cache when `classify_free`'s arena, now `slot_owner(c, p)`, is
the heap's, and onto the `FOREIGN` cache otherwise; the sized paths push
untested onto the home cache and leave the routing to the flush (design
6.3). A cache therefore may hold slots of any arena; isolation is a
property of the release, not of the caches.

**Entry points.** Every free entry point of theorem 3's table (`free`,
`free_sized`, `free_aligned_sized`, `realloc`, `reallocarray`,
`realloc_usable`, `free_usable`, `dallocx`, `sdallocx`, `rallocx`,
`cxx::delete*`), and the heap destructor and `tcache.flush`/`destroy`,
release slab slots only by the two paths above: outside tests,
`Shared::free_direct` is the only caller of `release_with`, `Heap::flush`
the only caller of `release_routed`, and these two the only callers of
`release_owned`. No entry point's `ensures` changes: the per-path
contracts are the statement, as `fits` is theorem 2's.

**Huge runs.** The huge allocator is one per process; an arena only
counts its runs. `Shared::free_huge` (`heap/shared.rs:1956`) credits
`owner(run.arena)`, which now provably has index `run.arena`, the arena
`Huge::free_run` read from the run's descriptor, which `alloc_run`
wrote from its `arena` argument.

**How the claim side is proved.** `Inner::base` (`class/mod.rs:784`), part
of the class lock invariant, gains `owns(s) ==> slab_owner(c, s) ==
arena`. Slabs join an arena's ghost only in `next_fresh_slab`, as the
group `take_group` just returned, and `take_group` (`class/table.rs:2446`)
now ensures that every slab of that group has `slab_owner` the taking
arena, from its owner-word store (`lemma_fresh_group`,
`class/fresh.rs:281`). `take` (`class/mod.rs:2128`) requires `ready`, so
an owned slab, and ensures its pick's owner; `plan` collects
`picks_owned`.

**Premises beyond the common ones.**
- `class::table::store_owner` and `load_owner` (`class/table.rs:217`,
  `:196`, `external_body`): `take_group`'s `Relaxed` store of the owner
  word fixes `slab_owner` for the group's slabs, and a `Relaxed` load
  of a handed-out group's word returns it. Together: each group's word
  is written once, under the commit lock, before the watermark passes
  the group, and never again (the only other writer is the test-only
  `corrupt_owner_word`). `owner_at`, `owner`, `check_located` and
  `arena_of_slot` are verified on top of `load_owner`. These replace the
  informal assumption `release_routed`'s termination exemption rested
  on.
- `ClassState::owner_locked` (trusted bridge) now also ensures its
  result is `slab_owner`, as its body's `owner` call does. Its direction
  "an owned slab's word names the arena" is now proved (`Inner::base`);
  the converse remains assumed.
- `arena::arena_at` (`arena.rs:1195`, `external_body`) now ensures the
  arena behind registry entry `index` has index `index`: `build` stores
  arena `index` there and `Arenas::init` stores `arena0` at 0.
- `arena::axiom_arena_assemble` now requires every class state's arena
  to be the parts' index (`Arena::init` proves it from
  `ClassState::new`'s new postcondition); `heap::shared::
  axiom_shared_built` now states `arena0` as written, as it states the
  tables.
- `slab_owner` is keyed by class, which rests on one table per class per
  process: the premise of `table::axiom_slot_loc` (theorem 5), not a new
  kind.
- The "written once" and "handed out before read" conditions of the two
  wrappers are not checked by Verus: `store_owner` is private with one
  call site, past `take_group`'s fallible steps; `owner_at` requires
  only `slab < nslabs`, and its callers (`owner`, `check_located`,
  `owner_unchecked`) test the watermark first.
- Sequential consistency for the owner words, as for every atomic.
  `check_located` reaches the owner load after a `Relaxed` watermark
  load (`check_live`), the one owner path without `Acquire`.

**Where the statement is weaker than the code.**
- The claim side stops at the picks: `plan` states that every slab it
  reserved in is the arena's, but `finish` does not restate it for the
  pointers it writes (they are slots of the picks' slabs, which its
  `SlotToks` entries record by bit, not by owner). So no `claim` or
  `malloc` postcondition names the arena of a returned block.
- No ghost "release log": the statement is per call of
  `release_owned`/`release_with`/`Shared::free_direct`, not one formula
  over a program's history. Entry points state nothing new.
- The huge side is code reading over `free_run`'s result: the run
  descriptor's arena is not tied to `alloc_run`'s argument by a
  postcondition (the huge lock's ghost descriptors carry it, but no
  token survives between the calls).
- `checked_slot`'s owner test on the `free` fast path reads through
  `owner_unchecked` (trusted, unchanged); it only picks the cache.

## 8. Re-entrant heap registration

Read at this section's commit (the `file:line`s are the `fn` line there).

**Model.** The calling thread's TLS slot is per-thread ghost state: a
`sys::tls::TlsSlot` token (`sys/tls.rs:83`) whose `value()` is what the
slot holds and whose `creates()` counts the `Heap::create` calls made
with it. `get`, `get_tp` (`sys/tls.rs:125`, `:164`, `:180`) take it
shared and return its value; `set` (`:205`) takes it `&mut` and makes
the written value its value. `Heap::create` (`heap/mod.rs:4107`) takes
it `&mut`, leaves the value alone and adds one to `creates`; it is the
only caller of `TlsSlot::note_create` (`sys/tls.rs:107`), the only
writer of `creates`. A function handed only `&TlsSlot` can therefore
neither write the slot nor create a thread heap through that token, and
one handed `&mut TlsSlot` states what it does to both in its `ensures`.
That it does neither at all also needs that nothing it calls makes a
token of its own: `TlsSlot::entry` is `pub(crate)`, and its only callers
are the four top-level users of the slot named in the premises below
(checked by inspection).

The public readers keep their signatures: `heap_for_thread` and
`existing_heap` (`heap/thread.rs:83`, `:200`) make the token at entry
(`TlsSlot::entry`, `sys/tls.rs:102`, value unconstrained until read)
and run the cores `heap_for_thread_in` and `existing_heap_in`
(`heap/thread.rs:100`, `:212`), which carry the contracts.

`ThreadKey::set` (`heap/shared.rs:291`) is the call that may re-enter:
glibc's `pthread_setspecific` `calloc`s a second-level block of 32 keys
the first time a thread sets a non-null value for a key in it (keys from
index 32 up; the first 32 live in the thread descriptor). Its wrapper
`heap::shared::pthread_setspecific` (`heap/shared.rs:326`,
`external_body`) models every nested allocator entry as a core holding
the token the outer call lends it: it requires `reentry_benign` of the
slot and ensures the token unchanged, which is what the cores guarantee
at a benign value (below).

**Statement** (`heap/thread.rs:43`, `:100`, `:153`, `:212`;
`heap/shared.rs:291`):

```rust
pub open spec fn reentry_benign(v: usize) -> bool {
    v == TLS_CREATING || v == TLS_EXITED
}

pub fn heap_for_thread_in<'a>(shared: &'a Shared, Tracked(tls): Tracked<&mut TlsSlot>)
    -> (r: Option<&'a mut Heap>)
    requires shared.wf(),
    ensures
        r matches Some(h) ==> h.wf(),
        reentry_benign(old(tls).value()) ==> r is None && *final(tls) == *old(tls),
        old(tls).value() > TLS_EXITED ==> r is Some && *final(tls) == *old(tls),
        old(tls).value() == 0 ==> final(tls).creates() == old(tls).creates() + 1,
        old(tls).value() == 0 && r is None ==> final(tls).value() == 0,

pub fn existing_heap_in(shared: *const Shared, Tracked(tls): Tracked<&TlsSlot>)
    -> (r: Option<&'static mut Heap>)
    ensures
        r matches Some(h) ==> h.wf(),
        r is None <==> tls.value() <= TLS_EXITED,

fn create_heap_for_thread<'a>(shared: &'a Shared, slot: usize,
    Tracked(tls): Tracked<&mut TlsSlot>) -> (r: Option<&'a mut Heap>)
    requires shared.wf(), slot == old(tls).value(),
    ensures
        r matches Some(h) ==> h.wf(),
        slot != 0 ==> r is None && *final(tls) == *old(tls),
        slot == 0 ==> final(tls).creates() == old(tls).creates() + 1,
        slot == 0 && r is None ==> final(tls).value() == 0,

pub fn set(&self, heap: *mut Heap, Tracked(tls): Tracked<&mut TlsSlot>)  // ThreadKey
    requires reentry_benign(old(tls).value()),
    ensures *final(tls) == *old(tls),
```

(Line breaks added.) While `create_heap_for_thread` is inside
`ThreadKey::set`, the slot holds `TLS_CREATING`: the call's `requires`
is checked there, after `set(TLS_CREATING)` and before the heap is
published. A nested `heap_for_thread` or `existing_heap` on that thread
then returns `None` and leaves the token exactly as it found it: the
slot still holds `TLS_CREATING`, no `Heap::create` call was made, and no
heap was handed out or published. `existing_heap_in` holds only a
shared borrow of the token, and its callees (`tls_heap_slot`,
`fast_flags`, `heap_from_slot`) make none of their own. The same holds at
`TLS_EXITED`, after the thread's destructor has run. On an empty slot
`heap_for_thread` makes exactly one `Heap::create` call and leaves the
slot empty unless it returns a heap; above the sentinels it returns the
published heap and writes nothing.

**If `Heap::create` itself allocated.** `Heap::create` runs on the
empty slot, before `TLS_CREATING` is set, and its contract says it
leaves the token's value alone: the model has no nested entry there.
That rests on a premise checked by inspection, not by Verus: nothing
`Heap::create` reaches calls code that can allocate. Its callees are
`assign_home` (`Arenas::assign`, atomics and the arenas lock),
`create_detached`, which calls `Meta::alloc_raw` (verified; the
metadata lock, and `mprotect` as a raw syscall on its commit path) and
`init_fields` (`tid()` and `now_ns()`: `gettid` and `clock_gettime` as
raw syscalls), `mark_bound`, and `Arenas::unbind` on the null path. The
abort paths (`sys::abort_fmt`, which writes with a raw `write` and
raises `SIGABRT`, and the panic handler, which aborts) do not return.
None calls libc. If one allocated, the precise outcome would be:
- a nested `existing_heap` sees the empty slot and returns `None` (the
  theorem's conclusion still holds for it);
- a nested `heap_for_thread` sees the empty slot and makes its own
  `Heap::create` call. If the outer call holds the arenas or metadata
  lock at that point, the nested one blocks on it (the `Lock` is not
  re-entrant): the thread deadlocks. Otherwise the nested call creates,
  registers and publishes a second heap `H2`; when the outer call
  resumes it overwrites the slot with `TLS_CREATING` and then its own
  heap `H1`, and the key with `H1`. `H2` is never destroyed: its
  metadata block, its arena binding (`Arenas::assign`'s count) and any
  slots cached in it are stranded. Blocks the nested call returned stay
  valid allocations. If `Heap::create` allocated on every call, the
  nesting would recurse until the stack overflowed;
- in the model the outer token is then stale, so the premise below
  fails and the theorem does not apply to that outer call.
A signal handler that allocates while `Heap::create` runs is the same
case (allocation is not async-signal-safe, and challoc does not claim
it).

**Premises beyond the common ones.**
- The slot premise (`sys/tls.rs:83`): a token is not read after
  the slot has changed other than through it. `TlsSlot::entry`
  (`pub(crate)`, verified body, trusted in effect) makes a token with an
  unconstrained value at each top-level user of the slot:
  `heap_for_thread`, `existing_heap`, `heap_destructor`
  (`heap/thread.rs:59`, writes `TLS_EXITED`) and the background thread's
  `create` (`purge/background.rs:230`). An entry nested in another
  stands for the outer token lent to it, which the nested call leaves
  unchanged when the value is benign. Writers of the slot are
  `create_heap_for_thread` (only on an empty slot), `heap_destructor`
  and `create`, all through their tokens. `create` parks a slot above
  the sentinels on `TLS_CREATING` across `pthread_join` and
  `pthread_create` and then restores it; when the slot is empty it
  does not park, a nested allocation there may create this thread's
  heap, and `create` does not read its token again, so the premise
  holds.
- `heap::shared::pthread_setspecific` (`external_body`): an FFI call
  whose nested allocator entries reach the slot only through the cores
  (or, at a benign value, `create`, which then writes nothing), holding
  the token it is lent. It replaces the `assume_specification`
  of `libc::pthread_setspecific` (formerly in `vspec/heap.rs`), which
  claimed nothing.
- `sys::tls::get`, `get_tp`, `set` (inline asm, `external_body`): a read
  returns the token's value; a write makes it the token's value.
- `Heap::create` makes no call that can allocate (above).

**Where the statement is weaker than the code.**
- It covers only the slot and heap creation. A nested call's other
  effects (class locks, the heapless slow paths it takes) are outside
  it; that `create_heap_for_thread` holds no allocator lock across
  `ThreadKey::set` is not stated here.
- After a successful creation the theorem does not state that the slot
  holds a value above the sentinels (it does: the heap address, or the
  address signed under PAC, which keeps its low 48 bits).
- When `pthread_key_create` failed (`ThreadKey::valid` false) no
  `pthread_setspecific` call is made and the heap is never destroyed at
  thread exit; the theorem is silent on that.
- The background thread's `create` parks only a slot above the
  sentinels. On an empty slot, a nested allocation in `pthread_create`
  or `pthread_join` creates this thread's heap in the ordinary way; the
  theorem says nothing about that nested creation.
- The token makes the claim per thread only by construction: nothing in
  the model names the thread, and a token never crosses threads because
  the slot is thread-local.

## 8. No allocator lock across external calls

Read at this section's commit (the `file:line`s are the `fn`/`axiom`
line there).

**Statement.** Every call the reports make out of the allocator happens
while the calling thread holds no allocator lock. The reports are
`malloc_stats_print` and `je_malloc_stats_print` (`api/print.rs:1048`,
through `stats_print`, `:1032`), `malloc_info` (`:1154`) and
`malloc_stats` (`:1169`). Their calls out are the caller's `write_cb`,
the `malloc_message` hook through `malloc_write`
(`api/jemalloc.rs:116`; with no hook, `write(2)` to stderr), `fwrite`
on the caller's stream, and `write(2)` to stderr. A callback, a hook or
`fwrite` may allocate (glibc allocates a stream's buffer on its first
write), so a lock held across one deadlocks the allocation.

All the calls out are in one trusted function, and it requires a ghost
permit:

```rust
#[verifier::external_body]
tracked struct Unlocked { _no_copy: () }                      // api/print.rs:71

axiom fn axiom_unlocked() -> (tracked u: Unlocked);           // api/print.rs:81

fn emit(target: &Target, buf: &mut [u8; 1024], len: usize,    // api/print.rs:91
    Tracked(u): Tracked<&Unlocked>)
```

An `Unlocked` stands for "the calling thread holds no allocator lock".
The type is private to `api/print.rs` and opaque, so verified code
cannot build one, and `axiom_unlocked` is its only source in verified
code. Each of the three entry points calls it once, after
`super::shared()` and right before it makes the report's `Sink`
(`:1035`, `:1160`, `:1171`). The `Sink` keeps the permit from creation
to drop (its ghost field `permit`, `:121`). `Sink::flush` (`:139`) is
verified and is the only caller of `emit`, and passes the sink's
permit. So Verus checks:
- No verified code makes a report's calls out without a permit: `emit`
  is the only function that makes them, and every call to it must
  supply an `Unlocked`.
- A permit exists only inside a report. It comes from an entry point's
  `axiom_unlocked` and lives in that report's `Sink`, so only a function
  handed the sink (as a `W: Write`) or a `fmt::Formatter` that writes to
  it can reach `emit`. No function that takes a lock is handed either.

The unverified code of `api/print.rs` (`impl Write for Sink`, the
`malloc_stats_print` wrapper, the exports) could still call `emit` with
a forged permit; it does not, and the source test below rejects
`assume_new` there.

**Why the permit's claim holds.** Verus does not check that the holder
of a permit holds no lock: an acquisition takes no token, so no ghost
state follows it (see "Where the statement is weaker" below). The claim
holds whenever the sink's code runs, for three reasons:
1. **Entry.** The entry point's C caller holds no allocator lock, and
   `shared()` releases every lock it takes before it returns.
   `axiom_unlocked` assumes this. It is a C-program contract in
   `SEAMS.md`; see "Where the statement is weaker" for the callers that
   break it.
2. **The report code takes no lock.** No function of `api/print.rs`
   outside its tests acquires one: it calls no `lock()`, `raw()`,
   `raw_lock`, `get_unlocked` or `fork_lock`. The formatting impls the
   reports run take none either. They are `core`'s `Display` for
   integers, `str` and `bool` and `Debug` for `[usize; 9]` (`hist_line`,
   `:751`), and `sys::cgroup::Cgroup`'s `Display` (`sys/cgroup.rs:1082`,
   outside `verus!`, with its nested `Bytes` and `Limit`), which reads
   fields and atomics.
3. **Every callee releases the locks it takes before it returns.** The
   callees that lock are `stats::collect`, `collect_arena` and
   `collect_all` (through `Huge::stats`, `Meta::stats`,
   `ClassState::bin_stats` per class, and the `Shared.stats` lock), the
   slab histogram's `ClassState::is_partial`, and `policy::empty_bytes`
   (through `ClassState::empty_bytes` and `Huge::empty_bytes`). The
   others (`Arenas::get_existing`, `Tcaches::len`, `Cgroup::sample`, the
   `thp`, `purge` and `policy` readers, the slab descriptors and
   bitmaps) only load atomics or read files. Each callee that locks
   takes a `Lock` whose `LockGuard` is a local or a temporary, dropped
   at the end of its scope, and the guard's `Drop` releases the lock
   (`lock.rs:383`). A guard cannot outlive the call that took it:
   `LockGuard` appears in no signature, field or type outside `lock.rs`,
   and nothing is forgotten or leaked (`mem::forget`, `ManuallyDrop`,
   `Box::leak`). The only acquisitions of a bare `RawLock` are the fork
   handlers' (`fork_prepare`, `api/mod.rs:462`; `background::fork_lock`,
   `purge/background.rs:452`), and no report reaches them.

The test `report_lock_premises_hold_in_the_source`
(`api/print.rs:1950`) checks the source side of items 1 to 3 on every
`cargo test`, outside comments and `#[cfg(test)]` items: no
acquisitions and no `assume_new` in `print.rs`, `LockGuard` only in
`lock.rs`, no forget or leak, bare `RawLock` acquisitions only in the
two fork handlers, and `axiom_unlocked` called by the three entry
points only. The test is textual: a `RawLock` reached through a local
binding (`let r = x.raw_lock(); r.lock()`) would get past it. Rust's
types do the rest of item 3: a guard borrows its lock, and none of
these callees returns a type that could hold one.

**Run-time check.** `a_report_callback_may_reenter_the_allocator`
(`:1879`) and `a_malloc_info_stream_may_reenter_the_allocator`
(`:1902`) re-enter the allocator from every call out of a text report,
a JSON report and `malloc_info` (through an unbuffered `fopencookie`
stream). Each call allocates and frees in every tier, collects the live
statistics and prints a nested report, which together take every lock
a report takes. A lock a report takes, held across a call, deadlocks
the call, and the test aborts after 60 s. A mutation that holds the
`Shared.stats` guard across `stats_print`'s final flush fails it. Locks
no report takes (the arena registry, the slab sets and tables, the
explicit-cache table, the background state) are not exercised.

**Scope.**
- The four report entry points. `malloc_write`'s only caller outside
  tests is `emit`. The allocator's other calls out (`pthread_*`, the
  C++ `new_handler`, the abort path's raw `write`) are outside the
  theorem.
- The `malloc_message` hook is called from the same `emit` as the
  callback, under the same permit. It has no run-time test of its own:
  the hook is process-wide, and a test that sets it races the other
  hook tests.

**Premises beyond the common ones.**
- `axiom_unlocked` (item 1).
- `emit` (`external_body`): a plain match over the target, claiming
  nothing.
- `Cgroup`'s `Display` (item 2), outside `verus!`.
- Items 2 and 3 as the source test and Rust's types check them. They
  are not Verus facts.

**Where the statement is weaker than the code.**
- **Verus does not check the permit's meaning.** Verus has no
  thread-local ghost state, so "this thread holds lock L" can only be a
  token the thread carries, and every acquisition would have to take it
  and give it back. That means a `Tracked` ledger on `Lock::lock` and on
  every function on a path from an entry point to a lock: about 60
  acquisition sites, the `malloc` and `free` slow paths among them.
  Invariant masks cannot stand in for it. They are lexical and per
  function, while a guard is held from `lock()` to the end of its
  holder's scope. Narrowing a mask also fails on every vstd function
  that keeps the default `any` (`?`, the `core` atomics, `write_fmt`).
  So the theorem proves where the calls are and that each carries the
  permit. That no lock is held while the permit is in use rests on
  items 1 to 3.
- **The entry premise is the C caller's.** A `pthread_atfork` handler
  (prepare, parent or child) registered before challoc's runs while
  challoc's fork handlers hold every lock, so one that printed a report
  would deadlock at its first collection. So would a signal handler
  that printed a report while the thread it interrupted held a lock.
  None of the reports is async-signal-safe.
- **Deadlock freedom is not proved.** The statement concerns the locks a
  report holds when it calls out. Whether the callee's allocation then
  finishes depends on the slow paths, whose lock waits are
  partial-correctness premises (common premises).

## Bad input

The coordinator assigns this theorem its number at merge. The
`file:line`s are the `fn` line at this section's commit.

**Statement.** For every exported entry point, a bad argument either
aborts (the call does not return) or gets the documented error result;
no returning call acts on it. The exceptions are a huge run's size on a
sized free, which the code does not check, and a free of a block that
was freed and reallocated, which no allocator can tell from a live one
(table below). Stated as `ensures` on the Rust functions
the exports call (an abort satisfies every postcondition, so each one
says what holds of a call that returns):

```rust
// heap/mod.rs:498, :512 (theorem 3's target; the size hint's class)
pub open spec fn free_target(p: *mut u8) -> bool {
    ||| slot_start(p)
    ||| process_regions().is_huge_block(p@.addr, 1)
}
pub open spec fn hint_ok(p: *mut u8, hint: Option<usize>) -> bool {
    forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) ==> hint == Some(c as usize)
}

// api/libc.rs:580, :591; api/cxx.rs:302, :314; api/jemalloc.rs:481
fn free_sized(p, size)              ensures p@.addr != 0 ==> hint_ok(p, spec_class_for(size, 1)),
fn free_aligned_sized(p, align, size)
    ensures p@.addr != 0 ==> hint_ok(p, if is_pow2(align) { spec_class_for(size, align) } else { None }),
fn sdallocx(p, size, flags)
    ensures p@.addr != 0 ==> hint_ok(p, spec_class_for(size, 1usize << ((flags as u32) & 0x3f))),
            p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),

// heap/mallocx.rs:95; api/jemalloc.rs:184, :275, :440
pub open spec fn bad_tcache(t: Tcache) -> bool {
    t matches Tcache::Explicit(i) && i as usize >= MAX_TCACHES
}
pub open spec fn tcache_checked(size: usize, flags: c_int) -> bool {
    let f = mallocx::decode_spec(flags as u32);
    mallocx::bad_tcache(f.tcache) ==> f.arena is Some && spec_class_for(size, 1usize << f.lg_align) is None
}
fn mallocx(size, flags)  ensures tcache_checked(size, flags), /* and theorem 2's fits */
fn dallocx(p, flags)     ensures p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),

// api/cxx.rs:260
fn new_aligned(size, align, nothrow)  ensures align != 0 && !is_pow2(align) ==> r@.addr == 0,

// heap/mod.rs:534; heap/live.rs:474
pub proof fn theorem_free_target_aligned(p: *mut u8)
    requires regions_ok(), free_target(p),
    ensures (p@.addr as int) % (QUANTUM as int) == 0,
pub proof fn theorem_interior(&self, p: *mut u8, k: int)   // Live
    requires regions_ok(), self.wf(), 0 < k < self.len(), p@.addr == self.addr() + k,
    ensures free_target(p) ==> self.run_of() && k % page_size() == 0,
```

(Abridged: the `ensures` lists hold the other theorems' conjuncts too.)
Per kind of bad input, with what is proved and what rests on the C
contract:

| Bad input | Entry points | Result | Proved |
|---|---|---|---|
| Pointer outside every class region and the huge region, in a gap, in the metadata, in a guard slab | every free, the `realloc` family, `malloc_usable_size`, `sallocx`, `xallocx` | abort | yes: `free_target` (theorem 3, theorem 7's `theorem_guard_free`) |
| Misaligned pointer (not a multiple of the quantum) | as above | abort | yes: `free_target` and `theorem_free_target_aligned` |
| Interior pointer of a slot, or of a huge run off a page boundary | as above | abort | yes: `free_target` and `Live::theorem_interior` (`lemma_slot_interior`, `lemma_run_interior`) |
| Interior page start of a huge run | frees, `realloc` family | abort (`Huge::free_run`: `not_a_run`) | under the C contract only (*(C)* below) |
| The same | `malloc_usable_size`, `sallocx`, `xallocx` | abort (`Huge::usable_size`: `not_a_run`) | no: the queries take no token, and nothing states it |
| Slot freed with the size of another class, or a huge size | `free_sized`, `free_aligned_sized`, `sdallocx`, sized `delete`s | abort (`hint_mismatch`) | yes for a live slot: `hint_ok`; for a non-live one, *(C)* |
| Non-power-of-two alignment to `free_aligned_sized` or the sized aligned `delete` | as above | abort on a slot | as above: `hint_ok(p, None)`, so no slot start returns |
| Huge run freed with a size that names another usable size | sized frees | freed; the size is ignored (as jemalloc does) | no: not checked |
| Non-power-of-two or zero alignment | `aligned_alloc`, `memalign` | NULL (`EINVAL`) | NULL yes (row 5); `errno` no |
| Alignment not a power of two, or not a multiple of `sizeof(void *)` | `posix_memalign` | `EINVAL` | yes (row 6) |
| Non-zero alignment that is not a power of two | `new`, `new[]` (`align_val_t`) | NULL (`nothrow_t`) or `bad_alloc`, after the handler loop | yes: `new_aligned` |
| Any `MALLOCX_LG_ALIGN` (0 to 63) | `mallocx`, `rallocx` | a block aligned to it, or NULL | yes: `fits(r, size, 1 << lg_align)` (theorem 2): no misaligned block |
| `MALLOCX_TCACHE(t)`, `t >= MAX_TCACHES` (1024; the field reaches 4093) | `mallocx`, `dallocx`, `sdallocx` | abort (`invalid_tcache`, or first `hint_mismatch` on `sdallocx`), except a huge `mallocx` with an explicit arena, which never looks the cache up | yes: `tcache_checked`, `!bad_tcache` (the frees' for a live block or a non-block pointer; *(C)* otherwise) |
| `MALLOCX_TCACHE(t)` of a cache never created | as above | abort, with the same exception | no: existence is a run-time table load, not modelled |
| `MALLOCX_ARENA(a)` of an arena that does not exist | `mallocx`, `rallocx` | NULL | no: as above |
| Double free of a slot back in its bitmap (stale), or a free of a never-allocated slot of a handed-out slab | plain frees | abort (`check_live`, `check_located`: `invalid_free`) | under the C contract only |
| The same | `malloc_usable_size`, `sallocx`, `xallocx` | abort (`check_live`) | no: the queries take no token, and nothing states it |
| The same, through a sized free | sized frees | abort at the cache flush (`double_free`, `straddling_double_free`) | under the C contract only |
| Double free while the first copy is still in a thread cache | every free | with `double_free_scan` enabled (off by default), abort if among the top four entries (`cached_twice`); else abort at the flush, or the slot is handed out twice first, as under jemalloc | under the C contract only |
| Double free of a huge run | every free | abort (`Huge::free_run`: `already_free`, or `not_a_run` once the freed run has been merged or left the table) | under the C contract only |
| Free of a block that was freed and reallocated (ABA) | every free | frees the new block | undetectable by any allocator |

*(C)*: a pointer that is a slot start or a huge page start but not a live
block is excluded by `heap::live::axiom_c_claim` (`heap/live.rs:576`,
the `free` contract in the common premises): the free cores take C's
`Live` token for the pointer and give it back to the allocator, and for
such a pointer C holds no token, so the axiom's token makes the proof
context inconsistent and nothing is proved for that call. The run-time
checks behind those rows are verified code (each abort helper ends in
`sys::abort_fmt` and returns `!`), but no theorem says they fire for
every non-live block: the bitmap word invariant (`bitmap::WordPred`)
holds the slot tokens of exactly the clear bits, so a clear bit means
the slot is back in its bitmap, but that fact holds only inside the
atomic invariant at the instant of the load, and the free cores'
`ensures` (`blk.block(p)`) come from the token, not from the test. Those
B cells stay `p`.

The pointer-shape rows hold without *(C)*: `axiom_c_claim`'s token for
a pointer that is no slot start and no huge page start claims nothing,
so the context stays consistent and the abort is proved. `hint_ok` and
`!bad_tcache` on the frees are about the program's own arguments (a
size, an alignment, a flags word) against `p`'s class, a function of
`p`; they are proved for every live block, where the axiom's token is
the one C holds, and for every non-block pointer, but for a slot or
huge page start that is not live they rest on *(C)* like the rows
above. `tcache_checked` on `mallocx` and `new_aligned`'s NULL take no
C token and hold unconditionally.

**Proof.**
- Shape: theorem 3. `theorem_free_target_aligned` uses
  `lemma_slot_fits_quantum` (class sizes are multiples of the quantum,
  class bases `MAX_SLAB`-aligned) and `lemma_huge_fits` (a huge page
  start is page-aligned). `lemma_slot_interior`: two slot starts of one
  class are a class size apart (`lemma_slots_apart`), slots of two
  classes lie in two strides (`lemma_classes_apart`), and a slot lies
  outside the huge region (`lemma_slot_run_apart`).
  `lemma_run_interior`: page starts of the huge region are a whole
  number of pages apart.
- Size hint: `check_size_hint` (`heap/shared.rs:2043`) aborts when
  `Regions::classify` says `Slab { class }` and `hint != Some(class)`.
  `classify` and `slab_slot` (`layout/regions.rs:761`, `:621`) also
  ensure that with separated regions they find every slot start in its
  class, guard slabs included (`lemma_slot_addr_found`, `:989`: the
  stride shift gives the class, the offset is inside the region, and
  the slot test of `Class::slot_of_offset` accepts it), so `hint_ok`
  holds whenever `check_size_hint` returns. The fast path
  (`free_sized_fast`, `heap/paths.rs:647`) pushes only when
  `Shared::sized_slot` found `p` a slot of the hinted class, and a slot
  has one class (`lemma_hint_ok`, from `vspec::zero::lemma_slot_unique`).
  `class_index` ensures `r == spec_class_for(n, 1)`, so the hints
  `free_sized` and `sdallocx` (flags 0) pass are `spec_class_for`'s.
- Flags: `Tcaches::get` (`heap/tcaches.rs:143`) ensures `None` for an
  id of `MAX_TCACHES` or more; `mallocx::alloc`, `alloc_huge` (without
  an explicit arena) and `free` (`heap/mallocx.rs:151`, `:241`, `:280`)
  then reach `invalid_tcache`.
- `new_aligned`: `aligned_alloc_block` returns NULL for a
  non-power-of-two alignment, so `oom` (`api/cxx.rs:174`) never returns
  a block for it: its `nothrow` exit returns NULL, and the throwing exit
  is `-> !`.

**Premises beyond the common ones.** None new. The size-hint rows rest
on the class table premises (`class_entry`, `axiom_class_table_wf`)
through `slab_slot`'s completeness; *(C)* is the `free` contract.

**Scope.**
- NULL: `free`, `free_sized`, `dallocx`, `sdallocx` and the `delete`s
  return at once for NULL (not stated: there is no model of the
  allocator state a no-op leaves alone); `malloc_usable_size(NULL)` is 0
  (proved); `sallocx` and `xallocx` abort for NULL by an explicit test
  (their `free_target(p)` does not say it, as `Regions::wf` does not
  rule out a slot at address 0).
- Before the first allocation, every free or query of a non-NULL
  pointer aborts (`free_before_init`); a thread's first `free` may
  create its heap before the abort (theorem 3).
- `errno` is not modelled (*(errno)* in the interface contract).

- Not checked, by the code either: a huge run freed through a sized
  free with a size that names another usable size. `hint_ok` constrains
  slot starts only, and `check_size_hint` passes every other pointer to
  the exact free, which ignores the size (jemalloc's `nallocx(size) ==
  sallocx(p)` is not checked for huge runs).

**Where the statement is weaker than the code.**
- The *(C)* rows: the run-time checks abort, but the proof excludes
  those inputs by the C contract instead of proving the abort. The
  queries' stale-slot and interior-page aborts are not stated.
- `MALLOCX_TCACHE(t)` with `t < MAX_TCACHES` that names no cache, and
  `MALLOCX_ARENA(a)` that names no arena, abort and return NULL
  respectively by run-time lookups the proofs do not model.
- `rallocx` keeps a block in place without looking at the flags' cache,
  so its `ensures` says nothing of `bad_tcache`; a move allocates and
  frees through `alloc_with` and `mallocx::free`, which check it.
- `new_aligned` with alignment 0: `oom` reads 0 as the unaligned
  operators' marker, so with a `new_handler` installed the retry is
  `malloc`'s and a block can come back; without one, NULL or
  `bad_alloc`. C++ makes the call undefined; the doc comment says so.

## Huge coalescing

Read at this section's commit (the `file:line`s are the `fn`/`spec`
line there). Numbered at merge.

**Statement** (`huge/mod.rs:1172`, `:918`):

```rust
spec fn coalesced(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> bool {
    forall|d1: *mut HugeDesc, d2: *mut HugeDesc|
        descs.contains_key(d1) && descs.contains_key(d2)
            && descs[d1].value().state != RunState::Active as u8
            && descs[d2].value().state != RunState::Active as u8
            ==> descs[d1].value().start + descs[d1].value().pages
                != descs[d2].value().start
}

proof fn theorem_coalesced(&self, d1: *mut HugeDesc, d2: *mut HugeDesc)
    requires
        self.wf(),
        self.descs@.contains_key(d1),
        self.descs@.contains_key(d2),
        self.descs@[d1].value().state != RunState::Active as u8,
        self.descs@[d2].value().state != RunState::Active as u8,
    ensures
        self.descs@[d1].value().start + self.descs@[d1].value().pages
            != self.descs@[d2].value().start,
```

(Line breaks and triggers dropped.) Huge free extents are maximally
coalesced: in every value the huge lock admits, no free extent
(`Empty` or `Purged`) ends where another starts. Together with the
existing `wf` conjuncts (live extents pairwise apart, inside
`next_page`), every maximal stretch of pages covered by free extents is
exactly one descriptor. This is the "adjacent free extents" check of
`Huge::audit` (`huge/mod.rs:4576`, the assert in its loop), now a
conjunct of `Inner::wf` (`:1129`), the huge lock's invariant.

**How it is kept.** `wf` gains two conjuncts: `coalesced`, and
`free_marked` (`:932`), which puts every page of every free extent in
the free-page bitmap's view.
- `Huge::find` (`:3869`) now ensures that it returns fresh pages (no
  extent) only when no free extent ends at the mark (`none_ends_at`). Its
  loop holds that every free extent touching the mark starts at or past
  `from`; `free_marked` puts that start in the bitmap, so
  `lowest_set_from` cannot skip it, and when the scan runs out of set
  bits there is none. `take_ok` (`:1802`) carries the fact into `take`.
- `Huge::take` (`:3979`): `Take::touch_ok` (`:2038`) records where each
  free piece lies: the head ends where the run starts, a tail runs from
  the run's end to the end of the extent it was cut from, and a hole
  runs from the mark, which no free extent touches, to the run.
  `lemma_took_coalesced` (`:2500`) does the case analysis; `took_free`
  states the bitmap after the take: what was set outside the run, plus
  `[mark, run start)`.
- `Huge::free_run` (`:5642`): `free_before` and `free_after` (`:1447`,
  `:1475`) now also ensure completeness: any free extent the table keys
  that ends (starts) at the given page is the result, so the neighbour
  search misses none. `lemma_free_keyed` (`:5519`) shows every free
  extent is still under its keys once the run has left the table, so
  `Merge::nbrs_ok` (`:4795`) holds: without `pred` no free extent ends
  at the run's start, and without `succ` none starts at its end.
  `lemma_freed_coalesced` (`:5040`) concludes for the merged extent.
- `purge_extent` (`:3492`) turns an `Empty` extent `Purged`: still free,
  in the same place.
- `Huge::new` starts with no descriptors.

**Findings.** None. `take`'s alignment holes were the suspected case:
the hole is a fresh `Purged` extent at the old mark, and it could touch
a free extent ending there. It cannot, because `find` returns no extent
only after its scan ran out of free pages, and the scan stops at a free
extent touching the mark, returning it or `None`. Merges of `Empty` and
`Purged` neighbours are coalesced: `free_run` merges both states.

**Premises beyond the common ones.**
- `vspec::huge::axiom_huge_lock_inv` (`vspec/huge.rs:35`): the lock
  protects `Inner::wf`, now including the two new conjuncts.
- `class::Set::ensure` (`class/sets.rs:464`, `external_body`) leaves the
  bitmap view unchanged; `alloc_aligned` calls it between `find` and
  `take`. The view's `set_range`, `clear_range` and `lowest_set_from`
  are verified (`bitmap.rs`).

**Where the statement is weaker than the code.**
- It speaks of descriptors, not pages: it does not state that a free
  page belongs to a free extent (the converse of `free_marked`), which
  `audit` also checks.
- It is stated at the lock invariant only. No entry point exports it.
- The `Lowest-address huge placement` candidate (`find` returns the
  lowest fit) is not proved; only that fresh pages mean no free extent
  touches the mark.

## 8. Pressure never lengthens decay

Read at this section's commit (the `file:line`s are the `fn` line
there).

**Statement** (`policy.rs:452`):

```rust
pub proof fn theorem_pressure_never_lengthens_decay(base: u64, limit: u64, c1: u64, c2: u64)
    requires
        c1 <= c2,
    ensures
        spec_band(limit, c1).wf(),
        spec_band(limit, c2).wf(),
        spec_band(limit, c1).le(spec_band(limit, c2)),
        spec_effective_decay_ns(base, spec_band(limit, c2))
            <= spec_effective_decay_ns(base, spec_band(limit, c1)),
```

(Line breaks added.) For one effective limit `limit` and one configured
period `base` (nanoseconds, `NEVER` included), more usage never gives a
lower band and never a longer effective decay period. The cgroup
policy's table (`policy.rs` module doc) is therefore monotone: as
`memory.current` rises toward the limit a period above 1 s stays, shrinks
linearly to 1 s across 70% to 90%, then drops to 0, and never goes back
up while usage keeps rising.

`Band::le` (`policy.rs:319`) is the table's row order `Low` < `Scaled`
< `Immediate`, by position within `Scaled`; `Unlimited` compares only
with itself (for `limit == 0` or `UNLIMITED` both bands are
`Unlimited` and the periods are equal). Band monotonicity alone is
`lemma_band_monotone` (`:397`), and the period's monotonicity in the
band is `lemma_decay_monotone` (`:421`), for any two well-formed bands
in that order.

The spec functions are the code's arithmetic exactly, for every input:
`usage_ppm` (`:214`) ensures `ppm == spec_usage_ppm(limit, current)`,
`band` (`:471`) ensures `b == spec_band(limit, current)` and
`effective_decay_ns` (`:561`) ensures `ns ==
spec_effective_decay_ns(base, band)`. `scaled_ppm` (`:261`) models
`usage_ppm`'s loop, which halves usage and limit together until the
limit fits 43 bits: the number of halvings depends on the limit alone,
so it is the same for both usages, and halving keeps their order
(`lemma_scaled_ppm_monotone`); `spec_usage_ppm` covers every limit,
past `2^43` bytes included. `frac16`'s exact contract (`den <= 2^47`)
covers `band`'s constant denominator 200 000, and `mul16`'s is exact.

**Entry points.** None: no C symbol's `ensures` changes. The theorem is
about two pure functions: `band`, which `evaluate` (`:853`, from
`purge::on_tick` and the background thread) and `Shared::init`
(`heap/shared.rs`) call, and `effective_decay_ns`, which
`Policy::effective_decay_ns` (`:740`) applies to the stored band for
the decay walks (`purge.rs`), `stats.cgroup.decay_ms`
(`api/mallctl.rs`) and the reports (`api/print.rs`).

**Premises beyond the common ones.** None: the proof uses no axiom,
`external_body` or `assume_specification`; the three functions and
`frac16`, `mul16` verify in place. The common premises are not used
either.

**Scope and where the statement is weaker than the code.**
- One limit. `evaluate` passes `min(memory.max, memory.high)` up the
  hierarchy as sampled; when that changes between two samples (`docker
  update`, a write to `memory.high`), the two bands are not comparable
  by this theorem, and a higher limit at the same usage does lengthen
  the period, as it should.
- Pure functions only. `Policy` stores the band as atomics
  (`frac16`, `immediate`, `limited`; `Policy::apply`) and
  `Policy::band` (`:703`) rebuilds it: that round trip is not proved
  (`position` is `external_body` because Verus has no view of an
  `AtomicU32`'s value), and it maps `Scaled(0)` back to `Low`, which
  gives the same period. Concurrent `evaluate` calls or a reader between
  `apply`'s stores (a torn `limited`/`frac16`/`immediate` triple) are
  not covered, nor is what the decay walks do with the period.
- Monotone in the usage sample, not in time: two samples taken in the
  other order (usage falling) lengthen the period again, by design.
- `NEVER` (`dirty_decay_ms:-1`) stays `NEVER` in every band; the
  theorem holds there with equality.
- The test `policy::tests::rising_usage_never_lengthens_the_period`
  samples the period half of the theorem at runtime, including limits
  past `2^43`.

## Interface contract

Theorems 1-7 above checked against challoc's exported C ABI (theorem 10
is the I6 row below): one row per exported symbol (`je_` twins and C++
array forms share their row), one column per interface property. Read
from the `ensures` of each entry point's Rust function at `ccbf1dd`;
the `file:line`s in this section are the `fn` line at that commit (the sections above are
at `b2cad68`). The U column, row 13's Z and row 22's F were revised
later, by the usable-size work (gap 3 below), and the R cells of rows 3,
4, 14 and 17 by the huge `kept` bound (gap 6); items they added are
cited by name. All the trusted premises listed above apply to every
cell. Every `extern "C"` wrapper is in `verus!` with no contract of its
own and only casts, except `malloc_stats_print`. So the Rust function's
`ensures` is the symbol's contract, and a verified body cannot panic or
overflow. Where that function is `.0` of a `_zeroed` twin (`calloc`,
`challoc_calloc_usable`, `mallocx`), the Z cell rests on the twin's
`ensures`: the zero token is ghost, so the exported function returns
the same block. Likewise the U cells of rows 12 and 14 rest on the
`_stored` twins' `ensures` (`malloc_usable_stored`,
`realloc_usable_stored`) and row 13's on `calloc_usable_zeroed`'s third
result: `malloc_usable` and `realloc_usable` themselves state nothing
about `*usable`. The D cells, the *(live)* B cells, row 15's U and row
17's Z rest on a
`_block` core (`malloc_block`, `free_block`, ...; for rows 12 to 14 the
`_stored` twins and `calloc_usable_zeroed`, which take the token too)
that the Rust entry point calls with a fresh `Live` token (allocations:
the token goes to C with the block) or with the one `axiom_c_claim`
gives for C's pointer (frees); the token is ghost, so the core is the
entry point's code, and its `ensures` is the symbol's contract for those
cells. They were updated for gap 1 (below); their `file:line`s are at
the commit that did it.

Properties:

- **R**: the result fits and is aligned (`heap::fits`, `heap/mod.rs:167`);
  since theorem 7 a slab result is never in a guard slab.
- **D**: the result is disjoint from every other live block: the core
  returns the token of the block (`Live::gives`), and two live tokens
  are disjoint (`Live::theorem_apart`, theorem 5).
- **Z**: the result is zeroed where the API promises it.
- **C**: contents are preserved (realloc family).
- **F**: failure semantics: NULL, `errno`, return code, the old block
  intact after a failed realloc, overflow in `calloc`/`reallocarray`.
- **B**: bad input (invalid or double free, bad alignment) aborts or is
  a documented no-op. It never corrupts silently. Since theorem 7 a
  free target is never in a guard slab, so a free into one aborts. The
  *(live)* cells (a stale or double free) stay `p`: the free cores
  require the token of a live block and ensure that what they freed was
  live, but C holds that token only by `axiom_c_claim` (the `free`
  contract), so a non-live free is excluded by that C-boundary
  assumption rather than proved to abort. The "Bad input" section above
  lists every kind of bad input by entry point, with what is proved to
  abort (pointer shape, size hints, alignments, cache ids) and what
  rests on the C contract.
- **U**: size consistency: `nallocx` equals `mallocx`'s usable size;
  `sallocx` and `malloc_usable_size` give the block's real usable size.

Codes: `P` proved, `p` partial, `-` missing, `.` not applicable.

| #  | Entry point                                 | R | D | Z | C | F | B | U |
|----|---------------------------------------------|---|---|---|---|---|---|---|
| 1  | `malloc`                                    | P | P | . | . | p | . | . |
| 2  | `calloc`                                    | P | P | P | . | p | . | . |
| 3  | `realloc`                                   | P | P | . | - | p | p | . |
| 4  | `reallocarray`                              | P | P | . | - | p | p | . |
| 5  | `aligned_alloc`, `memalign`                 | P | P | . | . | p | P | . |
| 6  | `posix_memalign`                            | p | p | . | . | p | P | . |
| 7  | `valloc`                                    | P | P | . | . | p | . | . |
| 8  | `pvalloc`                                   | p | P | . | . | p | . | . |
| 9  | `malloc_usable_size`                        | . | . | . | . | P | p | p |
| 10 | `free`                                      | . | . | . | . | - | p | . |
| 11 | `free_sized`, `free_aligned_sized`          | . | . | . | . | - | p | . |
| 12 | `challoc_malloc_usable`                     | P | P | . | . | p | . | P |
| 13 | `challoc_calloc_usable`                     | P | P | P | . | p | . | P |
| 14 | `challoc_realloc_usable`                    | P | P | . | - | p | p | P |
| 15 | `challoc_free_usable`                       | . | . | . | . | P | p | P |
| 16 | `mallocx`                                   | P | P | P | . | p | p | . |
| 17 | `rallocx`                                   | P | P | P | - | p | p | . |
| 18 | `xallocx`                                   | . | . | . | . | . | p | p |
| 19 | `sallocx`                                   | . | . | . | . | . | p | p |
| 20 | `dallocx`                                   | . | . | . | . | . | p | . |
| 21 | `sdallocx`                                  | . | . | . | . | . | p | . |
| 22 | `nallocx`                                   | . | . | . | . | P | . | P |
| 23 | `operator new`, `new[]`                     | P | P | . | . | p | . | . |
| 24 | `new`, `new[]` (`nothrow_t`)                | P | P | . | . | p | . | . |
| 25 | `new`, `new[]` (`align_val_t`, 4 forms)     | P | P | . | . | p | p | . |
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

- *(D)*: the entry point's `_block` core ensures `final(blk).gives(r,
  size)`: for a non-null result, `blk` is the token of the live block at
  `r` (`Live::block`) with at least the request's usable bytes. Two live
  tokens are disjoint (`Live::theorem_apart`, `heap/live.rs:423`), and a
  cached slot is no live block (`Heap::theorem_uniqueness_live`,
  `heap/mod.rs:3021`), so the result shares no byte with any other
  block the program holds, under the `free` contract for C's blocks.
  Slot tokens come from the class authorities (theorem 5); run tokens
  from the huge allocator's (`huge::RunToken`, `huge/mod.rs:652`), in
  place of the former uninterpreted `huge::allocated`.
- *(errno)*: `set_errno` (`api/libc.rs:83`, `api/print.rs:1045`) is
  `external_body` with no `ensures`, and no spec models `errno`. A
  "NULL with `ENOMEM`" contract is proved only for its NULL half
  (`enomem`, `einval`, `api/libc.rs:91`, `:102`, ensure `r@.addr == 0`).
  Nothing proves that the failure paths are the only NULL returns, or
  that `free` preserves `errno`.
- *(live)*: `free_target` (`heap/mod.rs:432`, theorem 3) is proved. A
  pointer that is not a slot start or a huge page start aborts. A free
  core takes the caller's token for `p` (`Live::claims`) and ensures
  `p@.addr != 0 ==> blk.block(p)`: what it freed was a live block, whose
  token it gave back to the cache or the bitmap word (slots) or the run
  authorities (runs). A non-live pointer that is a slot or huge page
  start has no token in C's hands; the boundary's `axiom_c_claim` (the
  `free` contract) makes such a call's context inconsistent, and the
  run-time checks (`SlabTable::check_live`, `class/table.rs:1540`, the
  cache flush's release, `Huge::free_run`) stay in the code for it. So
  the cell stays `p`: a non-live free is excluded by the C-boundary
  assumption, not proved to abort.
- *(U)*: the size specs are in `vspec/usable.rs`. `usable_is(p, size,
  n)` is the usable size of a block an allocation of `size` bytes
  returned: the class's size for a slot, `huge_usable(size)` (`size`
  rounded up to whole pages) for a huge page start, the ends
  `covers_usable` names. `usable_of(p, n)` is what a query reads for a
  live block: the class's size for a slot; for a huge page start only
  `run_bytes(p, n)`, the descriptor's shape (whole pages, at least one,
  guard inside the region). `usable_size` ensures `usable_of`,
  `malloc_usable_size`, `sallocx` and `xallocx` re-export it, and
  `free_usable` states it from its own classification. So a query of a slot is exact, and
  `vspec::usable::theorem_usable_consistent` proves that it reads what
  the allocation stated (`r == n` for a slot). With a token that claims
  `p`, `usable_size` also ensures `r == blk.len()` (`Live::len`, the
  block's usable bytes, a run token's usable pages included). The
  queries (`malloc_usable_size`, `sallocx`, `xallocx`) take no token, so
  their huge half is not tied to the request: `p` for rows 9, 18 and 19.
  `challoc_free_usable` takes the freed block's token and states `r ==
  blk.len()` too (row 15). The allocating `_usable` entry points are
  `.0` of `_stored` twins (`malloc_usable_stored`,
  `realloc_usable_stored`; `calloc_usable_zeroed`'s third result) that
  return the `Stored` token of `store_usable` (`external_body`, its
  ensures `s@.ptr() == usable && s@.val() == n`), the last store through
  `usable`. They state `usable_is(r, size, s@.val())` for a non-null
  result. For a huge run they read the size back with `usable_size`,
  right after `alloc_huge`, with the run token `alloc_huge` returned,
  whose usable bytes are `huge_usable(size)` (`alloc_huge`'s `ensures`:
  the descriptor `Huge::take` wrote takes `usable_pages(size)` pages
  plus the guard); this replaces the former trusted
  `vspec::huge::axiom_owned_run`.

Per row (cells marked `P` without a note are the cited `ensures`
alone):

1. `malloc` (`api/libc.rs:213`). R: `fits(r, size, QUANTUM)`. D: *(D)*.
   F: *(errno)*. Not stated: NULL only on exhaustion; `malloc(0)` is
   non-NULL (the module doc promises a distinct slot).
2. `calloc` (`api/libc.rs:328`, `.0` of `calloc_zeroed`, `:341`). R:
   `fits(r, n * m, QUANTUM)`. Z: `r.1@.covers(r.0, n * m)` (theorem 6).
   F: `n * m > usize::MAX ==> r@.addr == 0` is proved; the rest is
   *(errno)*.
3. `realloc` (`api/libc.rs:698`). R: `fits(r, size, QUANTUM)`, a kept
   block included (gap 6: `kept`, `:617`, now bounds a kept run by
   `size`, and unfolds to `fits`, `lemma_kept_fits`). D: *(D)*
   (`realloc_block`); a kept `p` comes back with the caller's own token.
   The move's disjointness from `p` is proved (`Live::theorem_apart` on
   the two blocks' tokens), and `axiom_moved_block` (`vspec/addr.rs:121`)
   grants only the two blocks' validity, with the disjointness as its
   precondition. C: the code copies `min(old, size)` bytes, with
   `usable_of(p, old)` and `old == blk.len()`, the block's usable bytes
   (`old_size_or_keep`, `:635`): the class size for a slot, a run token's
   usable pages for a huge `p`.
   `ptr::copy_nonoverlapping` has no byte-level spec, so contents are not
   modelled. F: `p != 0 && size == 0 ==> r@.addr == 0` is proved, and the
   core (`realloc_block`) gives the caller back its own token for `p` on a
   NULL from the move (`p` is still live; the code returns before
   `heap::free`). B: `p != 0 ==> free_target(p)`, *(live)*.
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
   D: `posix_memalign_block` ensures, on a `0` return, the token of a
   block of at least `size` bytes aligned to `align`, but not that it is
   the one stored through `out` (`store_out`), hence `p`.
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
11. `free_sized`, `free_aligned_sized` (`api/libc.rs:580`, `:591`). B:
    `free_target(p)`, *(live)*. The size hint is proved against the
    slot's class: `free_sized` ensures `p != 0 ==> hint_ok(p,
    spec_class_for(size, 1))` and `free_aligned_sized` the same for
    `spec_class_for(size, align)`, or `None` for a non-power-of-two
    alignment, so a slot freed with another class's size, or with a bad
    alignment, aborts ("Bad input"). The slow path checks it
    (`Shared::check_size_hint`, which ensures `hint_ok`); the fast
    path (`free_sized_fast`) pushes only a slot of the hinted class, and
    its liveness is checked when the cache flushes. F: as `free`.
12. `challoc_malloc_usable` (`api/libc.rs:798`, `.0` of
    `malloc_usable_stored`). R, D, F: as `malloc`. U: the `Stored` token
    of the last store through `usable` has `ptr() == usable` and, for a
    non-null result, `usable_is(r, size, val())`: the class's size, or
    `size` rounded up to whole pages for a huge run (*(U)*, from the
    run token `alloc_huge` returns). That the class is `nallocx(size, 0)`'s (the
    smallest that holds `size` bytes) is true of `class_index` but not
    exported here. Not stated either: the 0 stored for a null result
    (`Heap::alloc_fast`'s pop states a slot, and the regions' spec does
    not rule out a slot at address 0).
13. `challoc_calloc_usable` (`api/libc.rs:890`, `calloc_usable_zeroed`,
    `:905`). R, F: as `calloc`. Z: the token covers `n * m` bytes and
    `covers_usable(r.1, r.0, 0, n * m)`, the whole usable size the doc
    promises and the code zeroes (`write_zero(p, u)`, or the known-zero
    token of `heap::alloc_zeroed` or `alloc_huge`). For a huge run the
    `memset` length `u` is the usable size of the block's own token
    (`usable_size` with the token `alloc_huge` returned,
    `huge_usable(n * m)`), so no trusted premise remains
    (`axiom_owned_run` is gone). U: as row 12, for `n * m`, and the null
    result's 0 is stated.
14. `challoc_realloc_usable` (`api/libc.rs:969`, `.0` of
    `realloc_usable_stored`). R, D, C, F, B: as `realloc` (the keep
    decision is `realloc_old`, `:1010`). U: as row 12. A kept block's
    size is `realloc_old`'s, which ensures `usable_of(p, old)` and, when
    kept, `usable_is(p, size, old)` (for a run, `usable_for(size) ==
    Some(old)` is the keep test); a moved block's is
    `malloc_usable_stored`'s.
15. `challoc_free_usable` (`api/libc.rs:1055`). F: `p == 0 ==> r == 0`.
    B: as `free` (`free_usable_block`). U: `p != 0 ==> usable_of(p, r)`,
    *(U)*, and `r == blk.len()`, the freed block's usable bytes (the
    class size, or the run's usable pages, from its token on both paths):
    the class size from `heap::free_fast_class`, which ensures
    `is_slot(c, p)`, or from `free_usable_slow`'s classification
    (`:1081`), or `Huge::usable_size` there. The `r == blk.len()` half
    is the core's; at the export `blk` is the token `axiom_c_claim` gives
    for C's `p`, so for a huge run it holds under the `free` contract
    (the `usable_of` half needs no token).
16. `mallocx` (`api/jemalloc.rs:231`, `.0` of `mallocx_zeroed`, `:243`).
    R: `fits(r, size, 1 << (flags & 0x3f))`. With no `MALLOCX_LG_ALIGN`
    this is alignment 1, weaker than the quantum the code gives. Z: with
    `MALLOCX_ZERO`, `covers(r, size)` and `covers_usable(.., 0, size)`
    (`vspec/zero.rs:109`, the whole usable size; theorem
    `vspec::zero::theorem_zeroed_memory`, `:240`). Also, for a non-NULL
    result, `usable_is(r, size, nalloc_usable(size, flags & 0x3f))` (row
    22). F: NULL on failure
    with `errno` untouched (`alloc_with`'s doc) is not stated beyond
    `fits` allowing NULL. B: `tcache_checked(size, flags)`: an explicit
    cache id of `MAX_TCACHES` or more aborts, except for a huge request
    with an explicit arena, which never looks the cache up; every
    `MALLOCX_LG_ALIGN` gets an aligned block or NULL (R). An arena that
    does not exist returns NULL and a cache id below `MAX_TCACHES` that
    names no cache aborts, with the same huge exception (`alloc_with`'s
    doc); neither is stated, as existence is a run-time table load.
17. `rallocx` (`api/jemalloc.rs:260`, `rallocx_zeroed`, `:278`). R:
    `fits(r, size, 1 << (flags & 0x3f))`, a kept block included, as
    `realloc` (gap 6). Z: for a new or moved block with `MALLOCX_ZERO`,
    a token from offset `k` to the usable end (`covers_usable`). `k == 0`
    for a NULL `p`; for a non-NULL `p`, `k == min(old, size)` for an
    `old` with `usable_of(p, old)` (`old_size_or_keep` ensures it of the
    `old` it returns), so `min(class size, size)` for a slot `p`. For a
    huge `p`, `usable_of` gives only the shape of `old` (`run_bytes`:
    whole pages, at least one, inside the region), not the descriptor's
    value the code copies; `rallocx_zeroed` also ensures
    `k == min(blk.len(), size)`, the usable bytes of `p`'s token
    (`old_size_or_keep` ensures `old == blk.len()`, gap 1), which is the
    old run's usable size. So Z is `P` under the `free` contract, as D:
    at the export, `blk` is the token `axiom_c_claim` gives for C's `p`.
    A kept block gets no `Zeroed` token; jemalloc zeroes only past the old real
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
20. `dallocx` (`api/jemalloc.rs:440`). B: `p != 0 ==> free_target(p)`,
    *(live)*. With `MALLOCX_TCACHE_NONE` the release tests the bit
    immediately (**unconfirmed**, from the doc comment). An explicit
    cache id of `MAX_TCACHES` or more aborts (`!bad_tcache`, "Bad
    input").
21. `sdallocx` (`api/jemalloc.rs:481`). B: `p != 0 ==> free_target(p)`,
    *(live)*. jemalloc's `nallocx(size, flags) == sallocx(p)` contract
    is checked as in row 11, for slots: `p != 0 ==> hint_ok(p,
    spec_class_for(size, 1 << (flags & 0x3f)))` (`flags == 0` is
    `free_sized`, and `sdallocx_flags`/`sdallocx_routed` hint
    `class_for(size, align)`); a huge run's size is not checked. An
    explicit cache id of `MAX_TCACHES` or more aborts (`!bad_tcache`).
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
25. `new` `align_val_t` (`api/cxx.rs:260`). R: `is_pow2(align) ==>
    fits(r, size, align)`. F: as rows 23 and 24 by `nothrow`. B: a
    non-power-of-two alignment (undefined in C++) makes `aligned_alloc`
    return NULL, so the handler loop runs; `new_aligned` ensures
    `align != 0 && !is_pow2(align) ==> r@.addr == 0` (NULL for the
    `nothrow_t` forms, `bad_alloc` for the others). `p`: alignment 0 is
    `oom`'s marker for the unaligned operators, so with a `new_handler`
    installed the retry is `malloc`'s and a block can come back.
26. Unsized `delete` forms, with or without `align_val_t` or `nothrow_t`
    (`cxx::delete`, `api/cxx.rs:256`). B: as `free`; the alignment is
    ignored.
27. Sized `delete` (`cxx::delete_sized`, `api/cxx.rs:302`). B: as
    `free_sized`, `hint_ok` included.
28. Sized aligned `delete` (`cxx::delete_sized_aligned`,
    `api/cxx.rs:314`). B: as `free_aligned_sized`, `hint_ok` included.
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
| I3 uniqueness (no slot handed out twice) | P    | theorem 5              |
| I4 guard pages never handed out (slabs)  | P    | th. 7 (section 7)      |
| I4 guard pages never handed out (huge)   | p    | th. 7 (section 7)      |
| I5 purge never discards live data        | -    | below                  |
| I6 no cached slot lost at thread exit    | p    | th. 10 (section 10)    |

- I1, I2: for every result an entry point states `fits` for, with the
  premises and the "not covered" lists of sections 1 and 4.
- I3: a slot or run is unique while it is cached, held by a bitmap word,
  in flight, or live (theorem 5, *(D)* above): every holder has its
  token, and two tokens of one authority are of different slots or of
  disjoint pages. The model ends at the C boundary: a block C frees
  gets its token from the `free` contract (`axiom_c_claim`).
- I4: theorem 7 (lines as in section 7). For slabs, `Regions::is_slot`
  excludes the guard slabs, so every `fits` (the R column) and every
  `free_target` (the B column) states it per result: `theorem_guard_pages` and
  `theorem_guard_free` (`heap/mod.rs:212`, `:243`). For huge runs it is
  stated only of the huge lock's values (`Inner::theorem_run_guards`,
  `huge/mod.rs:941`): `alloc_run` returns the run's token (gap 1), but
  no lemma states the guard exclusion per result over it, and
  fallback-mode stale `PROT_NONE` guards are not modelled.
- I5: class purges run only on slabs in the `empty` set, and
  `walk_empty` (`class/decay.rs:265`) checks at run time that the
  descriptor says `Empty` with `live() == 0`, aborting otherwise
  (`not_empty`, `:315`). `page_group_is_idle` (`:207`) is trusted. No
  invariant ties `live() == 0` to "no slot of the slab is allocated or
  cached". `purge::Batch`'s "ranges are free" (`purge.rs:218`) is a doc
  comment; `Batch::wf` states only bounds. Theorem 6's
  `axiom_discarded_zero` premises lean on the same unproved fact.
- I6: theorem 10 (`Heap::flush_all`, `heap/mod.rs:3891`): the flush
  `destroy` runs first empties every home and node header and appends
  exactly what they held to the released log, each slot's token handed
  to `release_routed` with it. `destroy` itself is trusted, and section
  10's "weaker" list applies, hence `p`.

### Gaps, by cells turned to `P`

1. **Live-block ghost model**: done. The allocation cores hand the caller
   the block's `Live` token (the slot's, or a `RunToken` from the huge
   lock's run authorities in place of the uninterpreted
   `huge::allocated`), every free core consumes one, and the C boundary
   is the one `axiom_c_claim`. The D column (15 of 16 rows; row 6's
   token is not tied to `*out`), I3 and row 15's U are `P`. The
   *(live)* B cells of the rows that free (12 rows; the queries 9, 18,
   19 take no token) stay `p`: their cores require and consume the live
   block's token, but a non-live free is excluded by the C-boundary
   assumption (`axiom_c_claim`), not proved to abort. `axiom_free_token`
   and `axiom_owned_run` are gone, and `axiom_moved_block` no longer
   states the disjointness, which its callers prove (it was unsound for
   `p == q`).
2. **`errno` as ghost state**: give `set_errno` an `ensures` over a
   ghost per-thread `errno`, and add `r@.addr == 0 <==> failure` to the
   allocating entry points. This turns the F cells of rows 1-8, 12-14
   and 32 (and `free`'s preservation, rows 10, 11) to `P` or nearly so.
   Mostly spec work; the hot paths keep their code.
3. **Usable-size contract**: done for rows 12, 13, 14 and 22 (U to `P`)
   and row 13's Z, *(U)*; row 15's U is `P` from gap 1 (the freed
   block's token, under the `free` contract for a huge run).
   `usable_size` states both halves (`usable_of`), `malloc_usable_size`,
   `sallocx` and `xallocx` re-export it, and `challoc_free_usable`
   states it from its own classification; `*usable` is a `Stored` token from `store_usable`;
   `nallocx` and `mallocx` share `nalloc_usable`. Left: the huge half
   of a query (rows 9, 18, 19 stay `p`): `usable_size` with the block's
   run token gives exactly its usable bytes (gap 1), but the queries
   take no token (giving them one would add an `axiom_c_claim` site for
   a call that frees nothing); and the 0 that `challoc_malloc_usable`
   and `challoc_realloc_usable` store for NULL (a slot at address 0 is
   not ruled out by `Regions::wf`).
4. **Failure-path contracts**: `r@.addr == 0 ==> p` still live for the
   realloc family is now in the cores (`realloc_block`,
   `realloc_usable_stored`, `rallocx_zeroed` give back the caller's
   token); what is left is `posix_memalign`'s `*out` through a `PointsTo`
   for `store_out`, and `!nothrow ==> r@.addr != 0` for `operator new`
   (provable from `oom`'s loop today). Rows 3, 4, 6, 14, 17, 23-25.
5. **Byte contents**: a `PointsToRaw`-level spec of user blocks and of
   `copy_nonoverlapping` would turn the C column (4 rows) to `P`. It
   needs the permissions threading that `TODO.md` lists.
6. **Huge `kept` bound**: done for R (rows 3, 4, 14 and 17 to `P`).
   `Huge::usable_size` already ensures `spec_run_usable` from the run's
   descriptor (usable pages and guard inside the region);
   `vspec::usable::lemma_run_bytes_block` puts any `size <= old` of a
   huge page start in the region, so `kept`'s huge half carries
   `is_huge_block(p, max(size, 1))` and every realloc-family result
   `fits` (`kept` unfolds to it, `lemma_kept_fits`). Ghost-only: no
   machine code changed. Z of row 17: `old_size_or_keep` now ensures
   `usable_of(p, old)`, so `rallocx`'s `k` is `min(old, size)` for some
   `old` of a run's shape; `usable_of` does not name the descriptor's
   value, but with gap 1 `old_size_or_keep` also ensures `old ==
   blk.len()` and `rallocx_zeroed` ensures `k == min(blk.len(), size)`,
   so Z is `P` under the `free` contract (merge of gaps 1 and 6).
7. **Purge and guards**: tie the empty set and `purge::Batch` ranges to
   slot and extent tokens (I5, and theorem 6's purge premise). The guard
   half is done for slabs (`!guard_slab` is in `is_slot`, theorem 7);
   for huge runs, `Huge::alloc_run`'s result now comes with its run
   token (gap 1); state `theorem_run_guards` per result over that token
   (I4, huge row).
8. **Bad input**: done as far as the C contract allows ("Bad input"
   above). Rows 16 and 25 go from `-` to `p` (cache ids of
   `MAX_TCACHES` or more abort; a non-zero non-power-of-two `align_val_t`
   gets no block), and rows 11, 21, 27 and 28 gain `hint_ok`: a slot
   freed with another class's size aborts. The *(live)* cells stay `p`:
   a stale or double free of a slot or huge page start is excluded by
   `axiom_c_claim`, not proved to abort. Left: model arena and cache
   existence (rows 16, 17, 20, 21) and tie a huge run's size to the
   sized frees' hint, which the code does not check either.
