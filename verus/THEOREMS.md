# Whole-allocator theorems: statements and premises

This file states what challoc's proofs guarantee about the allocator as
a whole: each property in plain words, the Verus statement that proves
it, the entry points it covers, every trusted premise it rests on, and a
sketch of the proof. [RESULTS.md](RESULTS.md) summarizes the same
guarantees, with the coverage figures and the trust base.

## How this file is organized

The numbered sections 1 to 46 each state one property. Section 45,
fast-path cost bounds, is a machine-level statement over the release
binary that a disassembly checker (`scripts/perf-budget.sh`, the
`perf-budget` check) proves, not Verus. "Bad input" (the B column of the
interface contract), the
[theorems outside the numbered sections](#theorems-outside-the-numbered-sections),
four of which have their own sections after section 46 (thread exit,
explicit-cache destroy, exact sized frees and the double-free window),
and the interface contract are not numbered. The sections fall into the
groups [RESULTS.md](RESULTS.md#what-the-proofs-guarantee) uses:

| Group | Sections |
| --- | --- |
| Returned blocks and allocator metadata | 1, 2, 4, 5, 7, 24, 41; overflow reach and commits that avoid guards |
| Free paths and bad input | 3, 25, 43, 44; Bad input; exact sized frees and the double-free window (opt-in keys) |
| Caches, slabs and huge runs | 5, 11, 14, 17, 20, 23, 26, 28, 29, 34, 36, 37; explicit-cache destroy |
| Purge and decay | 18, 19, 30, 31, 38, 40 |
| Locks, fork and threads | 9, 10, 13, 15, 16, 33, 39; thread exit |
| The C boundary and system calls | 8, 21, 32, 42, 45, 46; secure-exec configuration; interface contract |
| Zeroing and usable size | 6, 12 |
| Statistics | 22, 27, 35 |

## How to read a section

Each section opens with the property in plain words. Its title and first
paragraph claim only what is proved: where a theorem holds only at a
lock invariant, rests on a source test or on the C-boundary assumption
(`axiom_c_claim`), the title or the first paragraph says so. Then come:

- **Statement:** the Verus `theorem_*` function, lemma or contract that
  proves it, and its file under `src/`.
- **Entry points:** the exported functions the statement covers.
- **Premises:** the trusted facts the proof uses beyond the common
  premises below, each named by item; a seam's contract and status are
  in its ledger file under `verus/ledger/src/`.
- **Where the statement is weaker than the code:** what the code does
  that the statement does not claim.
- **Proof sketch:** how the proof goes.

Citations name an item and its file. Facts marked **unconfirmed** are
read from comments and not checked against the proof. Verification-only
functions are `cfg(verus_keep_ghost)` and absent
from the shipped build. A function that takes a lock also takes
`Tracked<&mut LockOrder>` arguments, which the abridged signatures below
leave out (theorem 33). Remaining work is tracked in
[wolfi-dev/challoc#397](https://github.com/wolfi-dev/challoc/issues/397).

## Theorems outside the numbered sections

- **Secure-exec configuration** (`config::linux::theorem_secure_conf`,
  `config.rs`): in a secure-execution process (`AT_SECURE` set) no call
  of `env_var` returns, so `config::load` reads neither `MALLOC_CONF`
  nor `CHALLOC_CONF`. That `env_var` is the only reader of the
  environment is a source test (`environment_has_one_reader`,
  `config.rs`), not Verus. The theorem does not state that the loaded
  `Config` equals the one with neither variable set.
- **Overflow reach** (`heap::theorem_overflow_reach`, `heap/mod.rs`):
  between any byte of a block that `fits` and any byte of the metadata
  region, the `Shared` static, another class region or the huge region,
  above or below it, lies a `MAX_SLAB` (4 MiB) run of challoc's own
  reservation outside every region (`Regions::slack_run`). Its scope and
  trusted facts are in [RESULTS.md](RESULTS.md#exploit-techniques).
- **Commits avoid guards** (`class::table::theorem_slab_commit_avoids_guards`,
  `class/table.rs`; `huge::theorem_huge_commit_avoids_guards`,
  `huge/mod.rs`): every `mprotect` read-write commit of a class region
  stays in the region and shares no byte with a guard slab, except one
  starting in the range's first page for which an `MADV_GUARD_INSTALL`
  of that page succeeded (the marker stays; the rest of the slab is
  committed). Commits of a class in `thp:always`'s PMD-chunk mode, which
  places no guards, are claimed only to stay in the region. Every huge
  commit or `MADV_GUARD_REMOVE`, made holding the huge lock (its cell's
  permission, from the caller's guard), stays in the huge region and
  shares no byte with the guard page of a live run of the lock's value
  other than the one being freed, whose token has come back. Freed runs'
  guards are not covered. Scope and trusted facts are in
  [RESULTS.md](RESULTS.md#exploit-techniques).
- **Thread exit** ([Thread exit never reuses a destroyed heap](#thread-exit-never-reuses-a-destroyed-heap-rests-on-the-slot-premise)):
  `heap::thread::theorem_exit_no_rebuild`, `heap/thread.rs`.
- **Explicit-cache destroy** ([A lookup holding destroy's bound finds no destroyed explicit cache](#a-lookup-holding-destroys-bound-finds-no-destroyed-explicit-cache-rests-on-vspecentrys-write-list)):
  `heap::tcaches::theorem_tcache_destroyed_none`, `heap/tcaches.rs`.
- **Opt-in exact sized frees** ([Sized frees run plain `free`'s check under `exact_sized_free`](#sized-frees-run-plain-frees-check-under-exact_sized_free-rests-on-the-c-boundary-assumption)):
  `heap::paths::theorem_exact_sized_free`, `heap/paths.rs`.
- **Opt-in double-free scan** ([Double-free window under `double_free_scan`](#double-free-window-under-double_free_scan-rests-on-the-slot-state-premise)):
  `heap::slotstate::theorem_double_free_window`, `heap/slotstate.rs`.

## Configuration

| Axis | What is verified |
|---|---|
| Cache mode | thread caches (challoc has no per-CPU mode) |
| Quantum | `quantum16` (default) and `quantum8` |
| Target | x86_64 and aarch64, each with both quanta |
| PAC | on or off: `heap::pac_mode()` reads the uninterpreted `shared_at(shared_storage())` |
| Build | `cfg(not(test))`; test-only items (`Meta::new`, `*_t`) excluded |

Because `pac_mode()` (`heap/mod.rs`) reads the uninterpreted `shared_at(shared_storage())`, every proof
holds for both values.

Every combination runs in CI: the `verus` and `verus-arm64` checks
(`.chainguard/ci/verus.yaml`) each verify the default `quantum16` build
and then the `quantum8` build. On x86_64 `PUSH_PAC` is never set, but
the proofs do not use that.

## Common premises

Every theorem below rests on these. Later sections list only what they
add.

**One process, one `Shared`.**
- `layout::process_regions` (`layout/regions.rs`) and
  `heap::pac_mode` (`heap/mod.rs`) are opaque, defined as the
  regions and `PUSH_PAC` bit of `shared_at(shared_storage())`
  (`heap/shared.rs`), which is uninterpreted;
  `heap::shared::lemma_process_shared` (`heap/shared.rs`) reveals
  them.
- The kernel's page, `getauxval(AT_PAGESZ)` (the `getauxval`
  specifications, `sys/auxv.rs`), is `1 << process_regions().lg_page`:
  init reserves the regions with `sys::features::page_size()`
  (`Regions::reserve_owning`), and a process's `AT_PAGESZ` never
  changes. The page check in `purge::discard` rests on it; it also
  makes `page_size`'s abort unreachable.
- `api::shared_ref` (verified) ensures `r.wf()`; `wf`'s
  `regions == process_regions()` conjunct (`heap/shared.rs`, in `built`, which `wf` includes) ties
  it to the process. It reads the published value through the
  permission `STATE`'s invariant holds from `READY` on
  (`api::Published`, see "One-shot tokens" below). That `init_slow`
  runs `init` once before the `READY` release store, and that nothing
  writes a field `wf` reads afterwards, is proved from narrower
  premises: `axiom_state` (one invariant, its
  initial state), the `STATE` wrappers (under sequential consistency),
  and `heap::shared::axiom_shared_built`'s join, whose `shared_at`
  carries the "nothing unverified writes it" assumption. `shared` and
  `shared_if_ready` load `READY` and get the permission. The fact
  that it was published travels in
  contracts as the uninterpreted `api::ready()`: `Shared::wf` includes
  it (`Shared::built` is what `Shared::init` proves), and so do
  `Heap::wf` and the TLS slot token holding a heap
  (`heap::thread::slot_ok`); `api::axiom_ready` states it of a thread
  holding a `Published`, and `api::axiom_published` gives a thread that
  has it the frozen permissions back (both trusted: a `ready()` fact
  reaches a thread only through a load of `READY` or the publication,
  or from a thread it runs after, carried along in contracts, and no
  linear permission to the frozen statics exists once one does). The
  trusted items that hand out a `Shared::wf` or `Heap::wf` they did not
  get from a verified caller assert it too: `sys::tls::TlsSlot::entry`,
  `tcaches::heap_at` and `purge::background::shared_arg`.
  `mallctl::shared` is not one: it takes the `api::ReadyMark` of the
  `READY` its caller saw, `requires api::marks_ready(*mark)`, which
  `mallctl::ctl` requires and passes to every reader, and reads through
  `api::shared_marked`. The fast paths' accessor, `api::shared_deref`
  at the pointer `shared_base` gives, is verified that way, as are
  `heap::thread::fast_flags` and `heap::paths::shared_of`.
  The feature line (written not by `init` but at its first request)
  has its own once-cell invariant (`api::LinePred`,
  ONCECELL): `init` makes it with `LINE_STATE`'s permission
  (`api::LinePerm`, from `api::axiom_statics`, through the wrappers
  `line_load`, `line_compare_exchange` and `line_store`) and the
  permissions to `FEATURES` and `FEATURE_LINE`, and publishes it with
  `STATE` (`api::Published::line`); a request reaches it through
  `axiom_published`, from the `ready()` in its `shared()`'s
  `Shared::wf`. `api::line_ready` (verified) hands the permissions to
  the winner of `LINE_STATE`'s exchange from `LINE_UNBUILT`, which
  builds the line through them (`api::build_line`, verified) and
  freezes them into the invariant at its release store of
  `LINE_READY`; a request that loads `LINE_READY` gets the frozen
  permissions and reads the line through them (`api::published_line`,
  verified). The deferred cgroup record (`sys::cgroup::Deferred`) has
  the same protocol over its phase (`sys::cgroup::PhasePred`, through
  `vspec/byte.rs`) and its cell (a vstd `PCell`): `Deferred::get`
  reads the record through the permission frozen at `DONE`, and
  `Deferred::published` (`Shared::cgroup_now`) through the one a
  waiting `Deferred::discover` hands back. Each
  cell's fork-child reset puts back the permission a vanished thread
  of the parent held (`api::axiom_line_orphaned`,
  `sys::cgroup::axiom_cell_orphaned`), with a named residue: a signal
  handler that forks while its own thread builds (discovers) leaves
  the permission twice in the child.
- One more trusted source of `Shared::wf`:
  `purge::background::shared_arg` (`purge/background.rs`, the
  background thread's argument, `external_body`). `heap::paths::shared_of`,
  a heap's `&Shared`, reads the heap's `shared` field through the
  frozen permission, and `api::mallctl::shared`, the readers' accessor,
  requires `api::marks_ready(*mark)` and reads through
  `api::shared_marked` (`api/mod.rs`); neither is trusted.
- One `Shared`, at its storage: the pointer the cold paths hand
  `existing_heap`, and the one `heap::init_fields` (verified) stores in
  the heap's `shared` field (recorded by `FreshHeap::shared`, which
  `Heap::wf` states is the storage), are `heap::paths::shared_ptr`'s
  (`heap/paths.rs`, verified), which in the library is the static's:
  `api::shared_ptr` (`api/mod.rs`), trusted only as the projection of
  `SHARED`'s cell (`external_body`; Kani
  `api::kani_statics::shared_ptrs_agree`). No Verus fact ties the
  caller's well-formed `&Shared` to that pointer. That the heap found
  through it and the `Shared` the caller works through are one object
  is unverified: it rests on the exec code building one `Shared`, at
  its storage (`Shared::init(shared_storage_ptr(), ..)` in `api::init`,
  `api/mod.rs`), the same fact the opaque `process_regions` and `pac_mode` rest on for the
  regions and the PAC mode. The unit tests build other `Shared`s and
  take the `cfg(test)` `shared_ptr`, `ptr::from_ref(shared)`; the
  premise is about the shipped library.
- `Shared::init` (`heap/shared.rs`) proves
  `r ==> shared_at(this).built()`, using the tracked `Built` token,
  the trusted `begin` (`external_body`: the
  permission to the storage, for `Step::Shared`'s token), and verified
  in-place writers, each through its field's permission over the
  trusted `axiom_shared_layout`, `axiom_shared_regions`
  and `axiom_shared_meta`: `put_head`,
  `put_flags`, `put_table`, `put_line` and
  `lines_ptr` (the class lines'),
  `put_huge`, `arena0_ptr`, `put_registries`,
  `put_cgroup`, `put_policy` and
  `put_tail`. Arena 0 is built the same way, verified:
  `put_head`, `put_sets`, `put_class` and `put_tail` (`arena.rs`).
- `api::shared_storage_ptr`, `features_ptr`, `line_ptr` and
  `layout::classes_ptr` (`external_body`): raw-pointer projections of
  the statics, at `Shared::storage()`, `features_storage()`,
  `line_storage()` and `classes_storage()`.
- One-shot tokens (`vspec/once.rs`) and `STATE`'s invariant
  (`api::axiom_state`, `api::StatePred`): the invariant holds `STATE`'s
  permission (`api::StatePerm`, through the wrappers `state_load`,
  `state_claim` and `state_publish`), every step's `Once` token while
  `STATE` is `UNINIT`, and from `READY` on the permissions `init` froze
  (`api::Published`: the built `Shared`, from `axiom_shared_built`'s
  join, the class table, whose value is `layout::class_table()`
  by the definitional `layout::axiom_class_table` on the
  frozen permission, and the line's invariant; not the feature
  statics, which the feature line's first request completes and
  writes through the line's invariant, `api::LinePred`). It also holds
  one half of the ghost variable at `api::ready_mark()`, set and frozen
  at the `READY` store (`publish_ready`); a thread that loads `READY`
  gets the frozen half, `api::ReadyMark` (`marks_ready`). `api::claim_init`
  (verified) is `init_slow`'s compare-exchange from `UNINIT`, and takes
  the tokens out of the invariant for its winner, which `init` passes
  on: `Step::Statics` to `api::axiom_statics` (the class table and
  feature statics' storage and `LINE_STATE`'s permission; `init` writes `FEATURES` through its
  permission and puts it, with the line's and `LINE_STATE`'s, into the line's
  invariant), the rest to `Shared::init`.
  `Shared::begin`, `table::axiom_fresh_words` (one token per class) and
  `axiom_statics` each use up their step's token, so Verus checks that
  each runs at most once per process (a second call does not verify:
  `verus/audit/invariants.md`, "Per seam"). That the exchange
  succeeds once is proved given `axiom_state` (one invariant; it holds
  the tokens only in `UNINIT`, which nothing writes); what stays
  assumed is that premise, `axiom_state`'s initial state, the
  wrappers (trusted, partly checked by the loom models of
  `tests/loom/src/init.rs`; `verus/ledger/src/api/mod.rs.md`) and that
  `STATE` has no other writer (the source test
  `access_init_once_words_written_twice` in `tests/loom/tests/access.rs`
  checks that its only writes in shipped code are `state_claim`'s and
  `state_publish`'s).

**The `free` contract.** `heap::live::axiom_c_claim`
(`heap/live.rs`) is the one trusted step at the C ABI boundary: for
a pointer C passes to a free or a realloc, it gives a `Live` token that
claims it (`Live::claims`): the slot's token if the pointer is a
slot start, the token of the run that starts there if it is a huge page
start, nothing otherwise. The allocations hand C the token of every
block they return (`Live::gives`), so C, which holds the block,
holds the token. It is called once per C call, by the Rust entry point
the export calls, before anything else: `libc::free`, `free_sized_hint`
(`free_aligned_sized` and the aligned sized `delete`s; `free_sized`, and
so `operator delete(void*, size_t)` and its array form, claim in
`free_sized` itself, which hands the token to `free_sized_with`, as
`sdallocx` does),
`realloc`, `realloc_usable`, `free_usable`, and `jemalloc::rallocx`,
`dallocx`, `sdallocx`; every verified core below them takes the token as
a parameter. Verified code cannot call it otherwise: it uses up the
linear token of one C call (`vspec::cbuf::CCall`, the type of the
C-boundary buffer tokens), of a free or a realloc of the pointer
(`CEntry::Frees`, `CCall::block`), which no verified code builds
(private fields, no constructor) and which only the `extern "C"` bodies
mint, each module's `exports` with its own private `axiom_c_call`
(`api/libc.rs`, `api/jemalloc.rs`, `api/cxx.rs`, each
`requires e is Frees`), for the body's own pointer argument; the buffer
calls' mints (`axiom_c_call` in `api/mallctl.rs` and
`api/print.rs`, `requires e is Mallctl || e is NameToMib || e is
ByMib` and `e is StatsPrint`) exclude that entry, and each entry point
above requires its token's block to be its `p`. Once per body, for that
body's `p`, and no other maker of a free's token rest on convention
(`axiom_c_call`'s doc comment), not on Verus and not on a source test.
That no Rust code calls a minting export is checked by a line scan:
`api::tests::exports_return_their_callees_result` (`api/mod.rs`)
fails if a line outside a comment names an `exports` module (a path
through one, or a `use` of one). A double free, or a free of a slot or
huge page start that is not a live block, gives C a token it does not
hold, which makes the proof context inconsistent at that call: it voids
theorem 3 for that free and every theorem for later calls. A pointer
that is neither a slot start nor a huge page start gets no token and
the call does not return (theorem 3, which is not voided for it).

**Every heap a verified path is handed is `Heap::wf`.**
- `Heap::wf` (`heap/mod.rs`): `CachePerms::wf`, `tracks`,
  `zero_ok`, `tokened`, `claimed`, `regions_ok()`, `list_ok`, `home_ok`
  and `homed_ok` (the arena ticket); also `ready()` and that
  its `shared` field is the process's `Shared` at its storage.
- Assumed by `tcaches::heap_at` (`heap/tcaches.rs`, with its home
  headers at the pointer, `Heap::home_at`, and its
  nodes' block permissions and claims, `others::block_ok`;
  `external_body`; it also takes the caller's `HeapUse` token,
  which `heap::tcaches::axiom_heap_use`, `heap/tcaches.rs`, mints
  once per call that reaches an existing heap, so one token gives one
  heap reference at a time), and, for the calling thread's heap, by the slot
  token `sys::tls::TlsSlot::entry` (an axiom): a token whose
  slot holds a heap holds the heap's linear permission
  (`heap::thread::HeapPerm`, abstract: a `vstd` `PointsTo<Heap>` would
  own the home headers' bytes a second time, next to the heap's own
  `CachePerms`) for the heap the value names, with the heap well formed
  (`heap::thread::slot_ok`). `thread::heap_from_slot` (verified)
  authenticates the value and borrows the heap from the token
  through the trusted `heap::thread::slot_heap_ref`;
  `create_heap_for_thread` puts a new heap's permission in the token
  (`tcaches::heap_new`, over the trusted `HeapPerm::assemble`, which
  states what `init_fields`, `mark_bound` and `mark_rehome` stored).
- Heap creation is verified, the stores to `bound` and `rehome`
  included (`mark_bound`, `mark_rehome`, `heap/fresh.rs`, and
  `init_fields` are verified): `Heap::create` (`heap/mod.rs`),
  `create_detached`. Teardown, `destroy`, is verified
  (its heap block over `heap::axiom_heap_block`).
- No writer escapes the permissions: `Heap::set_arena`
  (`heap/mod.rs`) swaps the home headers through their permissions
  and ensures `wf`; `Heap::free_caches` writes a bare array and
  is used only by the unit tests, the heap's own arrays going through
  `Heap::free_caches_in`, which keeps the permissions.
- Trusted, but stated over the permissions: the slice primitive under
  `stack_mut` (`vspec::slice::slice_mut_elems`, the slice over exactly
  its entries' `PointsTo`s, which the verified `slice_mut_from_perms`
  carves out of the stack's; `stack_mut` itself is verified over it),
  `Heap::home_ptr_at` (`heap/cache.rs`, the home header's address,
  `heap + 128 + 64 * class`, under the verified `cache_ptr`, on
  the `malloc` and `free` fast paths, which proves from `wf` that the
  header is owned and caches `class`; partly checked by Kani,
  `verus/ledger/src/heap/cache.rs.md`). The `others` list
  readers `node_caches` and `node_caches_detached` are verified, over a shared view of the node's headers' permissions
  as one array (`vspec::ptr::axiom_array_view`). The nodes' `arena` and
  `next` fields go through permissions the heap holds (`Heap::heads`,
  `others::heads_ok`, part of `list_ok`): `node_link`, `node_next`,
  `node_arena` and `node_set_arena` are verified. The list's length
  bound is proved (`Heap::lemma_list_bound`: its keys are distinct
  `u16`s).
- `<[T]>::sort_unstable`'s contract (`vspec/slice.rs`: a permutation;
  a Kani record checks it for slices of up to 12 entries), under the
  flush's kept-half sort and the fallback of the verified
  `flush::group_by_slab` (whose result is a permutation of the batch,
  so the flush's tokens follow it).
- `thread::fast_flags` (verified): the `PUSH_PAC` bit, read
  through the permission frozen at `READY` at the pointer
  `api::shared_ptr`, `api::shared_base` or `heap::paths::shared_ptr`
  gives, is `pac_mode()`.
- The `free` fast path's dispatch byte (`vspec/flags.rs`;
  `Shared::free_flags`, verified over it with no precondition,
  ensures `r & !FREE_ROUTE == self.route_fixed()`): a `RouteByte`, whose `AtomicU8` is private to `vspec/flags.rs`,
  so the wrappers `new`, `load` and `fetch_or` (each `external_body`,
  trusted and partly checked: Kani on one thread,
  `kani_proofs::atomic::flags_route_byte`, and the loom models of
  `tests/loom/src/flags.rs` on copies of the protocol;
  `verus/ledger/src/vspec/flags.rs.md` lists what neither checks) are its only
  operations; they keep the byte's bits other than `ROUTE`
  (`FREE_ROUTE`) at the value `new` made (`flags::fixed`), which
  `Shared::wf` ties to `push_flags`: `init` makes the byte from
  `push_flags` (`put_flags`), and the only other write is
  `set_multi_arena`'s `fetch_or(FREE_ROUTE)`, whose `requires` Verus
  checks. So one `Relaxed` load is `push_flags` up to the `FREE_ROUTE`
  bit (`r & !FREE_ROUTE == route_fixed()`, which `Shared::wf` makes
  `flags() & !FREE_ROUTE`), for a reader that
  happens after `init` (what the `api::ready()` in `Shared::wf` stands
  for, under "One process, one `Shared`" above). With `Shared::wf` the
  byte's `PUSH_PAC` bit is `pac_mode()` and its `PUSH_ZERO` bit is
  `flags()`'s (`Shared::lemma_route_flags`, `heap/shared.rs`, which
  states both); both are what `heap_from_slot` and the push need.
- TLS asm: `sys::tls::get` (`sys/tls.rs`), `get_tp` (aarch64; the x86_64
  one is verified and returns 0) and `set`: a read returns the `TlsSlot` token's value
  (and the aarch64 `get_tp` the token's thread pointer,
  `TlsSlot::tp`) and `set` makes the written value the token's and
  leaves its ghost fields alone (theorem 15), nothing more.

**Class table.**
- `ClassTable::table` (`layout/mod.rs`, `external_body`): returns
  `class_table()`, trusting that the table `init` left is `table_ok`
  (as `adopt_page_geometry` keeps it). (Its readers
  include `Shared::init`, before `READY`, and the `Deref` impl, which
  can state no precondition, so it does not read through the frozen
  permission.)
- `layout::regions::class_entry` (verified, `requires
  api::ready()`): the entry, through the class table's permission
  frozen at `READY` (`api::axiom_published`), its element
  (`vspec::ptr::axiom_array_elem`, trusted: the layout of arrays and
  that a frozen permission to one gives one to each element), whose
  value is `class_table()` (`layout::axiom_class_table`, trusted,
  definitional), at the pointer the x86_64 barrier returns unchanged
  (`entry_barrier`, an empty `asm!`; trusted, partly checked:
  `make emptyasm` shows it assembles to no instruction, and the
  pointer's provenance is not checked,
  `verus/ledger/src/layout/regions.rs.md`).
- Every entry of `class_table()` is `wf` and sized `spec_class_size`
  (`vspec::class::lemma_class_table_wf`, verified): `class_table()` is
  the table after init when that table is `table_ok`, and otherwise a
  spec table that is `table_ok` (`layout::spec_ok_table`).
  `adopt_page_geometry` (verified) writes the table through the
  permission `api::axiom_statics` gives and keeps `table_ok`, which
  `api::Published` freezes at `READY`, and `layout::axiom_class_table`
  requires it.

**Class lines.** The `free` fast paths read each
class's slot geometry, bitmap and owner bases, hand-out group and a
copy of its hand-out watermark from `Shared::lines` (`heap::ClassLine`),
which `Shared::wf` states are the class's (`ClassLine::wf`, with the
table's guard mask stated beside it in `Shared::built`: what
`ClassLine::of` writes from the regions, the class table and the
table; `init` freezes the lines' permissions as it does the tables',
and `axiom_shared_built` states them). The copy is proved bounded and
trusted only in how it is reached:
- its bound (at most the table's slab count, which bounds the bitmap
  and owner words the fast path then reads through the table's
  permissions) is an atomic invariant in the line (the watermark's
  `class::table::CommittedPred`), and every store into it is verified
  inside it (`SlabTable::mirror_watermark`, `take_group`);
- the table stores through a raw pointer to the copy:
  `vspec::atom::usize_ptr` (a reference-to-pointer cast,
  `external_body`) names the atomic (`usize_id_at`), and
  `vspec::atom::store_usize_at` (`external_body`) stores through the
  pointer with that atomic's permission (both trusted, partly checked:
  Kani on one thread compares addresses and so leaves the pointer's
  provenance unchecked, and loom models of the watermark mirror check
  `store_usize_at` on a copy; `verus/ledger/src/vspec/atom.rs.md`);
  that the pointer stays valid
  is `Shared::init`'s contract (the `Shared` never moves);
- that a slab below the copy has its bitmap words and owner word
  committed (what `Unbacked::into_lazy` states of a slab below the
  watermark; `table::load_owner`'s witness shows the owner word stored)
  rests on
  the copy never being ahead of the watermark: each store into it is
  of a value the watermark held, under the commit lock and after the
  watermark's own store. Read off the code, not proved.
- the sized check reads the line's last eight
  bytes as one `u64`, `ClassLine::guard_word` (`external_body`: a
  type-punned load, and on x86_64 an empty `asm!`; trusted, partly
  checked by a Kani harness, a const assert and `make emptyasm`,
  `verus/ledger/src/heap/shared.rs.md`), whose contract is
  the word the fields make (`guard_mask | tz << 32 | lg_guard << 40`,
  the padding zero, which `ClassLine::wf` states); the line's 64-bit
  inverse (`inverse_of_odd_part_u64`, verified) and `Shared::region_hi`
  (written by `init`, stated by `Shared::wf`) are proved.

**Kernel `mmap`.**
- `linux::try_reserve` (`layout/regions.rs`): a reservation is
  non-zero and ends below `sys::pac::user_va_limit()`.
- `Reserved::is_disjoint` (`layout/regions.rs`), `into_unbacked`
 : live reservations are apart; the metadata range is minted
  once as `Unbacked`.
- `Unbacked::commit` (`vspec/unbacked.rs`, `external_body`): a
  committed range becomes a `PointsToRaw` (the `mprotect` contract); the
  source of every metadata permission (`meta.rs`,
  the last a table carved from a committed reservation; also
  called on the part of a committed range past the
  deferred tables in `Inner::commit_bump_to`, and on a just-carved
  table's deferred part after `Inner::flush` in
  `Inner::reserve_committed`), with `Unbacked::into_lazy` for the
  tables committed by their owners.
- `sys::pac::axiom_user_va_limit` (`sys/pac.rs`): the limit is at
  most `2^48`.

**Locks.** `lock::axiom_acquired`: an acquisition takes the permission
of the lock's cell (a vstd `PCell`) out of the free lock's invariant,
and the value satisfies `lock_inv` and `lock_frame`; the other entry is
`lock::axiom_held`, behind `Lock::get_unlocked`. The guard's `deref` and
`deref_mut` are verified, over two trusted restatements of vstd's
`PCell::borrow` and `borrow_mut` (`cell_ref`, `cell_mut`, kept for the
machine code); `drop` (`lock.rs`) is `external_body` and checks
nothing. `LockGuard::unlock` (`lock.rs`, verified, empty body: the
release is the guard's drop at its end) requires `lock_inv` and
`lock_frame` of the value. Every holder of the metadata lock outside the tests ends with it, on every path to a
return (`Meta::alloc`, `alloc_raw`, `alloc_zeroed`, `free`, `reserve`,
`reserve_table`, `commit`, `reserve_committed_raw`, `stats`), and so
does every holder of the explicit-cache table lock (`Tcaches::create`,
`destroy`), the slab-set tables' lock (`SetTable::ensure`), the huge
lock (`Huge::alloc_aligned`, `free_run`, `usable_size`, `lookup`,
`purge_empty`, `purge_curve`, `empty_bytes`, `stats`), the
process statistics lock (`collect_into`, `snapshot`,
`arena_stats_all_into`), the arena creation lock (`Arenas::assign`,
`create`, `materialise`), the per-arena statistics locks
(`Arena::stats_into`, `add_stats_to`, `store_stats`), the class lock
(`ClassState::plan`, `reopen`, `on_live_zero`, `bin_stats`,
`lowest_partial`, `is_partial`, `empty_bytes`, `purge_empty`,
`purge_curve`), the slab table's commit lock
(`SlabTable::mirror_watermark`, `guards`, `take_group`, `commit_run`,
`commit_slab`) and the background thread's state lock
(`purge::background::request`, `start`, `start_requested`,
`auto_tick`, `main`); the source test
`api::tests::guards_end_in_unlock` (`api/mod.rs`) checks that none of
these guards drops without it. For every other lock the invariant and
frame are not checked at the release: each verified holder ends with them
through its last call's postcondition or an `assert`. Ten locks'
holders all end with it. For all but the state lock (whose invariant
`axiom_state_lock_inv` states) no axiom states `lock_inv` and
`lock_frame`: each type's `LockSpec` impl defines them, and what is
left is the mutex contract (`lock::axiom_acquired`) and that every
holder outside the tests reaches `LockGuard::unlock`, which the source
test `api::tests::guards_end_in_unlock` checks. The arena creation lock
is also held by the fork handlers, whose release,
`Lock::unlock_held_perm`, requires the invariant and frame as
`LockGuard::unlock` does. The state lock is held by the fork handlers
too, and that hold still rests on `axiom_state_lock_inv`:
`purge::background::fork_child` writes the value through
`Lock::get_unlocked`, and `fork_unlock` releases it through
`RawLock::unlock_held`, which checks nothing. The one exception is the
initial value: eight of those nine take it from `Lock::new`, which
requires the invariant, but the per-arena statistics locks take theirs
from the trusted `Arena::put_stats`, which the test
`put_stats_writes_lock_new_of_empty` compares with
`Lock::new(ArenaStats::empty(index), ..)`. The impl for
`meta::Inner` (`meta.rs`), the metadata lock: `Inner::wf`, with the
page, the region's bounds and the ghost identities fixed. The impl for
`[Lazy; SET_GROUPS]` (`class/sets.rs`), the slab-set tables' lock:
each table's `Lazy::claimed`, with its base and length fixed. The impl
for `Snapshot` (`stats.rs`), the process statistics lock:
`Snapshot::inv`, with a frame true of any two snapshots. The impl for
`()` (`heap/tcaches.rs`), the explicit-cache table lock: both `true`.
The impl for `huge::Inner` (`huge/mod.rs`), the huge lock: `Inner::wf`,
with the ghost page count, page shift, region base, `Meta` identity and
run authorities' identities fixed. The impl for `class::Inner` (`class/mod.rs`), the class lock:
`Inner::inv`, with the sets' slab count, the arena, the table and the
owned-slab record's location fixed. The impl for `class::table::TableInner`
(`class/table.rs`), the slab table's commit lock: `TableInner::inv`,
with the ghost geometry, the marker authorities, the descriptor array
and the watermark's ghost location fixed.
The impl for `Created` (`arena.rs`), the arena creation lock:
`Created::ok`, with the snapshot half's location fixed and the count at
or above the one the lock was created with. The impl for `ArenaStats`
(`stats.rs`), the per-arena statistics locks: `ArenaStats::identities`,
with a frame true of any two values. The impl for
`purge::background::State` (`purge/background.rs`), the background
thread's state lock, defines only the frame, true of any two states,
which no release can break; its invariant is `axiom_state_lock_inv`'s.
Each other
lock type's `lock_inv`/`lock_frame` axiom says the lock protects that
type's invariant; the theorems that use one name it.
`Tcaches::init` makes each entry null with
`vspec::entry::new`, over the layout from `axiom_tcaches_layout` and
`axiom_tcaches_assemble`.

**Class state accessors.** None trusted: `TablePtr::get` reads the
class's table through the frozen permissions to every class's table
(`class::tables_ok`), which `Shared::init` freezes
(`vstd::modes::tracked_static_ref`) once it has written the tables,
and `axiom_shared_built` reads the built `Shared`'s tables from; a set
of a class state reaches the arena's set table through that table's
frozen permission, which `Arena::init` freezes once it has written it.

**Debug reads.** No trusted read; one trusted permission mint.
`class::verify::read_word`, the `check_zero` debug mode's volatile read
of a word of a slot, is verified (`core::ptr::read_volatile`,
`vspec::ptr`), through a permission to the slot's bytes:
`ClassState::check_zero` mints each claimed slot's from the slot token
the claim just took (`heap::live::Live::from_slot`, the hand-out's mint)
and gives the token back with the permission dropped
(`Live::take_slot`); `ClassState::check_fresh_slab_is_zero` reads a
`Fresh` slab's unclaimed slots, which no token covers, through the
permission `class::axiom_fresh_slab_mem` lends for as long as `take`
borrows the slab's descriptor permission from the class lock value
(trusted: the slab is committed and nothing else holds a permission to
its slots while it is `Fresh`; a borrowed permission only reads,
`vspec::user::UserMem::lemma_valid_reads`). Neither claims anything
about the value. The huge reads
(`huge::Huge::read_run_word`) are verified: `Huge::check_zero` mints the
run's permission from the run token `take` just made
(`heap::live::Live::from_run`, the hand-out's mint), reads through it with
`core::ptr::read_volatile` (`vspec::ptr`) and gives the token back with the
permission dropped (`Live::into_run`).

**`core` and `vstd`.** The `assume_specification`s in `src/vspec/`
(74, mostly `vspec/core_int.rs`, `slice.rs` and `sys.rs`; one of
them, `<*const T>::wrapping_add` in `vspec/config.rs`, claims nothing
about the pointer, for `env_var`'s walk of `environ`),
`getauxval`'s in `sys/auxv.rs`, and vstd's
specifications of `core`.

**Memory model.** Sequential consistency. Atomic loads return any
value (vstd), except the `free` path's dispatch byte (`vspec/flags.rs`,
above), whose `load` covers every value the one `RouteByte`, the
published `Shared`'s, holds for a reader that observed `READY`; the
three bitmap word writers
(`vspec/word.rs`) are one atomic step each and ignore
the ordering, as is the one word load that returns its value
(`vspec::word::load`, theorem 7). Kani runs each `vspec` atomic
wrapper on a real atomic, one thread (`src/kani/atomic.rs`), and the
loom models in `tests/loom` check interleavings of the protocols on
copies of the code, so the ledger lists the wrappers as partly checked:
the shipped bodies' interleavings are not checked. See
`verus/WEAKMEM.md`.

**Abort stand-in.** None: no site has Verus see another `-> !`
call than the build's abort. At `Huge::free_run`'s table lookup, where
the build calls `not_a_run(self.addr(page))`, the run token names the
run the table maps the page to (theorem 44), so the site is unreachable
and Verus checks the build's statement there itself. Every
abort helper verifies in place and ends in `sys::abort_fmt`, which
writes the message and raises `SIGABRT` (`sys/abort.rs`). The panic
handler (`lib.rs`) aborts. An abort satisfies every postcondition.

**Termination not proved** (`exec_allows_no_decreases_clause`):
`lock::RawLock::lock_slow` (`lock.rs`), `claim_with` (`class/mod.rs`),
`api::init_slow`, `api::line_ready` and `api::wait_line` (the wait for
another thread's build of the feature line), `api::init_gate_enter` and
`api::fork_gate_enter` (the waits for another thread's init or fork),
`sys::cgroup::Deferred::discover` (a waiting discovery's wait for
another thread's), `api/cxx.rs` `oom`, `purge/background.rs` `main`,
`getrandom_exact`, `read_full` and `write_all` (`sys/mod.rs`),
`sys::cgroup::scan_lines`, `arena::note_build_cost` (`arena.rs`, a
compare-and-exchange retry loop), and `config.rs` `env_var` (its walk ends at
the null pointer that ends `environ`, a fact about the C program). The theorems are
partial correctness: they say what holds if the call returns. None of
these is on the thread-cache fast paths, which terminate and take no
lock (theorem 9).

## 1. Region separation

Every non-null block an allocation returns lies wholly inside one class
region or the huge region, and shares no byte with the metadata region.

**Statement** (`heap/mod.rs`):

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
(`heap/mod.rs`); `Regions::user_block` (`layout/regions.rs`) is
a slot start of a class at least `size` bytes (`is_slot`), or a
page start of the huge region with `max(size, 1)` bytes of the region
after it (`is_huge_block`). The proof is
`Regions::lemma_user_block_separated` (`layout/regions.rs`).

**Entry points.** Each ensures `fits`; the theorem is a separate lemma
over that postcondition, not called by them. Rust-level functions; the
`extern "C"` wrappers (`api/libc.rs`, `api/jemalloc.rs`
and `api/cxx.rs` `exports`) are in `verus!` and only cast;
they also restate the core's `fits` in their own `ensures`.

| Function (file) | Size, align in `fits` |
|---|---|
| `malloc` (`api/libc.rs`) | `size`, `QUANTUM` |
| `calloc`, `calloc_zeroed` (`api/libc.rs`) | `n * m`, `QUANTUM` |
| `realloc` (`api/libc.rs`) | `size`, `QUANTUM` |
| `reallocarray` (`api/libc.rs`) | `n * m`, `QUANTUM` |
| `aligned_alloc` = `memalign` (`api/libc.rs`) | `size`, `align` (pow2) |
| `valloc`, `pvalloc` (`api/libc.rs`) | `size`, the page |
| `malloc_usable` (`api/libc.rs`) | `size`, `QUANTUM` |
| `calloc_usable` (`api/libc.rs`) | `n * m`, `QUANTUM` |
| `realloc_usable` (`api/libc.rs`) | `size`, `QUANTUM` |
| `mallocx` (`api/jemalloc.rs`) | `size`, `1 << (flags & 0x3f)` |
| `rallocx` (`api/jemalloc.rs`) | as `mallocx` |
| `cxx::new`, `new_nothrow` (`api/cxx.rs`) | `size`, `QUANTUM` |
| `cxx::new_aligned` (`api/cxx.rs`) | `size`, `align` (pow2) |

A kept block (`r == p`) is included: the realloc family ensures `fits`
for every result. The keep decision
(`old_size_or_keep`, `realloc_old`) ensures `kept(p, size, align)`
(`api/libc.rs`), a quantum-aligned slot of a large enough class, or a
huge page start with `max(size, 1)` bytes of the region from it, which
unfolds to `fits` at `align` and at the quantum (`kept` is `open`, so
it unfolds where it is used). The huge
half's bound: the keep test is `usable_for(size) == Some(old)`, so
`size <= old`, and `old`, the usable bytes `Huge::usable_size` reads
from the run's descriptor, ensures `spec_run_usable` (the usable pages
and the guard inside the region); `vspec::usable::lemma_run_bytes_block`
turns that into `is_huge_block(p, max(size, 1))`.

**Premises beyond the common ones.**
- Slab results: `CachePerms::tracks` (`heap/mod.rs`), part of the
  assumed `Heap::wf`; the cache entries' `is_slot` comes from verified
  refills and frees.
- With PAC: `sys::pac::axiom_auth_sign` (`sys/pac.rs`, used at
  `heap/mod.rs`): `spec_auth(spec_sign(p, m), m) == p` below the VA
  limit; `sign`, `auth` (`sys/pac.rs`) are `external_body`
  and equal `spec_sign`/`spec_auth`, one function per process (no
  `PR_PAC_RESET_KEYS`). The ledger (`verus/ledger/src/sys/pac.rs.md`)
  lists `sign` and `auth` as
  discharged on premises by Isla on the Armv8.5-A and Armv9.4-A models
  (`verus/isla/seams.md`): the premises, not checked, are that the kernel keeps the
  thread's data key A and EnDA fixed (K3 there), and for `auth` that it
  never resumes past an `autda` that faults (K2); `axiom_auth_sign` and
  `strip` are discharged by those runs, within the firmware and CPU
  premises (P12, P13) that file states. `sign`/`auth` require `pac::pac_available()`
  (uninterpreted; its one fact is the trusted `AT_HWCAP` reading in
  `sys::auxv`, which `pac::available` turns into its contract), and
  `auth` also requires `may_abort_corrupt() || signed(p, modifier)`.
- Huge results: the huge lock's invariant and frame, which
  `huge::Inner`'s `LockSpec` impl (`huge/mod.rs`) defines and every
  holder outside the tests checks at its `LockGuard::unlock` (common
  premises, "Locks"); `Huge::alloc_run`
  (`huge/mod.rs`) is verified. A kept run's bound rests on the
  same invariant: `Huge::usable_size` reads its `spec_run_usable` from a
  live `Active` descriptor under the huge lock invariant (`run_ok`,
  `desc_ok`).
- Class lock and commit lock invariants for the claim path:
  the class lock's, which `class::Inner`'s `LockSpec` impl
  (`class/mod.rs`) defines, and the commit lock's, which
  `table::TableInner`'s (`class/table.rs`) defines, both checked at the release: every
  holder outside the tests ends with `LockGuard::unlock`, which
  requires them (common premises, "Locks"); `next_slab_locked`
  (`class/table.rs`, verified); `bits_unchecked` is verified,
  through the word permissions of `table::axiom_fresh_words`
  (`class/table.rs`; one axiom returns both the value and the borrow
  permissions and requires a `ZeroRaw` token for the words; Kani
  partly checks it, `verus/ledger/src/class/table.rs.md`).
  **Unconfirmed** which of these the `is_slot` ensures of
  `ClassState::claim` needs; the slot address comes from
  `claim_with`'s verified `slots_in`.

**Where the statement is weaker than the code.**
- `mallocx` with no alignment flag is stated at alignment 1, not the
  quantum, though with no `MALLOCX_LG_ALIGN` the code returns a class or
  run aligned to at least the quantum.
- `posix_memalign` (`api/libc.rs`) states its result through the
  call's out-pointer permission (`vspec::cbuf::COut`, which the
  verified `store_out` writes through): on a `0` return the
  permission's value is a non-null pointer that `fits` at `size` and
  `align`, and on `EINVAL` or `ENOMEM` the value is unchanged. That C
  reads that value at `*out` after the return rests on the trusted
  `exports::axiom_out_at_return`.
- `fits` says `size` bytes; the code gives the class size or the run's
  usable bytes (theorem 2).

## 2. Fit and alignment

Every non-null block an allocation returns has room for the requested
size, in one slot of a large enough class or in the huge region from a
page start, and its address is a multiple of the requested alignment.

**Statement.** No theorem function: `fits` itself (above). Its
`user_block` conjunct puts `size` bytes in one slot of a class at least
`size` bytes, or in the huge region from a page start; its last conjunct
is `p % align == 0`. Lemmas: `lemma_slot_fits` (`heap/mod.rs`),
`lemma_slot_fits_quantum`, `lemma_huge_fits`,
`Regions::lemma_slot_aligned` (`layout/regions.rs`),
`layout::lemma_class_size_quantum`.

**Entry points.** Those of theorem 1, with the alignment column above.

**Premises beyond the common ones.**
- Slot alignment: each class base a multiple of its class's slab, of the
  page and of the 64 KiB colour unit (`base_aligned` in
  `Regions::separated`, proved by `from_parts` from `reserve_owning`'s
  offsets: a random multiple of `MAX_SLAB` plus the class's colour, or
  no colour under `thp:always` chunks); class sizes are
  multiples of the quantum and slabs powers of two (`Class::wf`, through
  `lemma_class_table_wf`, verified).
- Huge alignment: `Huge::alloc_run`'s placement, under the huge lock
  axioms; small alignments fall back to the page, at least 4 KiB
  (`Regions::separated`).
- `api::page_size` (`api/libc.rs`) is verified from `Shared::wf`.

**Where the statement is weaker than the code.**
- The huge fit is `size` bytes of the region, not the descriptor's
  `usable` bytes (`Huge::spec_usable`, `huge/mod.rs`), which the
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

**Statement** (`heap/mod.rs`):

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

| Function (file) | Condition on `free_target(p)` |
|---|---|
| `free` (`api/libc.rs`) | `p` non-null |
| `free_sized`, `free_aligned_sized` (`api/libc.rs`) | non-null |
| `realloc`, `realloc_usable` (`api/libc.rs`) | non-null |
| `reallocarray` (`api/libc.rs`) | non-null, `n * m` no overflow |
| `free_usable` (`api/libc.rs`) | non-null |
| `malloc_usable_size` (`api/libc.rs`) | non-null |
| `dallocx`, `sdallocx` (`api/jemalloc.rs`) | non-null |
| `rallocx` (`api/jemalloc.rs`) | non-null |
| `xallocx`, `sallocx` (`api/jemalloc.rs`) | always |
| `cxx::delete`, `delete_sized` (`api/cxx.rs`) | non-null |
| `cxx::delete_sized_aligned` (`api/cxx.rs`) | non-null |

Below them, `heap::free` and `free_direct` (`heap/paths.rs`),
`Heap::free`, and the fast paths `free_fast_slot` and
`free_fast_tail` (on their `Some` result) and
`free_sized_fast` (on its `true` result) ensure it. `free_fast` and
`free_fast_class` are `#[cfg(test)]` helpers outside
`verus!` that ensure nothing; the cache pushes and
`Shared::free_direct` (`heap/shared.rs`) require `is_slot` of their
class, `Shared::free_huge` a page index from
`Shared::classify_free`.

**Premises beyond the common ones.**
- Every abort ends in `sys::abort_fmt`, which never returns: the abort
  helpers (`heap::paths::invalid_free`, `heap/paths.rs`, and the
  class and huge abort helpers) verify and call it.
- `Shared::classify_free` and `Regions::classify` are verified; the
  `Regions::slab_slot` reads the class table through `class_entry`
  (common premise). Both fast paths read the geometry from the class's
  line (`ClassLine::set_slab`, `Shared::sized_slot`; "Class lines",
  common premises); `slot_in_class` is test-only.

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
  at the load (`SlabTable::is_set_unchecked`, `class/table.rs`)
  aborts; a sized free pushes without a bit test, and the cache flush catches it
  unless a `malloc` pops the slot first (a pop tests no bit), except
  under the opt-in `exact_sized_free` key, which sends it through plain
  `free`'s test ([below](#sized-frees-run-plain-frees-check-under-exact_sized_free-rests-on-the-c-boundary-assumption)); a non-live
  huge page start aborts in `Huge::free_run`. The residue: a free of a
  slot whose first copy is still in a cache, a thread's or an explicit
  tcache (its bit is still set; caught, if at all, by a later flush;
  with the opt-in `double_free_scan` key, every free route checks the
  slot's state byte at the call, `heap/slotstate.rs`, and aborts unless
  the slot is live: its steps are verified and the byte's value is a
  premise, [below](#double-free-window-under-double_free_scan-rests-on-the-slot-state-premise)), and a free of a block
  that was freed and handed out again (ABA), which no allocator can tell
  from a live one. "Bad input" below lists them by
  entry point.
- A thread's first `free` may create its heap before the abort.

## 4. User blocks are disjoint from allocator metadata

A spatial statement (`theorem_metadata_integrity`): no byte of a
non-null block an allocation returns lies in the metadata listed below,
the `Shared` static included. It does
not prove that every allocator write stays inside the object it is meant
for, nor that a returned byte is mapped with the right permissions.

**Statement** (`heap/mod.rs`):

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

(Line breaks added.) `disjoint` is `layout/regions.rs`. Each
conjunct is "lies in the metadata region" (`Regions::in_meta`,
`layout/regions.rs`) plus `lemma_user_block_apart`, except
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
the assumed `Heap::wf` and the huge lock's invariant (`huge::Inner`'s
`LockSpec` impl, checked at each holder's `LockGuard::unlock`).

**Premises beyond the common ones.**
- `vspec::ptr::axiom_ptr_add_addr` (`vspec/ptr.rs`): `p.add(i)` is
  `i` elements on, inside its allocation; used for stack entries
  (`heap/mod.rs`), `others` nodes, huge table slots and bitmap
  words.
- `vspec::ptr::axiom_raw_readable` (`vspec/ptr.rs`): a `PointsToRaw`
  makes its bytes valid for reads and, when there are some, in one
  allocation (`in_alloc`). It is the only such axiom, and it gives no
  `valid_writes`.
- The metadata lock's holders reach `LockGuard::unlock` (`lock.rs`):
  every holder outside the tests ends with it, which requires the invariant
  (`Inner::wf`) and frame (bounds and ghost identities fixed) that
  `meta::Inner`'s `LockSpec` impl (`meta.rs`) defines; the source test
  `api::tests::guards_end_in_unlock` (`api/mod.rs`) checks it (common
  premises, "Locks").
  `Meta::commit` is verified: its caller gives the claim to the range that `reserve` or
  `reserve_table` handed out, which each lazily committed table keeps
  for its pages past its watermark (`class::sets::Lazy::claimed`, over
  the process's `Meta`, `meta::process_pages_id`, known during `init`
  through `heap::shared::axiom_shared_meta`) and the background stack
  takes after its reservation; a commit takes the range's part.
  `Meta::free` is verified:
  its caller gives back the `MetaClaim` the allocation handed out, which
  the cache permissions (`CachePerms::claimed`) and the huge allocator
  (`huge::Inner`'s `dclaims`, the descriptor table's `claim`) carry;
  `Heap::destroy` (verified) gives back the claims
  of what it frees from the heap's ghost state (the stacks' from
  `CachePerms::blocks`, the nodes' from `Heap::heads`) and the heap
  block's from its home headers' permissions (`heap::axiom_heap_block`).
- `Shared::put_head` and `put_table` store what they are given
  (common premise); `SlabTable::new` (`class/table.rs`) proves
  `meta_ok` from the reservations `Meta::reserve_table` (`meta.rs`)
  and `Lazy::reserve` (`class/sets.rs`) hand out.
- The class table accessors `desc_unchecked`, `bits_unchecked`,
  `is_set_unchecked`, `owner_word` and `owner_at` read through
  permissions carved from the three reservations
  (`SlabTable::perms_ok`): `Meta::reserve_table` hands out the table's
  bytes once, an eager table's committed part from the `Carve` `Inner::reserve_eager`
  hands back
  and (a carve makes no system call; the object
  side's commit is deferred to the next object or table commit,
  `Inner::flush`) its part past `committed` through
  `Unbacked::into_lazy` of the token `reserve_eager` carved into the
  same `Carve`, and a reservation's through
  `Unbacked::into_lazy` (`vspec/unbacked.rs`, which uses up the token),
  `table::axiom_zero_desc`, `axiom_zero_owner`, `axiom_fresh_words`
  (`class/table.rs`: zero bytes, from the
  `ZeroRaw` `reserve_table` returns, are an initialised descriptor,
  owner word or bitmap word; Kani partly checks the three, leaving out
  the provenance clause of `perm_at`, which Kani cannot observe, and the
  ghost identities), the slice primitive
  under the bitmap view (`vspec::slice::slice_from_perms`, a slice over
  consecutive word permissions, `class/table.rs`) and the watermark's load
  (`vspec::atom::load_usize`, inside the watermark's invariant, which
  bounds it by the slab count). Premise: only committed pages are read (the
  accessors' `# Safety`).
- `TwoLevelBitmap::from_raw`, `from_raw_chunked` (`bitmap.rs`) are
  verified: they build each word's permission, holding 0, from the
  words' `ZeroRaw` each takes (a table's bytes, or a `SetTable` slot's,
  which `SetTable::new` carves once per slot from its groups'
  reservations) through `vspec::zero::axiom_zero_word` (eight bytes that
  read as zero are the `u64` 0; discharged by Kani,
  `kani_proofs::zero::zero_word_reads_zero`). The zero facts are `Meta::reserve_table`'s:
  an eager table's committed part from the `Carve`'s zero token (fresh
  memory above the bump pointer, `Unbacked::commit`) and its deferred
  part, like a reservation's, from `Unbacked::into_lazy` (the same `mmap`
  fact, on its commit-before-read premise: the owner's first
  `Meta::commit` runs the deferred commit). Premise (`ZeroRaw::axiom_new`): a permission is paired only
  with the zero token made with it, whose bytes nothing has written since
  (four pairings: three in `meta.rs`, and one in `huge/table.rs`,
  `Table::storage`'s binned branch, which pairs the permission and token
  `Meta::alloc_zeroed` returns for one object).
- `CachePerms::lemma_fresh_stack` (`heap/mod.rs`, proved): a stack
  `Meta::alloc_raw` just returned is fresh (it uses up the block's permission);
  `Heap::lemma_caches_in_meta` (`heap/mod.rs`) needs `cell_in_meta`,
  which `ensure_cache` (`heap/cache.rs`) proves for each stack.
- The huge lock's holders reach `LockGuard::unlock`, which requires
  the invariant and frame `huge::Inner`'s `LockSpec` impl defines
  (`api::tests::guards_end_in_unlock`).
- The `Shared` static: `Reserved::is_apart_from_static`
  (`layout/regions.rs`, trusted, the kernel's `mmap` contract): each of
  the three reservations is apart from the loaded object's data, which
  `from_parts` turns into `Regions::outside`.
- `huge::table::Table::storage` (`huge/table.rs`, verified): zeroed storage for the descriptor table, every entry empty,
  each with its permission, in the metadata region. It cuts the block's
  zero permission (`Meta::reserve_committed_raw`'s `ZeroRaw` for a
  reserved table; for a binned one, `Meta::alloc_zeroed`'s permission and
  zero token paired by `ZeroRaw::axiom_new`, above) into the entries'
  permissions (`entry_slots`, `huge/table.rs`), each empty by
  `huge::table::axiom_zero_entry` (`huge/table.rs`: 16 zero bytes at
  a non-null, 8-aligned pointer are an initialised entry with a null
  descriptor), discharged by Kani
  (`huge::table::kani_table::zero_entry_is_empty`). It requires a page
  of at least 16 bytes, which `Huge::new` requires. `Table::new` and
  `ensure` prove `entries_in_meta`.
- User-block writes: every write verified code makes to a block's bytes
  (`calloc`'s and the zeroing paths' fills, `zero_slot`, `zero_huge`,
  `realloc`'s copy) takes the block's permission (`vspec::user::UserMem`,
  held by its `Live` token), so it stays inside the block's usable bytes,
  given the permission model's trusted mint: the permission is minted at
  hand-out by `heap::live::axiom_hand_out`, at most once per block while
  held (the `Live` API keeps it with the slot or run token), and at the
  C boundary by `axiom_c_claim`.
  That no other write reaches user memory is discipline, not proof: the raw
  primitives `zero_raw` and `copy_nonoverlapping` stay callable on
  any `valid_writes` fact, which has no time (`vspec::ptr::rw_len`), so
  this holds because only `vspec::user` calls them on user memory.

**Not covered.**
- Thread-local storage: not apart from every block, so not in the
  statement. A thread whose stack the program allocated
  (`pthread_attr_setstack` over a `malloc`ed buffer) has its static TLS
  block, and so challoc's heap pointer slot, inside that buffer.
- Writes through `&` atomics (descriptors, owner words, `AtomicBitmap`
  words) are located, not tied to permissions. Bitmap words have
  `WordPerm`s inside `bitmap::WordPred` (`bitmap.rs`), but the
  theorem does not use them.
- Arenas' slab-set tables (`SetTable::new`, `Set::in_table`,
  `class/sets.rs`): no placement stated.
- A class bitmap whose bit count is not a multiple of 64: the last word
  is excluded (`spec_nbits() % 64 == 0`).
- Spatial only: the theorem places metadata and blocks; it does not
  prove that each allocator write stays inside the object it is meant
  for (the writes through permissions do, the atomics above are only
  located), nor that the block's bytes are mapped writable.

## 5. Uniqueness

**Statement** (`heap/mod.rs`; private to `heap`, in
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
        // ... slots, cls, owner, zero, handed, blocks, released unchanged ...
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

- Authority per class: `table::slot_loc` (`class/table.rs`),
  minted by `SlabTable::new`.
- Free slots: tokens in each word's `AtomicInvariant`,
  `bitmap::WordPred` (`bitmap.rs`): the word's `WordPerm` and the
  tokens of exactly its clear bits.
- Claimed: `AtomicBitmap::claim_word`, `claim_from` (`bitmap.rs`)
  move out the tokens of the bits their `fetch_or` won;
  `ClassState::claim_with` (`class/mod.rs`) returns them by index.
- Cached: `CachePerms::tokened` (`heap/mod.rs`), a conjunct of
  `Heap::wf` and `Heap::keeps`.
- Released: `ClassState::release_owned` (`class/mod.rs`) takes a
  `GhostSubset` of the batch; `AtomicBitmap::release`
  (`bitmap.rs`) hands each word's back, so its double-free abort is
  unreachable under the contract.
- Live: the program's. A `heap::live::Live` token (`heap/live.rs`)
  holds the slot's token while the program holds the block: the
  allocation paths move the popped, refilled, compacted or directly
  claimed slot's token into it (`heap::hand_out`, `heap/mod.rs`;
  `Heap::pop_compact_run`; `Shared::alloc_direct`), and the free paths
  take it back out (`Live::take_slot`) for the cache push
  or the release. Huge runs have the same shape: a `huge::RunToken`
  (`huge/mod.rs`) holds the run's extent, a singleton of the
  extents' authority, and its pages, a subset of the pages' authority;
  `Huge::take` inserts both into the authorities `Inner::wf` holds
  (`huge/mod.rs`: their sets are the `Active` descriptors'
  extents and pages, `active_runs`, `active_pages`), and
  `Huge::free_run` deletes them, after `agree` shows the token's extent
  is the descriptor's (`lemma_token_extent`).

**Live blocks (the D column).** `Live::theorem_apart`
(`heap/live.rs`): the usable bytes of two live tokens, wherever
they are held, share no byte. Two slots of one class are two tokens of
one authority, so different slots, a class size or more apart; slots of
two classes lie in two class regions; a slot and a run lie in the
superblock and the huge region; two runs hold disjoint page subsets.
A class's authority (`class::slot_loc`) is that of the class's table
in the stored `Shared`, by its opaque definition, which `Shared::init`
reveals; the claim and release paths reach the table through a class
state whose table is that one (`ClassState::is_process`, from
`Shared::lemma_published_tables`).
`Heap::theorem_uniqueness_live` (`heap/mod.rs`): a slot a
well-formed heap caches is no live block of its class.
`Heap::theorem_uniqueness_apart` (`heap/mod.rs`): two well-formed heaps
never cache the same slot, since a slot both held would have two tokens
of one authority, so cached entries of one class in two heaps stand for
different slots.

**Entry points.** The allocating entry points' cores return the token
of the block they hand out, with at least the request's usable bytes
(`Live::gives`, `heap/live.rs`): `malloc_block`, `calloc_zeroed`,
`alloc_aligned`, `aligned_alloc_block`, `posix_memalign_block`,
`valloc_block`, `pvalloc_block`, `malloc_usable_stored`,
`calloc_usable_zeroed`, `realloc_block`, `realloc_usable_stored`
(`api/libc.rs`), `mallocx_zeroed`, `rallocx_zeroed`
(`api/jemalloc.rs`), `new_block`, `new_aligned_block` (`api/cxx.rs`).
The exported functions call them and hand the token to C with the
block; the free cores take the token C holds (common premises, "The
`free` contract").

**Premises beyond the common ones.**
- `heap::live::axiom_c_claim` (`heap/live.rs`): the `free`
  contract at the C ABI boundary, one call per C call. See "The `free`
  contract" under Common premises.
- `table::axiom_fresh_words` (`class/table.rs`, one call in
  `SlabTable::new`): a just-reserved table's words read zero, one
  permission each; once per class is checked (its token), that `base`
  is the class bitmap's first word is not. Kani partly checks it
  (`kani_proofs::zero::fresh_words_load_zero`: the words load zero); the
  provenance clause, the one-shot token and the ghost location
  identities are not checked.
- `vspec::word::word_addr` (`vspec/word.rs`) is a word's identity;
  `fetch_or`, `store`, `fetch_and` are single
  atomic steps whose result is the permission's value (ordering
  ignored; trusted, partly checked by Kani on one thread and by the
  bitmap loom models on copies of the code,
  `verus/ledger/src/vspec/word.rs.md`).
- `SlabTable::bits_unchecked` is verified: the view's words are the
  class bitmap's through the permissions of `table::axiom_fresh_words`
  (`class/table.rs`), whose `word_addr` ties them to the word
  invariants.
- The class and commit locks' `LockSpec` impls (`class/mod.rs`,
  `class/table.rs`; the commit lock's
  value also keeps the owner words' persistent witness markers of the
  slabs below `next_slab`, and the class lock's frame the location of
  the owned-slab record, `Inner::oauth`; the class lock bridge
  `owner_locked` is verified), the descriptor permissions
  (`slab::DescPerm`:
  `table::axiom_zero_desc`, `class/table.rs`, which mints them from
  a `ZeroRaw`, partly checked by Kani; `slab::desc_stride` is
  discharged by Kani, and the `SlabDesc` accessors in `slab.rs` are
  trusted, partly checked by a loom model on a copy,
  `verus/ledger/src/slab.rs.md`; the accessors given the lock edge
  `lock::axiom_acquired` assumes) and `next_slab_locked`
  (`class/table.rs`, verified).
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
promises zero. `vspec::zero::theorem_zeroed_memory` (`vspec/zero.rs`)
concludes `z.has(a)` for every byte `a` from `from` to the usable end,
and `Zeroed::has` is interval membership. That a load of such a byte
reads zero is the token's meaning, not a proved fact: it rests on the
trusted token sources (`zero_raw` and the kernel axioms below) and on
the unproved discipline that nothing writes a token's bytes between its
creation and the return (premises below). The block's permission also
carries a byte state that also says the bytes are zero ("Byte state"
below). Plain `calloc(n, m)` promises,
and its token covers, only its `n * m` requested bytes; bytes past them
up to the usable size are unspecified (#54). `challoc_calloc_usable`,
`mallocx(.., MALLOCX_ZERO)` and `rallocx`'s new tail cover the whole
usable size.

**Statement.** The `Zeroed` token (`vspec/zero.rs`): `covers(p, n)` is
`addr() == p@.addr && n <= len()`; `covers_usable(z, p, from, size)`
(`vspec/zero.rs`) is a token from `p + from` to the block's usable
end (the class size, or `huge_usable(size)`). Postconditions:

| Function (file) | Token on a non-null result |
|---|---|
| `calloc_zeroed` (`api/libc.rs`) | `covers(r.0, n * m)` only |
| `calloc_usable_zeroed` (`api/libc.rs`) | `n * m`, and `covers_usable(.., 0, n * m)` |
| `mallocx_zeroed` (`api/jemalloc.rs`) | with `MALLOCX_ZERO`: `size`, and `covers_usable(.., 0, size)` |
| `rallocx_zeroed` (`api/jemalloc.rs`) | `size - k` bytes from `r + k`, and `covers_usable(.., k, size)` |

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

**Byte state (loads read zero).** The block's `Live` token holds the
permission to its usable bytes (`vspec::user::UserMem`: a `PointsToRaw`
and a ghost byte state, what a load of each byte reads). The same entry
points also state the zero bytes in that byte state
(`Live::reads_zero`): `calloc_zeroed` for its `n * m` bytes,
`calloc_usable_zeroed` and `jemalloc::alloc_with` for the whole usable
size (with `MALLOCX_ZERO`; `mallocx_zeroed`, `mallocx`'s core, does not
pass the `reads_zero` clause on), and `rallocx_zeroed` for the
moved block's bytes past the copy, `[k, len)`. When the code clears the
block, `vspec::user::zero_user` writes through the permission and
updates the byte state. So any verified write between the clear and the
return would have to take the permission and change the state. That
makes "no writer between the clear and the return" mechanical for
verified code. When the allocator reports the block known zero, its
`Zeroed` token becomes the byte state through the trusted bridge
`vspec::user::axiom_zeroed_bytes`. That rests on the token sources
below, as the coverage statement does, and on the token being fresh: a
stale `Zeroed` passed to the bridge would set a false byte state. So for
the known-zero hand-outs the byte state is as strong as the token's
meaning, and no stronger.

**Token sources.**
- `vspec::zero::zero_raw` (`vspec/zero.rs`, `external_body`,
  discharged by the Kani harness `zero_raw_fills`; it takes the bytes'
  permission):
  `write_bytes(p, 0, n)` makes `n` zero bytes.
- Never-written anonymous memory reads zero: for class slabs,
  `class::axiom_fresh_slots_zero` (`class/mod.rs`, called in
  `ClassState::take`, for a `Fresh` slab, with the slab's
  descriptor state ticket); for huge runs, `huge::axiom_fresh_anon_zero`
  (`huge/mod.rs`, private to `huge`, called in `zero_run`,
  pages at or past the mark); for metadata, the `Zeroed` that
  `Unbacked::commit` and `into_lazy` return (`vspec/unbacked.rs`,
  `external_body`), under `alloc_zeroed` (`meta.rs`) and
  `reserve_committed_raw` (`meta.rs`).
- `MADV_DONTNEED` memory reads zero: `class::axiom_discarded_slots_zero`
  (`class/mod.rs`, called in `take`, for a `Purged` slab,
  with the state ticket and the `PurgedZero` witness of a
  `purge::purged_zero` read, `purge.rs`) and
  `huge::axiom_discarded_zero` (`huge/mod.rs`, called in
  `zero_run`).
- The huge pair is private to `huge`; the class pair takes the slab
  descriptor's state ticket.

**Premises beyond the common ones.**
- Each kernel axiom's protocol premise, the caller's: a `Fresh` slab
  was never handed out; pages past the huge mark were never covered by a
  run; a `Purged` slab was discarded in the lock hold that
  set the state (`class/decay.rs` sets it whatever the advice) and
  has no allocated slot since; and `take` makes a zero claim of a
  `Purged` one only while `purge::purged_is_zero` (`purge.rs`, a
  `Relaxed` load of `LAZY_EVER`) reads true under the lock
  (`class/mod.rs`, through `purge::purged_zero`, whose witness the
  axiom requires), that is, no batch has used `MADV_FREE`. The run's
  pages in a `Purged` huge extent were discarded again by
  `Huge::rediscard` in `take`'s lock hold (`huge/mod.rs`; its success,
  `sys::madvise_ok` of exactly those pages, is what
  `axiom_discarded_zero` requires) and nothing has written them since.
  The other zero axioms take any range; their call sites check the
  recorded state, not the history.
- Cache path: `CachePerms::zero_ok` (`heap/zero.rs`), part of the
  assumed `Heap::wf`; `Heap::take_handed` (`heap/zero.rs`).
- The class and huge locks' invariants (`class::Inner`'s and
  `huge::Inner`'s `LockSpec` impls), for the state `take` reads.
- The fills and the copy are valid because they take the block's
  permission (`vspec::user`, from `heap::live::axiom_hand_out` or
  `axiom_c_claim`); no other axiom makes them valid.
- The only writes verified code makes to a token's bytes are zero fills
  and `rallocx_zeroed`'s copy into the new block's prefix, which it
  splits off (`api/jemalloc.rs`). The token is not a permission; nothing ties
  it to other writers. Stated in `vspec/zero.rs`, not proved.

**Where the statement is weaker than the code.**
- Coverage, not contents: the theorem is about the token's interval at
  the moment of return (above). A program's own later writes are its
  business; the gap is challoc's writers between the token's creation
  and the return, which are bounded by inspection only.
- `rallocx_zeroed`'s `k` is `min(old, size)` for some `old` with
  `usable_of(p, old)` (`old_size_or_keep` ensures
  `usable_of(p, old)` of the `old` it returns, and the code copies
  `min(old, size)`). That is exact for a slot `p` (the class's size). For
  a huge `p`, `usable_of` states only the shape of `old` (`run_bytes`:
  whole pages, at least one, inside the region), not the descriptor's
  value; the value comes from `p`'s token: `old_size_or_keep` also
  ensures `old == blk.len()`, and `rallocx_zeroed` ensures
  `k == min(blk.len(), size)`. That token is the one `axiom_c_claim`
  gives for C's pointer, so for a huge `p` `k` is exact under the `free`
  contract only.

## 7. Guard pages

For slabs, per result: every `fits` and every `free_target` excludes the
guard slabs. For huge runs, only at the huge lock invariant: every value
the huge lock admits keeps live runs' usable pages off every run's
guard page, but nothing is exported per call ("Huge runs" below).

**Statement** (`heap/mod.rs`; `layout/regions.rs`):

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
(`class/table.rs`) names: every `guard_interval`-th page group,
counted from 1. `commit_slab` and `commit_run` (`class/table.rs`,
below) put the guard on it: `MADV_GUARD_INSTALL` on the page group's
first page, or the whole slab or page group left `PROT_NONE` when the
advice fails, or (under `thp:always` with PMD chunks) no marker, the
slab left unused. Either way the guard page is inside the guard slabs,
so the theorem covers it. With guards off the mask is all ones and no
slab is a guard slab.

An arena commits its group a run at a time (`SlabTable::commit_run`,
without the commit lock, which it takes only to count the guard): a
guard slab always starts a run (`SlabTable::starts_run` ensures
`!r ==> !spec_is_guard(s)`), and `commit_run` installs its guard
(`install_guard`) before the run's `mprotect`; when the advice is
refused the guard slab stays `PROT_NONE` and only the slabs after it are
committed. Page groups above a slab and `thp:always` chunks use
`commit_slab`. The statement is about where blocks lie, and
`next_fresh_slab` (`class/fresh.rs`) hands out only a slab that is not a
guard slab. `starts_run` ensures only `!r ==> !spec_is_guard(s)`,
`run_end` only `s < r <= limit` (with `lemma_run_slabs`); that a run
holds no guard slab past its first slab and that every slab of a group
lies in the run of a run-starting slab at or below it are not proved
(the unit test `runs_tile_every_group`, `class/tests.rs`, checks the run
arithmetic). Each read-write commit of a class region stays in the
region and spares every guard slab, except a guard slab whose marker the
same path installed (`theorem_slab_commit_avoids_guards`, through the
commit wrapper's `requires`; see `verus/RESULTS.md`), so no guard slab
is made read-write by a commit. That a slab handed out without a commit
is read-write rests on the earlier run's `mprotect` having succeeded
(the cursor passes a run start only then) and on the `thp` mode, which
`SlabTable::runs` reads, being fixed after init: premises, as the
kernel's protections are not modelled. The slabs a run commits ahead of
the cursor are read-write before they are handed out
(`docs/hardening.md`); they are not guard slabs, so this theorem does
not cover them.

`guard_of` (`layout/regions.rs`) is `is_guard_slab`'s formula;
`guard_geom(c)` is the process's class `c` table's (the stored
`Shared`'s, by its opaque definition)
`(lg_slabs_per_page, guard_mask)`. `is_slot_addr` is `is_slot`
without the guard exclusion (slot `slot < nslots` of slab
`slab < nslabs`, guard slabs included); `slab_index` and `guard_of` are opaque: `slab_index` is
revealed only in `Regions::lemma_slab_index`, and `guard_of` in
`SlabTable::is_guard_slab`, `lemma_guard_test` (`class/table.rs`),
`ClassLine::lemma_sized_ok` (`heap/shared.rs`) and
`lemma_group_well_formed` (`class/decay.rs`). `is_slot` is in
`user_block`, so in `fits` (theorem 1), and in `slot_start`, so in
`free_target` (theorem 3): every row of the interface contract whose R
or B cell cites them carries the guard exclusion through those
predicates. The proofs are
`Regions::lemma_user_block_not_guard` (`layout/regions.rs`): a slot
lies inside its own slab, which `is_slot` puts outside the guard slabs,
and inside its class region, apart from every other class region and
from the huge region (`separated`); a huge block lies in the huge
region.

**Where `is_slot` is established.**
- Claims: `lemma_real_is_slot` (`class/mod.rs`) takes the bit's
  slab from `SlotToks::entry`, which records that the slab
  is not a guard slab. `finish` gets it from `picks_marks`,
  which `take` ensures from the class lock's `ready`
  (`!spec_is_guard`). The zero picks' slots get `is_slot` in `finish`
  itself, from the same premise: `Regions::lemma_is_slot` at the call
  (`class/mod.rs`); `SlabTable::slot_ptr` (`class/table.rs`)
  ensures only the slot's address.
- Unsized frees: `SlabTable::is_set_unchecked` (`class/table.rs`)
  ensures `r ==> !guard_slab(class, slab)`. It reads the word through
  `vspec::word::load` (`vspec/word.rs`) inside the word's invariant,
  where `bitmap::guards_ok` (`bitmap.rs`) holds: a guard slab's bits
  keep their fresh markers, so `marks_ok` makes them clear
  (`lemma_guard_clear`). `check_live` and `check_located`
  (`class/table.rs`) re-export it; `Shared::classify_free`,
  `set_slot` (so `heap::free_fast_slot`, the `free` fast path,
  `heap/paths.rs`; `checked_slot` is test-only) and the three
  `api/libc.rs` `check_live` callers (`usable_size`,
  `old_size_or_keep`, `realloc_old`) turn it into `is_slot`.
  The fast path's bit test is `set_slot`, which reads the class's line
  (`ClassLine::set_slab`): its
  slot-start test is `Class::slot_of_offset`'s on the line's copy of
  the class geometry (`ClassLine::lemma_slot_start`, over
  `layout::lemma_slot_test`), its region bound the compare with the
  line's watermark copy, which the copy's invariant bounds by the
  table's slab count, the region's (`ClassLine::wf`,
  `heap/shared.rs`, which states that the table's slab count is the
  region's), and its bit test
  `SlabTable::bit_word` (`class/table.rs`, a wrapper of
  `bit_word_held`, the word load `is_set_unchecked` calls), which
  ensures `!guard_slab` of a set bit; so `set_slot` ensures
  `is_slot`, and that the slab is below the slab count.
- Sized frees: `Shared::sized_slot` (`heap/shared.rs`) through the
  class line's `ClassLine::sized_ok`. The guard test and the
  slot-start test are the class line's; the region bound and the
  slot-start test are one 64-bit exact-division test:
  `ClassLine::lemma_sized_ok` proves that what `sized_ok` accepts
  is a slot start inside the region (`layout::lemma_slot_test64`: a
  quotient below `nslots` means the masked offset is `q * size`, below
  `2^22`, so no bit past the region is set) in a slab that `guard_of`
  does not name, by `ClassLine::wf`'s guard geometry (the guard word's page-group test on
  `rel >> lg_guard`, the slab's page group, below `2^26`). `ClassLine::wf`
  admits the table's guard mask or 0; on a zero mask every page group
  tests as a guard, so `sized_ok` accepts nothing, and `Shared::built`
  pins every line's mask to its table's.
- Cached slots: the cache invariant already holds `is_slot` of every
  entry, so pops, refills, flushes and rebuilds carry the guard
  exclusion unchanged.

`bitmap::guards_ok` is kept by every word writer: `fresh_word_invs`
starts every bit fresh, claims and releases leave the markers alone,
and `AtomicBitmap::init` (`bitmap.rs`) requires a slab that is
not a guard slab (`lemma_guards_move`); `take_group` calls it
only past the `is_guard_slab` test.

**Huge runs (at the huge lock invariant).** Each run's guard follows
its usable pages inside its extent, and live extents are pairwise apart
(`huge::Inner::wf`). `Inner::theorem_run_guards` (`huge/mod.rs`)
states the consequence for every value the huge lock admits, and for
nothing per call: the usable pages of a live run
share no page with the guard pages of any live run, its own included.

**Premises beyond the common ones.**
- `heap::shared::axiom_shared_built` (`heap/shared.rs`) also states the
  stored class lines, which carry the guard tests, as it states the
  tables.
- The class lines' premises ("Class lines", common premises): the
  watermark copy is never ahead of the watermark, and the copy's
  pointer stays valid.
- `vspec::word::load` (`external_body`, one atomic load of the word, as
  the three writers are one atomic step each); its result is the
  permission's value under sequential consistency.
- The class lock's invariant (`class::Inner`'s `LockSpec` impl) for
  `ready`, and `api::shared_deref`'s `Shared::wf`
  for the class lines and the table geometry the free paths read.

**Where the statement is weaker than the code.**
- Huge runs are covered only at the lock invariant: `alloc_run`
  returns its run's token (a `RunToken`, an `Active` descriptor's
  extent in the authorities `Inner::wf` holds), but no lemma turns
  `theorem_run_guards` into a per-result statement over the token, so no
  entry point states that a returned huge block avoids other runs' guard
  pages. A freed run's guard is made read-write again, or its marker
  removed, so its VMA merges back; freed runs' guards are not modelled
  (`theorem_huge_commit_avoids_guards` covers live runs' guards only).
- `theorem_guard_free` says that a pointer into a guard slab aborts,
  not which message it prints (`guard_free` for a handed-out guard slab
  through `check_live`'s `not_live`, `never_committed` past the
  watermark, `invalid_free` for a mid-slot address).
- The kernel's guard semantics (a fault on access) are not modelled: the
  theorem is about where blocks lie, not about what the guard does.

## 8. No syscall on the fast path

The thread-cache fast paths of `malloc` and `operator new`, `free`,
`free_sized` and flagless `sdallocx`, and `challoc_free_usable` make no
system call, except the write and `SIGABRT` of an integrity abort on a
corrupt PAC-signed cache entry or slot value.

**Statement** (`sys/mod.rs`; `api/libc.rs`):

```rust
#[verifier::opaque]
pub closed spec fn may_syscall() -> bool { true }

pub proof fn grant_syscalls()
    ensures
        may_syscall(),

#[cfg(verus_keep_ghost)]
fn theorem_no_syscall_on_fast_paths(which: u8, size: usize, p: *mut u8, Tracked(claim): Tracked<CCall>)
    requires
        integrity_aborts(),
        claim.block() == Some(p),
```

(Bodies elided.) `may_syscall()` is a capability: every function that
can enter the kernel requires it, and the thread-cache fast paths do
not. The fast paths make no system call, except the write and
`SIGABRT` of an integrity abort (a corrupt PAC-signed cache entry or slot
value), which `abort_fmt` makes under its own grant. They are, as the
entry points call them:

- `malloc`: a size of at most `SMALL_MAX` takes its class from the
  small table alone (`class_index_small` in `api/libc.rs`, over
  `class_of_small` and `layout::small_class`, whose `assert_unchecked`
  is the table's postcondition), and a larger one tail-calls
  `malloc_medium` (`class_index`); both pop through `malloc_class`:
  `api::shared_ptr`, `heap::existing_heap` (`heap/thread.rs`: the TLS
  read and `heap_from_slot`), `heap::fast_flags` and `Heap::alloc_fast`
  (`heap/paths.rs`), whose body is `pop_fast`. `calloc`'s cache pop is
  the same `pop_fast`, and `operator new`'s fast path is `malloc_fast`
  (`api/libc.rs`).
- `free` (null goes to the slow path; the heap and the dispatch byte
  come after the bit test): `heap::tls_heap_slot` (the TLS read),
  `api::shared_base`, `api::shared_deref` and `heap::free_fast_slot`.
  Inside it, the bit test `Shared::set_slot` reads the class's line
  (`ClassLine::set_slab`: its `slot_start`, `class::load_mirror` of the
  watermark copy, the aarch64 `bit_fields`, `class::table::word_index`
  and `SlabTable::bit_word`); then `Shared::free_flags` and
  `free_fast_tail`, which takes the heap from `heap_from_slot` in each
  arm of its dispatch and makes the owner test `Shared::home_owns`
  under `FREE_ROUTE` (`ClassLine::owner`, `SlabTable::owner_from`) and
  the push `Heap::push_fast`. `heap::free_fast` and
  `Shared::checked_slot` are test-only.
- `free_sized`, and `sdallocx` with no flags: the class from
  `class_index_small` (a larger size tail-calls `free_sized_medium`,
  over `class_index`), through `free_sized_with`, which `sdallocx` with
  no flags calls too; then `shared_base`, `existing_heap`,
  `shared_deref`, `Shared::push_flags` and `heap::free_sized_fast`
  (`heap/paths.rs`), whose `Shared::sized_slot` reads the class's line
  (`ClassLine::sized_ok`, over the guard word `ClassLine::guard_word`
  and `is_guard`).
- `challoc_free_usable`: `tls_heap_slot`, `shared_base`,
  `shared_deref`, `free_fast_slot` and `class_size` (`api/libc.rs`).
  `heap::free_fast_class` is test-only.

`theorem_no_syscall_on_fast_paths`, a verification-only function
(`cfg(verus_keep_ghost)`; the build has none), makes these calls where
`may_syscall()` does not hold, only `integrity_aborts()`, and verifies. It takes the block tokens
as the entry points do (`Live::empty()` for a pop, `axiom_c_claim` for
a free), which are ghost. So none of these functions
requires it, and neither does anything they call. `malloc_fast` is a
second witness: it makes the same calls and does not require it either.
Paths 0, 2, 3 and the last restate the fast-path code of `malloc_class`,
`free_block`, `free_sized_block` and `free_usable_block`; Verus checks
only the callees a restated path calls. That each restatement makes its
body's calls is a source test
(`api::restated::fast_path_restatements_match_their_bodies`), which
compares the names and order of the calls, not arguments or control
flow.
Each entry point's body (its `_block` core: `malloc_block`, `free_block`,
`free_sized_block`, `free_usable_block`; each core requires
`may_syscall()`) calls nothing else before its slow-path tail
call (`malloc_slow`, `malloc_slow_class`, `free_slow`,
`free_sized_unhinted` for a size no class holds, `free_sized_slow`,
`free_usable_slow`; also `malloc_medium` and `free_sized_medium`, for
sizes above the small table), which requires
`may_syscall()`.

**Why the capability can be trusted.**
- The only instructions that enter the kernel are the `syscall`/`svc 0`
  in `sys/raw.rs` (`syscall0`..`syscall6`, both architectures),
  each `requires may_syscall()`.
  The ledger (`verus/ledger/src/sys/raw.rs.md`) lists all 14 as
  discharged on premises by Isla up to the system call (on the Sail
  Arm and x86 models); the kernel's side (K1) is the premise, not checked.
- Every trusted body or specification that can reach the kernel, by a
  syscall, a libc call, a call through a function pointer or a verified
  function it calls unchecked, `requires may_syscall()` too:
  `linux::try_reserve` (`layout/regions.rs`), `ThreadKey::create`
  (`heap/shared.rs`), `atfork_handlers` (`api/mod.rs`),
  `background::spawn` and `pthread_join` (`purge/background.rs`;
  `init_fields`, with its `tid()` and `now_ns()`, `Meta::commit` and
  `Heap::destroy` are verified), `print::call_back`, `print::write_file`
  and `jemalloc::call_hook` (the report's call-outs, theorem 16;
  `print::emit` is verified), the
  C++ `call_handler` and `call_handler_caught` (the `nothrow_t` forms'
  call through `cxx_handler.cpp`'s catch),
  `call_get_new_handler`, `call_throw_bad_alloc` (`api/cxx.rs`), the
  `pthread_setspecific` wrapper (which may allocate; `heap/shared.rs`,
  theorem 15).
  (`shared::seed`, which reads `getrandom`, is verified: its syscalls
  are in `getrandom_exact` and `now_coarse_ns`; so is `Set::ensure`,
  which commits set words through `Meta`, and `thread.arena` calls
  `Heap::set_arena` directly. `sys::getrandom`, `read` and `write` are
  verified too: they reach the kernel through `raw::syscall3`, and their
  count bounds are kernel-fact axioms on its result.)
- `may_syscall` is opaque, so a verified function can prove it only from
  its own `requires` (or a loop invariant that carries it in from
  there), from `grant_syscalls`, or from the type invariant of a
  `Stderr` (the abort's sink, `sys/abort.rs`) or a `Sink` (the
  report's, `api/print.rs`), which only a context that holds it can
  build (`Stderr::new` and `Sink::new` require it; `Stderr`'s `Default`
  is outside the proofs). It is also closed: no module outside `sys` can
  reveal it.
- `grant_syscalls` is called only in the bodies of `extern "C"`
  functions and of `sys::abort_fmt` (`sys/abort.rs`), which never
  returns; a panic, a failed `vspec::check` included, reaches it through
  the `#[panic_handler]` (`lib.rs`). Outside tests, no Rust code
  calls an `extern "C"` function of the crate.
  `sys::tests::syscall_grants_only_at_roots` (`sys/mod.rs`)
  checks the source: every grant is in an `extern "C"` function or in
  `abort_fmt`. `sys::tests::syscall_capability_only_as_a_clause` checks
  that `may_syscall` appears only as a `requires` or loop invariant
  clause, or in `Stderr`'s and `Sink`'s type invariants.
- The later theorems' code follows the same discipline. Fork-handler
  lock handoff (theorem 13):
  `RawLock::lock_held` and `unlock_held`, `purge::background::fork_lock`
  and `fork_unlock`, and `fork_release` require `may_syscall()`; `fork_prepare`,
  `fork_parent` and `fork_child` are `extern "C"` and grant it.
  Re-entrant heap registration (theorem 15): `heap_for_thread_in`,
  `ThreadKey::set` and its `pthread_setspecific` wrapper require it,
  while `existing_heap_in`, `tls_heap_slot` and `tls::get`/`get_tp`
  (which only read the `TlsSlot` token or borrow its heap) do not, so
  the fast paths stay as above. No allocator lock across external calls (theorem 16):
  `print::emit`, `call_back`, `write_file` and `Sink::new` require it,
  and `Sink::flush` has it from `Sink`'s type invariant.

So a system call happens only below a frame that requires
`may_syscall()` or grants it, or that holds a value only such a frame
can make (below). Below a fast-path function no frame does, except
inside `abort_fmt`. A fast-path call that returns made no system call.

**Premises beyond the common ones.**
- The trusted bodies the fast paths reach make no system call:
  `sys::tls::get` and `get_tp` (a TLS load, `mrs`), `pac::auth` and
  `sign` (PAC instructions), `heap::thread::slot_heap_ref` (a cast),
  `Heap::home_ptr_at` (`heap/cache.rs`, address arithmetic),
  `arena_volatile` (`heap/mod.rs`, a volatile read),
  `vspec::atom::load_u16_at` (an atomic load, under the verified
  `load_owner` and the fast path's `owner_from`), the watermark's loads
  (`vspec::atom::load_usize` and `load_usize_seen`, atomic loads; the
  class line's watermark copy is read by the same `load_usize`, through
  the verified `class::load_mirror`), `vspec::word::load` (an atomic
  load), `vspec::flags::load` (a `Relaxed` load, under the verified
  `Shared::free_flags`), `layout::regions::entry_barrier` (an empty
  `asm!`, x86_64), `addr_to_ptr`, `api::shared_ptr`, `api::shared_base`
  and `class::table::word_index` (each an empty `asm!` on aarch64 and
  verified on x86_64, `shared_base` over `api::shared_storage_ptr`, a
  raw-pointer projection of `SHARED`), `ClassLine::guard_word` (one
  load, and on x86_64 an empty `asm!`), and the specification of
  `core::hint::assert_unchecked` (no instruction). `heap_from_slot`,
  `fast_flags`, `api::shared_deref` and `class_entry` are verified
  (Verus checks they make no system call), and so is
  `heap::paths::shared_ptr` (the cold paths only), which in the library
  returns `api::shared_ptr`.
  So do the `core` functions they use (`Option::map`, `?`, atomics,
  pointer arithmetic), which the common premises already trust.
- Two values reach the kernel from code with no `requires`, and only a
  frame with the capability can make them. `LockGuard::drop`
  (`lock.rs`, `external_body`; a destructor cannot have a
  `requires`) may `FUTEX_WAKE`, and a guard comes only from `Lock::lock`
  (`lock.rs`). `Sink`'s `fmt::Write::write_str` (verified, with
  `may_syscall()` in `Sink`'s type invariant; reached through
  `core::fmt`) flushes, and a sink comes only from
  `Sink::new` (`api/print.rs`). Both require `may_syscall()`, and no
  fast-path function takes either as an argument.
- The test checks where the grants are, not who calls the functions
  that make them: that no verified code calls an `extern "C"` function
  of the crate, and where `may_syscall` appears as a clause, are read
  from the source.
- The restated paths match the bodies they restate beyond what
  `fast_path_restatements_match_their_bodies` compares (arguments,
  control flow).
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
  `free_aligned_sized`'s hint (`class_for`) are not covered. Part of
  init's work runs later, on slow paths that
  require `may_syscall()` too: the cgroup discovery at the first
  periodic tick past `sys::cgroup::DEFER_NS` (`purge::sample` through
  `Shared::discover_cgroup_if_due`), at the first read of its figures
  (`Shared::cgroup_now`) or in `fork_prepare`
  (`discover_cgroup_for_fork`); the metadata object side's deferred
  commit at the next object or table commit (`meta::Inner::flush`);
  and the per-size THP probe at the feature line's first request
  (`api::line_ready`). The periodic tick also runs
  the lazy start's look (`purge::on_tick` → `background::auto_tick`:
  `policy::empty_bytes`, the state lock and, from an allocation's tick
  only, `start_requested` → `start`), behind `Heap::tick`, which
  requires `may_syscall()`. None is reached from a fast path.
- Faults. The first touch of a fresh slot or cache stack page can fault,
  and so can `PROT_NONE` guards and swapped-out pages. A page fault is a
  trap the kernel takes, not a system call, and no proof here models it.
- Machine code. The theorem is about the verified source. The build
  inlines these functions into the exported `malloc` and `free`. On the
  x86_64 release build, `scripts/perf-budget.sh` (theorem 45, the
  `perf-budget` check) checks that their fast paths hold no call or
  `syscall` instruction; the aarch64 machine code is not checked.
- Termination. The theorem says which instructions run, not that the
  fast path finishes: that is theorem 9 ("Fast paths take no lock and
  terminate"), whose invariant masks cover the same functions.

## 9. Fast paths take no lock and terminate

No function the fast branches of `malloc`, `free` and
`challoc_free_usable` call can acquire a lock, and each terminates; the
same holds for the callees of the `free_sized` and `sdallocx` fast branch
below `free_sized_fast`. `free_sized_fast` and `Shared::sized_slot`
declare no mask (so `any`): that the sized fast branch itself takes no
lock is read off those two bodies, not proved. The entry points
themselves are not masked, the slow paths are outside the theorem, and a
bound on the step count is read off the code, not proved ("Scope: which
paths", "What stays exempt").

**Statement.** No theorem function: the statement is the invariant mask
each fast-path function declares, which Verus checks at every call. A
function may call only functions whose mask is a subset of its own
(`opens_invariants`). An unannotated exec function has Verus's default
mask, `any`, and so do most `core` functions vstd specifies, among them
`Option`'s `?`, `Option::map`, the array-to-slice coercion and the
`core` atomics. The two lock acquisitions declare `any`:

```rust
pub fn lock(&self)                                  // RawLock, lock.rs
    opens_invariants any

pub fn lock<'a>(&'a self, Tracked(ord): Tracked<&'a mut LockOrder<'_>>, Tracked(under): Tracked<&mut LockOrder<'a>>) -> (g: LockGuard<'a, T>)   // Lock<T>, lock.rs
    ensures lock_inv(g@), lock_frame(lock_init(*self), g@), ...
    opens_invariants any
```

and so, by default, do the other acquiring functions (`try_acquire`,
`lock_slow` and both `try_lock`s, `lock.rs`). The fast paths declare
`none`, or only the namespaces the unsized free's tests open: the bitmap
words' (`bitmap::word_ns()`, the one `AtomicInvariant` the bit test
opens), the watermark copies' (`class::committed_ns()`, the one
`class::load_mirror` opens to read the class line's copy of the
watermark) and the owner words' (`class::owner_ns()`, the one
`class::table::OwnerPred` lives in, which the owner test opens):

```rust
pub(super) fn malloc_fast(size: usize, blk) -> (r: Option<*mut u8>)   // api/libc.rs
    opens_invariants none
pub fn alloc_fast(&mut self, class: usize, flags: u8, blk) -> ...    // heap/paths.rs
    opens_invariants none
pub unsafe fn free_fast_slot(shared: &Shared, t: usize, tp: usize, p: *mut u8, blk, tls) -> (r: Option<usize>)   // heap/paths.rs
    opens_invariants [crate::bitmap::word_ns(), crate::class::committed_ns(), crate::class::owner_ns()]
unsafe fn free_fast_tail(shared: &Shared, class: usize, slab: usize, p: *mut u8, flags: u8, t: usize, tp: usize, blk, tls) -> (r: Option<usize>)   // heap/paths.rs
    opens_invariants [crate::class::owner_ns()]
pub fn free_sized_fast(...) -> (r: bool)                             // heap/paths.rs
    // no mask (so `any`)
pub fn existing_heap(shared: *const Shared) -> ...                   // heap/thread.rs
    opens_invariants none
pub fn shared_base() -> (r: *const Shared)                          // api/mod.rs
    opens_invariants none
pub unsafe fn shared_deref(p: *const Shared) -> (r: &'static Shared)  // api/mod.rs
    opens_invariants none
```

`free_fast_slot` finds the slot's class and slab and tests its bit
(`Shared::set_slot`, mask `[word_ns(), committed_ns()]`), loads the
`free_flags` byte, and hands over to `free_fast_tail`, which takes the
heap from the slot value and, when the `FREE_ROUTE` bit is set, tests
the slab's owner (`Shared::home_owns`, `ClassLine::owner` and
`SlabTable::owner_from`, each `[owner_ns()]`) before the push. The free
paths reach `Shared` through `shared_base` and `shared_deref`.
`free_fast` and `free_fast_class` in `heap/paths.rs` are `#[cfg(test)]`
helpers outside `verus!`, with no mask or `ensures`, and no entry point
calls them.

(Signatures shortened.) `any` is no subset of any of these masks, so no
call chain from a masked function reaches a lock acquisition: **no
function the `malloc` and `free` fast branches call can acquire a
lock**. The fast branches call nothing else (read off the entry points,
below). The masks also rule out every other unannotated function, so
each function the masked ones can reach declares a mask itself; the
closure is the list below, and Verus checks every call in it.

**Bounded.** Every verified function in the closure has no
`exec_allows_no_decreases_clause`, so Verus proves it terminates, and
none contains a loop or a recursive call. The trusted ones contain no
loop. Each fast path is straight-line code with branches, so it runs a
bounded number of instructions whatever the heap holds. "Bounded" is
read off the code; Verus proves termination, not a step count.

**Scope: which paths.** The entry points' fast branches. The entry
points are not masked: their other branch is a tail call into the slow
path. Each C entry point mints or claims its block's token (theorem 5's
`Live`) and calls its `_block` core (`malloc_block`, `free_block`,
`free_sized_block`, `free_usable_block`), which holds the fast branch;
the cores are unmasked for the same reason. Their fast branches call
only the functions named here:
- `malloc` (`malloc_block`, `api/libc.rs`): for a size of at most
  `SMALL_MAX`, `class_index_small`, then `malloc_class`:
  `api::shared_ptr`, `existing_heap`, `fast_flags`, `Heap::alloc_fast`.
  A declined pop tail-calls `malloc_slow_class`; a larger size
  tail-calls `malloc_medium`. `malloc_fast` is the same branch as one
  masked function (`class_index`, then the same calls); it is also the
  C++ `operator new` family's fast path.
- `free` (`free_block`, `api/libc.rs`): `tls_heap_slot` (on a ghost
  `TlsSlot::entry()`), `shared_base`, `shared_deref`, `free_fast_slot`;
  then `free_slow`, which returns at once for null (the fast branch has
  no null test of its own: null is in no class region).
- `free_sized` and `sdallocx` with `flags == 0` (`free_sized_with`):
  `class_index_small` for a size of at most `SMALL_MAX` (a larger one
  tail-calls `free_sized_medium`, outside the masks as `free_sized_slow`
  is), then `free_sized_block`'s `shared_base`, `existing_heap`,
  `shared_deref`, `push_flags` and `free_sized_fast`; then
  `free_sized_slow`.
- `challoc_free_usable` (`free_usable_block`): `free_fast_slot` on the
  same TLS read as `free`'s. The class size it then returns comes from
  the unmasked `class_size`, outside the theorem.

**The closure.** These are the functions with a declared mask on these
paths, verified unless marked trusted:
- **Size and process state:** `class_index`, `class_index_small`,
  `layout::class_of`, `layout::class_of_small` and `small_class`;
  `api::shared_ptr` (trusted); `shared_base` (on aarch64 an empty
  `asm!`, trusted and partly checked by `make emptyasm`; on x86_64
  verified, over the trusted `shared_storage_ptr`) and `shared_deref`.
- **Heap lookup:** `existing_heap` (on a ghost `TlsSlot::entry()`),
  `existing_heap_in`, `tls_heap_slot`, `sys::tls::get` and the aarch64
  `get_tp` (trusted asm), the x86_64 `get_tp`, `fast_flags`,
  `heap_from_slot` (over the trusted `slot_heap_ref`).
- **Pop:** `Heap::alloc_fast`, `pop_fast`, `cache_ptr`, `home_ptr_at`
  (trusted, partly checked by Kani), `pop_top`, `CacheHeader::slots`.
- **Push:** `free_fast_slot`, `free_fast_tail`, `Heap::push_fast`,
  `push_cache_flags`, `push_cache`; `free_sized_fast` has no mask.
- **Free checks:** `Shared::push_flags`, `Shared::free_flags` (over the
  trusted `vspec::flags::load`), `set_slot`, `home_owns`,
  `Heap::arena_volatile` (trusted, discharged by Kani,
  `arena_volatile_reads_arena`); the class line's `ClassLine::set_slab`,
  `slot_start`, `bit_fields` (aarch64), `owner` and `sized_ok`,
  `class::load_mirror`, `SlabTable::bit_word`, `owner_from` and
  `load_owner` (whose load is `vspec::atom::load_u16_at`, trusted and
  partly checked), `class::word_index` (on aarch64 an empty `asm!`,
  discharged by `make emptyasm`; on x86_64 the identity, verified),
  `ClassLine::guard_word` (trusted, partly checked) and `is_guard`.
  `Shared::sized_slot` has no mask.
- **Layout:** the free function `layout::regions::class_entry` (over the
  trusted `entry_barrier` on x86_64). The class lines hold the geometry
  the free checks use; the sized check's bound is the field
  `Shared::region_hi`.
- **Primitives:** `sys::pac::sign` and `auth` (trusted asm, discharged
  on premises by Isla), `vspec::addr::addr_to_ptr` (trusted), and the
  `vspec` items under "Premises".

**Spelling in the code.** A masked function cannot use what vstd
specifies with the default mask. So the masked functions on these paths
spell `Option`'s `?` as `let Some(..) = .. else { return None }`,
`alloc_fast` matches `pop_fast`'s result instead of calling `map`, and
the array reads go through `vspec::slice::array_get_unchecked`. The
`free` dispatch reads the route from the `FREE_ROUTE` bit of the
`free_flags` byte (`Shared::free_flags`, `opens_invariants none`), which
it loads anyway. The class line's `set_slab` and `owner` are given the
class's table for its word and owner permissions; the table is not
loaded from, so the machine code is upstream's. `Shared::sized_slot`
keeps upstream's `hint?`, so it and its caller `free_sized_fast` carry
no mask.

**Premises beyond the common ones.**
- **Trusted masks.** Each trusted function's mask is a declaration that
  Verus takes on trust. Their bodies were read, and none takes a lock or
  waits:
  - `shared_ptr` (a projection of `SHARED`), and on x86_64
    `api::shared_storage_ptr` (`opens_invariants none`, a projection of
    `SHARED`; discharged by Kani).
  - `tls::get` and `get_tp` (asm), `slot_heap_ref` (a cast),
    `home_ptr_at`.
  - `arena_volatile`, `vspec::atom::load_usize` (the watermark copy's
    load), `vspec::flags::load` (one `Relaxed` load), `entry_barrier`
    (x86_64, an empty `asm!`).
  - `pac::sign` and `auth` (asm; under `cfg(test)` a counter
    increment), `addr_to_ptr`.
  - `shared_base` and `word_index` on aarch64 (each an empty `asm!`;
    `word_index` discharged and `shared_base` partly checked by `make
    emptyasm`, ledger `verus/ledger/src/class/table.rs.md` and
    `verus/ledger/src/api/mod.rs.md`).
  - `ClassLine::guard_word` (one 8-byte load and, on x86_64, an empty
    `asm!`; partly checked: Kani checks a copy of the load, a const
    assertion the layout and byte order, and `make emptyasm` the `asm!`;
    ledger `verus/ledger/src/heap/shared.rs.md`).
  - `vspec::atom::load_u16_at` (the owner word's load; partly checked by
    Kani on one thread and by loom, ledger
    `verus/ledger/src/vspec/atom.rs.md`) and `store_owner` (one store per
    slab, through the markers; partly checked by loom, ledger
    `verus/ledger/src/class/table.rs.md`), which the owner word's
    invariant rests on.
- **Wrappers.** `load_owner` (`class/table.rs`, a `Relaxed` load)
  declares the owner namespace. `vspec::slice::array_get_unchecked`
  stands for `get_unchecked` on an array.
- **Masks on existing specifications.**
  - `opens_invariants none` on the `vspec` specifications the paths use:
    `usize::leading_zeros`, `u32::rotate_right` and `u64::rotate_right`
    (`vspec/core_int.rs`), `<*mut T>::add` (`vspec/ptr.rs`) and
    `core::hint::assert_unchecked` (`vspec/misc.rs`).
  - Also declared: `vspec::word::load` (`vspec/word.rs`, the bit test's
    load), `<*mut T>::is_null` (`vspec/addr.rs`), and vstd's `ptr_ref`,
    `ptr_mut_ref`, `ptr_mut_write` and `wrapping_*`.
- **Destructors.** Verus requires every `Drop` impl to declare
  `opens_invariants none`, so the masks cannot exclude a lock taken in a
  destructor. No value on these paths has a `Drop` type (the guard
  types, `LockGuard` and `purge::Explicit`, come only from `Lock::lock`
  and the explicit purge).
- **Ghost calls.** The live-block steps (`heap::live::axiom_c_claim` at
  the C entry, `Live::take_slot` and `put_slot` through `lend_slot` on
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
  the full-cache push, the `zero_on_free` push (`PUSH_ZERO`) and every
  push under `double_free_scan` (`PUSH_STATE`) decline to the slow path,
  so they are outside the fast path by construction.
- **The `exec_allows_no_decreases_clause` functions** (common premises)
  are all off these paths: `RawLock::lock_slow` (`lock.rs`),
  `ClassState::claim_with`'s retry (`class/mod.rs`), `api::init_slow`,
  `line_ready`, `wait_line`, `init_gate_enter` and `fork_gate_enter`
  (`api/mod.rs`), the OOM handler loop `api::cxx::oom`, the purge
  thread's `main` (`purge/background.rs`), `sys::cgroup::scan_lines` and
  `Deferred::discover` (`sys/cgroup.rs`), the kernel retry loops
  `getrandom_exact`, `read_full` and `write_all` (`sys/mod.rs`), and
  `config::linux::env_var` (`config.rs`). They need a liveness model.
  Theorem 45 checks the compiled fast paths for back-edges (none).

**Where the statement is weaker than the code.**
- **Composition.** Verus checks the masked functions, not the entry
  points. That `malloc`, `free`, `free_sized` and `sdallocx` call only
  masked functions on their fast branch is read off their bodies, and so
  is that the unmasked `free_sized_fast` and `sized_slot` take no lock;
  their termination is proved (verified, no loop, no
  `exec_allows_no_decreases_clause`). `malloc`'s branch is an inlined
  copy of `malloc_fast`'s calls, not a call to it.
- **Step count.** Termination is proved; the step bound is not, because
  Verus has no cost model. The bound rests on the closure having no
  loop. Theorem 45 counts the steps of the compiled fast paths of the
  pinned release build (instructions, loads, stores), at machine level,
  outside Verus.
- **Other waits.** The masks prove that no lock acquisition is called,
  not that nothing waits. The fast paths contain no other wait either (no
  atomic read-modify-write, no spin), but that, too, is read off the
  code.
- **What Verus sees.** The theorem covers the code Verus sees, and the
  build compiles the same code: no `#[cfg(verus_keep_ghost)]` swap is on these
  paths.
- **Weak memory.** A load may return a stale value, but every branch on
  one still ends, so the bound holds.

## 10. No leak on thread exit (when the destructor runs)

Two claims, of which only the first is proved. (a) When a heap's
thread-exit destructor runs, the flush it runs first returns every slot
the heap's caches held to the release path: proved, below. (b) Every
heap gets a destructor: not proved, and not true in every run. A heap is
registered for one by `ThreadKey::set` (`heap/shared.rs`), which
returns at once, registering nothing, when the key is invalid
(`pthread_key_create` failed at init); the heap is still created and
published in TLS, so its cached slots are stranded when its thread
exits, and there is no heapless fallback for that case. A
`pthread_setspecific` failure (past glibc's first 32 keys, when its
second-level block cannot be allocated) does not abort: `ThreadKey::set`
returns `false`, and `create_heap_for_thread` destroys the heap at once
through `Heap::destroy`, the destructor's own path, so (a)'s flush covers
it, empties the slot again and serves the call without a cache, as after
a failed `Heap::create`; the next call tries again. Theorem 15 covers
registration's re-entrancy, not its success.

**Statement** (`heap/mod.rs`, `Heap::flush_all`, which
`Heap::destroy` runs first, after only the arena unbind):

```rust
pub fn flush_all(&mut self, shared: &Shared)
    requires
        crate::sys::may_abort() || crate::sys::valid_aborts(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(self).wf(),
    ensures
        final(self).keeps(*old(self)),
        final(self).same_shape(*old(self)),
        final(self).tcache_on() == old(self).tcache_on(),
        final(self).drained(),
        log_adds(final(self).released(), old(self).released(), old(self).held_all()),
```

with (`heap/mod.rs`; abridged)

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
cache memory `destroy` frees next (`free_caches_in` over the home array and
each node), so the thread-exit destructor (`heap_destructor`,
`heap/thread.rs`, through `Heap::destroy`, `heap/mod.rs`)
strands no cached block when it runs (claim (a); whether it runs is
(b)). `tcache.flush` (`Tcaches::flush`, `heap/tcaches.rs`), `malloc_trim` (`api/libc.rs`), the mallctl
purge's `flush_caller` (`api/mallctl.rs`) and
`thread.tcache.enabled` off (`Heap::set_tcache_enabled`,
`heap/mod.rs`) run the same function.

The released log (`Heap::released`, `heap/mod.rs`, ghost) is
appended to by `Heap::flush` alone (by inspection: no spec keeps it
fixed elsewhere; the theorem's `ensures` are relative, so this does not
weaken them), with the batch it hands to `Shared::release_routed` and
the same tokens. The chain that makes an entry of the log a release:

- `Heap::flush` (`heap/flush.rs`) also ensures, for any `n`:
  every other owned header holds what it held, the cache holds a subset
  of what it held, and `log_adds(final.released(), old.released(),
  old.held(cache).to_set() - final.held(cache).to_set())`: the slots
  that left the cache are the log's new entries. `lemma_flush_split`
  (`heap/flush.rs`) proves the split by counting (the cached,
  kept and released addresses are distinct, the kept are cached, the
  released are cached and not kept, and their numbers add up), and
  `Heap::lemma_flush_accounting` (`heap/mod.rs`) lifts it to the
  pairs.
- `Shared::release_routed` (`heap/shared.rs`) takes the bag of the
  batch's tokens by `&mut` and ensures `final(bag)@ == old(bag)@ -
  aset(ptr_addrs(ptrs@), 0, ptrs@.len())`: the bag lost exactly the
  batch's tokens. That each went to a `ClassState::release_owned` call
  (`class/mod.rs`), which passes it, slab by slab, through
  `release_slab` to `AtomicBitmap::release` (`bitmap.rs`), whose
  word invariant (`bitmap::WordPred`, `bitmap.rs`) must take the
  token of every bit it clears, is read from the verified bodies, not
  stated by an `ensures`. The `FOREIGN` cache's and a home cache's
  other-arena slots are released the same way through the owner arena's
  class (`release_routed`'s routing); there is no other "remote" path.
- `Heap::flush_arena` (`heap/mod.rs`) flushes one array whole and
  ensures its headers empty, the log gained what they held, and every
  other header unchanged; `flush_all` composes the home array and every
  node, using `lemma_home_apart` and `lemma_nodes_apart`
  (`heap/mod.rs`) for the headers' distinctness.

**Premises beyond the common ones.**
- `Heap::destroy` is verified: it calls `flush_all`
  first (after giving a counted heap's arena binding back, which touches
  no cache, so that a successor thread can be homed on the arena while
  the flush runs), on the heap `tcaches::heap_at`
  hands out for its pointer, `Heap::wf` by that function's premise, and
  not in use elsewhere by `tcaches::axiom_heap_use`
  (`heap/tcaches.rs`, trusted), whose `HeapUse` token `destroy`
  consumes: `heap_destructor` and `create_heap_for_thread` each make
  one. The heap `heap_destructor` hands it is the TLS slot's heap, whose
  `wf` `heap_at` assumes (common premise). After the flush
  `free_caches_in` (`heap/mod.rs`) frees each array; it requires
  every header of the array empty and reads each header through its
  permission, so its `assert_flushed` abort is proved unreachable.
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
- The arena-unbind that `destroy` runs before the flush and the exit
  purge it runs after it are not covered: they return no cached slot.

## 11. Shuffle is a permutation

The shuffle that randomizes the order of a refill's slots loses no slot
and duplicates none, whatever the random state holds.

**Statement** (`class/mod.rs`, and the `ensures` of
`shuffle_zero`):

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
The production shuffle is `shuffle_zero`, which `claim_random_zero`
calls. `shuffle` is a test-only wrapper
(`cfg_attr(not(test), allow(dead_code))`, called only from
`class/tests.rs`) that calls `shuffle_zero` with no tokens to carry
and is verified with the same `ensures`.

**Proof.** The Fisher-Yates loop (`for i in (1..len).rev()`, swap `i`
with `j = xorshift(rng) % (i + 1)`) keeps `permutes(out@, old(out)@)` as
an invariant; `lemma_swap_permutes` (`class/mod.rs`) shows one `<[T]>::swap`
leaves the multiset unchanged (the update-twice spec of `swap` in
`vspec/slice.rs`, with vstd's `to_multiset_update`). `j <= i < len` is
also checked (the `swap` precondition and the `%`), so the swap is in bounds and the remainder by `i + 1` never
divides by zero. Nothing about `xorshift`'s output is used.

**Where it applies.** `ClassState::claim_random_zero` (`class/mod.rs`) is the
only caller outside tests: with `slot_randomize`, `Heap::claim`
(`heap/flush.rs`) calls it for every refill and run rebuild, and it
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
`assume_specification` of `<[T]>::swap` (`vspec/slice.rs`;
discharged by Kani, ledger `verus/ledger/src/vspec/slice.rs.md`) and vstd's multiset lemmas.

**Scope.**
- Randomness is out of scope: nothing is claimed about the distribution
  of the permutation, the quality of `xorshift`, or the seed from
  `Heap::rng` (`heap/flush.rs`; `seed` is a `getrandom` read, or a
  clock read when that fails, and claims nothing about the value). The theorem holds for every `rng` value, including a stuck or
  adversarial one.
- The random start word of each pick that scans the bitmap in `finish`
  (not a `Fresh` or `Purged` prefix; `(xorshift(r) as usize) % words`)
  changes which free slots are claimed; the claim contract (slot tokens,
  `is_slot`, theorem 5's token protocol) is proved for every start word.

**Where the statement is weaker than the code.**
- `permutes` is stated on `shuffle_zero`'s argument and, inside
  `claim_random_zero`, on the claimed batch; no entry point's `ensures`
  says the batch was shuffled from `claim_with`'s output, because the
  pre-shuffle batch has no name outside the function (no ghost return
  value carries it). Callers see only the
  token postconditions, which say nothing about order.
- The flush's grouping (`flush::group_by_slab`, verified to permute the
  batch) is a different reordering, not covered here.

## 12. Zero-on-free (at release)

With `zero_on_free` configured, every free path clears a block's whole
usable size before it gives the block up to a thread cache, a bitmap word
or the huge region. The statement holds at the release: the cleared state
is not carried with the slot to its next hand-out.

**Statement** (`heap/shared.rs`, `heap/zero.rs`):

```rust
pub fn zero_on_free(&self) -> (r: bool)                    // Shared
    ensures
        r == (self.flags() & PUSH_ZERO != 0),
    opens_invariants none

pub proof fn theorem_zero_on_free(tracked z: &Zeroed, p: *mut u8, c: int)
    requires
        super::regions_ok(),
        process_regions().is_slot(c, p@.addr),
        z.covers(p, class_table()[c].size as int),
    ensures
        block_usable(p@.addr, 0) == class_table()[c].size,
        forall|a: int| p@.addr <= a < p@.addr + block_usable(p@.addr, 0) ==> #[trigger] z.has(a),

pub(crate) proof fn theorem_zero_on_free_huge(tracked z: &Zeroed, blk: Live, page: int)
    requires
        blk.run_block(page),
        z.addr() == blk.addr() as int,
        z.len() == blk.len(),
    ensures
        process_regions().is_huge_block(blk.addr(), blk.len()),
        forall|a: int| blk.addr() <= a < blk.addr() + blk.len() ==> #[trigger] z.has(a),
```

With `zero_on_free` configured, a free path gives up a freed block only
while it holds a `Zeroed` token (theorem 6's contents model) for all of
the block's usable bytes:

- a slot `p` of class `c`: every byte of its class's size (its usable
  size, `vspec::zero::block_usable`, one class per slot), at the moment
  its uniqueness token `tok` goes onto a thread cache (`Heap::push_cache`)
  or back to its bitmap word (`Shared::free_direct`);
- a huge run starting at `page`: whole pages from the run's start, at
  least one, a block of the huge region (the run of the free's
  `Live` token, `blk.run_block(page)`, whose length is the run's usable
  size; `Shared::zero_huge` also ensures `huge_run_zero`), at the moment
  `Shared::free_huge` releases it.

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
- `Heap::free_slow_in` (`heap/paths.rs`): `zero_slot` first (after
  the rehome test only: a young heap's `FOREIGN` free may
  first go to `rehome_on_free`, whose push declines under `PUSH_ZERO`
  and reaches `free_slow_in` for the home arena, which zeroes it), then
  either `Shared::free_direct` (no cache for the class) or
  `push_cache`. It is reached from `Heap::free_slow` (the
  home-arena push of `Heap::free`) and `Heap::free_other` (another
  arena's slot).
- `heap::free_direct` (`heap/paths.rs`), for a thread with no heap
  and `MALLOCX_TCACHE_NONE`: `zero_slot` before `Shared::free_direct`,
  `zero_huge` before `free_huge`.
- `Heap::free_huge` (`heap/paths.rs`): `zero_huge` before
  `free_huge`.
- `Shared::free_direct` and `Shared::free_huge` have no other callers
  outside tests, and `push_cache` is called only by `push_cache_flags`
  and `free_slow_in`.
- Every other free of a slot is a cache push, and every push declines
  under `PUSH_ZERO`: `Heap::push_cache_flags` (`heap/cache.rs`) and
  `push_fast` ensure `flags & PUSH_ZERO != 0 ==> !r`, so
  `free_sized_fast` (`heap/paths.rs`) ensures the same and falls
  back to the exact `heap::free` (`free_fast` and `free_fast_class` are
  `#[cfg(test)]` helpers outside `verus!`); every push is handed
  `Shared::push_flags()` (`api/libc.rs` `free`, `free_sized_block`,
  `challoc_free_usable`; `api/jemalloc.rs` `sdallocx`; `Heap::push`;
  `Heap::free_other`), whose result is `flags()`, and
  `Shared::zero_on_free` ensures `r == (flags() & PUSH_ZERO != 0)`. A
  declined push keeps the slot's token for the slow path (`!r ==>
  *final(tk) == *old(tk)`), which is `free_slow_in`.
  Plain `free` and `challoc_free_usable` push through `free_fast_slot`,
  which hands `free_fast_tail` the `free_flags` byte in place of
  `push_flags()`; `free_fast_tail` takes the heap from the slot value and
  passes `push_fast` 0 for the bytes 0 and `FREE_ROUTE` and the byte
  without `FREE_ROUTE` otherwise, so it declines whenever the byte has
  `PUSH_ZERO` (`lemma_free_route_bits`, `heap/paths.rs`, states the
  `PUSH_PAC` and `PUSH_ZERO` bit facts). `free_fast_slot` and
  `free_fast_tail` (`heap/paths.rs`) ensure
  `zero_on_free(shared) ==> r is None`, where `zero_on_free`
  (`heap/zero.rs`) is the `PUSH_ZERO` bit of `flags()`, and
  `free_fast_tail` also ensures `flags & PUSH_ZERO != 0 ==> r is None`.
  `Shared::lemma_route_flags` states that the byte's `PUSH_ZERO` bit is
  `flags()`'s.
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
- `vspec::zero::zero_raw` (`external_body`, theorem 6's token source):
  `write_bytes(p, 0, n)` makes `n` zero bytes. `zero_slot` and
  `zero_huge` clear through it (through `vspec::user::zero_user`,
  `vspec/user.rs`), with the same machine code as a direct
  `write_bytes`.
- The writes take the freed block's permission (`vspec::user::UserMem`),
  which the free path holds in its `Live` token together with the
  slot or run token: `zero_slot` and `zero_huge` borrow it
  (`Live::take_mem`, `put_mem`), and `take_slot` or `into_run` then drop
  it with the token.
  `zero_huge` gets the run's token and clears exactly that token's usable
  pages, through the cast of the page's address
  (`vspec::user::axiom_cast`, the exposed-provenance premise).
- `tok` is the slot token the freeing path takes from the block's `Live`
  token (`Live::take_slot`, theorem 5's live-block model); at the C
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
  (`api::Published`'s frozen permission: nothing writes `Shared` after
  `READY` but its atomics and the cgroup record's
  cell, `sys::cgroup::Deferred`, once); `zero_on_free` is a startup configuration key with no
  mallctl writer. Within one call `zero_on_free(shared)` is one value,
  so the guard on each obligation is the branch that cleared the block.

**Where the statement is weaker than the code.**
- It is a release-time statement: a token is held when the slot is given
  up, and it is not carried with the slot. The cache and the bitmap words
  keep no `Zeroed` token for a freed slot, so no pop or claim states
  that a slot freed under `zero_on_free` is zero when it is handed out
  (the code does not rely on it: `calloc` clears anything outside
  a cache's zero prefix).
- That the listed releases are the only ones is argued from the call
  graph above, not stated as one Verus fact: the decline of `push_fast`,
  `push_cache_flags`, `free_sized_fast` and the plain `free` route's
  `free_fast_slot` is proved, and the slow paths' coverage is by
  inspection. The obligation is not a `requires` of
  `Shared::free_direct`, `Shared::free_huge` or `push_cache`, so a
  release path that skipped it would still verify.
- The huge token's length is the free's `Live` token's (`Live::len`, the
  run's pages before the guard), which `zero_huge` ensures
  (`heap/shared.rs`); that this is `huge_usable(size)` of the
  original request rests on the run token's model (theorem 5).

## 13. Fork-handler lock handoff

The code's comments call this property "fork safety". What is proved: under the mutex and handoff premises below,
`fork_prepare` returns holding every lock of the fork lock list
(`listed`, over the arenas it found) and the background state lock,
and the parent and child handlers release each of them once. Not proved: that
`listed` names every lock in the crate (a source test checks it,
`fork_lock_list_matches_the_source_tree`), that the prepare handler
terminates (a `lock` that never returns satisfies everything), and
compatibility with other `pthread_atfork` handlers (those registered
before challoc's run in the fork window, a premise below). Reclaiming the caches of threads the child no longer has is
out of scope (design 9 leaves them marked).

**Statement** (`api/mod.rs`; asserted at the end of `fork_prepare` and
of `fork_release`):

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
    &&& 0 <= self.n() <= u32::MAX
    &&& forall|i: int| self.arenas().contains_key(i) ==> 0 <= i < self.n()
    &&& s.arenas.frozen(self.created())
    &&& self.created().value().snap().0 as int == self.n()
    &&& self.created().value().snap().1 == self.arenas()
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

// end of fork_prepare (api/mod.rs):
assert(fork_holds(*s, held, state));
// end of fork_release (api/mod.rs), after every unlock:
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
its key names in `s` (`lock_at`): the value the accessor path
through `Shared` reaches, with `arenas[i]` standing for the arena that
`get_existing(i)` returned.

`fork_release` is the body of `fork_parent` and `fork_child`. It starts
from that hold and hands every token back through
`RawLock::unlock_held` (`lock.rs`), which requires the token of the
lock it unlocks and consumes it. A token is not `Copy`, and the map is
empty at the end, so each listed lock is released once, by its holder.
`fork_child` first rewrites the background state through
`Lock::get_unlocked`, which requires the state lock's token
(`lock.rs`); `purge::background::fork_child`
(`purge/background.rs`) proves the state's lock invariant on the
value it leaves (the invariant includes `exited ==> !present`, and the
child clears both). After either handler
no listed lock is held.
In the child, no lock is owned by a thread that no longer exists: at
the fork the forking thread held every listed lock (the mutex premise
below excludes a second holder), and the child's one thread is that
thread's copy, which releases them. Under the lazy start the child
handler then sums the dirty bytes it inherited
(`policy::empty_bytes`, the lazy start's baseline): after the release,
from a lock-order token of its own (`axiom_entry`, theorem 33), so outside this
statement.

The tokens: `RawLock::lock_held` (`lock.rs`) is `lock` plus a
`LockHeld` whose `held()` is the lock. The field is private to
`lock.rs`, so outside it only `lock_held` builds a token; the handoff
axioms below are the other source.

**Premises beyond the common ones.**
- *The mutex.* `lock` returns only once this thread holds the lock, and
  no other thread holds it until `unlock`. This is `RawLock`'s futex
  contract; Verus does not check it (atomics return any value). The
  token records the calls; the mutex turns them into exclusive holds.
- *The handoff* (`axiom_fork_resumed_state`, `api/mod.rs`, and
  `axiom_fork_resumed`; axioms). Ghost state does not cross the
  C calls, so the hold `fork_prepare` ends with is restated where the
  release begins: `axiom_fork_resumed` ensures `fork_holds` at the
  `&Shared` the release reads, which it requires to be the process's
  one value, `Shared::built_at(Shared::storage())`: `shared` ensures
  that, so the prepare's hold and the release's are over the same
  `Shared` by proof. The
  premise includes that the bundle's `n` and `arenas` are what
  `fork_prepare` read, which the ensures does not name. glibc runs the
  parent and child handlers on the forking thread (in the child, its
  copy) after `_Fork`, and nothing releases a lock in between: signals
  are blocked from before the prepare handler's first lock to the
  release's last, and the atfork handlers registered before ours (whose prepare
  runs after ours and whose parent and child handlers run before ours)
  run allocator code whose releases do nothing (the fork window, below). The handlers call the axioms
  only when `FORK_LOCKED` says this fork's prepare took the locks
  (`fork_was_locked`; glibc serialises one fork's handlers and runs them
  on the forking thread, so a `true` read there is this fork's). `axiom_fork_resumed` also assumes that the saved signal mask lies
  inside the blockable signals (theorem 39). Each handler calls each axiom once. For
  `axiom_fork_resumed` a second call would prove `false` (the creation
  lock cell's permission it restates is exclusive), and the test
  `fork_hold_is_resumed_once` (`api/mod.rs`) checks that its
  wrapper `resume::resumed` has one call site; for
  `axiom_fork_resumed_state` the same test counts the call sites (one
  per handler), and nothing enforces the run-time count.
- *The fork window* (`lock::fork_holder_is_me`, premise).
  From `fork_prepare`'s `fork_window_open` to `fork_release`'s
  `fork_window_close`, the forking thread's contended acquires are
  served as acquired and its releases do nothing, so an atfork handler
  registered before challoc's may allocate there. The premise is that
  the forking thread has no allocator critical section open in the
  window, so each lock's data is as its last holder left it, and that no
  arena is built and no background thread started there
  (`Arenas::create`, `Arenas::materialise` and `purge::background::start`
  refuse on `lock::in_fork_window`, which has no specification). Verus
  models neither early return of `RawLock::lock_slow` or
  `RawLock::release`. The order of the window against the hold is
  proved: `fork_window_open` requires `lock::fork_prepared` of
  `fork_prepare`'s one root token (from `lock::axiom_fork_entry`): the
  root's top is `rank_state`, which only `RawLock::lock_held` and
  `Lock::lock_held_perm` of the lock at `rank_state` ensure (the state
  lock, taken last; `RawLock::new` ranks no lock above it, so lock order
  admits no acquisition after it on that token), and
  `RawLock::unlock_held` and `Lock::unlock_held_perm` require
  `lock::window_closed`, which only `fork_window_close` ensures. So the
  window opens after the last acquisition and closes before the first
  release.
- *The lock-order root.* `fork_prepare` takes its lock-order token from
  its own axiom, `lock::axiom_fork_entry` (`lock.rs`), the one root
  token; `axiom_entry` (`lock.rs`), every other entry point's, ensures
  `!o.root()`.
- *The arena table* is not a premise: `arena_held` and
  `count_held` (`api/mod.rs`) verify. `fork_prepare` takes the creation
  lock with `Arenas::lock_held`, which also hands it the permission of
  the lock's cell (`Lock::lock_held_perm`); the hold keeps it
  (`ForkHeld::created`). The lock's value (`arena::Created`) carries a
  ghost half of the registry's snapshot (the count and the published
  arenas), whose other half is in the registry's invariant
  (`arena::NarenasPred`, which also holds every table entry's permission
  and each published arena's frozen permission); `Arenas::create` and
  `build` change the count and the table only with both halves. So reads
  under the hold (`Arenas::count_frozen`, `get_frozen`) return the
  snapshot `fork_prepare` read, and the release and `fork_prepare`'s
  second arena scan cover the same arenas as its first scan, given the
  handoff (whose `axiom_fork_resumed` also restates the cell
  permission as part of the hold, `ForkHeld::wf`).
- `axiom_state_lock` (`purge/background.rs`, an axiom, used in
  `STATE`'s initialiser): the static is `state_static()`, whose raw
  lock is `state_lock()` (Verus does not read statics in
  specifications); its rank is proved,
  and `purge::background::state` verifies over the static's
  postcondition.
- The background state's invariant in the child also rests on
  `axiom_state_lock_inv` (`purge/background.rs`) and on
  `axiom_held`, behind `get_unlocked` (the value a holder left).
- *Completeness of the list.* That `listed` names every `Lock` in the
  crate is not proved. The test `fork_lock_list_matches_the_source_tree`
  checks the `Lock` fields and statics in the source tree against the
  test's `FORK_LOCKS` (`api/mod.rs`); that `FORK_LOCKS`, `ForkLock`
  and the handlers' bodies name the same locks is by inspection.

**Where the statement is weaker than the code.**
- Lock identity is by value along the accessor path (Verus has no
  addresses of references). The proof checks that every lock and unlock
  goes through the accessor path its key names (two locks' values cannot
  be proved equal, so a swapped token fails), not that the paths reach
  distinct addresses.
- The arenas are those `fork_prepare` found, not proved to be every
  arena that exists: `get_existing` ensures only the index of an arena
  it returns, not that `None` means none exists.
- "Released once" holds given one handoff per handler: a second call of
  `axiom_fork_resumed_state` would hand out a second state-lock token
  and still verify (no lemma says two tokens of one lock are apart); a
  second call of `axiom_fork_resumed` would prove `false`, and a source
  test checks it has one call site.
- The acquisition order is theorem 33, "Lock order is acyclic": every
  acquisition of `fork_prepare` ranks above the locks it already
  holds, with no exception (the slab-set locks are taken after the slab
  table locks). The release order is unstated.
- Partial correctness: a `lock` that never returns (a deadlock in
  prepare) satisfies everything. A prepare handler registered before
  challoc's that waits on another thread that allocates still deadlocks
  (design 9's residuals); one that allocates on its own thread runs in
  the fork window.
- Consistency in the child covers the one value the handlers write (the
  background state); the child also resets the deferred cgroup record
  and the feature line (`cgroup_fork_child`, `feature_line_fork_child`),
  which no listed lock guards, hands the statistics pool's objects the
  forking thread held to its new thread ID (`stats::pool_fork_child`),
  and reopens the init/fork gate (`fork_gate_exit`), both plain atomics
  with no specification. The handlers do not touch the other locked values,
  which keep the invariant their last holder left, as the lock axioms
  assume for every acquire.
- The signal mask's save and restore are not part of this statement;
  they are theorem 39, "Fork handlers restore the signal mask".

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

**Statement.** A slab slot belongs to one arena for the life of the
process: the arena `take_group` handed its slab's group to,
`slab_owner(c, slab)` (`class/table.rs`), and for an address
`slot_owner(c, p) = slab_owner(c, slab_index(c, p))`. The
allocation side and the release side are both stated against it: a
slot is released only into the arena that owns its slab, the arena whose
`plan` reserved in that slab, or the free aborts:

```rust
// class/mod.rs, ClassState::plan (the claim's lock hold): every
// slab a refill or claim reserves slots in is this arena's.
ensures picks_owned(self.spec_class() as int, self.spec_arena(), final(picks)@, 0, np as int),

// class/mod.rs
spec fn picks_owned(c: int, a: u16, p: Seq<Pick>, lo: int, hi: int) -> bool {
    forall|j: int| lo <= j < hi ==> slab_owner(c, (#[trigger] p[j]).slab as int) == a
}

// class/mod.rs, ClassState::release_owned (the one function that
// releases slots into an arena's class state).
ensures
    forall|i: int| 0 <= i < r ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[i]@.addr) == self.spec_arena(),
    r < ptrs@.len() ==> slot_owner(self.spec_class() as int, ptrs@[r as int]@.addr) != self.spec_arena(),

// class/mod.rs, ClassState::release_with: all or abort (`wrong_arena`).
ensures
    forall|i: int| 0 <= i < ptrs@.len() ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[i]@.addr) == self.spec_arena(),
```

`spec_arena` (`class/mod.rs`) is the class state's `arena` field;
`Arena::inv` (`arena.rs`) states that `classes[c]` has arena
`index`, so `Arena::class` ensures `r.spec_arena() ==
self.spec_index()`, and the lookups by index ensure the arena they
return has that index: `Shared::owner` and `Shared::arena`
(`heap/shared.rs`), `Arenas::get`, `materialise`, `build`
and `get_existing` (`arena.rs`). `Shared::wf` includes `arena0.spec_index()
== 0`, proved by `Shared::init` from `Arena::init`'s postcondition.

**Per free path.** Every slab free reaches an arena's class state only
through `release_owned`, from one of two places:

| Path | What is proved |
|---|---|
| `Shared::free_direct(arena, class, p)` (`heap/shared.rs`), from `heap::free_direct` (`heap/paths.rs`: `MALLOCX_TCACHE_NONE`, a thread without a heap) and `Heap::free_slow_in` (no cache for the class) | ensures `self.wf() ==> slot_owner(class, p) == arena`: it releases through `owner(arena).class(class)`, of arena `arena`, and `release_with` aborts (`wrong_arena`) otherwise. Both callers pass the owner: `heap::free_direct` the arena of `classify_free`, which ensures it is `slot_owner(c, p)` given the block's `Live` token (`heap/shared.rs`, from `check_located`, `class/table.rs`), and `free_slow_in` that of `arena_of_slot` (`heap/shared.rs`); so by their contracts `wrong_arena` cannot fire from them (Verus does not state it at the abort site). |
| `Shared::release_routed(arena, class, ptrs)` (`heap/shared.rs`), from `Heap::flush` (`heap/flush.rs`): every cache, home or `FOREIGN` | each run goes through `owner(a).class(class)` (arena `a`), whose `release_owned` releases only `a`'s slots and stops at the first slot of another arena; the next run starts at `arena_of_slot` of that slot, which ensures `slot_owner` (`class/table.rs`). Termination is proved (`decreases 2 * left + [next slot not a's]`): with owners fixed, the `changed arena during a flush` abort is unreachable, which its branch asserts (`assert(false)` in a proof block). |

Upstream of those, `Heap::free` (`heap/paths.rs`) pushes a slot onto
the home cache when `classify_free`'s arena, `slot_owner(c, p)`, is
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
counts its runs. `Shared::free_huge` (`heap/shared.rs`) credits
`owner(run.arena)`, which provably has index `run.arena`, the arena
`Huge::free_run` read from the run's descriptor, which `alloc_run`
wrote from its `arena` argument.

**How the claim side is proved.** `Inner::base` (`class/mod.rs`), part
of the class lock invariant, includes `owns(s) ==> slab_owner(c, s) ==
arena`. Slabs join an arena's ghost only in `next_fresh_slab`, as the
group `take_group` just returned, and `take_group` (`class/table.rs`)
ensures that every slab of that group has `slab_owner` the taking
arena, from its owner-word store (`lemma_fresh_group`,
`class/fresh.rs`). `take` (`class/mod.rs`) requires `ready`, so
an owned slab, and ensures its pick's owner; `plan` collects
`picks_owned`.

**Premises beyond the common ones.**
- `class::table::store_owner` (`class/table.rs`, `external_body`):
  `take_group`'s `Relaxed` store of the owner
  word fixes `slab_owner` for the group's slabs, and a `Relaxed` load
  of a handed-out group's word returns it. Together: each group's word
  is written once, under the commit lock, before the watermark passes
  the group, and never again (the only other writer is the test-only
  `corrupt_owner_word`). `owner_at`, `owner`, `check_located`,
  `arena_of_slot` and `load_owner` itself are verified.
- `ClassState::owner_locked` (`class/mod.rs`, verified) ensures its result is
  `slab_owner` and that a result naming this arena means the slab is
  owned (see "Owner words in their invariants", below).
- `stats::arena_at` (`stats.rs`, verified) ensures the arena it
  returns has index `i`, from `get_existing`'s postcondition; not a
  premise.
- `arena::axiom_arena_assemble` requires every class state's arena
  to be the parts' index (`Arena::init` proves it from
  `ClassState::new`'s postcondition); `heap::shared::
  axiom_shared_built` states `arena0` as written, as it states the
  tables.
- "Written once" is checked, up to the lock premise: `store_owner`
  uses up the owner-store markers (`table::owner_mark`) of its group's
  slabs, which the class's table minted once each (`table::mark_loc`,
  the stored `Shared`'s table's marker authority by its opaque
  definition, so only that table, `SlabTable::is_process`, can store an
  owner, and `slab_owner`, keyed by class, names that table's) and its commit lock holds from the
  watermark on (`TableInner::inv`), so a second store of a slab's owner
  does not verify, given that every holder of the commit lock restores
  its invariant before releasing it (`TableInner`'s `LockSpec` impl,
  checked at the release: every holder outside the tests ends with
  `LockGuard::unlock`). "Handed out before read"
  is not checked by Verus: `owner_at` requires only `slab < nslabs`,
  and its callers (`owner`, `check_located`) test the watermark first;
  `owner_from` (the class line's path, through the
  line's copies of `owners_base` and `lg_group`) calls `load_owner` with
  no witness, so it claims nothing of the value (its caller tests the
  class line's watermark copy, which is never ahead of the watermark:
  "Class lines", common premises).
- Sequential consistency for the owner words, as for every atomic.
  `check_located` reaches the owner load after a `Relaxed` watermark
  load (`check_live`), the one owner path without `Acquire`.
- **Owner words in their invariants.** `load_owner` verifies: the watermark's invariant
  (`table::WatermarkPred`) holds the owner words' witness markers of
  every slab below it, which a load hands its reader
  (`vspec::atom::load_usize_seen`), and an owner word's invariant
  (`table::OwnerPred`) holds the word's permission and the stored owner
  once the group's markers are persistent, so "handed out before read"
  is checked for `owner` and `check_located`; the fast path's
  `owner_from` claims nothing of the value, which `home_owns` only
  compares. `store_owner` is trusted (partly checked by loom, ledger
  `verus/ledger/src/class/table.rs.md`) and also
  records the value in the word's permission (`vspec::atom::u16_value`,
  read back by `vspec::atom::load_u16_at`). `owner_locked` verifies:
  the word records its taker (the registry's record of the taking
  state's arena, `arena::Published`, and the group's slabs in that
  state's owned-slab record, `Inner::oauth`), `take_group` and
  `owner_locked`'s callers hold the registry's record of their own arena
  (`ClassState::published_by`), and two records of one index agree, so
  "the word names this arena ⇒ the slab is owned" is proved, failed
  builds' class states included (they hold no record). What remains
  trusted on this path: `store_owner` (one store per slab, through the
  markers), `vspec::atom::load_u16_at` (partly checked by Kani on one thread and
  by loom) and `load_usize_seen` (partly checked by loom; ledger
  `verus/ledger/src/vspec/atom.rs.md`); `u16_value` stays the word's
  value because every store to an owner word is `store_owner`'s, which
  the source test `access_bitmap_words_only_through_vspec_word`
  (`tests/loom/tests/access.rs`) checks for `class/table.rs`: its one
  atomic write is `store_owner`'s store, the commit lock's invariant (`TableInner::inv`; its `seen` and `next_slab`
  conjuncts, as the others, checked at the release by `LockGuard::unlock`),
  the class lock's frame (the owned-slab record's location never
  changes, `class::Inner`'s `LockSpec` impl),
  and sequential consistency.

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
- The `free` fast path's owner test only picks the cache:
  `free_fast_tail` (`heap/paths.rs`) calls `Shared::home_owns`
  (`heap/shared.rs`) when the `free_flags` byte has `FREE_ROUTE`
  (several arenas), and skips it with one arena. `home_owns` reads the
  owner word through the class line (`ClassLine::owner`,
  `SlabTable::owner_from`, verified, the same `load_owner`) with no
  witness, so it only compares the value.

## 15. Re-entrant heap registration

While a thread registers its new heap, an allocator call nested in that
registration (glibc's `pthread_setspecific` may `calloc`) returns no
heap, creates none and leaves the thread's TLS slot as it found it. On
an empty slot a heap lookup makes exactly one heap creation, and above
the sentinels it returns the published heap and writes nothing.

**Model.** The calling thread's TLS slot is per-thread ghost state: a
`sys::tls::TlsSlot` token (`sys/tls.rs`) whose `value()` is what the
slot holds and whose `creates()` counts the `Heap::create` calls made
with it. `get`, `get_tp` (`sys/tls.rs`) take it
shared and return its value; `set` takes it `&mut` and makes
the written value its value. `heap::thread::create_heap_for_thread`
(`heap/thread.rs`) takes it `&mut` and adds one to `creates` right after
each of its `Heap::create` calls; it is the only caller of
`TlsCell::note_create` (`sys/tls.rs`), the only writer of `creates`.
`Heap::create` itself does not take the cell, so the count is a
convention of that caller. A function handed only `&TlsSlot` can therefore
neither write the slot nor add to its create count, and
one handed `&mut TlsSlot` states what it does to both in its `ensures`.
The token pairs the slot's value (`TlsCell`) with the heap loan
(`HeapLoan`), and `heap::thread::slot_ok` ties the two.
That it does neither at all also needs that nothing it calls makes a
token of its own: `TlsSlot::entry` is `pub(crate)`, and its only callers
are the four top-level users of the slot named in the premises below
(checked by inspection).

The public readers keep their signatures: `heap_for_thread` and
`existing_heap` (`heap/thread.rs`) make the token at entry
(`TlsSlot::entry`, `sys/tls.rs`, value unconstrained until read)
and run the cores `heap_for_thread_in` and `existing_heap_in`
(`heap/thread.rs`), which carry the contracts.

`ThreadKey::set` (`heap/shared.rs`) is the call that may re-enter:
glibc's `pthread_setspecific` `calloc`s a second-level block of 32 keys
the first time a thread sets a non-null value for a key in it (keys from
index 32 up; the first 32 live in the thread descriptor). Its wrapper
`heap::shared::pthread_setspecific` (`heap/shared.rs`,
`external_body`) models every nested allocator entry as a core holding
the token the outer call lends it: it requires `reentry_benign` of the
slot and ensures the token unchanged, which is what the cores guarantee
at a benign value (below).

**Statement** (`heap/thread.rs`;
`heap/shared.rs`):

```rust
pub open spec fn reentry_benign(v: usize) -> bool {
    v == TLS_CREATING || v == TLS_EXITED
}

pub fn heap_for_thread_in<'a>(shared: &'a Shared, Tracked(cell): Tracked<&mut TlsCell>,
    Tracked(loan): Tracked<&'a mut HeapLoan>) -> (r: Option<&'a mut Heap>)
    requires shared.wf(), slot_ok(*old(cell), *old(loan)),
    ensures
        r matches Some(h) ==> h.wf(),
        reentry_benign(old(cell).value()) ==> r is None && *final(cell) == *old(cell) && *final(loan) == *old(loan),
        old(cell).value() > TLS_EXITED ==> r is Some && *final(cell) == *old(cell) && final(loan).lent(*old(loan)),
        old(cell).value() == 0 ==> final(cell).creates() == old(cell).creates() + 1,
        old(cell).value() == 0 && r is None ==> final(cell).value() == 0,

pub fn existing_heap_in<'a>(shared: *const Shared, Tracked(tls): Tracked<&'a mut TlsSlot>)
    -> (r: Option<&'a mut Heap>)
    requires slot_ok(old(tls).cell, old(tls).loan), shared == Shared::storage() as *const Shared,
    ensures
        r matches Some(h) ==> h.wf(),
        r is None <==> old(tls).value() <= TLS_EXITED,
        r is None ==> *final(tls) == *old(tls),
        r is Some ==> final(tls).lent(*old(tls)),

fn create_heap_for_thread<'a>(shared: &'a Shared, slot: usize, Tracked(cell): Tracked<&mut TlsCell>,
    Tracked(loan): Tracked<&'a mut HeapLoan>) -> (r: Option<&'a mut Heap>)
    requires shared.wf(), slot == old(cell).value(), slot_ok(*old(cell), *old(loan)),
    ensures
        r matches Some(h) ==> h.wf(),
        slot != 0 ==> r is None && *final(cell) == *old(cell) && *final(loan) == *old(loan),
        slot == 0 ==> final(cell).creates() == old(cell).creates() + 1,
        slot == 0 && r is None ==> final(cell).value() == 0,

pub fn set(&self, heap: *mut Heap, Tracked(tls): Tracked<&mut TlsCell>) -> (r: bool)  // ThreadKey
    requires reentry_benign(old(tls).value()),
    ensures *final(tls) == *old(tls),
```

(Line breaks added.) While `create_heap_for_thread` is inside
`ThreadKey::set`, the slot holds `TLS_CREATING`: the call's `requires`
is checked there, after `set(TLS_CREATING)` and before the heap is
published. A nested `heap_for_thread` or `existing_heap` on that thread
then returns `None` and leaves the token exactly as it found it: the
slot still holds `TLS_CREATING`, no `Heap::create` call was made, and no
heap was handed out or published. `existing_heap_in` takes the token
`&mut` (it lends the heap through the token's loan,
`final(tls).lent(*old(tls))`), and on a `None` it leaves the token as
it found it; its callees (`tls_heap_slot`, `fast_flags`,
`heap_from_slot`) make no token of their own. The same holds at
`TLS_EXITED`, after the thread's destructor has run. On an empty slot
`heap_for_thread` makes exactly one `Heap::create` call and leaves the
slot empty unless it returns a heap; above the sentinels it returns the
published heap and writes nothing. The empty-slot case includes the
heapless path: when `ThreadKey::set` returns `false` (`pthread_setspecific`
failed) the heap is destroyed (`Heap::destroy`, with the slot still on
`TLS_CREATING`) and the slot written back to empty before `None` is
returned, so the next call on the thread creates again.

**If `Heap::create` itself allocated.** `Heap::create` runs on the
empty slot, before `TLS_CREATING` is set, and its contract says it
leaves the token's value alone: the model has no nested entry there.
That rests on a premise checked by inspection, not by Verus: nothing
`Heap::create` reaches calls code that can allocate. Its callees are
`assign_home` (`Arenas::assign`, atomics and the arenas lock),
`create_detached`, which calls `Meta::alloc_raw` (verified; the
metadata lock, and `mprotect` as a raw syscall on its commit path) and
`init_fields` (`tid()` and `now_ns()`: `gettid` and `clock_gettime` as
raw syscalls), `mark_bound` and `mark_rehome` (field stores), and
`Arenas::unbind` on the null path. The
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
- The slot premise (`sys/tls.rs`): a token is not read after
  the slot has changed other than through it. `TlsSlot::entry`
  (`pub(crate)`, verified body, trusted in effect) makes a token with an
  unconstrained value at each top-level user of the slot:
  `heap_for_thread`, `existing_heap`, `heap_destructor`
  (`heap/thread.rs`, writes `TLS_EXITED`) and the background thread's
  `create` (`purge/background.rs`). An entry nested in another
  stands for the outer token lent to it, which the nested call leaves
  unchanged when the value is benign. Writers of the slot are
  `create_heap_for_thread` (only on an empty slot), `heap_destructor`
  and `create`, all through their tokens. `create` parks a slot above
  the sentinels on `TLS_CREATING` across `pthread_join` and
  `pthread_create` and then restores it. When the slot is empty it
  does not park at first; a nested free in `pthread_join` may create
  this thread's heap through the lent token, and `create` then reads
  its token again and parks that heap across `pthread_create`; `spawn`
  (trusted) requires an empty slot or a sentinel, as `pthread_join`
  does. A nested allocation in `pthread_create` at a slot
  still empty may create the heap too, and `create` does not read its
  token after that call, so the premise holds. `create`'s contract
  states the result for the slot's value: `TlsCell::kept`
  (`sys/tls.rs`), which `create` (`purge/background.rs`)
  ensures, as do `pthread_join` and `spawn` (both
  `external_body`, so assumed there) and the callers up to the slow
  paths that may start the thread, says a non-empty slot ends as it was
  and that an empty slot ends empty or
  holding a heap, never a sentinel; a `create` that left
  `TLS_CREATING` in the slot fails to verify. It does not say which: a
  `create` that emptied a slot a nested entry filled, losing that heap,
  also satisfies `kept`.
  The token also carries the thread pointer (`TlsSlot::tp`) and, while the slot holds a heap, the heap's
  linear permission (`heap::thread::HeapPerm`), which
  `create_heap_for_thread` puts there and the slot readers borrow
  (`heap_from_slot`, through the trusted `slot_heap_ref`); `entry` is
  an axiom returning `&'static mut TlsSlot`, the thread's one slot,
  with `heap::thread::slot_ok`: a heap value comes with the permission
  to the heap it names, well formed (so `api::ready()`), and nothing
  else with one. It is made at the same top-level users and also at
  the `free` entry points' fast paths (`api::libc`: `free`,
  `free_usable`), which read the slot through it. Above the sentinels
  `heap_for_thread_in` and `existing_heap_in` (which borrows the
  token `&mut`) leave the token as it was but for what its heap holds
  (`TlsSlot::lent`); at a benign value they leave it exactly as it
  was.
- `heap::shared::pthread_setspecific` (`external_body`): an FFI call
  whose nested allocator entries reach the slot only through the cores
  (or, at a benign value, `create`, which then writes nothing), holding
  the token it is lent.
- `sys::tls::get`, `get_tp`, `set` (inline asm, `external_body`): a read
  returns the token's value; a write makes it the token's value and leaves its thread pointer and
  heap permission alone; the aarch64 `get_tp` returns the token's thread
  pointer.
- `Heap::create` makes no call that can allocate (above).
- Explicit caches reach a heap through `tcaches::heap_at` with a
  `HeapUse` token (`tcaches::axiom_heap_use`, `heap/tcaches.rs`): one
  token per call that reaches an existing heap.
- `Heap::destroy` (verified, theorem 10), on the heapless path:
  it takes no slot token and writes no slot (no call in it does; the slot
  holds `TLS_CREATING` across it, so a nested entry would be benign).

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
  theorem says nothing about that nested creation (a heap the join
  creates is then parked across `pthread_create`).
- The token makes the claim per thread only by construction: nothing in
  the model names the thread, and a token never crosses threads because
  the slot is thread-local.

## 16. No allocator lock across external calls (partly premised)

The statistics reports never call out of the allocator (to a callback,
the `malloc_message` hook, `fwrite` or `write(2)`) while they hold an
allocator lock they took, so an allocation made by the callee cannot
deadlock on one of them. Verus checks this through theorem 33's lock-order
token; that the report's C caller holds no allocator lock is a premise.

**Statement.** What Verus checks: every call the reports make out of
the allocator goes through `emit`, which requires a lock-order token at
top 0, and a report holds such a token only while no guard it took is
alive. The token is theorem 33's `LockOrder`, so top 0 means the
calling thread holds no allocator lock, as far as the verified lock
order reaches; what stays assumed is the entry premise
(`axiom_entry`, below) and theorem 33's own premises. The reports are
`malloc_stats_print` and `je_malloc_stats_print` (`api/print.rs`,
through `stats_print`), `malloc_info` and
`malloc_stats`. Their calls out are the caller's `write_cb`,
the `malloc_message` hook through `malloc_write`
(`api/jemalloc.rs`; with no hook, `write(2)` to stderr), `fwrite`
on the caller's stream, and `write(2)` to stderr. A callback, a hook or
`fwrite` may allocate (glibc allocates a stream's buffer on its first
write), so a lock held across one deadlocks the allocation.

`emit` is verified. It hands the buffer to one of three trusted functions, which
make the calls out: `call_back` (`api/print.rs`, the caller's
callback), `write_file` (`fwrite` on the caller's stream) and,
for the `malloc_message` hook, `call_hook` (`api/jemalloc.rs`,
called only by the verified `malloc_write`, whose only caller is
`emit`); or, for `malloc_stats` and a `malloc_write` with no hook, to
the verified `sys::write_all` (`sys/mod.rs`, raw `write(2)`, which
allocates nothing). `emit`, the three trusted functions and
`malloc_write` each require the token at top 0:

```rust
fn emit(target: &Target, buf: &[u8; 1024], len: usize,        // api/print.rs
    Tracked(ord): Tracked<&LockOrder>)
    requires crate::sys::may_syscall(), len < 1024, buf@[len as int] == 0, ord.top() == 0,

#[verifier::external_body]
fn call_hook(cb: Hook, buf: &[u8; 512],                       // api/jemalloc.rs
    Tracked(ord): Tracked<&LockOrder>)
    requires crate::sys::may_syscall(), ord.top() == 0, exists|i: int| 0 <= i < 512 && #[trigger] buf@[i] == 0,

pub(super) fn malloc_write(s: &[u8], Tracked(ord): Tracked<&LockOrder>)   // api/jemalloc.rs
    requires ord.top() == 0, ...
```

Each of the three entry points takes its token from `axiom_entry`
(`lock.rs`: `o.top() == 0`, `!o.root()`) once, after
`super::shared()` and right before it makes the report's `Sink`
(`api/print.rs`). The `Sink` keeps the
token from creation to drop (its ghost field `ord`, at top 0 by
its invariant). `Sink::flush` is verified and is the only
caller of `emit`; it stores the NUL at `buf[len]` that `emit` requires
and passes the sink's token. Every call of the report
that may take an allocator lock gets a token lent from the sink
(`Report::lend`, through `LockOrder::lend`, `lock.rs`):
same top, not a root, and borrowing the sink for as long as it lives.
A lock taken with the lent token hands out its guard's token for the
same borrow (`Lock::lock`, `lock.rs`), so the sink cannot be
written, and so not flushed, until every guard taken under it is gone;
and a token that is not a root cannot take a lock that outlives its
guard (`RawLock::lock_held` needs a root). So Verus checks:
- No verified code makes a report's calls out without a token at top
  0: `call_back`, `write_file` and `call_hook` are the only functions
  that make them (besides the raw `write(2)` to stderr), and every call
  to any of them must supply one.
- No report calls out while it holds a lock it took: the sink's token,
  the only one `emit` gets, is borrowed while any guard taken under it
  is alive.

The unverified code of `api/print.rs` and `api/jemalloc.rs` (the trusted
bodies of `print.rs`, among them `call_back` and `write_file`, and
`jemalloc.rs`'s `message_hook` and `call_hook`) could still call `emit`,
`malloc_write` or a call-out with a forged token; it does not (by
inspection; no source test checks it).

**Why the entry premise holds.** The entry point's C caller holds no
allocator lock, and `shared()` releases every lock it takes before it
returns. `axiom_entry` assumes this. It is a C-program contract in the
seam ledger; see "Where the statement is weaker" for the callers that
break it. The reports' callees that lock (`stats::collect`,
`collect_arena` and `collect_all`, through `Huge::stats`, `Meta::stats`,
`ClassState::bin_stats` per class and the `Shared.stats` lock; the slab
histogram's `ClassState::is_partial`; `policy::empty_bytes`, through
`ClassState::empty_bytes` and `Huge::empty_bytes`) take their locks with
the lent token, so the borrow above covers them. The only acquisitions
of a bare `RawLock` are the fork handlers' (`fork_prepare`,
`api/mod.rs`; `background::fork_lock`, `purge/background.rs`),
which need the root token `axiom_fork_entry` gives, and no report
reaches them.

**Run-time check.** The tests
`a_report_callback_may_reenter_the_allocator` and
`a_malloc_info_stream_may_reenter_the_allocator` (`api/print.rs`)
re-enter the allocator from every call out of a text report, a JSON
report and `malloc_info` (through an unbuffered `fopencookie` stream), and
fail when a call does not finish within a bound: the run-time evidence
for `call_back`'s and `write_file`'s premise.

**Scope.**
- The four report entry points. `malloc_write`'s only caller outside
  tests is `emit`. The allocator's other calls out (`pthread_*`, the
  C++ `new_handler`, the abort path's raw `write`) are outside the
  theorem.
- The `malloc_message` hook is called from the same `emit` as the
  callback, under the same token. It has no run-time test of its own:
  the hook is process-wide, and a test that sets it races the other
  hook tests.

**Premises beyond the common ones.**
- `axiom_entry` (the entry premise), and theorem 33's premises for the
  locks the callees take.
- `call_back`, `write_file` and `call_hook` (`external_body`): a call
  through the caller's function pointer when it gave one, one `fwrite`,
  and a one-line call of the hook, claiming nothing. `emit` is verified,
  and the NUL a callback or hook reads is checked (`Sink::flush` stores
  it; `emit`, `call_back` and `call_hook` require it).

**Where the statement is weaker than the code.**
- **The token is only as good as the lock order.** The token is the
  verified `LockOrder` every acquisition takes, so what remains assumed is theorem 33's premises
  (the fork root, `axiom_fork_entry`, and the trusted bodies it names) and the entry premise.
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

The B column of the interface contract below; not numbered.

**Statement.** Returning calls satisfy the pointer-shape checks and the
selected argument checks below. Pointer shape needs no C contract for
the call itself; like every theorem it holds in a run that has made no
earlier non-live free, which the live-pointer contract (`axiom_c_claim`)
excludes. For the pointer shape (no block start, misaligned, interior),
the size hints of a live slot, a live huge run or a non-block pointer, cache ids of
`MAX_TCACHES` or more and the alignments, a bad argument either makes
the call not return or gets the documented error result. "Does not return" is what is proved; the code
aborts, but that the call reaches `SIGABRT` rather than hanging is not
proved (partial correctness). A stale or double free of a slot or huge
page start, and a size hint on such a pointer, are not proved to be
rejected: the C-boundary assumption excludes them (*(C)* below), and
the run-time checks that catch some of them are code, not theorem (the
rows marked *(C)*). Not checked by the code either: a free of a block
that was freed and reallocated,
which no allocator can tell from a live one (table below). Stated as
`ensures` on the Rust functions the exports call (an abort satisfies
every postcondition, so each one says what holds of a call that
returns):

```rust
// heap/mod.rs (theorem 3's target; the size hint's class)
pub open spec fn free_target(p: *mut u8) -> bool {
    ||| slot_start(p)
    ||| process_regions().is_huge_block(p@.addr, 1)
}
pub open spec fn hint_ok(p: *mut u8, hint: Option<usize>) -> bool {
    forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) ==> hint == Some(c as usize)
}

// api/libc.rs; api/cxx.rs; api/jemalloc.rs
fn free_sized(p, size)              ensures p@.addr != 0 ==> hint_ok(p, spec_class_for(size, 1)),
fn free_aligned_sized(p, align, size)
    ensures p@.addr != 0 ==> hint_ok(p, if is_pow2(align) { spec_class_for(size, align) } else { None }),
fn sdallocx(p, size, flags)
    ensures p@.addr != 0 ==> hint_ok(p, spec_class_for(size, 1usize << ((flags as u32) & 0x3f))),
            p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),

// heap/mod.rs (the huge half of the size hint, over the caller's token
// `blk` for `p`); api/libc.rs `free_sized_block`, api/jemalloc.rs
// `sdallocx_block` (the cores the exports call with C's token)
pub open spec fn huge_hint_ok(blk: Live, p: *mut u8, size: usize) -> bool {
    blk.claims(p) && !slot_start(p) && process_regions().is_huge_block(p@.addr, 1) ==> {
        &&& size + live::page_size() - 1 <= usize::MAX
        &&& crate::vspec::zero::huge_usable(size as int) == blk.len()
    }
}
fn free_sized_block(p, size, hint, blk)   ensures p@.addr != 0 ==> huge_hint_ok(blk, p, size),
fn sdallocx_block(p, size, flags, blk)    ensures p@.addr != 0 ==> huge_hint_ok(blk, p, size),

// heap/mallocx.rs; api/jemalloc.rs
pub open spec fn bad_tcache(t: Tcache) -> bool {
    t matches Tcache::Explicit(i) && i as usize >= MAX_TCACHES
}
pub open spec fn tcache_checked(size: usize, flags: c_int) -> bool {
    let f = mallocx::decode_spec(flags as u32);
    mallocx::bad_tcache(f.tcache) ==> f.arena is Some && spec_class_for(size, 1usize << f.lg_align) is None
}
fn mallocx(size, flags)  ensures tcache_checked(size, flags), /* and theorem 2's fits */
fn dallocx(p, flags)     ensures p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),

// api/cxx.rs
fn new_aligned(size, align, nothrow)  ensures !is_pow2(align) ==> r@.addr == 0,

// heap/mod.rs; heap/live.rs
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
| Huge run freed with a size that does not round up to its pages | sized frees | abort (`huge_hint_mismatch`) | yes for a live run: `huge_hint_ok` over C's token for it; for a non-live one, *(C)* |
| Non-power-of-two or zero alignment | `aligned_alloc`, `memalign` | NULL (`EINVAL`) | yes (row 5; `EINVAL` by *(errno)*) |
| Alignment not a power of two, or not a multiple of `sizeof(void *)` | `posix_memalign` | `EINVAL` | yes (row 6) |
| Alignment that is not a power of two, 0 included | `new`, `new[]` (`align_val_t`) | NULL (`nothrow_t`) or `bad_alloc`, at once (no handler call, by inspection) | yes: `new_aligned` (the NULL or the throw) |
| Any `MALLOCX_LG_ALIGN` (0 to 63) | `mallocx`, `rallocx` | a block aligned to it, or NULL | yes: `fits(r, size, 1 << lg_align)` (theorem 2): no misaligned block |
| `MALLOCX_TCACHE(t)`, `t >= MAX_TCACHES` (1024; the field reaches 4093) | `mallocx`, `dallocx`, `sdallocx` | abort (`invalid_tcache`, or first `hint_mismatch` on `sdallocx`), except a huge `mallocx` with an explicit arena, which never looks the cache up | yes: `tcache_checked`, `!bad_tcache` (the frees' for a live block or a non-block pointer; *(C)* otherwise) |
| `MALLOCX_TCACHE(t)` of a cache never created | as above | abort, with the same exception | no: existence is a run-time table load, not modelled |
| `MALLOCX_ARENA(a)` of an arena that does not exist | `mallocx`, `rallocx` | NULL | no: as above |
| Double free of a slot back in its bitmap (stale), or a free of a never-allocated slot of a handed-out slab | plain frees | abort (`check_live`, `check_located`: `invalid_free`) | no: excluded by the C contract, *(C)* |
| The same | `malloc_usable_size`, `sallocx`, `xallocx` | abort (`check_live`) | no: the queries take no token, and nothing states it |
| The same, through a sized free | sized frees | abort at the cache flush (`double_free`, `straddling_double_free`); with the opt-in `exact_sized_free` key, at the call (`check_live`) | no: excluded by the C contract, *(C)*; under the key, that the free reaches the test is `theorem_exact_sized_free` |
| Double free while the first copy is still in a thread cache | every free | with `double_free_scan` enabled (off by default), abort at the call: the slot's state byte (`heap::slotstate`) is not `LIVE`; else abort at the flush, or the slot is handed out twice first, as under jemalloc | no: excluded by the C contract, *(C)* |
| The same, through `realloc` and the queries | `realloc` family, `malloc_usable_size`, `sallocx`, `xallocx` | with `double_free_scan`, abort after the bit test (`Shared::check_not_cached`); without it no abort at the call: the slot bit is still set, so `check_live` passes. `realloc(p, 1000)` after `p = malloc(16); free(p)` copies from `p` and frees it again, which pushes a second copy; a kept block is returned as is | no: excluded by the C contract, *(C)*; the queries take no token |
| Double free of a huge run | every free | abort (`Huge::free_run`: `already_free`, or `not_a_run` once the freed run has been merged or left the table) | no: excluded by the C contract, *(C)* |
| Free of a block that was freed and reallocated (ABA) | every free | frees the new block | undetectable by any allocator |

*(C)*: a pointer that is a slot start or a huge page start but not a live
block is excluded by `heap::live::axiom_c_claim` (`heap/live.rs`,
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
  class bases aligned to their slab) and `lemma_huge_fits` (a huge page
  start is page-aligned). `lemma_slot_interior`: two slot starts of one
  class are a class size apart (`lemma_slots_apart`), slots of two
  classes lie in two strides (`lemma_classes_apart`), and a slot lies
  outside the huge region (`lemma_slot_run_apart`).
  `lemma_run_interior`: page starts of the huge region are a whole
  number of pages apart.
- Size hint: `check_size_hint` (`heap/shared.rs`) aborts when
  `Regions::classify` says `Slab { class }` and `hint != Some(class)`.
  `classify` and `slab_slot` (`layout/regions.rs`) also
  ensure that with separated regions they find every slot start in its
  class, guard slabs included (`lemma_slot_addr_found`: the
  stride shift gives the class, the offset is inside the region, and
  the slot test of `Class::slot_of_offset` accepts it), so `hint_ok`
  holds whenever `check_size_hint` returns. The fast path
  (`free_sized_fast`, `heap/paths.rs`) pushes only when
  `Shared::sized_slot` found `p` a slot of the hinted class, and a slot
  has one class (`lemma_hint_ok`, from `vspec::zero::lemma_slot_unique`).
  `class_index` ensures `r == spec_class_for(n, 1)`, so the hints
  `free_sized` and `sdallocx` (flags 0) pass are `spec_class_for`'s.
  For `Huge { page }`, `check_size_hint` looks the page up
  (`Huge::lookup`, with the caller's run token, which makes the result
  that run's descriptor: `Active`, starting at `page`, its usable bytes
  the token's) and aborts (`huge_hint_mismatch`) unless
  `Huge::usable_for(size)` is the run's usable size, so it ensures
  `huge_hint_ok(blk, p, size)`. `classify` also ensures that with
  separated regions every page start of the huge region is `Huge`
  (`in_superblock` ensures a hit lies in the strides), so a run is
  never passed over as `Invalid`. The fast path declines a huge pointer
  (`free_sized_fast` ensures a push is a slot start's), and a declined
  push leaves the token's claim and length, so the cores
  (`free_sized_block`, `sdallocx_block`, `sdallocx_flags`,
  `sdallocx_routed`, `free_sized_slow`, `free_sized_unhinted`) ensure
  `huge_hint_ok` of C's token.
- Flags: `Tcaches::get` (`heap/tcaches.rs`) ensures `None` for an
  id of `MAX_TCACHES` or more; `mallocx::alloc`, `alloc_huge` (without
  an explicit arena) and `free` (`heap/mallocx.rs`)
  then reach `invalid_tcache`.
- `new_aligned`: `new_aligned_block` tests `align.is_power_of_two()`
  (whose specification is `is_pow2`) before any allocation or handler
  call: a non-power-of-two alignment, 0 included, returns NULL for the
  `nothrow` forms and leaves through `throw_bad_alloc` (`-> !`) for the
  others; `oom` (`api/cxx.rs`) is reached only with a power of two.

**Premises beyond the common ones.** None new. The size-hint rows rest
on the class table premise `class_entry` through `slab_slot`'s
completeness (`lemma_class_table_wf` is verified); *(C)* is the `free`
contract.

**Scope.**
- NULL: `free`, `free_sized`, `dallocx`, `sdallocx` and the `delete`s
  return for NULL (at once, except `free` and, on x86_64, the hinted
  sized frees, after their fast path declines it; not stated: there is no model of the
  allocator state a no-op leaves alone); `malloc_usable_size(NULL)` is 0
  (proved); `sallocx` and `xallocx` abort for NULL (proved: they ensure
  `p@.addr != 0`, from their explicit test and "No block at address 0").
- Before the first allocation, every free or query of a non-NULL
  pointer aborts (`free_before_init`); a thread's first `free` may
  create its heap before the abort (theorem 3).
- `errno` is the F column (*(errno)* in the interface contract), not
  this section's.

- A huge run's size hint is checked by page count, not by `nallocx`
  with the flags: any size that rounds up to the run's pages passes
  (`huge_hint_ok`), which is every size with the run's `nallocx` when
  no class serves it at the flags' alignment, and also the small size
  of a run an alignment made huge (`free_aligned_sized(p, 1 MiB, 64)`
  after `aligned_alloc(1 MiB, 64)`), with or without the alignment
  flag. `huge_hint_ok` is stated with C's token for the run (it names
  the run's pages); the entry points that claim it from the call's
  `CCall` (which the exports mint) state only `hint_ok`.

**Where the statement is weaker than the code.**
- The *(C)* rows: the run-time checks abort where they fire, but the
  proof excludes those inputs by the C contract instead of proving the
  abort. What the code guarantees for them: an unsized free, `realloc`
  or query of a slot whose bitmap bit is clear at the load aborts (a
  sized free defers that to the cache flush, which a `malloc` pop of the
  slot can precede); a freed slot still in a cache (a thread's or an
  explicit tcache; bit set) passes the call and is caught, if at all, by
  a later flush, or at the call with `double_free_scan` (its checks'
  steps are verified, the state bytes' values a premise,
  `heap/slotstate.rs`); an ABA free is undetectable. The queries'
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
  so its `ensures` states `!bad_tcache` only for a move
  (`p@.addr != 0 && r@.addr != 0 && r != p`); a move allocates and frees
  through `alloc_with` and `mallocx::free`, which check it. A kept block,
  and a failed move (NULL), say nothing of the cache id.

## 17. Huge coalescing (at the huge lock invariant)

Free huge extents are maximally coalesced: in every state the huge lock
admits, no free extent ends where another starts. This is a fact of the
lock invariant (`Inner::wf`); nothing is exported per call, and no entry
point's `ensures` mentions it.

**Statement** (`huge/mod.rs`):

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
`Huge::audit` (`huge/mod.rs`, the assert in its loop), stated as a
conjunct of `Inner::wf`, the huge lock's invariant.

**How it is kept.** `wf` has two conjuncts for it: `coalesced`, and
`free_marked`, which puts every page of every free extent in
the free-page bitmap's view.
- `Huge::find` ensures that it returns fresh pages (no
  extent) only when no free extent ends at the mark (`none_ends_at`). Its
  loop holds that every free extent touching the mark starts at or past
  `from`; `free_marked` puts that start in the bitmap, so
  `lowest_set_from` cannot skip it, and when the scan runs out of set
  bits there is none. `take_ok` carries the fact into `take`.
- `Huge::take`: `Take::touch_ok` records where each
  free piece lies: the head ends where the run starts, a tail runs from
  the run's end to the end of the extent it was cut from, and a hole
  runs from the mark, which no free extent touches, to the run.
  `lemma_took_coalesced` does the case analysis; `took_free`
  states the bitmap after the take: what was set outside the run, plus
  `[mark, run start)`.
- `Huge::free_run`: `free_before` and `free_after` also ensure
  completeness: any free extent the table keys
  that ends (starts) at the given page is the result, so the neighbour
  search misses none. `lemma_free_keyed` shows every free
  extent is still under its keys once the run has left the table, so
  `Merge::nbrs_ok` holds: without `pred` no free extent ends
  at the run's start, and without `succ` none starts at its end.
  `lemma_freed_coalesced` concludes for the merged extent.
- `purge_desc` turns an `Empty` extent `Purged`: still free, in the
  same place.
- `Huge::new` starts with no descriptors.

**The hard case.** `take`'s alignment holes: the hole is a fresh `Purged` extent at the old mark, and it could touch
a free extent ending there. It cannot, because `find` returns no extent
only after its scan ran out of free pages, and the scan stops at a free
extent touching the mark, returning it or `None`. Merges of `Empty` and
`Purged` neighbours are coalesced: `free_run` merges both states.

**Premises beyond the common ones.**
- The huge lock protects `Inner::wf`, including `coalesced`,
  `free_marked` and `free_exact` (theorem 29): `huge::Inner`'s
  `LockSpec` impl (`huge/mod.rs`) defines the invariant and the frame
  (an equivalence, `==`), and every holder checks both at its
  `LockGuard::unlock` (`api::tests::guards_end_in_unlock`).
- `class::Set::ensure` (`class/sets.rs`) leaves the bitmap view
  unchanged (verified, as is the commit it makes); `alloc_aligned`
  calls it between `find` and `take`. The view's `set_range`,
  `clear_range` and `lowest_set_from` are verified (`bitmap.rs`).

**Where the statement is weaker than the code.**
- It speaks of descriptors, not pages. The converse of `free_marked`
  (a free bit belongs to a free extent), which `audit` also checks, is
  theorem 29.
- It is stated at the lock invariant only. No entry point exports it.
- That `find` returns the lowest fit is theorem 23
  ("Lowest-address huge placement"), built on this one.

## 18. Pressure never lengthens decay (at a fixed limit)

As cgroup memory usage rises toward a fixed limit, the decay policy never
picks a longer decay period. This is a theorem about two pure functions,
`band` and `effective_decay_ns`, at one fixed limit and one configured
period. It is not about the stored policy the purge reads (`Policy`'s
atomics, whose round trip is not proved, below), nor about the purge
being timely: what the decay walks do with the period is out of scope.

**Statement** (`policy.rs`):

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

`Band::le` (`policy.rs`) is the table's row order `Low` < `Scaled`
< `Immediate`, by position within `Scaled`; `Unlimited` compares only
with itself (for `limit == 0` or `UNLIMITED` both bands are
`Unlimited` and the periods are equal). Band monotonicity alone is
`lemma_band_monotone`, and the period's monotonicity in the
band is `lemma_decay_monotone`, for any two well-formed bands
in that order.

The spec functions are the code's arithmetic exactly, for every input:
`usage_ppm` ensures `ppm == spec_usage_ppm(limit, current)`,
`band` ensures `b == spec_band(limit, current)` and
`effective_decay_ns` ensures `ns ==
spec_effective_decay_ns(base, band)`. `scaled_ppm` models
`usage_ppm`'s loop, which halves usage and limit together until the
limit fits 43 bits: the number of halvings depends on the limit alone,
so it is the same for both usages, and halving keeps their order
(`lemma_scaled_ppm_monotone`); `spec_usage_ppm` covers every limit,
past `2^43` bytes included. `frac16`'s exact contract (`den <= 2^47`)
covers `band`'s constant denominator 200 000, and `mul16`'s is exact.

**Entry points.** None: no C symbol's `ensures` changes. The theorem is
about two pure functions: `band`, which `cgroup_band` applies to the
effective limit against the leaf's usage and to each limited ancestor
within `sys::cgroup::LEVELS` against its own usage, keeping the most
severe (`most_severe`, `Band`'s derived order spelled out over
`severity`), and which `evaluate` (from `purge::on_tick` and the
background thread, and from the deferred cgroup discovery,
`Shared::discover_cgroup`; a no-op until the record is discovered) and
`Shared::init` (`heap/shared.rs`; only for a record discovered at init,
`thp:always`, else the `Unlimited` band)
call through `cgroup_band`; and `effective_decay_ns`, which
`Policy::effective_decay_ns` (`policy.rs`) applies to the stored band for
the decay walks (`purge.rs`), `stats.cgroup.decay_ms`
(`api/mallctl.rs`) and the reports (`api/print.rs`).

**Premises beyond the common ones.** None: the proof uses no axiom,
`external_body` or `assume_specification`; the three functions and
`frac16`, `mul16` verify in place. The common premises are not used
either.

**Scope and where the statement is weaker than the code.**
- One limit per term. `cgroup_band` takes `min(memory.max,
  memory.high)` up the hierarchy against the leaf's usage, and each
  ancestor's own limit against its own usage, as sampled; the theorem
  holds of each term (`band` at one limit), and `most_severe` never
  returns a less severe band than either argument (its `ensures`), but
  that the maximum over the terms is monotone in the usages is not
  stated. When a limit changes between two samples (`docker update`, a
  write to `memory.high`), the two bands are not comparable by this
  theorem, and a higher limit at the same usage does lengthen the
  period, as it should.
- Pure functions only. `Policy` stores the band as atomics
  (`scaled16`, one more than the position in the scaled band so that
  `Scaled(0)` at the 70% edge is told from `Low`, 0 outside it;
  `immediate`, `limited`; `Policy::apply`) and `Policy::band` (`policy.rs`)
  rebuilds it: that round trip is not proved. `Policy::position` reads
  `scaled16` inside its atomic invariant (`FracPred`: at most
  `FRAC_ONE + 1`), which bounds the value but does not say which store
  wrote it; that `apply` stores `Scaled(f)` as `f + 1` and `band` reads
  it back as `Scaled(f)` (the 70% edge's `Scaled(0)` included, which is
  not read back as `Low`) is the code, checked by the test
  `policy::tests::policy_band_round_trips_at_the_scaled_edge`.
  Concurrent `evaluate` calls or a reader between `apply`'s stores (a
  torn `limited`/`scaled16`/`immediate` triple) are not covered, nor is
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
  (`policy.rs`) samples the period half of the theorem at run
  time, including limits past `2^43`.

## 19. Purge never discards live data (huge runs; slabs at the class lock)

For huge runs, against the live-block tokens: no discarded byte is in a
live run. For slabs, only against the class lock's ghost idle state
(`class::decay::idle_byte`): a class purge discards only slabs that
state records as idle, and nothing ties idleness to the slot tokens
(the I5 slab row is `p`).

**Statement.** Three parts: what every discard is handed, and what the
huge and class purges hand it.

```rust
// purge.rs: the one place a batch's ranges reach `madvise`; it returns
// whether `madvise` took the range (`false` on `EINVAL`, `mlock`ed pages).
fn discard(addr: usize, len: usize, advice: Advice, Ghost(free): Ghost<spec_fn(int) -> bool>) -> bool
    requires
        frees(free, addr as int, len as int),

// huge/mod.rs
pub open spec fn free_page_byte(base: int, pg: int, np: int, held: Set<int>, b: int) -> bool {
    &&& base <= b
    &&& (b - base) / pg < np
    &&& !held.contains((b - base) / pg)
}

// heap/live.rs, in `impl Live`
pub proof fn theorem_purge_huge(tracked &self, tracked auth: &GhostSetAuth<int>, b: int)
    requires
        crate::heap::regions_ok(),
        self.wf(),
        auth.id() == crate::huge::page_loc(),
        free_page_byte(process_regions().huge_base as int, page_size(),
            (process_regions().huge_len >> process_regions().lg_page) as int, auth@, b),
    ensures
        !(self.addr() <= b < self.addr() + self.len()),

// class/decay.rs (closed; unfolded)
pub closed spec fn idle_byte(t: SlabTable, states: Map<nat, State>, b: int) -> bool {
    let sz = (1usize << class_table()[t.spec_class() as int].lg_slab) as int;
    let s = (b - t.base) / sz;
    &&& t.base <= b
    &&& s < t.spec_nslabs()
    &&& states.contains_key(s as nat)
    &&& states[s as nat] != State::Active
}
```

(Line breaks added.) A `purge::Batch` (`purge.rs`) carries a ghost
predicate `free` that its maker fixes (`Batch::new`); `push`
takes only ranges of bytes `free` admits (`frees`),
`Batch::wf` keeps that of every range held (`range_ok`),
merged ranges included, and `flush` hands `discard` the ranges
with `free`, and, when `madvise` refuses a merged range, its first, last and middle pieces, each inside the range
(`lemma_piece_ok`). So every byte any flush hands to `madvise`,
`MADV_DONTNEED` or `MADV_FREE`, including the flush inside a `push` to a
full batch and the re-issue of a refused range's pieces, satisfies the
batch's `free`. A range `madvise` refuses is not discarded: its memory
stays `Empty`, which the statement allows (it bounds what may be
discarded, not what must be). The makers report each range to a settler
(`purge::Settle`, the walk's `Tally`), which changes the lock value only
for ranges reported discarded; a settle only moves `Empty` slabs and
extents to `Purged`, so the lock value's ownership, `Active`-ness and
run authority stay those the batch's `free` was made from. The makers:

- **Huge purges** (`Huge::purge_empty`, `purge_curve`, `free_run`,
  `huge/mod.rs`): `free` is
  `free_page_byte(base, page, npages, S, _)`, where `S` is the set of
  the run-page authority the huge lock's value holds when the batch is
  made (`Inner::wf`: the `Active` descriptors' pages, `active_pages`).
  The walks keep the lock value's authority equal to `S` through every
  push and flush (the huge `Tally::inv`: `c.pgs@@ == held`, kept by
  `Huge::settle` and `purge_desc`), and `push_extent` proves an `Empty`
  extent's pages are not in it: they are apart from every `Active`
  extent. `free_run` hands `purge::discard_one` the freed run's pages
  after the run's token left the authority (`GhostSetAuth::delete`), so
  `S` does not hold them. `RunToken::theorem_purge_apart`
  (`huge/mod.rs`): a byte `free_page_byte` admits for the
  authority's set is in no page, guard included, of a run whose token is
  held anywhere (a token's pages are a subset of the authority's set,
  `GhostSubset::agree`). `Live::theorem_purge_huge` lifts that to every
  live block: a run's usable bytes are inside its pages, and a slot lies
  in the class superblock, which `Regions::separated` keeps apart from
  the huge region. The huge allocator's base, page and page count are
  the process regions' under `Shared::wf` (`heap/shared.rs`), so the
  batches' predicate is the theorem's.
- **Class purges** (`ClassState::purge_empty`, `purge_curve`,
  through `Purger` and `push_group`, with `ClassState::settle` as the
  settler): `free` is `idle_byte(t, states, _)`, the bytes
  of slabs of the class table that the arena's class lock value, when
  the walk starts, records as owned and not `Active` (`Fresh`, `Empty`
  or `Purged`). The class `Tally::inv` keeps the lock value agreeing with
  `states` on ownership and `Active`-ness through the walk (a purge only
  moves `Empty` to `Purged`), so at every flush the discarded bytes are
  those of slabs idle in the current lock value. `push_group` pushes a
  page group whose every slab is idle: with one slab per page, the slab
  is in the empty set (`Inner::slab_core`: `Empty`); with more,
  `page_group_is_idle` says so.

**Premises beyond the common ones.**
- Huge: none beyond the huge lock's holders reaching
  `LockGuard::unlock` (`huge::Inner`'s `LockSpec` impl). The run
  tokens' authorities are the process's huge allocator's by the opaque
  definitions of `huge::run_loc` and `huge::page_loc`, which
  `Shared::init` reveals.
- Class: none; the class lock's invariant and frame are
  `class::Inner`'s `LockSpec` impl's, checked at every release.
  `page_group_is_idle`
  (`class/decay.rs`) is verified: every slab of the group is owned
  because an arena owns whole hand-out groups (`class::whole_groups`,
  part of the class lock invariant, kept by `next_fresh_slab`, which
  adds a group `take_group` handed out all at once) and a page group
  lies inside one hand-out group (`SlabTable::inv`: page and hand-out
  groups are powers of two, the page group no larger); each is not
  `Active` by its descriptor's state, read through the permission the
  lock value holds (`slab::DescPerm`). The descriptor's arena compare
  in the body is not needed for the proof.
- Class, the slot-token link (not proved; see below).

**Where the statement is weaker than the code.**
- **Slabs are not tied to slot tokens.** "Idle in the lock value" is the
  class lock's ghost state, not the slot tokens: nothing proves that no
  slot of an idle slab is live, cached or claimed, so the class half
  stops at the lock state, plus the run-time check in
  `ClassState::walk_empty` (`class/decay.rs`, its abort)
  that an empty-set slab's descriptor reads `Empty` with `live() == 0`.
  The link the proof lacks is the descriptor's `live` counter, which is
  not modelled. **Unconfirmed** (read from the code and its comments, not
  checked by a proof): `live` counts set bits plus in-flight
  reservations, a claimer reserves under the class lock
  (`ClassState::take`) before it sets bits, and a releaser clears bits
  before it subtracts without the lock
  (`ClassState::dec_live`, `class/mod.rs`; `SlabDesc::sub_live`,
  `slab.rs`, verified, which ensures only that the count it read
  was at least `n`), so `live == 0` read under the lock
  means every bit of the slab is clear and every slot token is in its
  bitmap word's invariant. A proof of that needs `live` as a
  ghost-tracked atomic, whose value counts reservation credits (minted
  by `take`, spent by the claim's `fetch_or`) and release credits (taken
  by `fetch_and` with each cleared bit, spent by `sub_live`), plus a
  lock-held "frozen" flag for idle slabs; the code has no such ghost
  state.
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
- Theorem 6's purge premise rests on trust. `axiom_discarded_zero`
  takes any range whose `MADV_DONTNEED` the kernel answered with
  success (`sys::madvise_ok`, from `Huge::rediscard`); that nothing has
  written it since is a premise. `class::axiom_discarded_slots_zero`
  takes any `Purged` slab whose claim holds a `purge::PurgedZero`
  witness. That
  witness comes from `purge::purged_zero` (`purge.rs`, verified: `Some`
  exactly when `purge::purged_is_zero`, a plain `Relaxed` load of
  `purge::LAZY_EVER`, returns true); that the slab
  was discarded with `MADV_DONTNEED` in the hold that set `Purged` and
  is untouched since is a premise. `Batch::flush` mints no `Zeroed`
  token, and no lock value holds one per `Purged` slab or extent
  (wolfi-dev/challoc#397, fresh-slab zero facts).
- `MADV_FREE` and `MADV_DONTNEED` are treated alike: the theorem is
  about which bytes are discarded, not what they read afterwards.
- `SlabDesc::sub_live` (`slab.rs`) is verified; it requires
  `may_abort_pending()` and ensures only that the count it read was at
  least `n`.

## 20. `MALLOCX_TCACHE_NONE` bypasses thread caches

An allocation or free made with `MALLOCX_TCACHE_NONE` leaves every
thread cache it can reach caching exactly what it cached before.

**Model.** A heap's caches are its ghost cache permissions
(`Heap::perms`, a `CachePerms`): the headers it owns, their stacks and
entries, and the slot token under every cached entry
(`CachePerms::tokened`).
`perms` is a private field of `Heap`, so only `heap` code can change a
cache, and only through a `&mut Heap`. A verified function obtains a
heap only from `thread::heap_from_slot` (verified, borrowing it from the slot token, `TlsSlot::entry`; through
`heap_for_thread_in` and `existing_heap_in`), `tcaches::heap_at`
(trusted, `external_body`; through `Tcaches::get`, `heap_destructor`
and `Heap::destroy`) or `tcaches::heap_new`
(heap creation, verified over the trusted `HeapPerm::assemble`;
`mallctl::thread_heap` goes through `heap_for_thread`),
and each reference is assumed `Heap::wf` afresh: no ghost state links two
references to the same heap. The theorem is therefore stated per
reference: every heap the direct paths obtain, they leave caching
exactly what it cached.

**Statement** (`heap/mod.rs`; `heap/paths.rs`):

```rust
// The heap caches exactly what `old` cached.
pub closed spec fn caches_same(&self, old: Heap) -> bool {
    &&& self.perms@ == old.perms@
    &&& self.caches == old.caches
    &&& self.others == old.others
    &&& self.shared == old.shared
    &&& self.list@ == old.list@
    &&& self.keys@ == old.keys@
    &&& self.home_hdrs@ == old.home_hdrs@
    &&& self.heads == old.heads
    &&& self.arena == old.arena
    &&& self.bound == old.bound
    &&& self.rehome == old.rehome
    &&& self.homed == old.homed
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
- **allocation**: `mallocx::alloc` (`heap/mallocx.rs`) on
  `Tcache::None` calls `existing_heap` (`heap/thread.rs`) at most
  twice: once for the home arena when the flags name none
  (`Heap::arena`, `&self`), and once after a successful claim for
  `count_direct_alloc`, whose `ensures` gives `caches_same` of that
  reference. The slot comes from
  `alloc_direct` (`heap/mallocx.rs`) and `Shared::alloc_direct`
  (`heap/shared.rs`), which take no heap: `ClassState::claim_tokens`
  takes it out of the class's bitmap words, with its token, and the
  token goes into the caller's `Live` (`Live::from_slot`).
- **deallocation**: `mallocx::free` (`heap/mallocx.rs`) on
  `Tcache::None` calls `heap::free_direct` (`heap/paths.rs`):
  `Shared::classify_free` (`heap/shared.rs`), `zero_slot` under
  `zero_on_free`, then `Shared::free_direct`, which hands the
  slot's token (from the caller's `Live`, `take_slot`) to
  `ClassState::release_with` and so back to the bitmap word; then
  `count_direct_free` on the `existing_heap` result, with the same
  `ensures`. A huge pointer takes `Shared::free_huge` and
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
with the same tokens: no cache gains or loses a slot. The proof adds
only ghost code.

The test `api::jemalloc::tests::tcache_none_leaves_every_cache_alone`
(`api/jemalloc.rs`) checks the
same at run time against the real allocator: with entries
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

A mallctl name converts to a management information base (MIB) and back
to the same node: the name splits into components one way only, each MIB
entry is the one its component names, and `mallctlbymib` with that MIB
reaches the node `mallctl` reaches with the name. All items cited here
are in `api/mallctl.rs`.

**Model.** The name tree is the static `ROOT` and the `Node`s it
reaches. `root_nodes()` (uninterpreted) is its top level, and
`root` returns it; `spec_children` is what
`Node::children` returns: a directory's children, else none.
One shared recursive spec, `resolve`, says what node a MIB
names: each entry picks a child with `step`, the lone numeric
child of its parent for any entry, else the child at that index, and
the next entry looks among the picked child's children. `walk`
is the same descent driven by a name's components: `names`
matches a component under a lone numeric child when it is a nonempty
string of digits whose `decimal` value is the entry, else when the
child at the entry's index is named by it. `split` says
`comps` are the dotted components of a name: at least one, none holding
a dot, joined with dots back into the name. `level` is the
sibling list the `j`-th entry picks from, and `entry_named`
says which entry a component names there.

**Statement**:

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
component. `mallctl` passes `name_to_mib`'s node with
`&mib[..len]`, the MIB it built; `mallctlbymib` passes
`mib_to_node`'s node with the MIB it resolved, whatever the caller
supplied (read by the verified `mib_in`). `ctl` requires that
pairing (`resolve(root_nodes(), mib@) == Some(*node)`), so
Verus checks at both call sites that a key is performed on a node its
MIB names, with that MIB's entries unchanged. That the fixed
positions `ctl` and the
readers read with `at` (1 for the `<i>` of `arena.<i>`, 2 for the `<j>`
of `arenas.bin.<j>` and the `<i>` of `stats.arenas.<i>`, 4 for the
`<j>` of `stats.arenas.<i>.bins.<j>`) are those numeric levels is
checked by inspection, not stated.

**Proof.** The traversals' loops keep the descent as invariants:
`name_to_mib` keeps the ghost components matched so far, the node the
last one named (`walk` of the prefix), the current sibling list as that
node's children, and `name == join(comps) + "." + rest`;
`lemma_walk_push` and `lemma_join_push` extend them by
one component. `mib_to_node` keeps `resolve` of the prefix `mib[..k]`;
`lemma_resolve_split` continues a resolution among the last
node's children, which also shows that a bad entry leaves the whole MIB
unresolved. `child` and `child_by_index` ensure
`names` and `step` exactly. `lemma_walk_resolves` turns a walk into a
resolution, `lemma_walk_entries` reads off each entry, and
`lemma_split_unique` shows the components are unique: the
first ends at the first dot.

**Premises beyond the common ones.**
- `root` (`external_body`; discharged by Kani,
  `api::mallctl::kani_root::root_returns`): every call
  returns the same nodes, `root_nodes()`. It is `&ROOT`, a static, so
  this holds; nothing about the tree's content is assumed (the theorem
  holds for any tree).
- The caller's name is read by `cstr` through `name_byte`/`name_bytes`,
  verified over the C contract of a string argument (`vspec::cbuf`;
  the call's token `CCall`, minted by the export wrapper, and its
  `str_byte`; `name_bytes` states only the length); the theorem is about
  the bytes they return.

**Scope.**
- What a `Read` node's reader returns (`call`, a verified `match` over
  the `Reader` enum, with no `ensures`) and what an `Op` arm does are
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
  `every_name_round_trips_through_its_mib` checks every name
  in the tree (numeric components spelled `0`);
  `names_resolve_to_positional_mibs_and_back` checks the
  expected MIBs of selected names.
- `mallctlnametomib`, `mallctl` and `mallctlbymib` have no
  `ensures` tying their return codes to the round trip (interface row
  34; `mallctlnametomib` states only that an error writes nothing); the
  round trip is stated on the cores and required by `ctl`, not stated
  on the C entry points.

**Compatibility difference (documented, not changed).**
`mallctlnametomib(name, mibp, miblenp)` resolves the whole
name first and then compares its length with the caller's capacity
`*miblenp` (`mib_cap`): when the capacity is smaller it returns
`EINVAL` and writes neither `mibp` nor `*miblenp` (`mib_out`, is
not called). jemalloc 5.3.0's `ctl_lookup` (`src/ctl.c`) loops
`for (i = 0; i < *depthp; i++)` instead: when the capacity runs out
before the name does, it returns 0 with the first `*miblenp` entries
written, leaves `*miblenp` unchanged, and ignores the rest of the name
without checking it. With a capacity of 2, `stats.arenas.0.pactive`
gives `[stats, arenas]` and 0 in jemalloc, `EINVAL` here; a caller that
passes room for the full MIB gets the same MIB from both. Not in the
theorem: the capacity read is trusted and the return code has no
`ensures`.

## 22. Exact stats (ledger model, at quiescence)

When no allocation or free is in flight, the reported allocated bytes
equal the usable bytes of the live blocks plus the class size of every
slot held in a thread cache.

**Statement.** At a quiescent state, `stats.allocated` (read live by
`collect`, `collect_all`, `mallinfo2` and `malloc_stats`; at the last
`epoch` by `mallctl`) is the usable bytes of the live blocks plus the
class size of every slot in a thread cache. Three layers:

1. **The figures are true sums of what was read** (code, `stats.rs`).
   `collect_arena_with` hands out, through a ghost `pg`, the
   pages its load of the arena's `large` word returned, and ensures
   `arena_exact(r, pg)`: `small_allocated + large_allocated` is
   `Σ_c curregs[c] * size(c)` plus `(pg - large_runs * GUARD_PAGES) *
   page` whenever that true sum fits a `usize`. `collect_into`
   and `collect_all` hand out, through a ghost `rd`, the figures
   of each arena they found (in index order, each keyed by its registry
   ID, `ArenaStats::index`, with the IDs ascending: `ids_ascending`, see
   theorem 36) and those pages, and ensure
   `totals_exact` and `summed`: `totals.allocated`,
   and `allocated`, every bin's `curregs` and `large_runs` of the sum, are
   the true sums of those reads (`reads_allocated`) when they
   fit. `mallinfo2` (`api/glibc.rs`) hands out what `collect_all` read
   and summed, and ensures `uordblks` is the sum's `small_allocated`,
   `hblkhd` its `large_allocated` (so `uordblks + hblkhd` is the sum's
   `allocated`) and `hblks` its `large_runs`, and states how `arena` and
   `fordblks` are computed (the interface contract's row 31). Inside each function the ghost outputs are
   the values its loads returned (the body sets them from the loaded
   locals); to a caller they are only values for which the ensures hold
   (see "Scope").
   The figures are added with `fig_add`/`fig_mul`, whose spec is `+`/`*`
   when the result fits; `ArenaStats::add` (`stats.rs`) and
   `Totals::add_arena` (`stats.rs`) state each needed sum alone, so an
   unrelated figure's overflow does not hide it. `collect` and `epoch`
   call `collect_into`; `collect_arena` and `malloc_stats` add no
   contract of their own.

2. **The counters track the blocks** (ghost model, `stats/ledger.rs`).
   `Books` has one step per counter update and per slot move:
   `reserve` (`ClassState::take`, `class/mod.rs`: `claimed += n`
   under the class lock), `claim` (one bit won by `finish`, or by
   `claim_prefix` in the same lock hold as the reservation; the slot's
   token leaves its word, so no arena has it out), `move_slot` between
   flight, a cache and a live block (refill push, hand-out, free push,
   flush or direct-free take), `release` (`release_word`: the token back
   in its word, through the arena whose slab it is), `count`
   (`release_owned`'s `released.fetch_add(end)`, `class/mod.rs`),
   `take_run` (`Huge::take`: the run token's pages come from the page
   authority), `note` (`Shared::alloc_huge`'s `note_huge_alloc(r.pages)`),
   `free_run` (`Huge::free_run`) and `unnote` (`Shared::free_huge`'s
   `note_huge_free(run.pages)`), plus `new_arena` (`Arenas::create`) and
   `materialise` (`Arenas::build` of an automatic index); every counter
   step names a built arena, and an unbuilt index keeps zero counters
   (`unbuilt_fresh`; these and `start(auto)` are in theorem 36).
   `Books::inv` (`stats/ledger.rs`) holds at `Books::start`
   (`lemma_start`) and after every step (`lemma_step_*`), in any
   interleaving:
   `claimed - released` of each `(arena, class)` is its out slots plus
   the reservations not yet claimed plus the releases not yet counted;
   the out slots of a class are each in exactly one of flight, a cache
   and a live block, and out through one arena; each `large` word is its
   arena's live runs and their pages, less those not yet added, plus
   those retired and not yet subtracted; live runs share no page.
   `note` and `unnote` take any page count within the pending total, not
   the run's own: only the totals matter, and quiescence zeroes them.
   `Books::theorem_quiescent` (`stats/ledger.rs`): when no update is in progress
   (`quiescent`), `Σ_arenas (claimed - released)` of a class is
   its cached plus live slots, each arena's `claimed - released` is its
   out slots, each arena's word is its live runs and pages, and the words
   summed are all live runs and pages.

3. **Over the live tokens** (`stats/ledger.rs`):

```rust
pub proof fn theorem_exact_stats(b: Books, tracked live: Map<int, Live>, n: int,
    reads: Seq<ArenaStats>, pages: Seq<int>)
    requires
        b.inv(), b.quiescent(),
        // the scan read the ledger's counters of the arenas it names by
        // registry ID, every built one, in ascending ID order
        reads_keyed(b, reads, pages),
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
tokens count once each: `heap::live::lemma_all_apart` (`heap/live.rs`)
shows two tokens held at once stand for different slots, or for runs
whose pages are apart (`lemma_keys_apart`, from the slot tokens'
and run tokens' authorities), so the per-class slot sets and the run
extent and page sets have exactly the tokens' counts (`lemma_key_bytes`).
With layer 1, the `rd` a `collect_all` or `collect_into` call hands out
is, by its body, the theorem's `reads` and `pages`; so whenever the true
sum fits, the reported `allocated` is the live blocks' usable bytes plus
the cached slots'. The same holds of `mallinfo2`'s `rd` and its
`uordblks + hblkhd`.

Per arena, `theorem_quiescent` gives `claimed - released == |out(a, c)|`
and the word equal to the arena's live runs, in the model; the theorem
over tokens is stated for the sum over arenas only.

**Huge counters, at the code level** (`huge/mod.rs`,
`theorem_huge_stats`, private to `huge`): in every value the huge lock
admits, `stats.runs` is the number of extents the run authority holds and
`stats.run_pages` the number of pages the page authority holds: one
extent per live `RunToken`, and its pages (`theorem_run_counted`: a token's extent and pages are among them). These are the
`huge:` counters of `malloc_stats_print`; no quiescence premise is
needed, since they change under the huge lock with the authorities.
`free_run` ensures `r.pages == tok.npages()`, and
`Shared::alloc_huge` and `free_huge` assert that the pages they add to
and subtract from the `large` word are the token's
(`heap/shared.rs`).

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
  `hblks` and 0 to `uordblks`). The `stats` module doc's table states the same.
- `malloc_stats`: per arena `in use bytes` is `small.allocated`; the total
  is `allocated`; `max mmap bytes` is `large.allocated`.

**Premises beyond the common ones.**
- Quiescence: no allocation, free, flush or `epoch` runs during the scan
  or between the last counter update and it; then each `Relaxed` load in
  `bin_stats` and `Arena::large` reads the last store, and the scan's
  reads are the ledger's counters. Verus's atomics return any value
  (common premises), so this link is a premise, stated as
  `theorem_exact_stats`'s `reads` requirements.
- Arena IDs (a stable registry): the ledger's `k` is the `arenas.count()`
  the scan loaded, and the scan's `get_existing` finds every built
  arena, so its reads name every built arena (`reads_keyed`). Unbuilt
  indices may be skipped: `reads[j]` is the arena whose registry ID is
  `reads[j].index`, not arena `j`, so an unbuilt automatic arena needs
  no premise (theorem 36).
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
  figures `collect_all`, `collect_into` and `mallinfo2` return to
  `theorem_exact_stats`'s `reads` rests on reading the body (each output
  is the local the code loaded), not on the contract: the result does
  not carry the page count of the `large` word (`ArenaStats` holds runs,
  not pages).
- The ledger is a model: nothing threads a `Books` through the code, and
  `theorem_exact_stats` relates its sets to the tokens by premise. The
  class counters' updates are outside the bitmap words' invariants
  (`bitmap.rs`), so no code-level ledger ties a count to the bits.
- Overflow: the figures are exact only when the true sums fit a `usize`
  (always, for the address space they describe, but not proved).
- Per-arena exactness of the `large` word needs the descriptor's arena to
  be unchanged while the run is live (not proved; the run token carries
  no arena).

## 23. Lowest-address huge placement

A huge run goes at the lowest-address page where it fits: `Huge::find`
returns the lowest aligned start whose pages are all free, or `None` when
none exists. This is `Huge::find`'s contract, under the huge lock
invariant: no entry point exports it ("Where the statement is weaker"
below). Built on theorem 17.

**Statement** (`huge/mod.rs`):

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
        // ... (`may_abort()`, page-aligned base, ghost page count and shift)
    ensures
        // ... (the extent it comes from, as before)
        r matches Some((s, _)) ==> self.fits(g, s as int, need as int, align),
        r matches Some((s, _)) ==> forall|q: int| 0 <= q < s ==> !self.fits(g, q, need as int, align),
        r is None ==> forall|q: int| !self.fits(g, q, need as int, align),
```

(Line breaks and triggers dropped.) `in_free(descs, x, p)`:
`x` is a free (`Empty` or `Purged`) descriptor with page `p`.
`aligned_at(q, align)`: `align` is at most a page, or page
`q`'s address is a multiple of `align`. `need` counts the guard page.

In every value of the huge lock, the start `find` returns for `need`
pages at `align` is the lowest page with the run's pages aligned,
inside the region, and each free: in a free extent or at or past the
high-water mark. `None` means no page fits at all. "Free" is page by
page: a fit may span pages of several free extents and the untouched
suffix; maximal coalescing (`Inner::wf`, "Huge coalescing") is what
reduces that to the one extent `find` looks at.

**How it is proved.**
- `align_page` also ensures that no page in
  `[page, r)` is aligned: those pages start in `[a, align_up(a))`,
  below the first multiple of `align` (`lemma_below_align_up`).
- `find`'s loop holds two more facts at `from`: every free extent
  starting below `from` ends by it (`ends_by`), and no fit
  starts below `from`. Each round, `lemma_below` extends "no
  fit" to below `s`: no page in `[from, a)` is free
  (`Inner::lemma_none_free`: an extent holding one would start
  below `from` and so end by it, or start at a free bit `lowest_set_from`
  would have returned), and none in `[a, s)` is aligned.
- The pass to the next extent: a fit at `q` in `[s, e_end)` starts in
  `e` (extents are apart), so by `Inner::lemma_fit_extent` it
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

**The early return.** The early return when the extent `e` reaches the
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
- The huge lock protects `Inner::wf`, with `coalesced`, `free_marked`
  and (for `extent_at`'s abort) `free_exact`: `huge::Inner`'s `LockSpec`
  impl defines it, and every holder checks it at its `LockGuard::unlock`.
- `TwoLevelBitmap::lowest_set_from` (`bitmap.rs`) is verified: the
  lowest set bit at or past `from`, or none.

**Where the statement is weaker than the code.**
- It is `find`'s contract under the lock; no entry point exports it.
  `alloc_aligned` (`api/libc.rs`) returns NULL before `find` when the size
  overflows or the run or the alignment exceeds the region, and after
  it when a descriptor, the table or bitmap growth, or the kernel
  commit fails, so "huge `malloc` returns NULL only when nothing fits"
  is not claimed.
- A free bit that starts no free extent would abort in `extent_at`;
  with the converse of `free_marked` in `wf` ("Huge free bitmap is
  exact"), that abort is proved unreachable: under the lock invariant
  `find` never reaches it.
- Scope, not a gap: the order is by address only. `Purged` (zero)
  and `Empty` (dirty) pages count alike, as `find` treats them.

## 24. No block at address 0

**Statement.** No slot, no huge run and no block an allocation may
return is at address 0. So a null pointer is never a block: an
allocating entry point's non-null result is its block and NULL is its
failure; a query or free of NULL that is not a documented no-op aborts;
a usable size is never 0 for a block, so 0 marks NULL; two live blocks
are at two addresses; and a throwing `operator new` never returns NULL.

```rust
// layout/regions.rs: `separated` carries both bases
&&& forall|c: int| 0 <= c < NCLASSES ==> { ... &&& 0 < self.class_base[c] ... }
&&& 0 < self.huge_base

// layout/regions.rs
pub proof fn lemma_block_nonzero(self, c: int, p: usize, len: int, size: int)
    requires self.wf(), self.separated(),
    ensures
        self.is_slot(c, p) ==> p != 0,
        self.is_huge_block(p, len) ==> p != 0,
        self.user_block(p, size) ==> p != 0,

// heap/mod.rs
pub proof fn theorem_no_block_at_zero(p: *mut u8, c: int, size: int)
    requires regions_ok(),
    ensures
        process_regions().is_slot(c, p@.addr) ==> p@.addr != 0,
        process_regions().is_huge_block(p@.addr, 1) ==> p@.addr != 0,
        process_regions().user_block(p@.addr, size) ==> p@.addr != 0,
        free_target(p) ==> p@.addr != 0,

// heap/live.rs
pub proof fn lemma_nonnull(&self)                                  // Live
    requires regions_ok(), self.wf(),
    ensures self.addr() != 0, 0 < self.len(),
pub proof fn theorem_distinct(tracked &mut self, tracked other: &Live)
    requires regions_ok(), old(self).wf(), other.wf(),
    ensures final(self).addr() != 0, final(self).addr() != other.addr(), /* and theorem_apart's frame */

// vspec/usable.rs
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

// api/libc.rs (usable_size), api/jemalloc.rs (sallocx, xallocx)
fn usable_size(shared, p, blk)        ensures p@.addr != 0, 0 < r, /* and its U ensures */
fn sallocx(p, flags), xallocx(p, ..)  ensures p@.addr != 0, 0 < r,
// api/libc.rs
fn malloc_usable_size(p), free_usable(p), free_usable_block(p, blk)
                                      ensures p@.addr != 0 ==> 0 < r,
fn free_usable_slow(p, blk)           ensures 0 < r,

// api/cxx.rs (new, oom, new_slow, new_block,
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
pops (`malloc_block`, `malloc_fast`, `malloc_usable_stored`, `api/libc.rs`)
prove their non-null results with it.

**Proof.** `from_parts` (`layout/regions.rs`) proves `0 <
class_base[c]` from `0 < superblock` (`class_base[c]` is at least
`superblock + c * stride`), and requires `0 < huge.0`; `reserve`
proves `0 < superblock` and `0 < huge.0` from the reserved
mappings' `addr > 0` (`try_reserve`'s `0 < a`), as
`align_base` and `slab_multiple` only add to the mapping's address. A
slot start lies at or above its class base (`lemma_slot_in_region`) and
a huge page start at or above the huge base. A slot's usable size is
its class's size, at least the quantum (`lemma_class_table_wf`); a huge
run's is at least one page (`run_bytes`, `huge_usable`). Two live
tokens' usable bytes are disjoint and each holds at least one, so their
addresses differ. `operator new`: `oom` returns a non-null block or,
with `nothrow`, NULL; without `nothrow` its exhausted loop is
`throw_bad_alloc` (`-> !`), and `malloc_fast`'s `Some(p)` is non-null
(a slot).

**Premises beyond the common ones.** None: the kernel's `mmap`
contract (`try_reserve`, already a common premise); the class sizes are
verified (`lemma_class_table_wf`).

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
- `sallocx`, `xallocx`: NULL aborts (proved).
- Throwing `operator new` (unaligned and `align_val_t`): never NULL.
  With a non-power-of-two `align_val_t` (0 included) and no `nothrow`, the two
  `ensures` (`r@.addr == 0` and `r@.addr != 0`) together say the call
  does not return (the code throws `bad_alloc`, or aborts without
  libstdc++, before any handler call).

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
  cannot have acted on a block there. `free` has no null test on its
  fast branch: null never passes the bit test (what `set_slot` returns,
  through the class line, is a slot start, never at 0) and `free_slow`
  returns for it, so the no-op
  is the slow path's (`free_slow`'s `p@.addr != 0 ==>` ensures). The
  hinted sized free on x86_64 has no null test either
  (`free_sized_block`'s is `cfg(not(target_arch = "x86_64"))`): null
  never passes `sized_slot` (what it returns is a slot start of the
  hinted class, `ClassLine::lemma_sized_ok`, never at 0) and
  `free_sized_slow` returns for it on x86_64, whose `ensures` are
  `p@.addr != 0 ==>` as `free_slow`'s are.

## 25. Realloc follows the class/page keep rule

`realloc` and `rallocx` keep a block in place exactly when the new size
and alignment pick the block's own class, or, for a huge run, round up
to the run's own page count; otherwise they move it.

**Statement** (`api/libc.rs`; `api/jemalloc.rs`):

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

// Composed as C calls it (verification only, cfg(verus_keep_ghost)):
// n = malloc_usable_size(p) (usable_size with p's token), then realloc(p, n).
fn theorem_realloc_to_usable_size(p, blk: Live, errno) -> (r: (usize, *mut u8))
    requires may_abort(), may_syscall(), blk.claims(p), p@.addr != 0,
    ensures
        free_target(p),
        usable_of(p, r.0 as int),
        r.1@.addr == p@.addr <==> slot_start(p) || r.0 > MEDIUM_MAX,
```

(Abridged: each function has further `ensures`.) In
words: `realloc(p, size)` with `size != 0` returns `p` exactly when `size`
maps to `p`'s class (`class_of(size)` is `p`'s class: `size` lies in
`(size of the class below, size of p's class]`), or when `p` is a huge run
start, `size > MEDIUM_MAX` and `size` rounded up to whole pages is the
run's usable size; otherwise it moves (or fails, returning null).
`kept` (`api/libc.rs`) is the forward half: a kept block fits.
`rallocx` adds the alignment: a slot of the class `class_for(size, align)`
picks, or a run start that is a multiple of `align`. The move case is
`r != p` because the new block's token and `p`'s are apart
(`Live::theorem_apart`) and both have usable bytes (`Live::lemma_len_pos`,
`heap/live.rs`).

**Corollary.** `realloc(p, malloc_usable_size(p))` keeps every
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
returns a different pointer. The test
`api::libc::tests::realloc_to_usable_size_keeps` checks the kept cases
for every class and three run sizes.

**Proof.** `lemma_keeps_slot` (`api/libc.rs`) and `lemma_keeps_run`
reduce `keeps` to its slot or run half (one class per slot,
`vspec::zero::lemma_slot_unique`; no slot is a huge page start,
`lemma_slot_not_huge`). The slot half is the test the code makes:
`class_for`'s `spec_class_for` postcondition, and for `realloc_old`'s
`class_of` `lemma_class_of_for` (the class at or above `size`
with every class below smaller is the least serving class at alignment
1, and none serves above `MEDIUM_MAX`). The run half is the code's
`usable_for(size) == Some(old)`: `lemma_usable_for_run` shows
`usable_for`'s value is `huge_usable(size)` whenever it is defined, and
that when `size` rounded up overflows no run's usable size equals it (a
run and its guard page lie in the huge region, so `old <= usize::MAX -
page`). `old` is `blk.len()` by `Huge::usable_size`'s token
postcondition. The corollary: a slot's usable size is its class's size,
which `lemma_class_size_mono` makes the least serving class; a run's is
whole pages (`run_bytes`), which round up to themselves, and maps to no
class exactly above `MEDIUM_MAX` (`lemma_class_for_one`, finds
a serving class below the last, whose size is `MEDIUM_MAX`).

**Premises beyond the common ones.**
- The C contract `heap::live::axiom_c_claim` (`heap/live.rs`): the
  token `realloc` takes for `p` is the live block's, so `blk.len()` is the
  usable size `malloc_usable_size` reads.
- Nothing else: `class_of`, `class_for`, `Huge::usable_for`,
  `Huge::usable_size` and that the class table holds `spec_class_size`
  (`vspec::class::lemma_class_table_wf`) are verified.

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

## 26. Cache routing

Every cache a heap pops from belongs to the arena the allocation asked
for: cache keys are distinct, a header routes to at most one arena, and
no pop takes from the flush-only `FOREIGN` node.

**Model.** A heap's caches are its home headers (`Heap::home_hdrs`, one
per class) and the headers of the nodes on its `others` list
(`Heap::list`, ghost, head first). The ghost `Heap::keys`
(`heap/mod.rs`) gives each node its key, `keys[i]` the `arena`
field of `list[i]`: the arena whose cache the node is (an explicit
`MALLOCX_ARENA` arena, or a home arena `thread.arena` moved away from),
or `FOREIGN` (`u16::MAX`, `heap/mod.rs`) for the one flush-only node
that plain frees of other arenas' slots go onto (design 6.3 step 4).
`Heap::list_ok` (`heap/mod.rs`, part of `Heap::wf` and
`Heap::keeps`) states that the keys are pairwise distinct and none
is the home arena; `Heap::keeps` adds that the keys keep their places
counted from the end, as the nodes do, and that the home arena is
unchanged. A pop is a call of `Heap::pop_top` (`heap/cache.rs`), the
one function that takes an entry off a cache stack to hand it out; its
callers are `Heap::pop_fast`, `Heap::pop`, `Heap::pop_compact` and
`Heap::pop_compact_run`. An explicit tcache (`MALLOCX_TCACHE`) is a
heap of its own (`Tcaches::get`), popped through the same functions.

**Statement** (`heap/mod.rs`):

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

// Heap::pop (heap/cache.rs), Heap::pop_compact (heap/compact.rs),
// Heap::pop_compact_run:
    requires old(self).poppable(class as int, cache),

// Heap::other_caches (heap/others.rs):
    ensures
        !create ==> *final(self) == *old(self),
        r@.addr != 0 ==> final(self).keyed(r, arena),
        r@.addr == 0 ==> !final(self).has_key(arena),
// Heap::cache_slot (heap/others.rs), Heap::ensure_cache (heap/cache.rs):
    ensures r@.addr != 0 ==> final(self).routes(arena, class as int, r),

// Heap::alloc_in (heap/paths.rs), Heap::alloc_other (heap/others.rs):
    requires arena != FOREIGN,
// Heap::alloc_slow_in (heap/paths.rs):
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
- `Heap::pop_fast` (`heap/paths.rs`, the `malloc`/`calloc` fast
  path): `cache_ptr(class)`, the home header (`cache_ptr`'s `ensures`,
  `heap/cache.rs`). This one is read off its body: `pop_top` takes
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
- `Heap::set_arena` (`heap/mod.rs`): `other_caches(arena, true)`
  returns the node keyed `arena`, the only one; after the header
  exchange (`swap_hdrs`, which keeps `same_shape`, with the keys and
  the home arena) the node's key becomes the old home, which no node
  had, and the home becomes `arena`, which no other node has. When no
  node could be had, `other_caches` ensures no node is keyed `arena`,
  and `drop_home_caches` ensures `same_shape`.
- `Heap::try_rehome` (`heap/mod.rs`:
  a young thread heap moves its home onto the arena of a slot it frees
  on the `FOREIGN` slow path): it moves only when `other_caches(to,
  false)` returned null, so no node is keyed `to` (`other_caches`
  also ensures `!create ==> *final(self) == *old(self)`: a lookup
  changes nothing). The flushes that follow keep the keys
  (`flush_arena` ensures `same_shape`), the home headers are re-keyed
  through their permissions (`hdr_mut`, `lemma_set`, `same_shape`), and
  the home becomes `to`, which no node has; the old home was no node's
  key either. The move is the one change of the home arena besides
  `set_arena`, so `try_rehome` and its callers (`rehome_on_free`,
  `free_slow_in` with `FOREIGN`, `free_other`, `Heap::free`) ensure
  `Heap::wf` rather than `Heap::keeps`; `free_slow_in` also ensures
  `keeps` for the home arena (`free_slow`, the home push's slow path).
- Creation starts with no keys (`thread::create_heap_for_thread`,
  `Heap::lemma_fresh_wf`).
- `Flags::decode` (`heap/mallocx.rs`) ensures an explicit arena
  is below `0xfff`, so `mallocx::alloc` (`requires arena matches
  Some(a) ==> a != FOREIGN`) and `api::jemalloc::alloc_with`
  (`api/jemalloc.rs`) discharge `alloc_in`'s requires.

**`FOREIGN` is released whole.** In `Heap::free_slow_in`
(`heap/paths.rs`) a full `FOREIGN` cache is flushed by its capacity,
after which `cached_len(cache) == 0` (a proof assertion, from `flush`'s
`ensures`, `len` decreasing by `n`), before the push. `Heap::gc_arena`
(`heap/mod.rs`) ensures it: each header of a `FOREIGN` array is
left as `gc_left(old, p, true)`, which is `cached_len(p) == 0`.

**Explicit non-claim: arena purity.** Routing does not give "every slot
`malloc` returns belongs to the heap's home arena" (nor the explicit
arena's, for `MALLOCX_ARENA`). A sized free (`free_sized`, `sdallocx`,
`free_aligned_sized`: `heap::free_sized_fast`, `heap/paths.rs`)
pushes onto the home cache without loading the owner word (design 6.3,
the sized path, step 2), so the home cache can hold another arena's
slots, and its pops hand them out. The plain `free` routes another
arena's slot to the `FOREIGN` node (theorem "Arena isolation" covers the
bitmap side: every release goes back to the slot's own arena).

**Premises beyond the common ones.**
- None for the keys: `Heap::node_arena` reads the node's `arena` through
  the permission the heap holds (`Heap::heads`, `others::heads_ok`),
  whose value is proved to be the node's ghost key; its writers,
  `node_link` in `other_caches` and `node_set_arena` in `set_arena`,
  are verified, and `destroy` frees the nodes with the heap.
- Every heap a verified path is handed is `Heap::wf` (common premises),
  including the keys.

**Where the statement is weaker than the code.**
- The arena a pop serves is linked to the requested one at the pop
  sites (`routes` from `other_caches`/`ensure_cache`, and the home
  header), not by a requires of `pop`: `pop` takes no arena, and a ghost
  argument would change its signature. `poppable` itself admits any
  arena's cache except `FOREIGN`'s.
- `pop_fast` is covered by reading its body (one `cache_ptr` call), not
  by a requires.
- The release of a full `FOREIGN` cache on the free path is a proof
  assertion in `free_slow_in`, not an `ensures` (`gc_arena`'s is one).
- Per heap reference: as in "`MALLOCX_TCACHE_NONE` bypasses thread
  caches", Verus does not relate two references to the same heap, and
  an explicit cache's heap is assumed `wf` afresh at each
  `Tcaches::get`.
- The home arena of a heap is not constrained, so `poppable` admits the
  home header whatever the home arena is. It is never `FOREIGN` in
  practice, but by run-time checks and inspection, not by Verus: a
  thread heap's comes from `assign_home` (below `MAX_ARENAS`, which
  `Heap::homed_ok` states for every counted heap);
  `set_arena` requires the new one below `MAX_ARENAS`, which its only
  caller, `mallctl`'s `thread.arena`, checks at run time;
  `try_rehome` moves it only to an automatic arena (`to < auto`, checked
  at run time, so below `MAX_ARENAS`, and `homed_ok` holds after); and
  `Tcaches::create` takes
  any `arena` (its callers pass the thread's home arena or 0).
- Routing names the header a pop takes from, not the arena a refill
  inside the pop draws from: `pop_compact` and `pop_compact_run` refill
  an emptied cache through `shared.arena(self.hdr_ref(cache).arena)`,
  the header's own `arena` field, which `ensure_cache` and
  `CacheHeader::rekey` set and
  `swap_hdrs` moves with the header, but which no spec ties to the
  routed key.

## 27. Stats identities

The statistics challoc reports are internally consistent: per arena and
in the totals, allocated bytes never exceed active bytes, resident bytes
are active plus dirty plus metadata, and mapped bytes are resident plus
retained.

**Statement.** Two parts; the first is proved, the second holds by
construction (read from the code, not stated as an `ensures`).

1. **Identities** (proved). Every `ArenaStats` an arena stores, and
   every one `collect_arena`, `collect_into` and `collect_all` compute,
   satisfies `ArenaStats::identities` (`stats.rs`):

   ```rust
   small_allocated <= small_active && large_allocated <= large_active
   resident == small_active + large_active + small_dirty + large_dirty
               + huge_dirty + metadata
   retained == small_retained + large_retained + huge_retained
   mapped == resident + retained
   ```

   so `resident >= active + metadata`, `active >= allocated` and
   `mapped == resident + retained` per arena. Every `Snapshot` stored in
   `shared.stats` satisfies `Snapshot::inv`: with up to 256
   arenas, `Totals::identities`, the same three relations for
   the totals. `collect_into` and `collect_all` also
   ensure the identities, and `mapped` equal to the arenas' `mapped`
   summed, whenever that sum fits a `usize` (`sum_mapped`),
   for any number of arenas. The contracts:

   ```rust
   // Arena::stats (arena.rs), stats::arena_stats (stats.rs)
   ensures r.identities()                 // r matches Some(a) ==> a.identities()
   // Arena::store_stats (arena.rs)
   requires s.identities()
   // stats::snapshot, stats::collect
   ensures r.inv()                        // narenas <= 256 ==> totals.identities()
   // collect_arena_with
   ensures r.identities(), r.mapped <= arena_mapped_max(r.index), r.index == arena.spec_index()
   // collect_into; collect_all likewise for its sum
   ensures r.narenas == rd.0.len(), forall j: rd.0[j].identities(),
       sum_mapped(rd.0) <= usize::MAX ==> r.totals.mapped == sum_mapped(rd.0) && r.totals.identities(),
       r.inv()
   ```

2. **Coherence** (by construction). After
   an `epoch`, the stored per-arena figures sum to the stored totals
   (they come from one collection), and `mallctl("epoch")` returns the
   number its own refresh stored.
   - Arenas and totals (by construction):
     `collect_into` takes the process statistics lock
     (`shared.stats.lock()`) first and holds it across its whole scan,
     storing each arena's figures (`arena.store_stats(&a)`, under that
     arena's statistics lock) and then the totals under the one hold.
     It sums into the totals exactly the figures it stores for each
     arena (the same local `a`, read once), and `Arena::store_stats` has
     no other caller, so the arenas' stored figures change only under
     the process statistics lock: two `epoch`s cannot interleave their
     stores, and a reader holding that lock (`snapshot`, and
     `arena_stats_all_into`, which takes it across its sum) sees one whole
     collection. The test
     `stats::tests::concurrent_epochs_leave_arenas_summing_to_the_totals`
     (16 arenas, one thread allocating and freeing, two epoch writers
     per round, 2000 rounds) checks it. `arena_stats` (one arena's figures) does not take the
     process statistics lock, so a reader of `stats.arenas.<i>.*`
     beside `stats.*` during an `epoch` may see one arena of the
     new collection beside the old totals. The lock order this needs
     (the process statistics lock ranked below the class locks) is
     theorem 33's.
   - The epoch number (by construction): the
     `Epoch` arm of `mallctl::ctl` reads back, on a write, the number
     `stats::epoch` returned, which is the epoch number its own call
     stored (`collect_into` stores and returns one `Snapshot` under one
     hold of the snapshot lock), and on a read alone
     `stats::snapshot(shared).epoch`. The test
     `api::mallctl::tests::concurrent_epoch_writes_read_back_their_own_numbers`
     (four threads, 2000 epoch writes each, every number read back
     distinct) checks it. jemalloc holds `ctl_mtx`
     across the refresh and the read.

   Neither is stated as an `ensures`: `collect_into`'s ties only the
   totals' `allocated` (`totals_exact`) and `mapped` to `rd`, not to
   what was stored.

**Covers.** `mallctl` `stats.{allocated,active,metadata,resident,
retained,mapped}` and `stats.arenas.<i>.*` (from `snapshot` and
`arena_stats`), `malloc_stats_print` (`collect`, `collect_arena`),
`mallinfo2` and `malloc_stats` (`collect_all`); `mallinfo2` also ensures
glibc's identities `arena >= uordblks` and `fordblks == arena - uordblks`
when the arenas' `mapped` sum fits. Not covered:
`stats.arenas.4096.*` (`arena_stats_all_into`, `stats.rs`) sums stored figures
that each satisfy the identities, but carries no bound on how many it
sums or ghost record of them, so its result has no identity contract.

**Why no figure wraps.** The figures are added with `fig_add`/`fig_mul`,
exact only when the result fits (theorem 22). Each sum is bounded:
- per bin, `collect_bin` (`stats.rs`) ensures `BinStats::fits` (`stats.rs`):
  `curregs * size <= curslabs * slab_size`, from the clamp
  (`ClassState::bin_stats`, `class/mod.rs`, ensures
  `curregs <= curslabs * nslots`) and `nslots * size <= slab_size`
  (`Class::wf`); and `(curslabs + empty + purged) * slab_size <= 2^38`,
  the slabs being among the class region's
  (`ClassState::nslabs_are_the_region_s`, `lg_region <= 38`);
- over at most 64 classes, the slab bytes are at most `2^44`;
- a huge run count and page count come from the halves of the `large`
  word, below `2^32` (`Arena::large`, `arena.rs`, ensures it),
  and a page is at most `2^22` (`lg_page <= 22`,
  `Regions::separated`), so an arena's
  huge bytes are below `2^54`;
- arena 0 adds the free extents' pages, at most `u32::MAX`
  (`Huge::stats`, `huge/mod.rs`, ensures it from the huge lock
  invariant), and the committed metadata, at most the region's length,
  below `meta::va_limit() = 2^57` (`Meta::stats`, `meta.rs`,
  ensures it).
So arena 0's `mapped` is below `2^58` and any other's below `2^55`
(`arena_mapped_max`, `stats.rs`); every per-arena sum fits. The scan visits
arena `i` at index `i` (`arena_at`, ensures it), so arena 0
is counted once and `k` arenas sum below `2^58 + k * 2^55`, which fits
for `k <= 256`.

**Premises beyond the common ones.**
- The per-arena statistics lock's invariant, `ArenaStats::identities`,
  is stated by no axiom: `ArenaStats`'s `LockSpec` impl (`stats.rs`)
  defines it, with a frame true of any two values. Its writers: `Arena`
  initialisation (`ArenaStats::empty`, which ensures the identities,
  written by the trusted `Arena::put_stats`, which a test compares with
  `Lock::new`) and `Arena::store_stats` (which requires them). The process
  statistics lock's invariant, `Snapshot::inv`, is stated by no axiom:
  `Snapshot`'s `LockSpec` impl (`stats.rs`) defines it, `Lock::new` in
  `Shared::put_tail` (`heap/shared.rs`) requires it of
  `Snapshot::EMPTY`, and every holder (`collect_into`, `snapshot`,
  `arena_stats_all_into`) ends in `LockGuard::unlock`, which requires
  it (`api::tests::guards_end_in_unlock`).
  The per-arena statistics lock's holders (`Arena::store_stats`,
  `stats_into`, `add_stats_to`) also end in `LockGuard::unlock`, which
  requires its invariant and frame.
  The fork handlers take the raw locks only and write nothing.
- `process_regions()` is the `Shared`'s regions (`Shared::wf`), so
  `collect_bin`'s `requires process_regions().wf()` holds for every
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
- Coherence (part 2) is read from the code, not proved: no `ensures`
  ties the figures stored under the arenas' statistics locks to the
  totals stored under the process statistics lock, and a reader of one
  arena's `stats.arenas.<i>.*` (`arena_stats`, which does not take the
  process statistics lock) may see it from a newer collection than the
  `stats.*` totals it read before.
- The lock frames it rests on, which the `LockSpec` impls of `Created`
  (the arena creation lock), `meta::Inner` and `huge::Inner` define, are
  definitions, not axioms.

## 28. Cache flush completeness

**Statement.** Every flush of a whole heap empties every cache the heap
holds: after `Heap::flush_all` each header of the home array and of
every node on the heap's `others` list has `len == 0`, so the mallctl
flush operations strand no cached slot in a live heap. Turning
`thread.tcache.enabled` off also frees every one of those caches (no
stack left), and a cache GC step leaves each cache one of two stated
lengths (`gc_arena` states the exact one per array).

```rust
// heap/mod.rs (theorem 10's contract, with `tcache_on`)
pub fn flush_all(&mut self, shared: &Shared)
    ensures
        final(self).keeps(*old(self)),
        final(self).same_shape(*old(self)),
        final(self).tcache_on() == old(self).tcache_on(),
        final(self).drained(),
        log_adds(final(self).released(), old(self).released(), old(self).held_all()),

// heap/mod.rs
pub fn set_tcache_enabled(&mut self, shared: &Shared, on: bool)
    ensures
        final(self).wf(),
        final(self).tcache_on() == on,
        on ==> final(self).caches_same(*old(self)),
        !on ==> final(self).same_shape(*old(self)),
        !on ==> final(self).cleared(),
        !on ==> final(self).drained(),
        !on ==> log_adds(final(self).released(), old(self).released(), old(self).held_all()),

// heap/mod.rs
pub fn gc(&mut self, shared: &Shared)
    ensures
        final(self).keeps(*old(self)),
        final(self).same_shape(*old(self)),
        final(self).gc_all(*old(self)),
fn gc_arena(&mut self, shared: &Shared, arena: u16, node: *mut ArenaCaches)
    ensures
        final(self).keeps(*old(self)),
        final(self).same_shape(*old(self)),
        forall|c: int| 0 <= c < NCLASSES ==>
            final(self).gc_left(*old(self), old(self).array_hdr(node, c), arena == FOREIGN),
        // every other header: same value, same slots
        ...

// heap/cache.rs
pub(super) fn ensure_cache(&mut self, shared: &Shared, arena: u16, class: usize) -> (r: *mut CacheHeader)
    ensures
        ...,
        !shared.config.tcache || !old(self).tcache_on() ==> r@.addr == 0 && *final(self) == *old(self),
```

with (`heap/mod.rs`; abridged)

```rust
// drained: every home header and every header of a node on the
// `others` list has len == 0 (theorem 10).
// cleared: every one of those headers has cap == 0 (no stack).
pub proof fn lemma_cleared_drained(&self) requires self.wf(), self.cleared() ensures self.drained()

// What a GC step leaves in header p, which held old.hdr(p) before:
pub closed spec fn gc_left(&self, old: Heap, p: *mut CacheHeader, foreign: bool) -> bool {
    let lw = old.hdr(p).low_water as int;
    &&& self.cached_len(p) == if foreign { 0 } else { old.cached_len(p) - (lw - lw / 4) }
    &&& self.held(p).to_set() <= old.held(p).to_set()
    &&& self.hdr(p).low_water == self.hdr(p).len
}
// gc_all(old): gc_left(old, p, old.home() == FOREIGN) for every home
// header p, and gc_left(old, p, old.keys[i] == FOREIGN) for every
// header p of node i on old's `others` list.
```

In words:

- **Flush.** `flush_all` leaves every home header and every header of a
  node on the `others` list caching nothing, and (theorem 10) appends
  exactly the slots they held to the released log, each handed with its
  token to `Shared::release_routed`. Its callers: `thread.tcache.flush`
  (`ThreadTcacheFlush`, `api/mallctl.rs`, through `flush_caller`,
  which also serves `arena.<i>.purge` and `arena.<i>.destroy`),
  `tcache.flush` (`TcacheFlush`, through
  `Tcaches::flush`, `heap/tcaches.rs`), `malloc_trim`
  (`api/libc.rs`), and `Heap::destroy` (theorem 10).
  `Tcaches::flush` restates `drained()` after the call as a ghost
  `assert` (`flush_caller` relies on `flush_all`'s `ensures` alone);
  `Tcaches::flush` also ensures `false` for an id of
  `MAX_TCACHES` or more.
- **Caches off.** `set_tcache_enabled(false)` (`thread.tcache.enabled`
  written false, `ThreadTcacheEnabled`, `api/mallctl.rs`) runs `flush_all`, then
  `free_caches_in` (`heap/mod.rs`) over the home array and every
  node, which requires the array flushed (so its `assert_flushed`
  abort is proved unreachable there) and ensures each of the array's
  headers has no stack and no other header changed. So afterwards every
  cache is freed (`cleared`), hence empty (`drained`), the released log
  holds what they held, and `tcache_on()` is false. Writing it true
  changes no cache (`caches_same`).
- **Caches never come back while off.** `ensure_cache`, the only place a
  header gets a stack (`heap/cache.rs`, through the proof
  `CachePerms::lemma_fresh_stack`; by inspection), returns null and leaves the heap unchanged when
  `tcache:false` (`!shared.config.tcache`) or the heap's caches are off
  (`!tcache_on()`). So with `tcache:false` no header of any heap ever
  gets a stack and every cache stays empty; after
  `thread.tcache.enabled=false`, until it is written true again, the
  heap's caches stay freed.
- **GC.** One GC step (`gc`, run from `Heap::tick` every
  `PERIODIC_EVERY` slow paths once the GC interval has elapsed) leaves,
  per `gc_arena`, the `FOREIGN` cache empty and every other cache with
  `len - (lw - lw / 4)` entries, `lw` its low-water mark before the
  step: of the `lw` entries unused since the last GC it releases `lw -
  lw / 4` (three quarters, rounded up) and keeps `lw / 4`. What stays is
  a subset of the slots it held, and the low-water mark restarts at the
  new length; an unused header (no stack) stays empty. `gc` itself
  states the same per header, by the node's key (`Heap::keys`). Each `flush` it
  makes extends the released log as theorem 10 states per flush.

**Premises beyond the common ones.**
- Every heap a verified path is handed is `Heap::wf` (common premises:
  `TlsSlot::entry`, `heap_at`), and the statements are per reference, as
  in theorem 20: `flush_caller`, `Tcaches::flush` and the
  `ThreadTcacheEnabled` arm each state the result for the reference they
  hold; that the next call's reference is the same heap in the same
  state is not modelled.
- The `others` list is `Heap::list` (`list_ok`, with the nodes' `next`
  fields read through `Heap::heads`), as in theorem 10.
- That `ensure_cache` is the only writer that gives a header a stack is
  read from the code (one `lemma_fresh_stack` call, a proof); `set_arena` only
  exchanges headers between the home array and a node
  (`Heap::swap_hdrs`).

**Scope.** The mallctl keys `thread.tcache.flush`, `tcache.flush`,
`thread.tcache.enabled`, `arena.<i>.purge`, `arena.<i>.destroy`, and
`malloc_trim`, on the calling thread's heap or the named explicit
cache; the configuration option `tcache:false`; the periodic GC. The
mallctl call sites of `Heap::set_tcache_enabled` and `Tcaches::flush`
are verified and checked against the callees' preconditions
(`thread_heap` ensures its heap `wf`), so both keys' contracts are the
callees'.

**Where the statement is weaker than the code.**
- The emptiness is over the home headers and the headers of the nodes
  on the `others` list (`drained`); a header the permissions own that is
  neither is not ruled out (theorem 10's first gap), and the `caches`
  field, which only the tests' `free_caches` writes (`destroy` frees
  through `free_caches_in`), is modelled apart from the permissions.
- "Strands nothing" is the released log of theorem 10, with its gaps:
  that each token leaving `release_routed`'s bag reaches a bitmap word
  rests on the verified bodies, not on an `ensures`.
- That a home arena is never `FOREIGN` is not stated; `gc_all` covers
  both cases for the home array, and gives each node's headers by the
  node's key (`Heap::keys`).
- `gc` and `gc_arena` state lengths and subsets, not the released log:
  each `flush` they call appends what it released (theorem 10's per
  flush `log_adds`), but their `ensures` do not compose those entries.
- "Caches stay off" is per call: no invariant of `Heap::wf` ties
  `tcache_on()` or `config.tcache` to the headers' capacities; it
  follows from `ensure_cache`'s contract and the inspection premise
  above, not from one theorem over a run.
- `tcache.destroy` frees the explicit cache through `Heap::destroy`
  (verified; it flushes with `flush_all`, but its `ensures` states only
  the lock order); `thread.tcache.enabled` and
  `tcache.flush` on another thread's cache concurrently are outside it
  (the caller's bug, as in jemalloc).

## 29. Huge free bitmap is exact (at the huge lock invariant)

The huge free-page bitmap marks exactly the pages of free extents: a set
bit always belongs to a free extent, and every page of a free extent has
its bit set. This is a fact of every value the huge lock admits
(`Inner::wf`), like theorem 17; no entry point's `ensures` mentions it.
Built on theorems 17 and 23.

**Statement** (`huge/mod.rs`):

```rust
spec fn free_exact(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, f: vstd::set::Set<nat>) -> bool {
    forall|q: nat| f.contains(q) ==> exists|x: *mut HugeDesc| in_free(descs, x, q as int)
}

proof fn theorem_free_exact(&self, q: int)
    requires
        self.wf(),
        0 <= q,
    ensures
        self.free.map.view().contains(q as nat) <==> exists|x: *mut HugeDesc| in_free(self.descs@, x, q),
        q >= self.next_page ==> !self.free.map.view().contains(q as nat),
```

(Line breaks and triggers dropped.) `in_free(descs, x, q)` (theorem 23):
`x` is a live free (`Empty` or `Purged`) descriptor holding page `q`.
In every value the huge lock admits, page `q`'s bit in the free-page
bitmap is set exactly when `q` is a page of a free extent: no `Active`
page and no page at or past the high-water mark `next_page` has its
bit set. `free_marked` (theorem 17) is the
`<==` half; `free_exact`, the `==>` half, is a conjunct of
`Inner::wf`. `Huge::audit` checks the same at run time ("page
of run marked free", "page of free extent not marked free", and
`lowest_set_from(next_page)` is `None`); as extents cover every page
below the mark (the page tallies sum to it), the three checks are this
statement.

**Consequence: `extent_at`'s abort is unreachable.** `Inner::extent_at`,
which `find` calls at every free bit its scan stops at,
requires the bit set and every free extent starting below the page
ending by it (`ends_by`), and proves its "huge: free page {page} has no
extent" abort branch unreachable (`assert(false)` there, ghost): the
bit's page is in a free extent, which cannot start below the page, so
starts at it, and the table keys every extent's start (`keyed`).
`find` establishes `ends_by(a)` at the lowest set bit `a` at or
past `from` from its loop's `ends_by(from)`: a free extent starting in
`[from, a)` would put its first page's bit there. So under the lock
invariant `find` never reaches that abort (theorem 23).

**How it is kept.**
- `Huge::new`: `class::Set::new` (`class/sets.rs`)
  ensures the fresh bitmap's view is empty (from `lemma_fresh`, already
  called), and there are no descriptors.
- `Huge::take` (`huge/mod.rs`): `took_free` is an equivalence:
  after a take the set is exactly the old set outside the run plus the
  alignment hole `[mark, run start)`. Its `==>` direction needs no old bit
  in the run's pages outside the cleared range `[max(s, a), min(end,
  e_end))`: those pages are past `ext`, which then touches the mark
  (`take_ok`), or, with no `ext`, at or past the mark; and every old bit
  is below the mark (`lemma_free_below`, from `free_exact`).
  `Take::touch_ok` carries three coverage facts (a head is kept when `ext` starts below the run, a tail
  when it ends past it, a hole whenever the run starts past the mark), and
  `lemma_took_coalesced` concludes `free_exact`: an old bit
  outside the run is in an untouched free extent or in `ext`'s head or
  tail, and a hole bit in the hole or in the head that absorbed it.
- `Huge::free_run`: `freed_free` is an
  equivalence (the set gains exactly the run's pages, `set_range`), and
  `lemma_freed_coalesced` concludes `free_exact`: an old bit's
  extent is `pred`, `succ` (both the merged survivor) or untouched.
- `purge_desc` only turns an `Empty` extent `Purged`, which
  stays free in the same place.

Without `free_exact`, the invariant would admit an extra free bit on an
`Active` page or past the mark, under which `find` could reach
`extent_at`'s abort without breaking its contract; no path sets such a
bit.

**Premises beyond the common ones.**
- The huge lock protects `Inner::wf`, including `free_exact`:
  `huge::Inner`'s `LockSpec` impl defines it, and every holder checks it
  at its `LockGuard::unlock`.
- `class::Set::ensure` (verified) leaves the bitmap view unchanged, as
  in theorem 17. The view's `set_range`, `clear_range`,
  `lowest_set_from` and `lemma_fresh` are verified (`bitmap.rs`).

**Where the statement is weaker than the code.**
- It is stated at the lock invariant only; no entry point exports it,
  and nothing per call beyond `find`'s abort-freedom uses it.
- It speaks of the bitmap's view (`TwoLevelBitmap::view`), whose tie to
  the words in memory is the bitmap module's verified model; the second
  level's summary bits are part of that model, not of this statement.
- The descriptor table's storage comes from
  `huge::table::Table::storage`, verified over the Kani-discharged
  `axiom_zero_entry` and a `ZeroRaw::axiom_new` pairing for a binned
  table (theorem 4).

## 30. Decay deadline soundness (per gate, over the loaded values)

A theorem about one decay gate, `purge::Decayer`, over the values its
lock-free loads see: the deadline it returns is never earlier than
`now`, and, unless it is a back-off deadline (a failed discard's,
`Decayer::back_off`), never later than one step of the period it was
called with past the oldest `Empty` member's stamp or `now`, whichever
is later. A
`next_walk` more than one step away (stored under a longer period, since
shortened) is never returned. The bound is about what the body does with
what it loaded: callers get only its `t >= now` half (below). That the
background thread wakes at the deadline (the futex wait,
`note_decay_changed`) is out of scope.

**Statement** (`purge.rs`, over `Decayer::spec_decay`):

```rust
// purge.rs
pub fn due(&self, now: u64, decay_ns: u64) -> (r: Due)
    ensures
        match r {
            Due::Done(d) => {
                &&& d matches Some(t) ==> t >= now
                &&& exists|oldest: u64, retry: u64, next: u64, after: u64, retry2: u64|
                    d == #[trigger] Decayer::spec_decay(now, decay_ns, oldest, retry, next, after, retry2)
            },
            Due::Walk(curve, step, seen) => {
                &&& Decayer::spec_walks(now, decay_ns, seen@.0, seen@.1, seen@.2)
                &&& curve.period() == decay_ns
                &&& step == Decayer::spec_step(decay_ns)
            },
        },

// purge.rs
pub fn finish(&self, now: u64, step: u64, Ghost(decay_ns): Ghost<u64>, Ghost(seen): Ghost<(u64, u64, u64)>)
    -> (r: Option<u64>)
    requires
        Decayer::spec_walks(now, decay_ns, seen.0, seen.1, seen.2),
        step == Decayer::spec_step(decay_ns),
    ensures
        r matches Some(t) ==> t >= now,
        exists|after: u64, retry2: u64|
            r == #[trigger] Decayer::spec_decay(now, decay_ns, seen.0, seen.1, seen.2, after, retry2),

pub proof fn theorem_decay_deadline(now: u64, decay_ns: u64, oldest: u64, retry: u64, next: u64,
    after: u64, retry2: u64)
    ensures
        ({
            let r = Decayer::spec_decay(now, decay_ns, oldest, retry, next, after, retry2);
            let step = Decayer::spec_step(decay_ns);
            &&& r is None <==> oldest == 0 || (Decayer::spec_walks(now, decay_ns, oldest, retry, next)
                && after == 0)
            &&& r matches Some(t) ==> now <= t
            &&& r matches Some(t) ==> t == retry || t == retry2
                || t <= (if oldest <= now { now } else { oldest }) + step
            &&& r matches Some(t) ==> (oldest <= now ==> t == retry || t == retry2 || t <= now + step)
            &&& r matches Some(t) ==> (t == now ==> Decayer::spec_walks(now, decay_ns, oldest, retry, next)
                && (step == 0 || now == u64::MAX))
        }),
```

`oldest` is the first `oldest_empty` load, `retry` the `retry_at` load
before the walk, `next` the `next_walk` load, `after` the
`oldest_empty` load after the walk and `retry2` the `retry_at` load
after it; a load the call does
not make is a free variable of the existential. `spec_step(d)` is
`d / STEPS`, `Curve::step_ns`'s exact spec (`purge/curve.rs`) for
`Curve::new(d)`. `spec_walks` (`purge.rs`) is the walk's condition:
something `Empty`, its first step over, and `next` not within one step
after `now`. `spec_decay` is the code's branches with
`saturating_add` spelled as `sat_add`; it is `closed` (only
`purge.rs` sees its body), so callers reason through the theorem.

The gate is two halves: `due` decides whether to walk, the gate's owner
runs the walk in its own `decay_with`, and `finish` stores the next walk
time and loads the gate again. In words: the gate (`due`, then `finish`
after a walk) returns `None` exactly when the oldest-`Empty` load is
0 or a walk ran and the load after it reads 0. A deadline is at most
`max(oldest, now) + step`, so, with the oldest member stamped no later
than `now`, at most one step past `now`. That is the period-change case
of `due` (`next - now <= step`): after `set_decay_ms` (`arena.rs`) or
a pressure band (theorem 18) lowers the period, the next call does not
return a deadline computed with the old step: it returns the end of the
oldest member's first (new) step, or walks and returns one new step from
`now`. A deadline equal to `now` (the background thread then sleeps
`MIN_SLEEP_NS`) needs a zero step (`decay_ns < STEPS`) or saturation,
and only follows a walk.

A walk whose discard `madvise` refused (`mlock`ed
memory) calls `Decayer::back_off(now)`, which stores `now +
DISCARD_RETRY_NS` (one second) in `retry_at`: no walk runs before it
(`spec_walks` includes `now >= retry`), a call during it returns it, and a
walk's deadline is the later of the next step and the back-off. A
back-off deadline is the one deadline the step bound does not cover:
the theorem names it (`t == retry || t == retry2`), a value `retry_at`
held, and nothing bounds it against `now` beyond `now <= t` (it is a
stamp `back_off` took from the walk's clock, one second ahead).

With theorem 18 (`purge.rs`):

```rust
pub proof fn theorem_decay_deadline_under_pressure(base: u64, limit: u64, c1: u64, c2: u64,
    p1: u64, p2: u64, now: u64, oldest: u64, retry: u64, next: u64, after: u64, retry2: u64)
    requires
        c1 <= c2,
        p1 == spec_effective_decay_ns(base, spec_band(limit, c1)),
        p2 == spec_effective_decay_ns(base, spec_band(limit, c2)),
    ensures
        Decayer::spec_step(p2) <= Decayer::spec_step(p1),
        Decayer::spec_decay(now, p2, oldest, retry, next, after, retry2) matches Some(t)
            ==> t == retry || t == retry2
                || t <= (if oldest <= now { now } else { oldest }) + Decayer::spec_step(p1),
```

At one limit and configured period, higher usage never gives a longer
step, so the deadline bound under it is no later than under lower usage.
`Policy::effective_decay_ns` (`policy.rs`), the period
`purge::decay` and `purge::decay_arena` pass to every gate, ensures
`ns <= base`: the stored
band never lengthens a configured step either.

**Covers.** Both gates: `ClassState::decay_with` (`class/decay.rs`)
and `Huge::decay_with` (`huge/mod.rs`) ensure the gate's contract
for the `decay_ns` they are given. `mallctl
arena.<i>.decay` (`purge::decay_arena`) gives them the periods
`purge::decay` does, through the same `decay_one` and `decay_huge`
(the policy-scaled `effective_decay_ns`, and for the huge extents the
huge tier's own period). `Arena::decay_with`
and `purge::decay` (`purge.rs`, through `earliest`) take the minimum
of the per-gate deadlines; the background thread (`purge/background.rs`)
sleeps until `purge::decay`'s result, and `on_tick` keeps only whether it
is `Some` (dirty memory still waiting, which runs
the lazy start's look, `background::auto_tick`). No C
symbol's `ensures` changes.

**Premises beyond the common ones.** None for the deadline: no axiom,
`external_body` or `assume_specification` is used by the theorem. The
owner runs its walk between `Decayer::due` and `Decayer::finish` in its
own `decay_with`, with the caller's lock-order token (theorem 33). The
loads are `core` atomics, which vstd specifies with any value, which is
why the statement is over the values seen. The walk's `ensures` are not
used.

**Scope and where the statement is weaker than the code.**
- What callers can use. The witnesses are existential, so a caller
  cannot discharge `oldest <= now`: what the gate's and `decay_with`'s
  `ensures` give a caller is `t >= now`, and (through the theorem)
  `t == now` only when `decay_ns < STEPS` or `now == u64::MAX`. The
  one-step bound states what the body does with what it loaded; nothing
  ties `oldest` to a clock reading no later than `now`, since the stamps
  and the coarse clock are not ghost state.
- Per gate, not per thread wait. `Arena::decay_with` and `purge::decay`
  combine the gates' deadlines by `min` (`map_or`, `earliest`), and the
  background thread sleeps `max(deadline - now, MIN_SLEEP_NS)`; that the
  combined deadline is at most each gate's, and so the wait at most one
  step (or 1 ms) of the shortest period of any gate whose oldest member
  is stamped by `now` and that returns a deadline, is read off those
  bodies, not proved.
- Over loaded values, not the atomics. The theorem does not say the
  loads return the gate's current state: concurrent `set_oldest_empty`
  and `note_first_empty` calls (under the owner's lock, read here
  without it) can change `oldest_empty` between the two loads, and the
  `Relaxed` orderings are not modelled (`verus/WEAKMEM.md`). Because it
  is per call over what the call saw, it holds for concurrent calls on
  one gate (`arena.<i>.decay` through `mallctl` beside the background
  thread), but says nothing about which call's `next_walk` store wins.
- Stamps after `now`. An `oldest` later than `now` (a stamp from another
  thread's later coarse clock read) gives a deadline up to `oldest +
  step`; the theorem bounds it by that, not by `now + step`.
- Not that the walk runs at most once per step: the store of `next_walk`
  and the guard are in `finish`'s and `due`'s bodies, but no ghost history of walks is
  kept, so "at most once per step" is not an `ensures`.
- The `NEVER` period: callers skip it (`Arena::decay_with`, the huge
  branch of `purge::decay`, `mallctl arena.0.decay`'s huge call);
  the gate itself would return deadlines up to
  `sat_add(max(oldest, now), NEVER / STEPS)`, within the bound.
- The period is the one passed in: that it is the arena's current
  `decay_ns` after `set_decay_ms` is the caller's load (any value, as
  above), and theorem 18's link from usage to period is for the pure
  functions only (theorem 18's own scope, including the `Policy` atomics
  round trip).

## 31. Huge retain cap (at the free's lock release)

A freed huge run is discarded before the free releases the huge lock
only when its usable bytes exceed the configured retain cap or
`purge_on_free` is set, and such a run is discarded unless the kernel
refused the discard advice. Stated at the huge lock's release inside `Huge::free_run`, over the cap
and flag values the free read: no `ensures` exports it (below).

**Statement** (`huge/mod.rs`; proved by
`lemma_retain_cap`, called in `free_run` before `drop(g)`):

```rust
spec fn retain_cap(v0: Inner, v1: Inner, page: int, n: int, usable: int, cap: int,
    pof: bool, now: u64, discarded: bool, at: int, len: int) -> bool {
    &&& discarded ==> (usable > cap || pof)
    &&& discarded ==> crate::purge::discard_taken(at, len)
    &&& (usable > cap || pof) && !discarded ==> crate::purge::discard_refused(at, len)
    &&& exists|x: *mut HugeDesc| v1.descs@.contains_key(x) && {
        let e = v1.descs@[x].value();
        &&& e.start <= page && page + n <= e.start + e.pages
        &&& if discarded {
            (e.state == Purged && e.dirty_at == 0)
                || (e.state == Empty && empty_nbr_at(v0, page, n, e.dirty_at))
        } else {
            e.state == Empty && (e.dirty_at == now || empty_nbr_at(v0, page, n, e.dirty_at))
        }
    }
}

// An `Empty` extent of `v0` aged `t` touches the run.
spec fn empty_nbr_at(v0: Inner, page: int, n: int, t: u64) -> bool {
    exists|y| v0.descs@.contains_key(y) && v0.descs@[y].value().state == Empty
        && v0.descs@[y].value().dirty_at == t
        && (v0.descs@[y].value().start + v0.descs@[y].value().pages == page
            || v0.descs@[y].value().start == page + n)
}

// purge.rs (closed): the kernel's answer to some discard advice.
spec fn discard_taken(addr: int, len: int) -> bool {
    &&& 0 <= addr <= usize::MAX
    &&& 0 <= len <= usize::MAX
    &&& exists|a: i32| (a == MADV_DONTNEED || a == MADV_FREE)
        && sys::madvise_ok(addr as usize, len as usize, a)
}
spec fn discard_refused(addr: int, len: int) -> bool {
    exists|a: usize, l: usize, adv: i32| a <= addr && addr + len <= a + l
        && (adv == MADV_DONTNEED || adv == MADV_FREE) && sys::madvise_err(a, l, adv)
}

// In `free_run`, after the merged extent is entered, before `drop(g)`:
lemma_retain_cap(g0, g@, survivor, survivor != d, page as int, n as int,
    usable as int, cap as int, pof, now, discarded, addr as int, (n * page_size) as int);
// whose `ensures` is `retain_cap(g0, g@, ...)` with the same arguments.
```

(Line breaks and triggers dropped; `Purged`/`Empty` stand for
`RunState::Purged as u8`/`RunState::Empty as u8`.) `g0` is the huge lock
value `free_run` takes, `g@` the one it releases, and `[page, page + n)`
the freed run's pages (guard included). `free_run` binds the reads
itself (`huge/mod.rs`): `cap` is the `retain_max()` it
read, `over` is `usable > cap`, `pof` is `!over && self.purge_on_free()`
(the flag's value when it was read, at or below the cap, and `false`
when it was not), and `purge` is `over || pof`. `now` is its clock
reading (`now_ns().max(1)`), and `discarded` whether the run was
discarded: the ghost `st == Purged`, which `free_run` sets
only when `purge` holds and `purge::discard_one` of the run's whole
range, `[addr, addr + n * page_size)`, returns true (a discard `madvise` refuses, on `mlock`ed pages, leaves the
run `Empty` and dirty instead of aborting). `at` and `len` are that
range, `addr` and `n * page_size`.

In words: a huge run is discarded (`MADV_DONTNEED`, or `MADV_FREE` under
the lazy advice), pages and guard, before the free releases the huge
lock, only when its usable bytes exceed the cap read or the
`purge_on_free` flag read true; its pages then lie in a free extent that
is `Purged` with no age (`dirty_at == 0`), unless an `Empty` neighbour
merged in (below). A run not discarded lies in an `Empty` extent aged
`now`, unless it merged into an `Empty` neighbour, whose age the merged
extent keeps. A run above the cap, or freed while the flag read true, is
discarded unless the kernel answered a discard `madvise` of a range
holding its bytes with an error (pages the program `mlock`ed); a
discarded run's own range was answered with success. A refused run is
kept the same way, for the decay walk or the next explicit purge to
retry. This bounds the large-buffer return `docs/tuning.md`
describes for `huge_retain_max` ("purged the moment they are freed
instead of waiting for decay; smaller ones are retained for reuse and
decay", section 2's table) and for `huge_dirty_decay_ms:0` ("purges a
huge run the moment it is freed, whatever its size"), and design 7's
"Runs whose usable size is above `huge_retain_max` ... are purged at
once"; the default cap is `DEFAULT_RETAIN_MAX`, 64 MiB
(`huge/mod.rs`).

What is proved and what is not:
- Proved: the first conjunct checks `free_run`'s cap comparison against
  the values it bound (a discard needs `over || pof`), and the last
  states the extent's state, age and place, per outcome of the discard,
  from `free_run`'s merge (`Merge`, theorem 17).
- Proved: the second and third conjuncts
  give the kernel's answer. A discarded run's range was answered with
  success by some `MADV_DONTNEED` or `MADV_FREE` `madvise` of exactly
  that range (`discard_taken`); a run with `usable > cap || pof` that is
  not discarded lies in a range some such `madvise` answered with an
  error (`discard_refused`). So a run above the cap is discarded unless
  the kernel refused a discard advice on its bytes. `discard_one`
  (`purge.rs`) ensures `r ==> discard_taken` and
  `!r ==> discard_refused` of its range, verified from the `One`
  settler's invariant (the pushed range pending alone, or settled with
  `discarded` the kernel's answer), `Settle::settle`'s `requires` on
  `done`, `discard` (`purge.rs`) and `sys::madvise`
  (`sys/mod.rs`), whose `ensures` names the word `raw::syscall3`
  returned for the call.
- Not proved: that the answer was to this free's call. `discard_taken`
  and `discard_refused` are timeless: each says that some `madvise` with
  those arguments was answered so, not that the last one was, and a
  range can be both. A run above the cap that is kept may rest on a
  refusal from an earlier purge of a range holding it; the model keeps
  no call history. `discard` returns false only on `EINVAL` and aborts
  on any other errno, but `discard_refused` records only an error word,
  not which.
- Not constrained: the values of `cap` and the flag. `retain_max` and
  `purge_on_free` are plain atomics with no spec view (premises below).
- Theorem 19 proves the bytes `discard_one` hands `madvise` are no live
  block's.

**Covers.** Every free of a huge run: `free`, `free_sized`,
`sdallocx`, `dallocx`, the frees of `realloc`/`rallocx` moving a huge
run, and `MALLOCX_TCACHE_NONE` frees all reach `Shared::free_huge`
(`heap/shared.rs`, from `Heap::free_huge` and `free_direct` in
`heap/paths.rs`), the one non-test caller of `free_run`. Builds on
theorems 17 and 19: the survivor and its merged pages are `free_run`'s
`Merge` (theorem 17's coalescing), and the batch's `free` predicate is
theorem 19's (the run's pages left the run-page authority with its
token).

**Premises beyond the common ones.**
- The cap and flag are the configured ones: `retain_max` and
  `purge_on_free` (`huge/mod.rs`) are `Relaxed` loads of
  std atomics, whose values vstd leaves unconstrained, so the theorem
  is over the values read. By inspection, `set_retain_max` and
  `set_purge_on_free` are called only in `Shared::init`
  (`heap/shared.rs`), on the `Huge` before `put_huge`
  publishes it, and by tests (no mallctl or other path calls them;
  `opt.huge_retain_max` reads the config), so every free reads
  `huge_retain_max` and `huge_dirty_decay_ms == Some(0)` as configured.
  `purge_on_free` is set only for an explicit `huge_dirty_decay_ms:0`;
  with it unset and `dirty_decay_ms:0`, the flag is false and a run at
  or below the cap waits for the (zero-period) decay walk.
- The kernel's answer: `madvise_ok` and `madvise_err`
  (`sys/mod.rs`) read the word `raw::syscall3` returned
  (`raw::returned3`, which only `syscall3`'s trusted `asm!` body
  ensures: `sys/raw.rs`, on x86_64 and on aarch64; discharged on
  premises (K1) by Isla on both, up to the system call, the kernel's
  side assumed). The
  discard step itself is verified (above).
- `madvise` does what it is asked when it succeeds (the kernel
  contract, a common premise); `EINVAL` leaves the range in place and
  is reported, any other failure aborts in `discard`.
- The huge lock's invariant (`huge::Inner`'s `LockSpec` impl), as for
  theorems 17 and 19.

**Scope and where the statement is weaker than the code.**
- Not "ends `Purged`" outright. A run above the cap that coalesces with
  an `Empty` neighbour is discarded, but the merged extent is `Empty`
  with the neighbour's age (`free_run` keeps the oldest `Empty`
  neighbour as the survivor), so its discarded pages count as
  `empty_pages` (arena 0's `huge_dirty` in the stats) and the decay
  walk discards them again. A run at or below the cap that coalesces
  with an `Empty` neighbour takes that neighbour's age, not `now` (the
  code never compares the two), so it is usually purged earlier than
  its own age would have it. Both are by design, and conservative for
  RSS; the tests `retain_cap_decides_purged_or_empty_at_free` and
  `a_freed_run_takes_an_empty_neighbours_state_and_age`
  (`huge/tests.rs`) pin both outcomes, the discarded byte
  counts and the ages (the second waits for the clock to pass the
  neighbour's age first).
- At the lock release, not at `free`'s return: after `drop(g)` another
  thread may take the extent again, so no `ensures` can state the
  extent's state at return. The discard does happen before the free
  returns (it is inside the lock). `free_run` returns no ghost result
  that carries the statement to its callers (`free_huge` and the tests).
- Over the values read. A concurrent `set_retain_max` (tests only) or a
  run-time change of the configured values is outside the premise
  above. The `usable` compared is the descriptor's usable bytes (whole
  pages, guard excluded), not the size the caller asked for: a run of
  `cap + 1` requested bytes has `usable > cap` and is discarded; one
  whose request rounds up to exactly `cap` is retained.
- `now` is the coarse clock as read; nothing orders it against the
  neighbours' ages, and the survivor between two `Empty` neighbours is
  chosen by the code's `<` on their stored stamps, not by a clock fact.

## 32. Read-only APIs never abort

The read-only entry points (`get_defrag_hint`, `malloc_usable_size(NULL)`,
`nallocx`, `mallinfo`, `mallinfo2`, `challoc_features`, and `mallctl`
and `mallctlbymib` for every read-only key but the four `thread.*`
readers) abort at most at an integrity check once init has completed,
and only `mallinfo` and `mallinfo2` reach one. The proof tracks an
abort capability that only `extern "C"` bodies and the panic handler
grant.

**Statement** (`sys/mod.rs`; `api/mod.rs`, `api/mallctl.rs`):

```rust
#[verifier::opaque]
pub closed spec fn may_abort() -> bool { true }

pub proof fn grant_aborts()
    ensures
        may_abort(),

pub uninterp spec fn integrity_aborts() -> bool;

pub open spec fn may_abort_corrupt() -> bool {
    may_abort() || integrity_aborts()
}

#[cfg(verus_keep_ghost)]
fn theorem_read_only_never_aborts(shared: &Shared, size: usize, flags: c_int, p: *const u8,
    verbose: bool)
    requires
        crate::sys::may_syscall(),
        crate::sys::integrity_aborts(),
        shared.wf(),

#[cfg(verus_keep_ghost)]
fn theorem_mallctl_reads_never_abort(shared: &Shared, name: *const c_char, io: &Io,
    m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>, Tracked(cc): Tracked<CCall>,
    Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        crate::api::marks_ready(*mark),
        cc.fresh(CEntry::Mallctl { name: name as *const u8, oldp: io.oldp as *mut u8, .. }),

#[cfg(verus_keep_ghost)]
fn theorem_mallctlbymib_reads_never_abort(shared: &Shared, mibp: *const usize,
    miblen: usize, io: &Io, Tracked(mark): Tracked<&'static ReadyMark>,
    Tracked(cb): Tracked<CCall>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        crate::api::marks_ready(*mark),
        cb.fresh(CEntry::ByMib { mib: mibp, miblen, oldp: io.oldp as *mut u8, .. }),
```

(Bodies elided.) `may_abort()` is a capability, as theorem 8's
`may_syscall()`: every function that can reach an abort requires it,
and `grant_aborts` gives it only in the bodies of `extern "C"`
functions and of the panic handler. The integrity checks, the aborts
that test allocator state no verified invariant constrains, accept the
narrower `may_abort_corrupt()`
instead: `vspec::check` (`requires may_abort_corrupt() || c`) and
`sys::abort_integrity` (`sys/abort.rs`). The first witness holds
only `integrity_aborts()`, which nothing grants, the other two hold no
abort capability at all; all hold `may_syscall()` (the read-only paths
take locks) and a well-formed `Shared` (the readers require the
`ReadyMark` the witnesses take: `marks_ready`, in `api/mod.rs`, is the
token for a `READY` the caller saw, which `STATE`'s invariant keeps
final, so a reader may rely on `STATE` staying `READY`, which the
`ready()` fact alone does not give). They make the calls below and verify. So these
calls abort at most at an integrity check, whatever their arguments
(for the `mallctl*` pointers, whatever arguments keep the C contract,
as the premises below say), and only `mallinfo`/`mallinfo2` reach one:

- `get_defrag_hint(p)` (`api/jemalloc.rs`), whole, for every `p`
  and before init. It requires no abort capability at all (only
  `may_syscall()`): it never aborts.
- `malloc_usable_size(NULL)` (`api/libc.rs`, `requires may_abort()
  || p@.addr == 0`): returns 0 and never aborts.
- `nallocx` (`api/jemalloc.rs`) after its `shared()`:
  `Flags::decode` and `usable_for`; never aborts.
- `mallinfo2` and `mallinfo` (`api/glibc.rs`) after
  `shared()`: their bodies call `stats::collect_all`, `page_size`,
  `fig_add`, `fig_mul` and `saturating_sub`, and `mallinfo` clamps; the
  witness makes the calls (both bodies are verified, and require
  `may_abort()` for their `shared()`;
  `api::restated::read_only_restatements_match_their_bodies`
  checks that the witness and `mallinfo2`'s body make the same calls). Only the integrity check below.
- `challoc_features` (`api/mod.rs`) after `shared()`: the feature
  line's build at the first request
  (`api::line_ready`: the per-size THP probe and `FeatureLine::build`,
  through the verified `build_line`, which requires only
  `may_syscall()`) or the wait for another thread's (termination not
  claimed), then the static line; never aborts.
- `mallctl` and `mallctlbymib` after their `super::shared()`: `cstr`,
  `name_to_mib`, `mib_in`, `mib_to_node` and `ctl` (`api/mallctl.rs`)
  for every node where `ctl_never_aborts` holds: an interior
  node, a read whose reader is not one of `thread.*`'s four
  (`Reader::aborts`): every key under `opt`, `arenas` and `stats`,
  `version` and `config.*`; or `experimental.utilization.query`
  and `batch_query` (`utilization`, `batch_query`).
  `ctl` requires `may_abort() || ctl_never_aborts(*node, mib@)`. Every
  reader is a named verified function (`opt_value` and the rest),
  each called by the witness for
  the readers that do not abort; `call`, which runs the reader of the
  node the MIB resolves to, is a verified `match` over the `Reader` enum
  and requires `may_abort()` or `!read.aborts()`. Every such read but
  both utilization queries never aborts, `stats.arenas.<i>.*` with `i`
  = `MALLCTL_ARENAS_ALL` (`stats::arena_stats_all_into`) included: the
  witness requires no `integrity_aborts()`. The seven `stats.cgroup.*`
  reads behind `mallctl::tracked` (`limit`, `high`, `current`,
  `decay_ms`, `retention_cap`, `empty_bytes`, `full_purges`) run the
  deferred cgroup discovery first (`Shared::cgroup_now`) and are among
  them: a waiting `Deferred::discover` returns the
  cell's frozen permission (from its own release store of `DONE` when
  it discovered, else from the acquire load of `DONE` that ended its
  wait; the phase's invariant `PhasePred` holds that permission from
  `DONE` on), and `cgroup_now` reads the record through it
  (`Deferred::published`), with no second load of the phase and no
  check.
- `mallctlnametomib` requires no abort capability: it never
  aborts, before init too (it never runs it), for a `name`, `mibp` and
  `miblenp` that keep their C contract.

A corollary needs no new witness: theorem 8's
`theorem_no_syscall_on_fast_paths` (`api/libc.rs`) requires only
`integrity_aborts()` (and its call's block token), so the cache fast
paths of `malloc`, `free`, `free_sized` and `challoc_free_usable` it
calls abort at most at an integrity check; their checks that
fail leave the fast path for the slow one, which requires `may_abort()`.

The integrity checks the covered calls reach
(`sys::tests::narrow_aborts_only_at_their_check_sites` pins the callers
of `check` and `abort_integrity`, and
`sys::tests::trusted_aborts_require_a_capability` pins `check`, the
one trusted body among them, to its clause `may_abort_corrupt() || c`):
- `stats::huge_usable_pages` (`stats.rs`, abort in
  `huge_pages_short`): an arena's `large` word holding fewer
  pages than its runs' guards. The word is a plain `AtomicU64` that
  `note_huge_alloc`/`note_huge_free` update together, so it fails only
  on a corrupt counter. Reached by `mallinfo`/`mallinfo2`
  (`collect_all`), not by any mallctl read (the `stats.*` totals read the
  stored snapshot).

`Shared::cgroup_now` has no integrity check: it reads the record
through the frozen permission the waiting discovery hands back (a
waiting `Deferred::discover` ensures it has one), so neither the seven
`stats.cgroup.*` reads nor `malloc_stats_print`'s `text_general`,
`policy_line` and `json_general` require `may_abort_corrupt()`.
`discover`'s wait loop loads the phase through `load_phase` (an
`Acquire` load).

The arena count has no integrity check either: it is an atomic
invariant (`NarenasPred`, `arena.rs`): the counter's permission
(`vspec::count::CountPerm`) has a value at most `MAX_ARENAS`.
`Arenas::init` (verified) makes it with the permission
`vspec::count::new` mints for the clamped automatic count,
`Arenas::create` stores `index + 1` inside it for an `index` below
`MAX_ARENAS`, and `Arenas::count` (verified) loads inside it, so its
`ensures r <= MAX_ARENAS` is proved. With it, `stats::arena_at` requires
`i < MAX_ARENAS`, which every caller's loop below the count shows, and
has no check. `policy::empty_bytes` is verified, an index loop over
`Arenas::get_existing` with `fig_add64` sums, and requires only
`may_syscall()`. So no mallctl read needs `may_abort_corrupt()`: `ctl`,
`call`, the readers, `policy::evaluate`, `purge::sample`,
`print::policy_line` and `print::json_general` do not require it.

The `large` word has no invariant: no honest one rules out the
remaining check. `Shared::free_huge` subtracts a run from its arena's
word after `Huge::free_run` retired it, so between the two the run's
pages can be reallocated and added to the same arena's word again. The
word therefore counts the live runs plus one per free in progress, and
nothing bounds the frees in progress (one per thread), so nothing proves
the page half below `2^32`; past it the low half carries into the run
half and the guard test can fail.

The other integrity checks (`SlabTable::new`'s `check`, and
`AtomicBitmap::allocated`'s, `bitmap.rs`: a cleared padding bit,
reached only by `malloc_stats_print`'s `slab_histogram`, which requires
`may_abort()`; `allocated` itself requires `may_abort_corrupt()`) are on
no covered path. (Theorem 44's pending checks have their own capability,
`pending_aborts()`, which no witness of this theorem holds.)
`arena::alloc_sets` has no check (`global layout SetTable`,
`arena.rs`, gives the size). `print::Sink::flush` has no check:
`Sink`'s type invariant states the report buffer's bound
(`len < 1024`), which also puts the NUL `flush` stores at `buf[len]` in
bounds, and `Sink::write_str` (verified) keeps it.

**Why the capability can be trusted.**
- Every abort goes through `abort_fmt` (`sys/abort.rs`): `abort`,
  every `-> !` helper, and the `#[panic_handler]`
  (`lib.rs`) that a `panic!` in a trusted body reaches. `abort_fmt`
  and `abort` require `may_abort()`; `abort_integrity` (verified, a call
  of `abort_fmt` after the module-private `relabel`) and `check`
  (trusted, an `assert!`) require `may_abort_corrupt()`, or `c` for
  `check`. `die`, `exit_group` and `trap` (the process's end) require
  `may_abort()` too. Two more calls of `abort_fmt`, verified the same
  way, take narrower capabilities that no witness of
  this theorem holds (theorem 44): `abort_kernel`
  (`may_abort_kernel()`) and `abort_pending` (`may_abort_pending()`).
- Every function that can reach an abort names `may_abort()` or
  `may_abort_corrupt()` in a `requires` or loop-invariant clause.
  Abort-site specs that the caller can discharge say so:
  `RunState::from_u8` and `HugeDesc::state` (`huge/mod.rs`)
  require `v <= 2` (`spec_state() <= 2`) with no abort capability, which
  the huge lock invariant (`desc_ok`) gives `Huge::lookup`;
  `assert_flushed` and `assert_refill_empty` (`heap/mod.rs`) require
  `may_abort() || *len == 0`; `huge_usable_pages` requires
  `may_abort_corrupt()` or its condition, and `arena_at` its condition
  alone.
- Every trusted body that can abort requires a capability: those that
  call an abort or a function that requires it (`background::spawn`,
  `call_throw_bad_alloc`) and those that `assert!` or
  `panic!` (`check`, `assert_flushed`, `assert_refill_empty`;
  `assert_not_home`'s `assert_ne!` is its `requires`).
  `sys::tests::trusted_aborts_require_a_capability` checks the
  `external_body` functions whose text shows an abort, a panic macro or
  an `unwrap` (not an index panic) for the capability.
  `ClassState::owner_locked` requires `may_abort_pending()`,
  which no witness of this theorem holds.
- `may_abort` is closed and `integrity_aborts` uninterpreted, so a verified function
  can prove `may_abort()` only from its own `requires` (or a loop
  invariant that carries it), or from `grant_aborts`, and
  `may_abort_corrupt()` only from those or `integrity_aborts()`, which
  only the verification-only witnesses require (this theorem's,
  theorem 8's fast-path witness and theorem 44's two direct-free
  witnesses).
- `grant_aborts` is called only in `extern "C"` bodies (the
  `verified_export_pair!` macro, `api/mod.rs`, and every hand-written
  export) and in the `#[panic_handler]`, which the panic
  runtime calls and no Rust code does; never in `abort_fmt` or another
  `-> !` helper, unlike `grant_syscalls`.
  `sys::tests::abort_grants_only_at_roots` (`sys/mod.rs`) checks the
  grants' sites; `abort_capabilities_only_as_clauses` checks that
  `may_abort*` appear only as `requires`/invariant clauses and
  `integrity_aborts` only in the witnesses' `requires`,
  `narrow_aborts_only_at_their_check_sites` that the integrity checks are
  only in the functions listed above, and
  `trusted_aborts_require_a_capability` the trusted bodies.
  Outside tests no Rust code calls an `extern "C"` function of the
  crate.
- `mallctl`'s readers are named by the `Reader` enum and called by the
  verified `call`, a `match`; the statement names the reads by reader,
  not by subtree. That every key under `opt`, `arenas` and `stats` is
  covered (no `thread.*` reader under them) is read off the static tree,
  and `mallctl::tests::read_only_subtrees_are_roots_4_5_and_7`
  (`api/mallctl.rs`) checks it at run time: it checks each subtree's
  reader count (`KEYS.len()`, 8, 35).

**Premises beyond the common ones.**
- The `mallctl` pointers keep their C contract (`vspec::cbuf`): the
  witnesses take it as the entry points `mallctl` and `mallctlbymib` do,
  each as the token of its call (`cc`, `cb`: `CCall::fresh` of the
  call's `CEntry`; one witness per call, so none holds two), which only the export wrappers mint (the private
  `axiom_c_call` of `mallctl.rs`'s `exports`); `mallctlnametomib`'s
  token covers `name`, and `mibp` with `miblenp` (`CEntry::NameToMib`,
  `CArg::Counted`: `*miblenp` `size_t`s at `mibp`), and
  `mallctlbymib`'s its `mib` too (`CArg::Words`: `miblen` `size_t`s).
  Nothing is stated for a name, MIB or buffer that breaks it: longer
  than `isize::MAX` bytes, wrapping the address space, or in challoc's
  own memory other than a counter it publishes read as such
  (`cbuf::c_ro_apart`); nor for an overstated `*miblenp` or a `mib`
  shorter than `miblen`, even where challoc fails the call without
  reading it (`miblen > MAX_MIB`, a capacity below the MIB's length).
- Init is complete. `shared()` (`api/mod.rs`) runs init the first
  time, and init aborts (`init_slow`: re-entrant init; `init`
  and below: a metadata region too small or a refused table
  commit, a failed reservation, a region with less than `MAX_SLAB` of
  slack (`slack_broken`, `layout/regions.rs`), `getrandom` or
  page-size probe, ...), so `shared()` requires
  `may_abort()`. The witness takes `shared: &Shared` with `shared.wf()`
  and calls what `nallocx`, `mallinfo`, `mallinfo2`, `challoc_features`,
  `mallctl` and `mallctlbymib` call after `shared()`. A well-formed
  `Shared` is not a proof that init returned (it is well formed inside
  `init`, before `READY` is published); the entry points reach that code
  only after `shared()` returned. Inside the readers the local
  `mallctl::shared` is verified: it requires
  `marks_ready(*mark)`, the witnesses' `ReadyMark`, which `ctl` passes
  through `call` to every reader that reads the state, and it reads
  through `api::shared_marked`, whose load of `STATE` is `READY` under
  the mark, so its init path is dead and it never aborts.
  So the witnesses' `shared.wf()` is the theorem's one assumption
  about init here: the readers add none.
- The trusted bodies these calls reach do not panic (read from their
  bodies): the futex wait and wake (`sys/mod.rs`; their
  results are discarded, so `EAGAIN` or `EINTR` loops) under
  `RawLock::lock`/`unlock`, `LockGuard::drop` and `deref`, the static
  projections, `shared_base` and `shared_deref`, the `STATE`
  wrappers (`state_load`, `state_claim`, `state_publish`: one bare
  atomic operation each), the `LINE_STATE` wrappers (`line_load`,
  `line_compare_exchange`, `line_store`) and the deferred record's
  phase wrappers (`vspec::byte::load`, `compare_exchange`, `store`: one bare
  atomic operation each), `ClassTable::table`, `class_entry`,
  the watermark's load `vspec::atom::load_usize` (the registry is read
  through `Arenas::get_existing` and the verified `arena_ref`),
  `Config::get` and `KEYS`, `vspec::ptr::ref_ptr`
  (the address of `copy_prefix`'s value; the accesses to the caller's
  memory verify over `vspec::cbuf` and the `core` specs of
  `vspec::ptr`), the kernel fact `axiom_read_count` (`sys::read`'s
  count bound, which keeps the cgroup reader's slice in bounds),
  `fig_add`, `fig_mul` and `fig_add64` (`policy::empty_bytes`' sums),
  and the `core` functions vstd and `vspec` specify.
- The shipped release profile: overflow checks and debug assertions
  off. With them on (the dev profile the tests use), the `+` of
  `fig_add` and `fig_add64` (`empty_bytes`' sums among them) and the
  `debug_assert!`s on these paths can panic.
- The integrity checks above hold: a failed one means corrupt allocator
  state, not a bad argument.

**What it does not cover (paths that can abort).**
- `malloc_trim` (`api/libc.rs`) requires `may_abort()`, by two
  paths. `Heap::flush_all` returns the thread's cached slots, and the
  release aborts on a double free that sat in the cache
  (`straddling_double_free`, `heap/flush.rs`; the class release's
  `double_free`/`invalid_free`), the program's earlier invalid free
  detected late. `purge::purge_all` (`purge.rs`) discards through
  `discard`, which aborts when `madvise` fails.
- The first call of `nallocx`, `mallinfo`, `mallinfo2` or
  `challoc_features`, and of `mallctl` or `mallctlbymib` with a name or
  MIB that resolves (both resolve before `super::shared()`), runs init,
  which may abort (above). `get_defrag_hint`, `malloc_trim`,
  `malloc_usable_size` and `mallctlnametomib` never run init.
- `malloc_usable_size` of a non-NULL pointer aborts on a pointer that
  is not an allocation (by design; row 9), and before init
  (`free_before_init`).
- mallctl's writes and operations (`epoch`, `arena.<i>.*`,
  `thread.*`, `tcache.*`, `arenas.create`/`lookup`, ...), and the
  `thread.*` reads (they may create the thread's heap), require
  `may_abort()`.

**Where the statement is weaker than the code.**
- The witnesses replicate the post-`shared()` calls of `nallocx`,
  `mallinfo`, `mallinfo2`, `challoc_features`, `mallctl` and
  `mallctlbymib`, not those functions (whose `shared()` call needs the
  capability): that their bodies make no other call is read off the
  code, as in theorem 8.
- "Never aborts" is about the verified source: the witnesses also hold
  `may_syscall()`, and a fault (a bad caller pointer for `mallctl`'s
  `oldp`/`newp`, or a `PROT_NONE` page) is outside it, as is reaching
  `SIGABRT` rather than hanging, or termination (the lock waits).
- Corrupt state is not excluded for `mallinfo` and `mallinfo2`: the
  `large` word's check may fire; the theorem names it rather than
  proving it unreachable (above: the word has no honest invariant that
  excludes it). The seven `stats.cgroup.*` reads reach no check: that the
  record is published after a waiting discovery is proved from the
  phase's invariant (`sys::cgroup::PhasePred`), over the trusted
  `vspec::byte` wrappers (partly checked by Kani and loom,
  `verus/ledger/src/vspec/byte.rs.md`) and the fork
  child's `axiom_cell_orphaned`. Its residue (a fork from a signal
  handler on the discovering thread) is outside the theorem: run one
  after the other, the two discoveries rewrite a published record
  without an abort, but run concurrently (the child has started another
  thread) they race on the cell, as that axiom's doc says.
- Source tests check the grant sites (`abort_grants_only_at_roots`),
  the integrity-check callers
  (`sys::tests::narrow_aborts_only_at_their_check_sites`) and the
  trusted bodies (`sys::tests::trusted_aborts_require_a_capability`).

Machine code: `Arenas::count`'s load and `Arenas::create`'s store are
the bare `core` calls (the `vspec::count` wrappers are
`#[inline(always)]`), and the count's ghost field has no bytes.

## 33. Lock order is acyclic

challoc cannot deadlock on its own locks: every allocator lock has a
fixed rank, and every blocking acquisition Verus checks ranks above
each allocator lock its thread holds.

**Statement.** Every allocator lock has a fixed rank, and a blocking
acquisition ranks above every allocator lock its thread holds, so no
set of threads can each wait for a lock the next one holds (around such a
cycle the ranks would climb back to where they started). There is no
exception: `fork_prepare` takes the slab-set locks after the slab table
locks, in the run-time order (below).

```rust
// lock.rs: the ranks, lowest first (spec fns).
rank_tcaches()                 0x100_0000              explicit-cache table lock
rank_arenas()                  0x200_0000              arena creation lock
rank_stats()                   0x280_0000              process statistics lock
rank_class(i, c)               0x300_0000 + 0x100 i + c   class c of arena i
rank_table(c)                  0x400_0000 + c          slab table of class c
rank_sets(i)                   0x500_0000 + i          slab sets of arena i
rank_huge(), rank_meta()       0x600_0000, 0x700_0000
rank_arena_stats(i)            0x900_0000 + i
rank_state()                   0xA00_0000              background thread state

// lock.rs: a guard acquisition.
pub fn lock<'a>(&'a self, Tracked(ord): Tracked<&'a mut LockOrder<'_>>,
                Tracked(under): Tracked<&mut LockOrder<'a>>) -> (g: LockGuard<'a, T>)
    requires old(ord).allows(self.rank()),          // ord.top() < rank
    ensures  *final(ord) == *old(ord), final(under).top() == self.rank(), ...

// lock.rs: a raw acquisition (the fork handlers).
pub fn lock_held(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (t: Tracked<LockHeld>)
    requires old(ord).root(), old(ord).allows(self.rank()),
    ensures  t@.held() == *self, final(ord).root(), final(ord).top() == self.rank(),
```

A `LockOrder` (`lock.rs`, ghost, fields private to `lock.rs`) bounds
the ranks of the locks its owner holds (`top()`). `Lock::lock` borrows
the caller's token for the guard's lifetime `'a` and hands out another
at the lock's rank, also of lifetime `'a`, for the code under the guard;
Verus's lifetime checker rejects any use of the caller's token while the
guard or the handed-out token is in use (checked on a scratch file: a
second `lock` with the frozen token fails `E0499`). A handed-out token
used after its guard is dropped bounds a lock its owner does not hold,
which only forbids more. Raw holds outlive any guard, so `lock_held`
needs a root token (`root()`: from `axiom_fork_entry`; never handed out
under a guard) and
raises it in place. `RawLock::lock` itself requires `lock_rs_only()`, a
`true` whose body is closed, so verified code outside `lock.rs` cannot
acquire except through these two. Every function that may take a lock
takes its caller's token (`Tracked<&mut LockOrder>`, `requires
old(ord).allows(rank_x())` for the lowest rank it takes, `ensures
*final(ord) == *old(ord)`), so the bound follows the call chain. That
includes the ten `meta::Meta` methods that may take the metadata lock
(`alloc`, `alloc_raw`, `alloc_zeroed`, `free`, `reserve`,
`reserve_table`, `commit`, `reserve_committed`,
`reserve_committed_raw` and `stats`, `meta.rs`): each requires
`old(ord).allows(rank_meta())`.

The ranks are ghost fields of `RawLock` (`#[cfg(verus_keep_ghost)]`: a `Ghost`
field breaks `repr(transparent)`, which ordinary builds keep), set by
`RawLock::new(Ghost(rank))` / `Lock::new(v, Ghost(rank))`, and tied to
each lock by its owner's type invariant: `ClassState` (class `c` of
arena `arena`), `SlabTable`, `SetTable` (a slab-set rank), `Huge`, `Meta`,
`Arena` (sets and statistics), `Arenas`, `Tcaches`, and `Shared::wf`
(the process statistics lock). The fork accessors (`raw_lock`,
`stats_raw_lock`, `Arena::sets`, `Shared::*_lock`) state the rank.
**What is threaded** (every acquisition below is checked):
below):

| Path | Nesting checked |
|---|---|
| slow allocation: `alloc_slow_in` → `tick`, `Shared::arena`, `ensure_cache`, `alloc_direct`, `refill` → `Heap::claim` → `claim_with` → `plan` (class) → `next_slab` → `next_fresh_slab` → `take_group` (table) → `ensure_sets` → `Set::ensure` → `SetTable::ensure` (sets) → `Lazy::ensure` → `Meta::commit` (meta); `commit_run` (the run's `mprotect` and `install_guard` with no lock beyond the class lock, then the table for the guard count) → `commit_slab` (table; page groups above a slab, `thp:always` chunks) | Class(i,c) < Table(c) < Sets(i) < Meta; Arenas < Meta (`materialise` → `build` → `Arena::init`); the GC and decay of `tick` |
| slow free: `free_slow_in` → (`rehome_on_free` → `try_rehome` → `flush_arena` →) `flush` → `release_routed` → `release_owned` → `release_slab` → `dec_live` → `on_live_zero` (class), `reopen` (class); `Shared::free_direct` | class locks one at a time |
| huge: `Shared::alloc_huge` → `alloc_run` → `alloc_aligned` (huge; the lowest-fit `find` runs under it) → `alloc_desc`, `free_desc`, `Table::ensure`, `Set::ensure` (own store), `take` → `retire` (meta); `free_run` (huge) → `Table::ensure`, `retire` (meta) | Huge < Meta |
| cache flushes and routing: `Heap::flush_all` (`tcache.flush` through `Tcaches::flush`, `thread.tcache.flush` and `arena.<i>.purge` through `flush_caller`, `thread.tcache.enabled` through `set_tcache_enabled`) → `flush_arena` → `flush` → `release_routed` (class) and `free_caches_in` → `Meta::free` (meta); `gc` → `gc_arena` → `flush`; `set_arena` → `other_caches` (meta, when it creates a node) | class locks one at a time, then Meta |
| arena creation `Arenas::create`, heap creation `Heap::create` → `assign_home` (arenas) and `create_detached` (meta), explicit caches `Tcaches::create`/`destroy` (tcaches → meta / class, meta, huge) | Tcaches < Arenas, Meta, Class, Huge |
| purge `purge_all`, `purge_arena`, `decay` and `decay_arena` (through `decay_one`, `decay_huge`) → `Arena::decay_with`, `ClassState::decay_with`, `Huge::decay_with`, `on_tick`, `sample` → `policy::evaluate`; background `start` (state, then meta) and `main`; the lazy start: `on_tick` → `auto_tick` (`policy::empty_bytes`: class and huge locks, then state, then, on an allocation's tick, `start_requested`), `start_requested` (`start`, then state), `request` (state; from `ctl`'s `background_thread` write) | class, huge, state locks one at a time |
| statistics `epoch`, `collect` (`collect_into`, with its exact-stats ghost outputs) and `arena_stats_all_into`, under the process statistics lock held across the scan | Stats < Class(i, c) < Table(c), and Stats < Huge, Meta, ArenaStats(i), each released before the next is taken; `arena_stats_all_into`: Stats < ArenaStats(i) |
| statistics `collect_all`, `snapshot`, `arena_stats` (class → table, huge, meta, arena and process statistics), `mallctl` `ctl` and its named readers (`stats.*`, `stats.cgroup.empty_bytes`), `utilization`, `owner_of`, `get_defrag_hint`, `malloc_trim` | each lock with nothing above it held |
| `fork_prepare` (`api/mod.rs`) | Tcaches < Arenas < Stats < Class(i, c) (arenas ascending, classes ascending) < Table(c) < Sets(i) (arenas ascending) < Huge < Meta < ArenaStats(i) < State, through one root token; before them the deferred cgroup discovery's `policy::evaluate` from the same root token, each class lock with nothing above it held, all released on return (`*final(ord) == *old(ord)`) |
| `fork_child` after `fork_release` (under the lazy start: `policy::empty_bytes`, the inherited dirty bytes) | class and huge locks one at a time, from a token of its own |

`fork_prepare` proves it with loop invariants `ord.top() <
rank_class(i, 0)`, `< rank_class(i, c)`, `< rank_table(c)`, `<
rank_sets(i)`, `< rank_arena_stats(i)`; the arena indices it visits are
below `MAX_ARENAS` (`Arenas::count`, verified under its invariant
`NarenasPred`), so the `u16` arena indices of the arenas it finds
increase with `i`.

**The slab-set locks.** At run time `take_group` holds
Class(i,c) and Table(c) and takes Sets(i) (`SetTable::ensure` through
`ensure_sets`), so the ranks put Table below Sets. `fork_prepare` takes
the Sets locks after all the Table locks, in arena order, through
`lock_held` like every other lock, and `SetTable`'s type invariant
states its rank's range (`rank_sets(0) <= rank < rank_sets(0x1_0000)`).

**Premises beyond the common ones.**
- *Entry points hold no allocator lock* (`axiom_entry`, `lock.rs`;
  ledger category "allocator invariant"). The roots: `alloc_slow_in`,
  `free_slow_in`, `alloc_huge_in`, `Heap::free_huge`, `paths::free_direct`
  and the no-heap `alloc_direct` arms of `paths::alloc`,
  `alloc_declined`, `alloc_zeroed` (`heap/paths.rs`); `pop_compact`,
  `pop_compact_run`, `rebuild_run` (`heap/compact.rs`); the `mallocx`
  routes (`heap/mallocx.rs`); `libc::alloc_huge`,
  `malloc_trim` (`api/libc.rs`); `Huge::usable_size` (`huge/mod.rs`,
  called from the `api::libc` and `api::jemalloc` entry points and
  `Shared::zero_huge` on the free paths); `Shared::check_size_hint`
  (`heap/shared.rs`, the sized frees' slow paths); `ctl`,
  `utilization`, `owner_of`, `arena_stats`, `arena_figure` (for its
  `stats::Scratch` record), and the named readers
  `cgroup_empty_bytes`, `tracked` (the seven `stats.cgroup.*` readers'
  cgroup discovery) and
  `stats_allocated` to `stats_retained` (`api/mallctl.rs`; `mallctl`
  calls them through its node table); `get_defrag_hint`; `mallinfo2`
  (`api/glibc.rs`; its lock-order premise is listed in `axiom_entry`'s
  doc); the report call sites (`api/print.rs`), and the same
  call in theorem 32's verification-only witness
  `theorem_read_only_never_aborts` (`api/mod.rs`);
  `Shared::init` (no lock exists yet); `create_heap_for_thread` and
  `heap_destructor` (`heap/thread.rs`); the background thread's `main`;
  `fork_prepare`; and `fork_child` once
  `fork_release` has handed back every token of the fork hold. The fast paths take no lock (theorem 9), and no
  allocator entry point is re-entered while a lock is held: allocator
  code calls nothing that may allocate with a lock held (theorem 16 for
  the reports' calls out, by inspection elsewhere), and a signal
  handler that allocates is outside the C contract (the fork handlers
  block signals).
- *The fork prepare handler's root* (`axiom_fork_entry`, `lock.rs`):
  the one root token; every other entry point's `axiom_entry` token is
  not a root.
- *Functions that state the lowest rank they take*: `assign_home`,
  `Meta::commit`, `Heap::destroy`, `Set::ensure`, `Heap::set_arena`
  (the call `thread.arena` makes) and `policy::empty_bytes` (an index
  loop over `Arenas::get_existing`) are verified;
  `purge::background::state`, which names `STATE`'s rank, verifies over
  the static's postcondition, which its initialiser proves.
- *Ranks set in constructors*: none is trusted. `Tcaches::init` is
  verified (it writes the lock field with a lock created at the rank
  the type invariant states), and so is `Arenas::init`
  (`axiom_arenas_assemble` requires the creation lock's rank that
  `Arenas::type_inv` states).
- *The mutex*, as for fork safety: `lock` returns only once this thread
  holds the lock.

**Where the statement is weaker than the code.**
- Partial correctness and per-thread: the theorem rules out circular
  wait among allocator locks, not a lock held forever (a holder that
  never returns), and it says nothing about the futex itself.
- The ranks of a held set are bounded by `top()`, not tracked exactly;
  a token does not know which locks its owner holds.
- Code outside `verus!` and trusted bodies are covered by the premises
  above, not checked.
- `fork_release` takes no lock and is not part of the statement; the
  release order is unstated.

## 34. Balanced arena assignment (bound counts at their invariants)

A new thread's heap goes to the least loaded automatic arena, preferring
a built one at equal load, then the lowest index (jemalloc's
`arena_choose_hard` order), and each arena's bound count is exactly the
number of heaps holding a ticket for it, modulo `2^32`. Upstream issue
#43 (arena spread) is about which arena a new thread lands on; this
states both halves of that choice: the count it reads, and the pick it
makes from the counts.

**Statement.** `Arenas::assign` returns an automatic arena whose
`(bound, unbuilt)` key is the least of the keys it read under the
creation lock, the first one with that key (ties go to a built arena,
then to the lowest index), together with a fresh ticket for that arena.
Every bound count is, at every instant, the number of tickets out for its
arena, as a `u32` wraps it (an atomic invariant); `assign` and `bind` add
one ticket and one count, `try_bind_idle` does so only when its
compare-and-swap moves the count from 0 to 1, `unbind` takes the heap's
ticket back with one count, and a thread heap holds exactly the ticket
for its home arena while it is counted there (`Heap::bound`), none
otherwise.

```rust
// arena.rs
pub fn assign(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (u16, Tracked<Homed>, Ghost<Seq<(u32, bool)>>))
    requires
        crate::sys::may_syscall(),
        self.wf(),
        old(ord).allows(rank_arenas()),
    ensures
        (r.0 as int) < self.spec_auto(),
        self.ticket_of(r.1@, r.0 as int),
        self.spec_auto() == 1 ==> r.0 == 0,
        self.spec_auto() > 1 ==> picks_least(r.2@, self.spec_auto(), r.0 as int),
        *final(ord) == *old(ord),

// arena.rs
pub open spec fn key_lt(a: (u32, bool), b: (u32, bool)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && !a.1 && b.1)
}
pub open spec fn least_of(snap: Seq<(u32, bool)>, r: int) -> bool {
    &&& 0 <= r < snap.len()
    &&& forall|j: int| 0 <= j < snap.len() ==> !key_lt(snap[j], snap[r])
    &&& forall|j: int| 0 <= j < r ==> key_lt(snap[r], snap[j])
}
pub open spec fn picks_least(snap: Seq<(u32, bool)>, auto: int, r: int) -> bool {
    &&& least_of(snap, r)
    &&& snap.len() <= auto
    &&& snap.len() < auto ==> snap[r] == (0u32, false)
}

// arena.rs: the invariant of bound count i (one per index, Arenas::wf)
impl InvariantPredicate<BoundKey, BoundVal> for BoundPred {
    open spec fn inv(k: BoundKey, v: BoundVal) -> bool {
        &&& v.0.id() == k.id                            // the counter's permission
        &&& v.1.id() == k.loc                           // the arena's ticket set
        &&& forall|t: int| v.1@.contains(t) ==> t < v.2@
        &&& v.0.value() == wrap32(v.1@.len() as int)    // count == #tickets mod 2^32
    }
}

// arena.rs
pub fn unbind(&self, index: u16, Tracked(t): Tracked<Homed>)
    requires crate::sys::may_abort_pending(), (index as usize) < MAX_ARENAS, self.wf(), self.ticket_of(t, index as int),
    // body: assert(held >= 1 && prev == wrap32(held));
    //       assert(prev == 0 ==> held % 0x1_0000_0000 == 0);   (held: the tickets out)
pub fn bind(&self, index: u16) -> (r: Tracked<Homed>)
    requires index < MAX_ARENAS, self.wf(),
    ensures self.ticket_of(r@, index as int),
// arena.rs (Heap::try_rehome's bind)
pub fn try_bind_idle(&self, index: u16) -> (r: (bool, Tracked<Homed>))
    requires index < MAX_ARENAS, self.wf(),
    ensures r.0 ==> self.ticket_of(r.1@, index as int),
    // body: load_bound, then bind_if_idle: compare_exchange(0, 1) inside
    //       the invariant, a fresh ticket only on Ok (a placeholder,
    //       no_ticket, of a fresh set otherwise)

// heap/mod.rs, a conjunct of Heap::wf (and of Heap::keeps)
pub closed spec fn homed_ok(&self) -> bool {
    &&& self.bound == self.homed@ is Some
    &&& self.homed@ matches Some(t) ==> homed_on(t, self.arena as int)
    &&& self.rehome ==> self.bound
}
```

`snap` is the ghost sequence of keys `assign` read, `snap[i]` arena `i`'s
`(bound[i], table[i] is null)`. A `Homed` ticket (`arena.rs`) is a
`GhostSingleton<int>` of its arena's ticket set and the ghost index of the
arena; `homed_on(t, i)` says it is arena `i`'s in the process's
registry (`bound_loc`: the ticket-set locations of
`Shared::built_at(Shared::storage()).arenas`). `Shared::wf`
(`heap/shared.rs`) carries `arenas.wf()` (`arena.rs`:
between 1 and `MAX_ARENAS` automatic arenas, every invariant present over
its counter in namespace `bound_ns`, ticket sets at distinct locations)
and `arenas.is_process()` (its invariants are the process
registry's, so there is one authority per ticket-set location).

In words:

- **The pick.** With one automatic arena `assign` returns 0 without the
  lock. Otherwise it takes the creation lock and reads each automatic
  arena's key in index order; the arena it returns has no key read below
  its own, every arena before it has a key above it, and when it stopped
  before the last automatic arena it is because its key was `(0, false)`,
  an idle built arena, below which there is nothing. So among the
  arenas whose count it read, it took the least loaded, preferring a
  built one at equal load, then the lowest index (jemalloc's
  `arena_choose_hard` order).
- **The counts.** Every write of a bound count after init is `count_in`
  (`assign` and `bind`, with a fresh ticket), `bind_if_idle`
  (`try_bind_idle`'s compare-and-swap from 0 to 1, with a fresh ticket
  when it succeeds and no change when it fails) or `unbind` (with
  the caller's ticket for that arena, which the ticket set contains, so
  the count it decrements counted it), each inside the invariant, so at
  every instant each count is its arena's ticket count mod `2^32`. The
  counts `assign` reads go through the invariant too (`load_bound`,
  which asserts the value read is the ticket count there), as
  do `bound` and `try_bind_idle`'s first load; that is proved
  at the read and not exported, since no `ensures` can name the instant
  of a read.
- **Who holds the tickets.** `Heap::create` (`heap/mod.rs`) gets the
  ticket with the arena from `assign_home` (verified) and
  either gives it back (`unbind`, when the metadata region is exhausted)
  or returns it with the heap; `create_heap_for_thread`
  (`heap/thread.rs`) installs it in the heap's ghost `homed` field
  with the permissions, which `Heap::lemma_fresh_wf` (`heap/mod.rs`)
  turns into `homed_ok`; when `ThreadKey::set` refuses the heap
  (theorem 15's heapless path) it drops the ticket and hands the heap to
  `destroy`, which unbinds it. `set_arena` (`thread.arena`) hands the
  old home's ticket to `unbind` and stores `bind`'s ticket for the new
  home. `Heap::try_rehome` (the late
  move of a young heap, `rehome` set, onto an idle automatic arena)
  does the same with `try_bind_idle`'s ticket for the new home, only
  when it won the compare-and-swap; a lost one hands back a placeholder
  that is dropped, and the heap stays where it is. `destroy` (
  verified) unbinds a counted heap's home arena, before its flush.
  No other code makes or spends a ticket, so the tickets out for
  arena `i` are the counted heaps homed on `i`, plus a `create` in
  flight between `assign` and `create_heap_for_thread`.

With theorems 32 and 33: `assign` takes the creation lock with a
`LockOrder` token that allows `rank_arenas()`, which `assign_home` and
`Heap::create` take from `create_heap_for_thread`'s `axiom_entry` root
and pass down; the counter operations take no lock. `unbind` and its
abort (`unbind_unbound`) require `may_abort_pending()`, which `create`
carries and which `set_arena`'s and `destroy`'s `may_abort()` and
`try_rehome`'s `valid_aborts()` each imply.

Machine code: the counter wrappers are `#[inline(always)]` around the
`core` calls, the tickets, the tokens and the snapshot are zero-sized,
and the tuple comparison stays `core`'s (`key_less`, trusted, below).

**How it is proved.** The loop of `assign` carries `least_of(snap,
best)` and `best_key == snap[best]` once a key is read, with `snap.len()`
the loop index except at the `break` (`invariant_except_break`), and its
`ensures` gives the `(0, false)` case at the `break`. `count_in` and
`unbind` open the arena's invariant around the counter operation:
`new_ticket` (`arena.rs`) inserts the bound `v.2` as a fresh ticket
and raises it, `delete_singleton` removes the caller's (which `agree`
places in the set, so the set shrinks by one), and `lemma_wrap_step`
moves the wrapped count by the same one. `Shared::init` gets
`arenas.wf()` from `Arenas::init` and `is_process()` from
`Arenas::lemma_is_process` (`init` builds the process's
`Shared`).

**Findings.** None: the code matches the statement. The tests
`automatic_arenas_are_assigned_least_loaded_and_built_lazily` and
`heap_churn_reuses_the_vacated_arena` (`heap/tests.rs`) exercise the
order and the counts; `rehoming_only_takes_an_idle_automatic_arena`
and `a_young_heap_moves_onto_the_idle_arena_it_frees_into` exercise
`try_bind_idle` and the moved binding.

**Premises beyond the common ones.**
- `vspec::count` (`vspec/count.rs`, the `vspec::word` pattern): a
  `CountPerm` is the counter with its `id` (`count_id`), and `fetch_add`,
  `fetch_sub` (wrapping), `load` and `compare_exchange` (the strong form:
  it fails only on a different value, by C++20's semantics) are one
  atomic operation on it, sequentially consistent in the model (the
  orderings, all `Relaxed`, are ignored, as for the bitmap words). The
  four are trusted, partly checked by Kani and loom
  (`verus/ledger/src/vspec/count.rs.md`).
- `Arenas::init` (verified) ensures `wf` of the
  registry it writes: each bound count is written with a permission of
  value 0 from `vspec::count::new` (verified, its identity a ghost one;
  the only place a `CountPerm` is minted, one per counter; `Policy::new` mints the
  position's there too), and its invariant holds that
  permission and a fresh ticket set emptied of the `{0}` that shows its
  location is its own (`fresh_sets`, `fresh_invs`). Its field writes go
  through the permissions `axiom_arenas_layout` (rustc's field order,
  checked by the build) gives and `axiom_arenas_assemble` joins, as for
  `Arena::init`. The arena count's invariant (`NarenasPred`, the type
  invariant's `count_inv`) holds a permission `vspec::count::new` mints with
  the clamped automatic count. The spare storage of a failed build
  (`spare`, null at init) and its invariant (`SparePred`, the type
  invariant's `spare_inv`) are two more fields of the layout and the
  assembly (offsets `AR_SPARE`, `AR_SPARE_INV`); `spare` is vstd's
  `PAtomicPtr`, whose permission vstd mints, and holds no bound count.
  Nothing writes a bound count outside the wrappers afterwards (by
  inspection).
- `key_less` (`arena.rs`, `external_body`, discharged by Kani,
  `verus/ledger/src/arena.rs.md`): `core`'s `<` on `(u32,
  bool)` is lexicographic with `false < true`. vstd specifies no order on
  `bool`; a verified spelling of the comparison changes
  `create_heap_for_thread`'s machine code.
- The fresh heap's fields, read back: one linear ghost token,
  `FreshHeap` (`heap/fresh.rs`, a tracked struct, not `Copy`), holds the
  permission to the heap's
  128 bytes of plain fields (`head()`, `heap_head`) and records what
  the field writers stored in `shared`, `arena` and `bound`. `init_fields`
  (verified) takes the head permission that `create_detached`
  splits off the heap's block (with the block's `MetaClaim`, which it
  drops), stores the fields through permissions carved from it (one
  per field up to `last_gc`, then one `HeadRest` for the rest, so the
  stores keep their alignment and machine code), forgets the values
  and returns the token holding the permission, with `shared`, `arena` and
  `bound` set to what it stored (`shared` is `shared_ptr`'s, at
  `Shared::storage`). That the token's `shared`, `arena` and `bound`
  are the fields' values is not a Verus fact: the token's fields are
  private to `heap::fresh` (`init_fields`'s module), so nothing else
  builds, takes apart or changes one but `mark_bound` (and
  `mark_rehome`, which stores `rehome` through the token's permission
  and keeps what the token records), both verified.
  `mark_bound` (`heap/fresh.rs`) needs the permission and hands back
  one to the same bytes (`heap_head`), and records `bound` (its body
  stores `true` there through a permission carved from
  the token's); `tcaches::heap_new`
  (`heap/tcaches.rs`) uses the token up (through
  `HeapPerm::assemble`, below), giving the permission to the heap's,
  whose `others`, `shared`, `arena` and `bound` are what it records.
  `create_detached` and `create` pass it on. The token carries the
  bare `PointsToRaw`, so one linear object, not two, stands for the
  fresh heap.
  A token is valid while its heap is live: `destroy` does not take it,
  so one kept past `destroy` would be stale for a later heap in the same
  block; no code keeps one (`create_heap_for_thread` uses it right after
  `create`, `Tcaches::create` drops it). `heap_new` is
  verified: the token is used up by the trusted
  `thread::HeapPerm::assemble`, which makes the heap's linear permission
  with those claims (and, from `FreshHeap::shared`, that its `shared`
  field is the process's `Shared`), and `heap_new` puts it in the
  thread's slot token and borrows the heap from there.
- `Heap::destroy` (verified; it takes a `tcaches::HeapUse` token and
  requires `h@.addr != 0`) unbinds a counted heap's
  home arena with the ticket the heap holds (`Heap::homed_ok`), first,
  and clears the heap's `bound` and `rehome` with it, so the dying heap
  holds no ticket and is not counted for the rest of `destroy` while
  another heap may be homed on (and win, through `try_bind_idle`) the
  same arena: "the tickets out are the counted heaps" holds throughout.
- `homed_ok`'s `rehome ==> bound`: a new thread heap is
  counted, so it holds whatever `mark_rehome` (`heap/fresh.rs`,
  verified) stored;
  an explicit cache, assumed `wf` (`heap_at`), rests on `init_fields`
  storing `false` (verified) and on `create` being the one writer that
  sets `rehome` (by inspection; `set_arena` and `try_rehome` clear
  it). `same_shape`, `caches_same`, `updates` and the field lists
  of `claim` and the accounting lemmas keep `rehome` too.
- Every heap a verified path is handed is `Heap::wf`, with
  `homed_ok` (common premises: `TlsSlot::entry`, `heap_at`): a thread
  heap's ticket is the one `create_heap_for_thread` installed, kept by
  every verified writer (`keeps`, `same_shape`, `caches_same` and
  `updates` keep `bound` and `homed`); an explicit cache is not
  counted and holds none. One reference at a time (their safety
  contracts) is what keeps a ticket from being spent twice.
- `axiom_shared_built` (`heap/shared.rs`) also says the built
  `Shared`'s `arenas` is the value its permission holds.

**Scope.** Thread heap creation (`heap_for_thread`), `thread.arena`
(`set_arena`), the late move of a young heap (`try_rehome`, on the
`FOREIGN` slow path of `free`) and thread exit (`destroy`, through
`heap_destructor`).
Explicit caches (`tcache.create`, `Heap::create_detached`) and explicit
arenas (`arenas.create`) take no part, as in the code: they are never
counted.

**Where the statement is weaker than the code.**
- The keys are read one at a time, not as one snapshot: `bind` and
  `unbind` take no lock, so a count can change between two reads (and
  the `table` loads are plain `core` loads, so "unbuilt" is as read).
  "Least" is over the values read, each the arena's ticket count at its
  own read, which `assign`'s `ensures` does not restate.
- Counts are exact only modulo `2^32`. `unbind` asserts that its abort
  (`unbind_unbound`, a count of zero) happens only when the tickets out
  for the arena, at least one, are a multiple of `2^32`; that fewer than
  `2^32` heaps are ever homed on one arena is not proved (each heap is a
  metadata block, so the metadata region bounds it in fact), so the
  abort is not proved unreachable outright.
- The ticket count includes a heap between `assign` and its
  installation, and `destroy` gives back exactly the heap's
  ticket; "number of heaps homed on `i`" is the number of tickets, tied
  to heaps through `homed_ok`, which, like the rest of `Heap::wf`, is
  assumed of every heap handed to verified code rather than carried
  across calls.
- With one automatic arena (`spec_auto() == 1`) nothing is read; the
  statement is `r.0 == 0`.
- `Arenas::bound` and `get_existing` (the exit purge's "last heap left"
  test in `destroy`) are outside: they read a count once, unordered with
  the other threads' binds and unbinds, so the statement says nothing of
  what the test sees.

## 35. Utilization and defrag hint are exact (over the values read)

The jemalloc defragmentation queries that Valkey and KeyDB use report
exact figures: `experimental.utilization` returns a pointer's free
slots, slots and bytes of its slab (or the huge run's usable size), and
`get_defrag_hint` returns 1 exactly when design 13.5's move rule holds,
both over the values the call loaded.

Valkey's and KeyDB's defragmenters depend on these shapes
(`allocatorShouldDefrag` divides `size` by `nregs` and asserts a bin
size; `activeDefragAlloc` moves an object when the hint is 1).

**Statement.** Over the values each call loaded (a ghost record its body
fills in from the loaded locals; Verus does not constrain the loads,
so "exact" is over those values, as in layer 1 of theorem 22):
`experimental.utilization.query` (`utilization`, `api/mallctl.rs`)
and every triple `batch_query` stores report, for a live slot
(bitmap bit set, slab below the watermark) of a bin class, `(nslots -
live, nslots, nslots * size)`, plus, verbose and with the owning arena
built, `bin_nregs = curslabs * nslots`, `bin_nfree = bin_nregs -
curregs` and the lowest partial slab's address as `slabcur_addr` (null
when there is none); `(0, 1, size)` for a live slot of a class above
`BIN_MAX`; `(0, 1, usable)` for the start of an active huge run; zeros
for everything else. `get_defrag_hint(p)` (`api/jemalloc.rs`; exported as
`je_get_defrag_hint`) is 1
exactly when the design 13.5 rule holds over its reads: the allocator is
ready, `p` is a live slot of a slab class whose slab is not full (`live
< nslots`), the owning arena is built, the slab is not that arena's
lowest partial slab of the class (jemalloc's `slabcur`), and `live *
(curslabs - 1) <= curregs - live(slabcur)` (both differences clamped at
zero; `live * curslabs <= curregs` when the current slab's descriptor
read as uncommitted or there is no partial slab); 0 otherwise.

Each function proves this in its body: at every `return` it asserts
`util_read`/`hint_read` of the record built from what it loaded. Its
`ensures` exports it as an existential over the record, so what a caller
can use is weaker (see "Where the statement is weaker than the code").

```rust
// api/mallctl.rs
pub fn utilization(shared: &Shared, p: *const c_void, verbose: bool) -> (u: Utilization)
    requires crate::sys::may_syscall(), shared.wf(),
    ensures
        exists|rd: UtilRead| #[trigger] util_read(crate::layout::process_regions(), p as usize, verbose, rd, u),

// api/mallctl.rs (opaque, revealed in utilization; abridged; c = class_table()[class])
pub open spec fn util_from(rg: Regions, p: usize, verbose: bool, rd: UtilRead, u: Utilization) -> bool {
    match rd.kind {
        Kind::Slab { class, .. } =>
            if !rd.allocated || !rd.committed { triple_is(u, 0, 0, 0) && no_bin(u) }
            else if c.size > BIN_MAX { triple_is(u, 0, 1, c.size) && no_bin(u) }
            else {
                &&& triple_is(u, sat_sub(c.nslots, rd.live), c.nslots, c.nslots * c.size)
                &&& if verbose && rd.found {
                    &&& u.bin_nregs == curslabs * c.nslots
                    &&& u.bin_nfree == u.bin_nregs - rd.curregs
                    &&& u.slabcur_addr@.addr == match rd.lowest {
                        Some(s) => rg.class_base[class] + (s << c.lg_slab), None => 0 }
                } else { no_bin(u) }
            },
        Kind::Huge { page } => match rd.run {
            Some(r) if r.start == page && r.state == RunState::Active =>
                triple_is(u, 0, 1, r.usable) && no_bin(u),
            _ => triple_is(u, 0, 0, 0) && no_bin(u) },
        Kind::Invalid => triple_is(u, 0, 0, 0) && no_bin(u),
    }
}
// util_read = classified(rg, p, rd.kind) (classify's
// postcondition), the bin_stats bounds on rd.st and rd.curregs
// (curregs <= curslabs * nslots), rd.lowest a slab of the class, and util_from.

// api/mallctl.rs (verified over vspec::cbuf; the util_triple precondition is checked at the call)
unsafe fn store_triple(out: *mut usize, i: usize, nfree: usize, nregs: usize, size: usize,
    Ghost(oldlen): Ghost<usize>, Ghost(q): Ghost<*const c_void>)
    requires
        (3 * i + 3) * 8 <= oldlen,
        util_triple(crate::layout::process_regions(), q as usize, nfree, nregs, size),
        c_buffer(out as *mut u8, oldlen as int), c_writes(out@.provenance),
// batch_query's loop body:
//     let q = query_ptr(ptrs, i, Ghost(io.newlen));
//     let u = utilization(shared, q, false);
//     store_triple(out, i, u.nfree, u.nregs, u.size, Ghost(oldlen), Ghost(q));

// api/jemalloc.rs (defrag_rule is opaque, revealed in get_defrag_hint)
pub fn get_defrag_hint(p: *const u8) -> (r: c_int)
    requires crate::sys::may_syscall(),
    ensures r == 0 || r == 1, exists|rd: HintRead| #[trigger] hint_read(p as usize, rd, r),
pub open spec fn defrag_rule(rd: HintRead) -> bool {
    &&& rd.ready
    &&& match rd.kind {
        Kind::Slab { class, slab, .. } => {
            &&& rd.allocated &&& rd.committed &&& rd.live < n &&& rd.found
            &&& rd.lowest != Some(slab as usize)
            &&& match rd.cur_live {
                Some(l) => rd.live * sat_sub(curslabs, 1) <= sat_sub(rd.curregs, l),
                None => rd.live * curslabs <= rd.curregs }
        },
        _ => false }
}
// hint_read: rd.ready ==> classified(process_regions(), p, rd.kind), the
// bin_stats bound on rd.st, rd.live <= u32::MAX, rd.curregs and
// cur_live nonnegative, and
// r == if defrag_rule(rd) { 1 } else { 0 }.
```

`UtilRead` (`api/mallctl.rs`) and `HintRead` (`api/jemalloc.rs`) are ghost
records of what the call read: `p`'s classification, the slot's bitmap
bit (`is_allocated`), the watermark test (`desc_if_committed`), the slab's
live count (`live`'s load in the hint; `nslots - nfree` in the query,
that is `nfree`'s load clamped at `nslots`), whether the arena the
descriptor names is built (`get_existing`), that arena's class counters
from one lock hold (`bin_stats`: `st`, `curregs`), its lowest partial
slab (`lowest_partial`), the huge descriptor at `p`'s page
(`Huge::lookup`, under the huge lock), and for the hint the amount
`curregs` lost to the current slab (`cur_live`: its live count clamped at
`curregs`).

In words:

- **The classification is exact.** `classified` restates
  `Regions::classify`'s postcondition; with `Shared::wf`'s separated
  regions a slot start is `Slab` of its class, slab and slot, and a
  `Huge` page is the page start it names. So "a live slot of a bin
  class" in the figures is `p`'s own class and slot.
- **The shapes are exact.** For each branch the figure is the stated
  function of the reads, with no slack: `nfree = nslots - live`
  (`SlabDesc::nfree` ensures `r <= class.nslots`), `nregs`, `size`
  and `bin_nregs` exact products, `bin_nfree` an exact difference
  (`bin_stats`' clamp keeps `curregs` within `bin_nregs`, so the
  `saturating_sub` never clamps), the slab address `slab_addr`'s. The
  `BIN_MAX` boundary is by size: `NBINS` ensures that the classes
  below it are exactly those whose `spec_class_size` is at most `BIN_MAX`
  (`layout/mod.rs`; `count_bins` proves it with
  `lemma_class_size_mono`), and `Utilization::ZERO`
  (`api/mallctl.rs`) ensures its zero figures.
- **`batch_query` stores exactly those triples.** `store_triple` requires
  its triple to be `util_triple` of `q`, the pointer `query_ptr` read for
  index `i` (passed as a ghost argument): some possible read of a
  non-verbose query of `q` reports that triple. Verus checks the
  precondition at the one call, so every triple written is the query's
  answer for the pointer read at its index.
- **The hint is the rule.** In the body, `r == 1` iff `defrag_rule(rd)`
  for the record of the loads; every early `return 0` (not ready, not a
  slot, bit clear, uncommitted, full slab, no arena, `slab == slabcur`)
  is a conjunct the rule requires.
- **`curregs` is theorem 22's.** The `curregs` read is `bin_stats`'
  second value, the figure theorem 22 sums: at a quiescent state it is
  the arena's `claimed - released` for the class, its slots out (cached
  and live, `Books::theorem_quiescent`).

Machine code: the read records and `store_triple`'s ghost parameter
have no bytes, and `NBINS`, `count_bins` and `ZERO` are `exec const`s
whose `ensures` add no code.

**How it is proved.** A ghost `rd` is filled in as each value is read
(`rd.live = cls.nslots - u.nfree` from `nfree`'s bound in `utilization`;
`rd.cur_live` is `curregs` before less `curregs` after the subtraction
in the hint), and each `return` asserts `util_read`/`hint_read` of it,
the trigger of the existential `ensures`. `util_from` and `defrag_rule`
are opaque, revealed only in the two bodies, so their callers' queries
hold them as atoms. The `BIN_MAX` branch uses `lemma_class_table_wf`
(`size == spec_class_size`) and `NBINS`' `ensures`. `util_triple`'s
existential has one trigger, `util_read(.., rd, u)`, over a conjunction
whose other conjunct (`triple_is(u, ..)`) does not mention `rd`, so it
cannot carry a trigger of its own; it is a goal (proved from
`utilization`'s `ensures` at the call), not a hypothesis instantiated
elsewhere.

**Findings.** None: the code matches the statement. The tests
`utilization_queries_answer_valkeys_defragmenter` (`api/mallctl.rs`)
and `get_defrag_hint_follows_the_utilization_data`
(`api/jemalloc.rs`) exercise the shapes and the rule at run time.

**Premises beyond the common ones.**
- The loads Verus does not constrain are the reads: the bitmap word, the
  watermark, the descriptor's `live` and `arena`, the arena table entry,
  the class's `released`/`claimed` and its locked `stats`, the partial
  set and the huge descriptor. "Exact" is over those values.
- The records are what was loaded: each field is set from the local the
  code loaded (by reading the body; the `ensures` cannot name the
  instant of a load).
- `query_ptr` and `store_triple` (verified over the C contract of the
  caller's buffers, `vspec::cbuf`): `query_ptr` reads the pointer at
  index `i` of the caller's array (its value is not stated: the records
  are over the value read); `store_triple` writes `nfree`, `nregs`,
  `size` at words `3i..3i+3` and nothing else (by its body: the unaligned
  writes' effect is not stated). That the stored triples
  are still there when `mallctl`
  returns needs the caller's `oldp` and `newp` not to overlap (a later
  `store_triple` could overwrite a pointer before it is read, or a later
  query read a stored word as a pointer) and nothing else to write them
  during the call; neither is checked.
- `classified`'s `Invalid` and `Huge` cases are as weak as `classify`'s
  postcondition: it does not say a huge page start is `Huge` (it is, by
  the body).

**Scope.** `utilization` (both `mallctl` keys) and `get_defrag_hint`,
for every pointer. `owner_of` (`arenas.lookup`) is not covered.

**Where the statement is weaker than the code.**
- The `ensures` are existential over the record: to a caller, `rd` is
  some read consistent with the result, not the one the body made (the
  same caveat as theorem 22's ghost outputs). A caller can derive only
  this: from `utilization`, that `nregs` and `size` are exact for the
  branch the result is consistent with (zeros, `(1, size)` of `p`'s
  class above `BIN_MAX`, `(nslots, nslots * size)` of `p`'s bin class, or
  `(1, any usable)` for a huge page), that `nfree <= nslots`, that
  `bin_nregs` is a multiple of `nslots` with `bin_nfree <= bin_nregs`,
  and that a non-null `slabcur_addr` is a slab start of the class; from
  `get_defrag_hint`, only `r == 1 ==>` the allocator is ready and `p` is
  a slot start of a slab class (both values are consistent with some
  read whenever that holds). The iff with the rule holds over the actual
  loads inside the body, not in the contract.
- The reads are not one snapshot: the bit, the watermark, `live`, the
  arena counters and the partial set are separate loads (the class lock
  is taken twice by the hint and by the verbose query), so the figures
  are exact for what was read, not for one instant. A claim in flight
  makes `nfree` under-report (`SlabDesc::nfree`); theorem 22's
  quiescence premise makes `curregs` exact.
- "Live slot" is "bit set": a cached slot (in a thread cache) also has
  its bit set, so it reads as live, as in jemalloc (the bit is the
  `free` check's).
- `usable` is the huge descriptor's `usable` field as read under the huge
  lock; that it equals the run's pages before the guard (`Live::len`) is
  not restated here.
- `cur_live` is the amount the subtraction removed (the current slab's
  live count clamped at `curregs`); the statement does not name the load.
- The arena's class counters are those of `a.class(class)` for the arena
  the descriptor named when read; that it is the slab's owner at that
  instant is theorem 14's (slab ownership never changes once assigned),
  not restated.

## 36. Arena IDs survive enumeration holes (with a stable registry)

The statistics scans key every arena's figures by its registry ID, and
theorem 22's exact-stats identity holds even when some arena indices
are unbuilt. The registry's indices can have holes: an automatic arena is
built only when something first asks for it (`Arenas::get`, through
`Arenas::materialise`: a refill of a heap homed there, `mallctl`'s
`arena.<i>.*`, an explicit-arena allocation), and `arenas.create` hands
out the next index after every automatic one, built or not. With
`narenas:2`, only arena 0 built and one explicit arena, the registry
holds IDs 0 and 2 and nothing at 1.

**Statement.** Two layers.

1. **The scan keys each arena by its registry ID** (code, `stats.rs`).
   In `collect_into` and `collect_all` each read is
   built with `index` the ID of the arena it was read from
   (`collect_arena_with` ensures `r.index ==
   arena.spec_index()`; `stats::arena_at` ensures `a.spec_index() ==
   i` for the table index `i` it was asked for, from
   `Arenas::get_existing`, `arena.rs`, verified through `arena_ref`),
   and the loop invariant keeps
   every earlier read's ID below `i`. The `ensures` (and `mallinfo2`'s,
   `api/glibc.rs`, from `collect_all`'s) export what a caller can
   name: the IDs of the reads handed out through the ghost `rd` ascend,
   so no arena is read twice (`ids_ascending`, `stats.rs`), and each is
   below `MAX_ARENAS`:

   ```rust
   // stats.rs
   pub open spec fn ids_ascending(reads: Seq<ArenaStats>) -> bool {
       forall|i: int, j: int| 0 <= i < j < reads.len()
           ==> #[trigger] reads[i].index < #[trigger] reads[j].index
   }
   // collect_into and collect_all, ensures
   ids_ascending(final(rd)@.0),
   forall|j: int| 0 <= j < final(rd)@.0.len()
       ==> ((#[trigger] final(rd)@.0[j]).index as usize) < MAX_ARENAS,
   ```

   `collect_into` stores each arena's figures in that arena
   (`arena.store_stats(&a)`, with `a.index == arena.spec_index()`), so
   `stats.arenas.<i>.*` reads arena `i`'s own figures.

2. **Exact stats over a sparse registry** (ghost model,
   `stats/ledger.rs`). The ledger `Books` has `built`, the
   indices whose entry is published, beside `k`, the registry's count.
   `Books::start(auto)` (`lemma_start`) has `k = auto` and only
   arena 0 built; `new_arena` (`Arenas::create`) builds index
   `k` and adds one to `k`; `materialise(a)` (`Arenas::build` of an
   automatic index, `arena.rs`; `lemma_step_materialise`) builds
   `a`. Every counter step (`reserve`, `claim`, `release`, `count`,
   `take_run`, `note`, `free_run`, `unnote`) requires its arena built,
   and `Books::inv` includes `unbuilt_fresh`: every unbuilt
   index below `k` has zero counters and no slots or runs (`fresh`,
   one quantifier per conjunct), and bounds `k` by
   `MAX_ARENAS` (`new_arena` requires `k < MAX_ARENAS`, as `create`
   checks; `start` requires `1 <= auto <= MAX_ARENAS`, as `init`
   clamps), so a ledger never has a built index a `u16` ID cannot name
   and `reads_keyed` can always be met. `theorem_exact_stats`
   takes the reads keyed by ID:

   ```rust
   // stats/ledger.rs
   pub open spec fn reads_keyed(b: Books, reads: Seq<ArenaStats>, pages: Seq<int>) -> bool {
       &&& reads.len() == pages.len()
       &&& ids_ascending(reads)
       &&& forall|j| 0 <= j < reads.len() ==> (#[trigger] reads[j]).index < b.k
       &&& forall|a| 0 <= a < b.k && #[trigger] b.built.contains(a)
           ==> exists|j| 0 <= j < reads.len() && (#[trigger] reads[j]).index == a
       &&& forall|j, c| ... reads[j].bins[c].curregs == (b.net)(reads[j].index as int, c)
       &&& forall|j| ... reads[j].large_runs == (b.wruns)(reads[j].index as int)
       &&& forall|j| ... pages[j] == (b.wpages)(reads[j].index as int)
   }
   ```

   `lemma_reads_keyed` shows such reads sum to the ledger's
   counters over all of `0..k`: by induction over the arenas, the reads
   of IDs below `a` are a prefix (the IDs ascend), and an index no read
   names is not built (every built one is named), so its counters are
   zero. Theorem 22's conclusion (the reported `allocated` is the live
   blocks' usable bytes plus the cached slots') therefore holds with
   holes: it does not require `reads.len() == b.k` or `reads[i]` to be
   arena `i`, which the scan of the registry above (IDs 0 and 2, two
   reads, `k = 3`) does not satisfy.

The test `stats::tests::collection_keys_arenas_by_registry_id_across_a_hole`
(`stats.rs`) runs the example: `narenas:2`, `create` returns 2,
arena 1 stays unbuilt through `collect` and `epoch`, both scans count 2
arenas, `arena_stats(1)` is `None`, arenas 0 and 2 report index 0 and 2
with their own `curregs` (1 and 2), and the totals are their sum.

**Covers / stated in.** `collect_into` (so `epoch`, `collect`,
`malloc_stats_print`'s totals and the `stats.*` snapshot), `collect_all`
(`mallinfo`, `mallinfo2`, `malloc_stats`), and `theorem_exact_stats`.
`Arenas::create` (`arena.rs`) also ensures the ID it returns is
below `MAX_ARENAS`. `mallinfo2` (`api/glibc.rs`, verified) ensures
`summed`, `ids_ascending` and the `< MAX_ARENAS` bound of its reads.

**Premises beyond theorem 22's.**
- A stable registry during the scan: the `arenas.count()` the scan
  loads is the ledger's `k`, and each entry load in `get_existing` sees
  the entry as published (non-null exactly for the built indices).
  `get_existing`'s `ensures` gives nothing on `None` (a load outside
  the creation lock may read a stale null), so "every
  built arena is read" is `reads_keyed`'s fourth conjunct, a premise,
  like the quiescence link of the counters. Nothing frees or unpublishes
  an arena, so an entry, once non-null, stays the same arena.
  `get_existing`'s `spec_index() == index` is proved: it
  loads the entry inside the registry's invariant (`vspec::entry::load`)
  and reads the arena through its frozen permission (`arena_ref`).
- The ledger's `materialise` and `new_arena` steps abstract
  `Arenas::build`'s release store of the entry and `Arenas::create`'s
  `count` store (read from the code, as theorem 22's other steps).

**Scope and where the statement is weaker than the code.**
- "Visits every published arena once" is: once is proved (`ids_ascending`);
  every is the stable-registry premise above. The ensures says the
  IDs are below `MAX_ARENAS`, not below the count the scan loaded (a
  local the `ensures` cannot name); `reads_keyed` requires `< k`.
- `Arenas::init` is verified, and the registry's entries are under its
  invariant (`NarenasPred`, `arena.rs`): `build` requires
  that the creation lock's snapshot has no arena at the index, so each
  index is published at most once (proved), and `create`'s index is at
  or above `auto` by the creation lock's frame (`Created`'s `LockSpec`
  impl: the lock's `n` never drops below its initial, automatic count,
  which every release checks, the fork handlers' `Lock::unlock_held_perm`
  included). The ledger's `built` set is not tied to that
  snapshot (read from the code).
- `Snapshot::narenas` is the number of arenas the scan found, so it is
  below `arenas.narenas` while an automatic arena is unbuilt
  (`malloc_stats_print`'s "Arenas: 2 (2 automatic)" with IDs 0 and 2,
  where `arenas.narenas` reads 3); its doc comment says what it counts.
- `arena_stats_all_into` (`stats.arenas.4096.*`) and the report scans in
  `api/print.rs` have no keyed contract; they key their output by
  `a.index()` too.

## 37. Slab claim never misses free capacity (at quiescence)

A refill that comes back empty has not missed free capacity: if the
arena was quiescent, every committed slab it owns was full. The code's
half is verified; the lock-free half is a verified ghost protocol model
tied to the code by premise.

**Statement.** A non-empty claim of an arena's state of a class returns
no slot only after a lock hold that ran dry, in whose lock value every
committed slab of the arena is owned, `Active` and not a guard (proved
of the code). If the arena was quiescent at that hold, no committed slab
had a free slot: in the ghost protocol model linked to that hold by
premise, every one has every slot's bit set and `live == nslots`, so
none had an unreserved free slot either. Two layers.

1. **The code's half: what a dry hold saw** (`class/mod.rs`, verified).

```rust
// class/mod.rs
fn claim_with(&self, meta: &Meta, out: &mut [*mut u8], mut rng: Option<&mut u64>,
    Tracked(toks): Tracked<&mut SlotToks>, Tracked(ex): Tracked<&mut Ghost<(Inner, Map<nat, usize>)>>,
    pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (Claim, Tracked<Map<int, Zeroed>>))
    ensures
        ...
        r.0.n == 0 && old(out)@.len() > 0 ==> self.exhausted(final(ex)@.0, final(ex)@.1),

// class/mod.rs
fn plan(&self, meta: &Meta, want: usize, picks: &mut [Pick; MAX_PICKS], ...,
    Tracked(ex): Tracked<&mut Ghost<(Inner, Map<nat, usize>)>>, Tracked(ord): Tracked<&mut LockOrder>) -> (np: usize)
    ensures
        np == 0 && want > 0 ==> self.exhausted(final(ex)@.0, final(ex)@.1),
        ...

// class/mod.rs
spec fn exhausted(&self, g: Inner, seen: Map<nat, usize>) -> bool {
    &&& g.inv()
    &&& g.nbits() == self.spec_nslabs()
    &&& g.arena@ == self.arena
    &&& g.table@ == self.spec_table()
    &&& g.oauth@.id() == self.spec_oloc()
    &&& g.empty.map@ == VSet::<nat>::empty()
    &&& g.purged.map@ == VSet::<nat>::empty()
    &&& forall|x: nat| #[trigger] g.partial.map@.contains(x) ==> seen.contains_key(x)
    &&& forall|x: nat| #[trigger] g.partial.map@.contains(x) ==> seen[x] >= class_table()[self.spec_class() as int].nslots
}
```

   `g` is the class lock's value as the dry hold released it (the
   invariant `Inner::inv` with the ghost link), `seen` the `live` loads of
   that hold's partial-set scan, by slab. `claim_with` returns `n == 0`
   for a non-empty `out` only by breaking out of its loop on a plan of no
   picks with `want == out.len()`, and `plan` returns no picks for a
   non-zero `want` only when its scan of the partial set took no slab
   (every partial slab's `live` load was `nslots` or more; the slabs it
   cleared as full left the set) and `next_slab` then returned `None`,
   which ensures both idle sets empty and the partial set unchanged
   (`class/fresh.rs`). `lemma_exhausted` (`class/mod.rs`) draws
   the consequence in the lock value: every committed slab is owned, not a
   guard and `Active` (neither idle set holds it, and a committed slab is
   never `Fresh`, a conjunct of `Inner::slab_core`,
   `class/mod.rs`), and a partial one was read with `live >= nslots`.

2. **The lock-free half: a ghost protocol model** (`class/complete.rs`,
   verified). `Proto` is one arena's state of one class: the
   lock's half as in `Inner` (states, committed set, the three sets) and
   what Verus does not see in the code, per slab its set bits, `live`,
   the released bits not yet subtracted (`pend`) and the unflushed
   `Reopen` entries naming it (`reopen`), and per refill its unclaimed
   reservations (`resv`) and its claimed slots not yet handed out
   (`flight`). One step per atomic update or lock hold of the protocol:
   `begin` (a claim starts), `own` (`take_group` hands the arena a slab
   `Fresh`), `take` (`ClassState::take`: reserve `n <= nslots - live`,
   `Active`, out of the partial set exactly when `live == 0` and every
   slot is reserved), `clear` (`plan`'s clear of a full partial slab),
   `claim` (one bit won by `finish` or `claim_prefix`), `hand_out`
   (`claim_with` returns, every reservation met), `release`
   (`release_word`'s `fetch_and` of one out slot, with a `Reopen` entry
   when its word was full), `dec` (`dec_live`), `live_zero`
   (`on_live_zero`), `reopen_step` (one entry of `reopen`) and `purge`
   (`settle`, after `push_group` and the discard, the
   `Purge::PurgeLocked` step). `Proto::inv` holds at `Proto::start`
   (`lemma_start`) and after every step (`lemma_step_*`),
   in any interleaving. Its core (`slab_inv`): `live`
   is the set bits plus the unclaimed reservations plus the unsubtracted
   releases, at most `nslots`; and a *closed* slab (`Active`, out of the
   partial set, no `Reopen` entry pending) has every slot set or
   reserved, and while a reservation is out it is all its closer's (the
   refill whose `take` closed it), whose flight holds every set bit. So a
   release of a closed slab's slot finds nothing reserved (the slot is in
   no flight), the slab and so the slot's word full, and records an
   entry: the slab is then not closed. `theorem_quiescent`: with
   nothing in flight, `live` is the set bits and a closed slab is full.
   `theorem_dry`: at a quiescent state with both idle sets empty
   and every partial slab's `live` at `nslots` or more, every committed
   slab is `Active` with every bit set and `live == nslots`.
   `lemma_example` runs one slot through `own`, `take`, `claim`,
   `hand_out`, `release`, `reopen_step`, `dec`, `live_zero` and to
   `purge`'s precondition, chaining the step lemmas into a state that
   meets `theorem_dry`'s premises with a committed slab;
   `lemma_example_clear` reaches `clear`'s (its `bits == slots`
   case) and proves its states keep `Proto::inv`. So the steps' preconditions and the theorem's premises are not
   vacuous, though `lemma_example`'s state has no partial slab, so the
   partial-`live` premise holds there trivially.

The two layers meet in:

```rust
// class/complete.rs
pub(super) proof fn theorem_claim_complete(&self, b: Proto, g: Inner, seen: Map<nat, usize>, s: nat)
    requires
        self.exhausted(g, seen),
        b.inv(),
        b.quiescent(),
        b.nslots == class_table()[self.spec_class() as int].nslots as int,
        b.st == g.states@,
        b.committed == g.committed@,
        b.partial == g.partial.map@,
        b.empty == g.empty.map@,
        b.purged == g.purged.map@,
        forall|x: nat| #[trigger] seen.contains_key(x) ==> seen[x] as int == (b.live)(x),
        g.committed@.contains(s),
    ensures
        s < self.spec_nslabs(),
        g.owns(s),
        g.st(s) == State::Active,
        !self.spec_table().spec_is_guard(s as usize),
        forall|x: int| 0 <= x < b.nslots ==> #[trigger] (b.bits)(s).contains(x),
        (b.live)(s) == b.nslots,
```

So, contrapositively: if an owned, committed, non-guard slab of the
arena has a free slot (a clear bit) or an unreserved one (`live <
nslots`) at a quiescent hold, the claim does not return zero.

**Covers.** Verified, in the code layer: `ClassState::claim_with`
(`class/mod.rs`), `plan`, `take`, `next_slab`
and `next_fresh_slab` (`class/fresh.rs`). Modelled, as
steps of `Proto` (the code's `protocol::Refill`, `Release` and `Purge`
steps, `class/mod.rs`), and related to the code by premise: `finish`,
`claim_prefix`, `release_word` (`class/mod.rs`), `reopen`,
`dec_live`, `on_live_zero`, `push_group` and
`settle` (`class/decay.rs`)
and `take_group`. `claim_tokens` (`class/mod.rs`) and
`claim_random_zero` call `claim_with` with a local `ex`,
return its claim count unchanged and do not export the ensures (their
signatures are the heap's interface). `claim` (which calls
`claim_tokens`) and `claim_random` (which calls
`claim_random_zero`) are test-only wrappers
(`cfg_attr(not(test), allow(dead_code))`), verified over the caller's
`Published` witness and lock-order token; production code calls the
verified `claim_tokens` and `claim_random_zero` directly
(`heap/flush.rs`, `heap/shared.rs`). The ghost `seen` records
`plan`'s `desc.live()` load, which `plan` binds to a local.

**Premises beyond the common ones.**
- Quiescence at the dry hold: no claim between its `take` and its
  return, no release between its `fetch_and` and its `dec_live` and
  `reopen` flush, of this arena and class (`Proto::quiescent`). The
  requesting claim itself has nothing out: a dry hold has no picks, and
  any earlier plan of a claim that ends with no slot reserved only
  `n == 0` picks (`take` does not promise `n > 0`), so nothing it
  reserved or claimed is outstanding.
- The link: the model's lock half is the lock value `g`, and each `live`
  load of the hold read the model's count (at a quiescent state nothing
  writes it). Verus does not see the atomics' values (common premises),
  so the model is related to the code by premise, as theorem 22's ledger
  is: each step's update is read from the code named above, and its
  precondition is what the code knows there (the slot's token is out of
  its word and in no refill's batch for `release`; `finish` claims every
  reservation before `claim_with` returns, `r == picks_sum`, for
  `hand_out`; `take` reads `live` under the lock, and a concurrent
  `sub_live` only lowers it, so `n <= nslots - live` holds at the add).
- Abstractions, each a superset of the code's behaviours: a multi-bit
  `fetch_or` or `fetch_and` is a run of one-bit steps; the zero pick's
  prefix claims are `claim` steps after the `take` rather than inside
  its hold; a release batch's `Reopen` list is a per-slab count of its
  entries (the list is thread-local, so pushing at the `fetch_and` is
  unobservable), with `dedup` allowed only when an entry is pending, as
  `Reopen::push` drops a slab only when the list already ends with it;
  `plan`'s clear asks only "full or an entry pending" at the clear: its
  `live` and `is_full` loads are not a snapshot, but a word read full
  and released since then was found full by the first such release,
  whose entry waits for the lock the clear holds; `release`'s full-word
  test is over the slab's slots below `nslots`, while the code's
  compares the whole word with `slab_mask()`, padding bits included; the
  two agree because an initialised slab's padding bits stay set, which
  the model takes as given.
- Arena isolation (theorem 14): the slots released to this state are of
  slabs it owns (`release_owned`'s owner checks), and `reopen` and
  `on_live_zero` act only on its own slabs.

**Scope and where the statement is weaker than the code.**
- Partial correctness: `claim_with`'s retry loop's termination is
  assumed (`exec_allows_no_decreases_clause`), so the statement is of a
  claim that returns.
- The requesting arena only, and quiescent states only: a concurrent
  release between clearing a bit and its `Reopen` flush can hide a free
  slot from a refill (the slab is out of the partial set until the
  flush), and an unclaimed reservation or a release not yet subtracted
  can make `live` overstate or understate the set bits. Another arena's
  slabs are not available to it, and the uncommitted rest of its current
  group is excluded (a dry `next_fresh_slab` found no commit possible).
- The ghost outputs are set from the loads by `plan`'s body; to a caller
  of `claim_with`, `ex` is a value for which `exhausted` holds, and the
  tie of `seen` to the loads (and of the model to the code) is the link
  premise above.
- The model is not threaded through the code: nothing carries a `Proto`
  from step to step, and `theorem_claim_complete` relates it to the lock
  value by premise. The descriptor's `live` and the `Reopen` lists are
  not ghost state inside the bitmap words' invariants. Unlike `src/model`,
  whose steps mirror the code's `protocol` enums by construction, the
  steps here are tied to the code by reading it, and `src/model`'s
  `check` has no run-time cross-check of this fact.
- The public claims do not export the fact (their contracts do not
  mention it); it is stated at `claim_with`, which `claim_tokens` and
  `claim_random_zero` call and return directly. `exhausted` includes
  `g.oauth@.id() == self.spec_oloc()`, and `take` also ensures that a
  slab its hold did not fill stays in the partial set.

**Findings.** None: the protocol keeps the sets complete. The
`Inner::slab_core` conjunct (a committed slab is not `Fresh`) needs
`next_fresh_slab` to hand its committed slab out under `inv_but`, which
`take` requires; `next_slab` and `next_fresh_slab` state that
they leave the partial set alone and that `None` means both idle sets
are empty.

## 38. Explicit purge leaves no empty slabs (at each lock release; one slab per page)

An explicit purge (`malloc_trim`, `arena.<i>.purge`) discards every
empty slab and every empty huge extent it finds, except where `madvise`
refuses or a page holds several slabs, and `malloc_trim` returns 1
exactly when it discarded a byte.

Stated per lock release, over a ghost record (`purge::Left`) the purges
set themselves: when `purge::purge_all` or `purge_arena` returns, no
class of an arena it visited had an `Empty` slab in the class lock value
its purge released, except in a class with several slabs per page or in
a range `madvise` refused during the purge (`mlock`ed memory),
and the huge allocator had no `Empty` extent in the huge lock
value its purge released except in such a range; the count returned
is nonzero iff `madvise` took a byte and a flush reported it discarded,
so `malloc_trim` returns 1 iff its purge discarded a byte.
The record's link to the lock values is the two assignments that set it
(`*left = Ghost(..)` from the guard's value just before it drops, in
`ClassState::purge_empty` and `Huge::purge_empty`), read from the code:
the `ensures` alone do not bind it to a lock, as theorem 22's ghost
reads. Not a statement about the allocator at return: frees after each
lock is released can make new `Empty` slabs (it is the state at return
only at quiescence).

**Statement.**

```rust
// purge.rs
pub ghost struct Left {
    pub bytes: int,                         // bytes reported discarded
    pub arenas: Set<int>,                   // arenas whose every class was purged
    pub classes: Set<(int, int)>,           // (arena, class) locks taken and released
    pub slabs: ISet<(int, int, int)>,       // (arena, class, slab) `Empty` in a released value
    pub huge: bool,                         // the huge lock was taken and released
    pub extents: ISet<int>,                 // descriptors `Empty` in the released huge value
    pub refused: ISet<(int, int, int)>,     // slabs of a range `madvise` refused, `Empty` then
    pub refused_extents: ISet<int>,         // descriptors of an extent `madvise` refused
}

// purge.rs: class `c` has several slabs per page.
pub open spec fn paged(c: int) -> bool { crate::layout::guard_geom(c).0 != 0 }

// purge.rs
pub open spec fn clean(self) -> bool {
    &&& forall|t: (int, int, int)| #[trigger] self.slabs.contains(t) ==> paged(t.1) || self.refused.contains(t)
    &&& forall|a: int| #[trigger] self.extents.contains(a) ==> self.refused_extents.contains(a)
    &&& self.bytes >= 0
}
pub open spec fn covers(self) -> bool {
    forall|a: int, c: int| #[trigger] self.classes.contains((a, c))
        <==> self.arenas.contains(a) && 0 <= c < NCLASSES
}

// purge.rs
pub fn purge_all(shared: &Shared, Tracked(left): Tracked<&mut Ghost<Left>>,
    Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
    ensures
        (r > 0) == (final(left)@.bytes > 0),
        forall|i: int| #[trigger] final(left)@.arenas.contains(i) ==> 0 <= i < MAX_ARENAS,
        final(left)@.covers(),
        final(left)@.huge,
        final(left)@.clean(),

// purge.rs
pub fn purge_arena(shared: &Shared, index: u16, Tracked(left): ..., Tracked(ord): ...)
    -> (r: Option<usize>)
    ensures
        r is None ==> final(left)@ == Left::none(),
        r matches Some(n) ==> (n > 0) == (final(left)@.bytes > 0),
        r is Some ==> final(left)@.arenas == set![index as int],
        final(left)@.huge == (r is Some && index == 0),
        final(left)@.covers(),
        final(left)@.clean(),

// api/libc.rs
pub fn malloc_trim(_pad: usize, Tracked(left): Tracked<&mut Ghost<Left>>) -> (r: c_int)
    ensures
        r == 0 || r == 1,
        (r == 1) == (final(left)@.bytes > 0),
        final(left)@.covers(),
        final(left)@.clean(),
```

(Line breaks added; `ord` clauses dropped.) In words: `purge_all`
(`malloc_trim`, `arena.<ARENAS_ALL>.purge`, the cgroup `high`-rise full
purge in `purge::decay`) visits the arenas in `left.arenas`, all below
`MAX_ARENAS`, and for each purges every class (`covers`), then the huge
allocator (`huge`); each class purge leaves no slab `Empty` in the value
its lock releases unless the class is `paged` or the slab is in
`left.refused`, and the huge purge leaves no extent `Empty` outside
`left.refused_extents` (`clean`, which also says `bytes >= 0`). `bytes`,
the sum over every batch of the bytes `madvise` took and a flush
reported discarded (`Batch::sent`), is positive iff the returned count
(slabs or extents made `Purged`) is.

How it is proved:
- **The walk ends past every member** (`class/decay.rs`).
  `ClassState::walk_empty` ensures `none_above(g, last)` (
  opaque): no member of the empty set lies above the last slab visited;
  each visit is handed `next_after(g, last, slab)` (opaque):
  `slab` is the lowest member above the last, from `lowest_set` and
  `lowest_set_from`'s contracts.
- **A full-cutoff purge with one slab per page pushes every visit, and
  every pushed slab is settled.** The batch's settler, the class
  `Tally` (`purge::Settle`), carries the walk's invariant over the lock
  value and the pieces pending (`Batch::pieces`; each a page group with
  a slab in the empty set, `pend_ok`). When `full` (`cutoff ==
  u64::MAX` and `spp == 1`), every member of the empty set at or below
  the last slab visited is pending or in the tally's `refused`, the
  slabs `Empty` in a range a flush reported refused; `push_group` keeps
  it at each visit (`next_after`), and `ClassState::settle` at each
  report (discarded: the slab leaves the set, `Purged`; refused: it
  joins `refused`). After the walk, `top`, a bound on every member
  (kept by every settle, which only removes members), is set to the
  last slab (`none_above`); after the final flush nothing is pending, so
  every member is refused. `Inner::slab_core` makes the empty set
  exactly the owned `Empty` slabs, so `ClassState::purge_empty` sets
  `left.slabs` to the released value's owned `Empty` slabs and
  `left.refused` to the tally's `refused`, tagged with its arena and
  class, and proves the first inside the second, or, when `spp != 1`,
  the class `paged` (`slabs_per_page` ensures `spp == 1 <<
  guard_geom(c).0`).
- **The huge purge visits the whole `Empty` list.** `Huge::purge_empty`
  walks the list from its head without unlinking (`push_extent`; a
  flush's `Huge::settle` unlinks only extents reported discarded,
  `purge_desc`). The huge `Tally` keeps the walk (`walked`): the list
  ends with the members left to visit, `rest`, and every member before
  them was visited, each pending or, for a full purge, in `refused` once
  reported refused. The loop ends at a null `next` (nothing left) or at
  a stamp past `cutoff`, impossible at `u64::MAX`; `Inner::wf` puts
  every `Empty` descriptor on the list, so `left.extents` lies in
  `left.refused_extents` (`Tally::lemma_walked_all`). A merged huge range
  is one extent: free extents never touch (theorem 17), so a second
  pending piece would start where the first ends.
- **Count iff bytes.** Each settle adds the bytes it is reported
  discarded to its tally's `bytes`, which `Batch::push` and `flush`
  equate with their change of `Batch::sent`. The tally keeps `purged >
  0` iff `bytes > 0`: a range reported discarded holds a slab in the
  empty set (`pend_ok`; the pending groups are disjoint) or is an
  `Empty` extent, which the settle makes `Purged`. After each purge's
  final flush, `sent()` is its `bytes` and the tally's `purged` the
  count returned.
- **Composition.** `Arena::purge_all` (`arena.rs`) joins its
  classes' records (`Left::join`, `lemma_join`, `purge.rs`), with the
  arena invariant giving `classes[c]` class `c` and the arena's index;
  `purge_all` and `purge_arena` join the arenas' and the huge records;
  `malloc_trim` returns `(purge_all(..) > 0) as c_int`, and `Left::none()`
  before init.

**Covers.** `malloc_trim`; `mallctl` `arena.<i>.purge` (both forms) and
`arena.<i>.destroy`; the full purge `purge::decay` runs on a cgroup
`high` rise; the exit purge in `Heap::destroy` (verified; its call is through
`purge_arena`'s contract).

**Premises beyond the common ones.**
- The class and huge locks' invariants (`class::Inner`'s and
  `huge::Inner`'s `LockSpec` impls, checked at every release), as for
  theorems 17, 19 and 29: the value a lock
  hands over satisfies its invariant, which is what ties the empty set to
  the `Empty` slabs and the `Empty` list to the `Empty` extents.
- `madvise` does what it is asked when it succeeds (a common premise);
  `discard` reports `EINVAL` as a refusal and aborts on any other error.
  `bytes` counts what `discard` reported taken, and `refused` what a
  flush reported refused: that the report is `discard`'s result is read
  from `Batch::flush`'s body, not stated in a contract. A refused range
  stays `Empty`, and the next explicit purge retries it.
- One slab per page, for the class half: `paged(c)` is false exactly
  when `slabs_per_page` is 1. That holds for every class on every
  supported page size (`layout::lg_slab_at`, `layout/mod.rs`,
  raises every slab to the page up to 64 KiB, and the process page is at
  most 64 KiB: `Regions::separated`, `layout/regions.rs`, carries
  `lg_page <= 16`, proved from `getauxval`'s specification at
  init), but nothing
  proves it of the process's table: `guard_geom` is the stored `Shared`'s
  table's geometry, and
  `class_table()` is not tied to `classes_for_page` (`SlabTable::new`
  computes `slabs_per_page = (page / slab).max(1)`, `class/table.rs`).
  The statement keeps it as the `paged` exception instead.

**Scope and where the statement is weaker than the code.**
- At each lock release, not at return: the record is set from the lock
  value just before each guard drops; after that, a free can bring a
  slab back to `Empty` (`on_live_zero`) or free a huge run into an
  `Empty` extent. At quiescence (no other thread allocating or freeing)
  the values released are the values at return.
- With several slabs per page (a page above 64 KiB), an `Empty` slab
  whose page group holds an `Active` slab stays `Empty` by design
  (`page_group_is_idle` vetoes the discard; the page group is the
  arena's whole and handed out, `class::whole_groups`, so its watermark
  and arena checks are run-time defences that never veto for the real
  lock value): the code's behaviour, not a bug, and the `paged`
  exception.
- The arenas visited are those `get_existing` finds below the count
  `arena_count` read once: an arena built during the purge may be
  skipped; `left.arenas` is not stated to contain every built arena.
- The record is ghost and set by the function itself: a caller learns it
  only through the `ensures` (as theorem 22's ghost reads), and that it
  is the released value's is read from the body (`*left = Ghost(..)`
  from `g@` right before the release), not exported as a lock fact.
- `malloc_trim`'s cache flush (`flush_all`) runs before the purge and
  discards nothing; "1 iff bytes were discarded" is about the purge.

## 39. Fork handlers restore the signal mask

The fork handlers keep every blockable signal blocked while they hold
the allocator locks, and give the forking thread back its own signal
mask on both sides of the fork. The handlers block signals
so that a signal handler on the forking thread cannot re-enter the
allocator, or fork again, while it holds every allocator lock (module
doc of `api`, design section 9); this states that they do, and that
the thread gets back the mask the slot holds, on both sides of the
fork.

**Statement.** `fork_prepare` blocks every blockable signal but glibc's
internal `SIGCANCEL` and `SIGSETXID` (32 and 33;
a thread holding `SIGSETXID` blocked hangs every
`setuid`-family call) before its
first acquisition of a lock of the fork lock list and keeps them
blocked through its last (its deferred cgroup
discovery, `Shared::discover_cgroup_for_fork`, runs first, with the
mask unchanged: the policy evaluation it may run takes and releases
class locks as any slow path does, and holds none on return), and it saves
the mask that was in force at its entry in `FORK_SIGMASK` with every
allocator lock held, recording it in the hold (`ForkHeld::saved`; that
the word stored is that mask is read from the code, not proved). The
release (the body of `fork_parent` and `fork_child`) reads the slot
with every lock still held, keeps every blockable signal blocked through
its last unlock, and then leaves the thread with exactly the mask the
slot held. That this is the mask the preparing thread had at its
prepare's entry (in the parent, and in the child, the forking thread's
copy) is the handoff premise below: each half is proved on its own
side of the C calls. The ghost token `sys::SigMask` is the calling
thread's mask; only `block_all_signals` and `set_signal_mask` change it
(through `&mut`, by their trusted raw calls), so each loop invariant
`sig.mask() == all_blocked()` in the handlers is the mask at that
acquisition or release.

```rust
// sys/mod.rs
pub open spec fn effective(m: u64) -> u64 { m & !unblockable() }   // less SIGKILL, SIGSTOP
pub open spec fn all_blocked() -> u64 { effective(BLOCKABLE_SIGNALS) }   // every bit but 8, 18 (SIGKILL, SIGSTOP), 31 and 32
pub open spec fn wf(self) -> bool { effective(self.mask()) == self.mask() && self.mask() & !BLOCKABLE_SIGNALS == 0 }   // SigMask

// sys/mod.rs
pub fn block_all_signals(Tracked(sig): Tracked<&mut SigMask>) -> (r: u64)
    requires ..., old(sig).wf(),
    ensures r == old(sig).mask(), final(sig).mask() == all_blocked(), final(sig).wf(),
pub fn set_signal_mask(mask: u64, Tracked(sig): Tracked<&mut SigMask>)
    requires ..., mask & !BLOCKABLE_SIGNALS == 0,
    ensures final(sig).mask() == effective(mask), final(sig).wf(),

// api/mod.rs
pub(crate) open spec fn fork_sig_holds(sig: SigMask, held: ForkHeld, orig: u64) -> bool {
    &&& sig.wf()
    &&& sig.mask() == all_blocked()
    &&& held.saved() == Some(orig)
    &&& effective(orig) == orig
    &&& orig & !crate::sys::BLOCKABLE_SIGNALS == 0
}

// fork_prepare (api/mod.rs): `orig` is the entry token's mask,
// read before `block_all_signals`; every lock loop has the invariant
// `sig.mask() == all_blocked()`; at the end:
assert(fork_holds(*s, held, state));
assert(fork_sig_holds(sig, held, orig));

// api/mod.rs (both verified): the slot is written and read
// only under every allocator lock; the store, under the prepare's hold
// (no permission to the slot, `no_slot`), is recorded in the
// hold with the word it went to (`slot_addr`), and the release's hold
// carries the permission to that word (`has_slot`: a
// `vspec::word::WordPerm` for `slot_addr()` whose value is `saved`),
// through which the load reads
fn save_sigmask(mask, Ghost(s), Tracked(held): Tracked<&mut ForkHeld>, Tracked(state))
    requires fork_holds(s, *old(held), *state), old(held).no_slot(),
    ensures *final(held) == old(held).with_saved(mask, final(held).slot_addr()),
{ let slot = fork_slot(); slot.store(mask, Ordering::Relaxed);
  proof { held.saved = Some(mask); held.slot_addr = word_addr(slot); } }
fn saved_sigmask(w: &AtomicU64, Ghost(s), Tracked(held): Tracked<&ForkHeld>, Tracked(state)) -> (r: u64)
    requires fork_holds(s, *held, *state), held.has_slot(), held.slot_addr() == word_addr(w),
    ensures held.saved() == Some(r),
{ word::load(w, Ordering::Relaxed, Tracked(held.slot.tracked_borrow())) }

// fork_release (api/mod.rs): from the handoff, `saved` is the
// hold's slot content and `fork_sig_holds(sig, held, saved)`;
// every release loop has `sig.mask() == all_blocked()`; after
// the last unlock and `set_signal_mask`:
assert(sig.mask() == saved);
```

**Covers / stated in.** `fork_prepare`, `fork_release` (the body of
`fork_parent` and `fork_child`), `save_sigmask`, `saved_sigmask`, `fork_slot`
(`api/mod.rs`); `block_all_signals` and `set_signal_mask`
(`sys/mod.rs`), whose `ensures` rest on the kernel's
`rt_sigprocmask` semantics. `purge::background::spawn`
(`external_body`) calls both with placeholder tokens
(`Tracked::assume_new()`); it is not part of the statement.

**Proof.** `block_all_signals` returns the entry token's mask (`orig`)
and leaves `all_blocked()`; nothing else touches `sig` until the
release's last unlock, which the loop invariants restate at each
acquisition and release. `save_sigmask` is called after the last
acquisition, so its requirement (`fork_holds`, every listed lock and the
state lock) holds; it stores `orig` and records it in the hold, and
`ForkHeld::lemma_with_saved` (`api/mod.rs`) keeps `fork_holds`
across the store. `saved_sigmask` is called before the release's first
unlock (`fork_unlock(state)` consumes the state token, so a later call
could not meet the requirement); it loads through the hold's slot
permission, so `word::load`'s `r == perm.value()` and `has_slot` give
`held.saved() == Some(r)`. The read mask has `SIGCANCEL` and
`SIGSETXID` clear (`fork_sig_holds`, from the handoff), as
`set_signal_mask` requires. Restoring with `set_signal_mask` leaves
`effective(saved)`, which is `saved` because the slot holds a mask the
kernel held (`lemma_effective_idempotent`, `sys/mod.rs`, a
bit-vector fact).

Two threads forking at once with different masks (`FORK_SIGMASK` is one
process-wide slot): the slot's content is recorded in the hold of every
allocator lock (`ForkHeld`), and its permission exists only in the
release's hold; each write (`save_sigmask`) and each read
(`saved_sigmask`) requires that hold, so, with theorem 13's mutex
premise, a second thread's write happens only after the first thread's
release has read the slot and unlocked: each release reads its own
prepare's mask. glibc serialises the handlers with its atfork lock
anyway; the unit tests do not, and
`concurrent_fork_handlers_restore_each_threads_mask` (`api/mod.rs`)
runs two threads with different masks through the prepare and parent
handlers 200 times each and checks each gets its own mask back;
`fork_handlers_block_signals_while_locked` (`api/mod.rs`) checks both sides
for one thread.

**Findings.** None: the code matches the statement.

**Premises beyond the common ones** (and theorem 13's mutex and
handoff premises).
- *The calling thread's mask* (`sys::axiom_thread_mask`,
  `sys/mod.rs`; axiom). The token minted at `fork_prepare`'s entry
  stands for the calling thread's mask (its value unnamed: a mask the
  kernel can hold, with glibc's `SIGCANCEL` and `SIGSETXID` unblocked,
  as glibc's `sigprocmask` and `pthread_sigmask` strip them and
  challoc's raw calls never block them), and nothing on the thread
  changes the mask while the token is held except through it: challoc
  calls `rt_sigprocmask` only in `block_all_signals`, `set_signal_mask`
  and `abort` (which unblocks `SIGABRT` and never returns), and no other
  code runs on the thread inside the handler. One call per handler
  entry; nothing enforces it at run time (`fork_hold_is_resumed_once`,
  `api/mod.rs`, checks there is one call site of each of the thread-mask
  and handoff axioms). `SigMask`'s field is private to a module of its own
  (`sys::sigmask`), so no code builds a token.
- *The kernel's `rt_sigprocmask`* (`sigprocmask_block`, `sys/mod.rs`, and
  `sigprocmask_setmask`, in `sys::linux`, private;
  `external_body`, each the one `raw::syscall4` call its caller made
  before). A successful `SIG_BLOCK` of `BLOCKABLE_SIGNALS`, every
  signal but `SIGCANCEL` and `SIGSETXID`, wrote the mask in force before
  it to the old-set buffer and left exactly `all_blocked()`: `SIG_BLOCK`
  adds to the mask in force, and no thread has those two blocked
  (glibc's `sigprocmask` and `pthread_sigmask` refuse to block them, and
  challoc's raw calls block only `BLOCKABLE_SIGNALS` and restore masks
  saved from those); a successful `SIG_SETMASK` leaves the requested mask less
  `SIGKILL` and `SIGSTOP` (signals 9 and 19, in the 64-bit kernel
  sigset on both architectures). Success is the word `decode` takes for
  success; on failure nothing is said (the callers abort). The value
  comes from the call, so no caller can choose it.
- *The handoff, its signal half* (`api::axiom_fork_resumed_sig`,
  `api/mod.rs`, and `axiom_fork_resumed`'s conjuncts, which also say
  the saved mask lies inside the blockable signals;
  axioms). The parent and child handlers run on the thread whose
  prepare returned holding `fork_sig_holds` (in the child, its copy,
  which inherits the mask), with the mask still `all_blocked()`:
  glibc's `_Fork` restores the mask it found, and the atfork handlers
  registered before challoc's (prepare after ours, parent and child
  before ours) leave the mask as they found it. The resumed hold's
  `saved` is what that prepare stored, a mask the kernel held; the
  `ensures` cannot name the prepare's `orig`, so that the two are the
  same value is the premise. The resumed hold also carries the slot's
  permission at that value (`has_slot`): the one trusted claim that
  `FORK_SIGMASK` holds the recorded mask (the store is a plain relaxed
  store, under every lock, through `fork_slot`, and `save_sigmask`
  requires a hold without the permission, `no_slot`; only a holder of
  every lock writes the slot, and the permission exists only in the
  release's hold, so its value is the slot's while it exists). One call
  per handler; nothing enforces it at run time (`fork_hold_is_resumed_once`
  checks there is one call site).
- *The slot's identity* (part of `axiom_fork_resumed`). Verus cannot name
  a static in a specification, so `fork_slot` (verified, `&FORK_SIGMASK`)
  claims nothing. The prepare's hold records the word its store went to
  (`slot_addr`; bookkeeping only, as nothing reads that hold after the
  store), and the resumed hold's permission is for the word the axiom
  is passed, so that the release's word is the prepare's (both
  `&FORK_SIGMASK`; a static has one address) is the handoff premise.
  The `&Shared` both handlers read is not a premise: the axiom requires
  it to be `Shared::built_at(Shared::storage())`, which `shared`
  ensures. Passed another word, the axiom
  would mint a second permission to it, so it is private to the module
  `api::resume`, whose one function, the verified `resumed`
  (`fork_release` calls it), passes `fork_slot()`'s word and returns it
  with the hold.
- *Its loads* (`vspec::word::load`, the existing permission-atomic
  wrapper: one atomic load, whose result is the permission's value).

**Scope.** The fork handlers, for every completing pair (a prepare
that returns, then the parent or the child handler on the same thread).
Not covered: the background thread's spawn (it blocks signals around
`pthread_create` so the new thread starts with every signal blocked;
trusted body), and a prepare whose `fork` is never followed by a
handler (glibc always runs one).

**Where the statement is weaker than the code.**
- "The preparing thread's original mask" in the release is the slot's
  value as the resumed hold records it; that it is the `orig` of the
  prepare on this thread is the handoff premise. The prepare side
  proves that it records `orig` in the hold (the store into
  `FORK_SIGMASK` itself is unchecked: a store of any other value would
  verify, so that the slot holds `orig` rests on the code and on the
  handoff premise), and the release side that it restores what it read;
  neither proof sees the other.
- "Throughout the hold" is the token's value at each loop iteration
  and each straight-line step between acquisitions; it rests on the
  thread-mask premise (nothing else changes the mask). That a blocked
  signal is not delivered is the kernel's, not stated.
- Signal masks are the kernel's 64-bit sigset. glibc's `sigset_t` is
  wider, but the kernel reads and writes 8 bytes (`SIGSET_WORDS_BYTES`),
  so nothing is lost on Linux.
- A second call of `axiom_thread_mask` or `axiom_fork_resumed_sig`
  would mint a second token for the same mask. No lemma relates two
  tokens, so this proves no `false`, but a stale one could state a wrong
  mask: the one-call-per-handler premise is what excludes it. (A second
  call of `axiom_fork_resumed` would prove `false`, theorem 13.)

## 40. Purge batches are well formed

Every range a purge hands to `madvise` (`purge::discard`, the one place
a batch's ranges reach the kernel) is non-empty, starts and ends on a
page boundary of the process's page, and lies, byte by byte, in the
huge region or in a class region outside that class's guard slabs; so
no byte of it is in the metadata region. For every batch: the huge purges
(`Huge::purge_empty`, `purge_curve`, `free_run`) and the class purges
(`ClassState::purge_empty`, `purge_curve`, through `push_group`), each
flush, including the one inside a `push` to a full batch and the
re-issue of a refused range's pieces.

**Statement.**

```rust
// purge.rs (opaque; the private `lemma_well_formed` unfolds it)
pub open spec fn well_formed(addr: int, len: int) -> bool {
    &&& page_bytes() > 0                      // `1 << process_regions().lg_page`
    &&& len > 0
    &&& addr % page_bytes() == 0
    &&& len % page_bytes() == 0
    &&& forall|b: int| addr <= b < addr + len ==> #[trigger] discardable(b)
}

// purge.rs
pub open spec fn discardable(b: int) -> bool {
    ||| huge_byte(b)                          // huge_base <= b < huge_base + huge_len
    ||| exists|c: int| 0 <= c < NCLASSES && #[trigger] class_byte(c, b)
}
pub open spec fn class_byte(c: int, b: int) -> bool {
    let r = process_regions();
    let sz = (1usize << class_table()[c].lg_slab) as int;
    &&& r.class_base[c] <= b < r.class_base[c] + r.spec_nslabs(c) * sz
    &&& !guard_slab(c, (b - r.class_base[c]) / sz)
}

// purge.rs: what every call hands `madvise`.
fn discard(addr: usize, len: usize, advice: Advice, Ghost(free): Ghost<spec_fn(int) -> bool>) -> bool
    requires
        frees(free, addr as int, len as int),
        well_formed(addr as int, len as int),

// purge.rs
pub proof fn theorem_purge_well_formed(addr: int, len: int, b: int, c: int, s: int)
    requires
        crate::heap::regions_ok(),
        well_formed(addr, len),
        addr <= b < addr + len,
    ensures
        len > 0,
        page_bytes() > 0,
        addr % page_bytes() == 0,
        (addr + len) % page_bytes() == 0,
        discardable(b),
        !process_regions().in_meta(b, 1),
        0 <= c < NCLASSES && 0 <= s < process_regions().spec_nslabs(c) && guard_slab(c, s)
            ==> !(process_regions().slab_span(c, s).0 <= b
                  < process_regions().slab_span(c, s).0 + process_regions().slab_span(c, s).1),
```

(Abridged: `crate::` paths, `#![auto]` triggers and `discard`'s
capability clauses dropped; line breaks added.) In words: `discard`'s
precondition is the statement, checked at each of its calls in
`Batch::flush_in` (a range, and, when `madvise` refuses it, its first,
last and middle pieces); `theorem_purge_well_formed` reads it
out: the range is non-empty and page-aligned at both ends, and each
byte is in the huge region or a class region, outside the metadata
region (`Regions::in_meta`), and in no guard slab `s` of any class `c`
(for every `c` and `s`, as its arguments). A `Batch` (`purge.rs`)
also records the ranges it handed over, in call order (a ghost `gone`,
`Batch::discarded`): each `discard` call appends its range, `push` and
`flush` only extend the record. `Batch::wf` keeps `well_formed` of
every recorded range, but no lemma reads it out above the batch.
`discard` requires `well_formed(addr, len)`,
so every call is checked against it.

How it is proved:
- **The batch.** `Batch::wf` (closed) keeps `well_formed` of every range
  it holds, of each piece of a merged range (`runs_wf`: a merge only
  joins ranges of one length, `lemma_run_merge`), and of every range in
  `discarded()`; a piece of a range is inside it
  (`lemma_piece_ok`); `push`
  requires it of the pushed range, and its merge with the last range
  (adjacent ranges go out as one) keeps it (`lemma_well_formed_join`:
  two adjacent page-aligned ranges of discardable bytes are
  one). `well_formed` is opaque, so the batch's invariant carries one
  fact per range.
- **Class purges.** `push_group` (`class/decay.rs`) pushes the page
  group of `slab` (`spp` slabs from `first = slab & !(spp - 1)`) and
  requires `slab` in the empty set and `spp` the table's slabs per page,
  which `Purger::visit` has. `lemma_group_well_formed`
  (`class/decay.rs`): the table's base and a page group's bytes are
  page multiples (`SlabTable::inv` includes `lg_page ==
  process_regions().lg_page` and the opaque `groups_paged`,
  `class/table.rs`, which `SlabTable::new` proves through
  `lemma_new_geometry`, which ensures `spp * slab % page == 0`, and
  `lemma_groups_paged`), so `first * slab` is a multiple of the
  group's bytes; the group's slabs lie below the table's `nslabs`, the
  process's (`SlabTable::inv`), and share `slab`'s page group
  (`x >> lg_spp == slab >> lg_spp`), so none is a guard slab
  (`guard_of` reads only the page group) because `slab`, `Empty`, is
  not one (`Inner::slab_core`: an owned guard slab is `Fresh`).
- **Huge purges.** `Huge::is_process` (`huge/mod.rs`, closed): the
  allocator's base, page and page count are the process's huge
  region's, its base a page start, the regions well formed and
  separated, and its run authorities the process's
  (`Huge::runs_process`). `Shared::lemma_huge_process` (`heap/shared.rs`) proves
  it from `Shared::wf`; `purge_empty`, `purge_curve`, `decay`,
  `decay_with` and `free_run` require it, and `push_extent` has it
  through the purge tally's invariant (`Tally::inv`), and `purge::purge_all`,
  `purge_arena`, `decay` and `decay_arena` require `shared.wf()` (every caller has
  it). `Huge::lemma_purge_range` (`huge/mod.rs`): pages `start..start
  + pages` (at least one) of the region are a well-formed range, since
  `npages * page <= huge_len`. `push_extent` pushes an `Empty` extent
  (at least a page, `desc_ok`) and `free_run` the freed run, guard page
  included.
- **Never metadata, never a guard slab.** `theorem_purge_well_formed`:
  a huge byte is outside the superblock and the metadata region
  (`Regions::separated`); a class byte lies in a slab below the class's
  `nslabs`, inside stride `c` of the superblock (`lemma_slab_in_stride`),
  apart from the metadata region and from every other class's stride; in
  its own class its slab index is the byte's, which `class_byte` says is
  not a guard slab.

**Covers.** Every discard: `malloc_trim`, `mallctl` `arena.<i>.purge`,
`arena.<i>.decay` and `arena.<i>.destroy`, the decay walks
(`purge::decay`, the background thread, the slow path's tick), the
`high`-rise full purge, every free of a huge run that purges at once
(above `huge_retain_max` or under `huge_dirty_decay_ms:0`, `Huge::free_run`),
and the exit purge in `Heap::destroy` (verified,
so through `purge_arena`'s contract).

**Premises beyond the common ones.**
- The process's regions: the class half rests on `SlabTable::inv`
  (`base`, `nslabs` and `lg_page` the process regions'), which
  `SlabTable::new` proves from its caller's `regions == process_regions()`,
  well formed and separated (`Shared::init`); the huge half and the
  metadata clause on `Shared::wf` (`regions_ok()`, the huge allocator
  over the huge region), which `api::shared_ref` claims of the published
  `Shared` (as every verified function taking a `&Shared`).
- The guard geometry: a table's guard slabs are the process's
  (`guard_geom`, by its definition over the stored `Shared`'s tables,
  `SlabTable::is_process`), as for theorem 7.
- The class lock's invariant (`class::Inner`'s `LockSpec` impl, checked
  at every release): the value `push_group`
  holds satisfies the lock invariant, which is what makes an empty-set
  slab `Empty`, so not a guard slab.

**Scope and where the statement is weaker than the code.**
- "Page" is the process regions' `lg_page`, the page `Regions` was built
  with (the kernel's `AT_PAGESZ` at init), not re-read from the kernel.
- The huge half allows every page of the huge region, and a freed run's
  guard page is discarded with the run (its `MADV_GUARD_INSTALL` guard
  was removed first): by design, a freed run's pages all become free.
  "Never a guard slab" is about the class regions' guard slabs.
- A class range may cover uncommitted slabs (`Fresh` slabs of the page
  group when a page holds several slabs): `madvise` of reserved, never
  touched pages is harmless, and they are inside the class region.
- The ranges a batch holds are disjoint (`Batch::wf`: `disjoint_upto`,
  from `push`'s `apart`), and each piece pushed is reported to the
  settler once (`Settle`), but a range may reach `madvise` more than
  once: a refused merged range is re-issued piece by piece, and a range
  refused in one purge is pushed again by the next. The record
  `discarded()` is not exported above the batch (`purge::Left`, theorem
  38, carries the byte count and the refused slabs and extents).
- Only `purge::Batch` discards are covered; `MADV_GUARD_INSTALL`
  (`class/table.rs`, `huge/mod.rs`) and `MADV_GUARD_REMOVE` drop or keep
  contents of guard pages outside any batch (theorem 7).
- The unit tests build batches over an unconstrained ghost predicate
  (`Batch::new_t`) and push arbitrary ranges; they are not verified code.

## 41. Seal targets stay inside challoc's reservations

Every range `Regions::seal` hands `mseal` (`never_committed`'s ranges,
the one place init seals) is non-empty, lies inside one of challoc's
three reservations (the class superblock's, the huge region's or the
metadata region's `mmap`), and shares no byte with any class region,
the huge region, the metadata region or the process's `Shared` static.
So no range `mseal` gets overlaps a region the allocator commits, purges
or reprotects in, or lies outside the address space challoc reserved.

**Statement.**

```rust
// layout/regions.rs (opaque; each proof that needs it reveals it, such as `lemma_superblock_gap`)
pub open spec fn seal_target(self, m: Mapping) -> bool {
    let region = (1usize << self.lg_region) as int;
    &&& m.len > 0
    &&& self.in_mapping(m.addr as int, m.len as int)
    &&& forall|c: int| 0 <= c < NCLASSES ==>
        disjoint(#[trigger] self.class_base[c] as int, region, m.addr as int, m.len as int)
    &&& disjoint(self.huge_base as int, self.huge_len as int, m.addr as int, m.len as int)
    &&& disjoint(self.meta_base as int, self.meta_len as int, m.addr as int, m.len as int)
    &&& disjoint(m.addr as int, m.len as int, shared_static_addr(), shared_static_len())
}

// layout/regions.rs: inside `superblock_map`, `huge_map` or `meta_map`.
pub open spec fn in_mapping(self, a: int, len: int) -> bool

// layout/regions.rs
pub fn never_committed(&self) -> (r: ([Mapping; MAX_SEALED], usize))
    requires
        self.wf(),
    ensures
        r.1 <= MAX_SEALED,
        self.separated() && self.mapped() ==> forall|i: int| 0 <= i < r.1 ==> self.seal_target(#[trigger] r.0[i]),

// layout/regions.rs: before each `mseal(m.addr, m.len, 0)`.
pub fn seal(&self) -> u32
    requires
        crate::sys::may_syscall(),
        self.wf(),
        self.separated(),
        self.mapped(),
    ...
        assert(self.seal_target(*m));
```

(Abridged: line breaks added.) In words: `seal` asserts `seal_target`
of each range right before its `mseal` call; `never_committed` ensures
it of every range it returns, for regions that are separated and inside
their mappings. `Regions::mapped` is the reservation
arithmetic: the superblock's `NCLASSES` strides inside `superblock_map`,
the huge region inside `huge_map`, the metadata region inside
`meta_map`, the three mappings pairwise disjoint and none holding a byte
of the `Shared` static. `Regions::from_parts` and
`Regions::reserve_owning` ensure it, so `seal`'s one call
(in `api::init`, `api/mod.rs`, on the regions
`reserve_owning` returned; `init_slow` runs `init` once) has
its precondition.

How it is proved:
- **The mappings.** `from_parts` requires each region inside
  its mapping, the mappings pairwise disjoint and apart from the static
  (`reserve_owning` proves these from `align_base`, `slab_multiple`,
  `class_offset` and
  the trusted `Reserved::is_disjoint` and `is_apart_from_static`); it
  ensures them as `mapped`, with `NCLASSES << lg_stride` as
  `NCLASSES * stride` (`lemma_shl_mul`).
- **The class gaps.** `never_committed`'s loop keeps, for separated and
  mapped regions, `superblock_map.addr <= start <= superblock + c *
  stride`, every class below `c` ending at or before `start`, and
  `seal_target` of every range pushed so far (`push_range`, `layout/regions.rs`,
  ensures exactly what it appends and that it keeps the earlier
  entries). A gap `start..class_base[c]` lies inside the superblock's
  mapping (`lemma_class_in_strides`) and before every class
  from `c` on (`lemma_class_before`: class `j`'s region ends
  before class `k`'s starts for `j < k`, each inside its stride);
  `lemma_superblock_gap` concludes, and covers the slack after
  the last class (`k == NCLASSES`). The huge and metadata regions are
  inside their own mappings, disjoint from the superblock's.
- **The side slack.** `lemma_huge_slack` and
  `lemma_meta_slack`: a range in the huge (metadata) mapping
  before or after its region is apart from it by its bounds, from the
  other region by the mappings' disjointness, and from every class
  region because each class region lies in the superblock's strides,
  inside `superblock_map`.
- **The static.** Every range is inside a mapping, which `mapped` keeps
  apart from the static.

**Covers.** The init sweep (`api::init`, which `api::init_slow` runs
once, when the probe said `mseal=yes`): every range it seals, including those before a refusal
stops the sweep.

**Premises beyond the common ones.**
- The kernel's `mmap` contract as trusted for region separation
  (`linux::try_reserve`, `Reserved::is_disjoint`,
  `Reserved::is_apart_from_static`): the three reservations are live,
  pairwise disjoint and apart from the loaded object's data.
- The reservations stay mapped: nothing unmaps them (`Regions::release`
  is test-only), so the `Mapping` values describe live mappings when
  `seal` runs. That the `Mapping` fields are the reservations
  `try_reserve` returned is by construction in `reserve_owning` (the
  superblock and huge tokens are dropped there); no ensures ties the
  fields to a live `Reserved` token.

**Scope and where the statement is weaker than the code.**
- Nothing states the ranges page-aligned, which `mseal` needs to
  succeed: the mapping addresses `try_reserve` returns are not stated
  page multiples (they are, from the kernel), and the class bases (page
  multiples, `base_aligned`), the region lengths and the huge and
  metadata bases are `MAX_SLAB` or page multiples. A misaligned range
  would fail with `EINVAL` and stop the
  sweep, not seal anything else.
- "Never committed" itself is not proved: the statement is that the
  ranges avoid the class, huge and metadata regions, which is where the
  allocator commits, purges and reprotects (theorem 40 for purges); that
  no code touches a reservation byte outside them is not a verified
  fact.
- Nothing states the ranges pairwise disjoint or in address order, nor
  that together with the regions they cover the three mappings
  (`layout::tests::never_committed_is_the_complement_of_the_regions`
  tests the cover; it is not verified code).
- That every region keeps `MAX_SLAB` of its reservation below and above
  it is a run-time check, not part of the statement: `reserve_owning`
  checks `Regions::slack_ok` (`layout/regions.rs`: `never_committed`
  returns `MAX_SEALED` ranges, each at least `MAX_SLAB` long) and aborts
  if it fails (`slack_broken`). For the huge and metadata
  regions `reserve_owning` also proves the slack, from `slab_multiple`'s
  lower bound; for the class regions it rests on
  `class_offset`'s bounds and the test
  `extreme_draws_keep_slack_on_both_sides`.
- `print`'s `never_committed().1` (the count `malloc_stats_print`
  reports) holds no `mapped` fact and gets only the bound.
- Only `Regions::seal` is covered. `sys::features::probe_mseal` seals a
  scratch page of its own `mmap`, outside challoc's reservations by
  design; `sys::mseal_blocks_munmap_when_supported` seals a page of its
  own, and `layout::linux_tests::seal_child` seals a test layout through
  `Regions::seal` in the test build (not verified; `Regions::reserve`,
  the `cfg(test)` wrapper of `reserve_owning`, ensures its preconditions).
- Ghost only: the proof adds no code to the release library.

## 42. Mallctl write/read contract (settings and objects)

`ctl`, the core both `mallctl` and `mallctlbymib` call, ensures that a
call that fails changed none of the allocator state the keys set (the
background thread's request, a decay period, `thread.tcache.enabled`,
`thread.arena`, the arenas and tcaches), but for two errors that follow
a change in jemalloc 5.3.0's order too, and that with both `oldp` and
`newp` a setting copied out its old value before it applied the new
one; what changed is a ghost value that the callee making each change
records (`request`, the `set_*` setters, the two `create`s,
`stats::epoch`, each ensuring what it records), so that every change is
recorded is by inspection of those callees and the arms.
A read copies at most `*oldlenp` bytes, and a failed
`mallctlnametomib` writes nothing. Items are in `api/mallctl.rs` unless
named.

**Statement.**

```rust
pub ghost enum Change {                                             // vspec/ctl.rs
    Unchanged,
    Set { read_first: bool },   // a setting took the key's new value
    Created,                    // arenas.create, tcache.create
    Refreshed,                  // epoch's write
}

spec fn error_after_change(node: Node, r: Result<(), c_int>, c: Change) -> bool {
    ||| c is Refreshed
    ||| node.entry matches Entry::Op(Op::BackgroundThread) && r == Err::<(), c_int>(EAGAIN)
}

fn ctl(shared: &Shared, Tracked(mark): Tracked<&'static ReadyMark>, node: &Node, mib: &[usize], io: &Io,
    Tracked(cc): Tracked<&mut CCall>, Tracked(fx): Tracked<&mut Ghost<Change>>,
    Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: Result<(), c_int>)
    requires ..., crate::api::marks_ready(*mark), shared.wf(), io.handed(*old(cc)), !old(cc).written(),
        resolve(root_nodes(), mib@) == Some(*node),
    ensures
        r is Err && !error_after_change(*node, r, final(fx)@) ==> final(fx)@ is Unchanged,
        io.oldp@.addr != 0 && io.oldlenp@.addr != 0
            ==> (final(fx)@ matches Change::Set { read_first } ==> read_first),

pub open spec fn reads_ok<T>(self, call: CCall) -> bool {
    self.oldp@.addr == 0 || self.oldlenp@.addr == 0
        || call.len(self.oldlenp as *const usize) == vstd::layout::size_of::<T>()
}
fn read<T: Copy>(&self, v: T, Tracked(call): Tracked<&mut CCall>) -> (r: Result<(), c_int>)
    ensures
        r matches Err(e) ==> e == EINVAL,
        r is Ok <==> self.reads_ok::<T>(*old(call)),
        self.oldp@.addr != 0 && self.oldlenp@.addr != 0 ==> final(call).written(),
fn verify_read<T: Copy>(&self, Tracked(call): Tracked<&CCall>) -> (r: Result<(), c_int>)
    ensures
        r matches Err(e) ==> e == EINVAL,
        r is Ok <==> self.reads_ok::<T>(*call),

pub fn mallctlnametomib(name: *const c_char, mibp: *mut usize, miblenp: *mut usize,
    Tracked(cc): Tracked<&mut CCall>) -> (r: c_int)
    requires old(cc).fresh(CEntry::NameToMib { .. }),
    ensures r != 0 ==> !final(cc).written(),
```

(Abridged.) In words: `fx` is what the call changed, recorded by the
callee that makes the change: `request`, `set_decay_ms`,
`set_tcache_enabled` and `set_arena` each ensure `fx` is
`Change::Set { read_first: cc.written() }`, `Arenas::create` and
`Tcaches::create` ensure `Created` when they return `Some` and leave
`fx` unchanged otherwise, and `stats::epoch` ensures `Refreshed`.
`read_first` is the call token's `written` flag when the setter runs. In those arms a token is written exactly when a read has
copied through `oldp` (`Io::read` opens `oldp`'s writes; the only other
opener, `batch_query`, belongs to a key that changes nothing), so
`read_first` says the old value went out first. Every arm
that changes a setting reads first and propagates the read's error
with `?` before the change; `arenas.create` and `tcache.create` check
`verify_read` before creating, and since `verify_read` and `read`
agree (`reads_ok` of the same `*oldlenp`), the read after the creation
cannot fail. `mallctl` and `mallctlbymib` (`api/mallctl.rs`) pass
`ctl`'s result unchanged (`result`: `Ok` is 0, `Err(e)` is `e`).

- **Reads write at most `*oldlenp` bytes.** The writes through `oldp`
  are `copy_prefix` (`n <= oldlen`, `n = min(oldlen,
  size_of::<T>())`, through `ptr::copy_nonoverlapping`), `store`
  (`oldlen == size_of::<T>()`, through `write_unaligned`) and
  `batch_query`'s triples (each inside `oldlen == 24 * n`), with
  `oldlen` the value `old_len` read from `*oldlenp`: their
  `requires` (`c_buffer(oldp, oldlen)`, `c_writes`) bound every byte
  written to the caller's `oldlen`. `read` copies out only when the
  caller asked (`oldp` and `oldlenp` non-null), and fails `EINVAL`
  after the prefix copy when the sizes differ.
- **`mallctlnametomib` writes nothing on failure.** Its writes go
  through `mib_out`, which needs `c_writes` of `mibp` and
  `miblenp`; only `open_writes` gives those, and it runs after every
  check (`cstr`, the null tests, `name_to_mib`, the capacity), so a
  nonzero result leaves the token unwritten. The export wrapper
  passes the token by `&mut` (`let Tracked(mut cc) = nametomib_call(..)`),
  so the ensures can name its final state.

**Covers.** `mallctl` and `mallctlbymib` for every key `ctl` performs
(`Op` arms; a `Read` key is read-only and changes nothing), and
`mallctlnametomib`. `ctl` requires `resolve(root_nodes(), mib@) == Some(*node)`
(theorem 21). The `mallctl` and `mallctlbymib` C exports
(`api/mallctl.rs`) are verified: each mints its call's token with
`axiom_c_call` from its own parameters and lends `errno`
(`vspec::errno::lend`), so Verus checks that the C symbols reach the
verified `mallctl` and `mallctlbymib` with a fresh, unwritten token.

**Premises beyond the common ones.**
- `CCall::sized` and `CCall::counted` (`vspec/cbuf.rs`,
  trusted, the C contract of a sized buffer and a counted array)
  also ensure that the `size_t` read holds the token's view
  `CCall::len(lenp)`: C does not change `*oldlenp` (or
  `*miblenp`) during the call before challoc writes, as for the
  string argument's view. This is what makes `verify_read`'s check the
  one `read` makes.
- The changes `fx` records are the calls named above, by inspection of
  `ctl`'s arms and those callees: each callee's `ensures` states what it
  records, but the theorem does not prove that `request`, `set_decay_ms`
  and the rest change state, nor that no other call in an arm does (see
  Scope).

**Scope.**
- "Allocator state" here is the settings and objects the keys write:
  `background_thread`'s request (the write also
  disarms the lazy start, `background::AUTO`, in the same state lock
  hold as the request), `arena.<i>.dirty_decay_ms` (one arena
  or, for `arena.4096`, every arena's default), `thread.tcache.enabled`,
  `thread.arena`, and the arena and tcache sets. Not in it, as in
  jemalloc: the calling thread's heap, which `thread.tcache.enabled` and
  `thread.arena` create on first use before they check their arguments;
  thread-cache contents, which `arena.<i>.purge` and `arena.<i>.destroy`
  flush (`flush_caller`) before they validate `<i>`, so an unknown
  arena still flushes the caller's caches before `ENOENT`; the commands
  (`arena.<i>.purge`, `decay`, `destroy`, `thread.tcache.flush`,
  `tcache.flush`, `tcache.destroy`), whose callees report a missing
  arena or tcache as their failure and are taken to do nothing then.
- The two exceptions (`error_after_change`): `epoch`'s write refreshes
  the stats and then reads the new epoch, so a wrong `*oldlenp` fails
  `EINVAL` after the refresh (jemalloc's `epoch_ctl` does the same, and
  reads the new value too; `Change::Refreshed` is recorded only once the
  refresh is done, so a write that fails its size check is an error
  with nothing changed); `background_thread` set to true requests
  the thread and fails `EAGAIN` when it cannot start it, with the
  request left set (jemalloc's `background_thread_ctl` leaves its
  enabled flag set and fails too). A read error of `background_thread`
  comes before the request.
- `arenas.create` and `tcache.create` read the new index (there is no
  old value); for them the statement is that a read that would fail is
  caught before anything is created.
- `max_background_threads` checks its write and changes nothing.

**Where the statement is weaker than the code.**
- `read_first` is stated for settings only; `Created` and `Refreshed`
  carry no ordering (the creates verify the read first, `epoch` reads
  after its refresh by design).
- Nothing is stated about the value copied out beyond its order: that
  it is the setting's value (a load of an atomic) is the code.
- Ghost only: `ctl`'s `fx` and `mallctlnametomib`'s `&mut` token are
  `Tracked` parameters, erased in the build. The tests
  `contract_*` (`api::mallctl::tests`) check the behaviour at run time.

## 43. jemalloc size contracts

A sized free returns normally only when given a size that names the
block: for a slot, a size and alignment whose `nallocx` is the slot's
class size (jemalloc's `nallocx(size, flags) == sallocx(p)`); for a
huge run, a size that rounds up to the run's pages, which is the run's
`nallocx` at every alignment that leaves the size huge and placeable
(stated with C's token for the run). And
`nallocx(size, flags)` is 0 exactly when `mallocx(size, flags)` rejects
the size or the alignment: no class serves it, and no run for it has a
placement in the huge region even when nothing is placed; `mallocx`
then returns NULL in every state.

**Statement.**

```rust
// heap/mod.rs; the sized frees' cores (api/libc.rs
// `free_sized_block`, api/jemalloc.rs `sdallocx_block`) ensure both
// for C's token `blk` (the free paths' `hint_ok` is theorem "Bad input")
pub open spec fn hint_ok(p: *mut u8, hint: Option<usize>) -> bool
pub open spec fn huge_hint_ok(blk: Live, p: *mut u8, size: usize) -> bool {
    blk.claims(p) && !slot_start(p) && process_regions().is_huge_block(p@.addr, 1) ==> {
        &&& size + live::page_size() - 1 <= usize::MAX
        &&& crate::vspec::zero::huge_usable(size as int) == blk.len()
    }
}

// api/jemalloc.rs
pub proof fn theorem_sized_slot_nallocx(p: *mut u8, size: usize, lg_align: u32, c: int)
    requires
        hint_ok(p, spec_class_for(size, 1usize << lg_align)),
        process_regions().is_slot(c, p@.addr),
    ensures
        nalloc_usable(size, lg_align) == class_table()[c].size,
pub proof fn theorem_sized_run_nallocx(blk: Live, p: *mut u8, size: usize, lg_align: u32)
    requires
        huge_hint_ok(blk, p, size), blk.claims(p), !slot_start(p),
        process_regions().is_huge_block(p@.addr, 1),
        spec_class_for(size, 1usize << lg_align) is None,
        huge_placeable(size, 1usize << lg_align),
    ensures
        nalloc_usable(size, lg_align) == blk.len(),

// vspec/zero.rs
pub open spec fn run_placeable(base: int, pg: int, np: int, size: int, align: int) -> bool {
    &&& size + pg - 1 <= usize::MAX
    &&& align <= np * pg
    &&& exists|q: int| 0 <= q && q + run_need(pg, size) <= np && #[trigger] page_aligned(base, pg, q, align)
}
pub open spec fn huge_placeable(size: usize, align: usize) -> bool  // the process's huge region

// api/jemalloc.rs
pub open spec fn nalloc_usable(size: usize, lg_align: u32) -> int {
    match spec_class_for(size, 1usize << lg_align) {
        Some(c) => class_table()[c as int].size as int,
        None => if huge_placeable(size, 1usize << lg_align) { huge_usable(size as int) } else { 0 },
    }
}
pub proof fn theorem_nallocx_zero(size: usize, lg_align: u32)
    requires regions_ok(),
    ensures
        nalloc_usable(size, lg_align) == 0
            <==> spec_class_for(size, 1usize << lg_align) is None && !huge_placeable(size, 1usize << lg_align),
#[cfg(verus_keep_ghost)]
fn theorem_nallocx_zero_rejects(size: usize, flags: c_int, Tracked(errno): Tracked<&mut ThreadErrno>)   // body: n = nallocx; p = mallocx;
    requires may_abort(), may_syscall(),                       // assert(n == 0 ==> p@.addr == 0)

// huge/mod.rs
proof fn theorem_placeable_fits_empty(&self, g: &Inner, size: usize, align: usize)
    requires g.next_page == 0, self.spec_placeable(size, align),
    ensures exists|q: int| self.fits(g, q, run_need(self.spec_page(), size as int), align),
pub fn alloc_run(..) -> (r: (Placed, Tracked<Zeroed>))
    ensures r.0.ptr@.addr != 0 ==> self.spec_placeable(size, align), ...
```

(Abridged.) `nallocx` ensures `r == nalloc_usable(size, flags & 0x3f)`
(interface row 22) and `mallocx` ensures a non-NULL result's usable size
is `nalloc_usable` (`usable_is`). In words:

- **Sized frees.** A sized free that returns normally satisfies
  `hint_ok` (theorem "Bad input": a slot freed with another class's
  size aborts) and, with C's token for the block, `huge_hint_ok`: for a
  live run, `size` rounded up to whole pages is the run's usable size.
  `theorem_sized_slot_nallocx` and `theorem_sized_run_nallocx` turn
  these into `nallocx`: a slot's free was given a size and alignment
  whose `nallocx` is the class size, `sallocx(p)`; a run's free a size
  whose `nallocx` is the run's usable size at any alignment that leaves
  it huge and placeable.
- **`nallocx`'s zero.** `theorem_nallocx_zero`: 0 exactly when no class
  serves the request and it is not `huge_placeable` (class sizes and a
  run's usable size are positive). `theorem_nallocx_zero_rejects`
  calls `nallocx` and `mallocx` with the same arguments and verifies
  that a 0 from the first means NULL from the second, whatever the
  allocator's state: a non-NULL block is a slot or a run, whose usable
  size `mallocx` states is `nalloc_usable`, positive. Conversely
  `huge_placeable` is exactly a placement in the empty region:
  `Huge::placeable` (`huge/mod.rs`) ensures it is
  `spec_placeable`, `run_placeable` over the huge allocator's own
  region (`Shared::lemma_huge_placeable`, `heap/shared.rs`, ties
  the two), and `theorem_placeable_fits_empty` gives a fit
  (`Huge::fits`) under a lock value with nothing placed, which `find`
  (`huge/mod.rs`) then returns (its `None` means no page fits). A
  run `alloc_run` places in any state was placeable (its start is an
  aligned page the run and its guard fit from), and the
  `alloc_huge` layers carry that up to `mallocx`.

How it is proved:
- `Huge::run_pages` (`huge/mod.rs`) ensures
  its result is `run_need(page, size)` and that `None` means not
  placeable; `placeable` compares the first aligned page `align_page(0,
  align)` (which ensures no lower page is aligned) plus the run's pages
  with the region: any placement starts at or after it.
- `check_size_hint` (`heap/shared.rs`): for a huge page start it
  looks the run up with the caller's run token (`Huge::lookup`,
  `huge/mod.rs`, ensures that with a token of a run starting
  at `page` the result is that run's `Active` descriptor with the
  token's usable bytes: the token's extent is in the lock's run
  authority, which names one `Active` descriptor starting there, and
  the table keys it at `page`) and aborts (`huge_hint_mismatch`,
  `heap/shared.rs`) unless `Huge::usable_for(size)` is the run's
  usable size. `Regions::classify` (`layout/regions.rs`) also
  ensures that with separated regions every huge page start is `Huge`
  (a hit of `in_superblock` lies in the strides), so no run is passed over.
- The fast path (`free_sized_fast`, `heap/paths.rs`) ensures a
  push is a slot start's, and a declined push keeps the token's claim
  and length, so `huge_hint_ok` of the token reaches the cores.
- `free_sized` claims C's token itself and hands it
  to `free_sized_with` (also `sdallocx_block`'s no-flags branch), which
  passes it to `free_sized_block`, or, above `SMALL_MAX`, to
  `free_sized_medium`, which does; both ensure `huge_hint_ok` of the
  token as `free_sized_block` does, so the statement covers this
  sized path (the huge sizes are above `SMALL_MAX` and reach
  `check_size_hint` through `free_sized_medium`).

**Premises beyond the common ones.** The run token and
`axiom_c_claim` (the C contract of a live block, "Bad input"), and the
lock invariant of the huge allocator (theorems 23 and 29); `check_size_hint`
is a root of the lock-order premise (`axiom_entry`, theorem 33: the
sized frees' slow paths hold no lock). The sized frees, `mallocx` and
`theorem_nallocx_zero_rejects` take the thread's `errno` token.

**Scope and where the statement is weaker than the code.**
- A huge run's size is checked by page count, not by `nallocx` at the
  flags: a size that rounds up to the run's pages passes at any
  alignment, including the small size of a run an alignment made huge
  (`mallocx(64, MALLOCX_LG_ALIGN(20))` then `sdallocx(p, 64, 0)`, where
  `nallocx(64, 0)` is a class size) and an alignment no run could be
  placed at. jemalloc's contract asks for the flags the block was
  allocated with; challoc takes these as the same block (upstream's
  test `sdallocx_of_a_huge_run_takes_any_size_in_its_last_page`).
  `theorem_sized_run_nallocx` therefore needs the alignment to leave
  the size huge and placeable.
- `huge_hint_ok` is stated with C's token for the run (its pages); the
  entry points that claim it (`axiom_c_claim`, of the call's `CCall`,
  which the exports mint with `axiom_c_call`: `free_sized`,
  `free_aligned_sized`, `sdallocx`) state only `hint_ok`. A freed or
  foreign run is excluded by the C contract, as
  for every free (*(C)*).
- "Rejects" is about the size and alignment: a non-zero `nallocx` does
  not promise success (the region may be full, metadata or the kernel
  may refuse, an explicit arena or tcache may not exist), and the class
  path's failures are of that kind. "Empty region" is a lock value whose
  mark is page 0; that `find` returns a placement then is its `ensures`
  (no `None` while a page fits), not a run of `alloc_run`.

## 44. Valid programs never abort (the frees after init, through the caches or none, sized or not, and the lookups of `realloc` and `malloc_usable_size`, up to resource failures and named pending checks)

A free given the token of a live block (what a program that frees only
what it was given, once, holds by the C contract, `axiom_c_claim`)
reaches none of the free paths' corruption, invalid-free and
double-free checks listed below as proved: it can abort only where a
resource it needs fails (a system call, or metadata the reservation or
the kernel cannot supply; two such failures a valid program can reach
are named below), or at one of the checks named below as pending,
which fire for a valid program only if allocator state no verified
invariant constrains is corrupt (or, for `unbind_unbound`, a count
exceeds a bound no invariant states). Which aborts are resource failures or
pending checks rests on reading the code (below).
`sys::tests::narrow_aborts_only_at_their_check_sites` checks that the
two sinks are called only at its listed sites,
`sys::tests::trusted_aborts_require_a_capability` that the trusted
bodies whose text shows an abort require a capability for it, and
`sys::tests::abort_grants_only_at_roots` (`sys/mod.rs`) where
`grant_aborts` is called. Covered, after init (`shared.wf()`; the prefix before it is
not): the free through no cache (`heap::free_direct`: a thread with no
heap, `MALLOCX_TCACHE_NONE`), slot and run; a free on a
heap (`Heap::free`: the push onto the home or the `FOREIGN` cache, the
slow path with its tick, cache GC, decay walks and purges, the arena
it may build and the cache it may create, the flush and its release,
the move of a young heap's home), and a sized free (`free_sized`,
`sdallocx`, `free_aligned_sized`) of a size that names the block, on a
heap or none; and the lookups of the block that
`malloc_usable_size` and `realloc` make.

**Statement** (`sys/mod.rs`;
`sys/abort.rs`; `heap/paths.rs`; `api/libc.rs`):

```rust
pub uninterp spec fn kernel_aborts() -> bool;
pub open spec fn may_abort_kernel() -> bool { may_abort() || kernel_aborts() }

pub uninterp spec fn pending_aborts() -> bool;
pub open spec fn may_abort_pending() -> bool { may_abort() || pending_aborts() }

pub open spec fn valid_aborts() -> bool { kernel_aborts() && pending_aborts() }

pub fn abort_kernel(args: fmt::Arguments<'_>) -> !
    requires
        crate::sys::may_abort_kernel(),

pub fn abort_pending(args: fmt::Arguments<'_>) -> !
    requires
        crate::sys::may_abort_pending(),

#[cfg(verus_keep_ghost)]
fn theorem_valid_free_never_aborts(shared: &Shared, p: *mut u8, Tracked(blk): Tracked<Live>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        crate::sys::integrity_aborts(),
        shared.wf(),
        blk.block(p),
// body: free_direct(shared, p, blk)

#[cfg(verus_keep_ghost)]
fn theorem_valid_run_free_never_aborts(shared: &Shared, h: &mut Heap, p: *mut u8, Tracked(blk): Tracked<Live>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        shared.wf(),
        old(h).wf(),
        blk.block(p),
        process_regions().is_huge_block(p@.addr, 1),
// body: classify_free, then h.free_huge(shared, page, blk)

#[cfg(verus_keep_ghost)]
fn theorem_valid_cache_free_never_aborts(shared: &Shared, h: &mut Heap, p: *mut u8, Tracked(blk): Tracked<Live>,
    Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        shared.wf(),
        old(h).wf(),
        blk.block(p),
// body: h.free(shared, p, blk)

#[cfg(verus_keep_ghost)]
fn theorem_valid_sized_free_never_aborts(shared: &Shared, h: &mut Heap, p: *mut u8, size: usize, hint: Option<usize>,
    Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        shared.wf(),
        old(h).wf(),
        blk.block(p),
        hint matches Some(c) ==> c < NCLASSES,
        hint_ok(p, hint),
        super::huge_hint_ok(blk, p, size),
// body: free_sized_fast when hinted, then shared.check_size_hint and h.free

#[cfg(verus_keep_ghost)]
fn theorem_valid_sized_direct_free_never_aborts(shared: &Shared, p: *mut u8, size: usize, hint: Option<usize>, Tracked(blk): Tracked<Live>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        crate::sys::integrity_aborts(),
        shared.wf(),
        blk.block(p),
        hint_ok(p, hint),
        super::huge_hint_ok(blk, p, size),
// body: shared.check_size_hint, then free_direct

#[cfg(verus_keep_ghost)]
fn theorem_valid_lookups_never_abort(shared: &Shared, p: *mut u8, size: usize, Tracked(blk): Tracked<&Live>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        shared.wf(),
        blk.block(p),
// body: usable_size, old_size_or_keep (realloc) and realloc_old
```

`kernel_aborts()` and `pending_aborts()` are two more capabilities
beside theorem 32's `may_abort()` and `integrity_aborts()`:
uninterpreted, granted by nothing, each accepted only by its sink
(`abort_kernel`, `abort_pending`: `abort_fmt` under the narrower
`may_abort_kernel()` or `may_abort_pending()`, as `abort_integrity` is
under `may_abort_corrupt()`). The witnesses hold the two, not
`may_abort()`, and verify. The two direct-free witnesses
(`theorem_valid_free_never_aborts`,
`theorem_valid_sized_direct_free_never_aborts`) also hold
`integrity_aborts()`: `free_direct` reads the thread's TLS
slot and requires `may_abort_corrupt()`, so a corrupt slot stays an
integrity abort there. The other witnesses hold no `integrity_aborts()`:
theorem 32's integrity checks are on none of their paths.
Every function on the covered paths requires `may_abort()` or, in its
place, one of: `valid_aborts()` with what a live block gives it
(`may_abort() || (valid_aborts() && blk.block(p))`, and the forms with
`holds_slot`, `owned_slots`, `live_slots`, `states_ok`, a slot
address, the cache entries apart from `p`, and the sized free's
contract below); `valid_aborts()` alone, where the token is a
parameter the function requires anyway (`Heap::free_slow`,
`free_slow_in`, `push`, `free_other`, `rehome_on_free`, `free_run`,
`Shared::free_huge`, `Heap::free_huge`, `zero_huge`, `release_slab`,
`release_word`), where the batch's tokens are the cache's (`flush`,
`flush_arena`, `gc`, `gc_arena`), or where no abort but a pending
check or a resource failure is left (`dec_live`, `on_live_zero`,
`reopen`, `try_rehome`, `ensure_cache`, `Shared::arena`, and the
decay and purge walks:
`purge::decay`, `decay_one`, `decay_huge`, `purge_all`,
`Arena::decay_with`, `Arena::purge_all`, `ClassState::decay_with`,
`ClassState::purge_empty`, `purge_curve`, `walk_empty`, `EmptyVisitor::visit`,
`page_group_is_idle`, `push_group`, `ClassState::settle`, `Huge::decay_with`,
`Huge::purge_empty`, `purge_curve`, `push_extent`, `purge_desc`, `Huge::settle`, the batch's `Batch::push`, `flush`, `flush_in` and `purge::discard_one`); `valid_aborts()` on a
free's tick only (`Heap::tick::<false>`, `purge::on_tick` and
`background::auto_tick` with `alloc` false: a free's tick never
starts the background thread); `may_abort_pending()` or
`may_abort_kernel()` alone (`SlabTable::desc`, `owner`, `bits`,
`Shared::owner`, `Arenas::unbind`, `SlabDesc::sub_live`, `state`,
`state_held`, `State::from_u8`, `slab::bad_state_byte`; `purge::discard`); or no abort
capability at all, for a slot address (`locate`) and where nothing
aborts (`Table::ensure`, `Arenas::get`, `materialise`, `build`,
`Arena::init`, `alloc_sets`). Every other abort it reaches needs `may_abort()`,
which the witnesses lack, so each is proved unreachable with the token,
or it is a resource failure (`abort_kernel`) or a pending check
(`abort_pending`). So for a live block's
token, whatever the allocator's state that the proofs' invariants
allow, these cannot fire:

- **Not an allocation** (`Shared::classify_free`, `heap/shared.rs`,
  `invalid_free`; `rehome_on_free`'s own `invalid_free` after the move,
  `heap/mod.rs`; and the lookups' `not_an_allocation` in
  `usable_size`, `api/libc.rs`, and `old_size_or_keep`'s and
  `realloc_old`'s): a live block is a slot start or a
  page start of the huge region (`Live::lemma_block_target`,
  `heap/live.rs`), which `Regions::classify` never calls `Invalid`.
- **Slot not live, guard slab** (`SlabTable::check_live`,
  `class/table.rs`, `not_live`: `invalid_free` or `guard_free`):
  the bit test `is_set_unchecked` reads the word through
  `bit_word_held` with the caller's slot token
  (`Live::slot_ref`, `heap/live.rs`; `holds_slot`,
  `class/table.rs`). Inside the word's invariant, which holds the
  tokens of exactly its clear bits (`bitmap::WordPred`), the token is
  disjoint from the word's (`GhostSubset::disjoint_singleton`), so the
  bit is set. `classify_free`, and the lookups (`usable_size`,
  `old_size_or_keep`, `realloc_old`), hand `check_live` the token of the
  live slot at the address `classify` located
  (`Live::lemma_block_slot_addr`, `heap/live.rs`: a live block at a
  slot address of class `c`, a guard slab's too, is the slot of class
  `c`, so `is_slot`; for it `vspec::zero::lemma_slot_unique` takes slot
  addresses).
- **Not a slot start, outside the class region** (`SlabTable::locate`,
  `class/table.rs`, and `ClassState::locate`, `class/mod.rs`,
  `invalid_pointer`): neither fires for a slot address
  (`Regions::lemma_slot_addr_found`); both require `may_abort()` or that
  (with the regions well formed), and need no capability then.
  `Shared::arena_of_slot` (`heap/shared.rs`) requires `may_abort()`
  or a slot address with `valid_aborts()`.
- **Released through another arena** (`ClassState::release_with`,
  `class/mod.rs`, `wrong_arena`): `free_direct` passes the owner
  `classify_free` read (`slot_owner`), so the batch is `owned_slots`
  (`class/mod.rs`) and `release_owned` leaves no slot of another
  arena. `release_routed` (`heap/shared.rs`) follows the owner
  words, and its "changed arena" abort is unreachable.
- **Guard slab, double free in the batch** (`release_owned`,
  `class/mod.rs`: `guard_free`, the scratch bit's `double_free`):
  a live slot is in no guard slab (`live_slots`; theorem 7),
  and a set scratch bit is a slot whose token the batch's accumulated
  tokens hold, disjoint from the one split off for this pointer (for
  every caller: the batch's tokens are distinct). A flush's batch is
  `live_slots`: every cached entry holds a slot of its header's class
  (`lemma_released`, `heap/flush.rs`).
- **Double free at the word** (`release_word`, `class/mod.rs`,
  `double_free`): `AtomicBitmap::release` ensures the released bits
  were set (their tokens were not in the word), so the check fails for
  no caller.
- **A straddling pair at the flush** (`Heap::flush`,
  `heap/flush.rs`, `straddling_double_free`): the lowest released
  address is a released entry's and `kept_max` a kept entry's
  (`lemma_kept_index`, follows it through the kept sort), and
  the cache's entries stand for distinct slots (their tokens are
  distinct), so the compare fails for every caller (`lemma_released`).
- **A size hint that does not name the block**
  (`Shared::check_size_hint`, `heap/shared.rs`, `hint_mismatch` and
  `huge_hint_mismatch`): with the sized free's contract (the premise
  below) the hint names `p`'s class and `size` rounds to the run's
  pages.
- **Re-keying a cache that still holds slots** (`CacheHeader::rekey`,
  `heap/cache.rs`, the trusted `assert_rekey_flushed`,
  `heap/mod.rs`): `try_rehome` (`heap/mod.rs`) re-keys the
  home headers after the home flush emptied them and the `FOREIGN`
  flush kept them.
- **Not a run, already free** (`Huge::free_run`, `huge/mod.rs`, two
  `not_a_run` and `already_free`; `Huge::usable_size`, which
  `zero_on_free`'s `Shared::zero_huge`, `heap/shared.rs`, and the
  lookups reach): the run token's extent is in the lock's run
  authority, which names the `Active` descriptor that starts at `page`,
  keyed there in the table (as `Huge::lookup` shows it, theorem 43).
  `free_run` always holds the token, so its three checks fail for no
  caller (`assert(false)`, as theorem 29's `Inner::extent_at`);
  `usable_size` holds it when its caller does (`holds_run`,
  `huge/mod.rs`). The site of `not_a_run(self.addr(page))` is unreachable, and Verus
  checks the build's call there itself.
- **A run state byte that is no state** (`HugeDesc::state` in
  `Inner::free_before`, `free_after`, `retire`, `huge/mod.rs`, `bad_run_state_byte`): every live descriptor's byte
  is a state (`desc_ok`; `states_ok`).
- **An empty count below zero on a purge** (`ClassState::settle`,
  `class/decay.rs`, `slab_count_underflow` in its `unwrap_or_else`
  closure, which `requires false`): the slab it purges is `Empty`, so in
  the empty set (`slab_core`), whose size the count is (`Inner::base`).
- **An extent count below zero** (`huge_count_underflow`,
  `huge/mod.rs`, through `HugeStats::sub` in `drop_extent`, which
  `requires false`): `sub` requires the extent counted (`can_sub`).
- `Arena::alloc_sets` (`arena.rs`) has no size check: the
  table fits an object by `global layout SetTable` (`arena.rs`,
  which Verus checks against the compiler's), and it requires no abort
  capability.

The resource failures the covered paths reach (`abort_kernel`;
`sys::tests::narrow_aborts_only_at_their_check_sites` checks that it is
called only at the test's listed sites):
- `Huge::free_run`'s `madvise(MADV_GUARD_REMOVE)` of the run's guard,
  with `guard_install`.
- `purge::discard`, the `madvise` of the purged pages: of a huge run
  purged at once (above `huge_retain_max`, or under
  `huge_dirty_decay_ms:0`), and of the slabs and extents the tick's
  decay walks purge; `discard` requires
  `may_abort_kernel()` for it. It aborts only
  on an errno other than `EINVAL`: `madvise` fails with `EINVAL` on
  pages the program locked (`mlock`, `mlockall(MCL_FUTURE)`), and the
  range stays `Empty` and dirty, to be retried, so a valid program
  that locks its memory does not abort its free here. What is left,
  `ENOMEM` (part of one of the allocator's reservations unmapped) or
  `EPERM` (the program sealed the allocator's free memory), is a
  program that touched memory it does not own; that a valid program
  reaches neither is read from the errno list, not proved (`madvise`'s
  errors have no specification). The page-alignment check before the
  call (upstream's `assert!` with `sys::features::page_size()`) is
  proved never to fail, from `well_formed` and the premise that
  `getauxval(AT_PAGESZ)` is the process regions' page (the
  `getauxval` specification, `verus/ledger/src/sys/auxv.rs.md`; init reserves the regions with it), which
  also makes `page_size`'s own abort unreachable, so `page_size`
  requires no `may_abort()`.

Metadata the arena build, the cache stack and the arena's set table
cannot get is no abort: `Shared::arena` and `ensure_cache` return none,
and the free goes straight to the slab.

The pending checks the covered paths reach (`abort_pending`, which
requires `may_abort_pending()`; the helpers' other callers
require `may_abort()`). Each tests
state no verified invariant constrains as the check needs; each fires on an
invalid free, and on a valid one only if that state is corrupt:
- `class::abort::never_committed` (`class/abort.rs`), through
  `SlabTable::check_live`, `desc`, `owner` and `bits`: the slab at or
  above its class's watermark. A live slot's slab was handed out, but
  no invariant ties the watermark to the tokens.
- `owner_mismatch` and `release_into_fresh_slab` (`class/abort.rs`), in `release_owned`: the descriptor's arena against the owner
  word, and a `Fresh` slab (one nothing was claimed from).
- `SlabDesc::sub_live` (`slab.rs`), through
  `dec_live`: the live count would go below zero.
- `slab::bad_state_byte` (`slab.rs`), through `State::from_u8`,
  `SlabDesc::state` and `state_held` (`ClassState::state_locked`,
  `class/mod.rs`: the class lock holder's reads in
  `reopen`, `on_live_zero` and the decay walks): a slab state byte that
  is no state.
- `heap::shared::no_such_arena` (`heap/shared.rs`), through
  `Shared::owner`: the owner word names an arena that was never built.
  (`alloc_huge`'s call, of its caller's arena index, holds `may_abort()`;
  on the free paths `ensure_cache` reads the home arena's and `try_rehome`
  the one an owner word names.)
- The `check_zero` debug mode's `shadow_written` and `release_into_fresh`
  (`class/verify.rs`), through `check_shadow` and
  `dec_live`: a descriptor written outside its API, a release into a
  slab that reads `Fresh`.
- `ClassState::walk_empty`'s "slab in the empty set is
  not empty" (`class/decay.rs`, the tick's decay walk): a slab of
  the empty set whose descriptor is not `Empty` with no live slot (the
  walk reads the state byte raw, not through `state_locked`, and no
  invariant relates the live count to the sets); and
  `arena::unbind_unbound` (`arena.rs`), through
  `Arenas::unbind` in `try_rehome`: the heap holds its arena's ticket,
  so the bound count reads the tickets out as a `u32` wraps it, and
  reads 0 only with a multiple of `2^32` heaps homed there, which no
  verified invariant bounds.
- `huge::table::Table::insert`'s "descriptor table full"
  (`huge/table.rs`), through `free_run`: `free_run`'s `ensure` is best
  effort, and every allocation's `ensure` keeps a key free for the free
  of each live run (`Huge::alloc_aligned`), so a free after a failed
  growth still finds room and reaching the abort means the counts are
  corrupt. That room is argued in `insert`'s doc, not proved, so a free
  that cannot show it holds `may_abort_pending()`.
- With the opt-in `double_free_scan` key on, the slot-state checks of
  `heap/slotstate.rs`: `slotstate::free` on every free route
  (`Heap::free_slow_in` and `free_direct`, `heap/paths.rs`), which
  aborts on a slab past the watermark, a `CACHED` slot or a `FREE`
  slot; `slotstate::release` in `free_slow_in` and `release_all` in a
  flush's release (`Heap::flush`, `heap/flush.rs`), which abort on a
  slot that is not `CACHED`; and `check_not_cached` in the lookups
  (`Shared::check_not_cached`, `heap/shared.rs`), which aborts on a
  `CACHED` slot. Each check's step is verified, over `vspec::byte` on a
  permission `axiom_slot_byte` mints; the state the byte holds is that
  axiom's premise ([double-free window](#double-free-window-under-double_free_scan-rests-on-the-slot-state-premise)),
  and no verified invariant ties the bytes to the caches. Every check
  only adds an abort.

`sys::tests::abort_capabilities_only_as_clauses` checks that
`kernel_aborts` and `pending_aborts` appear only in their definitions,
in `valid_aborts`' and in the witnesses' `requires`.
`sys::tests::narrow_aborts_only_at_their_check_sites` checks that
`abort_kernel` and `abort_pending` are called only at its listed sites.
Its list also has `heap::slotstate`'s `init` and `commit`
(`abort_kernel`) and `free`, `claim`, `pop`, `release` and
`check_not_cached` (`abort_pending`), the `double_free_scan` slot
state. `init`, `commit`, `claim` and `pop` are on init and allocation
paths, not the covered frees; the slot-state checks the frees reach are
listed above.

**Premises beyond the common ones.**
- The C contract of a free (`axiom_c_claim`, "Bad input"): the witnesses
  take the live block's token, `blk.block(p)`, which a free of a block
  the program holds gets from it. Nothing is stated for a free without
  one; an invalid or double free still aborts (theorem 3).
- The C contract of a sized free: the size names the block (`hint` is
  the size's class at the alignment, `hint_ok`, the class the token's
  slot has; for a run, `size` rounds up to the run's usable bytes,
  `huge_hint_ok`), what C23 `free_sized` and jemalloc's `sdallocx` ask
  of their callers. A wrong size still aborts ("Bad input").
- `shared.wf()` stands for init complete, as in theorem 32: `free`'s
  `shared_if_ready` and `free_before_init`, and `heap_for_thread` (which
  creates a heap, and may abort when that fails), come before the calls
  the witnesses make, and are not covered; nor is `mallocx::free`'s
  `invalid_tcache` (a bad `MALLOCX_TCACHE` is "Bad input").
- `abort_kernel` and `abort_pending` (`sys/abort.rs`, verified): the
  relabels of `abort_fmt`'s capability, as `abort_integrity`'s. Each
  body calls `abort_fmt` after `relabel`, a proof private to
  `sys/abort.rs` that reveals `may_abort()` (opaque, and `true`). The
  relabel itself is this theorem's policy:
  `sys::abort::tests::abort_relabels_only_in_the_sinks` checks that only
  the three sinks call `relabel` and that nothing but `grant_aborts`
  reveals `may_abort`, and `relabel` requires one of the narrower
  capabilities. `kernel_aborts()` and `pending_aborts()` are `uninterp` and granted by
  nothing.
- The trusted bodies on these paths abort only as their capability says
  (`owner_locked`, verified, requires `may_abort_pending()` for its call
  of `SlabTable::owner`).
  `sys::tests::trusted_aborts_require_a_capability` checks that each
  `external_body` function whose text shows an abort, a panic macro or
  an `unwrap` requires `may_abort()`, or its listed compound clause; it
  does not see an index panic or an abort through a callee.
- The shipped release profile (overflow checks and debug assertions off),
  as in theorem 32.

**What it does not cover (paths that can still abort for a valid
program).**
- The prefix before init and before a heap: `shared_if_ready`,
  `free_before_init`, `heap_for_thread` (heap creation).
- The allocation `realloc` makes between its lookup and its free of the
  old block (an allocation's checks, `live_above_slots`,
  `slab_count_underflow` on the claim side, `reservation_unmet`), and
  the entry points' own wrappers (`free_sized_block` and the rest, which
  the witnesses replicate after init).
- The pending checks above, which need invariants this theorem does not
  have: the live counts against the bits, the slab states and the
  watermark against the tokens, the live count against the sets, the owner word
  against the registry, the arena bound counts below `2^32`, and the
  `double_free_scan` state bytes against the caches.
- Resource failures: they are the statement's exception (above).

**Where the statement is weaker than the code.**
- The witnesses replicate the calls `free` and the sized frees make
  after their `shared_if_ready` (`free_direct`; `Heap::free` on the
  thread's or an explicit cache's heap; `free_sized_fast`,
  `check_size_hint`, then `Heap::free` or `free_direct`), and
  `Heap::free`'s huge branch,
  not those functions, as theorem 32's do.
- "Never aborts" is about the verified source, as in theorem 32.

Machine code: the sinks `abort_pending` and `abort_kernel` are
`#[inline(always)]` calls of `abort_fmt`, and the token proofs pass only
ghost (`Tracked`) parameters (`classify_free`, `check_located`,
`check_live`, `is_set_unchecked`, `bit_word_held`; `bit_word` calls
`bit_word_held` with no token). The proofs also shape a few blocks: the
`Invalid` arms of `classify_free`, `usable_size`, `old_size_or_keep`
and `realloc_old` are blocks, a `&Inner` borrow of the huge lock guard
in `usable_size` and `free_run` carries the token proof, and the
`unwrap_or_else` closure in `ClassState::settle` has a return type and
`requires false`. The cache entries' disjointness is a precondition
`free_slow_in` and `rehome_on_free` prove, not an executable parameter.

## 45. Fast-path cost bounds (machine level, pinned build)

On the pinned x86_64 release build, every path through the fast branch
of `malloc`, `free`, `free_sized`, `sdallocx` and `challoc_free_usable`
executes at most the table's instructions, explicit memory loads,
explicit memory stores and conditional branches, and touches at most the
table's 64-byte cache lines (P1, P4); no such path has a back-edge (P3);
and no export's whole body holds an atomic read-modify-write, a fence, a
call, a system call or an indirect jump (P2). This is a statement about
one binary, checked by a CFG walker over its disassembly
(`scripts/perf-budget.rs`), not by Verus; it gives no cycle counts.
The `perf-budget` check (`.chainguard/ci/perf-budget.yaml`) runs
`sh scripts/perf-budget.sh q16` and `q8` on every pull request, and
`perf-budget-main` runs them on main after every landing.

**Cost model.** A path is the instruction sequence from the export's
entry to a `ret`. Its latency is at most `a*I + b*L + c*S`, where `I` is
the number of instructions executed, `L` the number of explicit memory
loads, `S` the number of explicit memory stores, `a` bounds one
instruction's latency with every operand in a register, and `b` and `c`
bound the extra latency of one load and one store with its line in L1
and its page in the TLB. An atomic read-modify-write would cost a fourth
term `R`; P2 makes it zero. Branch mispredictions, cache and TLB misses
are not in the model (see "Not claimed").

**Statement** (checked by `sh scripts/perf-budget.sh`, which builds the
library and fails unless every row's maxima equal its `budget` line in
`verus/perf-budget.txt` and every fast path's counts, sorted, equal its
`paths` line, so a cost added to a path below the worst one fails too):

- **P1, budget.** For each row of the table, the maximum over the
  row's fast paths of `I`, `L`, `S` and conditional branches is the
  row's value, so each path's latency is at most
  `a*I + b*L + c*S` with the row's `I`, `L`, `S`. A path ends, and is
  not counted, at an edge into one of the export's exit functions: the
  `#[cold]` `#[inline(never)]` slow paths (`api::libc::malloc_slow`,
  `malloc_slow_class`, `free_slow`, `free_sized_slow`,
  `free_usable_slow`, `api::jemalloc::sdallocx_flags`) and the medium
  paths (`malloc_medium`, `free_sized_medium`, sizes above
  `SMALL_MAX`). A path ending at `int3` or `ud2` (an abort) is not
  counted either; the bodies have none. An edge anywhere else out
  of the body fails the check.
- **P2, no synchronisation or kernel entry.** Over the whole body of
  each export (fast and slow-entry branches, from the symbol's address
  to address + size in the dynamic symbol table): no `lock` prefix, no
  `xchg`, `xadd` or `cmpxchg*`, no `mfence`/`lfence`/`sfence`, no
  `call`, no `syscall`/`sysenter`/`int N`, no indirect jump. The source
  atomics on these paths are all `Relaxed` loads, which compile to plain
  `mov`s: `free`'s watermark copy (`class::load_mirror`), bitmap word
  (`SlabTable::is_set_unchecked`), `Shared::free_flags`
  (`heap/shared.rs`) and, routed, the owner word
  (`class::load_owner`, `class/table.rs`). `malloc`, `free_sized` and
  `sdallocx` read no atomic on x86_64: `push_flags` is a plain `u8`, and
  `malloc`'s `fast_flags` load is dead off aarch64 (`PUSH_PAC` is never
  set). Theorems 8 and 9 state the source-level half (no syscall
  capability, no lock-taking mask); P2 is the machine-level check that
  the compiled bodies add none.
- **P3, constant time.** The graph of each row's fast paths, as the
  walker explores it, has no back-edge: every path is enumerated
  explicitly and a jump to an address already on the path fails the
  check. So the counts do not depend on the cache's length, the number
  of slabs, heaps, threads or arenas.
- **P4, cache lines.** The maximum over the row's fast paths of the
  number of distinct 64-byte lines its explicit accesses touch is the
  row's `lines`. Accesses with a constant address (`rip`-relative
  statics: the GOT word holding the TLS offset, the `Shared` tail with
  `free_flags`, `push_flags`, `region_hi`, `regions.superblock` and
  `lg_stride`) count their exact lines. Accesses through the same base
  expression form a group (the cache header through the heap pointer
  plus `class * 64`; the class line through `SHARED + class * 64`; the
  slot stack entry; the bitmap word; the owner word; `SMALL_LOOKUP`),
  and a group counts the most lines its byte range can cover over every
  base alignment the layout premises allow. Groups are summed, so the
  count is an upper bound. The layout facts it uses: `CacheHeader` is
  `repr(C, align(64))` and 64 bytes (`CACHE_HEADER == 64`,
  `heap/cache.rs`), so `Heap` is 64-byte aligned and
  `caches[c]` is `heap + 128 + 64 * c`; `ClassLine` is
  `repr(C, align(64))` and `CLASS_STRIDE` (64) bytes, at offset 0 of
  `Shared` (`heap/shared.rs`, the `const` asserts after it);
  `SMALL_LOOKUP`'s entries are multiples of `CLASS_STRIDE`
  (`small_entry_ok`, `layout/mod.rs`, proved).

**Measured table** (the `budget` lines of `verus/perf-budget.txt`;
rustc 1.98.1, `quantum16`, `cargo build --release --locked`). `-` is every
configuration (no cell fixed); `free_flags=8` is `FREE_ROUTE` alone (a
second arena exists, no push option), the routed free of theorem 26.

| export | configuration | insns | loads | stores | cond. branches | lines | fast paths |
|---|---|---|---|---|---|---|---|
| `malloc` | any | 34 | 11 | 5 | 7 | 5 | 5 |
| `free` | `free_flags == 0` | 51 | 18 | 2 | 7 | 7 | 1 |
| `free` | `free_flags == FREE_ROUTE` | 62 | 22 | 2 | 9 | 9 | 1 |
| `free` | any | 67 | 22 | 2 | 12 | 9 | 6 |
| `free_sized` | `push_flags == 0` | 40 | 13 | 2 | 6 | 7 | 1 |
| `free_sized` | any | 42 | 13 | 2 | 7 | 7 | 2 |
| `sdallocx` (flags 0) | `push_flags == 0` | 42 | 13 | 2 | 7 | 7 | 1 |
| `sdallocx` (flags 0) | any | 44 | 13 | 2 | 8 | 7 | 2 |
| `challoc_free_usable` | `free_flags == 0` | 56 | 19 | 2 | 8 | 8 | 2 |
| `challoc_free_usable` | `free_flags == FREE_ROUTE` | 67 | 23 | 2 | 10 | 10 | 2 |
| `challoc_free_usable` | any | 72 | 23 | 2 | 13 | 10 | 7 |

`malloc`'s five paths differ only in the `low_water`, `run_end` and
`zeroed` stores (27 to 34 instructions, 2 to 5 stores); its lines are
the `SMALL_LOOKUP` entry, the GOT word, the TLS slot, the cache header
and the stack entry. `free`'s seven (`free_flags == 0`) are the GOT
word, the TLS slot, the `Shared` tail line, the class line, the bitmap
word, the cache header and the stack entry; `FREE_ROUTE` adds the
heap's first line (`heap.arena`) and the owner word. The header and the
stack are separate objects, so two lines, never one.
`challoc_free_usable`'s second path is `challoc_free_usable(NULL)`
(four instructions: `test`, `je`, `xor`, `ret`). The table's `paths`
lines list every fast path's counts; the "fast paths" column above is
their number. The `quantum8` build (`verus/perf-budget-q8.txt`,
`sh scripts/perf-budget.sh q8`) has the same counts; only the cells
move (`free_flags` at `SHARED + 0xe00`, 56 class lines).

**Premises.**
- **Fast path.** `size <= SMALL_MAX` (1024) for `malloc`, `free_sized`
  and `sdallocx`; a heap exists (the TLS slot is above `TLS_EXITED`,
  `cmp ..., 3`); the cache is non-empty (`malloc`) or non-full (the
  frees); the pop is not a scheduled pop (`allocs + 1` has a bit of
  `compact_mask`); the pointer passes the free's slot checks (its class
  region, slot start, committed slab, set bit; for the sized frees, the
  size hint). Each failing check is an edge to an exit, outside P1.
- **Configuration.** `free_flags` (`Shared` offset `0xd00`) and
  `push_flags` (`0xd01`) hold the row's values for the whole call (the
  walker gives every load of the cell the row's value and follows only
  the branch arm the flags then select). A row whose cell no fast path
  reads fails the check, which catches a field that moved away from the
  table's offset, unless another read lands on the old offset. The "any"
  rows need no configuration premise.
- **Machine.** No page fault, preemption or signal during the call;
  cache and TLB costs inside `b` and `c`; the library loaded at a
  page-aligned base (so static addresses' lines are exact).
- **Alignment.** Every explicit access is naturally aligned up to 8
  bytes (Rust's alignment rule for the `u8`..`u64` and pointer fields
  read), the heap
  pointer in the TLS slot is 64-byte aligned (`Heap`'s alignment, on
  x86_64 without PAC; a path with a second access through `fs` fails the
  check), and `SMALL_LOOKUP`'s entries are multiples of 64 (the two
  `align-load` lines of the table). An indexed load from a static is
  attributed to the static its constant part lies in (the tables are
  indexed from their start).
- **Toolchain.** rustc 1.98.1 (the `rust-1.98~1.98.1` package of
  `.chainguard/ci/perf-budget.yaml`), `quantum16` (default features),
  `cargo build --release --locked` with the crate's release profile (fat LTO, one
  codegen unit, `panic = "abort"`), x86_64, disassembled by GNU objdump.
  Another compiler, feature set or target is another binary: the check
  then measures it and fails with its counts.
- **The checker** (`scripts/perf-budget.rs`, trusted): objdump's
  decoding, the classifier (which mnemonics read, write or
  read-modify-write a memory operand; which set the flags), and the
  value tracker (constants through `mov`, `movzx`, `lea`, 64-bit
  `add`/`sub`/`inc`/`dec`, `shl`/`and` by an immediate, `xor` of a
  register with itself, `test`, `cmp` and the flags; addresses as
  linear sums of opaque values, a `rip`-relative one as the load base
  plus its link-time address). Every mnemonic
  outside its tables fails the check, so a new instruction in a hot
  body is classified by hand before it is counted; a path falls through
  by each instruction's encoded length, so a line the parser skipped is
  a gap the walk fails on. `scripts/perf-budget.sh` runs a self-test
  first: five mutations of `malloc`'s disassembly (a register `mov`
  made a load, a `ret` made a `call`, a `ret` made a jump to the entry,
  an unknown instruction, a deleted instruction) and three of the table (a maximum above the
  measured one, a cell at a stale offset, a changed `paths` entry) must
  each fail the check with the matching message. Isla's x86 footprint
  (`verus/ISLA.md`) is not used to certify the classification.

**Not claimed.**
- Cycles, branch mispredictions, cache or TLB misses, store-forwarding
  stalls, false sharing or contention on the lines in P4 (the class
  line and the `Shared` tail are read-shared by every thread; the
  watermark copy and `free_flags` are written on slow paths).
- The slow and medium paths, the exit edges' prefixes (the work done
  before a check fails), `calloc` (reads the init state, has a frame and
  calls `memset` unless the slot is known zero), `realloc` (a jump into
  the core), `free_aligned_sized`, flagged `sdallocx`, the `je_`-prefixed
  copies and `operator new`/`delete`.
- `free(NULL)`: on x86_64 there is no null test; the null pointer fails
  the class-region check after 10 instructions and goes to `free_slow`.
- The `ret`'s read of the return address, which is part of the `ret`
  instruction, not a load in `L`, and its stack line, which is not in
  P4's count (the caller's `call` wrote it).
- aarch64: the walker reads x86_64 only (it refuses another
  architecture); the aarch64 fast paths (PAC, `ldp`, the `shared_base`
  register hint) are not measured.
- Anything about the source: Verus does not see this theorem. The
  link to the source is the codegen rule (`CONTRIBUTING.md`, hot set
  frozen) and theorems 8, 9 and 26, which name the same paths.

## 46. Allocator decisions never depend on user-memory contents (verified code; trusted bodies read from the code)

No allocator control flow reads a byte of a user block, except
`realloc`'s copy and the opt-in `check_zero` debug reads. So an overflow
from one block into a live or freed neighbour, or a write through a
dangling pointer, cannot steer `malloc` or `free`: what the program
writes into its blocks reaches the allocator only as the bytes a
`realloc` copies into the new block (no branch depends on them), and,
with `check_zero` on, as an abort on a non-zero word. This is the proved
form of "no free-list pointers in freed blocks" (`docs/hardening.md`):
challoc keeps no metadata in user blocks, and no decision it makes reads
one. The proof covers the verified code. The trusted bodies are covered
only by reading their code (premises below).

**Statement** (`vspec/user.rs`; `purge.rs`;
`api/libc.rs`):

```rust
#[verifier::opaque]
pub closed spec fn may_read_user() -> bool { true }

pub proof fn grant_user_reads()
    ensures
        may_read_user(),

pub fn check_zero() -> (r: bool)
    ensures
        r ==> crate::vspec::user::may_read_user(),

#[cfg(verus_keep_ghost)]
fn theorem_no_user_reads(which: u8, n: usize, m: usize, flags: c_int, p: *mut u8,
    Tracked(claim): Tracked<CCall>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        claim.block() == Some(p),
```

(Bodies elided, line breaks added.) `may_read_user()` is a capability,
as theorem 8's `may_syscall()` is. Every primitive that reads bytes of
a user block requires it:

- `UserMem::lemma_valid_reads` (`vspec/user.rs`), the one way
  verified code outside `vspec::user` gets a read bound (`valid_reads`)
  on bytes a `UserMem` covers;
- `copy_user` (`vspec/user.rs`), the `copy_nonoverlapping` out of
  the old block;
- and, through them, `class::verify::read_word` (`class/verify.rs`,
  a slot's word), `Huge::read_run_word` (`huge/mod.rs`, a run's
  word, through the permission the hand-out will mint) and
  `libc::copy_moved` (`api/libc.rs`, realloc's copy over the two
  blocks' `Live` tokens). The `check_zero` walks that call the word
  reads require it too: `ClassState::check_zero` and
  `check_fresh_slab_is_zero` (`class/verify.rs`), and
  `Huge::check_zero` (`huge/mod.rs`).

It is granted in two places:

- the realloc cores `libc::realloc_block` (`api/libc.rs`),
  `libc::realloc_usable_stored` and `jemalloc::rallocx_zeroed`
  (`api/jemalloc.rs`), in the proof block just before their
  `copy_moved`. By then the move is decided: the keep test
  (`old_size_or_keep`, `realloc_old`) and the new allocation read no
  byte of `p`.
- `purge::check_zero()`, whose `true` (the `check_zero` config key is
  on) grants it. Its callers that read are the class claim's pick loop
  (`ClassState::finish`, `class/mod.rs`, under a
  loop invariant `check_zero ==> may_read_user()`), the class take's
  `Fresh`-slab check (`ClassState::take`) and
  the huge allocation (`Huge::alloc_aligned`, `huge/mod.rs`,
  under the `prior` that is `Some` only when `check_zero()`
  returned `true`). `check_zero`'s body calls the grant unconditionally;
  only its `ensures` (`r ==>`) passes the capability on, so a caller
  holds it only after a `true`.

Every other verified function is verified without the capability:
allocation, free, refill, flush, purge, decay, stats, `mallctl`, the
fork handlers and the thread destructor. `theorem_no_user_reads`, a
verification-only function (`cfg(verus_keep_ghost)`), witnesses this for the
cores of the allocation and free entry points other than the
reallocs: `malloc_block`, `calloc_zeroed`, `aligned_alloc_block`,
`jemalloc::mallocx_zeroed`, `free_block`, `free_sized_with`,
`free_usable_block`, `jemalloc::dallocx_block`, `sdallocx_block` and
`malloc_usable_size`, slow paths included. It calls them where
`may_abort()` and `may_syscall()` hold but `may_read_user()` does not,
and it verifies. So none of them requires the capability, and below them
only `check_zero()`'s `true` grants it.

**Why the capability covers every read of a user block.**
- A read in verified code needs a permission to its bytes. That is
  either a vstd `PointsTo` (`ptr_ref`, `PointsTo::read`) or, for a raw
  read (`read_volatile`, `read_unaligned`, `copy_nonoverlapping`:
  `vspec/ptr.rs`, `vspec/addr.rs`), a `valid_reads` bound. A
  `valid_reads` bound comes only from `vspec::ptr::axiom_raw_readable`
  of a `PointsToRaw`, from `vspec::cbuf`'s facts about a C argument, or
  from `vspec::ptr::ref_ptr` (a Rust reference, which verified code makes
  only through a permission).
- The only permissions to a user block's bytes are `UserMem`s. They are
  minted at hand-out (`heap::live::axiom_hand_out`, `heap/live.rs`),
  at the C boundary (`axiom_c_claim`) and, lent for reads, to a
  `Fresh` slab's slots (`class::axiom_fresh_slab_mem`,
  `class/mod.rs`, whose one caller is the `check_zero` branch of
  `take`). This is theorem 4's premise "no `PointsToRaw` is ever made
  for the class or huge regions but there". Every other permission
  verified code holds is to metadata, apart from every block (theorem 4),
  or to the `Shared` static.
- A `UserMem`'s `PointsToRaw` is a private field of `vspec::user`, so
  only that module turns it into a bound. The read bound is
  `lemma_valid_reads` (`vspec/user.rs`, through
  `vspec::ptr::axiom_raw_readable`), which requires the capability, and
  the private `lemma_raw_range` gives the permission's raw
  range to `zero_raw` and `copy_raw`. Its callers are `zero_user`,
  whose body is one `write_bytes` and reads nothing, and
  `copy_user`, which requires the capability.
- `may_read_user` is opaque. A verified function can prove it only
  from its own `requires` or a loop invariant that carries it in from
  there, from `grant_user_reads`, or from a `true` that
  `purge::check_zero()` returned.
  `sys::tests::user_read_grants_only_in_listed_callers`
  (`sys/mod.rs`) checks the source: each of the four grant sites
  calls `grant_user_reads` exactly once, and nothing else calls it. That
  `may_read_user` appears in an `ensures` only as `check_zero`'s
  `r ==> may_read_user()` and in the grant's own `ensures`, and
  otherwise only as a `requires` or loop invariant clause that ends with
  it, apart from its definition and the grant's `reveal`, is read from
  the code; no source test checks it (wolfi-dev/challoc#397).

**Premises beyond the common ones.**
- Theorem 4's: metadata permissions lie apart from every block, and
  the mints (`axiom_hand_out`, `axiom_c_claim`,
  `axiom_fresh_slab_mem`) are the only sources of a permission to the
  class and huge regions.
- The trusted bodies read no user block. This is read from the code,
  not proved (a sweep of every `external_body` body and of the code
  outside `verus!`). The trusted bodies the
  allocator runs (`verus/SEAMS.md`) load:
  - metadata: `double_free_scan`'s slot-state bytes
    (`heap::slotstate::step`), `others` nodes, `arena_volatile`, descriptor
    and watermark loads, the load wrappers of `vspec::word`, `count`,
    `entry` and `atom`, and `slice_from_perms`'s bitmap words;
  - the statics: `ClassLine::guard_word`, the class
    table, the feature line, `malloc_message`, `malloc_conf`;
  - TLS (`sys::tls::get`, `get_tp`, `errno`);
  - the rseq area (`rseq::read_cpu_id`) and the loader's rseq globals;
  - the environment (`config::linux::environ_array` and `read_slot`,
    and `byte_at` through `cstr`; `env_var`, which walks it through
    them, is verified).
  Kernel files are read through verified wrappers over
  `raw::syscall*`. The C arguments of `mallctl` are read by verified
  code. `Heap::destroy` is verified (`heap/mod.rs`); its
  flush and frees take slots, not their bytes. (`load_owner`,
  `owner_locked` and `Shared::free_flags`, loads of metadata, are
  verified.)
- Trusted code calls none of the primitives that require the
  capability (`copy_user`, `lemma_valid_reads`, `read_word`,
  `read_run_word`, `copy_moved`): Verus does not hold an `external_body`
  body or code outside `verus!` to a `requires`. Read from the code:
  every caller of each is verified.
- The kernel's reads are not allocator reads. `madvise` and the guard
  installs on a block's pages change its mapping, not a decision.
- The capability, its grant, `check_zero`'s contract and the witness
  are verified.

**What it does not cover.**
- Callbacks. `malloc_stats_print`'s `write_cb`, the `malloc_message`
  hook and the C++ `new_handler` are the program's code. What they read
  is the program's business.
- Caller-supplied buffers. `mallctl`'s `name`, `oldp`, `oldlenp`,
  `newp` and MIB arrays, `malloc_stats_print`'s `opts`, and the
  out-parameters (`posix_memalign`'s `memptr`, `challoc_*_usable`'s
  `usable`) are C arguments (`vspec::cbuf`), not user blocks in this
  theorem's sense. That holds even when the program passes a pointer into
  one of its own blocks: the decision then depends on what the program
  passed, as with any argument.
- Memory serving as a runtime stack or TLS. A thread whose stack or
  static TLS block the program placed in a `malloc`ed buffer
  (`pthread_attr_setstack`) has challoc's TLS heap slot, and the
  allocator's own frames, inside a user block. The allocator reads that
  slot on every call, and an overflow into it is not excluded. Theorem 4
  leaves TLS out for the same reason.
- Machine-level side channels: timing, caches, speculation, and what
  the hardware lets one block's contents affect. The theorem is about
  the reads the source makes.
- Writes. Fills and copies into a block (`zero_user`, `copy_user`'s
  destination) are theorems 4, 6 and 12. This theorem is about reads.
- Faults. A fault on a block's page (a guard page, a purged page) is
  the kernel's, not a read by the allocator.
- Within the two exceptions: `realloc` copies the bytes without
  branching on them (`copy_nonoverlapping`'s length is fixed before the
  copy). The `check_zero` reads decide whether to abort, which is that
  debug mode's purpose; it is off by default.

## Thread exit never reuses a destroyed heap (rests on the slot premise)

After a thread's thread-exit key destructor (`heap_destructor`) frees
its heap, the thread's TLS slot never names that heap again and the
thread builds no second cache. glibc BZ 22111 and BZ 28028 broke this
property: thread exit rebuilt the tcache after shutdown and leaked it.
The section is not numbered.

**Statement.** Every write of a thread's TLS slot is a step its phases
allow (`slot_step`): `TLS_EXITED` is final, a new heap value is written
only over `TLS_CREATING`, and the slot is emptied only from null or
`TLS_CREATING`. In `heap_destructor` the token holds `TLS_EXITED`, with
the heap's permission out of it, just before `Heap::destroy` (an
assertion; that `destroy` comes after the store is read off the code,
since it takes no part of the token). At a token whose slot holds
`TLS_EXITED`, the fast paths' reader (`existing_heap_in`) and the slow
paths' reader (`heap_for_thread_in`, which creates heaps) both return `None` and
leave the token as it was: no heap handed out, no slot write, no
`Heap::create` call; and every value `set` may store over it is
`TLS_EXITED` again. Verus checks each write and each reader; that every
later entry on the thread sees `TLS_EXITED` is the slot premise below.

```rust
// heap/thread.rs
pub ghost enum SlotPhase { Empty, Creating, Live, Exited }
pub open spec fn slot_phase(v: usize) -> SlotPhase   // 0, TLS_CREATING (1), TLS_EXITED (2), above them
pub open spec fn slot_step(old: usize, new: usize) -> bool {
    &&& (slot_phase(old) is Exited) ==> (slot_phase(new) is Exited)
    &&& (slot_phase(new) is Live) && new != old ==> (slot_phase(old) is Creating)
    &&& (slot_phase(new) is Empty) ==> (slot_phase(old) is Empty) || (slot_phase(old) is Creating)
}
pub proof fn lemma_exited_final(old: usize, new: usize)
    requires old == TLS_EXITED, slot_step(old, new),
    ensures new == TLS_EXITED,

// sys/tls.rs (trusted asm; the requires binds its verified callers)
pub fn set(value: *mut c_void, Tracked(slot): Tracked<&mut TlsCell>)
    requires crate::heap::slot_step(old(slot).value(), value as usize),
    ensures final(slot).set_to(*old(slot), value as usize),

// heap/thread.rs, in heap_destructor after the store, before Heap::destroy
assert(tls.value() == TLS_EXITED && tls.loan.heap() is None && slot_ok(tls.cell, tls.loan));

// heap/thread.rs (verification only, cfg(verus_keep_ghost))
fn theorem_exit_no_rebuild(shared: &Shared, sp: *const Shared, Tracked(tls): Tracked<&mut TlsSlot>)
    requires may_abort(), may_syscall(), shared.wf(), sp == Shared::storage() as *const Shared,
        slot_ok(old(tls).cell, old(tls).loan), old(tls).value() == TLS_EXITED,
    ensures *final(tls) == *old(tls),
        forall|v: usize| slot_step(old(tls).value(), v) ==> v == TLS_EXITED,
{
    assert forall|v: usize| slot_step(old(tls).value(), v) implies v == TLS_EXITED by {
        lemma_exited_final(old(tls).value(), v);
    }
    let fast = existing_heap_in(sp, Tracked(&mut *tls));
    assert(fast is None);
    let slow = heap_for_thread_in(shared, Tracked(&mut tls.cell), Tracked(&mut tls.loan));
    assert(slow is None);
}
```

**Covers.** Every verified slot write: `heap_destructor` (any phase to
`Exited`), `create_heap_for_thread` (`heap/thread.rs`: empty to
`Creating` after `Heap::create`; `Creating` to the heap value, or to
null when the thread-exit key cannot take the heap), and the background
thread's `create` (`purge/background.rs`: a heap value parked on
`Creating` across `pthread_join` and `pthread_create`, then put back).
The readers: `existing_heap` and `tls_heap_slot` (the `malloc`, `free`,
`free_sized` and `challoc_free_usable` fast paths), `heap_for_thread`
and the entries that call `heap_for_thread_in` with their own token
(`heap::paths::alloc`, `alloc_declined`, `alloc_zeroed`,
`heap::mallocx::alloc`). `tls_heap_slot` returns `None` at a sentinel
by its own `ensures`. `ThreadKey::set`, the only path to
`pthread_setspecific`, is called only by `create_heap_for_thread`, at
`Creating`, so after exit the key is not registered again and the
destructor does not run twice for one heap.

**Proof.** Each writer proves `slot_step` from the value its cell holds:
the creation writes only after reading null (its `slot != 0` guard),
and the background `create` parks only a value above the sentinels and
writes it back while the cell holds `TLS_CREATING`, which the trusted
`pthread_join` and `spawn` keep (`TlsCell::kept`: a non-empty cell is
unchanged). The theorem follows from the readers' `ensures`: at a
`reentry_benign` value both return `None` with the token unchanged.

**Premises.**
- The slot premise (`TlsSlot::entry`, `sys/tls.rs`): each top-level
  user's token holds the slot as the last verified writer left it.
  Ghost state does not cross from one C call to the next, so the step
  from "`heap_destructor` left `TLS_EXITED`" to "every later entry's
  token holds `TLS_EXITED`" is this premise plus `lemma_exited_final`
  at every write in between; Verus checks each write, not the sequence.
- One token per user: two `TlsSlot::entry` calls in one verified
  function would give two tokens, and a write through a stale one would
  be checked against a value the slot no longer holds. The source test
  `sys::tls::tests::tls_entry_only_at_listed_users` (`sys/tls.rs`)
  checks that only the listed users call it, once each (twice in
  `heap::mallocx::alloc` and in `theorem_no_syscall_on_fast_paths`, one
  per `match` arm). Not checked: that a nested user (the allocator
  re-entered from `pthread_setspecific`, `pthread_join` or
  `pthread_create`) is lent the outer token, and that a user is done
  with its token before another top-level user runs (the two frees'
  slow paths).
- The nested-entry specs, trusted: `pthread_setspecific`
  (`heap/shared.rs`) leaves a sentinel unchanged; `pthread_join`
  and `spawn` (`purge/background.rs`) keep a non-empty cell
  (`TlsCell::kept`).
- The slot accessors (`get`, `get_tp`, `set`): inline asm, partly
  checked by Isla (`verus/isla/seams.md`); that only these sequences
  touch the slot (no exported accessor; `shared_object_uses_initial_exec_tls`
  checks the dynamic symbols). The tests write the slot with
  `Tracked::assume_new()`, outside the proof.
- That glibc runs the key destructor on the exiting thread, once per
  registered value, and nothing allocator-related runs on that thread
  after its static TLS is gone.

**Not covered.** Explicit caches (`tcache.create`/`tcache.destroy`)
are reached through `Tcaches`, not the slot; that a destroyed one is
not used again is `tcaches::axiom_heap_use`'s premise. The
destructor's flush and free of the heap itself are `Heap::destroy`'s
contract.

**Mutations** (each rejected by `cargo verus focus`; the exec ones also
run on the base tree, where they verify):

| id | change | rejected by |
|---|---|---|
| S1 | `slot_step` without "`Exited` is final" | `lemma_exited_final` postcondition |
| X1 | `heap_destructor` stores `p` back after `Heap::destroy` | `set` precondition |
| X2 | `create_heap_for_thread` without its `slot != 0` guard | `set` precondition (the `Creating` store), and its `ensures` |
| X3 | the creation publishes the heap from the empty slot, skipping `Creating` | `set` precondition (3 stores), `ThreadKey::set` precondition |
| X4 | the background `create` parks any non-null slot, `TLS_EXITED` included | `set` precondition |
| X5 | the background `create` writes `saved` back even when it parked nothing | `set` precondition |
| V1 | the theorem asserts `existing_heap_in` returns `Some` at `TLS_EXITED` | assertion |

## A lookup holding destroy's bound finds no destroyed explicit cache (rests on `vspec::entry`'s write list)

An explicit cache (`tcache.create`) is a heap owned by no thread, found
by id in the `Tcaches` table; `tcache.destroy` empties its entry and
frees the heap. A lookup of the id that holds destroy's lower-bound
token finds no cache, and so does not reach the freed heap, until
`tcache.create` hands the id out again.

**Statement.** Each table entry carries a ghost generation, even while
the entry is null and odd while it holds a cache, which never moves
back: the table's invariant (`TablePred`, `heap/tcaches.rs`) holds one
`vspec::entry` permission per entry, and every lower bound it has
issued for an entry is at most that entry's generation. `destroy`
swaps the entry to null inside the invariant, before the heap is freed,
and leaves in its `TableWrite` the record of that write (the entry's
atomic, null, an even generation) with a lower-bound token for that
generation. The caller copies the token into its `HeapUse`
(`watch_from`), and `get` loads the entry inside the same invariant,
recording the entry's atomic, the address read and the generation,
checked against the token (`floor`). Then the generation `get` read is
at least the one `destroy` left; if `get` found a cache, it read a later
generation; so a `get` that reads the generation `destroy` left finds no
cache. `destroy_then_get` (verification only) calls `destroy`,
`watch_from` and `get` in that order and discharges the theorem's
premises.

```rust
// heap/tcaches.rs
pub proof fn theorem_tcache_destroyed_none(id: u16, tracked w: &TableWrite, tracked u: &HeapUse, found: bool)
    requires
        (id as usize) < MAX_TCACHES,
        w.last() matches Some(cleared) && cleared.index == id as int,
        u.read() matches Some(read) && read.index == id as int && (found <==> read.addr != 0)
            && read.floor == Some(w.last()->Some_0.generation) && read.atomic == w.last()->Some_0.atomic,
    ensures
        u.read()->Some_0.generation >= w.last()->Some_0.generation,
        found ==> u.read()->Some_0.generation > w.last()->Some_0.generation,
        u.read()->Some_0.generation == w.last()->Some_0.generation ==> !found,
```

**Premises.**
- That a later, odd generation means a `create` of `id`: an entry turns
  odd only when a non-null pointer is stored in it, and the table's
  only such store is `Tcaches::create`'s, at the id it returns. This
  rests on `vspec::entry`'s list of writes and on reading this module,
  not on a specification; `tests/loom/tests/access.rs` checks that the
  table is written only by `Tcaches::init`'s `put_entry` and through
  `vspec::entry::store` in `create` and `vspec::entry::swap` in
  `destroy`.
- `vspec::entry`'s wrapped atomic operations (`store`, `load`, `swap`,
  `vspec/entry.rs`) and its identity rules. The proof model is
  sequentially consistent: both records come from the table's
  invariant, so neither the orderings of the accesses nor a history of
  the table's writes are premises, and the chosen orderings are not
  proved sufficient.
- `axiom_tcaches_assemble`, with which `Tcaches::init` builds the
  invariant over the entries `vspec::entry::new` made, and
  `axiom_tcaches_layout`, discharged by a const assertion.
- The caller contract: an explicit cache is used by one thread at a
  time and not concurrently with its `tcache.destroy`, as in jemalloc
  (`Tcaches::get`).

**Not covered.** A `get` that holds no lower bound from the `destroy`,
which includes every `MALLOCX_TCACHE(id)` path: `heap::mallocx`'s
`alloc`, `alloc_huge` and `free`, `api::jemalloc`'s `sdallocx_routed`
and `Tcaches::flush` make their tokens with `axiom_heap_use` alone, so
that they do not use a destroyed cache stays that axiom's premise.

**Mutations** (`cargo verus focus --locked -- --verify-only-module
heap::tcaches`, each on `heap/tcaches.rs`):

| id | change | rejected by |
|---|---|---|
| M1 | `destroy` swaps in the entry's current value instead of null | assertion (the invariant) |
| M2 | `destroy` loads the entry instead of swapping it | assertion |
| M3 | `get` skips its null check | postcondition |
| M4 | `get` uses a plain load outside the permission | `HeapUse` type invariant |
| M5 | `get` loads entry `i + 1` with entry `i`'s permission | precondition |
| M6 | `create` does not advance the generation | assertion (parity) |
| M7 | the theorem without its `floor` premise | postcondition |
| M8 | `destroy` records the generation from before its swap | `TableWrite` type invariant |
| M9 | `create` moves the generation back, to 1 | assertion (bound) |
| M10 | `get` records the floor without checking the token | assertion |
| M11 | `destroy` takes its lower-bound token before the swap | invariant, assertion, `TableWrite` type invariant |
| M12 | `get` takes another entry's token as its floor | `HeapUse` type invariant |
| M13 | `create` records no bound at its new generation | invariant |
| M14 | `destroy_then_get` reads through another `Tcaches` whose bound location equals this one's | the theorem's precondition (`read.atomic`) |

M14 with the theorem's premise lacking `read.atomic == ...` verifies, so
that conjunct is what rejects it.

## Sized frees run plain `free`'s check under `exact_sized_free` (rests on the C-boundary assumption)

`exact_sized_free` is an opt-in hardening key (`config.rs`;
[docs/hardening.md](../docs/hardening.md), (1)). With it, the sized
frees check the slot at the call through plain `free`'s exact path.

**Statement.** With the key, the hinted fast push declines every
pointer and leaves the heap and the caller's token unchanged, so a
sized free goes on to the size check and the exact free, whose
`Shared::classify_free` tests the slot's slab and bit
(`SlabTable::check_live`, `class/table.rs`) before anything is cached
or released. Verus checks the decline and the unchanged heap and token
(the assertion) and the composed route's `ensures`, which are the same
as without the key; that the exact free's test aborts on a clear bit is
read from the code.

```rust
// heap/shared.rs, in Shared::built (part of Shared::wf; Shared::init makes it from ClassLine::of)
self.lines[c].guard_mask == if self.config.exact_sized_free { 0 } else { crate::layout::guard_geom(c).1 }

// heap/shared.rs (a guard mask of 0 makes the guard test match every slab)
pub(super) fn sized_slot(&self, p: *mut u8, hint: Option<usize>) -> (r: Option<usize>)
    ensures self.exact_sized_free() ==> r is None, ...

// heap/paths.rs
pub fn free_sized_fast(shared: &Shared, h: &mut Heap, p: *mut u8, hint: Option<usize>, flags: u8,
    Tracked(blk): Tracked<&mut Live>) -> (r: bool)
    ensures shared.exact_sized_free() ==> !r,
        shared.exact_sized_free() ==> *final(h) == *old(h) && *final(blk) == *old(blk), ...

// heap/paths.rs (verification only, cfg(verus_keep_ghost))
fn theorem_exact_sized_free(shared: &Shared, h: &mut Heap, p: *mut u8, size: usize, hint: Option<usize>,
    Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires may_abort(), may_syscall(), shared.wf(), shared.exact_sized_free(), old(h).wf(),
        hint matches Some(c) ==> c < NCLASSES, blk.claims(p),
    ensures final(errno).val() == old(errno).val(), free_target(p), hint_ok(p, hint), blk.block(p),
{
    let tracked mut blk = blk;
    let ghost (h0, b0) = (*h, blk);
    if hint.is_some() {
        let pushed = free_sized_fast(shared, h, p, hint, shared.push_flags(), Tracked(&mut blk));
        assert(!pushed && *h == h0 && blk == b0);
    }
    shared.check_size_hint(p, size, hint, Tracked(&blk));
    h.free(shared, p, Tracked(blk), Tracked(errno));
}
```

**Covers.** Every sized free that has a fast push: `free_sized`,
`free_aligned_sized` and the sized `operator delete`s (through
`free_sized_block`), and `sdallocx` on the thread's cache or an explicit
one (`sdallocx_flags`, `sdallocx_routed`). The theorem composes the
steps on a given heap, as `theorem_valid_sized_free_never_aborts`
does; the real slow paths (`free_sized_slow`, `sdallocx_routed`) reach
that heap through `heap::free` or `mallocx::free`. A sized free that
finds no heap, or names no cache (`MALLOCX_TCACHE_NONE`), makes no fast
push and reaches the same check through `free_direct` or
`heap::free`. A huge size has no fast push with the key or without it.

**Premises.**
- The C-boundary assumption (`axiom_c_claim`): the caller's token
  claims `p`, as for every free path.
- The key is the configuration `init` applied: `Shared::wf` names it
  (`Shared::exact_sized_free`, `heap/shared.rs`); `Shared::init` proves
  each class line's guard mask from it (`ClassLine::of`), and
  `Shared::built` states it.

**Not covered.** The value of the bit the exact free loads: no
`ensures` states it, for plain `free` either; that a clear bit or a
slab not handed out aborts is read from `SlabTable::check_live`
(which states the guard-slab case in its `ensures`).

**Mutations** (each rejected by `cargo verus focus`; X2 is the one exec change):

| id | change | rejected by |
|---|---|---|
| V1 | the theorem without the `exact_sized_free()` premise | assertion |
| X2 | `ClassLine::of` keeps the table's guard mask under the key | `ClassLine::of` postcondition |
| S3 | `Shared::built` names the table's guard mask whatever the key | `Shared::sized_slot` postcondition |
| S4 | `free_sized_fast`'s unchanged heap and token claimed without the key | `free_sized_fast` postcondition |
| S5 | `free_sized_fast` states the unchanged heap only | assertion |
| V6 | the theorem asserts that the fast push pushed | assertion |
| S7 | `Shared::sized_slot` without `exact_sized_free() ==> r is None` | `free_sized_fast` postconditions |
| S8 | `ClassLine::lemma_sized_ok` without its nonzero guard mask | `Shared::sized_slot` postcondition |

## Double-free window under `double_free_scan` (rests on the slot-state premise)

`double_free_scan` is an opt-in hardening key, off by default
([docs/hardening.md](../docs/hardening.md), (1)): one state byte per
slot, `FREE`, `CACHED` or `LIVE`, which every transition of the slot
moves with one `step` (`heap/slotstate.rs`). This section states how
long a slot's second free is caught at the call.

**Statement.** Given the slot-state premise below, after a free of a
slot returns, every later free of that slot aborts until the slot is
handed to the program again: by a cache pop (`CACHED` to `LIVE`) or by
`Shared::alloc_direct_state`'s claim (`FREE` to `LIVE`). In between, the byte is
`CACHED` or `FREE` whatever other steps the code takes on it (a flush's
release, a refill's claim), and the free's step returns that value, on
which `free` aborts. Verus checks two parts: `free`, `claim`, `pop` and
`release` each return only when their step found the state the
transition starts from and left the one it ends in (an assertion at
each one's end), and the sequence lemma over the code's transitions,
which no code calls. That the byte holds, at each entry point, the value
the last step on that slot left is a premise no contract encodes.
The byte operations are `vspec::byte`'s wrappers (`load`, `store`,
`compare_exchange`), on a permission one axiom mints for the byte.

```rust
// heap/slotstate.rs
pub open spec fn transition(from: u8, to: u8) -> bool {
    ||| from == FREE && (to == CACHED || to == LIVE)    // claim
    ||| from == CACHED && (to == LIVE || to == FREE)    // pop, release
    ||| from == LIVE && (to == CACHED || to == FREE)    // free
    ||| from == NONE && to == NONE                      // check_not_cached's read
}
pub open spec fn after(held: u8, from: u8, to: u8) -> u8   // to if held == from, else held
pub open spec fn seen(held: u8, from: u8) -> u8            // MOVED if held == from, else held
pub open spec fn run(s: u8, ops: Seq<(u8, u8)>) -> u8      // after, step by step, from s
pub open spec fn hands_out(s: u8, ops: Seq<(u8, u8)>) -> bool {
    exists|i: int| 0 <= i < ops.len() && ops[i].1 == LIVE && run(s, ops.take(i)) == ops[i].0
}

// heap/slotstate.rs
pub proof fn theorem_double_free_window(to: u8, ops: Seq<(u8, u8)>)
    requires
        to == CACHED || to == FREE,
        forall|i: int| 0 <= i < ops.len() ==> transition(ops[i].0, ops[i].1),
        !hands_out(to, ops),
    ensures
        run(to, ops) == CACHED || run(to, ops) == FREE,
        seen(run(to, ops), LIVE) == run(to, ops),

// heap/slotstate.rs (every transition; requires transition(from, to));
// obs is the value the byte held and the one it holds after
fn step(shared: &Shared, class: usize, p: *mut u8, from: u8, to: u8, exchange: bool,
        Tracked(obs): Tracked<&mut Ghost<(u8, u8)>>) -> (r: u8)
    ensures
        r != PAST ==> is_state(final(obs)@.0) && r == seen(final(obs)@.0, from)
            && final(obs)@.1 == after(final(obs)@.0, from, to),

// heap/slotstate.rs, minted in step for the byte `byte` computed;
// the operations are vspec::byte::{load, store, compare_exchange} on it
axiom fn axiom_slot_byte(b: &AtomicU8) -> (tracked perm: BytePerm)
    ensures perm.id() == byte_id(b), is_state(perm.value()),

// heap/slotstate.rs, at the end of free, claim, pop, release
assert(obs@.0 == LIVE && obs@.1 == to);      // free: to is CACHED or FREE
assert(obs@.0 == FREE && obs@.1 == to);      // claim: to is CACHED or LIVE
assert(obs@.0 == CACHED && obs@.1 == LIVE);  // pop
assert(obs@.0 == CACHED && obs@.1 == FREE);  // release
```

**Covers.** The five entry points (`free`, `claim`, `pop`, `release`,
`check_not_cached`) and `step`. Verus checks that each passes `step` a
pair `transition` lists (its `requires`), so the free's `to` is `CACHED`
or `FREE` and a claim's `CACHED` or `LIVE`; a hand-out (`to == LIVE`)
is then made only by `pop` and a claim to `LIVE`. Read from the code,
not proved: that `step` is the only code that reads or writes a state
byte (the module keeps the arrays private); that only
`Shared::alloc_direct_state` claims to `LIVE` (`heap/shared.rs`);
and that every free route calls `free` first under the key: the cached
free's slow path (`Heap::free_slow_in`, `heap/paths.rs`, before the slot
reaches a cache or its slab) and the direct free (`free_direct`,
`heap/paths.rs`). The fast push declines under the key by its `ensures`
(`Heap::push_cache_flags`, `heap/cache.rs`).

**Proof.** `step` follows from the `vspec::byte` wrappers' contracts on
the minted permission; each entry point's assertion follows from
`step`'s `ensures` and the byte holding a state, since `seen` returns
`MOVED` only for a byte that held `from` (a state is never `MOVED`).
The theorem is an induction on `ops`: a step that moves the byte from
`CACHED` or `FREE` either moves it to `LIVE`, which `hands_out`
excludes, or to `CACHED` or `FREE` (`transition`; `NONE` moves only
from `NONE`, which no byte holds).

**Premises.**
- The slot-state premise: at each step the slot's byte holds a state
  (`axiom_slot_byte`, `heap/slotstate.rs`, whose contract says only
  this), the one the last step on that slot left (`FREE` before the
  first: the array is zero-filled). Each step mints a fresh permission;
  ghost state does not cross from one call to the next, so the step from
  "this free left `to`" to "the next step on the slot finds what the
  steps since left" is this premise. Verus checks each step, not the
  sequence. No verified invariant ties the bytes to the caches or the
  bitmap.
- The source test
  `heap::slotstate::tests::slot_byte_axiom_is_called_only_in_step` pins
  the mint to `step`, once. The slot-state bytes are not made by
  `vspec::byte::new`; this mint is the only other source of a `BytePerm`
  (`vspec/byte.rs`'s trust list names it).
- The operations: `vspec::byte::load`, `store` and `compare_exchange`
  (`core`'s load, store and strong compare-and-exchange, partly checked
  by Kani in `src/kani/atomic.rs`); each returns the operation's result,
  so no proof can supply one. That the byte at the address `byte`
  computes is that slot's is `byte_at`'s trusted contract.
- Atomicity: the free's compare-and-exchange makes two simultaneous
  frees of one slot find `LIVE` once (`tests/loom/src/slotstate.rs`
  models the race); the claims, pops and releases load and store, and
  only the thread that holds the slot makes them.

**Not covered.** A free after the slot is handed out again passes: the
slot is the program's, and nothing tells the two frees apart. A free of
a slot whose slab is past the watermark aborts without a read (`PAST`),
but no ghost state follows it. `realloc`, `malloc_usable_size` and
`sallocx` take `check_not_cached`, which aborts on `CACHED` only (a
`FREE` slot fails their bit test); no assertion is stated for it. With
the key off nothing here runs.

**Mutations** (each rejected by `cargo verus focus`, module
`heap::slotstate`):

| id | change | rejected by |
|---|---|---|
| M1 | the theorem without `!hands_out(to, ops)` | assertions in its proof |
| M2 | the theorem without "every step is a `transition`" | assertions in its proof |
| M3 | `transition` admits `CACHED` to `NONE` | the theorem's postcondition |
| M4 | the theorem's `requires to == CACHED \|\| to == FREE` becomes `to == CACHED \|\| to == LIVE` | the theorem's postconditions |
| M5 | `step` returns `MOVED` on a failed compare-and-exchange | `step`'s postcondition |
| M6 | `step`'s load path returns `MOVED` without the store | `step`'s postcondition |
| M7 | `axiom_slot_byte` without "the byte holds a state" | `step`'s postcondition (`is_state`) |
| M8 | `free` without its abort on `FREE` (the last `else if`) | `free`'s assertion |
| M9 | `free` steps from `CACHED` instead of `LIVE` | `step`'s precondition and `free`'s assertion |
| M10 | `claim` without `requires to == CACHED \|\| to == LIVE` | `step`'s precondition |
| M11 | `step` exchanges from `to` to `from` (arguments swapped) | `step`'s postcondition |
| M12 | `pop` steps from `FREE` instead of `CACHED` | `pop`'s assertion |
| M13 | `release` hands the slot out (`CACHED` to `LIVE`) | `release`'s assertion |
| M14 | `free` admits `to == LIVE` | `step`'s precondition |
| M15 | `step`'s load is `core`'s, not `vspec::byte::load` | `step`'s postcondition |

## Interface contract

The per-call theorems above (1-3, 5-7 and the "Bad input" section)
checked against challoc's exported C ABI; the whole-allocator ones are
the internal rows below (I1-I7: theorems 1, 4, 5, 7, 19, 8 and 10). One
row per exported symbol (`je_` twins and C++ array forms share their
row), one column per interface property. Read from the `ensures` of
each entry point's Rust function and of its export. All the
trusted premises listed above apply to every cell. Every `extern "C"`
wrapper of the libc, jemalloc, C++, glibc, report and `mallctl` entry
points is in `verus!`. It only casts, mints the call's C tokens
(`axiom_c_call`, `axiom_c_out`) and, for an entry point that takes one,
lends the calling thread's `errno` token (*(errno)* below); the ghost
`grant_syscalls` of theorem 8, `grant_aborts` of theorem 32 and, in the
13 `api::libc` bodies whose entry point states `errno`,
`axiom_errno_at_return` (*(errno)*) run nothing. Its own `ensures`
restates part of the Rust function's, and the nullable `api::libc`
exports state its `errno` failure values as the `errno` the C caller
reads (`c_errno()`); glibc's exports (`api/glibc.rs`) and the
`mallctl` exports (`api/mallctl.rs`) state none of their own.
`api::tests::exports_return_their_callees_result` (`api/mod.rs`)
checks that every export body in `api::libc`, `api::cxx` and
`api::jemalloc` is that setup, one call of its Rust entry point, and
that call's result returned unchanged. So the Rust function's `ensures`
is the symbol's contract, and the verified parts of its body (code
inside `verus!` without `external_body`) cannot panic or overflow. That
is not all of every body: the report rows (32, 33) write through the
trusted `call_back`, `write_file` and `call_hook`, and every row rests on the trusted functions and axioms
of the common premises. Where that function is `.0` of a `_zeroed` twin (`calloc`,
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
cells.

Properties:

- **R**: the result fits and is aligned (`heap::fits`, `heap/mod.rs`);
  a slab result is never in a guard slab (theorem 7).
- **D**: the result is disjoint from every other live block: the core
  returns the token of the block (`Live::gives`), and two live tokens
  are disjoint (`Live::theorem_apart`, theorem 5).
- **Z**: the zeroing entry points return a `Zeroed` token covering the
  bytes the API promises zero (theorem 6: token coverage at return, not
  proved contents); plain `calloc` covers only its `n * m` bytes.
- **C**: contents are preserved (realloc family).
- **F**: failure semantics: NULL, `errno`, return code, the old block
  intact after a failed realloc, overflow in `calloc`/`reallocarray`.
  P means NULL comes exactly from the failure paths, with `errno` set;
  exhaustion has no model, so it does not mean "NULL only when memory
  is exhausted". "Old block intact" means its `Live` token comes back
  (ownership and liveness), not that its bytes are unchanged.
- **B**: bad input. A call that returns satisfies the pointer-shape
  checks (`free_target`) and the selected argument checks (size hints,
  cache ids of `MAX_TCACHES` or more, alignments); for other input the
  call does not return normally (the code aborts; reaching `SIGABRT`
  rather than hanging is not proved), or it gets the documented error
  result or no-op. Pointer shape needs no C contract for the call
  itself (a non-block pointer's token claims nothing); it, like every
  theorem, holds only in a run that has made no earlier non-live free,
  which `axiom_c_claim` excludes. A free target is never in a guard
  slab (theorem 7), so a free into one does not return. The
  *(live)* cells (a stale or double free) stay `p`: the free cores
  require the token of a live block and ensure that what they freed was
  live, but C holds that token only by `axiom_c_claim` (the `free`
  contract), so a non-live free is excluded by that C-boundary
  assumption rather than proved to abort. The "Bad input" section above
  lists every kind of bad input by entry point, with what is proved not
  to return (pointer shape, size hints, alignments, cache ids of
  `MAX_TCACHES` or more) and what rests on the C contract. For the
  read-only entry points B is the converse, theorem 32 ("Read-only APIs
  never abort"): no argument makes the call abort (row 35; the covered
  reads of row 34).
- **U**: size consistency: when `mallocx(size, flags)` succeeds,
  `nallocx(size, flags)` equals its usable size, and `nallocx` is 0
  for a request no placement can serve, which `mallocx` always rejects
  (a run past the huge region, or an alignment no page of it
  satisfies: `nallocx(1, MALLOCX_LG_ALIGN(63))` is 0, as jemalloc's);
  `sallocx` and `malloc_usable_size` give the block's real usable size.

Codes: `P` proved, `p` partial, `-` missing, `.` not applicable.

| #  | Entry point                                 | R | D | Z | C | F | B | U |
|----|---------------------------------------------|---|---|---|---|---|---|---|
| 1  | `malloc`                                    | P | P | . | . | P | . | . |
| 2  | `calloc`                                    | P | P | P | . | P | . | . |
| 3  | `realloc`                                   | P | P | . | p | P | p | . |
| 4  | `reallocarray`                              | P | P | . | p | P | p | . |
| 5  | `aligned_alloc`, `memalign`                 | P | P | . | . | P | P | . |
| 6  | `posix_memalign`                            | P | P | . | . | P | P | . |
| 7  | `valloc`                                    | P | P | . | . | P | . | . |
| 8  | `pvalloc`                                   | p | P | . | . | P | . | . |
| 9  | `malloc_usable_size`                        | . | . | . | . | P | p | p |
| 10 | `free`                                      | . | . | . | . | P | p | . |
| 11 | `free_sized`, `free_aligned_sized`          | . | . | . | . | P | p | . |
| 12 | `challoc_malloc_usable`                     | P | P | . | . | P | . | P |
| 13 | `challoc_calloc_usable`                     | P | P | P | . | P | . | P |
| 14 | `challoc_realloc_usable`                    | P | P | . | p | P | p | P |
| 15 | `challoc_free_usable`                       | . | . | . | . | P | p | P |
| 16 | `mallocx`                                   | P | P | P | . | p | p | . |
| 17 | `rallocx`                                   | P | P | P | p | p | p | . |
| 18 | `xallocx`                                   | . | . | . | . | . | p | p |
| 19 | `sallocx`                                   | . | . | . | . | . | p | p |
| 20 | `dallocx`                                   | . | . | . | . | . | p | . |
| 21 | `sdallocx`                                  | . | . | . | . | . | p | . |
| 22 | `nallocx`                                   | . | . | . | . | P | . | P |
| 23 | `operator new`, `new[]`                     | P | P | . | . | P | . | . |
| 24 | `new`, `new[]` (`nothrow_t`)                | P | P | . | . | p | . | . |
| 25 | `new`, `new[]` (`align_val_t`, 4 forms)     | P | P | . | . | p | P | . |
| 26 | `delete`, `delete[]` (unsized, 8 forms)     | . | . | . | . | . | p | . |
| 27 | `delete`, `delete[]` (sized)                | . | . | . | . | . | p | . |
| 28 | `delete`, `delete[]` (sized, aligned)       | . | . | . | . | . | p | . |
| 29 | `malloc_trim`                               | . | . | . | . | P | . | . |
| 30 | `mallopt`                                   | . | . | . | . | P | . | . |
| 31 | `mallinfo`, `mallinfo2`                     | . | . | . | . | . | . | . |
| 32 | `malloc_stats`, `malloc_info`               | . | . | . | . | P | . | . |
| 33 | `malloc_stats_print`                        | . | . | . | . | . | - | . |
| 34 | `mallctl`, `mallctlnametomib`, `..bymib`    | . | . | . | . | - | p | . |
| 35 | `get_defrag_hint`                           | . | . | . | . | P | P | . |
| 36 | `challoc_version`, `challoc_features`       | . | . | . | . | . | . | . |
| 37 | `malloc_conf`, `malloc_message`, `je_..`    | . | . | . | . | . | . | . |

Notes that recur below:

- *(D)*: the entry point's `_block` core ensures `final(blk).gives(r,
  size)`: for a non-null result, `blk` is the token of the live block at
  `r` (`Live::block`) with at least the request's usable bytes. Two live
  tokens are disjoint (`Live::theorem_apart`, `heap/live.rs`), and a
  cached slot is no live block (`Heap::theorem_uniqueness_live`,
  `heap/mod.rs`), so the result shares no byte with any other
  block the program holds, under the `free` contract for C's blocks.
  Slot tokens come from the class authorities (theorem 5); run tokens
  from the huge allocator's (`huge::RunToken`, `huge/mod.rs`).
- *(errno)*: the model is `vspec/errno.rs`. One ghost resource,
  `ThreadErrno`, stands for the calling thread's `errno` for one call;
  `val()` is the value challoc last wrote to it. A token comes only from
  `lend` (`external_body`, no `ensures`: the caller's `errno` is
  unknown), which each export wrapper that takes one calls once before
  the call (the `mallctl` and `mallctlbymib` exports included), and the only write is `set_errno` (`external_body`,
  `ensures final(errno).val() == e`), which takes it `&mut`. Every Rust function between a wrapper and a
  `set_errno` takes `Tracked<&mut ThreadErrno>`. The allocating libc
  entry points state the final value. Every free states it unchanged:
  `free`, the sized frees, `challoc_free_usable`, `dallocx`, `sdallocx`
  and the C++ deletes (rows 10, 11, 15, 20, 21 and 26-28). The
  `operator new` paths (rows 23-25), `mallocx` and `rallocx` (rows 16
  and 17) only pass the token on and state nothing about it: `enomem` and `einval` ensure `ENOMEM` and `EINVAL`, and
  each allocating entry point ensures `r@.addr == 0 ==>
  final(errno).val() == ENOMEM` (rows 1, 2, 7, 8, 12, 13), with the
  documented exceptions stated: `EINVAL` for a bad alignment (row 5),
  `realloc(p, 0)`'s null with `errno` unchanged (rows 3, 4, 14),
  `posix_memalign`'s untouched `errno` on `EINVAL` (row 6). So every
  null an allocating entry point returns is a failure that set `errno`,
  and every non-null one is a block (`fits`). The exports of rows 1-8
  and 12-14 restate the failure values as the `errno` the C caller
  reads: `axiom_errno_at_return` (`vspec/errno.rs`, trusted, a
  C-program contract) uses up the lent token right after the call and,
  when the export's own `failed` condition holds, ensures `c_errno()`
  (uninterpreted: `errno` as C reads it after this return) is
  the token's value. So `malloc`'s export ensures `r@.addr == 0 ==>
  c_errno() == ENOMEM` (`api/libc.rs`), `realloc`'s the same
  except for a block resized to 0, `aligned_alloc`'s and
  `memalign`'s `EINVAL` and `ENOMEM`,
  `posix_memalign`'s `r == ENOMEM ==> c_errno() == ENOMEM`,
  and `reallocarray`'s overflow `ENOMEM`. The unchanged-`errno`
  claims (`realloc(p, 0)`, `posix_memalign`'s `EINVAL`, `free`) are not
  restated: the axiom gives nothing where `failed` is false, because a
  libc call in trusted code (`pthread_setspecific` at heap creation) may
  have written `errno` after challoc's last write. The fast pops return no
  null: a slot is never at address 0 ("No block at address 0",
  `heap::theorem_no_block_at_zero`, from `Regions::separated`'s
  `0 < class_base[c]` and `0 < huge_base`, which `from_parts` proves or
  requires and `reserve` proves from `try_reserve`'s `0 < a`, the
  kernel's `mmap` contract). `free`'s core states the frame:
  `free_block` and `free_slow` ensure `final(errno).val() ==
  old(errno).val()`, as do `heap::free` and `Heap::free`. `realloc`'s
  `p != 0 && size == 0 ==> final(errno).val() == old(errno).val()` uses
  that frame. A path that reaches `set_errno` only through
  `background::start` keeps the caller's value, because `start` takes
  the caller's token and ensures it unchanged. The premises: `lend` is
  called only by export wrappers, once per call; `get_errno` (trusted)
  ensures it reads the token's value, assuming no libc call in trusted
  code wrote `errno` since; `axiom_errno_at_return` is called only by an
  export, right after its call, on the token it lent, with that
  export's own `failed` condition (Verus does not check `failed`: an
  export that passed `true` would get the claim on every path), and no
  verified code calls an export, so no proof sees two returns
  (`c_errno` names one). The source test
  `api::tests::exports_return_their_callees_result` (`api/mod.rs`)
  checks these by reading the source: every `lend` call is in an export
  body, at most one per body in `api::libc`, `api::cxx` and
  `api::jemalloc`; every `axiom_errno_at_return` call is in one of the
  13 `api::libc` bodies, right after the call, with that export's
  `failed`; and no line outside a comment names an `exports` module
  (verified code that called `lend` could write `errno` without the
  caller's token); a nested call (an allocation from inside libc or a
  C++ `new_handler`) gets its own token from its own wrapper. And the
  token models challoc's own writes, not libc's. Trusted code calls libc
  functions that may write `errno` (`getauxval`, `pthread_key_create`
  and `pthread_atfork` during init, `pthread_setspecific` at heap
  creation, `pthread_attr_*`, `pthread_create` and `pthread_join` in
  `purge::background::start`, `fwrite` in `malloc_info`'s sink). A
  stated failure value holds at return because each `set_errno` is the
  last thing its path does (its failure result, a null, `-1` or the
  error code, is returned right after). A function that states no final
  value leaves `errno` alone only if its path makes no such libc write
  or undoes it. `free`'s path never runs init (`shared_if_ready`).
  glibc's `pthread_setspecific` does not touch `errno` on a path that
  returns (a failed second-level allocation sets `ENOMEM`, and challoc
  then aborts). `background::start` saves `errno` with `get_errno` and
  restores it with `set_errno` around its calls, under the caller's
  token, and ensures `final(errno).val() == old(errno).val()`
  (`purge/background.rs`).
- *(live)*: `free_target` (`heap/mod.rs`, theorem 3) is proved. A
  call with a pointer that is not a slot start or a huge page start does
  not return. A free core takes the caller's token for `p` (`Live::claims`) and ensures
  `p@.addr != 0 ==> blk.block(p)`: what it freed was a live block, whose
  token it gave back to the cache or the bitmap word (slots) or the run
  authorities (runs). A non-live pointer that is a slot or huge page
  start has no token in C's hands; the boundary's `axiom_c_claim` (the
  `free` contract) makes such a call's context inconsistent, and the
  run-time checks (`SlabTable::check_live`, `class/table.rs`, the
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
  return the `Stored` token of `store_usable` (verified: it writes through
  the out-pointer's permission, the call's `vspec::cbuf::COut` that the
  export wrapper mints with `libc.rs`'s private `axiom_c_out`, and the token,
  `s@.ptr() == usable && s@.val() == n`, is made from it), the last store
  through `usable`. They state `usable_is(r, size, s@.val())` for a non-null
  result. For a huge run they read the size back with `usable_size`,
  right after `alloc_huge`, with the run token `alloc_huge` returned,
  whose usable bytes are `huge_usable(size)` (`alloc_huge`'s `ensures`:
  the descriptor `Huge::take` wrote takes `usable_pages(size)` pages
  plus the guard).

Per row (cells marked `P` without a note are the cited `ensures`
alone):

1. `malloc` (`api/libc.rs`). R: `fits(r, size, QUANTUM)`. D: *(D)*.
   F: `r@.addr == 0 ==> final(errno).val() == ENOMEM` (*(errno)*): every
   NULL is a failure that set `ENOMEM`, and no block is NULL ("No block
   at address 0"). A non-NULL `malloc(0)` is a distinct block, at an
   address no other live block has (`Live::theorem_distinct`), as the
   module doc promises. Not stated: NULL only on exhaustion (exhaustion
   has no spec), so `malloc(0)` may be NULL with `ENOMEM` like any
   allocation.
2. `calloc` (`api/libc.rs`, `.0` of `calloc_zeroed`). R:
   `fits(r, n * m, QUANTUM)`. Z: `r.1@.covers(r.0, n * m)` (theorem 6).
   F: `n * m > usize::MAX ==> r@.addr == 0` and `r@.addr == 0 ==>
   final(errno).val() == ENOMEM`, so an overflow is NULL with `ENOMEM`.
3. `realloc` (`api/libc.rs`). R: `fits(r, size, QUANTUM)`, a kept
   block included (`kept` bounds a kept run by
   `size`, and unfolds to `fits`). D: *(D)*
   (`realloc_block`); a kept `p` comes back with the caller's own token.
   The move's disjointness from `p` is proved (`Live::theorem_apart` on
   the two blocks' tokens). The copy takes both blocks' permissions
   (`libc::copy_moved`, `vspec::user::copy_user`), which also makes the
   ranges disjoint. C: the code copies `min(old, size)` bytes, with
   `usable_of(p, old)` and `old == blk.len()`, the block's usable bytes
   (`old_size_or_keep`): the class size for a slot, a run token's
   usable pages for a huge `p`. Contents are modelled:
   `realloc_block` (likewise `realloc_usable_stored` and `rallocx_zeroed`)
   ensures that a moved block's first `min(blk.len(), size)` bytes read
   as `p`'s did when the copy was made (`Live::copied_from`, over the
   byte states). The copy's effect on the byte states is the trusted
   `vspec::user::copy_raw` (`vspec/user.rs`, `external_body`,
   discharged by Kani); its safety is
   verified. F:
   `p != 0 && size == 0 ==> r@.addr == 0` with `errno` unchanged (the
   frame across `free`, *(errno)*), every other NULL with
   `ENOMEM`, and the core (`realloc_block`) gives the caller back its own
   token for `p` on a NULL from the move (`p` is still live; the code
   returns before `heap::free`). B: `p != 0 ==> free_target(p)`, *(live)*.
4. `reallocarray` (`api/libc.rs`). As `realloc` for `n * m`. F also
   proves `n * m > usize::MAX ==> r@.addr == 0 && final(errno).val() ==
   ENOMEM`. B: `free_target(p)` only when `n * m` does not overflow (an
   overflowing call returns NULL without looking at `p`).
5. `aligned_alloc`, `memalign` (`api/libc.rs`). R:
   `is_pow2(align) ==> fits(r, size, align)`. B:
   `!is_pow2(align) ==> r@.addr == 0 && final(errno).val() == EINVAL`.
   challoc's `memalign` rejects a non-power-of-two alignment, unlike
   glibc's, which rounds the alignment up (the module doc names the
   `EINVAL`). F: a NULL for a power-of-two alignment comes with `ENOMEM`.
6. `posix_memalign` (`api/libc.rs`). R: `alloc_aligned`
   ensures `fits(p, size, align)`, and `store_out` (verified,
   through the call's `vspec::cbuf::COut`) stores `p` through `out`; the
   export ensures that on a `0` return C reads at `out` a non-null `q`
   with `fits(q, size, align)` (`c_out`, through the trusted
   `axiom_out_at_return`). F: the return code is `0`, `ENOMEM` or
   `EINVAL`; `EINVAL` leaves `errno` unchanged, and `ENOMEM` comes from
   `alloc_aligned`'s `enomem`, so it also sets `errno = ENOMEM`, which
   the export restates as `c_errno()` (the function's doc says only that
   the `EINVAL` path leaves `errno` alone; the module doc's "leaves
   `errno` alone" is wider than the code). On a nonzero return the export
   ensures C reads at `out` what it left there (`c_out_entry`). B:
   `r == EINVAL <==> !(is_pow2(align) && align % 8 == 0)`.
   D: `posix_memalign_block` ensures, on a `0` return, the token of a
   block of at least `size` bytes aligned to `align`, and that it is the
   one stored through `out` (`*out`'s value through the `COut`), so `P`
   by the `_block`-core rule above.
7. `valloc` (`api/libc.rs`). R: `fits` at the page
   (`1 << lg_page`). F: NULL with `ENOMEM`.
8. `pvalloc` (`api/libc.rs`). R: `fits(r, size, page)`. The
   page-rounded size the code allocates is not stated. F: NULL with
   `ENOMEM`, the `checked_add` overflow included.
9. `malloc_usable_size` (`api/libc.rs`). F: `p == 0 ==> r == 0` and
   `p != 0 ==> 0 < r` (0 exactly for NULL, "No block at address 0"). B:
   `p != 0 ==> free_target(p)`, *(live)*. Before init, a non-NULL `p`
   aborts (`free_before_init`). U: `p != 0 ==> usable_of(p, r)`,
   exact for a slot, the descriptor's shape for a huge run, *(U)*.
10. `free` (`api/libc.rs`). B: `p != 0 ==> free_target(p)`, *(live)*;
    NULL is a no-op (not in the `ensures`; the return
    is `free_slow`'s, after the fast branch's TLS read and bit test, which
    null never passes). F: `errno` preservation (the module doc's
    promise): `free_block`, the core `free` calls with the token its
    wrapper lends, ensures `final(errno).val() == old(errno).val()`,
    proved over `free_slow`, `heap::free` and `background::start`; the
    export states nothing about `errno` (`failed` is false for a free,
    *(errno)*). It rests on *(errno)*'s premises: `lend` is called only
    by the export wrappers (the source test
    `api::tests::exports_return_their_callees_result`), `get_errno` reads
    the token's value, and no libc call on `free`'s path writes `errno`
    unrestored. A thread's first `free` may create
    its heap before the abort (theorem 3).
11. `free_sized`, `free_aligned_sized` (`api/libc.rs`). B:
    `free_target(p)`, *(live)*. The size hint is proved against the
    slot's class: `free_sized` ensures `p != 0 ==> hint_ok(p,
    spec_class_for(size, 1))` and `free_aligned_sized` the same for
    `spec_class_for(size, align)`, or `None` for a non-power-of-two
    alignment, so a slot freed with another class's size, or with a bad
    alignment, aborts ("Bad input"); a huge run freed with a size that
    does not round up to its pages aborts too (`free_sized_block`
    ensures `huge_hint_ok` of C's token). The slow path checks it
    (`Shared::check_size_hint`, which ensures `hint_ok`); the fast
    path (`free_sized_fast`) pushes only a slot of the hinted class, and
    its liveness is checked when the cache flushes. F: `errno`
    preservation, as `free`: `free_sized`, `free_aligned_sized` and the
    cores they call (`free_sized_with`, `free_sized_hint`,
    `free_sized_block`) ensure `final(errno).val() == old(errno).val()`,
    proved over `free_sized_slow`, `heap::free` and `background::start`
    (a fast-path push takes no token); the export states nothing about
    `errno`, and the premises are row 10's.
12. `challoc_malloc_usable` (`api/libc.rs`, `.0` of
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
13. `challoc_calloc_usable` (`api/libc.rs`, `calloc_usable_zeroed`). R, F: as `calloc`. Z: the token covers `n * m` bytes and
    `covers_usable(r.1, r.0, 0, n * m)`, the whole usable size the doc
    promises and the code zeroes (`zero_user(p, u)`, `api/libc.rs`, or the known-zero
    token of `heap::alloc_zeroed` or `alloc_huge`). For a huge run the
    `memset` length `u` is the usable size of the block's own token
    (`usable_size` with the token `alloc_huge` returned,
    `huge_usable(n * m)`), so no trusted premise remains. U: as row 12, for `n * m`, and the null
    result's 0 is stated (0 exactly for NULL, as row 12).
14. `challoc_realloc_usable` (`api/libc.rs`, `.0` of
    `realloc_usable_stored`). R, D, C, F, B: as `realloc` (the keep
    decision is `realloc_old`). U: as row 12. A kept block's
    size is `realloc_old`'s, which ensures `usable_of(p, old)` and, when
    kept, `usable_is(p, size, old)` (for a run, `usable_for(size) ==
    Some(old)` is the keep test); a moved block's is
    `malloc_usable_stored`'s.
15. `challoc_free_usable` (`api/libc.rs`). F: `p == 0 ==> r == 0`
    and `p != 0 ==> 0 < r`, so 0 exactly for NULL ("No block at address
    0").
    B: as `free` (`free_usable_block`). U: `p != 0 ==> usable_of(p, r)`,
    *(U)*, and `r == blk.len()`, the freed block's usable bytes (the
    class size, or the run's usable pages, from its token on both paths):
    the class size from `heap::free_fast_slot` (`heap/paths.rs`),
    which ensures `is_slot(c, p)`, or from `free_usable_slow`'s classification
    (`api/libc.rs`), or `Huge::usable_size` there. The `r == blk.len()` half
    is the core's; at the export `blk` is the token `axiom_c_claim` gives
    for C's `p`, so for a huge run it holds under the `free` contract
    (the `usable_of` half needs no token).
    `errno` is preserved as by `free`: `free_usable` and
    `free_usable_block` ensure `final(errno).val() == old(errno).val()`
    (over `free_usable_slow` and `heap::free`).
16. `mallocx` (`api/jemalloc.rs`, `.0` of `mallocx_zeroed`).
    R: `fits(r, size, 1 << (flags & 0x3f))`. With no `MALLOCX_LG_ALIGN`
    this is alignment 1, weaker than the quantum the code gives. Z: with
    `MALLOCX_ZERO`, `covers(r, size)` and `covers_usable(.., 0, size)`
    (`vspec/zero.rs`, the whole usable size; theorem
    `vspec::zero::theorem_zeroed_memory`). Also, for a non-NULL
    result, `usable_is(r, size, nalloc_usable(size, flags & 0x3f))` (row
    22). F: NULL on failure with `errno` untouched (`alloc_with`'s doc):
    `mallocx` takes the `errno` token but states nothing about it
    (*(errno)*), so the untouched `errno` is not proved; the first call
    runs init, whose libc calls are *(errno)*'s premise. NULL itself is not stated beyond
    `fits` allowing NULL. B: `tcache_checked(size, flags)`: an explicit
    cache id of `MAX_TCACHES` or more aborts, except for a huge request
    with an explicit arena, which never looks the cache up; every
    `MALLOCX_LG_ALIGN` gets an aligned block or NULL (R). An arena that
    does not exist returns NULL and a cache id below `MAX_TCACHES` that
    names no cache aborts, with the same huge exception (`alloc_with`'s
    doc); neither is stated, as existence is a run-time table load.
17. `rallocx` (`api/jemalloc.rs`, `rallocx_zeroed`). R:
    `fits(r, size, 1 << (flags & 0x3f))`, a kept block included, as
    `realloc`. Z: for a new or moved block with `MALLOCX_ZERO`,
    a token from offset `k` to the usable end (`covers_usable`). `k == 0`
    for a NULL `p`; for a non-NULL `p`, `k == min(old, size)` for an
    `old` with `usable_of(p, old)` (`old_size_or_keep` ensures it of the
    `old` it returns), so `min(class size, size)` for a slot `p`. For a
    huge `p`, `usable_of` gives only the shape of `old` (`run_bytes`:
    whole pages, at least one, inside the region), not the descriptor's
    value the code copies; `rallocx_zeroed` also ensures
    `k == min(blk.len(), size)`, the usable bytes of `p`'s token
    (`old_size_or_keep` ensures `old == blk.len()`), which is the
    old run's usable size. So Z is `P` under the `free` contract, as D:
    at the export, `blk` is the token `axiom_c_claim` gives for C's `p`.
    A kept block gets no `Zeroed` token; jemalloc zeroes only past the old real
    size, which a kept block keeps. C, F, B: as `realloc`, except that
    `errno` untouched is not stated (the token is passed on, as row 16);
    NULL `p` allocates.
18. `xallocx` (`api/jemalloc.rs`). Never resizes; returns
    `sallocx(p, 0)`. B: `free_target(p)` and `p@.addr != 0`
    unconditionally, so NULL aborts. U: `usable_of(p, r)` and `0 < r`,
    as row 19.
19. `sallocx` (`api/jemalloc.rs`). B: `free_target(p)` and
    `p@.addr != 0` unconditionally: NULL aborts (`not_an_allocation`), stated ("No block at address 0"); *(live)*.
    U: `usable_of(p, r)` and `0 < r`, *(U)*. For a slot `mallocx(size, flags)`
    returned, `theorem_usable_consistent` gives `r == nallocx(size,
    flags)`, jemalloc's contract.
20. `dallocx` (`api/jemalloc.rs`). B: `p != 0 ==> free_target(p)`,
    *(live)*. With `MALLOCX_TCACHE_NONE` the release tests the bit
    immediately (**unconfirmed**, from the doc comment). An explicit
    cache id of `MAX_TCACHES` or more aborts (`!bad_tcache`, "Bad
    input").
    `dallocx` and `dallocx_block` ensure `errno` unchanged (through
    `heap::mallocx::free`, which ensures it for every cache choice).
21. `sdallocx` (`api/jemalloc.rs`). B: `p != 0 ==> free_target(p)`,
    *(live)*. jemalloc's `nallocx(size, flags) == sallocx(p)` contract
    is checked as in row 11, for slots: `p != 0 ==> hint_ok(p,
    spec_class_for(size, 1 << (flags & 0x3f)))` (`flags == 0` is
    `free_sized`, and `sdallocx_flags`/`sdallocx_routed` hint
    `class_for(size, align)`); for a huge run, the size must round up
    to the run's pages (`sdallocx_block` ensures `p != 0 ==>
    huge_hint_ok(blk, p, size)` of C's token, "Bad input"). An
    explicit cache id of `MAX_TCACHES` or more aborts (`!bad_tcache`).
    `sdallocx` and `sdallocx_block` ensure `errno` unchanged, as row 11:
    `flags == 0` through `free_sized_with`, other flags through
    `sdallocx_flags` (`free_sized_block`, or `sdallocx_routed` and
    `heap::mallocx::free`).
22. `nallocx` (`api/jemalloc.rs`). F, U: `r == 0 || size <= r` and
    `r == nalloc_usable(size, flags & 0x3f)` (`api/jemalloc.rs`), the
    size of `spec_class_for(size, 1 << (flags & 0x3f))`, or
    `huge_usable(size)` when the run has a placement in the empty huge
    region at the alignment (`huge_placeable`, `vspec/zero.rs`: the page
    rounding does not overflow, the alignment is at most the region's
    length, and the run and its guard page fit from some aligned page;
    `Huge::placeable` ensures it is `Huge::spec_placeable`, the same
    predicate over the huge allocator's own region), or 0. `mallocx`
    and `mallocx_zeroed` (through `alloc_with`) ensure
    `r != 0 ==> usable_is(r, size, nalloc_usable(size, flags & 0x3f))`,
    so `nallocx(size, flags)` is the usable size `mallocx(size, flags)`
    gives when `mallocx` succeeds: a run `Huge::alloc_run` places is
    placeable (`find`'s start is one aligned page the run fits from),
    and `Shared::alloc_huge`, `Heap::alloc_huge_in` and
    `mallocx::alloc_huge` carry that up. `nallocx(1,
    MALLOCX_LG_ALIGN(63))` is 0 (no alignment past the huge region can
    be served), as jemalloc's. A non-zero `nallocx` still does not
    promise that `mallocx` succeeds (the region may be full, or the
    kernel refuse), only that the request is not one it always rejects
    (theorem 43).
    The two calls
    agree because `class_for` ensures
    `r == spec_class_for(size, align)` for a power-of-two `align`, the
    smallest class that serves the request (`least_class`, from
    `class_of`'s and `class_index`'s minimality: no smaller class
    holds `size` bytes), and `Huge::usable_for` is `Huge::spec_usable`.
    F: `r == 0` exactly when no class serves the request and no run for
    it is placeable (class sizes and `huge_usable` are positive);
    `mallocx` then returns NULL (`alloc_huge` ensures `huge_placeable`
    of a non-NULL run).
23. `operator new`, `new[]` (`api/cxx.rs`). R:
    `fits(r, size, QUANTUM)`. F: the C++ contract, never NULL: `new`
    ensures `r@.addr != 0` ("No block at address 0"), from `new_block`,
    `new_slow` and `oom`'s `!nothrow ==> r@.addr != 0` and the fast
    pop's non-null slot. `oom` returns a pointer only after a non-null
    test or, with `nothrow`, NULL; otherwise it leaves through
    `throw_bad_alloc` (`-> !`, which throws `std::bad_alloc` or aborts
    when libstdc++ is absent). `oom` has no `decreases` (it waits on the
    program's handler).
24. `new` `nothrow_t` (`api/cxx.rs`). R: as row 23. F: NULL after
    the handler loop, or at once when the handler throws (`oom` calls it
    through `call_handler_caught`, `cxx_handler.cpp`'s catch, which
    reports a throw as `false`); not stated beyond `fits`.
25. `new` `align_val_t` (`api/cxx.rs`). R: `is_pow2(align) ==>
    fits(r, size, align)`. F: the throwing forms as row 23
    (`new_aligned` ensures `!nothrow ==> r@.addr != 0`); `p` because
    the `nothrow_t` forms' NULL is not stated beyond `fits` (row 24). B: a
    non-power-of-two alignment (undefined in C++), 0 included, fails at
    once, without the handler loop; `new_aligned` ensures
    `!is_pow2(align) ==> r@.addr == 0` (NULL for the `nothrow_t` forms,
    `bad_alloc` for the others).
26. Unsized `delete` forms, with or without `align_val_t` or `nothrow_t`
    (`cxx::delete`, `api/cxx.rs`). B: as `free`; the alignment is
    ignored. `errno` unchanged, as `free`.
27. Sized `delete` (`cxx::delete_sized`, `api/cxx.rs`). B: as
    `free_sized`, `hint_ok` included. `errno` unchanged, as `free_sized`.
28. Sized aligned `delete` (`cxx::delete_sized_aligned`,
    `api/cxx.rs`). B: as `free_aligned_sized`, `hint_ok` included.
    `errno` unchanged, as `free_aligned_sized`.
29. `malloc_trim` (`api/libc.rs`). F: `r == 0 || r == 1`. Not stated:
    whether anything was purged.
30. `mallopt` (`api/glibc.rs`). F: `r == 1` (accepted and ignored).
31. `mallinfo2` (`api/glibc.rs`) ensures the three zero fields,
    `uordblks`, `hblkhd` and `hblks` from what `collect_all` read and
    summed, `arena` as the sum's `resident` less its `large_allocated`
    (0 where negative), `fordblks` as `arena` less `uordblks` (0 where
    negative, so `fordblks <= arena`), and, when the arenas' `mapped`
    sum fits, `arena >= uordblks` and `fordblks == arena - uordblks`;
    it leaves out `ordblks` and `keepcost`. `mallinfo` is
    verified with no contract. Both require `may_abort()` and
    `may_syscall()`, which their exports grant. The figures are
    unbounded counters (`fig_add`, `fig_mul`).
32. `malloc_stats`, `malloc_info` (`api/print.rs`):
    verified bodies. F (`malloc_info`; `malloc_stats` returns nothing
    and states nothing): `malloc_info` ensures `r == 0 || r == -1`,
    `r == -1 <==> options != 0 || stream` is null, and `r == -1 ==>
    final(errno).val() == EINVAL` (*(errno)*); a `0` may leave `errno`
    changed by the sink's `fwrite`, as glibc's may. The targets
    (`file_target`, `stderr_target`) are verified
    constructors; the write goes through the verified `emit` to
    the trusted `write_file` (`fwrite`) or the verified
    `sys::write_all`.
33. `malloc_stats_print` (`api/print.rs`): verified body, no
    `ensures`. Its `verified_export_pair!` exports take the
    callback as the opaque, `#[repr(transparent)]` `WriteCallback`
    (the C function pointer's ABI) and call the verified
    `exports::stats_print_entry`, which mints the call's token. B: the
    program's `opts` string is read by `opt_byte`, verified over the C
    contract of a string argument (`vspec::cbuf`; the token `CCall` from
    the private `axiom_c_call` of `print.rs`'s `exports`, and its
    `str_c_byte`), and the callback through trusted code (`call_back`,
    `api/print.rs`; with no callback, `call_hook` through `malloc_write`).
34. `mallctl`, `mallctlnametomib`, `mallctlbymib` (`api/mallctl.rs`): verified bodies, no `ensures` on the return code
    beyond `mallctlnametomib`'s `r != 0 ==> !final(cc).written()`
    (`result`, maps `Err(e)` to `e`); the C exports are verified and state no `ensures`: each mints
    its call's token with `axiom_c_call` from its own parameters, and
    `mallctl`'s and `mallctlbymib`'s lend `errno`, so Verus checks the
    bodies' preconditions on the call's token. Every access to the
    caller's memory claims at most bounds: `name_byte`, `name_bytes`,
    `old_len`, `store`, `load`, `copy_prefix`, `query_ptr`,
    `store_triple`, `mib_cap`, `mib_out` and `mib_in` verify over the C
    contract of their arguments (`vspec::cbuf`). So B (a bad
    `oldlenp` or `newlen`, an unknown name) is
    not stated as an error result; the batch-query length check (bug #1)
    is verified inside the body. B is `p` by theorem 32: once init has
    run, no name, MIB or buffer length that keeps the C contract of the
    pointers (`vspec::cbuf`) makes a read under `opt`,
    `arenas` or `stats`, or a utilization query, abort (not even at an
    integrity check); writes, operations and
    the other reads may. Theorem 21 (MIB round-trip) states that a name and its MIB
    reach the same node with the same indices (on the cores, and `ctl`
    requires it of its node and MIB; not on the exports). The cache flush keys (theorem "cache flush completeness"):
    `thread.tcache.flush`, `arena.<i>.purge` and `arena.<i>.destroy`
    (`flush_caller`) and `tcache.flush` (`Tcaches::flush`) leave every home
    and `others`-node cache of the heap they flush empty (`drained`), each
    cached slot appended to the released log with its token (theorem 10);
    `thread.tcache.enabled` written false leaves them freed (`cleared`),
    and `tcache:false` or a disabled heap gets no cache (`ensure_cache`).
    Those call sites are verified against their callees. Per
    heap reference, as theorem 20; no cell changes (the key's effect is
    not one of the columns).
35. `get_defrag_hint` (`api/jemalloc.rs`). F: `r == 0 || r == 1`;
    in its body, 1 exactly when the design 13.5 rule holds over its reads
    (section "Utilization and defrag hint are exact"; the `ensures`
    exports it existentially). B:
    never aborts, for every pointer and before init (theorem 32): it
    requires no `may_abort()`, which an abort would need, and the
    theorem's witness calls it without (`theorem_read_only_never_aborts`,
    `api/mod.rs`). The body is verified.
36. `challoc_version` (`api/mod.rs`), `challoc_features`:
    verified, no contract.
37. Data symbols `malloc_conf`, `malloc_message`, `je_malloc_message`
    (`api/jemalloc.rs`): no code. `message_hook`, which
    reads the hooks, is trusted; `malloc_write` is verified, over the
    trusted one-line `call_hook`.

### Internal properties under every row

| Property                                 | Code | Where                  |
|------------------------------------------|------|------------------------|
| I1 region separation                     | P    | th. 1, heap/mod.rs |
| I2 user blocks apart from metadata       | P    | th. 4, heap/mod.rs |
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
- I4: theorem 7. For slabs, `Regions::is_slot`
  excludes the guard slabs, so every `fits` (the R column) and every
  `free_target` (the B column) states it per result: `theorem_guard_pages` and
  `theorem_guard_free` (`heap/mod.rs`). For huge runs it is
  stated only of the huge lock's values (`Inner::theorem_run_guards`,
  `huge/mod.rs`): `alloc_run` returns the run's token, but
  no lemma states the guard exclusion per result over it, and
  fallback-mode stale `PROT_NONE` guards are not modelled.
- I5: theorem 19. Every flush discards
  only bytes its batch's ghost `free` admits (`purge::discard`). Huge
  purges admit pages of no live run, proved against the run tokens
  (`RunToken::theorem_purge_apart`, `Live::theorem_purge_huge`). Class
  purges admit slabs the arena's class lock value records as idle
  (`class::decay::idle_byte`), plus the run-time `Empty`, `live() == 0`
  check in `walk_empty`; nothing ties idleness to the slot tokens (the
  descriptor's `live` counter is not modelled), so that row stays `p`.
  Theorem 6's `axiom_discarded_zero` premises lean on that link
  and on the `Purged` state's history.
- I6: theorem 8. The cache fast paths of rows
  1, 10, 11 (`free_sized`), 15, 21 (no flags), 23, 24, 26 and 27, and
  row 2's pop, make no system call, except the write and `SIGABRT` of
  an integrity abort (`theorem_no_syscall_on_fast_paths` requires
  `integrity_aborts()`); a call that returns made none, and
  everything else they do is behind a call that requires
  `may_syscall()`. Other rows may enter the kernel.
- I7: theorem 10 (`Heap::flush_all`): the flush
  `destroy` runs first empties every home and node header and appends
  exactly what they held to the released log, each slot's token handed
  to `release_routed` with it. `destroy` itself is verified (its heap block over
  `heap::axiom_heap_block`), section
  10's "weaker" list applies, and that every heap gets a destructor is
  not proved (with an invalid thread key none does), hence `p`.

### Cells marked `p` or `-`

What each partial or missing cell leaves out, and what it rests on:

- **B, the *(live)* cells**: the free cores require and consume the live
  block's token, but a non-live free (a stale or double free of a slot
  or huge page start) is excluded by the C-boundary assumption
  (`axiom_c_claim`), not proved to abort. The queries (rows 9, 18, 19)
  take no token. Rows 16, 17, 20 and 21 also leave out arena and cache
  existence, a run-time table load: an arena that does not exist returns
  NULL, and a cache id below `MAX_TCACHES` that names no cache aborts,
  neither stated. Row 34's B is theorem 32's covered reads only, and row
  33's `-` is a verified body with no `ensures`.
- **C** (rows 3, 4, 14, 17): `p`. The realloc cores (`realloc_block`,
  `realloc_usable_stored`, `rallocx_zeroed`) ensure that a moved block's
  first `min(blk.len(), size)` bytes read as `p`'s did
  (`final(out).copied_from(blk, ..)`, `Live::copied_from`, over the byte
  states). That rests on `copy_raw`'s trusted effect on the byte states,
  which Kani checks (row 3). Not stated: that a block kept in place keeps
  its bytes, and anything beyond the first `min(blk.len(), size)` bytes.
- **F**: rows 16 and 17 take the `errno` token but state nothing about
  it (their first call runs init's libc calls); rows 24 and 25 do not
  state the `nothrow_t` forms' NULL beyond `fits`; row 34 states no
  `ensures` on the return code beyond `mallctlnametomib`'s. The frees'
  `P` cells (rows 10, 11, 15, 26-28) and the unchanged `errno` of rows
  20 and 21 rest on the premise that no libc call in trusted code on
  their paths leaves `errno` changed (*(errno)*).
- **R** (row 8): `pvalloc`'s page-rounded size is not stated.
- **U** (rows 9, 18, 19): the huge half of a query. `usable_size` with
  the block's run token gives exactly its usable bytes, but the queries
  take no token (giving them one would add an `axiom_c_claim` site for a
  call that frees nothing), so for a huge run they state only the
  descriptor's shape (`run_bytes`). A stored or returned 0 means NULL
  exactly (`theorem_stored_zero_iff_null`; the queries' `p != 0 ==>
  0 < r`).
- **I4, huge runs**: `Inner::theorem_run_guards` is stated of the huge
  lock's values; `Huge::alloc_run`'s result comes with its run token,
  but no lemma states the guard exclusion per result over it.
- **I5, slabs**: class batches admit only slabs idle in the class lock's
  ghost state; the link from idleness to the slot tokens needs the
  descriptor's `live` counter as ghost-tracked state. Theorem 6's purge
  premises (`axiom_discarded_zero`, `axiom_discarded_slots_zero`) rest
  on the same link.
- **I7**: see its bullet above.

Remaining work on these cells is tracked in wolfi-dev/challoc#397.
