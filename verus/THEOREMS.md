# Whole-allocator theorems: statements and premises

Reference for the twenty-three whole-allocator theorems as the code
states them: the Verus statement, the entry points it covers, and every
trusted premise it rests on, with `file:line`. Read at `b2cad68`
(branch `verus`); line numbers are the `fn`/`axiom` line at that commit,
except where a section names its own commit.
`verus/TODO.md` ("Theorem candidates") carries the history and the
open work; this file says what the code proves today. Each title and
first paragraph claims only what is proved: where a theorem holds only
at a lock invariant, rests on a source test or on the C-boundary
assumption (`axiom_c_claim`), the title or the first paragraph says so.
The sections are numbered 1 to 23 in order; "Bad input" (the B column)
and the interface contract are not numbered.

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
and the call does not return (theorem 3, which is not voided for it).

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
  and `set` makes the written value the token's (theorem 15), nothing
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

## 3. Invalid free (pointer shape)

What is proved is the pointer's shape, not its liveness: a free or a
query that returns normally was given NULL (where allowed), a slot start
or a huge page start. Rejection of a slot or huge page start that is not
a live block (a stale or double free) is not proved: the C-boundary
assumption `axiom_c_claim` (the `free` contract, common premises)
excludes such a call. And "aborts" below means the proof shows the call
cannot return normally, not that it reaches `SIGABRT` rather than
hanging: the paths before an abort may wait on a lock, whose
termination is not proved (common premises, "Termination not proved").

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
only for a slot start or a huge page start; for anything else the call
does not return (the code aborts). Since theorem 7, `is_slot` excludes
guard slabs, so a call with a pointer into one does not return either
(`theorem_guard_free`).

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
  `heap::free`, `Heap::free`). For a slot or huge page start that is not
  live, C has no token, and the boundary's `axiom_c_claim` (the `free`
  contract, common premises) makes the context inconsistent instead of
  proving the abort.
- The run-time guarantee for non-live block starts, read from the code
  and not proved: an unsized free, `realloc`, `malloc_usable_size`,
  `sallocx` or `challoc_free_usable` of a slot whose bitmap bit is clear
  at the load (`SlabTable::is_set_unchecked`, `class/table.rs:1288`)
  aborts; a sized free pushes untested, and the cache flush catches it
  unless a `malloc` pops the slot first (a pop tests no bit); a non-live
  huge page start aborts in `Huge::free_run`. The residue: a free of a
  slot whose first copy is still in a cache, a thread's or an explicit
  tcache (its bit is still set; caught, if at all, by a later flush, or
  at the call by `double_free_scan` when it is among the top four
  entries of the thread cache it is pushed to), and a free of a block
  that was freed and handed out again (ABA), which no allocator can tell
  from a live one. "Bad input" below lists them by
  entry point.
- A thread's first `free` may create its heap before the abort.

## 4. User blocks are disjoint from allocator metadata

A spatial statement (named "metadata integrity" in `TODO.md` and the
code's comments): no byte of a non-null block an allocation returns
lies in the metadata listed below, the `Shared` static included. It does
not prove that every allocator write stays inside the object it is meant
for, nor that a returned byte is mapped with the right permissions.

**Statement** (`heap/mod.rs:317`, read at the base of the theorem
restatements, `7d4e997`):

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
            &&& forall|x: int| #[trigger] shared.meta.is_link(x)
                ==> disjoint(b, n, x, 8)
            &&& disjoint(b, n, crate::layout::shared_static_addr(),
                         crate::layout::shared_static_len())
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
`layout/regions.rs:912`) plus `lemma_user_block_apart` (`:921`), except
two. `Meta`'s free-list link words (`Meta::is_link`: the link word of
every free object in every value the metadata lock admits, the one word
`Meta` writes outside what it hands out) lie in what it holds
(`Meta::lemma_link_held`, from `meta::Inner::lemma_links_in_region`).
The `Shared` static lies outside all three reservations
(`Regions::outside`, a conjunct of `Regions::separated`;
`lemma_user_block_outside`).

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
- The `Shared` static: `Reserved::is_apart_from_static`
  (`layout/regions.rs`, trusted, the kernel's `mmap` contract): each of
  the three reservations is apart from the loaded object's data, which
  `from_parts` turns into `Regions::outside`.
- `huge::table::axiom_zero_entry` (`huge/table.rs:174`): all-zero bytes
  are an empty descriptor table entry (`Table::storage`).
- User-block writes: `vspec::addr::axiom_fresh_block`,
  `axiom_moved_block`, `axiom_slot_writable`, `axiom_huge_writable`
  (`vspec/addr.rs:104`, `:121`, `:140`, `:153`) grant validity for any
  length; the theorem covers where a block is, not that every write to
  it stays inside it.

**Not covered.**
- Thread-local storage: not apart from every block, so not in the
  statement. A thread whose stack the program allocated
  (`pthread_attr_setstack` over a `malloc`ed buffer) has its static TLS
  block, and so challoc's heap pointer slot, inside that buffer. (The
  `Shared` static and `Meta`'s link words are conjuncts now.)
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
- Spatial only: the theorem places metadata and blocks; it does not
  prove that each allocator write stays inside the object it is meant
  for (the writes through permissions do, the atomics above are only
  located), nor that the block's bytes are mapped writable.

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

## 6. Zeroed memory (zero-token coverage at return)

What is proved is zero-token coverage at return: the zeroing entry
points return a ghost `Zeroed` token that covers the bytes the API
promises zero. `vspec::zero::theorem_zeroed_memory` (`vspec/zero.rs:242`)
concludes `z.has(a)` for every byte `a` from `from` to the usable end,
and `Zeroed::has` is interval membership. That a load of such a byte
reads zero is the token's meaning, not a proved fact: it rests on the
trusted token sources (`write_zero` and the kernel axioms below) and on
the unproved discipline that nothing writes a token's bytes between its
creation and the return (premises below). There is no byte-state model
whose writes would invalidate a token. Plain `calloc(n, m)` promises,
and its token covers, only its `n * m` requested bytes; bytes past them
up to the usable size are unspecified (#54). `challoc_calloc_usable`,
`mallocx(.., MALLOCX_ZERO)` and `rallocx`'s new tail cover the whole
usable size. (The `vspec/zero.rs` lines in this section are at
`7d4e997`; the table's are at `b2cad68`.)

**Statement.** The `Zeroed` token (`vspec/zero.rs`): `covers(p, n)` is
`addr() == p@.addr && n <= len()`; `covers_usable(z, p, from, size)`
(`vspec/zero.rs:109`) is a token from `p + from` to the block's usable
end (the class size, or `huge_usable(size)`). Postconditions:

| Function (`file:line`) | Token on a non-null result |
|---|---|
| `calloc_zeroed` (`api/libc.rs:319`) | `covers(r.0, n * m)` only |
| `calloc_usable_zeroed` (`api/libc.rs:853`) | `n * m`, and `covers_usable(.., 0, n * m)` |
| `mallocx_zeroed` (`api/jemalloc.rs:220`) | with `MALLOCX_ZERO`: `size`, and `covers_usable(.., 0, size)` |
| `rallocx_zeroed` (`api/jemalloc.rs:251`) | `size - k` bytes from `r + k`, and `covers_usable(.., k, size)` |

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
- Coverage, not contents: the theorem is about the token's interval at
  the moment of return (above). A program's own later writes are its
  business; the gap is challoc's writers between the token's creation
  and the return, which are bounded by inspection only.
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

For slabs, per result: every `fits` and every `free_target` excludes the
guard slabs. For huge runs, only at the huge lock invariant: every value
the huge lock admits keeps live runs' usable pages off every run's
guard page, but nothing is exported per call ("Huge runs" below).

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

**Huge runs (at the huge lock invariant).** Each run's guard follows
its usable pages inside its extent, and live extents are pairwise apart
(`huge::Inner::wf`). `Inner::theorem_run_guards` (`huge/mod.rs:941`)
states the consequence for every value the huge lock admits, and for
nothing per call: the usable pages of a live run
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

## 8. No syscall on the fast path

Read at the lane commit `32171ab`, before its merge onto the live-block
model (the `file:line`s are the `fn` line there).

**Statement** (`sys/mod.rs:103`, `:113`; `api/libc.rs:441`):

```rust
#[verifier::opaque]
pub open spec fn may_syscall() -> bool { true }

pub proof fn grant_syscalls()
    ensures
        may_syscall(),

#[cfg(verus_only)]
fn theorem_no_syscall_on_fast_paths(which: u8, size: usize, p: *mut u8)
```

(Bodies elided.) `may_syscall()` is a capability: every function that
can enter the kernel requires it, and the thread-cache fast paths do
not. The fast paths make no system call. They are, as the entry points
call them:

- `malloc`: `class_index` (`api/libc.rs:120`), `api::shared_ptr`
  (`api/mod.rs:262`), `heap::existing_heap` (`heap/thread.rs:153`: the
  TLS read and `heap_from_slot`), `heap::fast_flags` (`:196`) and
  `Heap::alloc_fast` (`heap/paths.rs:55`), whose body is `pop_fast`
  (`:81`). `calloc`'s cache pop is the same `pop_fast`, and
  `operator new`'s fast path is `malloc_fast` (`api/libc.rs:286`).
- `free`: `existing_heap`, `shared_ptr`, `api::shared_unchecked`
  (`api/mod.rs:273`), `Shared::push_flags` and `heap::free_fast`
  (`heap/paths.rs:533`), with the check (`Shared::checked_slot`) and the
  push (`Heap::push_fast`) inside it.
- `free_sized`, and `sdallocx` with no flags: `class_index`, `shared_ptr`,
  `existing_heap`, `shared_unchecked` and `heap::free_sized_fast`
  (`heap/paths.rs:594`).
- `challoc_free_usable`: `existing_heap`, `shared_ptr`,
  `shared_unchecked`, `heap::free_fast_class` (`heap/paths.rs:564`) and
  `class_size` (`api/libc.rs:890`).

`theorem_no_syscall_on_fast_paths`, a verification-only function
(`cfg(verus_only)`; the build has none), makes these calls where
`may_syscall()` does not hold, and verifies. It takes the block tokens
as the entry points do (`Live::empty()` for a pop, `axiom_c_claim` for
a free), which are ghost. So none of these functions
requires it, and neither does anything they call. `malloc_fast` is a
second witness: it makes the same calls and does not require it either.
Each entry point's body (its `_block` core: `malloc_block`, `free_block`,
`free_sized_block`, `free_usable_block`; each core requires
`may_syscall()`) calls nothing else before its slow-path tail
call (`malloc_slow`, `malloc_slow_class`, `free_slow`,
`free_sized_unhinted` for a size no class holds, `free_sized_slow`,
`free_usable_slow`), which requires `may_syscall()`.

**Why the capability can be trusted.**
- The only instructions that enter the kernel are the `syscall`/`svc 0`
  in `sys/raw.rs` (`syscall0`..`syscall6`, both architectures,
  `sys/raw.rs:20`, `:153`), each `requires may_syscall()`.
- Every trusted body or specification that can reach the kernel, by a
  syscall, a libc call, a call through a function pointer or a verified
  function it calls unchecked, `requires may_syscall()` too:
  `sys::getrandom`, `read`, `write` (`sys/mod.rs:569`, `:755`, `:825`),
  `linux::try_reserve` (`layout/regions.rs:1443`), `ThreadKey::create`
  (`heap/shared.rs:255`), `register_fork_handlers` (`api/mod.rs:412`),
  `background::spawn` and `pthread_join` (`purge/background.rs:333`,
  `:229`), `init_fields`, `assign_home`, `Heap::destroy`
  (`heap/mod.rs:4060`, `:4087`, `:4277`), `Meta::commit`, `Meta::free`
  (`meta.rs:2745`, `:2796`), mallctl's reader call `call` and
  `heap_set_arena`, `heap_set_tcache_enabled`, `tcaches_flush`
  (`api/mallctl.rs:192`, `:1042`, `:1052`, `:1063`), `print::emit`
  (the report's only call-out, theorem 16), the C++ `call_handler`,
  `call_throw_bad_alloc` (`api/cxx.rs:163`, `:119`), the
  `pthread_setspecific` wrapper (which may allocate; `heap/shared.rs`,
  from theorem 15, in place of an `assume_specification`), `Set::ensure`
  (`class/sets.rs`, which commits set words through `Meta`; its
  capability and `Inner::ensure_sets`' were added at the merge), and
  the `assume_specification` of `flush::seed` (`getrandom`,
  `heap/flush.rs:1365`).
- `may_syscall` is opaque, so a verified function can prove it only from
  its own `requires` (or a loop invariant that carries it in from
  there), from `grant_syscalls`, or from the type invariant of a
  `Stderr` (the abort's sink, `sys/abort.rs:34`), which only a context
  that holds it can build (`Stderr::new` requires it; `Default` is outside
  the proofs).
- `grant_syscalls` is called only in the bodies of `extern "C"`
  functions and of `sys::abort_fmt` (`sys/abort.rs:143`), which never
  returns; a panic, a failed `vspec::check` included, reaches it through
  the `#[panic_handler]` (`lib.rs:108`). Outside tests, no Rust code
  calls an `extern "C"` function of the crate.
  `api::tests::syscall_grants_only_at_roots` checks the source: every
  grant is in an `extern "C"` or `-> !` function, and `may_syscall`
  appears only as a `requires` or loop invariant clause, apart from its
  definition, the grant and `Stderr`'s invariant.
- The later theorems' code follows the same discipline (merged after
  this section's commit). Fork-handler lock handoff (theorem 13):
  `RawLock::lock_held` and `unlock_held`, `purge::background::fork_lock`
  and `fork_unlock`, and `fork_release` require `may_syscall()`; `fork_prepare`,
  `fork_parent` and `fork_child` are `extern "C"` and grant it.
  Re-entrant heap registration (theorem 15): `heap_for_thread_in`,
  `ThreadKey::set` and its `pthread_setspecific` wrapper require it,
  while `existing_heap_in`, `tls_heap_slot` and `tls::get`/`get_tp`
  (which only take the `TlsSlot` token) do not, so the fast paths stay
  as above. No allocator lock across external calls (theorem 16):
  `print::emit`, `Sink::new` and `Sink::flush` require it.

So a system call happens only below a frame that requires
`may_syscall()` or grants it, or that holds a value only such a frame
can make (below). Below a fast-path function no frame does, except
inside `abort_fmt`. A fast-path call that returns made no system call.

**Premises beyond the common ones.**
- The trusted bodies the fast paths reach make no system call:
  `heap_from_slot` (`heap/thread.rs:218`, PAC `autda` and a cast),
  `fast_flags` (a raw read), `sys::tls::get` and `get_tp` (a TLS load,
  `mrs`), `pac::auth` and `sign` (PAC instructions), `Heap::home_ptr_at`
  and `cached_recently` (`heap/cache.rs`, address arithmetic and loads),
  `arena_volatile` (`heap/mod.rs`, a volatile read),
  `SlabTable::owner_unchecked`, `load_owner` (a `Relaxed` load) and
  `watermark` (`class/table.rs`),
  `vspec::word::load` (an atomic load), `layout::regions::class_entry`
  (an empty `asm!`), `addr_to_ptr`, `api::shared_ptr` and `shared_ref`.
  So do the `core` functions they use (`Option::map`, `?`, atomics,
  pointer arithmetic), which the common premises already trust.
- Two values reach the kernel from code with no `requires`, and only a
  frame with the capability can make them. `LockGuard::drop`
  (`lock.rs:403`, `external_body`; a destructor cannot have a
  `requires`) may `FUTEX_WAKE`, and a guard comes only from `Lock::lock`
  (`lock.rs:288`). `Sink`'s `fmt::Write::write_str` (outside `verus!`,
  reached through `core::fmt`) flushes, and a sink comes only from
  `Sink::new` (`api/print.rs:74`). Both require `may_syscall()`, and no
  fast-path function takes either as an argument.
- The test checks where the grants are, not who calls the functions
  that make them: that no verified code calls an `extern "C"` function
  of the crate is read from the source.
- The trusted specifications are consistent as formulas. The proof
  needs no runtime fact: `may_syscall` is true but opaque, so even a
  false premise at run time (a double free voiding `axiom_c_claim`)
  gives a fast-path function no way to establish it.

**What it does not cover.**
- The slow paths. Refill, flush, heap creation, `init` on the first call,
  huge blocks, `errno` and the scheduled pop that `pop_fast` declines all
  run behind a call that requires `may_syscall()`. `calloc` reaches its
  pop through `api::shared` and `heap_for_thread`, which may initialise
  or create a heap; only the pop itself is covered. `realloc` and
  `free_aligned_sized`'s hint (`class_for`) are not covered.
- Faults. The first touch of a fresh slot or cache stack page can fault,
  and so can `PROT_NONE` guards and swapped-out pages. A page fault is a
  trap the kernel takes, not a system call, and no proof here models it.
- Machine code. The theorem is about the verified source. The build
  inlines these functions into the exported `malloc` and `free`, and on
  x86_64 at this commit their machine code has no `syscall` instruction.
  It leaves them only by a jump to `malloc_slow` or `free_slow`. That is
  a reading of the disassembly, not a proof.
- Termination. The theorem says which instructions run, not that the
  fast path finishes: that is theorem 9 ("Fast paths take no lock and
  terminate"), whose invariant masks cover the same functions.

## 9. Fast paths take no lock and terminate

The masked fast-path functions listed below (the fast branches of
`malloc`, `free`, `free_sized` and `sdallocx` with no flags, and
`challoc_free_usable`) call no lock acquisition, and each terminates.
The entry points themselves are not masked, the slow paths are outside
the theorem, and a bound on the step count is read off the code, not
proved ("Scope: which paths", "What stays exempt").

Read at the lane commit `b14b729` (the `file:line`s are the `fn` line
there), except the fast-path signatures below, which are at its merge
`831ed97`.

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
- **The nine `exec_allows_no_decreases_clause` functions** (common
  premises) are all off these paths: lock waits, `claim_with`'s retry,
  `init_slow`, the OOM handler loop, the purge thread, and the kernel
  retry loops in `sys/`. They need a liveness model.

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

## 10. No leak on thread exit (when the destructor runs)

Two claims, of which only the first is proved. (a) When a heap's
thread-exit destructor runs, the flush it runs first returns every slot
the heap's caches held to the release path: proved, below. (b) Every
heap gets a destructor: not proved, and not true in every run. A heap is
registered for one by `ThreadKey::set` (`heap/shared.rs:296`), which
returns at once, registering nothing, when the key is invalid
(`pthread_key_create` failed at init); the heap is still created and
published in TLS, so its cached slots are stranded when its thread
exits, and there is no heapless fallback for that case. A
`pthread_setspecific` failure aborts instead. Theorem 15 covers
registration's re-entrancy, not its success.

Read at the lane commit `2801c34`, before its merge onto the live-block
model (the `file:line`s are the `fn` line there).

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
strands no cached block when it runs (claim (a); whether it runs is
(b)). `tcache.flush` (`Tcaches::flush`, `heap/tcaches.rs:163`), `malloc_trim` (`api/libc.rs:1609`), the mallctl
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
- Claim (b), above: that the destructor runs at all (a valid thread
  key, or a fallback that holds no cache when there is none) is not
  stated; with an invalid key a heap is created anyway and its caches
  are stranded at thread exit.
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
- The arena-unbind and exit purge that `destroy` runs after the flush
  are not covered: they return no cached slot.

## 11. Shuffle is a permutation

The `file:line`s are the `fn` line at the commit that proved it.

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

## 12. Zero-on-free

Read at this section's commit (the `file:line`s are the `fn` line
there, or the release's line where a release is named).

**Statement** (`heap/zero.rs:473`, `:495`, `:520`):

```rust
pub open spec fn zero_on_free(s: &Shared) -> bool {
    s.flags() & PUSH_ZERO != 0
}

pub proof fn theorem_zero_on_free(tracked z: &Zeroed, tracked tok: &GhostSingleton<int>, p: *mut u8, c: int)
    requires
        super::regions_ok(),
        process_regions().is_slot(c, p@.addr),
        tok.id() == slot_loc(c),
        tok@ == p@.addr as int,
        z.covers(p, class_table()[c].size as int),
    ensures
        block_usable(p@.addr, 0) == class_table()[c].size,
        forall|a: int| p@.addr <= a < p@.addr + block_usable(p@.addr, 0) ==> #[trigger] z.has(a),

pub(crate) proof fn theorem_zero_on_free_huge(tracked z: &Zeroed, s: &Shared, page: usize)
    requires
        s.wf(),
        s.huge_run_zero(*z, page),
    ensures
        process_regions().is_huge_block(z.addr() as usize, z.len()),
        forall|a: int| z.addr() <= a < z.addr() + z.len() ==> #[trigger] z.has(a),
```

With `zero_on_free` configured, a free path gives up a freed block only
while it holds a `Zeroed` token (theorem 6's contents model) for all of
the block's usable bytes:

- a slot `p` of class `c`: every byte of its class's size (its usable
  size, `vspec::zero::block_usable`, one class per slot), at the moment
  its uniqueness token `tok` goes onto a thread cache (`Heap::push_cache`)
  or back to its bitmap word (`Shared::free_direct`);
- a huge run starting at `page`: whole pages from the run's start, at
  least one, a block of the huge region (`Shared::huge_run_zero`: the
  size `Huge::usable_size` returns, which `Huge::spec_run_usable`
  bounds), at the moment `Shared::free_huge` releases it.

The two theorems are the obligations the verifier discharges just
before each release when `zero_on_free(shared)` holds: their `requires`
is checked at the call, their `ensures` says what the token means byte
by byte. Their proofs are small (the slot's class is unique; the run's
bytes lie in the huge region); the substance is the precondition, met
from the tokens `zero_slot` and `zero_huge` return.
Since nothing hands a slot out without its uniqueness token (theorem 5),
a freed slot's bytes are zero before it can be handed out again, on the
premise below that nothing writes them in between.

**Paths that honour it, and why every slot free is one of them.**
- `Heap::free_slow_in` (`heap/paths.rs:522`): `zero_slot` first, then
  either `Shared::free_direct` (no cache for the class, `:562`) or
  `push_cache` (`:596`). It is reached from `Heap::free_slow` (the
  home-arena push of `Heap::free`) and `Heap::free_other` (another
  arena's slot).
- `heap::free_direct` (`heap/paths.rs:844`), for a thread with no heap
  and `MALLOCX_TCACHE_NONE`: `zero_slot` before `Shared::free_direct`
  (`:880`), `zero_huge` before `free_huge` (`:902`).
- `Heap::free_huge` (`heap/paths.rs:318`): `zero_huge` before
  `free_huge` (`:342`).
- `Shared::free_direct` and `Shared::free_huge` have no other callers
  outside tests, and `push_cache` is called only by `push_cache_flags`
  and `free_slow_in`.
- Every other free of a slot is a cache push, and every push declines
  under `PUSH_ZERO`: `Heap::push_cache_flags` (`heap/cache.rs:593`) and
  `push_fast` (`:553`) ensure `flags & PUSH_ZERO != 0 ==> !r`, so
  `free_fast`, `free_fast_class` and `free_sized_fast` ensure the same
  and fall back to the exact `heap::free`; every push is handed
  `Shared::push_flags()` (`api/libc.rs` `free`, `free_sized_hint`,
  `challoc_free_usable`; `api/jemalloc.rs` `sdallocx`; `Heap::push`;
  `Heap::free_other`), whose result is `flags()`, and
  `Shared::zero_on_free` ensures `r == (flags() & PUSH_ZERO != 0)`. A
  declined push keeps the slot's token for the slow path (`!r ==>
  *final(tk) == *old(tk)`), which is `free_slow_in`.
- `realloc`, `rallocx`, `dallocx`, explicit tcaches and the sized paths'
  slow halves all free through `heap::free`, `Heap::free` or (for
  `MALLOCX_TCACHE_NONE`) `heap::free_direct`, so through the paths
  above. A free before init aborts (`free_before_init`).

**Paths that move freed slots without clearing them.** Flushes, GC,
cache rebuilds, `tcache.flush`/`destroy` and thread exit release slots
already in a cache: under `zero_on_free` each was cleared when it was
pushed (above), or came from a refill and was never handed out. The
theorem does not follow a token through the cache (see below).

**Premises beyond the common ones.**
- `vspec::zero::write_zero` (`external_body`, theorem 6's token source):
  `write_bytes(p, 0, n)` makes `n` zero bytes. `zero_slot` and
  `zero_huge` clear through it, with the same machine code as a direct
  `write_bytes`.
- `vspec::addr::axiom_slot_writable` and `axiom_huge_writable`, as
  before, for the writes' validity.
- `tok` is the slot token the freeing path takes from the block's `Live`
  token (`Live::claim_slot`, theorem 5's live-block model); at the C
  boundary that token comes from `heap::live::axiom_c_claim` (the `free`
  contract).
- As for theorem 6, a token is not a permission: that nothing writes a
  freed block's bytes between the clear and the next hand-out (no
  allocator writer touches slot bytes, the cache keeps its entries in a
  separate stack; the program's use after free is outside the contract)
  is stated, not proved.
- Huge runs: `zero_huge` and `free_huge` take the huge lock separately;
  the run the token covers is the one `free_huge` releases unless another
  thread frees the same run in between (a double free).
- `zero_on_free` is fixed for the process: `push_flags` is a plain field
  `init` writes once and every free path reads through `&Shared`
  (`api::shared_ref`'s premise that nothing writes `Shared` after
  `READY`); `zero_on_free` is a startup configuration key with no
  mallctl writer. Within one call `zero_on_free(shared)` is one value,
  so the guard on each obligation is the branch that cleared the block.

**Where the statement is weaker than the code.**
- It is a release-time statement: a token is held when the slot is given
  up, and it is not carried with the slot. The cache and the bitmap words
  keep no `Zeroed` token for a freed slot, so no pop or claim states
  that a slot freed under `zero_on_free` is zero when it is handed out
  (the code does not rely on it: `calloc` still clears anything outside
  a cache's zero prefix). Carrying it would need a token per cached
  entry above the zero prefix and per clear bitmap bit.
- That the listed releases are the only ones is argued from the call
  graph above, not stated as one Verus fact: the fast pushes' decline is
  proved, the slow paths' coverage is by inspection. A new release path
  that skipped the obligation would still verify. Making it mechanical
  means a ghost `Tracked<&Zeroed>` argument with the obligation as a
  `requires` on `Shared::free_direct`, `Shared::free_huge` and
  `push_cache` (no machine-code cost, but a signature change on paths
  other work is changing).
- The huge token's size is stated only as `Huge::spec_run_usable`
  admits it (whole pages, at least one, inside the region):
  `zero_huge` reads the size with no run token
  (`Huge::usable_size(page, None)`), and only with the run's token does
  `usable_size` name the descriptor's usable pages. So that the token
  covers all of the run's usable pages, `huge_usable(size)` of the
  original request, is its code's, not proved; handing `zero_huge` the
  run token the freeing path holds (`Heap::free_huge`'s `tok`, the
  token `heap::free_direct` takes with `take_run`) would prove it.

## 13. Fork-handler lock handoff

Formerly titled "Fork safety", as the code's comments still call it.
What is proved: under the mutex and handoff premises below,
`fork_prepare` returns holding every lock of the fork lock list
(`listed`, over the arenas it found) and the background state lock,
and the parent and child handlers release each of them once. Not proved: that
`listed` names every lock in the crate (a source test checks it,
`fork_lock_list_matches_the_source_tree`), that the prepare handler
terminates (a `lock` that never returns satisfies everything), the
acquisition order, and compatibility with other `pthread_atfork`
handlers (the handoff premise assumes those registered before
challoc's take no allocator lock). Reclaiming the caches of threads the
child no longer has is out of scope (design 9 leaves them marked).

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

## 14. Arena isolation (slab ownership and release routing)

What is proved: a slab slot's owning arena is fixed once its group is
handed out, a claim reserves only in slabs its arena owns, and every
release of a slab slot reaches the arena that owns it (or the free
aborts). Not proved, and not true: that `mallocx(size,
MALLOCX_ARENA(a))` returns a slot of arena `a`. The claim side stops at
the picks, and a cache may hold other arenas' slots: the sized frees
push a slot onto the home cache without testing its owner and leave the
routing to the flush (design 6.3), so a pop from that cache can return
a foreign slot. Isolation is a property of reservation and release, not
of the blocks an allocation returns.

Read at this section's commit (the `file:line`s are the `fn`/`axiom`
line there).

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

## 15. Re-entrant heap registration

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

## 16. No allocator lock across external calls (partly premised)

Read at this section's commit (the `file:line`s are the `fn`/`axiom`
line there).

**Statement.** What Verus checks: every call the reports make out of
the allocator is made by `emit`, which requires an `Unlocked` permit,
and a permit exists only inside a report. Verus checks possession of
the permit, not its meaning: the permit tracks no acquisition, so that
the calling thread holds no allocator lock while it holds one rests on
the premises below (the C caller's, a source test's and Rust's types'),
not on Verus. The intended property, which those premises give: every
call the reports make out of the allocator happens while the calling
thread holds no allocator lock. The reports are
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

The B column of the interface contract below; not numbered. The
`file:line`s are the `fn` line at this section's commit.

**Statement.** Returning calls satisfy the pointer-shape checks and the
selected argument checks below. Pointer shape needs no C contract for
the call itself; like every theorem it holds in a run that has made no
earlier non-live free, which the live-pointer contract (`axiom_c_claim`)
excludes. For the pointer shape (no block start, misaligned, interior),
the size hints of a live slot or a non-block pointer, cache ids of
`MAX_TCACHES` or more and the alignments, a bad argument either makes
the call not return or gets the documented error result. "Does not return" is what is proved; the code
aborts, but that the call reaches `SIGABRT` rather than hanging is not
proved (partial correctness). A stale or double free of a slot or huge
page start, and a size hint on such a pointer, are not proved to be
rejected: the C-boundary assumption excludes them (*(C)* below), and
the run-time checks that catch some of them are code, not theorem (the
rows marked *(C)*). Not checked by the code either: a huge run's size on
a sized free, and a free of a block that was freed and reallocated,
which no allocator can tell from a live one (table below). Stated as
`ensures` on the Rust functions the exports call (an abort satisfies
every postcondition, so each one says what holds of a call that
returns):

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
contract. "Result" is what the code does; for an "abort" row, "yes" in
"Proved" means that the call does not return normally with that input,
and for an error-result row, that a returning call returns that result:

| Bad input | Entry points | Result (the code) | Proved |
|---|---|---|---|
| Pointer outside every class region and the huge region, in a gap, in the metadata, in a guard slab | every free, the `realloc` family, `malloc_usable_size`, `sallocx`, `xallocx` | abort | yes: `free_target` (theorem 3, theorem 7's `theorem_guard_free`) |
| Misaligned pointer (not a multiple of the quantum) | as above | abort | yes: `free_target` and `theorem_free_target_aligned` |
| Interior pointer of a slot, or of a huge run off a page boundary | as above | abort | yes: `free_target` and `Live::theorem_interior` (`lemma_slot_interior`, `lemma_run_interior`) |
| Interior page start of a huge run | frees, `realloc` family | abort (`Huge::free_run`: `not_a_run`) | no: excluded by the C contract, *(C)* below |
| The same | `malloc_usable_size`, `sallocx`, `xallocx` | abort (`Huge::usable_size`: `not_a_run`) | no: the queries take no token, and nothing states it |
| Slot freed with the size of another class, or a huge size | `free_sized`, `free_aligned_sized`, `sdallocx`, sized `delete`s | abort (`hint_mismatch`) | yes for a live slot: `hint_ok`; for a non-live one, *(C)* |
| Non-power-of-two alignment to `free_aligned_sized` or the sized aligned `delete` | as above | abort on a slot | as above: `hint_ok(p, None)`, so no slot start returns |
| Huge run freed with a size that names another usable size | sized frees | freed; the size is ignored (as jemalloc does) | no: not checked |
| Non-power-of-two or zero alignment | `aligned_alloc`, `memalign` | NULL (`EINVAL`) | yes (row 5; `EINVAL` by *(errno)*) |
| Alignment not a power of two, or not a multiple of `sizeof(void *)` | `posix_memalign` | `EINVAL` | yes (row 6) |
| Non-zero alignment that is not a power of two | `new`, `new[]` (`align_val_t`) | NULL (`nothrow_t`) or `bad_alloc`, after the handler loop | yes: `new_aligned` |
| Any `MALLOCX_LG_ALIGN` (0 to 63) | `mallocx`, `rallocx` | a block aligned to it, or NULL | yes: `fits(r, size, 1 << lg_align)` (theorem 2): no misaligned block |
| `MALLOCX_TCACHE(t)`, `t >= MAX_TCACHES` (1024; the field reaches 4093) | `mallocx`, `dallocx`, `sdallocx` | abort (`invalid_tcache`, or first `hint_mismatch` on `sdallocx`), except a huge `mallocx` with an explicit arena, which never looks the cache up | yes: `tcache_checked`, `!bad_tcache` (the frees' for a live block or a non-block pointer; *(C)* otherwise) |
| `MALLOCX_TCACHE(t)` of a cache never created | as above | abort, with the same exception | no: existence is a run-time table load, not modelled |
| `MALLOCX_ARENA(a)` of an arena that does not exist | `mallocx`, `rallocx` | NULL | no: as above |
| Double free of a slot back in its bitmap (stale), or a free of a never-allocated slot of a handed-out slab | plain frees | abort (`check_live`, `check_located`: `invalid_free`) | no: excluded by the C contract, *(C)* |
| The same | `malloc_usable_size`, `sallocx`, `xallocx` | abort (`check_live`) | no: the queries take no token, and nothing states it |
| The same, through a sized free | sized frees | abort at the cache flush (`double_free`, `straddling_double_free`) | no: excluded by the C contract, *(C)* |
| Double free while the first copy is still in a thread cache | every free | with `double_free_scan` enabled (off by default), abort if among the top four entries (`cached_twice`); else abort at the flush, or the slot is handed out twice first, as under jemalloc | no: excluded by the C contract, *(C)* |
| The same, through `realloc` and the queries | `realloc` family, `malloc_usable_size`, `sallocx`, `xallocx` | no abort at the call: the slot bit is still set, so `check_live` passes. `realloc(p, 1000)` after `p = malloc(16); free(p)` copies from `p` and frees it again, which pushes a second copy (aborts at that push only with `double_free_scan`); a kept block is returned as is | no: excluded by the C contract, *(C)*; the queries take no token |
| Double free of a huge run | every free | abort (`Huge::free_run`: `already_free`, or `not_a_run` once the freed run has been merged or left the table) | no: excluded by the C contract, *(C)* |
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
so the context stays consistent and the call is proved not to return.
`hint_ok` and `!bad_tcache` on the frees are about the program's own arguments (a
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
  (proved); `sallocx` and `xallocx` abort for NULL (proved: they ensure
  `p@.addr != 0`, from their explicit test and "No block at address 0").
- Before the first allocation, every free or query of a non-NULL
  pointer aborts (`free_before_init`); a thread's first `free` may
  create its heap before the abort (theorem 3).
- `errno` is the F column (*(errno)* in the interface contract), not
  this section's.

- Not checked, by the code either: a huge run freed through a sized
  free with a size that names another usable size. `hint_ok` constrains
  slot starts only, and `check_size_hint` passes every other pointer to
  the exact free, which ignores the size (jemalloc's `nallocx(size) ==
  sallocx(p)` is not checked for huge runs).

**Where the statement is weaker than the code.**
- The *(C)* rows: the run-time checks abort where they fire, but the
  proof excludes those inputs by the C contract instead of proving the
  abort. What the code guarantees for them: an unsized free, `realloc`
  or query of a slot whose bitmap bit is clear at the load aborts (a
  sized free defers that to the cache flush, which a `malloc` pop of the
  slot can precede); a freed slot still in a cache (a thread's or an
  explicit tcache; bit set) passes the call and is caught, if at all, by
  a later flush or, among the top four entries of the thread cache, by
  `double_free_scan`; an ABA free is undetectable. The queries'
  stale-slot and interior-page aborts are not stated.
- Every "abort" is proved as "does not return normally". The abort
  helpers verify and end in `sys::abort_fmt`, but a call may wait on a
  lock before it reaches one, and lock waits are not proved to
  terminate (common premises, "Termination not proved"), so a hang is
  not ruled out.
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

## 17. Huge coalescing (at the huge lock invariant)

A fact of every value the huge lock admits (`Inner::wf`); nothing is
exported per call, and no entry point's `ensures` mentions it.

Read at this section's commit (the `file:line`s are the `fn`/`spec`
line there).

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
- That `find` returns the lowest fit is theorem 23
  ("Lowest-address huge placement"), built on this one.

## 18. Pressure never lengthens decay (at a fixed limit)

A theorem about two pure functions, `band` and `effective_decay_ns`, at
one fixed limit and one configured period. It is not about the stored
policy the purge reads (`Policy`'s atomics, whose round trip is not
proved and is not exact at the 70% edge, below), nor about the purge
being timely: what the decay walks do with the period is out of scope.

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
  `AtomicU32`'s value), and it is not exact: usage at the 70% edge
  (700,000 to 700,003 ppm of the limit) gives `Scaled(0)`, which
  `Policy::band` reads back as `Low`. The period is the same, but the
  advice is not: `advice` is lazy only with headroom, the `Low` band, so
  by the design table usage at 70% is eager, while the stored policy's
  `headroom()` reads `Low` there and the purge gets the lazy advice
  (fixed upstream after this base; the code here still maps it).
  Concurrent `evaluate` calls or a reader between `apply`'s stores (a
  torn `limited`/`frac16`/`immediate` triple) are not covered, nor is
  what the decay walks do with the period.
- A 0 limit. `spec_band(0, c)` is `Unlimited` for every usage, as for
  `UNLIMITED` (no limit known), while `thp::forced_by_limit` treats a 0
  limit as a limit below `THP_MIN_LIMIT` (it forces `never` under
  `thp:always`). The theorem holds for `band` as written; the two
  readings of 0 are not reconciled.
- Monotone in the usage sample, not in time: two samples taken in the
  other order (usage falling) lengthen the period again, by design.
- `NEVER` (`dirty_decay_ms:-1`) stays `NEVER` in every band; the
  theorem holds there with equality.
- The test `policy::tests::rising_usage_never_lengthens_the_period`
  samples the period half of the theorem at runtime, including limits
  past `2^43`.

## 19. Purge never discards live data (huge runs; slabs at the class lock)

For huge runs, against the live-block tokens: no discarded byte is in a
live run. For slabs, only against the class lock's ghost idle state
(`class::decay::idle_byte`): a class purge discards only slabs that
state records as idle, and nothing ties idleness to the slot tokens
(the I5 slab row is `p`).

The `file:line`s are the `fn` line at this section's commit.

**Statement.** Three parts: what every discard is handed, and what the
huge and class purges hand it.

```rust
// purge.rs:697: the one place a batch's ranges reach `madvise`.
fn discard(ranges: &[(usize, usize)], advice: Advice, Ghost(free): Ghost<spec_fn(int) -> bool>)
    requires
        ranges.len() <= MAX_BATCH,
        forall|i: int| 0 <= i < ranges.len() ==> frees(free, ranges[i].0 as int, ranges[i].1 as int),

// huge/mod.rs:744
pub open spec fn free_page_byte(base: int, pg: int, np: int, held: Set<int>, b: int) -> bool {
    &&& base <= b
    &&& (b - base) / pg < np
    &&& !held.contains((b - base) / pg)
}

// heap/live.rs:477, in `impl Live`
pub proof fn theorem_purge_huge(tracked &self, tracked auth: &GhostSetAuth<int>, b: int)
    requires
        crate::heap::regions_ok(),
        self.wf(),
        auth.id() == crate::huge::page_loc(),
        free_page_byte(process_regions().huge_base as int, page_size(),
            (process_regions().huge_len >> process_regions().lg_page) as int, auth@, b),
    ensures
        !(self.addr() <= b < self.addr() + self.len()),

// class/decay.rs:154 (closed; unfolded)
pub closed spec fn idle_byte(t: SlabTable, states: Map<nat, State>, b: int) -> bool {
    let sz = (1usize << class_table()[t.spec_class() as int].lg_slab) as int;
    let s = (b - t.base) / sz;
    &&& t.base <= b
    &&& s < t.spec_nslabs()
    &&& states.contains_key(s as nat)
    &&& states[s as nat] != State::Active
}
```

(Line breaks added.) A `purge::Batch` (`purge.rs:227`) carries a ghost
predicate `free` that its maker fixes (`Batch::new`, `:328`); `push`
(`:366`) takes only ranges of bytes `free` admits (`frees`, `:236`),
`Batch::wf` (`:303`) keeps that of every range held, merged ranges
included, and `flush` (`:420`) hands `discard` the ranges with `free`.
So every byte any flush discards, `MADV_DONTNEED` or `MADV_FREE`,
including the flush inside a `push` to a full batch, satisfies the
batch's `free`. The makers:

- **Huge purges** (`Huge::purge_empty`, `purge_curve`, `free_run`,
  `huge/mod.rs:3247`, `:3533`, `:5327`): `free` is
  `free_page_byte(base, page, npages, S, _)`, where `S` is the set of
  the run-page authority the huge lock's value holds when the batch is
  made (`Inner::wf`: the `Active` descriptors' pages, `active_pages`).
  The walks keep the lock value's authority equal to `S` until the
  flush (`purge_extent`, `:3349`, ensures it unchanged), and
  `purge_extent` proves an `Empty` extent's pages are not in it: they
  are apart from every `Active` extent. `free_run` makes its batch after
  the freed run's token left the authority (`GhostSetAuth::delete`), so
  `S` no longer holds the run's pages. `RunToken::theorem_purge_apart`
  (`huge/mod.rs:715`): a byte `free_page_byte` admits for the
  authority's set is in no page, guard included, of a run whose token is
  held anywhere (a token's pages are a subset of the authority's set,
  `GhostSubset::agree`). `Live::theorem_purge_huge` lifts that to every
  live block: a run's usable bytes are inside its pages, and a slot lies
  in the class superblock, which `Regions::separated` keeps apart from
  the huge region. The huge allocator's base, page and page count are
  the process regions' under `Shared::wf` (`heap/shared.rs`), so the
  batches' predicate is the theorem's.
- **Class purges** (`ClassState::purge_empty`, `purge_curve`,
  `class/decay.rs:518`, `:839`, through `Purger`, `:347`, and
  `purge_group`, `:605`): `free` is `idle_byte(t, states, _)`, the bytes
  of slabs of the class table that the arena's class lock value, when
  the walk starts, records as owned and not `Active` (`Fresh`, `Empty`
  or `Purged`). `Purger::inv` (`:389`) keeps the lock value agreeing with
  `states` on ownership and `Active`-ness through the walk (a purge only
  moves `Empty` to `Purged`), so at every flush the discarded bytes are
  those of slabs idle in the current lock value. `purge_group` pushes a
  page group whose every slab is idle: with one slab per page, the slab
  is in the empty set (`Inner::slab_core`: `Empty`); with more,
  `page_group_is_idle` (`:241`) says so.

**Premises beyond the common ones.**
- Huge: none beyond the huge lock axioms (`vspec/huge.rs`) and
  `huge::axiom_run_locs` (one huge allocator per process), which
  theorem 5's run tokens already use.
- Class: `page_group_is_idle` (`class/decay.rs:241`, `external_body`)
  now also ensures every slab of the group is not `Active` in the lock
  value; its body reads each descriptor's state and arena, and the
  ensures rests on the owner-word agreement it already assumed and on
  the descriptor-state agreement `ClassState::state_locked`
  (`class/mod.rs:1429`) assumes.
- Class, the slot-token link (not proved; see below).

**Where the statement is weaker than the code.**
- **Slabs are not tied to slot tokens.** "Idle in the lock value" is the
  class lock's ghost state, not the slot tokens: nothing proves that no
  slot of an idle slab is live, cached or claimed, so the class half
  stops at the lock state, plus the run-time check in
  `ClassState::walk_empty` (`class/decay.rs:299`, the abort at `:325`)
  that an empty-set slab's descriptor reads `Empty` with `live() == 0`.
  The missing link is the descriptor's `live` counter, which is not
  modelled. **Unconfirmed** (read from the code and its comments, not
  checked by a proof): `live` counts set bits plus in-flight
  reservations, a claimer reserves under the class lock
  (`ClassState::take`) before it sets bits, and a releaser clears bits
  before it subtracts without the lock
  (`ClassState::dec_live`, `class/mod.rs:1867`; `SlabDesc::sub_live`,
  `slab.rs:255`, `external_body`), so `live == 0` read under the lock
  means every bit of the slab is clear and every slot token is in its
  bitmap word's invariant. Proving it needs `live` as a ghost-tracked
  atomic: a permission for the counter in an atomic invariant whose
  value counts reservation credits (minted by `take`, spent by the
  claim's `fetch_or` into the word invariant of each bit it sets) and
  release credits (taken by `fetch_and` with each cleared bit, spent by
  `sub_live`), plus a lock-held "frozen" flag for idle slabs. That
  threads tracked arguments through `take`, `finish`,
  `AtomicBitmap::claim_word`, `claim_from`, `release`,
  `ClassState::release_word` and `dec_live`, the allocation and free
  paths, and changes `bitmap::WordPred`; it is not done here.
- No lemma places the class half's bytes: `idle_byte` bytes lie in the
  class region, apart from the huge region and the other classes'
  slots, but nothing states it, so `Live`-level disjointness is proved
  for huge purges only.
- The huge half is stated for the run-page authority, not per batch
  range: each flush site proves the batch's `free` from the lock value
  it holds; no lemma states "the bytes of this flush" as a set.
- Only `purge::Batch` discards are covered. `MADV_GUARD_INSTALL`
  (`class/table.rs`, `huge/mod.rs`) also drops page contents, of guard
  pages only (theorem 7), outside any batch.
- Theorem 6's purge premise is not discharged: `axiom_discarded_zero`
  and `class::axiom_discarded_slots_zero` still take any range, and a
  `Purged` state's history (discarded with `MADV_DONTNEED` in the hold
  that set it, untouched since) stays a premise. Discharging it would
  need `Batch::flush` to mint `Zeroed` tokens from an eager discard,
  the class and huge lock values to hold them per `Purged` slab or
  extent (split and joined as extents are carved and coalesced), and a
  ghost link from `purge::LAZY_EVER`, a plain `Relaxed` atomic, to the
  batches that used `MADV_FREE`.
- `MADV_FREE` and `MADV_DONTNEED` are treated alike: the theorem is
  about which bytes are discarded, not what they read afterwards.

## 20. `MALLOCX_TCACHE_NONE` bypasses thread caches

Read at this section's commit (the `file:line`s are the `fn` line
there).

**Model.** A heap's caches are its ghost cache permissions
(`Heap::perms`, a `CachePerms`): the headers it owns, their stacks and
entries, and the slot token under every cached entry
(`CachePerms::tokened`; "Uniqueness: slot tokens" in `verus/TODO.md`).
`perms` is a private field of `Heap`, so only `heap` code can change a
cache, and only through a `&mut Heap`. A verified function obtains a
heap only from the trusted `thread::heap_from_slot` (through
`heap_for_thread_in` and `existing_heap_in`), `tcaches::heap_at`
(through `Tcaches::get`, and `heap_destructor`) or `tcaches::heap_new`
(heap creation; `mallctl::thread_heap` goes through `heap_for_thread`),
and each reference is assumed `Heap::wf` afresh: no ghost state links two
references to the same heap. The theorem is therefore stated per
reference: every heap the direct paths obtain, they leave caching
exactly what it cached.

**Statement** (`heap/mod.rs:2964`, `:2978`; `heap/paths.rs:282`,
`:306`, `:329`, `:342`, `:355`, `:368`):

```rust
// The heap caches exactly what `old` cached.
pub closed spec fn caches_same(&self, old: Heap) -> bool {
    &&& self.perms@ == old.perms@
    &&& self.caches == old.caches
    &&& self.others == old.others
    &&& self.list@ == old.list@
    &&& self.home_hdrs@ == old.home_hdrs@
    &&& self.arena == old.arena
}

pub proof fn lemma_caches_same(&self, old: Heap, p: *mut CacheHeader, i: int)
    requires self.caches_same(old),
    ensures
        self.owns(p) == old.owns(p),
        self.hdr(p) == old.hdr(p),
        self.hdr_class(p) == old.hdr_class(p),
        self.cached_len(p) == old.cached_len(p),
        self.cached(p, i) == old.cached(p, i),
        self.tokened() == old.tokened(),
        self.home() == old.home(),
        old.wf() ==> self.wf(),

// Heap::count_direct_alloc, count_direct_free, count_uncached_alloc,
// count_uncached_free, alloc_huge_in, free_huge (the last two serve the
// thread's huge runs; of them only alloc_huge_in is on a
// MALLOCX_TCACHE_NONE path, a huge one):
    ensures final(self).keeps(*old(self)), final(self).caches_same(*old(self)),
```

The direct paths are:
- **allocation**: `mallocx::alloc` (`heap/mallocx.rs:143`) on
  `Tcache::None` calls `existing_heap` (`heap/thread.rs:200`) at most
  twice: once for the home arena when the flags name none
  (`Heap::arena`, `&self`), and once after a successful claim for
  `count_direct_alloc`, where a ghost `assert(h.caches_same(before))`
  restates the contract at the call. The slot comes from
  `alloc_direct` (`:220`) and `Shared::alloc_direct`
  (`heap/shared.rs:1890`), which take no heap: `ClassState::claim_tokens`
  takes it out of the class's bitmap words, with its token, and the
  token goes into the caller's `Live` (`Live::from_slot`).
- **deallocation**: `mallocx::free` (`heap/mallocx.rs:275`) on
  `Tcache::None` calls `heap::free_direct` (`heap/paths.rs:798`):
  `Shared::classify_free` (`heap/shared.rs:1949`), `zero_slot` under
  `zero_on_free`, then `Shared::free_direct` (`:1921`), which hands the
  slot's token (from the caller's `Live`, `claim_slot`) to
  `ClassState::release_with` and so back to the bitmap word; then
  `count_direct_free` on the `existing_heap` result, with the same
  ghost assert. A huge pointer takes `Shared::free_huge` and
  `count_uncached_free`, likewise.
- Neither path calls `Tcaches::get`, so no explicit cache is reached;
  `heap_for_thread` is not called, so no heap is created.

These are the paths of `mallocx`, `rallocx`, `dallocx` and `sdallocx`
(`sdallocx_routed`) with `MALLOCX_TCACHE_NONE` for a slab size:
`jemalloc::alloc_with` calls `mallocx::alloc` for every size with a
class, and `rallocx_zeroed`, `dallocx_block` and `sdallocx_routed` free
through `mallocx::free`. So a valid slab `mallocx`/`rallocx` allocation
or `dallocx`/`sdallocx`/`rallocx` free with the flag leaves every cache
of every heap it reaches holding the same slots, in the same entries,
with the same tokens: no cache gains or loses a slot. Machine code is
unchanged (the additions are ghost).

The test `api::jemalloc::tests::tcache_none_leaves_every_cache_alone`
checks the same at run time against the real allocator: with entries
in the thread's home caches, another arena's, the foreign cache and an
explicit cache, direct `mallocx`, `rallocx`, `dallocx` and `sdallocx`
calls (arena, zero and alignment flags) leave every cache's entries and
pop count unchanged.

**Premises beyond the common ones.**
- Every heap a verified path is handed is `Heap::wf` (common premises);
  `caches_same` itself needs no premise.
- The direct paths obtain no heap other than the `existing_heap`
  results (checked by inspection, not by Verus): outside `heap/` and
  `api/` the crate uses no `heap` item that yields a `Heap` (only
  `Shared`, constants such as `MIN_CAP` and `TLS_EXITED`, and spec items
  such as `fits`, `regions_ok`); `Shared`'s methods in `heap/shared.rs` call
  none of the heap sources above (`tcaches` appears there only in
  `Shared::wf`, its initialisation and the fork lock list); and the
  remaining callees (`zero_slot`, the `Live` methods,
  `libc::old_size_or_keep` on `rallocx`'s path, `check_size_hint` on
  `sdallocx`'s) take no heap.

**Where the statement is weaker than the code.**
- Per reference: Verus does not relate the heap the first
  `existing_heap` call returns to the second's (no ghost state survives
  between calls), so the composed claim, the thread's heap unchanged
  across the whole call, rests on each reference's frame plus the
  inspection premise above.
- It is not an `ensures` of `mallocx::alloc` or `mallocx::free`: those
  take no heap, and adding one would restructure their signatures.
- That the allocated slot came from the class's bitmap words and the
  freed one went back to them is what the verified bodies of
  `Shared::alloc_direct` and `Shared::free_direct` do with the slot
  token; their `ensures` do not name the class state.
- Only cached slots are covered. The counters (`uncached`) do change,
  as the flag's statistics require.
- Huge sizes: `mallocx::alloc_huge` with the flag calls
  `heap_for_thread`, which may create the thread's heap (with empty
  caches, proved inside creation, `CachePerms::fresh`, but not in
  `heap_for_thread`'s `ensures`), and `alloc_huge_in`, which ensures
  `caches_same`; the theorem does not otherwise cover them.
- Another thread using the same explicit cache concurrently is outside
  it; with `MALLOCX_TCACHE_NONE` this thread does not touch it.

## 21. MIB round-trip

Read at this section's commit (the `file:line`s are the `fn` line
there, all in `api/mallctl.rs`).

**Model.** The name tree is the static `ROOT` and the `Node`s it
reaches. `root_nodes()` (`:658`, uninterpreted) is its top level, and
`root` (`:665`) returns it; `spec_children` (`:673`) is what
`Node::children` (`:228`) returns: a directory's children, else none.
One shared recursive spec, `resolve` (`:695`), says what node a MIB
names: each entry picks a child with `step` (`:682`), the lone numeric
child of its parent for any entry, else the child at that index, and
the next entry looks among the picked child's children. `walk` (`:732`)
is the same descent driven by a name's components: `names` (`:715`)
matches a component under a lone numeric child when it is a nonempty
string of digits whose `decimal` value is the entry, else when the
child at the entry's index is named by it. `split` (`:767`) says
`comps` are the dotted components of a name: at least one, none holding
a dot, joined with dots back into the name. `level` (`:777`) is the
sibling list the `j`-th entry picks from, and `entry_named` (`:808`)
says which entry a component names there.

**Statement** (`:808`, `:994`, `:1071`, `:1155`, `:1832`):

```rust
spec fn entry_named(nodes: Seq<Node>, comps: Seq<Seq<u8>>, mib: Seq<usize>, j: int) -> bool {
    let l = level(nodes, mib, j);
    &&& l.len() == 1 && numeric(l[0])
        ==> comps[j].len() > 0 && all_digits(comps[j]) && mib[j] == decimal(comps[j])
    &&& !(l.len() == 1 && numeric(l[0]))
        ==> mib[j] < l.len() && l[mib[j] as int].name.spec_bytes() == comps[j]
}

proof fn theorem_mib_round_trip(name: Seq<u8>, comps: Seq<Seq<u8>>, mib: Seq<usize>, n: Node)
    requires
        split(name, comps),
        walk(root_nodes(), comps, mib) == Some(n),
    ensures
        resolve(root_nodes(), mib) == Some(n),
        forall|other: Seq<Seq<u8>>| #[trigger] split(name, other) ==> other == comps,
        mib.len() == comps.len(),
        forall|j: int| 0 <= j < mib.len() ==> #[trigger] entry_named(root_nodes(), comps, mib, j),

fn name_to_mib(name: &[u8], mib: &mut [usize; MAX_MIB]) -> (r: Result<(usize, &'static Node), c_int>)
    ensures
        r matches Ok((len, _)) ==> 1 <= len <= MAX_MIB
            && forall|j: int| len <= j < MAX_MIB ==> #[trigger] final(mib)[j] == old(mib)[j],
        r matches Ok((len, n)) ==> exists|comps: Seq<Seq<u8>>| #[trigger] split(name@, comps)
            && walk(root_nodes(), comps, final(mib)@.subrange(0, len as int)) == Some(*n),
        r matches Ok((len, n)) ==> resolve(root_nodes(), final(mib)@.subrange(0, len as int)) == Some(*n),
        r matches Err(e) ==> e == ENOENT,

fn mib_to_node(mib: &[usize]) -> (r: Result<&'static Node, c_int>)
    ensures
        r matches Ok(n) ==> resolve(root_nodes(), mib@) == Some(*n),
        r is Err <==> resolve(root_nodes(), mib@) is None,
        r matches Err(e) ==> e == ENOENT,
        mib@.len() == 0 ==> r is Err,

fn ctl(shared: &Shared, node: &Node, mib: &[usize], io: &Io) -> Result<(), c_int>
    requires
        shared.wf(),
        resolve(root_nodes(), mib@) == Some(*node),
```

(Line breaks added.) If `name_to_mib(name)` succeeds with `(len, n)`,
`name` splits one way only into components, the MIB `mib[..len]` has one
entry per component, each entry is the one that component names among
the children of the node the entries before it reached, and the MIB
resolves to `n`. `mib_to_node` succeeds exactly on MIBs that resolve,
and returns the node they resolve to; so given that MIB it returns `n`
(the same node value). Every entry at a numeric level (the `<i>` of
`arena.<i>.*` and `stats.arenas.<i>.*`, the `<j>` of `arenas.bin.<j>.*`
and `stats.arenas.<i>.bins.<j>.*`) is the decimal value of its
component. `ctl`'s `requires` is discharged at both of its callers:
`mallctl` (`:2008`) passes `name_to_mib`'s node with `&mib[..len]`, the
MIB it built; `mallctlbymib` (`:2108`) passes `mib_to_node`'s node with
the MIB it resolved, whatever the caller supplied (through the trusted
`mib_in`). So a key is always performed on a node its MIB names, with
that MIB's entries unchanged. That the fixed positions `ctl` and the
readers read with `at` (1 for the `<i>` of `arena.<i>`, 2 for the `<j>`
of `arenas.bin.<j>` and the `<i>` of `stats.arenas.<i>`, 4 for the
`<j>` of `stats.arenas.<i>.bins.<j>`) are those numeric levels is
checked by inspection, not stated.

**Proof.** The traversals' loops keep the descent as invariants:
`name_to_mib` keeps the ghost components matched so far, the node the
last one named (`walk` of the prefix), the current sibling list as that
node's children, and `name == join(comps) + "." + rest`;
`lemma_walk_push` (`:833`) and `lemma_join_push` (`:868`) extend them by
one component. `mib_to_node` keeps `resolve` of the prefix `mib[..k]`;
`lemma_resolve_split` (`:970`) continues a resolution among the last
node's children, which also shows that a bad entry leaves the whole MIB
unresolved. `child` (`:1020`) and `child_by_index` (`:1052`) now ensure
`names` and `step` exactly (the former `child` contract was a
disjunction). `lemma_walk_resolves` (`:792`) turns a walk into a
resolution, `lemma_walk_entries` (`:817`) reads off each entry, and
`lemma_split_unique` (`:892`) shows the components are unique: the
first ends at the first dot.

**Premises beyond the common ones.**
- `root` (`external_body`): every call returns the same nodes,
  `root_nodes()`. It is `&ROOT`, a static, so this holds; nothing about
  the tree's content is assumed (the theorem holds for any tree).
- The caller's name is read by `cstr` through `name_byte`/`name_bytes`
  (`external_body`, bounds only); the theorem is about the bytes they
  return.

**Scope.**
- Function pointers are not modelled: what a `Read` node's reader
  returns (`call`, `external_body`) and what an `Op` arm does are
  outside the statement. The statement fixes which node and which
  indices they get, not what they do with them.
- `Node` equality is value equality (name and entry): `mib_to_node`'s
  node is the same node value as `name_to_mib`'s, not necessarily the
  same address. No two siblings in the tree are equal, but the theorem
  does not use that.

**Where the statement is weaker than the code.**
- `child` returns the first child of that name; `names` accepts any
  child of that name. With duplicate sibling names (there are none) the
  theorem would still hold but not say which index is chosen.
- Completeness of `name_to_mib` is not stated: nothing says it succeeds
  for every name that does split into matching components (it fails on
  more than `MAX_MIB` components, and on a non-final component naming a
  key, which `walk` would reject one step later anyway). The test
  `every_name_round_trips_through_its_mib` (`:2320`) checks every name
  in the tree (numeric components spelled `0`);
  `names_resolve_to_positional_mibs_and_back` (`:2339`) checks the
  expected MIBs of selected names.
- `mallctlnametomib`, `mallctl` and `mallctlbymib` still have no
  `ensures` on their return codes (interface row 34); the round trip is
  stated on the cores and on `ctl`'s argument, not on the C entry points.

**Compatibility difference (documented, not changed).**
`mallctlnametomib(name, mibp, miblenp)` (`:2069`) resolves the whole
name first and then compares its length with the caller's capacity
`*miblenp` (`mib_cap`, `:2043`): when the capacity is smaller it returns
`EINVAL` and writes neither `mibp` nor `*miblenp` (`mib_out`, `:2056`, is
not called). jemalloc 5.3.0's `ctl_lookup` (`src/ctl.c`) loops
`for (i = 0; i < *depthp; i++)` instead: when the capacity runs out
before the name does, it returns 0 with the first `*miblenp` entries
written, leaves `*miblenp` unchanged, and ignores the rest of the name
without checking it. With a capacity of 2, `stats.arenas.0.pactive`
gives `[stats, arenas]` and 0 in jemalloc, `EINVAL` here; a caller that
passes room for the full MIB gets the same MIB from both. Not in the
theorem: the capacity read is trusted and the return code has no
`ensures`.

## 22. Exact stats

The `file:line`s are the `fn` line at this section's commit.

**Statement.** At a quiescent state, `stats.allocated` (read live by
`collect`, `collect_all`, `mallinfo2` and `malloc_stats`; at the last
`epoch` by `mallctl`) is the usable bytes of the live blocks plus the
class size of every slot in a thread cache. Three layers:

1. **The figures are true sums of what was read** (code, `stats.rs`).
   `collect_arena_with` (`:671`) hands out, through a ghost `pg`, the
   pages its load of the arena's `large` word returned, and ensures
   `arena_exact(r, pg)` (`:1012`): `small_allocated + large_allocated` is
   `Σ_c curregs[c] * size(c)` plus `(pg - large_runs * GUARD_PAGES) *
   page` whenever that true sum fits a `usize`. `collect_into` (`:800`)
   and `collect_all` (`:922`) hand out, through a ghost `rd`, the figures
   of each arena they found (in index order) and those pages, and ensure
   `totals_exact` (`:1087`) and `summed` (`:1075`): `totals.allocated`,
   and `allocated`, every bin's `curregs` and `large_runs` of the sum, are
   the true sums of those reads (`reads_allocated`, `:1058`) when they
   fit. `mallinfo2` (`api/glibc.rs:91`) hands out what `collect_all` read
   and summed and ensures `uordblks + hblkhd` is the sum's `allocated` and
   `hblks` its `large_runs`. Inside each function the ghost outputs are
   the values its loads returned (the body sets them from the loaded
   locals); to a caller they are only values for which the ensures hold
   (see "Scope").
   The figures are added with `fig_add`/`fig_mul`, whose spec is `+`/`*`
   when the result fits; `ArenaStats::add` (`:494`) and
   `Totals::add_arena` (`:356`) now state each needed sum alone, so an
   unrelated figure's overflow does not hide it. `collect` and `epoch`
   call `collect_into`; `collect_arena` and `malloc_stats` add no
   contract of their own.

2. **The counters track the blocks** (ghost model, `stats/ledger.rs`).
   `Books` (`:46`) has one step per counter update and per slot move:
   `reserve` (`ClassState::take`, `class/mod.rs:2247`: `claimed += n`
   under the class lock), `claim` (one bit won by `finish`, or by
   `claim_prefix` in the same lock hold as the reservation; the slot's
   token leaves its word, so no arena has it out), `move_slot` between
   flight, a cache and a live block (refill push, hand-out, free push,
   flush or direct-free take), `release` (`release_word`: the token back
   in its word, through the arena whose slab it is), `count`
   (`release_owned`'s `released.fetch_add(end)`, `class/mod.rs:4785`),
   `take_run` (`Huge::take`: the run token's pages come from the page
   authority), `note` (`Shared::alloc_huge`'s `note_huge_alloc(r.pages)`),
   `free_run` (`Huge::free_run`) and `unnote` (`Shared::free_huge`'s
   `note_huge_free(run.pages)`), plus `new_arena`. `Books::inv` (`:201`)
   holds at `Books::start` (`:221`, `lemma_start`, `:1014`) and after
   every step (`lemma_step_*`, `:636`-`:993`), in any interleaving:
   `claimed - released` of each `(arena, class)` is its out slots plus
   the reservations not yet claimed plus the releases not yet counted;
   the out slots of a class are each in exactly one of flight, a cache
   and a live block, and out through one arena; each `large` word is its
   arena's live runs and their pages, less those not yet added, plus
   those retired and not yet subtracted; live runs share no page.
   `note` and `unnote` take any page count within the pending total, not
   the run's own: only the totals matter, and quiescence zeroes them.
   `Books::theorem_quiescent` (`:1111`): when no update is in progress
   (`quiescent`, `:212`), `Σ_arenas (claimed - released)` of a class is
   its cached plus live slots, each arena's `claimed - released` is its
   out slots, each arena's word is its live runs and pages, and the words
   summed are all live runs and pages.

3. **Over the live tokens** (`stats/ledger.rs:1475`):

```rust
pub proof fn theorem_exact_stats(b: Books, tracked live: Map<int, Live>, n: int,
    reads: Seq<ArenaStats>, pages: Seq<int>)
    requires
        b.inv(), b.quiescent(), reads_ok(reads, pages), reads.len() == b.k,
        // the scan read the ledger's counters, arena i at index i
        forall|i, c| ... reads[i].bins[c].curregs == (b.net)(i, c),
        forall|i| ... reads[i].large_runs == (b.wruns)(i),
        forall|i| ... pages[i] == (b.wpages)(i),
        // live[0..n] are the program's live blocks, held at once
        n >= 0, forall|i| 0 <= i < n ==> live.contains_key(i) && live[i].wf(),
        forall|c| ... (b.live)(c) == key_slots(live_keys(live, n), c),
        b.runs_upto(b.k) == key_runs(live_keys(live, n)),
    ensures
        reads_allocated(reads, pages)
            == live_bytes(live, n) + class_bytes(cached_counts(b), 0, NCLASSES),
```

(Abridged.) `live_bytes` is `Σ live[i].len()`, each block's usable bytes
(`Live::len`: its class size, or its run's pages before the guard). The
tokens count once each: `heap::live::lemma_all_apart` (`heap/live.rs:786`)
shows two tokens held at once stand for different slots, or for runs
whose pages are apart (`lemma_keys_apart`, `:762`, from the slot tokens'
and run tokens' authorities), so the per-class slot sets and the run
extent and page sets have exactly the tokens' counts (`lemma_key_bytes`).
With layer 1, the `rd` a `collect_all`, `collect_into` or `mallinfo2`
call hands out is, by its body, the theorem's `reads` and `pages`; so
whenever the true sum fits, the reported `allocated` (and `uordblks +
hblkhd`) is the live blocks' usable bytes plus the cached slots'.

Per arena, `theorem_quiescent` gives `claimed - released == |out(a, c)|`
and the word equal to the arena's live runs, in the model; the theorem
over tokens is stated for the sum over arenas only.

**Huge counters, at the code level** (`huge/mod.rs:5277`,
`theorem_huge_stats`, private to `huge`): in every value the huge lock
admits, `stats.runs` is the number of extents the run authority holds and
`stats.run_pages` the number of pages the page authority holds: one
extent per live `RunToken`, and its pages (`theorem_run_counted`,
`:5323`: a token's extent and pages are among them). These are the
`huge:` counters of `malloc_stats_print`; no quiescence premise is
needed, since they change under the huge lock with the authorities.
`free_run` (`:5446`) now ensures `r.pages == tok.npages()`, and
`Shared::alloc_huge` and `free_huge` assert that the pages they add to
and subtract from the `large` word are the token's
(`heap/shared.rs:1991`, `:2038`).

**What the figures count.**
- `allocated` (`curregs * size`): slots out of the bitmaps, which at a
  quiescent state are the live slots and the cached ones (thread caches
  count, as in jemalloc), at their class size, not the requested size;
  huge runs at their usable bytes (pages before the guard).
- `active`, `resident`, `retained`, `mapped`, `pactive`, `pdirty`: slab
  and extent states, not blocks; layer 1 covers `allocated` only.
- `mallinfo2`: `uordblks` is `small.allocated` (classes up to `BIN_MAX`),
  `hblkhd` is `large.allocated` (the slab classes above `BIN_MAX` and the
  huge runs), `hblks` the huge runs only; so `hblkhd` can be non-zero with
  `hblks` zero (four live 32 KiB blocks add 131072 to `hblkhd`, 0 to
  `hblks` and 0 to `uordblks`). The module doc's table said `hblkhd` was
  huge runs only and `uordblks` all slots; it now says what the code
  counts.
- `malloc_stats`: per arena `in use bytes` is `small.allocated`; the total
  is `allocated`; `max mmap bytes` is `large.allocated`.

**Premises beyond the common ones.**
- Quiescence: no allocation, free, flush or `epoch` runs during the scan
  or between the last counter update and it; then each `Relaxed` load in
  `bin_stats` and `Arena::large` reads the last store, and the scan's
  reads are the ledger's counters. Verus's atomics return any value
  (common premises), so this link is a premise, stated as
  `theorem_exact_stats`'s `reads` requirements.
- Arena indices: the scan finds an arena at every index below
  `arenas.count()`, so `reads[i]` is arena `i` (`collect_*` skip an index
  `get_existing` returns `None` for).
- No wrap and no clamp: `claimed - released` is below `2^64` (it is a
  count of slots), and `bin_stats` does not clamp it, which holds at a
  quiescent state because a slab with a slot out has `live > 0`, so it is
  neither `Empty` nor `Purged` and counts in `curslabs` (argued from
  `on_live_zero`, not proved).
- The ledger abstracts the code: each step's counter update is the line
  named above (Verus does not see the atomics' values, so "`take` adds
  its reservation to `claimed`" is read from the code); the reservations
  `take` makes are what is claimed (`finish` ensures `r == picks_sum`, so
  a claim of `n` slots reserved exactly `n`), and `release_owned`'s `end`
  is the number of tokens it returned (its `ensures`: the bag loses the
  distinct addresses of `ptrs[..end]`).
- Arena attribution: a slot is claimed only from a slab its arena owns
  (`take` requires `ready`, which includes `owns(slab)`), released only
  through the arena that owns its slab (`release_owned`'s owner and
  descriptor checks, plain loads Verus does not see), and slab ownership
  never changes once assigned (`take_group`); a huge run's descriptor
  keeps the arena it was allocated through until it is freed. The
  `release` step's precondition (`x` is out through `a`) and `free_run`'s
  (the run is in `a`) rest on these.
- The `free` contract (`axiom_c_claim`), through the tokens.

**Scope and where the statement is weaker than the code.**
- Quiescent states only; a racy read can be off by the updates in
  progress (the ledger's `resv`, `unc`, `ta_*`, `tf_*`), or clamped.
- `mallctl`'s `stats.*` return the snapshot of the last `epoch`; they are
  exact if the state was quiescent at that `epoch` (`collect_into` stores
  what it returns, under the `stats` lock, whose value Verus does not
  constrain afterwards).
- The ghost outputs of layer 1 are set from the loaded values by each
  function's body, but its `ensures` does not say so: to a caller, `rd`
  and `pg` are values the callee picked, and a pick whose true sum does
  not fit a `usize` (a huge page count, say) satisfies `arena_exact`,
  `summed` and `totals_exact` for any result. So the chain from the
  figures `mallinfo2` returns to `theorem_exact_stats`'s `reads` rests on
  reading the body (each output is the local the code loaded), not on
  the contract. Closing it needs the page count of the `large` word in
  the result (`ArenaStats` holds runs, not pages), an exec struct change.
- The ledger is a model: nothing threads a `Books` through the code, and
  `theorem_exact_stats` relates its sets to the tokens by premise. A
  code-level ledger would need the class counters' updates inside the
  bitmap words' invariants (each `claim_word` and `release` updating a
  per-class count with the bit), which changes `bitmap.rs`'s protocol.
- Overflow: the figures are exact only when the true sums fit a `usize`
  (always, for the address space they describe, but not proved).
- Per-arena exactness of the `large` word needs the descriptor's arena to
  be unchanged while the run is live (not proved; the run token carries
  no arena).

## 23. Lowest-address huge placement

`Huge::find`'s contract, under the huge lock invariant: no entry point
exports it ("Where the statement is weaker" below). Built on theorem 17.

Read at this section's commit (the `file:line`s are the `fn`/`spec`
line there). Numbered at merge, and placed last to keep the numbers in
order.

**Statement** (`huge/mod.rs:4028`, `:3236`, `:970`):

```rust
spec fn page_free(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, next_page: int, p: int) -> bool {
    p >= next_page || exists|x: *mut HugeDesc| in_free(descs, x, p)
}

spec fn fits(&self, g: &Inner, q: int, need: int, align: usize) -> bool {
    &&& 0 <= q
    &&& q + need <= self.npages
    &&& self.aligned_at(q, align)
    &&& forall|p: int| q <= p < q + need ==> page_free(g.descs@, g.next_page as int, p)
}

fn find(&self, g: &Inner, need: usize, align: usize) -> (r: Option<(usize, *mut HugeDesc)>)
    requires
        g.wf(),
        1 <= need <= self.npages,
        align > 0,
        align & sub(align, 1) == 0,
        align <= self.npages * pow2(self.lg_page as nat),
        // ... (page-aligned base, ghost page count and shift)
    ensures
        // ... (the extent it comes from, as before)
        r matches Some((s, _)) ==> self.fits(g, s as int, need as int, align),
        r matches Some((s, _)) ==> forall|q: int| 0 <= q < s ==> !self.fits(g, q, need as int, align),
        r is None ==> forall|q: int| !self.fits(g, q, need as int, align),
```

(Line breaks and triggers dropped.) `in_free(descs, x, p)` (`:962`):
`x` is a free (`Empty` or `Purged`) descriptor with page `p`.
`aligned_at(q, align)` (`:3229`): `align` is at most a page, or page
`q`'s address is a multiple of `align`. `need` counts the guard page.

In every value of the huge lock, the start `find` returns for `need`
pages at `align` is the lowest page with the run's pages aligned,
inside the region, and each free: in a free extent or at or past the
high-water mark. `None` means no page fits at all. "Free" is page by
page: a fit may span pages of several free extents and the untouched
suffix; maximal coalescing (`Inner::wf`, "Huge coalescing") is what
reduces that to the one extent `find` looks at.

**How it is proved.**
- `align_page` (`:3269`) now also ensures that no page in
  `[page, r)` is aligned: those pages start in `[a, align_up(a))`,
  below the first multiple of `align` (`lemma_below_align_up`, `:982`).
- `find`'s loop holds two more facts at `from`: every free extent
  starting below `from` ends by it (`ends_by`, `:975`), and no fit
  starts below `from`. Each round, `lemma_below` (`:3247`) extends "no
  fit" to below `s`: no page in `[from, a)` is free
  (`Inner::lemma_none_free`, `:1244`: an extent holding one would start
  below `from` and so end by it, or start at a free bit `lowest_set_from`
  would have returned), and none in `[a, s)` is aligned.
- The pass to the next extent: a fit at `q` in `[s, e_end)` starts in
  `e` (extents are apart), so by `Inner::lemma_fit_extent` (`:1266`) it
  ends in `e` or `e` ends at the mark. The page after a free extent
  that ends below the mark is free only if a free extent starts there
  (apart again), which `coalesced` forbids. `e` does not touch the mark
  on this path, and `s` is `e`'s lowest aligned page with
  `s + need > e_end`, so no `q >= s` fits.
- At the returns: `s` fits by `e`'s pages (and the pages past the mark
  when `e` touches it). After the scan, no free bit is left at or past
  `from`, so `lemma_below` with `a = next_page` covers everything below
  `align_page(next_page)`, where all pages are fresh. The `None`s are
  the same bound plus `s + need > npages` for every `q >= s`.

**Findings.** None. The early return when the extent `e` reaches the
mark (`e_end == next_page`) is correct even when the alignment carries
`s` past `e_end`: `e` and the untouched suffix are one free stretch, no
free extent lies past the mark, every earlier extent was ruled out, and
the pages between extents are not free; so `s` is the lowest aligned
page of that stretch and nothing lower fits. When `s + need` overruns
the region it returns `None`, and no higher page fits either. Tests:
`huge::tests::alignment_past_a_trailing_extent_takes_fresh_pages`
exercises that return; `placement_is_the_lowest_fit` checks each
placement (or NULL) over 2,000 random steps (allocations, frees and
purges) against a brute-force lowest fit over the live runs.

**Premises beyond the common ones.**
- `vspec::huge::axiom_huge_lock_inv` (`vspec/huge.rs:35`): the lock
  protects `Inner::wf`, with `coalesced` and `free_marked`.
- `TwoLevelBitmap::lowest_set_from` (`bitmap.rs:3285`) is verified: the
  lowest set bit at or past `from`, or none.

**Where the statement is weaker than the code.**
- It is `find`'s contract under the lock; no entry point exports it.
  `alloc_aligned` (`:4633`) returns NULL before `find` when the size
  overflows or the run or the alignment exceeds the region, and after
  it when a descriptor, the table or bitmap growth, or the kernel
  commit fails, so "huge `malloc` returns NULL only when nothing fits"
  is not claimed.
- A free bit that starts no free extent aborts in `extent_at`
  (`:2829`); the converse of `free_marked` is not in `wf`, so the proof
  does not show that abort unreachable. An abort is not a return, so
  the statement holds.
- Scope, not a gap: the order is by address only. `Purged` (zero)
  and `Empty` (dirty) pages count alike, as `find` treats them.

## 24. No block at address 0

The `file:line`s are the `fn` line at this section's commit.

**Statement.** No slot, no huge run and no block an allocation may
return is at address 0. So a null pointer is never a block: an
allocating entry point's non-null result is its block and NULL is its
failure; a query or free of NULL that is not a documented no-op aborts;
a usable size is never 0 for a block, so 0 marks NULL; two live blocks
are at two addresses; and a throwing `operator new` never returns NULL.

```rust
// layout/regions.rs:199: `separated` now carries both bases
&&& forall|c: int| 0 <= c < NCLASSES ==> { ... &&& 0 < self.class_base[c] ... }
&&& 0 < self.huge_base

// layout/regions.rs:1134
pub proof fn lemma_block_nonzero(self, c: int, p: usize, len: int, size: int)
    requires self.wf(), self.separated(),
    ensures
        self.is_slot(c, p) ==> p != 0,
        self.is_huge_block(p, len) ==> p != 0,
        self.user_block(p, size) ==> p != 0,

// heap/mod.rs:559
pub proof fn theorem_no_block_at_zero(p: *mut u8, c: int, size: int)
    requires regions_ok(),
    ensures
        process_regions().is_slot(c, p@.addr) ==> p@.addr != 0,
        process_regions().is_huge_block(p@.addr, 1) ==> p@.addr != 0,
        process_regions().user_block(p@.addr, size) ==> p@.addr != 0,
        free_target(p) ==> p@.addr != 0,

// heap/live.rs:475, :494
pub proof fn lemma_nonnull(&self)                                  // Live
    requires regions_ok(), self.wf(),
    ensures self.addr() != 0, 0 < self.len(),
pub proof fn theorem_distinct(tracked &mut self, tracked other: &Live)
    requires regions_ok(), old(self).wf(), other.wf(),
    ensures final(self).addr() != 0, final(self).addr() != other.addr(), /* and theorem_apart's frame */

// vspec/usable.rs:179, :203, :237
pub proof fn lemma_usable_of_pos(p: *mut u8, n: int)
    requires regions_ok(), free_target(p), usable_of(p, n),
    ensures 0 < n,
pub proof fn lemma_usable_is_pos(p: *mut u8, size: int, n: int)
    requires regions_ok(), 0 <= size, process_regions().user_block(p@.addr, size), usable_is(p, size, n),
    ensures 0 < n,
pub proof fn theorem_stored_zero_iff_null(p: *mut u8, size: int, align: int, n: int)
    requires fits(p, size, align), 0 <= size,
        p@.addr != 0 ==> usable_is(p, size, n), p@.addr == 0 ==> n == 0,
    ensures n == 0 <==> p@.addr == 0,

// api/libc.rs:722 (usable_size), api/jemalloc.rs:436 (sallocx), :422 (xallocx)
fn usable_size(shared, p, blk)        ensures p@.addr != 0, 0 < r, /* and its U ensures */
fn sallocx(p, flags), xallocx(p, ..)  ensures p@.addr != 0, 0 < r,
// api/libc.rs:790, :1555, :1572, :1611
fn malloc_usable_size(p), free_usable(p), free_usable_block(p, blk)
                                      ensures p@.addr != 0 ==> 0 < r,
fn free_usable_slow(p, blk)           ensures 0 < r,

// api/cxx.rs:238 (new); :181, :218, :265, :285, :298 (oom, new_slow, new_block,
// new_aligned, new_aligned_block)
fn new(size)                          ensures r@.addr != 0,
fn oom, new_slow, new_block, new_aligned, new_aligned_block
                                      ensures !nothrow ==> r@.addr != 0,
```

**Covers / stated in.** The interface contract's rows 1 and 12-15
(`malloc(0)`, the `_usable` stores and queries), 9 (`malloc_usable_size`),
18 and 19 (`sallocx`, `xallocx`: NULL aborts), and 23-25 (`operator new`).
Every `fits` result and every `Live::gives` token (rows 1-8, 12-17,
23-25): a non-null result is the block `fits` names, and every `wf`
token (the one `gives` names for a non-null result) is at a non-zero
address. The fast
pops (`malloc_block`, `malloc_fast`, `malloc_usable_stored`, `:266`,
`:333`, `:1126`) prove their non-null results with it, in place of the
former `libc::lemma_slot_nonnull`.

**Proof.** `from_parts` (`layout/regions.rs:386`) proves `0 <
class_base[c]` from `0 < superblock` (`class_base[c]` is at least
`superblock + c * stride`), and requires `0 < huge.0`; `reserve`
(`:1903`) proves `0 < superblock` and `0 < huge.0` from the reserved
mappings' `addr > 0` (`try_reserve`'s `0 < a`, `:1634`), as
`align_base` and `slab_multiple` only add to the mapping's address. A
slot start lies at or above its class base (`lemma_slot_in_region`) and
a huge page start at or above the huge base. A slot's usable size is
its class's size, at least the quantum (`axiom_class_table_wf`); a huge
run's is at least one page (`run_bytes`, `huge_usable`). Two live
tokens' usable bytes are disjoint and each holds at least one, so their
addresses differ. `operator new`: `oom` returns a non-null block or,
with `nothrow`, NULL; without `nothrow` its exhausted loop is
`throw_bad_alloc` (`-> !`), and `malloc_fast`'s `Some(p)` is non-null
(a slot).

**Premises beyond the common ones.** None new: the kernel's `mmap`
contract (`try_reserve`, already a common premise) and the class table
premises (`axiom_class_table_wf`).

**Scope.**
- `malloc(0)` returns NULL only with `ENOMEM` (row 1's F); a non-null
  `malloc(0)` is a block (`fits`) with its own token
  (`malloc_block`'s `gives`), at least one usable byte and an address
  no other live block has (`theorem_distinct`): the distinct pointer
  the module doc of `api::libc` promises. That it is a class-0 slot is
  what the code does (`class_index(0)`), not stated.
- The `_usable` entry points: `*usable` is 0 exactly when the result is
  NULL (`theorem_stored_zero_iff_null` over the `_stored` twins'
  `ensures`); `malloc_usable_size` and `challoc_free_usable` return 0
  exactly for NULL.
- `sallocx`, `xallocx`: NULL aborts (proved; formerly an explicit test
  read off the code, "Bad input").
- Throwing `operator new` (unaligned and `align_val_t`): never NULL.
  With a non-power-of-two `align_val_t` and no `nothrow`, the two
  `ensures` (`r@.addr == 0` and `r@.addr != 0`) together say the call
  does not return: it throws `bad_alloc` or loops in the handler.

**Where the statement is weaker than the code.**
- `malloc(0)` being non-NULL is not stated: like every allocation it
  returns NULL on exhaustion, which has no spec. What is proved is
  that NULL is only the failure (with `ENOMEM`) and a non-NULL result
  is a distinct block.
- `theorem_stored_zero_iff_null` is a corollary over the `_stored`
  twins' `ensures`, not an `ensures` of theirs; it applies to each by
  its `fits`, `usable_is` and null-store conjuncts.
- `operator new`: whether a `new_handler` that never frees memory makes
  the loop run forever is not stated (`oom` has no `decreases`).
- The free side: `free` and the sized frees return for NULL as a
  documented no-op; that NULL is no free target says only that they
  cannot have acted on a block there.

## 25. Realloc keeps whenever it can

## 26. Cache routing

The
`file:line`s are the `fn`/`spec` line at this section's commit.

**Statement** (`api/libc.rs:896`, `:1072`, `:1137`, `:1242`, `:1308`,
`:1716`, `:1785`; `api/jemalloc.rs:344`):

```rust
// The keep rule, exactly: a slot of the class `class_for` picks, or a run
// start when no class serves `size` at `align`, `size` rounds up to the
// run's usable bytes `len` and `p` is a multiple of `align`.
pub open spec fn keeps(p: *mut u8, size: usize, align: usize, len: int) -> bool {
    ||| exists|c: int| process_regions().is_slot(c, p@.addr)
            && spec_class_for(size, align) == Some(c as usize)
    ||| process_regions().is_huge_block(p@.addr, 1)
            && spec_class_for(size, align) is None
            && huge_usable(size as int) == len
            && (p@.addr as int) % (align as int) == 0
}

fn old_size_or_keep(shared, what, p, size, align, blk: &Live) -> (r: Option<usize>)
    ensures r is None <==> keeps(p, size, align, blk.len()), ...
fn realloc_old(shared, p, size, blk: &Live) -> (r: (bool, usize))
    ensures r.0 <==> keeps(p, size, 1, blk.len()), ...
fn realloc_block(p, size, blk: Live, out, errno) -> (r: *mut u8)
    ensures p@.addr != 0 && size != 0
        ==> (r@.addr == p@.addr <==> keeps(p, size, 1, blk.len())), ...
fn realloc_usable_stored(p, size, usable, blk: Live, out, errno) -> (r: (*mut u8, _))
    ensures p@.addr != 0 && size != 0
        ==> (r.0@.addr == p@.addr <==> keeps(p, size, 1, blk.len())), ...
fn rallocx_zeroed(p, size, flags, blk: Live, out) -> (r: (*mut u8, _, _))
    ensures p@.addr != 0
        ==> (r.0@.addr == p@.addr <==> keeps(p, size, 1usize << (flags & 0x3f), blk.len())), ...

// The corollary: for `p`'s usable size `n` (`usable_of`), the rule keeps
// exactly a slot, or a run of more than MEDIUM_MAX usable bytes.
pub proof fn lemma_keeps_usable(p: *mut u8, n: usize)
    requires regions_ok(), free_target(p), usable_of(p, n as int),
    ensures keeps(p, n, 1, n as int) <==> slot_start(p) || n > MEDIUM_MAX,

// Composed as C calls it (verification only, cfg(verus_only)):
// n = malloc_usable_size(p) (usable_size with p's token), then realloc(p, n).
fn theorem_realloc_to_usable_size(p, blk: Live, errno) -> (r: (usize, *mut u8))
    requires may_syscall(), blk.claims(p), p@.addr != 0,
    ensures
        free_target(p),
        usable_of(p, r.0 as int),
        r.1@.addr == p@.addr <==> slot_start(p) || r.0 > MEDIUM_MAX,
```

(Abridged: the `ensures` above are added to the existing ones.) In
words: `realloc(p, size)` with `size != 0` returns `p` exactly when `size`
maps to `p`'s class (`class_of(size)` is `p`'s class: `size` lies in
`(size of the class below, size of p's class]`), or when `p` is a huge run
start, `size > MEDIUM_MAX` and `size` rounded up to whole pages is the
run's usable size; otherwise it moves (or fails, returning null). The
earlier `kept` (`:863`) remains the forward half: a kept block fits.
`rallocx` adds the alignment: a slot of the class `class_for(size, align)`
picks, or a run start that is a multiple of `align`. The move case is
`r != p` because the new block's token and `p`'s are apart
(`Live::theorem_apart`) and both have usable bytes (`Live::lemma_len_pos`,
`heap/live.rs:140`).

**Corollary and finding.** `realloc(p, malloc_usable_size(p))` keeps every
slot and every run of more than `MEDIUM_MAX` usable bytes, and **moves a
run of `MEDIUM_MAX` bytes or fewer**: its usable size maps to a class, and
the rule keeps a run only for a size no class serves. Such runs exist:
`aligned_alloc`, `memalign`, `posix_memalign` and `mallocx` with an
alignment above `MEDIUM_MAX` and a size of `MEDIUM_MAX` or less get a run
of the size rounded up to whole pages (one page for a size up to a
page), at most `MEDIUM_MAX` bytes (`class_for` returns `None` for any
`align > MEDIUM_MAX`). So
`p = aligned_alloc(512 KiB, 16)`, `n = malloc_usable_size(p)` (one page,
4096 on x86_64), `realloc(p, n)` returns a new slot, and so does every
`realloc` of `p` to a size a class serves. The move is correct (contents
are copied, the result fits; `realloc` owes no alignment beyond the
quantum), and jemalloc also moves a large extent to a small size, but
it is a block the allocator could keep. Reproduction:
`p = aligned_alloc(2 * MEDIUM_MAX, 16); realloc(p, malloc_usable_size(p))`
returns a different pointer;
`realloc_to_usable_size_keeps` checks the kept cases for every class and
three run sizes. Keeping it would change the run half of the rule (keep a
run whenever `huge_usable(size) == len`, whatever `class_of(size)` says),
an exec change on `realloc`'s path left to the coordinator.

**Proof.** `lemma_keeps_slot` (`:907`) and `lemma_keeps_run` (`:921`)
reduce `keeps` to its slot or run half (one class per slot,
`vspec::zero::lemma_slot_unique`; no slot is a huge page start,
`lemma_slot_not_huge`). The slot half is the test the code makes:
`class_for`'s `spec_class_for` postcondition, and for `realloc_old`'s
`class_of` `lemma_class_of_for` (`:1039`: the class at or above `size`
with every class below smaller is the least serving class at alignment
1, and none serves above `MEDIUM_MAX`). The run half is the code's
`usable_for(size) == Some(old)`: `lemma_usable_for_run` (`:980`) shows
`usable_for`'s value is `huge_usable(size)` whenever it is defined, and
that when `size` rounded up overflows no run's usable size equals it (a
run and its guard page lie in the huge region, so `old <= usize::MAX -
page`). `old` is `blk.len()` by `Huge::usable_size`'s token
postcondition. The corollary: a slot's usable size is its class's size,
which `lemma_class_size_mono` makes the least serving class; a run's is
whole pages (`run_bytes`), which round up to themselves, and maps to no
class exactly above `MEDIUM_MAX` (`lemma_class_for_one`, `:1020`, finds
a serving class below the last, whose size is `MEDIUM_MAX`).

**Premises beyond the common ones.**
- The C contract `heap::live::axiom_c_claim` (`heap/live.rs:679`): the
  token `realloc` takes for `p` is the live block's, so `blk.len()` is the
  usable size `malloc_usable_size` reads.
- `vspec::class::axiom_class_table_wf` (`vspec/class.rs:12`): the class
  table holds `spec_class_size`.
- Nothing else new: `class_of`, `class_for`, `Huge::usable_for` and
  `Huge::usable_size` are verified.

**Scope.** `realloc`, `reallocarray` and `challoc_realloc_usable` (through
`realloc_block` and `realloc_usable_stored`), and `rallocx` (through
`rallocx_zeroed`), for a non-null `p` they are allowed to free. `size ==
0` frees `p` (`realloc`) and is outside the rule there; `rallocx` applies
the rule to 0 too.

**Where the statement is weaker than the code.**
- It is stated on the `_block` cores, with the caller's token: the C
  entry points `realloc`, `realloc_usable` and `rallocx` make the token
  inside (`axiom_c_claim`), so their own `ensures` cannot name its
  `len`, and do not state the rule.
- `malloc_usable_size` takes no token, so its `ensures` (`usable_of`)
  does not pin a run's usable size to its descriptor's value; the
  composition goes through `usable_size` with `p`'s token
  (`theorem_realloc_to_usable_size`), which is what `malloc_usable_size`
  runs, with an empty token.
- `theorem_realloc_to_usable_size` composes the two calls in one
  function, with the same token for both, as `axiom_c_claim` would give
  it at each call; nothing ties two separate C calls (another thread
  could free `p` between them, which the C contract forbids).

**Model.** A heap's caches are its home headers (`Heap::home_hdrs`, one
per class) and the headers of the nodes on its `others` list
(`Heap::list`, ghost, head first). The ghost `Heap::keys`
(`heap/mod.rs:2844`) gives each node its key, `keys[i]` the `arena`
field of `list[i]`: the arena whose cache the node is (an explicit
`MALLOCX_ARENA` arena, or a home arena `thread.arena` moved away from),
or `FOREIGN` (`u16::MAX`, `heap/mod.rs:133`) for the one flush-only node
that plain frees of other arenas' slots go onto (design 6.3 step 4).
`Heap::list_ok` (`heap/mod.rs:2963`, part of `Heap::wf` and
`Heap::keeps`) now states that the keys are pairwise distinct and none
is the home arena (before, it said only that the node pointers are
distinct); `Heap::keeps` (`:2904`) adds that the keys keep their places
counted from the end, as the nodes do, and that the home arena is
unchanged. A pop is a call of `Heap::pop_top` (`heap/cache.rs:429`), the
one function that takes an entry off a cache stack to hand it out; its
callers are `Heap::pop_fast`, `Heap::pop`, `Heap::pop_compact` and
`Heap::pop_compact_run`. An explicit tcache (`MALLOCX_TCACHE`) is a
heap of its own (`Tcaches::get`), popped through the same functions.

**Statement** (`heap/mod.rs:3045`, `:3059`, `:3156`):

```rust
// `cache` is the header for `class` of `arena`'s cache in this heap.
pub closed spec fn routes(&self, arena: u16, class: int, cache: *mut CacheHeader) -> bool {
    if arena == self.arena { cache == self.home_ptr(class) }
    else { exists|i| 0 <= i < self.list@.len() && self.keys@[i] == arena
                     && cache == node_hdr(self.list@[i], class) }
}

// A header a pop may take from.
pub open spec fn poppable(&self, class: int, cache: *mut CacheHeader) -> bool {
    exists|a: u16| (a != FOREIGN || a == self.home()) && self.routes(a, class, cache)
}

pub proof fn theorem_cache_routing(&self, class: int, cache: *mut CacheHeader, a: u16, b: u16)
    requires self.wf(), 0 <= class < NCLASSES,
    ensures
        forall|r, s| 1 <= r < s <= self.list_len() ==> self.node_key(r) != self.node_key(s),
        forall|r| 1 <= r <= self.list_len() ==> self.node_key(r) != self.home(),
        self.routes(a, class, cache) && self.routes(b, class, cache) ==> a == b,
        self.poppable(class, cache) ==> forall|r, k|
            1 <= r <= self.list_len() && self.node_key(r) == FOREIGN && 0 <= k < NCLASSES
                ==> cache != node_hdr(self.node_from_end(r), k),

// Heap::pop (heap/cache.rs:365), Heap::pop_compact (heap/compact.rs:183),
// Heap::pop_compact_run (:282):
    requires old(self).poppable(class as int, cache),

// Heap::other_caches (heap/others.rs:417):
    ensures
        r@.addr != 0 ==> final(self).keyed(r, arena),
        r@.addr == 0 ==> !final(self).has_key(arena),
// Heap::cache_slot (heap/others.rs:618), Heap::ensure_cache (heap/cache.rs:817):
    ensures r@.addr != 0 ==> final(self).routes(arena, class as int, r),

// Heap::alloc_in (heap/paths.rs:198), Heap::alloc_other (heap/others.rs:284):
    requires arena != FOREIGN,
// Heap::alloc_slow_in (heap/paths.rs:474):
    requires arena != FOREIGN || arena == old(self).home(),
```

(Line breaks and triggers dropped.) In words: every pop takes its slot
from the home cache, from the `others` node keyed by the requested
arena, or (an explicit tcache being a heap) from one of those of the
explicit cache; the `FOREIGN` node is never popped; and a heap's
`others` keys are pairwise distinct and never its home arena, so the
header a pop site routes to is the requested arena's cache and no
other's. The pop sites:
- `Heap::alloc_in` (`MALLOCX_ARENA` on the thread's heap or an explicit
  cache): the home header when `arena` is the home arena
  (`Heap::lemma_routes_home`), else `alloc_other`.
- `Heap::alloc_other`: the class's header in the node `other_caches`
  found for `arena` (`Heap::lemma_keyed_routes`: a node keyed `arena`
  routes it, and `arena` is then not the home arena).
- `Heap::alloc_slow_in`: the header `ensure_cache` routes `arena` to,
  after a refill (`Heap::lemma_keeps_routes`); its callers pass the home
  arena, arena 0 (a shared class) or `alloc_in`'s/`alloc_other`'s
  `arena`.
- `Heap::alloc_slow`: the home header (the scheduled compacting pop).
- `Heap::pop_fast` (`heap/paths.rs:101`, the `malloc`/`calloc` fast
  path): `cache_ptr(class)`, the home header (`cache_ptr`'s `ensures`,
  `heap/cache.rs:329`). This one is read off its body: `pop_top` takes
  the header's permissions, not the heap, so it carries no requires.
- `pop`, `pop_compact` and `pop_compact_run` pop the `cache` they were
  handed, also after a `tick` or refill (`Heap::lemma_keeps_poppable`,
  `lemma_poppable_same`).

**How the keys are kept.**
- `other_caches` walks the list reading each key (`node_arena`) and
  returns the first node keyed `arena`; the loop carries that the
  visited keys are not `arena`, so a creation adds at the head a key no
  node has. `assert_not_home`, discharged by `other_caches`' requires
  `create ==> arena != home`, makes the new key not the home arena.
- `Heap::set_arena` (`heap/mod.rs:4626`): `other_caches(arena, true)`
  returns the node keyed `arena`, the only one; after the header
  exchange (`swap_hdrs`, which keeps `same_shape`, now with the keys and
  the home arena) the node's key becomes the old home, which no node
  had, and the home becomes `arena`, which no other node has. When no
  node could be had, `other_caches` ensures no node is keyed `arena`,
  and `drop_home_caches` now ensures `same_shape`.
- Creation starts with no keys (`thread::create_heap_for_thread`,
  `Heap::lemma_fresh_wf`).
- `Flags::decode` (`heap/mallocx.rs:103`) now ensures an explicit arena
  is below `0xfff`, so `mallocx::alloc` (`:155`, `requires arena matches
  Some(a) ==> a != FOREIGN`) and `api::jemalloc::alloc_with`
  (`api/jemalloc.rs:198`) discharge `alloc_in`'s requires.

**`FOREIGN` is released whole.** Proof assertions, not a contract: in
`Heap::free_slow_in` (`heap/paths.rs:546`) a full `FOREIGN` cache is
flushed by its capacity, after which `cached_len(cache) == 0` (from
`flush`'s `ensures`, `len` decreasing by `n`), before the push; in
`Heap::gc_arena` (`heap/mod.rs:4498`) a `FOREIGN` node's caches are
flushed by their length, likewise to 0.

**Explicit non-claim: arena purity.** Routing does not give "every slot
`malloc` returns belongs to the heap's home arena" (nor the explicit
arena's, for `MALLOCX_ARENA`). A sized free (`free_sized`, `sdallocx`,
`free_aligned_sized`: `heap::free_sized_fast`, `heap/paths.rs:724`)
pushes onto the home cache without loading the owner word (design 6.3,
the sized path, step 2), so the home cache can hold another arena's
slots, and its pops hand them out. The plain `free` routes another
arena's slot to the `FOREIGN` node (theorem "Arena isolation" covers the
bitmap side: every release goes back to the slot's own arena).

**Premises beyond the common ones.**
- `Heap::node_arena` (`heap/others.rs:244`, `external_body`) now
  ensures the value it reads is the node's ghost key. Its writers record
  what they store: `node_link` (`:102`) in `other_caches` (the new node,
  at the head of the list and the keys) and `node_set_arena` (`:152`) in
  `set_arena`; `destroy` frees the nodes with the heap.
- Every heap a verified path is handed is `Heap::wf` (common premises),
  now including the keys.

**Where the statement is weaker than the code.**
- The arena a pop serves is linked to the requested one at the pop
  sites (`routes` from `other_caches`/`ensure_cache`, and the home
  header), not by a requires of `pop`: `pop` takes no arena, and a ghost
  argument would change its signature. `poppable` itself admits any
  arena's cache except `FOREIGN`'s.
- `pop_fast` is covered by reading its body (one `cache_ptr` call), not
  by a requires.
- The release of the whole `FOREIGN` cache is a proof assertion in two
  bodies, not an `ensures`.
- Per heap reference: as in "`MALLOCX_TCACHE_NONE` bypasses thread
  caches", Verus does not relate two references to the same heap, and
  an explicit cache's heap is assumed `wf` afresh at each
  `Tcaches::get`.
- The home arena of a heap is not constrained, so `poppable` admits the
  home header whatever the home arena is. It is never `FOREIGN` in
  practice, but by run-time checks and inspection, not by Verus: a
  thread heap's comes from `assign_home` (below `MAX_ARENAS`);
  `set_arena` requires one below `MAX_ARENAS`, but its only caller is
  the trusted `api::mallctl::heap_set_arena` wrapper, whose caller
  checks `thread.arena`'s value at run time; and `Tcaches::create` takes
  any `arena` (its callers pass the thread's home arena or 0).
- Routing names the header a pop takes from, not the arena a refill
  inside the pop draws from: `pop_compact` and `pop_compact_run` refill
  an emptied cache through `shared.arena(self.hdr_ref(cache).arena)`,
  the header's own `arena` field, which `ensure_cache` sets and
  `swap_hdrs` moves with the header, but which no spec ties to the
  routed key.

## 27. Stats identities and epoch coherence

The
`file:line`s are the `fn` line at this section's commit.

**Statement.** Two parts; the first is proved, the second is **false**
today and is stated as what holds instead.

1. **Identities** (proved). Every `ArenaStats` an arena stores, and
   every one `collect_arena`, `collect_into` and `collect_all` compute,
   satisfies `ArenaStats::identities` (`stats.rs:364`):

   ```rust
   small_allocated <= small_active && large_allocated <= large_active
   resident == small_active + large_active + small_dirty + large_dirty
               + huge_dirty + metadata
   retained == small_retained + large_retained + huge_retained
   mapped == resident + retained
   ```

   so `resident >= active + metadata`, `active >= allocated` and
   `mapped == resident + retained` per arena. Every `Snapshot` stored in
   `shared.stats` satisfies `Snapshot::inv` (`:581`): with up to 256
   arenas, `Totals::identities` (`:413`), the same three relations for
   the totals. `collect_into` (`:1100`) and `collect_all` (`:1269`) also
   ensure the identities, and `mapped` equal to the arenas' `mapped`
   summed, whenever that sum fits a `usize` (`sum_mapped`, `:1056`),
   for any number of arenas. The contracts:

   ```rust
   // Arena::stats (arena.rs:1021), stats::arena_stats (stats.rs:1231)
   ensures r.identities()                 // r matches Some(a) ==> a.identities()
   // Arena::store_stats (arena.rs:1033)
   requires s.identities()
   // stats::snapshot (:1217), stats::collect (:1205)
   ensures r.inv()                        // narenas <= 256 ==> totals.identities()
   // collect_arena_with (:883)
   ensures r.identities(), r.mapped <= arena_mapped_max(r.index), r.index == arena.spec_index()
   // collect_into (:1100); collect_all (:1269) likewise for its sum
   ensures r.narenas == rd.0.len(), forall j: rd.0[j].identities(),
       sum_mapped(rd.0) <= usize::MAX ==> r.totals.mapped == sum_mapped(rd.0) && r.totals.identities(),
       r.inv()
   ```

2. **Coherence** (false; what holds). The intended claim: after an
   `epoch`, the stored per-arena figures sum to the stored totals (they
   come from one collection), and `mallctl("epoch")` returns the number
   its own refresh stored. Both fail with concurrent epoch writers:
   - `collect_into` calls `arena.store_stats(&a)` for each arena inside
     its scan and takes `shared.stats.lock()` only afterwards, for the
     totals. Writers A and B scan concurrently; B stores arena 0, A
     stores arena 0, A stores its totals, B stores its totals: arena 0
     holds A's figures, the totals B's. A concurrency test
     (16 arenas, one thread allocating and freeing, two
     epoch writers per round) fails within 20 rounds on every run (5 of
     5 here), e.g. arenas summing to `resident` 1130496 against stored
     totals of 819200. A reader between one epoch's arena stores and its
     snapshot store sees the same mix even with a single writer.
   - The `Epoch` arm of `mallctl::ctl` (`api/mallctl.rs:1878`) calls
     `stats::epoch` and discards the number it returns, then reads
     `stats::snapshot(shared).epoch`, which another writer may have
     bumped in between. A concurrency test
     (four threads, 2000 epoch writes each) reads
     the same number back twice on every run (7988 to 7999 distinct of
     8000). jemalloc holds `ctl_mtx` across the refresh and the read.

   The tests go with the upstream fix rather than here; the fix is
   exec code (store the arenas' figures and the snapshot under one lock,
   or serialise `epoch`; return `stats::epoch`'s value), which this
   theorem does not change. What each call does by construction (read
   from the body, not stated as an `ensures`): `collect_into` sums into
   the totals exactly the figures it stores for each arena (the same
   local `a`, read once), and stores and returns one `Snapshot` under
   one hold of the snapshot lock, so `stats::epoch` returns the epoch
   number its own call stored. Its `ensures` ties only the totals'
   `allocated` (`totals_exact`) and `mapped` to `rd`, not to what was
   stored.

**Covers.** `mallctl` `stats.{allocated,active,metadata,resident,
retained,mapped}` and `stats.arenas.<i>.*` (from `snapshot` and
`arena_stats`), `malloc_stats_print` (`collect`, `collect_arena`),
`mallinfo2` and `malloc_stats` (`collect_all`). Not covered:
`stats.arenas.4096.*` (`arena_stats_all`, `:1242`) sums stored figures
that each satisfy the identities, but carries no bound on how many it
sums or ghost record of them, so its result has no identity contract.

**Why no figure wraps.** The figures are added with `fig_add`/`fig_mul`,
exact only when the result fits (theorem 22). Each sum is bounded:
- per bin, `collect_bin` (`:790`) ensures `BinStats::fits` (`:209`):
  `curregs * size <= curslabs * slab_size`, from the clamp
  (`ClassState::bin_stats`, `class/mod.rs:2393`, now ensures
  `curregs <= curslabs * nslots`) and `nslots * size <= slab_size`
  (`Class::wf`); and `(curslabs + empty + purged) * slab_size <= 2^38`,
  the slabs being among the class region's
  (`ClassState::nslabs_are_the_region_s`, `lg_region <= 38`);
- over at most 64 classes, the slab bytes are at most `2^44`;
- a huge run count and page count come from the halves of the `large`
  word, below `2^32` (`Arena::large`, `arena.rs:1004`, now ensures it),
  and a page is at most `2^22` (`lg_page <= 22`,
  `Regions::separated`), so an arena's
  huge bytes are below `2^54`;
- arena 0 adds the free extents' pages, at most `u32::MAX`
  (`Huge::stats`, `huge/mod.rs:3459`, now ensures it from the huge lock
  invariant), and the committed metadata, at most the region's length,
  below `meta::va_limit() = 2^57` (`Meta::stats`, `meta.rs:2602`, now
  ensures it).
So arena 0's `mapped` is below `2^58` and any other's below `2^55`
(`arena_mapped_max`, `:379`); every per-arena sum fits. The scan visits
arena `i` at index `i` (`arena_at`, `:865`, now ensures it), so arena 0
is counted once and `k` arenas sum below `2^58 + k * 2^55`, which fits
for `k <= 256`.

**Premises beyond the common ones.**
- The two `lock_inv` axioms (`axiom_arena_stats_lock_inv`,
  `vspec/arena.rs:43`, and `axiom_snapshot_lock_inv`, `stats.rs:590`)
  now say the stored value satisfies `ArenaStats::identities` and
  `Snapshot::inv` (they said "any value" before). Their writers:
  `Arena` initialisation (`ArenaStats::empty`, which ensures the
  identities) and `Arena::store_stats` (which requires them);
  `Shared::init` (`Snapshot::EMPTY`, `heap/shared.rs:1169`) and
  `collect_into` (which asserts it before releasing the guard). The
  fork handlers take the raw locks only and write nothing. As for every
  lock, that each holder leaves the invariant true is checked at the
  holder's last write, not at the guard's drop (`lock.rs`, `lock_inv`).
- `process_regions()` is the `Shared`'s regions (`Shared::wf`), so
  `collect_bin`'s new `requires process_regions().wf()` holds for every
  caller in `stats.rs` (the unit tests call it unverified).

**Scope and where the statement is weaker than the code.**
- Beyond 256 arenas the totals' identities hold when the arenas'
  `mapped` sum fits; it always does for the memory it describes, but
  the bound is per arena, not global (two arenas' `large` words could
  in the model each claim `2^54` bytes).
- The figures hold whatever the counters' values: the identities are
  about how the figures are formed, not about whether `curregs` or the
  `large` word are the live blocks (theorem 22 does that at a quiescent
  state). A clamped `curregs` (`bin_stats`) keeps the identities but is
  not exact.
- `pactive` and `pdirty` (`active / page`, `dirty / page`) and the bins'
  counters are outside the identities.
- Coherence across concurrent epochs, and between an epoch's arena
  stores and its snapshot store for a concurrent reader, is false
  (part 2).

## Interface contract

The per-call theorems above (1-3, 5-7 and the "Bad input" section)
checked against challoc's exported C ABI; the whole-allocator ones are
the internal rows below (I1-I7: theorems 1, 4, 5, 7, 19, 8 and 10). One
row per exported symbol (`je_` twins and C++ array forms share their
row), one column per interface property. Read from the `ensures` of
each entry point's Rust function at `ccbf1dd`; the `file:line`s in this
section are the `fn` line at that commit (the sections above are at
`b2cad68`, or their own commit). The U column, row 13's Z and row 22's F were revised
later, by the usable-size work (gap 3 below), the R cells of rows 3,
4, 14 and 17 by the huge `kept` bound (gap 6), and the F column by the
`errno` work (gap 2); items they added are cited by name. All the
trusted premises listed above apply to every cell. Every `extern "C"`
wrapper is in `verus!` with no contract of its own and only casts (and,
for an entry point that may write `errno`, lends it the calling thread's
`errno` token, *(errno)* below; the ghost `grant_syscalls` of theorem 8
runs nothing), except `malloc_stats_print`. So the Rust function's
`ensures` is the symbol's contract, and the verified parts of its body
(code inside `verus!` without `external_body`) cannot panic or
overflow. That is not all of every body: the `mallctl` rows (34) reach
the caller's memory only through `external_body` helpers, the report
rows (32, 33) write through the trusted `emit` and sinks, row 33 is
outside `verus!`, and every row rests on the trusted functions and
axioms of the common premises. Where that function is `.0` of a `_zeroed` twin (`calloc`,
`challoc_calloc_usable`, `mallocx`), the Z cell rests on the twin's
`ensures`: the zero token is ghost, so the exported function returns
the same block. Likewise the U cells of rows 12 and 14 rest on the
`_stored` twins' `ensures` (`malloc_usable_stored`,
`realloc_usable_stored`) and row 13's on `calloc_usable_zeroed`'s third
result: `malloc_usable` and `realloc_usable` themselves state nothing
about `*usable`. The D cells, the *(live)* B cells, row 15's U, row
17's Z and the F cells of rows 3, 4 and 14 (the old block's token
given back on a failed move; row 4 reaches `realloc_block` through
`realloc`) rest on a
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
- **Z**: the zeroing entry points return a `Zeroed` token covering the
  bytes the API promises zero (theorem 6: token coverage at return, not
  proved contents); plain `calloc` covers only its `n * m` bytes.
- **C**: contents are preserved (realloc family).
- **F**: failure semantics: NULL, `errno`, return code, the old block
  intact after a failed realloc, overflow in `calloc`/`reallocarray`.
- **B**: bad input. A call that returns satisfies the pointer-shape
  checks (`free_target`) and the selected argument checks (size hints,
  cache ids of `MAX_TCACHES` or more, alignments); for other input the
  call does not return normally (the code aborts; reaching `SIGABRT`
  rather than hanging is not proved), or it gets the documented error
  result or no-op. Pointer shape needs no C contract for the call
  itself (a non-block pointer's token claims nothing); it, like every
  theorem, holds only in a run that has made no earlier non-live free,
  which `axiom_c_claim` excludes. Since theorem 7 a free target is
  never in a guard slab, so a free into one does not return. The
  *(live)* cells (a stale or double free) stay `p`: the free cores
  require the token of a live block and ensure that what they freed was
  live, but C holds that token only by `axiom_c_claim` (the `free`
  contract), so a non-live free is excluded by that C-boundary
  assumption rather than proved to abort. The "Bad input" section above
  lists every kind of bad input by entry point, with what is proved not
  to return (pointer shape, size hints, alignments, cache ids of
  `MAX_TCACHES` or more) and what rests on the C contract.
- **U**: size consistency: when `mallocx(size, flags)` succeeds,
  `nallocx(size, flags)` equals its usable size (not unconditionally:
  `nallocx(1, MALLOCX_LG_ALIGN(63))` returns one page, 4096 with 4 KiB
  pages, while `mallocx` returns NULL, where jemalloc's `nallocx`
  returns 0); `sallocx` and `malloc_usable_size` give the block's real
  usable size.

Codes: `P` proved, `p` partial, `-` missing, `.` not applicable.

| #  | Entry point                                 | R | D | Z | C | F | B | U |
|----|---------------------------------------------|---|---|---|---|---|---|---|
| 1  | `malloc`                                    | P | P | . | . | P | . | . |
| 2  | `calloc`                                    | P | P | P | . | P | . | . |
| 3  | `realloc`                                   | P | P | . | - | P | p | . |
| 4  | `reallocarray`                              | P | P | . | - | P | p | . |
| 5  | `aligned_alloc`, `memalign`                 | P | P | . | . | P | P | . |
| 6  | `posix_memalign`                            | p | p | . | . | p | P | . |
| 7  | `valloc`                                    | P | P | . | . | P | . | . |
| 8  | `pvalloc`                                   | p | P | . | . | P | . | . |
| 9  | `malloc_usable_size`                        | . | . | . | . | P | p | p |
| 10 | `free`                                      | . | . | . | . | P | p | . |
| 11 | `free_sized`, `free_aligned_sized`          | . | . | . | . | P | p | . |
| 12 | `challoc_malloc_usable`                     | P | P | . | . | P | . | P |
| 13 | `challoc_calloc_usable`                     | P | P | P | . | P | . | P |
| 14 | `challoc_realloc_usable`                    | P | P | . | - | P | p | P |
| 15 | `challoc_free_usable`                       | . | . | . | . | P | p | P |
| 16 | `mallocx`                                   | P | P | P | . | p | p | . |
| 17 | `rallocx`                                   | P | P | P | - | p | p | . |
| 18 | `xallocx`                                   | . | . | . | . | . | p | p |
| 19 | `sallocx`                                   | . | . | . | . | . | p | p |
| 20 | `dallocx`                                   | . | . | . | . | . | p | . |
| 21 | `sdallocx`                                  | . | . | . | . | . | p | . |
| 22 | `nallocx`                                   | . | . | . | . | P | . | P |
| 23 | `operator new`, `new[]`                     | P | P | . | . | P | . | . |
| 24 | `new`, `new[]` (`nothrow_t`)                | P | P | . | . | p | . | . |
| 25 | `new`, `new[]` (`align_val_t`, 4 forms)     | P | P | . | . | p | p | . |
| 26 | `delete`, `delete[]` (unsized, 8 forms)     | . | . | . | . | . | p | . |
| 27 | `delete`, `delete[]` (sized)                | . | . | . | . | . | p | . |
| 28 | `delete`, `delete[]` (sized, aligned)       | . | . | . | . | . | p | . |
| 29 | `malloc_trim`                               | . | . | . | . | P | . | . |
| 30 | `mallopt`                                   | . | . | . | . | P | . | . |
| 31 | `mallinfo`, `mallinfo2`                     | . | . | . | . | . | . | . |
| 32 | `malloc_stats`, `malloc_info`               | . | . | . | . | P | . | . |
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
- *(errno)*: the model is `vspec/errno.rs`. One ghost resource,
  `ThreadErrno`, stands for the calling thread's `errno` for one call;
  `val()` is the value challoc last wrote to it. A token comes only from
  `lend` (`external_body`, no `ensures`: the caller's `errno` is
  unknown), which each export wrapper that may write `errno` calls once
  before the call, and the only write is `set_errno` (`external_body`,
  `ensures final(errno).val() == e`), which takes it `&mut`; the copy in
  `api/print.rs` is gone. Every Rust function between a wrapper and a
  `set_errno` takes `Tracked<&mut ThreadErrno>` and, except the
  `operator new` paths (rows 23-25, which only pass it on), states the
  final value: `enomem` and `einval` ensure `ENOMEM` and `EINVAL`, and
  each allocating entry point ensures `r@.addr == 0 ==>
  final(errno).val() == ENOMEM` (rows 1, 2, 7, 8, 12, 13), with the
  documented exceptions stated: `EINVAL` for a bad alignment (row 5),
  `realloc(p, 0)`'s null with `errno` unchanged (rows 3, 4, 14),
  `posix_memalign`'s untouched `errno` on `EINVAL` (row 6). So every
  null an allocating entry point returns is a failure that set `errno`,
  and every non-null one is a block (`fits`). The fast pops return no
  null: a slot is never at address 0 ("No block at address 0",
  `heap::theorem_no_block_at_zero`, from `Regions::separated`'s
  `0 < class_base[c]` and `0 < huge_base`, which `from_parts` proves or
  requires and `reserve` proves from `try_reserve`'s `0 < a`, the
  kernel's `mmap` contract). A function that is not handed the token
  (`free`, the sized frees, the `*allocx` family) cannot reach
  `set_errno`, so a caller's token is unchanged across it; `realloc`'s
  `p != 0 && size == 0 ==> final(errno).val() == old(errno).val()` is
  that frame for `free`, proved. The premises: `lend` is called only by
  export wrappers, once per call, which nothing but a `grep lend` checks
  (verified code that called it could write `errno` without the
  caller's token); a nested call (an allocation from inside libc or a
  C++ `new_handler`) gets its own token from its own wrapper. And the
  token models challoc's own writes, not libc's. Trusted code calls libc
  functions that may write `errno` (`getauxval`, `pthread_key_create`
  and `pthread_atfork` during init, `pthread_setspecific` at heap
  creation, `pthread_attr_*`, `pthread_create` and `pthread_join` in
  `purge::background::start`, `fwrite` in `malloc_info`'s sink). A
  stated failure value holds at return because each `set_errno` is the
  last thing its path does (its failure result, a null, `-1` or the
  error code, is returned right after). A function without the token
  leaves `errno` alone only if its path makes no such libc write or
  undoes it: `free`'s path never runs init (`shared_if_ready`), glibc's
  `pthread_setspecific` does not touch `errno` on a path that returns
  (a failed second-level allocation sets `ENOMEM`, and challoc then
  aborts), and `background::start` saves and restores it around its
  calls (`errno_read`, `errno_write`, trusted, outside the model).
- *(live)*: `free_target` (`heap/mod.rs:432`, theorem 3) is proved. A
  call with a pointer that is not a slot start or a huge page start does
  not return. A free core takes the caller's token for `p` (`Live::claims`) and ensures
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
   F: `r@.addr == 0 ==> final(errno).val() == ENOMEM` (*(errno)*): every
   NULL is a failure that set `ENOMEM`, and no block is NULL ("No block
   at address 0"). A non-NULL `malloc(0)` is a distinct block, at an
   address no other live block has (`Live::theorem_distinct`), as the
   module doc promises. Not stated: NULL only on exhaustion (exhaustion
   has no spec), so `malloc(0)` may be NULL with `ENOMEM` like any
   allocation.
2. `calloc` (`api/libc.rs:328`, `.0` of `calloc_zeroed`, `:341`). R:
   `fits(r, n * m, QUANTUM)`. Z: `r.1@.covers(r.0, n * m)` (theorem 6).
   F: `n * m > usize::MAX ==> r@.addr == 0` and `r@.addr == 0 ==>
   final(errno).val() == ENOMEM`, so an overflow is NULL with `ENOMEM`.
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
   modelled. F: `p != 0 && size == 0 ==> r@.addr == 0` with `errno`
   unchanged (the frame across `free`, *(errno)*), every other NULL with
   `ENOMEM`, and the core (`realloc_block`) gives the caller back its own
   token for `p` on a NULL from the move (`p` is still live; the code
   returns before `heap::free`). B: `p != 0 ==> free_target(p)`, *(live)*.
4. `reallocarray` (`api/libc.rs:733`). As `realloc` for `n * m`. F also
   proves `n * m > usize::MAX ==> r@.addr == 0 && final(errno).val() ==
   ENOMEM`. B: `free_target(p)` only when `n * m` does not overflow (an
   overflowing call returns NULL without looking at `p`).
5. `aligned_alloc`, `memalign` (`api/libc.rs:1228`). R:
   `is_pow2(align) ==> fits(r, size, align)`. B:
   `!is_pow2(align) ==> r@.addr == 0 && final(errno).val() == EINVAL`.
   challoc's `memalign` rejects a non-power-of-two alignment, unlike
   glibc's, which rounds the alignment up (the module doc names the
   `EINVAL`). F: a NULL for a power-of-two alignment comes with `ENOMEM`.
6. `posix_memalign` (`api/libc.rs:1251`). R: `alloc_aligned`
   (`:1146`) ensures `fits(p, size, align)`, but `store_out` (`:1243`,
   `external_body`) claims nothing about `*out`. F: the return code is
   `0`, `ENOMEM` or `EINVAL`; `EINVAL` leaves `errno` unchanged, and
   `ENOMEM` comes from `alloc_aligned`'s `enomem`, so it also sets
   `errno = ENOMEM` (the function's doc says only that the `EINVAL` path
   leaves `errno` alone; the module doc's "leaves `errno` alone" is wider
   than the code). Not stated: `0` iff an allocation was stored, and
   `*out` untouched on failure (gap 4). B:
   `r == EINVAL <==> !(is_pow2(align) && align % 8 == 0)`.
   D: `posix_memalign_block` ensures, on a `0` return, the token of a
   block of at least `size` bytes aligned to `align`, but not that it is
   the one stored through `out` (`store_out`), hence `p`.
7. `valloc` (`api/libc.rs:1297`). R: `fits` at the page
   (`1 << lg_page`). F: NULL with `ENOMEM`.
8. `pvalloc` (`api/libc.rs:1306`). R: `fits(r, size, page)`. The
   page-rounded size the code allocates is not stated. F: NULL with
   `ENOMEM`, the `checked_add` overflow included.
9. `malloc_usable_size` (`api/libc.rs:565`). F: `p == 0 ==> r == 0` and
   `p != 0 ==> 0 < r` (0 exactly for NULL, "No block at address 0"). B:
   `p != 0 ==> free_target(p)`, *(live)*. Before init, a non-NULL `p`
   aborts (`free_before_init`, `:508`). U: `p != 0 ==> usable_of(p, r)`,
   exact for a slot, the descriptor's shape for a huge run, *(U)*.
10. `free` (`api/libc.rs:386`). B: `p != 0 ==> free_target(p)`, *(live)*;
    NULL is a no-op (early return; not in the `ensures`). F: `errno`
    preservation (the module doc's promise): `free` is not handed the
    `errno` token, so it cannot reach `set_errno`, and a caller's token is
    unchanged across it (proved where verified code calls it: `realloc`
    and `realloc_usable` with `size == 0`). It is `P` by construction:
    it rests on *(errno)*'s premises, that `lend` is called only by
    the export wrappers (a `grep`) and that no libc call on `free`'s
    path writes `errno` unrestored. A thread's first `free` may create
    its heap before the abort (theorem 3).
11. `free_sized`, `free_aligned_sized` (`api/libc.rs:580`, `:591`). B:
    `free_target(p)`, *(live)*. The size hint is proved against the
    slot's class: `free_sized` ensures `p != 0 ==> hint_ok(p,
    spec_class_for(size, 1))` and `free_aligned_sized` the same for
    `spec_class_for(size, align)`, or `None` for a non-power-of-two
    alignment, so a slot freed with another class's size, or with a bad
    alignment, aborts ("Bad input"). The slow path checks it
    (`Shared::check_size_hint`, which ensures `hint_ok`); the fast
    path (`free_sized_fast`) pushes only a slot of the hinted class, and
    its liveness is checked when the cache flushes. F: as `free` (no
    token).
12. `challoc_malloc_usable` (`api/libc.rs:798`, `.0` of
    `malloc_usable_stored`). R, D, F: as `malloc`. U: the `Stored` token
    of the last store through `usable` has `ptr() == usable` and, for a
    non-null result, `usable_is(r, size, val())`: the class's size, or
    `size` rounded up to whole pages for a huge run (*(U)*, from the
    run token `alloc_huge` returns); for a null result, `val() == 0`
    (the fast pop's slot is not null, `theorem_no_block_at_zero`). So
    `val() == 0` exactly for NULL (`theorem_stored_zero_iff_null`: a
    block's usable size is positive, "No block at address 0"). That the
    class is `nallocx(size, 0)`'s (the smallest that holds `size` bytes)
    is true of `class_index` but not exported here.
13. `challoc_calloc_usable` (`api/libc.rs:890`, `calloc_usable_zeroed`,
    `:905`). R, F: as `calloc`. Z: the token covers `n * m` bytes and
    `covers_usable(r.1, r.0, 0, n * m)`, the whole usable size the doc
    promises and the code zeroes (`write_zero(p, u)`, or the known-zero
    token of `heap::alloc_zeroed` or `alloc_huge`). For a huge run the
    `memset` length `u` is the usable size of the block's own token
    (`usable_size` with the token `alloc_huge` returned,
    `huge_usable(n * m)`), so no trusted premise remains
    (`axiom_owned_run` is gone). U: as row 12, for `n * m`, and the null
    result's 0 is stated (0 exactly for NULL, as row 12).
14. `challoc_realloc_usable` (`api/libc.rs:969`, `.0` of
    `realloc_usable_stored`). R, D, C, F, B: as `realloc` (the keep
    decision is `realloc_old`, `:1010`). U: as row 12. A kept block's
    size is `realloc_old`'s, which ensures `usable_of(p, old)` and, when
    kept, `usable_is(p, size, old)` (for a run, `usable_for(size) ==
    Some(old)` is the keep test); a moved block's is
    `malloc_usable_stored`'s.
15. `challoc_free_usable` (`api/libc.rs:1055`). F: `p == 0 ==> r == 0`
    and `p != 0 ==> 0 < r`, so 0 exactly for NULL ("No block at address
    0").
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
    22). F: NULL on failure with `errno` untouched (`alloc_with`'s doc):
    `mallocx` is not handed the `errno` token (*(errno)*), so it cannot
    write it from verified code; the first call runs init, whose libc
    calls are *(errno)*'s premise. NULL itself is not stated beyond
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
    size, which a kept block keeps. C, F, B: as `realloc`, except that
    `errno` is untouched (no token, as row 16); NULL `p` allocates.
18. `xallocx` (`api/jemalloc.rs:332`). Never resizes; returns
    `sallocx(p, 0)`. B: `free_target(p)` and `p@.addr != 0`
    unconditionally, so NULL aborts. U: `usable_of(p, r)` and `0 < r`,
    as row 19.
19. `sallocx` (`api/jemalloc.rs:340`). B: `free_target(p)` and
    `p@.addr != 0` unconditionally: NULL aborts (`not_an_allocation`,
    `:135`), now stated ("No block at address 0"); *(live)*.
    U: `usable_of(p, r)` and `0 < r`, *(U)*. For a slot `mallocx(size, flags)`
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
    gives when `mallocx` succeeds. The converse fails: `nalloc_usable`
    ignores the alignment for a huge request, so `nallocx(1,
    MALLOCX_LG_ALIGN(63))` returns one page (4096 with 4 KiB pages) while
    `mallocx` returns NULL (no alignment past the huge region can be
    served; jemalloc's `nallocx` returns 0 there). A non-zero `nallocx`
    therefore does not promise that `mallocx` succeeds. The two calls
    agree because `class_for` ensures
    `r == spec_class_for(size, align)` for a power-of-two `align`, the
    smallest class that serves the request (`least_class`, from
    `class_of`'s and `class_index`'s new minimality: no smaller class
    holds `size` bytes), and `Huge::usable_for` is `Huge::spec_usable`.
    F: `r == 0` exactly when no class serves the request and `size`
    rounded up to a page overflows (class sizes and `huge_usable` are
    positive); `mallocx` then returns NULL (`alloc_huge` ensures
    `huge_ok(size)` of a non-NULL run).
23. `operator new`, `new[]` (`api/cxx.rs:214`). R:
    `fits(r, size, QUANTUM)`. F: the C++ contract, never NULL: `new`
    ensures `r@.addr != 0` ("No block at address 0"), from `new_block`,
    `new_slow` and `oom`'s `!nothrow ==> r@.addr != 0` and the fast
    pop's non-null slot. `oom` returns a pointer only after a non-null
    test or, with `nothrow`, NULL; otherwise it leaves through
    `throw_bad_alloc` (`-> !`, which throws `std::bad_alloc` or aborts
    when libstdc++ is absent). `oom` has no `decreases` (it waits on the
    program's handler).
24. `new` `nothrow_t` (`api/cxx.rs:226`). R: as row 23. F: NULL after
    the handler loop; not stated beyond `fits`.
25. `new` `align_val_t` (`api/cxx.rs:260`). R: `is_pow2(align) ==>
    fits(r, size, align)`. F: the throwing forms as row 23
    (`new_aligned` ensures `!nothrow ==> r@.addr != 0`); `p` because
    the `nothrow_t` forms' NULL is not stated beyond `fits` (row 24). B: a
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
    verified bodies. F (`malloc_info`; `malloc_stats` returns nothing
    and states nothing): `malloc_info` ensures `r == 0 || r == -1`,
    `r == -1 <==> options != 0 || stream` is null, and `r == -1 ==>
    final(errno).val() == EINVAL` (*(errno)*); a `0` may leave `errno`
    changed by the sink's `fwrite`, as glibc's may. The sinks
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
    the body. Theorem 21 (MIB round-trip) states that a name and its MIB
    reach the same node with the same indices (on the cores, not the
    exports).
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
| I2 user blocks apart from metadata       | P    | th. 4, heap/mod.rs:317 |
| I3 uniqueness (no slot handed out twice) | P    | theorem 5              |
| I4 guard pages never handed out (slabs)  | P    | th. 7 (section 7)      |
| I4 guard pages, huge (lock invariant)    | p    | th. 7 (section 7)      |
| I5 purge never discards live data (huge) | P    | th. 19 (section 19)    |
| I5 purge never discards live data (slab) | p    | th. 19 (section 19)    |
| I6 no syscall on the cache fast paths    | P    | th. 8 (section 8)      |
| I7 destructor loses no cached slot       | p    | th. 10 (section 10)    |

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
- I5: theorem 19 (lines as in section 19). Every flush discards
  only bytes its batch's ghost `free` admits (`purge::discard`). Huge
  purges admit pages of no live run, proved against the run tokens
  (`RunToken::theorem_purge_apart`, `Live::theorem_purge_huge`). Class
  purges admit slabs the arena's class lock value records as idle
  (`class::decay::idle_byte`), plus the run-time `Empty`, `live() == 0`
  check in `walk_empty`; nothing ties idleness to the slot tokens (the
  descriptor's `live` counter is not modelled), so that row stays `p`.
  Theorem 6's `axiom_discarded_zero` premises still lean on that link
  and on the `Purged` state's history.
- I6: theorem 8 (lines as in section 8). The cache fast paths of rows
  1, 10, 11 (`free_sized`), 15, 21 (no flags), 23, 24, 26 and 27, and
  row 2's pop, make no system call; everything else they do is behind a
  call that requires `may_syscall()`. Other rows may enter the kernel.
- I7: theorem 10 (`Heap::flush_all`, lines as in section 10): the flush
  `destroy` runs first empties every home and node header and appends
  exactly what they held to the released log, each slot's token handed
  to `release_routed` with it. `destroy` itself is trusted, section
  10's "weaker" list applies, and that every heap gets a destructor is
  not proved (with an invalid thread key none does), hence `p`.

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
2. **`errno` as ghost state**: done for rows 1-5, 7, 8, 10-14 and 32
   (F to `P`), *(errno)*. `set_errno` ensures the value it writes to
   the `ThreadErrno` token the export wrapper lends (`lend`); the
   allocating entry points ensure `r@.addr == 0 ==> final(errno).val() ==
   ENOMEM` (or `EINVAL`, or unchanged, where the API says so), and the
   frees hold no token. The hot set's machine code is unchanged (the
   token is ghost). Rows 3, 4 and 14 are `P` with gap 1: their cores
   also give the caller back the old block's token on a failed move.
   Left: `posix_memalign`'s `*out` (row 6, gap 4); rows 16 and 17's
   untouched `errno` and rows 10 and 11's rest on the premise that no
   libc call in trusted code on their paths leaves `errno` changed
   (init's, for the `*allocx` family's first call).
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
   a call that frees nothing). The 0 that `challoc_malloc_usable` and
   `challoc_realloc_usable` store for NULL is stated since gap 2, and
   since "No block at address 0" a stored or returned 0 means NULL
   exactly (`theorem_stored_zero_iff_null`; the queries' `p != 0 ==>
   0 < r`).
4. **Failure-path contracts**: `r@.addr == 0 ==> p` still live for the
   realloc family is now in the cores (`realloc_block`,
   `realloc_usable_stored`, `rallocx_zeroed` give back the caller's
   token); what is left is `posix_memalign`'s `*out` through a `PointsTo`
   for `store_out` (row 6). `!nothrow ==> r@.addr != 0` for `operator new`
   is done ("No block at address 0": row 23's F to `P`).
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
7. **Purge and guards**: the purge half is done for huge runs and
   stopped at the class lock for slabs (theorem 19): every
   `purge::Batch` carries a ghost predicate its flushes discard within;
   huge batches admit only pages outside the run-page authority, so no
   run token's pages (I5 huge `P`); class batches admit only slabs idle
   in the class lock's ghost state, and the link from
   idleness to the slot tokens needs the descriptor's `live` counter as
   ghost-tracked state (I5 slab `p`). Theorem 6's purge premise is not
   discharged. The guard half is done for slabs (`!guard_slab` is in
   `is_slot`, theorem 7); for huge runs, `Huge::alloc_run`'s result now
   comes with its run token (gap 1); state `theorem_run_guards` per
   result over that token (I4, huge row).
8. **Bad input**: done as far as the C contract allows ("Bad input"
   above). Rows 16 and 25 go from `-` to `p` (cache ids of
   `MAX_TCACHES` or more abort; a non-zero non-power-of-two `align_val_t`
   gets no block), and rows 11, 21, 27 and 28 gain `hint_ok`: a slot
   freed with another class's size aborts. The *(live)* cells stay `p`:
   a stale or double free of a slot or huge page start is excluded by
   `axiom_c_claim`, not proved to abort. Left: model arena and cache
   existence (rows 16, 17, 20, 21) and tie a huge run's size to the
   sized frees' hint, which the code does not check either.
