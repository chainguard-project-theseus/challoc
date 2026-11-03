# Whole-allocator theorems: statements and premises

Reference for the forty-six whole-allocator theorems as the code in
wolfi-dev/challoc#138 (the tracking PR of the series) states them (all 46
also on main, some only in part:
see each section's "Changed since #138" note): the Verus
statement, the entry points it covers, and every trusted premise it
rests on, with `file:line`. Line numbers are the `fn`/`axiom` line on
main at `45efb38c`.
`verus/TODO.md` ("Theorem candidates") carries the history and the
open work; this file says what the code proves today. Each title and
first paragraph claims only what is proved: where a theorem holds only
at a lock invariant, rests on a source test or on the C-boundary
assumption (`axiom_c_claim`), the title or the first paragraph says so.
The sections are numbered 1 to 46 in order (45, fast-path cost bounds,
is a machine-level statement over the release binary that a
disassembly checker, not Verus, checks: `scripts/perf-budget.sh`, the
`perf-budget` check); "Bad input" (the B column)
and the interface contract are not numbered.

Marked **unconfirmed**: facts read from comments, not checked against
the proof.

**Port note.** This file was written for the version of the proofs in
wolfi-dev/challoc#138 and ported to main at `45efb38c`.
Every `file:line` points into main at `45efb38c`, except the few marked
"in #138". Where a section says its `file:line`s are at its own
commit, that commit is where they were first read, not where they point
now. Names the prose gives as history ("since upstream #78", "in place
of", "formerly") need not exist on main. Where main's statement or
premises differ from #138's, the section states main's and adds a
"Changed since #138" note; where main proves less (a function
outside `verus!`, a dropped mask or `ensures`), the note says what does
and does not hold on main. Theorem 31 is on main since
wolfi-dev/challoc#416 (`25e8e4f6`), and its section states main's
version. Main also proves
`config::linux::theorem_secure_conf` (`config.rs:1686`,
wolfi-dev/challoc#426), which #138 lacks and this file does not number:
in a secure-execution process (`AT_SECURE` set) no call of `env_var`
returns, so `config::load` reads neither `MALLOC_CONF` nor
`CHALLOC_CONF`; that `env_var` is the only reader of the environment is
a source test (`environment_has_one_reader`, `config.rs:3162`), not
Verus (`verus/RESULTS.md`). Main also proves
`heap::thread::theorem_exit_no_rebuild` (`heap/thread.rs:509`), which
#138 lacks: its section, "Thread exit never reuses a destroyed heap",
comes after theorem 46 and is not numbered. Theorem 45's table was
measured on main at `98790244`, not `45efb38c` (its section's "Changed
since #138" note). Verification-only functions are
`cfg(verus_keep_ghost)` on main (`cfg(verus_only)` in #138). As
in #138, a function that takes a lock also takes
`Tracked<&mut LockOrder>` arguments, which the abridged signatures below
leave out (theorem 33).

## Configuration

| Axis | What is verified |
|---|---|
| Cache mode | thread caches (upstream main has no per-CPU mode) |
| Quantum | `quantum16` (default) and `quantum8` |
| Target | x86_64 and aarch64, each with both quanta |
| PAC | on or off: `heap::pac_mode()` is uninterpreted |
| Build | `cfg(not(test))`; test-only items (`Meta::new`, `*_t`) excluded |

Because `pac_mode()` (`heap/mod.rs:683`) is uninterpreted, every proof
holds for both values.

Every combination runs on main: the `verus` and `verus-arm64` checks
(`.chainguard/ci/verus.yaml`) each verify the default `quantum16` build
and then the `quantum8` build. On x86_64 `PUSH_PAC` is never set, but
the proofs do not use that.

## Common premises

Every theorem below rests on these. Later sections list only what they
add.

**One process, one `Shared`.**
- `layout::process_regions` (`layout/regions.rs:162`) and
  `heap::pac_mode` (`heap/mod.rs:683`) are uninterpreted;
  `heap::shared::axiom_process_shared` (`heap/shared.rs:201`) defines
  them as the regions and `PUSH_PAC` bit of
  `shared_at(shared_storage())` (`heap/shared.rs:163`, `:189`).
- The kernel's page, `getauxval(AT_PAGESZ)` (the `getauxval`
  specifications, `sys/auxv.rs`), is `1 << process_regions().lg_page`:
  init reserves the regions with `sys::features::page_size()`
  (`Regions::reserve_owning`), and a process's `AT_PAGESZ` never
  changes. Added with the upstream 7a12536a port, for the page check in
  `purge::discard`; it also makes `page_size`'s abort unreachable.
- `api::shared_ref` (verified) ensures `r.wf()`; `wf`'s
  `regions == process_regions()` conjunct (`heap/shared.rs:240`, in `built`, which `wf` includes) ties
  it to the process. It reads the published value through the
  permission `STATE`'s invariant holds from `READY` on
  (`api::Published`, see "One-shot tokens" below). What was its
  assumption (`init_slow` runs `init` once before the `READY` release
  store, and nothing writes a field `wf` reads afterwards) is now
  proved from narrower premises: `axiom_state` (one invariant, its
  initial state), the `STATE` wrappers (under sequential consistency),
  and `heap::shared::axiom_shared_built`'s join, whose `shared_at`
  carries the "nothing unverified writes it" assumption. `shared` and
  `shared_if_ready` load `READY` and get the permission. Since package
  P7 (READY+HEAPPERM) the fact that it was published travels in
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
  get from a verified caller assert it too, a widening of what they
  claim since P7: `sys::tls::TlsSlot::entry`, `tcaches::heap_at`
  and `purge::background::shared_arg` (`mallctl::shared` was one until
  package RDE: it now takes the `api::ReadyMark` of the `READY` its
  caller saw, `requires api::marks_ready(*mark)`, which `mallctl::ctl`
  requires and passes to every reader, and reads through
  `api::shared_marked`). `shared_unchecked` (the fast paths'
  accessor; since upstream #77 `shared_deref`, at the pointer
  `shared_base` gives) is verified that way since P7, as are
  `heap::thread::fast_flags` and `heap::paths::shared_of`.
  The feature line (since upstream #110 written not by `init` but at
  its first request) has its own once-cell invariant (`api::LinePred`,
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
  `purge::background::shared_arg` (`purge/background.rs:747`, the
  background thread's argument, `external_body`). (`heap::paths::shared_of`,
  a heap's `&Shared`, was another until P7; it reads the heap's `shared`
  field through the frozen permission now. `api::mallctl::shared`, the
  readers' accessor, was another until package RDE; it requires
  `api::marks_ready(*mark)` and reads through `api::shared_marked`
  (`api/mod.rs:848`) now.)
- One `Shared`, at its storage (since P7): a well-formed `&Shared` is
  the one at `Shared::storage()`, the only place `Shared::init` builds
  one. Assumed where a reference becomes a pointer:
  `heap::paths::shared_ptr` (`external_body`), what the cold paths hand
  `existing_heap` and what `heap::init_fields` (verified since P6,
  trusted with this claim before) stores in the heap's `shared` field,
  recorded by `FreshHeap::shared`, which `Heap::wf` states is the
  storage. The unit tests build other `Shared`s; the
  premise is about the shipped library, as `axiom_process_shared`'s.
- `Shared::init` (`heap/shared.rs:2029`) proves
  `r ==> shared_at(this).built()`, using the tracked `Built` token
  (`:170`), the trusted `begin` (`:1550`, `external_body`: the
  permission to the storage, for `Step::Shared`'s token), and verified
  in-place writers, each through its field's permission over the
  trusted `axiom_shared_layout` (`:1281`), `axiom_shared_regions`
  (`:1506`) and `axiom_shared_meta` (`:1524`): `put_head` (`:1672`),
  `put_flags` (`:1715`), `put_table` (`:1757`), `put_line` and
  `lines_ptr` (`:1797`, `:1820`, the class lines', since upstream #87),
  `put_huge` (`:1852`), `arena0_ptr` (`:1837`), `put_registries`
  (`:1873`), `put_cgroup` (`:1916`), `put_policy` (`:1938`) and
  `put_tail` (`:1961`). Arena 0 is built the same way, verified:
  `put_head`, `put_sets`, `put_class` and `put_tail` (`arena.rs:846`,
  `:909`, `:944`, `:971`).
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
  since P7, by the definitional `layout::axiom_class_table` on the
  frozen permission, and the line's invariant; since upstream #110 not the feature
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
  `Shared::begin`, `table::axiom_slot_loc`, `table::axiom_fresh_words`
  (one token per class each), `huge::axiom_run_locs` and
  `axiom_statics` each use up their step's token, so Verus checks that
  each runs at most once per process (a second call does not verify:
  `verus/audit/invariants.md`, "One-shot premises"). That the exchange
  succeeds once is proved given `axiom_state` (one invariant; it holds
  the tokens only in `UNINIT`, which nothing writes); what stays
  assumed is that premise, `axiom_state`'s initial state, the
  wrappers (`state_claim` was discharged by loom in #138; main's
  ledger lists it as trusted, tool loom/GenMC) and that `STATE` has no
  other writer (in #138, `verus/loom/run.sh` checked its two
  writes; main's `tests/loom` has no such source check).

**The `free` contract.** `heap::live::axiom_c_claim`
(`heap/live.rs:913`) is the one trusted step at the C ABI boundary: for
a pointer C passes to a free or a realloc, it gives a `Live` token that
claims it (`Live::claims`, `:256`): the slot's token if the pointer is a
slot start, the token of the run that starts there if it is a huge page
start, nothing otherwise. The allocations hand C the token of every
block they return (`Live::gives`, `:218`), so C, which holds the block,
holds the token. It is called once per C call, by the Rust entry point
the export calls, before anything else: `libc::free`, `free_sized_hint`
(`free_sized`, `free_aligned_sized`, the sized `delete`s; since upstream
#101 `free_sized`, and so `operator delete(void*, size_t)` and its array
form, claim in `free_sized` itself, which hands the token to
`free_sized_with`, as `sdallocx` does; `free_aligned_sized` and the
aligned sized `delete`s still claim in `free_sized_hint`),
`realloc`, `realloc_usable`, `free_usable`, and `jemalloc::rallocx`,
`dallocx`, `sdallocx`; every verified core below them takes the token as
a parameter. Verified code cannot call it otherwise: it uses up the
linear token of one C call (`vspec::cbuf::CCall`, the type of the
C-boundary buffer tokens), of a free or a realloc of the pointer
(`CEntry::Frees`, `CCall::block`), which no verified code builds
(private fields, no constructor) and which only the `extern "C"` bodies
mint, each module's `exports` with its own private `axiom_c_call`
(`api/libc.rs:2971`, `api/jemalloc.rs:1170`, `api/cxx.rs:507`, each
`requires e is Frees`), for the body's own pointer argument; the buffer
calls' mints (`axiom_c_call` in `api/mallctl.rs:3436` and
`api/print.rs:1467`, `requires e is Mallctl || e is NameToMib || e is
ByMib` and `e is StatsPrint`) exclude that entry, and each entry point
above requires its token's block to be its `p`. Once per body, for that
body's `p`, and no other maker of a free's token rest on convention
(`axiom_c_call`'s doc comment), not on Verus and not on a source test.
That no Rust code calls a minting export is checked by a line scan:
`api::tests::exports_return_their_callees_result` (`api/mod.rs:3995`)
fails if a line outside a comment names an `exports` module (a path
through one, or a `use` of one). A double free, or a free of a slot or
huge page start that is not a live block, gives C a token it does not
hold, which makes the proof context inconsistent at that call: it voids
theorem 3 for that free and every theorem for later calls. A pointer
that is neither a slot start nor a huge page start gets no token and
the call does not return (theorem 3, which is not voided for it).

**Every heap a verified path is handed is `Heap::wf`.**
- `Heap::wf` (`heap/mod.rs:3266`): `CachePerms::wf`, `tracks`,
  `zero_ok`, `tokened`, `claimed`, `regions_ok()`, `list_ok`, `home_ok`
  and `homed_ok` (the arena ticket). Since P7 also `ready()` and that
  its `shared` field is the process's `Shared` at its storage.
- Assumed by `tcaches::heap_at` (`heap/tcaches.rs:184`, with since
  package P10 its home headers at the pointer, `Heap::home_at`, and its
  nodes' block permissions and claims, `others::block_ok`;
  `external_body`; on main it also takes the caller's `HeapUse` token,
  which `heap::tcaches::axiom_heap_use`, `heap/tcaches.rs:144`, mints
  once per call that reaches an existing heap, so one token gives one
  heap reference at a time), and, for the calling thread's heap, by the slot
  token `sys::tls::TlsSlot::entry` (an axiom since P7): a token whose
  slot holds a heap holds the heap's linear permission
  (`heap::thread::HeapPerm`, abstract: a `vstd` `PointsTo<Heap>` would
  own the home headers' bytes a second time, next to the heap's own
  `CachePerms`) for the heap the value names, with the heap well formed
  (`heap::thread::slot_ok`). `thread::heap_from_slot` (verified since
  P7) authenticates the value and borrows the heap from the token
  through the trusted `heap::thread::slot_heap_ref`;
  `create_heap_for_thread` puts a new heap's permission in the token
  (`tcaches::heap_new`, over the trusted `HeapPerm::assemble`, which
  states what `init_fields`, `mark_bound` and `mark_rehome` stored).
- Heap creation is verified, the stores to `bound` and `rehome`
  included (`mark_bound`, `mark_rehome`, `heap/fresh.rs:242`, `:278`,
  verified bodies on main; in #138, `external_body` and discharged
  by RefinedRust; `init_fields` verifies since P6): `Heap::create` (`heap/mod.rs:5107`),
  `create_detached` (`:5151`). Teardown, `destroy` (`:5539`), is verified
  since package P10 (its heap block over `heap::axiom_heap_block`).
- No writer escapes the permissions: `Heap::set_arena`
  (`heap/mod.rs:4787`) swaps the home headers through their permissions
  and ensures `wf`; `Heap::free_caches` (`:5808`) writes a bare array and
  is used only by the unit tests, the heap's own arrays going through
  `Heap::free_caches_in` (`:6156`), which keeps the permissions.
  (`others::swap_home` was such a writer, gone before #138.)
- Trusted, but stated over the permissions: the slice primitive under
  `stack_mut` (`vspec::slice::slice_mut_elems`, the slice over exactly
  its entries' `PointsTo`s, which the verified `slice_mut_from_perms`
  carves out of the stack's; `stack_mut` itself is verified over it),
  `Heap::home_ptr_at` (`heap/cache.rs:321`, the home header's address,
  `heap + 128 + 64 * class`, under the verified `cache_ptr`, `:338`, on
  the `malloc` and `free` fast paths, which proves from `wf` that the
  header is owned and caches `class`), and `cached_recently`
  (`heap/cache.rs:740`, `external_body`; discharged by Kani in main's
  ledger). The `others` list
  readers `node_caches` and `node_caches_detached` are verified since
  package P10, over a shared view of the node's headers' permissions
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
- `thread::fast_flags` (verified since P7): the `PUSH_PAC` bit, read
  through the permission frozen at `READY` at the pointer
  `api::shared_ptr`, `api::shared_base` or `heap::paths::shared_ptr`
  gives, is `pac_mode()`.
- The `free` fast path's dispatch byte (`vspec/flags.rs`, since package
  FFLAGS; `Shared::free_flags`, verified over it with no precondition,
  ensures `r & !FREE_ROUTE == self.route_fixed()`): a `RouteByte`, whose `AtomicU8` is private to `vspec/flags.rs`,
  so the wrappers `new`, `load` and `fetch_or` (each `external_body`;
  discharged by loom in #138; trusted in main's ledger, partly checked
  by Kani on one thread, `kani_proofs::atomic::flags_route_byte`, which
  leaves out concurrent interleavings, that every write after `new` is a
  `fetch_or` within `ROUTE`, and that `put_flags`' move keeps `fixed`) are its only
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
  `flags()`'s (`Shared::lemma_route_flags`, `heap/shared.rs:3513`, which
  states both since wolfi-dev/challoc#428, as `lemma_free_flags` did in
  #138); both are what `heap_from_slot` and the push need.
- TLS asm: `sys::tls::get` (`sys/tls.rs:328`), `get_tp` (aarch64,
  `:383`; the x86_64 one, `:364`, is verified and returns 0), `set`
  (`:410`); a read returns the `TlsSlot` token's value
  (and, since P7, the aarch64 `get_tp` the token's thread pointer,
  `TlsSlot::tp`) and `set` makes the written value the token's and
  leaves its ghost fields alone (theorem 15), nothing more.

**Class table.**
- `ClassTable::table` (`layout/mod.rs:1388`, `external_body`): entries
  are `class_table()`, `wf`, sized `spec_class_size`. (Its readers
  include `Shared::init`, before `READY`, and the `Deref` impl, which
  can state no precondition, so it does not read through the frozen
  permission.)
- `layout::regions::class_entry` (verified since P7, `requires
  api::ready()`): the entry, through the class table's permission
  frozen at `READY` (`api::axiom_published`), its element
  (`vspec::ptr::axiom_array_elem`, trusted: the layout of arrays and
  that a frozen permission to one gives one to each element), whose
  value is `class_table()` (`layout::axiom_class_table`, trusted,
  definitional), at the pointer the x86_64 barrier returns unchanged
  (`entry_barrier`, an empty `asm!`; discharged in #138, trusted
  in main's ledger: it assembles to no instruction, so main's Isla runs,
  which cover the x86_64 system calls since wolfi-dev/challoc#415, have
  nothing to trace).
- `vspec::class::axiom_class_table_wf` (`vspec/class.rs:12`), the same
  fact for proofs; `adopt_page_geometry` (verified) writes the table
  through the permission `api::axiom_statics` gives and keeps
  `table_ok`, which `api::Published` freezes at `READY`.

**Class lines (since upstream #87).** The `free` fast paths read each
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
  pointer with that atomic's permission (both trusted in main's ledger,
  partly checked by Kani on one thread, which compares addresses and so
  leaves the pointer's provenance unchecked); that the pointer stays valid
  is `Shared::init`'s contract (the `Shared` never moves);
- that a slab below the copy has its bitmap words and owner word
  committed (what `Unbacked::into_lazy` states of a slab below the
  watermark, and `table::load_owner`'s premise was until package OWNER,
  whose witness now shows the word stored) rests on
  the copy never being ahead of the watermark: each store into it is
  of a value the watermark held, under the commit lock and after the
  watermark's own store. Read off the code, not proved.
- since upstream #101 the sized check reads the line's last eight
  bytes as one `u64`, `ClassLine::guard_word` (`external_body`: a
  type-punned load, and on x86_64 an empty `asm!`), whose contract is
  the word the fields make (`guard_mask | tz << 32 | lg_guard << 40`,
  the padding zero, which `ClassLine::wf` states); the line's 64-bit
  inverse (`inverse_of_odd_part_u64`, verified) and `Shared::region_hi`
  (written by `init`, stated by `Shared::wf`) are proved.

**Kernel `mmap`.**
- `linux::try_reserve` (`layout/regions.rs:2002`): a reservation is
  non-zero and ends below `sys::pac::user_va_limit()`.
- `Reserved::is_disjoint` (`layout/regions.rs:1963`), `into_unbacked`
  (`:1982`): live reservations are apart; the metadata range is minted
  once as `Unbacked`.
- `Unbacked::commit` (`vspec/unbacked.rs:115`, `external_body`): a
  committed range becomes a `PointsToRaw` (the `mprotect` contract); the
  source of every metadata permission (`meta.rs:2530`, `:3250`, `:3503`,
  the last a table carved from a committed reservation; since
  upstream #110 called on the part of a committed range past the
  deferred tables in `Inner::commit_bump_to`, and on a just-carved
  table's deferred part after `Inner::flush` in
  `Inner::reserve_committed`), with `Unbacked::into_lazy` for the
  tables committed by their owners.
- `sys::pac::axiom_user_va_limit` (`sys/pac.rs:61`): the limit is at
  most `2^48`.

**Locks.** `lock::axiom_acquired`: an acquisition takes the permission
of the lock's cell (a vstd `PCell`) out of the free lock's invariant,
and the value satisfies `lock_inv` and `lock_frame`; the other entry is
`lock::axiom_held`, behind `Lock::get_unlocked`. The guard's `deref` and
`deref_mut` are verified, over two trusted restatements of vstd's
`PCell::borrow` and `borrow_mut` (`cell_ref`, `cell_mut`, kept for the
machine code); `drop` (`lock.rs:985`) is `external_body` and checks
nothing. `LockGuard::unlock` (`lock.rs:885`, verified, empty body: the
release is the guard's drop at its end) requires `lock_inv` and
`lock_frame` of the value. Since wolfi-dev/challoc#396 every holder of
the metadata lock outside the tests ends with it, on every path to a
return (`Meta::alloc`, `alloc_raw`, `alloc_zeroed`, `free`, `reserve`,
`reserve_table`, `commit`, `reserve_committed_raw`, `stats`); the
source test `meta::tests::meta_guards_end_in_unlock` (`meta.rs:4594`)
checks that no guard drops without it. No other lock's holders call
it, so for every other lock the invariant and frame are not checked at
the release: each verified holder ends with them through its last
call's postcondition or an `assert`. Each
lock type's `lock_inv`/`lock_frame` axiom says the lock protects that
type's invariant; the theorems that use one name it. Also:
`class::sets::axiom_set_tables_lock_inv` (`class/sets.rs:976`) and
`heap::tcaches::axiom_tcaches_lock_inv` (the explicit-cache table lock
guards `()`, so its invariant is true). `Tcaches::init` writes the
`external_body` constant `heap::tcaches::EMPTY_TABLE` (the empty table;
nothing is claimed about its entries).

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
(76 on main, mostly `vspec/core_int.rs`, `slice.rs` and `sys.rs`; the
76th, `<*const T>::wrapping_add` in `vspec/config.rs`, claims nothing
about the pointer, for `env_var`'s walk of `environ`),
`getauxval`'s in `sys/auxv.rs`, and vstd's
specifications of `core`.

**Memory model.** Sequential consistency. Atomic loads return any
value (vstd), except the `free` path's dispatch byte (`vspec/flags.rs`,
above), whose `load` covers every value the one `RouteByte`, the
published `Shared`'s, holds for a reader that observed `READY`; the
three bitmap word writers
(`vspec/word.rs:57`, `:75`, `:113`) are one atomic step each and ignore
the ordering, as is the one word load that returns its value
(`vspec::word::load`, theorem 7). On main Kani runs each `vspec` atomic
wrapper on a real atomic, one thread (`src/kani/atomic.rs`,
wolfi-dev/challoc#433), so the ledger lists them as partly checked:
the interleavings, where the orderings matter, are not checked. See
`verus/WEAKMEM.md`.

**Abort stand-in.** None remains: no site has Verus see another `-> !`
call than the build's abort. The last, `not_a_run_page` at
`Huge::free_run`'s table lookup (where the build calls
`not_a_run(self.addr(page))`, whose address of an unvalidated page could
not be bounded), went with theorem 44: the run token names the run the
table maps the page to, so the site is unreachable and Verus checks the
build's statement there itself. Every
abort helper verifies in place and ends in `sys::abort_fmt`, which
writes the message and raises `SIGABRT` (`sys/abort.rs`). The panic
handler (`lib.rs`) aborts. An abort satisfies every postcondition.

**Termination not proved** (`exec_allows_no_decreases_clause`):
`lock.rs:113` `lock_slow`, `class/mod.rs:2870` `claim_with`,
`api/mod.rs:1247` `init_slow`, `api::line_ready` (since upstream #110:
the wait for another thread's build of the feature line),
`sys::cgroup::Deferred::discover` (since #110: a waiting discovery's
wait for another thread's),
`api/cxx.rs:256` `oom`, `purge/background.rs:763` `main`,
`sys/mod.rs:841`, `:1046`, `:1129`, `sys/cgroup.rs:880`, and, since
wolfi-dev/challoc#413, `config.rs:1870` `env_var` (its walk ends at the
null pointer that ends `environ`, a fact about the C program). The theorems are
partial correctness: they say what holds if the call returns. None of
these is on the thread-cache fast paths, which terminate and take no
lock (theorem 9).

**Changed since #138.** The C-boundary mint is `axiom_c_call` in
each module's `exports` (`axiom_c_block` in #138), and that each
body mints once and for its own pointer is now convention: the source
test `c_blocks_only_for_the_entrys_pointer` is not on main
(wolfi-dev/challoc#397). A new premise, `axiom_heap_use`
(`heap/tcaches.rs:144`, #382), mints one `HeapUse` token per call that
reaches an existing heap. `mark_bound` and `mark_rehome` are verified in
Verus. `mallctl::shared` requires the `ReadyMark` (`marks_ready`) and
reads through `shared_marked`, where #138 required `ready()` and
read through `shared_deref`; `STATE`'s invariant carries the mark.
`lemma_route_flags` replaces `lemma_free_flags` and, since
wolfi-dev/challoc#428, states the dispatch byte's `PUSH_ZERO` bit as
`lemma_free_flags` did. `aarch64` + `quantum8` is
verified on main (`verus-arm64`'s quantum-8 step); #138 ran three
configurations. Weaker on main: the seams #138 discharged with loom (the `vspec` atomics
wrappers, `state_claim`, `store_owner`, the `SlabDesc` accessors and
others) are trusted in main's ledger, which accepts only Kani,
const-assert and Isla citations; the loom models still run in
`tests/loom`, on copies of the code (wolfi-dev/challoc#397). Since
wolfi-dev/challoc#433 Kani runs the `vspec` atomics wrappers (`word`,
`byte`, `count`, `entry`, `flags` and most of `atom`) on one thread,
so the ledger lists them as partly checked; that is still weaker than
#138's loom checks, which covered interleavings. `load_usize_seen`,
`store_owner`, the `STATE` and `LINE_STATE` wrappers and the `SlabDesc`
accessors have no check on main.

## 1. Region separation

**Statement** (`heap/mod.rs:198`, `:210`):

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
(`heap/mod.rs:189`); `Regions::user_block` (`layout/regions.rs:536`) is
a slot start of a class at least `size` bytes (`is_slot`, `:448`), or a
page start of the huge region with `max(size, 1)` bytes of the region
after it (`is_huge_block`, `:524`). The proof is
`Regions::lemma_user_block_separated` (`layout/regions.rs:1459`).

**Entry points.** Each ensures `fits`; the theorem is a separate lemma
over that postcondition, not called by them. Rust-level functions; the
`extern "C"` wrappers (`api/libc.rs:2955`-`:3350`, `api/jemalloc.rs`
`exports`, `api/cxx.rs` `exports`) are in `verus!` and only cast; on
main they also restate the core's `fits` in their own `ensures`
(in #138 they had no contract).

| Function (`file:line`) | Size, align in `fits` |
|---|---|
| `malloc` (`api/libc.rs:305`) | `size`, `QUANTUM` |
| `calloc`, `calloc_zeroed` (`api/libc.rs:502`, `:525`) | `n * m`, `QUANTUM` |
| `realloc` (`api/libc.rs:1548`) | `size`, `QUANTUM` |
| `reallocarray` (`api/libc.rs:1734`) | `n * m`, `QUANTUM` |
| `aligned_alloc` = `memalign` (`api/libc.rs:2653`) | `size`, `align` (pow2) |
| `valloc`, `pvalloc` (`api/libc.rs:2790`, `:2818`) | `size`, the page |
| `malloc_usable` (`api/libc.rs:1819`) | `size`, `QUANTUM` |
| `calloc_usable` (`api/libc.rs:2003`) | `n * m`, `QUANTUM` |
| `realloc_usable` (`api/libc.rs:2157`) | `size`, `QUANTUM` |
| `mallocx` (`api/jemalloc.rs:399`) | `size`, `1 << (flags & 0x3f)` |
| `rallocx` (`api/jemalloc.rs:448`) | as `mallocx` |
| `cxx::new`, `new_nothrow` (`api/cxx.rs:333`, `:348`) | `size`, `QUANTUM` |
| `cxx::new_aligned` (`api/cxx.rs:387`) | `size`, `align` (pow2) |

A kept block (`r == p`) is included: the realloc family ensures `fits`
for every result (revised by gap 6 below). The keep decision
(`old_size_or_keep`, `realloc_old`) ensures `kept(p, size, align)`
(`api/libc.rs:1196`), a quantum-aligned slot of a large enough class, or a
huge page start with `max(size, 1)` bytes of the region from it, which
unfolds to `fits` at `align` and at the quantum (`kept` is `open` on
main, so it unfolds where it is used; `lemma_kept_fits` stated it at
#138). The huge
half's bound: the keep test is `usable_for(size) == Some(old)`, so
`size <= old`, and `old`, the usable bytes `Huge::usable_size` reads
from the run's descriptor, ensures `spec_run_usable` (the usable pages
and the guard inside the region); `vspec::usable::lemma_run_bytes_block`
turns that into `is_huge_block(p, max(size, 1))`.

**Premises beyond the common ones.**
- Slab results: `CachePerms::tracks` (`heap/mod.rs:1580`), part of the
  assumed `Heap::wf`; the cache entries' `is_slot` comes from verified
  refills and frees.
- With PAC: `sys::pac::axiom_auth_sign` (`sys/pac.rs:71`, used at
  `heap/mod.rs:761`): `spec_auth(spec_sign(p, m), m) == p` below the VA
  limit; `sign`, `auth` (`sys/pac.rs:199`, `:245`) are `external_body`
  and equal `spec_sign`/`spec_auth`, one function per process (no
  `PR_PAC_RESET_KEYS`). Main's ledger lists `sign` and `auth` as
  trusted, partly checked by Isla on the Armv8.5-A and Armv9.4-A models
  (`verus/isla/seams.md`): not checked is that the kernel keeps the
  thread's data key A and EnDA fixed (K3 there), and for `auth` that it
  never resumes past an `autda` that faults (K2); `axiom_auth_sign` and
  `strip` are discharged by those runs, within the firmware and CPU
  premises (P12, P13) that file states. `sign`/`auth` require `pac::pac_available()`
  (uninterpreted; its one fact is the trusted `AT_HWCAP` reading in
  `sys::auxv`, which `pac::available` turns into its contract), and
  `auth` also requires `may_abort_corrupt() || signed(p, modifier)`.
- Huge results: `vspec::huge::axiom_huge_lock_inv`, `_frame`
  (`vspec/huge.rs:29`, `:40`); `Huge::alloc_run`
  (`huge/mod.rs:4587`) is verified. A kept run's bound rests on the
  same axioms: `Huge::usable_size` reads its `spec_run_usable` from a
  live `Active` descriptor under the huge lock invariant (`run_ok`,
  `desc_ok`).
- Class lock and commit lock invariants for the claim path:
  `class::axiom_class_lock_inv`, `axiom_class_lock`
  (`class/mod.rs:2167`, `:2178`), `table::axiom_table_lock_inv`,
  `_frame` (`class/table.rs:268`, `:280`), `next_slab_locked`
  (`class/table.rs:4196`, verified); `bits_unchecked` is verified,
  through the word permissions of `table::axiom_fresh_words`
  (`class/table.rs:2454`; in #138 two axioms,
  `axiom_fresh_words` for the value permissions and `axiom_init_words`
  for the borrow permissions; main's one axiom returns both and
  requires a `ZeroRaw` token for the words).
  **Unconfirmed** which of these the `is_slot` ensures of
  `ClassState::claim` needs; the slot address comes from
  `claim_with`'s verified `slots_in`.

**Where the statement is weaker than the code.**
- `mallocx` with no alignment flag is stated at alignment 1, not the
  quantum, though with no `MALLOCX_LG_ALIGN` the code returns a class or
  run aligned to at least the quantum.
- `posix_memalign` (`api/libc.rs`) states only its return code and
  `errno`;
  `posix_memalign_block` adds that on a `0` return `*out` holds, through
  the call's out-pointer permission (`vspec::cbuf::COut`, which the
  verified `store_out` writes through), the block whose token it hands
  out. Neither states `*out` untouched on failure.
- `fits` says `size` bytes; the code gives the class size or the run's
  usable bytes (theorem 2).

**Changed since #138.** The statement is unchanged. The
`extern "C"` wrappers now ensure `fits` themselves; `lemma_kept_fits` is
gone because `kept` is open; the bitmap words' two axioms
(`axiom_fresh_words`, `axiom_init_words`) are one `axiom_fresh_words`,
which needs a `ZeroRaw` token where neither did, and which Kani partly
checks on main (trusted with no check in #138). The PAC seams
(`axiom_auth_sign`, `sign`, `auth`), Isla-discharged in #138, are on
main: `axiom_auth_sign` discharged by Isla, `sign` and `auth` partly
checked.

## 2. Fit and alignment

**Statement.** No theorem function: `fits` itself (above). Its
`user_block` conjunct puts `size` bytes in one slot of a class at least
`size` bytes, or in the huge region from a page start; its last conjunct
is `p % align == 0`. Lemmas: `lemma_slot_fits` (`heap/mod.rs:582`),
`lemma_slot_fits_quantum` (`:599`), `lemma_huge_fits` (`:630`),
`Regions::lemma_slot_aligned` (`layout/regions.rs:1680`),
`layout::lemma_class_size_quantum`.

**Entry points.** Those of theorem 1, with the alignment column above.

**Premises beyond the common ones.**
- Slot alignment: each class base a multiple of its class's slab, of the
  page and of the 64 KiB colour unit (`base_aligned` in
  `Regions::separated`, proved by `from_parts` from `reserve_owning`'s
  offsets: a random multiple of `MAX_SLAB` plus the class's colour, or
  no colour under `thp:always` chunks); class sizes are
  multiples of the quantum and slabs powers of two (`Class::wf`, through
  `axiom_class_table_wf`).
- Huge alignment: `Huge::alloc_run`'s placement, under the huge lock
  axioms; small alignments fall back to the page, at least 4 KiB
  (`Regions::separated`).
- `api::page_size` (`api/libc.rs:2765`) is verified from `Shared::wf`.

**Where the statement is weaker than the code.**
- The huge fit is `size` bytes of the region, not the descriptor's
  `usable` bytes (`Huge::spec_usable`, `huge/mod.rs:4147`), which the
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

**Statement** (`heap/mod.rs:481`, `:500`):

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
| `free` (`api/libc.rs:597`) | `p` non-null |
| `free_sized`, `free_aligned_sized` (`api/libc.rs:927`, `:985`) | non-null |
| `realloc`, `realloc_usable` (`api/libc.rs:1548`, `:2157`) | non-null |
| `reallocarray` (`api/libc.rs:1734`) | non-null, `n * m` no overflow |
| `free_usable` (`api/libc.rs:2366`) | non-null |
| `malloc_usable_size` (`api/libc.rs:1140`) | non-null |
| `dallocx`, `sdallocx` (`api/jemalloc.rs:624`, `:673`) | non-null |
| `rallocx` (`api/jemalloc.rs:448`) | non-null |
| `xallocx`, `sallocx` (`api/jemalloc.rs:585`, `:600`) | always |
| `cxx::delete`, `delete_sized` (`api/cxx.rs:443`, `:458`) | non-null |
| `cxx::delete_sized_aligned` (`api/cxx.rs:476`) | non-null |

Below them, `heap::free` and `free_direct` (`heap/paths.rs:1151`, `:1172`),
`Heap::free` (`:239`), and the fast paths `free_fast_slot` and
`free_fast_tail` (`:832`, `:889`, on their `Some` result) and
`free_sized_fast` (`:991`, on its `true` result) ensure it (upstream #78
split the unsized path into `free_fast_slot` and `free_fast_push`; #87
replaced `free_fast_push` with `free_fast_tail`). `free_fast` and
`free_fast_class` (`:1547`, `:1558`) are `#[cfg(test)]` helpers outside
`verus!` that ensure nothing; the cache pushes and
`Shared::free_direct` (`heap/shared.rs:2934`) require `is_slot` of their
class, `Shared::free_huge` (`:3181`) a page index from
`Shared::classify_free` (`:2976`).

**Premises beyond the common ones.**
- Every abort ends in `sys::abort_fmt`, which never returns: the abort
  helpers (`heap::paths::invalid_free`, `heap/paths.rs:1435`, and the
  class and huge abort helpers) verify and call it.
- `Shared::classify_free` and `Regions::classify` are verified; the
  sized fast path's `slot_in_class` and `Regions::slab_slot` (and, since
  upstream #78, the unsized fast path's `Regions::slab_slot_unbounded`)
  read the class table through `class_entry` (common premise). Since
  upstream #87 both fast paths read the geometry from the class's line
  (`ClassLine::set_slab`, `Shared::sized_slot`; "Class lines", common
  premises); `slot_in_class` is test-only and `slab_slot_unbounded`
  gone.

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
  at the load (`SlabTable::is_set_unchecked`, `class/table.rs:3116`)
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

A spatial statement (formerly named "metadata integrity"): no byte of a
non-null block an allocation returns lies in the metadata listed below,
the `Shared` static included. It does
not prove that every allocator write stays inside the object it is meant
for, nor that a returned byte is mapped with the right permissions.

**Statement** (`heap/mod.rs:335`, read at the base of the theorem
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

(Line breaks added.) `disjoint` is `layout/regions.rs:129`. Each
conjunct is "lies in the metadata region" (`Regions::in_meta`,
`layout/regions.rs:1596`) plus `lemma_user_block_apart` (`:1605`), except
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
- `vspec::ptr::axiom_ptr_add_addr` (`vspec/ptr.rs:222`): `p.add(i)` is
  `i` elements on, inside its allocation; used for stack entries
  (`heap/mod.rs:3677`), `others` nodes, huge table slots and bitmap
  words.
- `vspec::ptr::axiom_raw_readable` (`vspec/ptr.rs:201`): a `PointsToRaw`
  makes its bytes valid for reads and, when there are some, in one
  allocation (`in_alloc`). Since wolfi-dev/challoc#400 it is the only
  such axiom: `axiom_raw_valid`, which also gave `valid_writes`, is
  gone, because every caller needed only `in_alloc`.
- `vspec::meta::axiom_meta_lock_inv`, `axiom_meta_lock_frame`
  (`vspec/meta.rs:30`, `:42`): the metadata lock protects `Inner::wf`;
  bounds and ghost identities fixed. Since wolfi-dev/challoc#396 both
  are checked at the release: every holder outside the tests ends with
  `LockGuard::unlock` (`lock.rs:885`), which requires them, and
  `meta::tests::meta_guards_end_in_unlock` (`meta.rs:4594`) checks the
  source for a guard that drops without it (common premises, "Locks").
  `Meta::commit` is verified (package
  META): its caller gives the claim to the range that `reserve` or
  `reserve_table` handed out, which each lazily committed table keeps
  for its pages past its watermark (`class::sets::Lazy::claimed`, over
  the process's `Meta`, `meta::process_pages_id`, known during `init`
  through `heap::shared::axiom_shared_meta`) and the background stack
  takes after its reservation; a commit takes the range's part.
  `Meta::free` is verified:
  its caller gives back the `MetaClaim` the allocation handed out, which
  the cache permissions (`CachePerms::claimed`) and the huge allocator
  (`huge::Inner`'s `dclaims`, the descriptor table's `claim`) carry;
  `Heap::destroy` (verified since package P10) gives back the claims
  of what it frees from the heap's ghost state (the stacks' from
  `CachePerms::blocks`, the nodes' from `Heap::heads`) and the heap
  block's from its home headers' permissions (`heap::axiom_heap_block`).
- `Shared::put_head` and `put_table` store what they are given
  (common premise); `SlabTable::new` (`class/table.rs:1562`) proves
  `meta_ok` from the reservations `Meta::reserve_table` (`meta.rs:892`)
  and `Lazy::reserve` (`class/sets.rs:134`) hand out (`Lazy::reserve_raw`
  in #138).
- The class table accessors `desc_unchecked`, `bits_unchecked`,
  `is_set_unchecked`, `owner_word` and `owner_at` read through
  permissions carved from the three reservations
  (`SlabTable::perms_ok`): `Meta::reserve_table` hands out the table's
  bytes once, an eager table's committed part from the `Carve` `Inner::reserve_eager`
  hands back (`meta::Inner::handed` in #138)
  and, since upstream #110 (a carve makes no system call; the object
  side's commit is deferred to the next object or table commit,
  `Inner::flush`), its part past `committed` through
  `Unbacked::into_lazy` of the token `reserve_eager` carved into the
  same `Carve` (`meta::Inner::handed_lazy` in #138), and a reservation's through
  `Unbacked::into_lazy` (`vspec/unbacked.rs:148`, which uses up the token),
  `table::axiom_zero_desc`, `axiom_zero_owner`, `axiom_fresh_words`
  (`class/table.rs:2391`, `:2407`, `:2454`: zero bytes, from the
  `ZeroRaw` `reserve_table` returns, are an initialised descriptor,
  owner word or bitmap word; `axiom_init_descs`, `_owners`, `_words` at
  #138, which took uninitialised permissions; Kani partly checks the
  three on main, leaving out the provenance clause of `perm_at`, which
  Kani cannot observe, and the ghost identities), the slice primitive
  under the bitmap view (`vspec::slice::slice_from_perms`, a slice over
  consecutive word permissions, `class/table.rs:4367`) and the watermark's load
  (`vspec::atom::load_usize`, inside the watermark's invariant, which
  bounds it by the slab count). Premise: only committed pages are read (the
  accessors' `# Safety`).
- `TwoLevelBitmap::from_raw`, `from_raw_chunked` (`bitmap.rs`) are
  verified: they build each word's permission, holding 0, from the
  words' `ZeroRaw` each takes (a table's bytes, or a `SetTable` slot's,
  which `SetTable::new` carves once per slot from its groups'
  reservations) through `vspec::zero::axiom_zero_word` (eight bytes that
  read as zero are the `u64` 0; discharged by Kani on main,
  `kani_proofs::zero::zero_word_reads_zero`, trusted in #138). The zero facts are `Meta::reserve_table`'s:
  an eager table's committed part from the `Carve`'s zero token (fresh
  memory above the bump pointer, `Unbacked::commit`; `meta::Inner::handed_zero`
  in #138) and its deferred
  part, like a reservation's, from `Unbacked::into_lazy` (the same `mmap`
  fact, on its commit-before-read premise: the owner's first
  `Meta::commit` runs the deferred commit). Premise (`ZeroRaw::axiom_new`): a permission is paired only
  with the zero token made with it, whose bytes nothing has written since
  (four pairings on main, `meta.rs:941`, `:3255`, `:3504` and
  `huge/table.rs:552`, `Table::storage`'s binned branch, which pairs the
  permission and token `Meta::alloc_zeroed` returns for one object;
  `meta::zero_raw` was the one pairing in #138).
- `CachePerms::lemma_fresh_stack` (`heap/mod.rs:1460`, proved; the
  trusted `heap::axiom_fresh_stack` in #138): a stack
  `Meta::alloc_raw` just returned is fresh (it uses up the block's permission);
  `Heap::lemma_caches_in_meta` (`heap/mod.rs`) needs `cell_in_meta`,
  which `ensure_cache` (`heap/cache.rs`) proves for each stack.
- Huge lock axioms (`vspec/huge.rs:29`, `:40`) for `huge::Inner`.
- The `Shared` static: `Reserved::is_apart_from_static`
  (`layout/regions.rs`, trusted, the kernel's `mmap` contract): each of
  the three reservations is apart from the loaded object's data, which
  `from_parts` turns into `Regions::outside`.
- `huge::table::Table::storage` (`huge/table.rs:520`, verified, as in
  #138): zeroed storage for the descriptor table, every entry empty,
  each with its permission, in the metadata region. It cuts the block's
  zero permission (`Meta::reserve_committed_raw`'s `ZeroRaw` for a
  reserved table; for a binned one, `Meta::alloc_zeroed`'s permission and
  zero token paired by `ZeroRaw::axiom_new`, above) into the entries'
  permissions (`entry_slots`, `huge/table.rs:399`), each empty by
  `huge::table::axiom_zero_entry` (`huge/table.rs:378`: 16 zero bytes at
  a non-null, 8-aligned pointer are an initialised entry with a null
  descriptor), discharged by Kani
  (`huge::table::kani_table::zero_entry_is_empty`). It requires a page
  of at least 16 bytes, which `Huge::new` requires. In #138 it also ensured
  `entries_in_meta`; on main `Table::new` and `ensure` prove it.
- User-block writes: every write verified code makes to a block's bytes
  (`calloc`'s and the zeroing paths' fills, `zero_slot`, `zero_huge`,
  `realloc`'s copy) takes the block's permission (`vspec::user::UserMem`,
  held by its `Live` token), so it stays inside the block's usable bytes,
  given the permission model's trusted mint: the permission is minted at
  hand-out by `heap::live::axiom_hand_out`, at most once per block while
  held (the `Live` API keeps it with the slot or run token), and at the
  C boundary by `axiom_c_claim`.
  That no other write reaches user memory is discipline, not proof: the raw
  primitives `zero_raw` (`write_zero` in #138) and
  `copy_nonoverlapping` stay callable on
  any `valid_writes` fact, which has no time (`vspec::ptr::rw_len`), so
  this holds because only `vspec::user` calls them on user memory
  (`verus/TODO.md` in #138, "User memory permissions", item 4).

**Not covered.**
- Thread-local storage: not apart from every block, so not in the
  statement. A thread whose stack the program allocated
  (`pthread_attr_setstack` over a `malloc`ed buffer) has its static TLS
  block, and so challoc's heap pointer slot, inside that buffer. (The
  `Shared` static and `Meta`'s link words are conjuncts now.)
- Writes through `&` atomics (descriptors, owner words, `AtomicBitmap`
  words) are located, not tied to permissions. Bitmap words now have
  `WordPerm`s inside `bitmap::WordPred` (`bitmap.rs:3535`), but the
  theorem does not use them.
- Arenas' slab-set tables (`SetTable::new`, `Set::in_table`,
  `class/sets.rs`): no placement stated.
- A class bitmap whose bit count is not a multiple of 64: the last word
  is excluded (`spec_nbits() % 64 == 0`).
- Spatial only: the theorem places metadata and blocks; it does not
  prove that each allocator write stays inside the object it is meant
  for (the writes through permissions do, the atomics above are only
  located), nor that the block's bytes are mapped writable.

**Changed since #138.** The statement is unchanged. `huge::table::Table::storage`
is verified over `axiom_zero_entry`, as in #138, where the axiom took the
uninitialised permission and a `Zeroed` token; on main it takes a
`ZeroRaw` and, as in #138, Kani discharges it. The binned branch adds a
`ZeroRaw::axiom_new` pairing (premises above). The class table axioms now take a `ZeroRaw`
(`axiom_zero_desc`, `axiom_zero_owner`, `axiom_fresh_words`, in place of
`axiom_init_descs`, `_owners`, `_words`), which narrows them, Kani
partly checks them (wolfi-dev/challoc#401), and
`axiom_fresh_stack` became the proved `lemma_fresh_stack`. Stronger on
main: `axiom_zero_word` is discharged by Kani (trusted in #138), and
`axiom_raw_valid`, which gave write validity, is gone; its callers use
the read-only `axiom_raw_readable`. Stronger in
one place: the metadata lock's invariant and frame are checked at each
holder's `LockGuard::unlock` (wolfi-dev/challoc#396); in #138 nothing
checked them at the release.

## 5. Uniqueness

**Statement** (`heap/mod.rs:1922`, `:1967`; private to `heap`, in
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

- Authority per class: `table::slot_loc` (`class/table.rs:645`),
  minted by `SlabTable::new`.
- Free slots: tokens in each word's `AtomicInvariant`,
  `bitmap::WordPred` (`bitmap.rs:3535`): the word's `WordPerm` and the
  tokens of exactly its clear bits.
- Claimed: `AtomicBitmap::claim_word`, `claim_from` (`bitmap.rs:999`,
  `:1182`) move out the tokens of the bits their `fetch_or` won;
  `ClassState::claim_with` (`class/mod.rs:2870`) returns them by index.
- Cached: `CachePerms::tokened` (`heap/mod.rs:1603`), a conjunct of
  `Heap::wf` and `Heap::keeps`.
- Released: `ClassState::release_owned` (`class/mod.rs:5047`) takes a
  `GhostSubset` of the batch; `AtomicBitmap::release`
  (`bitmap.rs:1241`) hands each word's back, so its double-free abort is
  unreachable under the contract.
- Live: the program's. A `heap::live::Live` token (`heap/live.rs:45`)
  holds the slot's token while the program holds the block: the
  allocation paths move the popped, refilled, compacted or directly
  claimed slot's token into it (`heap::hand_out`, `heap/mod.rs:710`;
  `Heap::pop_compact_run`; `Shared::alloc_direct`), and the free paths
  take it back out (`Live::take_slot`) for the cache push
  or the release. Huge runs have the same shape: a `huge::RunToken`
  (`huge/mod.rs:962`) holds the run's extent, a singleton of the
  extents' authority, and its pages, a subset of the pages' authority;
  `Huge::take` inserts both into the authorities `Inner::wf` holds
  (`huge/mod.rs:2978`: their sets are the `Active` descriptors'
  extents and pages, `active_runs`, `active_pages`, `:1201`, `:1209`), and
  `Huge::free_run` deletes them, after `agree` shows the token's extent
  is the descriptor's (`lemma_token_extent`, `:1771`).

**Live blocks (the D column).** `Live::theorem_apart`
(`heap/live.rs:969`): the usable bytes of two live tokens, wherever
they are held, share no byte. Two slots of one class are two tokens of
one authority, so different slots, a class size or more apart; slots of
two classes lie in two class regions; a slot and a run lie in the
superblock and the huge region; two runs hold disjoint page subsets.
`Heap::theorem_uniqueness_live` (`heap/mod.rs:4033`): a slot a
well-formed heap caches is no live block of its class.

**Entry points.** The allocating entry points' cores return the token
of the block they hand out, with at least the request's usable bytes
(`Live::gives`, `heap/live.rs:218`): `malloc_block`, `calloc_zeroed`,
`alloc_aligned`, `aligned_alloc_block`, `posix_memalign_block`,
`valloc_block`, `pvalloc_block`, `malloc_usable_stored`,
`calloc_usable_zeroed`, `realloc_block`, `realloc_usable_stored`
(`api/libc.rs`), `mallocx_zeroed`, `rallocx_zeroed`
(`api/jemalloc.rs`), `new_block`, `new_aligned_block` (`api/cxx.rs`).
The exported functions call them and hand the token to C with the
block; the free cores take the token C holds (common premises, "The
`free` contract").

**Premises beyond the common ones.**
- `heap::live::axiom_c_claim` (`heap/live.rs:913`): the `free`
  contract at the C ABI boundary, one call per C call. See "The `free`
  contract" under Common premises.
- `table::axiom_slot_loc` (`class/table.rs`, one call in
  `SlabTable::new`): the table `SlabTable::new` builds for a class is
  the class's (its slot and marker authorities and guard geometry);
  once per class per process is checked (it uses up the class's
  `Once` token, common premises).
- `huge::axiom_run_locs` (`huge/mod.rs`, one call in `Huge::new`): its
  run authorities are the process's; once per process is checked (it
  uses up the `Huge` step's token).
- `table::axiom_fresh_words` (`class/table.rs`, one call in
  `SlabTable::new`): a just-reserved table's words read zero, one
  permission each; once per class is checked (its token), that `base`
  is the class bitmap's first word is not. Kani partly checks it on main
  (`kani_proofs::zero::fresh_words_load_zero`: the words load zero); the
  provenance clause, the one-shot token and the ghost location
  identities are not checked.
- `vspec::word::word_addr` (`vspec/word.rs:36`) is a word's identity;
  `fetch_or`, `store`, `fetch_and` (`:57`, `:75`, `:113`) are single
  atomic steps whose result is the permission's value (ordering
  ignored; GenMC-discharged in #138, trusted in main's ledger, partly
  checked by Kani on one thread).
- `SlabTable::bits_unchecked` is verified: the view's words are the
  class bitmap's through the permissions of `table::axiom_fresh_words`
  (`class/table.rs:2454`), whose `word_addr` ties them to the word
  invariants.
- Class and commit lock axioms (`class/mod.rs:2167`, `:2178`,
  `class/table.rs:268`, `:280`; since package OWNER the commit lock's
  value also keeps the owner words' persistent witness markers of the
  slabs below `next_slab`, and the class lock's frame the location of
  the owned-slab record, `Inner::oauth`; the class lock bridge
  `owner_locked` verifies since then), the descriptor permissions
  (`slab::DescPerm`:
  `table::axiom_zero_desc`, `class/table.rs:2391`, which mints them from
  a `ZeroRaw`, partly checked by Kani on main; `axiom_init_descs` in #138; `slab::desc_stride` is
  discharged by Kani, and the `SlabDesc` accessors in `slab.rs`, which
  #138 discharged by loom, are trusted in main's ledger; the accessors given the lock edge
  `lock::axiom_acquired` assumes) and `next_slab_locked`
  (`class/table.rs:4196`, verified).
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

**Changed since #138.** Stronger: `theorem_uniqueness_cached` also
ensures `blocks` and `released` unchanged. The class table axioms behind
the premises take a `ZeroRaw` and are partly checked by Kani
(theorem 4). Weaker: the bitmap word wrappers, GenMC-discharged in
#138, are checked on main only on one thread (common premises).

## 6. Zeroed memory (zero-token coverage at return)

What is proved is zero-token coverage at return: the zeroing entry
points return a ghost `Zeroed` token that covers the bytes the API
promises zero. `vspec::zero::theorem_zeroed_memory` (`vspec/zero.rs:380`)
concludes `z.has(a)` for every byte `a` from `from` to the usable end,
and `Zeroed::has` is interval membership. That a load of such a byte
reads zero is the token's meaning, not a proved fact: it rests on the
trusted token sources (`zero_raw` and the kernel axioms below) and on
the unproved discipline that nothing writes a token's bytes between its
creation and the return (premises below). The block's permission now
carries a byte state that also says the bytes are zero ("Byte state"
below). Plain `calloc(n, m)` promises,
and its token covers, only its `n * m` requested bytes; bytes past them
up to the usable size are unspecified (#54). `challoc_calloc_usable`,
`mallocx(.., MALLOCX_ZERO)` and `rallocx`'s new tail cover the whole
usable size.

**Statement.** The `Zeroed` token (`vspec/zero.rs`): `covers(p, n)` is
`addr() == p@.addr && n <= len()`; `covers_usable(z, p, from, size)`
(`vspec/zero.rs:270`) is a token from `p + from` to the block's usable
end (the class size, or `huge_usable(size)`). Postconditions:

| Function (`file:line`) | Token on a non-null result |
|---|---|
| `calloc_zeroed` (`api/libc.rs:525`) | `covers(r.0, n * m)` only |
| `calloc_usable_zeroed` (`api/libc.rs:2028`) | `n * m`, and `covers_usable(.., 0, n * m)` |
| `mallocx_zeroed` (`api/jemalloc.rs:417`) | with `MALLOCX_ZERO`: `size`, and `covers_usable(.., 0, size)` |
| `rallocx_zeroed` (`api/jemalloc.rs:477`) | `size - k` bytes from `r + k`, and `covers_usable(.., k, size)` |

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

**Byte state (loads read zero).** The block's `Live` token now holds the
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
- `vspec::zero::zero_raw` (`vspec/zero.rs:448`, `external_body`,
  Kani-checked by `zero_raw_fills`; `write_zero` in #138):
  `write_bytes(p, 0, n)` makes `n` zero bytes.
- Never-written anonymous memory reads zero: for class slabs,
  `class::axiom_fresh_slots_zero` (`class/mod.rs:3491`, called in
  `ClassState::take`, `:4171`, for a `Fresh` slab, with the slab's
  descriptor state ticket); for huge runs, `huge::axiom_fresh_anon_zero`
  (`huge/mod.rs:2087`, private to `huge`, called in `zero_run`, `:2055`,
  `:2065`, pages at or past the mark); for metadata, the `Zeroed` that
  `Unbacked::commit` and `into_lazy` return (`vspec/unbacked.rs:115`,
  `:148`, `external_body`), under `alloc_zeroed` (`meta.rs:715`) and
  `reserve_committed_raw` (`meta.rs:1018`).
- `MADV_DONTNEED` memory reads zero: `class::axiom_discarded_slots_zero`
  (`class/mod.rs:3530`, called in `take`, `:4173`, for a `Purged` slab,
  with the state ticket and the `PurgedZero` witness of a
  `purge::purged_zero` read, `purge.rs:266`) and
  `huge::axiom_discarded_zero` (`huge/mod.rs:2111`, called in
  `zero_run`, `:2059`, `:2064`).
- In #138 the huge pair lived in `vspec/sys.rs`, shared
  (`pub(crate)`), and the class pair took the class lock's value where
  main's takes the descriptor's state ticket.

**Premises beyond the common ones.**
- Each kernel axiom's protocol premise, the caller's: a `Fresh` slab
  was never handed out; pages past the huge mark were never covered by a
  run; a `Purged` slab or extent was discarded in the lock hold that
  set the state (`class/decay.rs:1428` sets it whatever the advice) and
  has no allocated slot or run since; and `take` makes a zero claim of a
  `Purged` one only while `purge::purged_is_zero` (`purge.rs:246`, a
  `Relaxed` load of `LAZY_EVER`) reads true under the lock
  (`class/mod.rs:4141`, through `purge::purged_zero`, whose witness the
  axiom requires), that is, no batch has used `MADV_FREE`. The
  axioms take any range; the call sites check the recorded state, not
  the history.
- Cache path: `CachePerms::zero_ok` (`heap/zero.rs:57`), part of the
  assumed `Heap::wf`; `Heap::take_handed` (`heap/zero.rs:516`).
- Class and huge lock axioms, for the state `take` reads.
- The fills and the copy are valid because they take the block's
  permission (`vspec::user`, from `heap::live::axiom_hand_out` or
  `axiom_c_claim`); `axiom_fresh_block` and `axiom_moved_block` are gone.
- The only writes verified code makes to a token's bytes are zero fills
  and `rallocx_zeroed`'s copy into the new block's prefix, which it
  splits off (`api/jemalloc.rs:477`). The token is not a permission; nothing ties
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

**Changed since #138.** The statement is unchanged. The token
sources moved: the huge zero axioms are private to `huge`, the class
ones take the slab descriptor's state ticket (and, for a `Purged` slab,
a `purge::purged_zero` witness) in place of the class lock's value, and
`write_zero` is `zero_raw` (Kani harness `zero_raw_fills`, `write_zero`
in #138), which takes the bytes' permission.

## 7. Guard pages

For slabs, per result: every `fits` and every `free_target` excludes the
guard slabs. For huge runs, only at the huge lock invariant: every value
the huge lock admits keeps live runs' usable pages off every run's
guard page, but nothing is exported per call ("Huge runs" below).

Read at this section's commit (the `file:line`s are the `fn`/`axiom`
line there).

**Statement** (`heap/mod.rs:236`, `:261`; `layout/regions.rs:183`,
`:448`):

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
(`class/table.rs:2717`) names: every `guard_interval`-th page group,
counted from 1. `commit_slab` (`class/table.rs:4615`; since upstream #114
`commit_run` too, below) puts the guard on
it: `MADV_GUARD_INSTALL` on the page group's first page, or the whole
slab or page group left `PROT_NONE` when the advice fails, or (under
`thp:always` with PMD chunks) no marker, the slab left unused. Either
way the guard page is inside the guard slabs, so the theorem covers it.
With guards off the mask is all ones and no slab is a guard slab.

Since upstream #114 an arena commits its group a run at a time
(`SlabTable::commit_run`, without the commit lock, which it takes only
to count the guard): a guard slab always starts a run
(`SlabTable::starts_run` ensures `!r ==> !spec_is_guard(s)`), and
`commit_run` guards it with `install_guard` after the run's one
`mprotect`, as `commit_slab` did; page groups above a slab and
`thp:always` chunks keep `commit_slab`. The statement is unchanged:
it is about where blocks lie, and `next_fresh_slab` still hands out
only a slab that is not a guard slab. In #138 two facts about the
runs were proved of the run arithmetic (`class/table.rs`:
`spec_run_start` and `spec_run_end`, which `starts_run` and `run_end`
computed, by their `ensures`), where upstream tests it
(`runs_tile_every_group`, `class/tests.rs:2199`): `lemma_run_next`, a
run holds no guard slab past its first slab and ends where the next run
starts or at the group's end; and `lemma_run_covers`, every slab of a
group lies in the run of a run-starting slab at or below it, which
`next_fresh_slab` asserted for each slab it hands out without a commit.
Main has none of the four: `starts_run` (`class/table.rs:4455`) ensures
only `!r ==> !spec_is_guard(s)`, `run_end` (`:4569`) only
`s < r <= limit` (with `lemma_run_slabs`, `:2139`), and
`next_fresh_slab` (`class/fresh.rs:127`) ensures its slab is not a
guard slab; the two run facts are not proved on main. That `commit_run`'s one
`mprotect` spans exactly `s..run_end(s, limit)` is read from its body
(no contract states it, as `commit` has no ghost model); with it, no
guard slab is made read-write before the cursor reaches it. That a
slab handed out without a commit is read-write also rests on the
earlier run's `mprotect` having succeeded (the cursor passes a run
start only then) and on the `thp` mode, which `SlabTable::runs` reads,
being fixed after init: premises, as the kernel's protections are not
modelled. The slabs a run
commits ahead of the cursor are read-write before they are handed out
(`docs/hardening.md`); they are not guard slabs, so nothing here
covered them.

`guard_of` (`layout/regions.rs:170`) is `is_guard_slab`'s formula;
`guard_geom(c)` (`:178`) is the process's class `c` table's
`(lg_slabs_per_page, guard_mask)`. `is_slot_addr` (`:431`) is the old
`is_slot` (slot `slot < nslots` of slab `slab < nslabs`, guard slabs
included); `slab_index` and `guard_of` are opaque: `slab_index` is
revealed only in `Regions::lemma_slab_index`, and `guard_of` in
`SlabTable::is_guard_slab`, `lemma_guard_test` (`class/table.rs:2118`),
`ClassLine::lemma_sized_ok` (`heap/shared.rs:621`) and
`lemma_group_well_formed` (`class/decay.rs:1084`). `is_slot` is in
`user_block`, so in `fits` (theorem 1), and in `slot_start`, so in
`free_target` (theorem 3): every row of the interface contract whose R
or B cell cites them now carries the guard exclusion, with no change to
any entry point's `ensures`. The proofs are
`Regions::lemma_user_block_not_guard` (`layout/regions.rs:1507`): a slot
lies inside its own slab, which `is_slot` puts outside the guard slabs,
and inside its class region, apart from every other class region and
from the huge region (`separated`); a huge block lies in the huge
region.

**Where `is_slot` is established.**
- Claims: `lemma_real_is_slot` (`class/mod.rs:731`) takes the bit's
  slab from `SlotToks::entry` (`:482`), which now records that the slab
  is not a guard slab. `finish` gets it from `picks_marks` (`:3695`),
  which `take` ensures from the class lock's `ready` (`:1683`:
  `!spec_is_guard`). The zero picks' slots get `is_slot` in `finish`
  itself, from the same premise: `Regions::lemma_is_slot` at the call
  (`class/mod.rs:3264`); `SlabTable::slot_ptr` (`class/table.rs:3493`)
  ensures only the slot's address.
- Unsized frees: `SlabTable::is_set_unchecked` (`class/table.rs:3116`)
  ensures `r ==> !guard_slab(class, slab)`. It now reads the word through
  `vspec::word::load` (`vspec/word.rs:96`) inside the word's invariant,
  where `bitmap::guards_ok` (`bitmap.rs:3511`) holds: a guard slab's bits
  keep their fresh markers, so `marks_ok` makes them clear
  (`lemma_guard_clear`, `:3553`). `check_live` and `check_located`
  (`class/table.rs:3661`, `:3616`) re-export it; `Shared::classify_free`,
  `set_slot` (so `heap::free_fast_slot`, the `free` fast path,
  `heap/paths.rs:832`; `checked_slot` is test-only) and the three
  `api/libc.rs` `check_live` callers (`usable_size`,
  `old_size_or_keep`, `realloc_old`) turn it into `is_slot`.
  Since upstream #78 the fast path is `heap::free_fast_slot`, whose bit
  test is `set_slot`. (Before #87 `set_slot` took its slab from a
  region-unbounded `slab_slot` and bounded it only by the watermark
  compare; that intermediate code is not on main.) Since upstream #87
  `set_slot` reads the class's line (`ClassLine::set_slab`): its
  slot-start test is `Class::slot_of_offset`'s on the line's copy of
  the class geometry (`ClassLine::lemma_slot_start`, over
  `layout::lemma_slot_test`), its region bound the compare with the
  line's watermark copy, which the copy's invariant bounds by the
  table's slab count, the region's (`ClassLine::wf`,
  `heap/shared.rs:439`; on main `SlabTable::is_handed_out` no longer
  states the slab count, `ClassLine::wf` does), and its bit test
  `SlabTable::bit_word` (`class/table.rs:4748`, a wrapper of
  `bit_word_held`, the word load `is_set_unchecked` calls), which
  ensures `!guard_slab` of a set bit; so `set_slot` still ensures
  `is_slot`, and that the slab is below the slab count.
- Sized frees: `Shared::sized_slot` (`heap/shared.rs:3606`) through the
  class line's `ClassLine::sized_ok` (`:598`). (Before upstream #101 the
  test was `GuardTest::is_guard`, which a `bit_vector` lemma proved
  equal to `guard_of`, with `Shared::wf` stating that `guard_test[c]`
  tested the class's geometry.) Since upstream #87 the
  test is the class line's (until #101 `lines[c].guard`, which `ClassLine::wf`
  stated is `GuardTest::of` the table), and the slot-start test the
  line's (`ClassLine::sized_ok`, after the region bound), in place of
  `Regions::slot_in_class`. Since upstream #101 the region bound and the
  slot-start test are one 64-bit exact-division test, and `GuardTest` is
  gone: `ClassLine::lemma_sized_ok` proves that what `sized_ok` accepts
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
and `AtomicBitmap::init` (`bitmap.rs:680`) now requires a slab that is
not a guard slab (`lemma_guards_move`, `:3759`); `take_group` calls it
only past the `is_guard_slab` test.

**Huge runs (at the huge lock invariant).** Each run's guard follows
its usable pages inside its extent, and live extents are pairwise apart
(`huge::Inner::wf`). `Inner::theorem_run_guards` (`huge/mod.rs:3140`)
states the consequence for every value the huge lock admits, and for
nothing per call: the usable pages of a live run
share no page with the guard pages of any live run, its own included.

**Premises beyond the common ones.**
- `table::axiom_slot_loc` (`class/table.rs:2431`) now also fixes
  `guard_geom(c)` to the table's geometry: the same one-table-per-class
  assumption that names the slot tokens' authority, no new kind of
  premise.
- `heap::shared::axiom_shared_built` (`heap/shared.rs:1459`) now also
  states the stored guard tests, as it states the tables (since
  upstream #87 the stored class lines, which carry them).
- Since upstream #87, the class lines' premises ("Class lines", common
  premises): the watermark copy is never ahead of the watermark, and
  the copy's pointer stays valid.
- `vspec::word::load` (`external_body`, one atomic load of the word, as
  the three writers are one atomic step each); its result is the
  permission's value under sequential consistency.
- Class lock axioms for `ready`, and `api::shared_unchecked`'s (since
  upstream #77 `api::shared_deref`'s) `Shared::wf`
  for the guard tests and the table geometry the free paths read (since
  upstream #87, for the class lines).

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

**Changed since #138.** The statement is unchanged. The run facts
`lemma_run_next` and `lemma_run_covers`, with `spec_run_start` and
`spec_run_end`, are not on main (above); the statement never used them.
Two facts moved without weakening the theorem, which holds on main:
`is_slot` of the zero picks is proved at `finish`'s call site
(`Regions::lemma_is_slot`) rather than ensured by `SlabTable::slot_ptr`,
and that the table's slab count is the region's is a conjunct of
`ClassLine::wf` rather than an ensures of `SlabTable::is_handed_out`.
Since wolfi-dev/challoc#441 `ClassLine::wf` also admits a zero guard
mask, on which the sized check accepts nothing, and `Shared::built`
states the table's mask, which #138's `ClassLine::wf` stated: the same
fact for a well-formed `Shared`.

## 8. No syscall on the fast path

Read at the lane commit `32171ab`, before its merge onto the live-block
model (the `file:line`s are the `fn` line there).

**Statement** (`sys/mod.rs:111`, `:121`; `api/libc.rs:673`):

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

- `malloc`: `class_index` (`api/libc.rs:100`), `api::shared_ptr`
  (`api/mod.rs:871`), `heap::existing_heap` (`heap/thread.rs:382`: the
  TLS read and `heap_from_slot`), `heap::fast_flags` (`:460`) and
  `Heap::alloc_fast` (`heap/paths.rs:80`), whose body is `pop_fast`
  (`:125`). `calloc`'s cache pop is the same `pop_fast`, and
  `operator new`'s fast path is `malloc_fast` (`api/libc.rs:431`).
  Since upstream #101 a size of at most `SMALL_MAX` takes its class from
  the small table alone (`class_index_small`, over `class_of_small` and
  `layout::small_class`, whose `assert_unchecked` is the table's
  postcondition) and a larger one tail-calls `malloc_medium`
  (`class_index`); both pop through `malloc_class` (`shared_ptr`,
  `existing_heap`, `fast_flags`, `alloc_fast`).
- `free`: `existing_heap`, `shared_ptr`, `api::shared_unchecked`
  (gone on main), `Shared::push_flags` and `heap::free_fast` (before
  upstream #78; on main `free_fast`, at `heap/paths.rs:1547`, and
  `Shared::checked_slot` are test-only), with the check (`Shared::checked_slot`) and the
  push (`Heap::push_fast`) inside it. Since upstream #78 (null left to
  the slow path, the heap and the dispatch byte produced after the bit
  test): `heap::tls_heap_slot` (the TLS read), `api::shared_unchecked`,
  `sys::opaque` and `heap::free_fast_slot`, with the bit test
  (`Shared::set_slot`), `Shared::free_flags`, `heap_from_slot` and
  `free_fast_push` (the owner test `Shared::home_owns` under
  `FREE_ROUTE`, and `Heap::push_fast`) inside it.
  Since upstream #77 `api::shared_base` and `shared_deref` take the
  place of `shared_unchecked` and `sys::opaque`, and the bit test's
  word index goes through `class::table::word_index`. Since upstream
  #87 the bit test reads the class's line (`ClassLine::set_slab`: its
  `slot_start`, `class::load_mirror` of the watermark copy, the aarch64
  `bit_fields`, `word_index` and `SlabTable::bit_word`), the owner test
  is `ClassLine::owner` (`SlabTable::owner_from`), and
  `free_fast_tail` takes the place of `free_fast_push`, with the heap
  from `heap_from_slot` in each arm of its dispatch.
- `free_sized`, and `sdallocx` with no flags: `class_index`, `shared_ptr`,
  `existing_heap`, `shared_unchecked` and `heap::free_sized_fast`
  (`heap/paths.rs:991`); since upstream #77 `shared_base` in place of
  `shared_ptr`, and `shared_deref` in place of `shared_unchecked`; since
  #87 `sized_slot` reads the class's line (`ClassLine::sized_ok`); since
  #101 `free_sized`'s class comes from `class_index_small` (a larger size
  tail-calls `free_sized_medium`), through `free_sized_with`, which
  `sdallocx` with no flags calls too, and `sized_ok` reads the guard word
  (`ClassLine::guard_word`, `is_guard`).
- `challoc_free_usable`: `existing_heap`, `shared_ptr`,
  `shared_unchecked`, `heap::free_fast_class` (before upstream #78;
  test-only on main, `heap/paths.rs:1558`) and
  `class_size` (`api/libc.rs:1800`); since upstream #78
  `tls_heap_slot`, `shared_unchecked`, `free_fast_slot` and
  `class_size` (since #77 `shared_base` and `shared_deref` in place of
  `shared_unchecked`).

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
`free_usable_slow`; since upstream #101 also `malloc_medium` and
`free_sized_medium`, for sizes above the small table), which requires
`may_syscall()`.

**Why the capability can be trusted.**
- The only instructions that enter the kernel are the `syscall`/`svc 0`
  in `sys/raw.rs` (`syscall0`..`syscall6`, both architectures,
  `sys/raw.rs:37`-`:138`, `:172`-`:269`), each `requires may_syscall()`.
  Main's ledger lists all 14 as trusted, partly checked by Isla up to
  the system call (x86_64 since wolfi-dev/challoc#415, on the Sail x86
  model); the kernel's side is not checked.
- Every trusted body or specification that can reach the kernel, by a
  syscall, a libc call, a call through a function pointer or a verified
  function it calls unchecked, `requires may_syscall()` too:
  `linux::try_reserve` (`layout/regions.rs:2002`), `ThreadKey::create`
  (`heap/shared.rs:967`), `atfork_handlers` (`api/mod.rs:1407`),
  `background::spawn` and `pthread_join` (`purge/background.rs:657`,
  `:483`; `init_fields` verifies since P6, its `tid()` and `now_ns()`
  checked, `Meta::commit` since package META and `Heap::destroy` since
  P10), `print::call_back`, `print::write_file` and
  `jemalloc::call_hook` (the report's call-outs, theorem 16; `print::emit`
  is verified since wolfi-dev/challoc#413), the
  C++ `call_handler` and `call_handler_caught` (the `nothrow_t` forms'
  call through `cxx_handler.cpp`'s catch),
  `call_get_new_handler`, `call_throw_bad_alloc` (`api/cxx.rs:220`,
  `:238`, `:152`, `:181`), the
  `pthread_setspecific` wrapper (which may allocate; `heap/shared.rs`,
  from theorem 15, in place of an `assume_specification`).
  (`shared::seed`, which reads `getrandom`, is verified: its syscalls
  are in `getrandom_exact` and `now_coarse_ns`; so is `Set::ensure`,
  which commits set words through `Meta`, and `thread.arena` calls
  `Heap::set_arena` directly. `sys::getrandom`, `read` and `write` are
  verified too: they reach the kernel through `raw::syscall3`, and their
  count bounds are kernel-fact axioms on its result.)
- `may_syscall` is opaque, so a verified function can prove it only from
  its own `requires` (or a loop invariant that carries it in from
  there), from `grant_syscalls`, or from the type invariant of a
  `Stderr` (the abort's sink, `sys/abort.rs:25`) or a `Sink` (the
  report's, `api/print.rs:159`), which only a context that holds it can
  build (`Stderr::new` and `Sink::new` require it; `Stderr`'s `Default`
  is outside the proofs). It is also closed: no module outside `sys` can
  reveal it.
- `grant_syscalls` is called only in the bodies of `extern "C"`
  functions and of `sys::abort_fmt` (`sys/abort.rs:139`), which never
  returns; a panic, a failed `vspec::check` included, reaches it through
  the `#[panic_handler]` (`lib.rs:98`). Outside tests, no Rust code
  calls an `extern "C"` function of the crate.
  `sys::tests::syscall_grants_only_at_roots` (`sys/mod.rs:1652`)
  checks the source: every grant is in an `extern "C"` function or in
  `abort_fmt`. That `may_syscall` appears only as a `requires` or loop
  invariant clause, or in `Stderr`'s and `Sink`'s type invariants, is
  read from the source (in #138 the test checked it).
- The later theorems' code follows the same discipline (merged after
  this section's commit). Fork-handler lock handoff (theorem 13):
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
  `heap_from_slot` (`heap/thread.rs:488`, PAC `autda` and a cast),
  `fast_flags` (a raw read), `sys::tls::get` and `get_tp` (a TLS load,
  `mrs`), `pac::auth` and `sign` (PAC instructions), `Heap::home_ptr_at`
  and `cached_recently` (`heap/cache.rs`, address arithmetic and loads),
  `arena_volatile` (`heap/mod.rs`, a volatile read),
  `load_owner` (a `Relaxed` load; verified since package OWNER, over
  `vspec::atom::load_u16_at`, an atomic load, which the fast path's
  `owner_from` uses) and
  the watermark's load (`vspec::atom::load_usize`, an atomic load;
  `load_usize_seen` since package OWNER),
  `vspec::word::load` (an atomic load; since upstream #87 the class
  line's watermark copy is read by the same `load_usize`, through the
  verified `class::load_mirror`), `layout::regions::class_entry`
  (an empty `asm!`), `addr_to_ptr`, `api::shared_ptr` and
  `shared_unchecked`; since upstream #78 also `Shared::free_flags` (a
  `Relaxed` load; since package FFLAGS `vspec::flags::load`) and
  `sys::opaque` (an empty `asm!` on aarch64), and
  since #77 `api::shared_base` and `class::table::word_index` (each an
  empty `asm!` on aarch64; since ASMHINT trusted on aarch64 only, the
  x86_64 items verified, `shared_base` over `api::shared_storage_ptr`,
  a raw-pointer projection of `SHARED`) and `api::shared_deref` (a
  reference to the static) in place of `sys::opaque` and `shared_unchecked`; since
  upstream #101 `ClassLine::guard_word` (one load, and on x86_64 an empty
  `asm!`), and the specification of `core::hint::assert_unchecked` (no
  instruction). Since package P7 `heap_from_slot`, `fast_flags`,
  `shared_deref` and `class_entry` are verified (Verus checks they make no
  system call); in their place the trusted bodies are
  `heap::thread::slot_heap_ref` (a cast), `layout::regions::entry_barrier`
  (an empty `asm!`, x86_64) and `heap::paths::shared_ptr` (a cast, on
  the cold paths only).
  So do the `core` functions they use (`Option::map`, `?`, atomics,
  pointer arithmetic), which the common premises already trust.
- Two values reach the kernel from code with no `requires`, and only a
  frame with the capability can make them. `LockGuard::drop`
  (`lock.rs:985`, `external_body`; a destructor cannot have a
  `requires`) may `FUTEX_WAKE`, and a guard comes only from `Lock::lock`
  (`lock.rs:696`). `Sink`'s `fmt::Write::write_str` (verified, with
  `may_syscall()` in `Sink`'s type invariant; reached through
  `core::fmt`) flushes, and a sink comes only from
  `Sink::new` (`api/print.rs:181`). Both require `may_syscall()`, and no
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
  `free_aligned_sized`'s hint (`class_for`) are not covered. Since
  upstream #110 part of init's work runs later, on slow paths that
  require `may_syscall()` too: the cgroup discovery at the first
  periodic tick past `sys::cgroup::DEFER_NS` (`purge::sample` through
  `Shared::discover_cgroup_if_due`), at the first read of its figures
  (`Shared::cgroup_now`) or in `fork_prepare`
  (`discover_cgroup_for_fork`); the metadata object side's deferred
  commit at the next object or table commit (`meta::Inner::flush`);
  and the per-size THP probe at the feature line's first request
  (`api::line_ready`). Since upstream #111 the periodic tick also runs
  the lazy start's look (`purge::on_tick` → `background::auto_tick`:
  `policy::empty_bytes`, the state lock and, from an allocation's tick
  only, `start_requested` → `start`), behind `Heap::tick`, which
  requires `may_syscall()`. None is reached from a fast path.
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

**Changed since #138.** Weaker on main: the theorem now requires
`integrity_aborts()`, so a fast path may abort at a corrupt PAC-signed
cache entry or slot value, and that abort writes and raises `SIGABRT`
(in #138 it required no abort capability). The grant test is
`sys::tests::syscall_grants_only_at_roots`, and it no longer checks
where `may_syscall` appears as a clause. The source test
`api::restated::fast_path_restatements_match_their_bodies` is new; it
ties the restated paths to their bodies by call names. The
`sys/raw.rs` wrappers, Isla-discharged in #138, are partly checked by
Isla on main, which does not check the kernel's side.

## 9. Fast paths take no lock and terminate

The masked fast-path functions listed below (the fast branches of
`malloc`, `free` and `challoc_free_usable`, and the callees of the
`free_sized` and `sdallocx` fast branch below `free_sized_fast`) call no
lock acquisition, and each terminates. On main `free_sized_fast` and
`Shared::sized_slot` declare no mask (so `any`): that the sized fast
branch takes no lock is read off those two bodies, not proved. The
entry points themselves are not masked, the slow paths are outside the
theorem, and a bound on the step count is read off the code, not proved
("Scope: which paths", "What stays exempt").

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
pub fn lock(&self)                                  // RawLock, lock.rs:98
    opens_invariants any

pub fn lock<'a>(&'a self, Tracked(ord): Tracked<&'a mut LockOrder<'_>>, Tracked(under): Tracked<&mut LockOrder<'a>>) -> (g: LockGuard<'a, T>)   // Lock<T>, lock.rs:696
    ensures lock_inv(g@), lock_frame(lock_init(*self), g@), ...
    opens_invariants any
```

and so, by default, do the other acquiring functions (`try_acquire`,
`lock_slow`, both `try_lock`s, `lock.rs:78`, `:113`, `:87`, `:728`). The
fast paths declare `none`, or only the bitmap words' and the watermarks'
namespaces for the unsized free (`bitmap::word_ns()`, the one
`AtomicInvariant` `is_set_unchecked` opens for its bit test, and
`class::committed_ns()`, the one `SlabTable::watermark` opens to read
the watermark, which its invariant bounds by the slab count):

```rust
pub(super) fn malloc_fast(size: usize, blk) -> (r: Option<*mut u8>)   // api/libc.rs:431
    opens_invariants none
pub fn alloc_fast(&mut self, class: usize, flags: u8, blk) -> ...    // heap/paths.rs:80
    opens_invariants none
pub fn free_fast(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8, blk) -> (r: bool)   // in #138; heap/paths.rs:1547 on main is #[cfg(test)], outside verus!, no mask
    opens_invariants [crate::bitmap::word_ns(), crate::class::committed_ns()]
pub fn free_fast_class(...) -> (r: Option<usize>)                    // in #138; heap/paths.rs:1558 on main as free_fast
    opens_invariants [crate::bitmap::word_ns(), crate::class::committed_ns()]
pub fn free_sized_fast(...) -> (r: bool)                             // heap/paths.rs:991
    // opens_invariants none in #138; no mask (so `any`) on main
pub fn existing_heap(shared: *const Shared) -> ...                   // heap/thread.rs:382
    opens_invariants none
pub unsafe fn shared_deref(p: *const Shared) -> (r: &'static Shared)  // api/mod.rs:939
    opens_invariants none
```

Since upstream #78 the unsized `free` fast path is these two, in place
of `free_fast` and `free_fast_class` (which are now test-only wrappers
of the same `set_slot` and `free_fast_push`; on main they are
`#[cfg(test)]` helpers outside `verus!`, with no mask or `ensures`):

```rust
pub unsafe fn free_fast_slot(shared: &Shared, t: usize, tp: usize, p: *mut u8, blk) -> (r: Option<usize>)   // heap/paths.rs
    opens_invariants [crate::bitmap::word_ns(), crate::class::committed_ns()]
fn free_fast_push(table: &SlabTable, slab: usize, class: usize, h: &mut Heap, p: *mut u8, flags: u8, blk) -> (r: Option<usize>)
    opens_invariants none
```

and since upstream #77 the free paths reach `Shared` through these two,
in place of `shared_unchecked`:

```rust
pub fn shared_base() -> (r: *const Shared)                          // api/mod.rs
    opens_invariants none
pub unsafe fn shared_deref(p: *const Shared) -> (r: &'static Shared)
    opens_invariants none
```

Since upstream #87 the dispatch after the bit test is this one, in
place of `free_fast_push` (the heap comes from the slot value in each
arm), and the bit test reads the class's line, whose watermark copy's
invariant is in `committed_ns()` too (`class::load_mirror`), so
`free_fast_slot`'s mask is unchanged:

```rust
unsafe fn free_fast_tail(shared: &Shared, class: usize, slab: usize, p: *mut u8, flags: u8, t: usize, tp: usize, blk) -> (r: Option<usize>)   // heap/paths.rs
    opens_invariants none
```

Since package OWNER the owner word is read inside its invariant
(`class::table::OwnerPred`, in `class::owner_ns()`), so the owner test
and the functions above it declare that namespace: `free_fast_tail`,
`Shared::home_owns`, `ClassLine::owner` and `SlabTable::owner_from`
declare `[crate::class::owner_ns()]`, and `free_fast_slot` adds it to
its two:

```rust
pub unsafe fn free_fast_slot(shared: &Shared, t: usize, tp: usize, p: *mut u8, blk) -> (r: Option<usize>)   // heap/paths.rs
    opens_invariants [crate::bitmap::word_ns(), crate::class::committed_ns(), crate::class::owner_ns()]
unsafe fn free_fast_tail(shared: &Shared, class: usize, slab: usize, p: *mut u8, flags: u8, t: usize, tp: usize, blk) -> (r: Option<usize>)   // heap/paths.rs
    opens_invariants [crate::class::owner_ns()]
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
loop except `cached_recently` (`heap/cache.rs:740`, the
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
  `malloc_slow_class`. Since #101 `class_index_small` in place of
  `class_index` for a size of at most `SMALL_MAX`, then `malloc_class`
  (the same calls); a larger size tail-calls `malloc_medium`.
- `free` (`free_block`, `api/libc.rs`): `existing_heap`, `shared_unchecked`,
  `Shared::push_flags`, `free_fast`; then `free_slow`. Since upstream
  #78: `tls_heap_slot` (on a ghost `TlsSlot::entry()`),
  `shared_unchecked`, `sys::opaque`, `free_fast_slot`; then `free_slow`,
  which returns at once for null (no null test of the fast branch's
  own: null is in no class region). Since #77 `shared_base` and
  `shared_deref` in place of `shared_unchecked` and `sys::opaque`.
- `free_sized` and `sdallocx` with `flags == 0`: `class_index`, then
  `free_sized_block`'s `existing_heap`, `shared_unchecked`, `push_flags`,
  `free_sized_fast`; then `free_sized_slow` (since #77 `shared_base`,
  its `existing_heap` and `shared_deref`). Since #101 `class_index_small`
  for a size of at most `SMALL_MAX` (a larger one tail-calls
  `free_sized_medium`, outside the masks as `free_sized_slow` is), and
  the branch is `free_sized_with`'s, `sdallocx`'s too.
- `challoc_free_usable` (`free_usable_block`): its `free_fast_class` call on the same heap
  lookup (since upstream #78, `free_fast_slot` on `free`'s TLS read,
  without `sys::opaque`; since #77 through `shared_base` and
  `shared_deref`). The class size it then returns comes from the
  unmasked `class_size`, outside the theorem.

**The closure.** These are the functions with a declared mask on these
paths, verified unless marked trusted:
- **Size and process state:** `class_index`, `layout::class_of`;
  `api::shared_ptr` (trusted), `shared_unchecked` (trusted; since
  upstream #77 `shared_base`, trusted (since ASMHINT only on aarch64,
  the x86_64 item verified, over the trusted `shared_storage_ptr`), and
  `shared_deref`, trusted until package P7 and verified since); since
  #101 `class_index_small`, `layout::class_of_small` and `small_class`.
- **Heap lookup:** `existing_heap` (on a ghost `TlsSlot::entry()`),
  `existing_heap_in`, `tls_heap_slot`, `sys::tls::get` and
  the aarch64 `get_tp` (trusted asm), the x86_64 `get_tp`, `fast_flags`
  (verified since P7), `heap_from_slot` (verified since P7, over the
  trusted `slot_heap_ref`).
- **Pop:** `Heap::alloc_fast`, `pop_fast`, `cache_ptr`, `home_ptr_at`
  (trusted), `pop_top`, `CacheHeader::slots`.
- **Push:** `free_fast_slot`, `free_fast_push` (since upstream #78; before,
  `free_fast`, `free_fast_class`; since #87 `free_fast_tail` in place of
  `free_fast_push`), `free_sized_fast` (no mask on main; `none` at
  #138),
  `Heap::push_fast`, `push_cache_flags`, `push_cache`, `cached_recently`
  (trusted).
- **Free checks:** `Shared::push_flags`, `Shared::free_flags`
  (since #78; trusted until package FFLAGS, verified on main over the
  trusted `vspec::flags::load`), `home_owns` and `set_slot` (before #78,
  `multi_arena` and `checked_slot`), `sized_slot` (no mask on main, and
  its `hint?` uses vstd's default mask; `none` in #138),
  `GuardTest::is_guard`,
  `Heap::arena_volatile` (RefinedRust in #138; on main discharged
  by Kani, `arena_volatile_reads_arena`, `heap/mod.rs:7395`); `SlabTable::watermark`,
  `is_handed_out`, `is_set_unchecked`, `owner_unchecked`,
  `owner_at`, `owner_from`, `handed_out_seen`, `check_live_seen` (on
  main `committed_seen`, `class/table.rs:3334`, and the proof
  `lemma_held_seen`, `:3247`),
  `load_owner` (verified since OWNER; its loads are `load_u16_at` and
  `load_usize_seen`, loom-discharged in #138, trusted in main's ledger,
  `load_u16_at` partly checked by Kani on one thread), and since #77 `word_index`
  (trusted on aarch64, an empty `asm!`; since ASMHINT the x86_64 item,
  the identity, verified). Since #87 the class line's
  `ClassLine::set_slab`, `slot_start`, `bit_fields` (aarch64), `owner`
  and `sized_ok`, `class::load_mirror`, and `SlabTable::bit_word` and
  `owner_from` in place of `is_handed_out`, `is_set_unchecked`,
  `owner_unchecked` (removed) and `owner_at`; `load_owner` (until
  package OWNER: since then `owner_from` loads through
  `vspec::atom::load_u16_at`, trusted, inside the word's invariant, and
  `load_owner` is not on the fast path), `word_index`
  stay, and `GuardTest::is_guard` until #101 (since then `GuardTest` is gone: the
  sized check is `ClassLine::sized_ok`, over `guard_word`, trusted, and
  `is_guard`).
- **Layout:** `Regions::slab_slot_unbounded` (since #78, for the
  unsized path; `slab_slot` before), `slot_in_class`, `region_len`, the
  free function `layout::regions::class_entry` (trusted; verified since
  package P7, over the trusted `entry_barrier` on x86_64);
  `Class::slab_size`, `slab_mask`, `slot_of_offset`. From #87 until #101 only
  `region_len` (the sized path's bound; since #101 `Shared::region_hi`,
  a field, and nothing from the layout): the lines hold the geometry,
  `slab_slot_unbounded` is gone and `slot_in_class` is test-only.
- **Primitives:** `sys::pac::sign` and `auth` (trusted asm, partly
  checked by Isla on main),
  `sys::opaque` (trusted, an empty `asm!` on aarch64, from #78 until
  #77 replaced it with `shared_base`),
  `vspec::addr::addr_to_ptr` (trusted), and the `vspec` items under
  "Premises".

**Code changes.** A masked function cannot use what vstd specifies with
the default mask. So the eight `?`s on these paths are now (at
#138; on main `sized_slot`'s is `hint?` again, below)
`let Some(..) = .. else { return None }`, and `alloc_fast`'s `map` is a
`match`. The four array reads (`class_base` twice, `tables`,
`guard_test`) and the `multi_arena` load in `checked_slot`'s path go
through the wrappers below; `owner_at`'s load is arena isolation's
trusted `load_owner` (theorem 14), which declares the mask. The release
library's machine code is identical to the previous commit's on x86_64.
Upstream #78 replaced the `multi_arena` load with the `FREE_ROUTE` bit
of the `free_flags` byte the push dispatch loads anyway
(`Shared::free_flags`, `opens_invariants none`; trusted until package
FFLAGS, now verified over `vspec::flags::load`), so the
`vspec::atomic::load_bool` wrapper below had no caller left and is
gone. From upstream #87 until #101 the class line's sized check spelled
`slot_start(rel).is_some()` as a `match` (vstd's `Option::is_some` has
the default mask; the same code), and `set_slab` and `owner` are given
the class's table for its word and owner permissions (it is not loaded
from: the machine code is upstream's). In #138 the verified
`sized_slot` spelled upstream's `hint?` as
`let Some(class) = hint else { return None }` to keep its mask, which
changed the order of two cold tail calls in `free_sized_medium` and
`sdallocx_flags`. Main keeps upstream's `hint?` and drops the masks of
`sized_slot` and `free_sized_fast`.

**Premises beyond the common ones.**
- **Trusted masks.** Each trusted function's mask is a declaration that
  Verus takes on trust. Their bodies were read, and none takes a lock or
  waits:
  - `shared_ptr` and `shared_unchecked` (a projection of `SHARED`
    and a reference to it).
  - `tls::get` and `get_tp` (asm), `fast_flags` (a field read),
    `heap_from_slot` (calls `pac::auth`), `home_ptr_at`.
  - `cached_recently` (the loop above), `arena_volatile`,
    `vspec::atom::load_usize` (the watermark's load; `load_usize_in` at
    #138), `class_entry`.
  - `pac::sign` and `auth` (asm; under `cfg(test)` a counter
    increment), `addr_to_ptr`.
  - Since upstream #78: `Shared::free_flags` (one `Relaxed` load;
    verified since package FFLAGS, over the trusted `vspec::flags::load`) and
    `sys::opaque` (an empty `asm!`, or the identity off aarch64).
  - Since upstream #77, in place of `sys::opaque` and
    `shared_unchecked`: `shared_base` and `word_index` (each an empty
    `asm!` on aarch64, trusted there; since ASMHINT the x86_64 items
    verify: `word_index` is the identity, and `shared_base` is
    `shared_storage_ptr`'s pointer (address and provenance), so on x86_64
    `api::shared_storage_ptr` (trusted, `opens_invariants none`, a
    projection of `SHARED`) is on these paths in its place; Isla records
    in #138; on main the aarch64 items are trusted, Islaris/Isla in
    the ledger, and the x86_64 items verified) and
    `shared_deref` (a reference to the static).
  - Since upstream #101: `ClassLine::guard_word` (one 8-byte load and,
    on x86_64, an empty `asm!`).
  - Since package P7 `shared_deref`, `fast_flags`, `heap_from_slot` and
    `class_entry` are verified (their masks are checked); in their place
    `heap::thread::slot_heap_ref` (a cast) and, on x86_64,
  trusted on this path: `store_owner` (one store per slab, through the
  markers), `vspec::atom::load_u16_at` and `load_usize_seen` (loom- and
  C11-discharged in #138, trusted in main's ledger, `load_u16_at` partly
  checked by Kani on one thread, whose harness assumes the same
  `store_owner` discipline; `u16_value`
- **New wrappers.** `vspec::atomic::load_bool` (removed with upstream
  #78, above): the `core`
  atomic load, declared `opens_invariants none`; `load_owner`
  (`class/table.rs`, a `Relaxed` load) declares the same mask.
  `vspec::slice::array_get_unchecked` stands for `get_unchecked` on an
  array.
- **Masks on existing specifications.**
  - Added here: `opens_invariants none` on the `vspec` specifications the
    paths use: `usize::leading_zeros` and `u32::rotate_right`
    (`vspec/core_int.rs`), and `<*mut T>::add` (`vspec/ptr.rs`); since
    upstream #101 `u64::rotate_right` (`vspec/core_int.rs`) and
    `core::hint::assert_unchecked` (`vspec/misc.rs`), both new.
  - Already declared: `vspec::word::load` (`vspec/word.rs:96`, the bit
    test's load), `<*mut T>::is_null` (`vspec/addr.rs:58`), and vstd's
    `ptr_ref`, `ptr_mut_ref`, `ptr_mut_write` and `wrapping_*`.
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
  the full-cache push, the `zero_on_free` push and a scan hit decline to
  the slow path, so they are outside the fast path by construction.
- **The twelve `exec_allows_no_decreases_clause` functions** (common
  premises) are all off these paths: `RawLock::lock_slow`
  (`lock.rs`), `ClassState::claim_with`'s retry (`class/mod.rs`),
  `api::init_slow` and `api::line_ready` (`api/mod.rs`), the OOM
  handler loop `api::cxx::oom`, the purge thread's `main`
  (`purge/background.rs`), `sys::cgroup::scan_lines` and
  `Deferred::discover` (`sys/cgroup.rs`), and the kernel retry loops
  `getrandom_exact`, `read_full` and `write_all` (`sys/mod.rs`), and,
  since wolfi-dev/challoc#413, `config::linux::env_var` (`config.rs`). They
  need a liveness model. Theorem 45 checks the
  compiled fast paths for back-edges (none).

**Where the statement is weaker than the code.**
- **Composition.** Verus checks the masked functions, not the entry
  points. That `malloc`, `free`, `free_sized` and `sdallocx` call only
  masked functions on their fast branch is read off their bodies, and
  so, on main, is that the unmasked `free_sized_fast` and `sized_slot`
  take no lock.
  `malloc`'s branch is an inlined copy of `malloc_fast`'s calls, not a
  call to it.
- **Step count.** Termination is proved; the step bound is not, because
  Verus has no cost model. The bound rests on the closure having no loop
  but `cached_recently`'s. Theorem 45 counts the
  steps of the compiled fast paths of the pinned release build
  (instructions, loads, stores), at machine level, outside Verus.
- **Other waits.** The masks prove that no lock acquisition is called,
  not that nothing waits. The fast paths contain no other wait either (no
  atomic read-modify-write, no spin), but that, too, is read off the
  code.
- **What Verus sees.** The theorem covers the code Verus sees, and the
  build compiles the same code: no `#[cfg(verus_keep_ghost)]` swap is on these
  paths.
- **Weak memory.** A load may return a stale value, but every branch on
  one still ends, so the bound holds.

**Changed since #138.** Weaker on main: `free_sized_fast` and
`Shared::sized_slot` declare no mask (both `opens_invariants none` at
#138; main's `sized_slot` uses `hint?`), so for the `free_sized`
and `sdallocx` fast branch the theorem covers only the functions below
them, and that those two take no lock is read off their bodies.
Termination still holds for them (verified, no loop, no
`exec_allows_no_decreases_clause`). Main renames `check_live_seen` and
`handed_out_seen` (`committed_seen`, `lemma_held_seen`), drops
`watermark_seen`, and replaces `load_usize_in` (mask
`[inv@.namespace()]`) with `load_usize` (mask `none`); `Lock::lock`
takes theorem 33's `LockOrder` tokens.

## 10. No leak on thread exit (when the destructor runs)

Two claims, of which only the first is proved. (a) When a heap's
thread-exit destructor runs, the flush it runs first returns every slot
the heap's caches held to the release path: proved, below. (b) Every
heap gets a destructor: not proved, and not true in every run. A heap is
registered for one by `ThreadKey::set` (`heap/shared.rs:1006`), which
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

Read at the lane commit `2801c34`, before its merge onto the live-block
model (the `file:line`s are the `fn` line there).

**Statement** (`heap/mod.rs:6441`, `Heap::flush_all`, which
`Heap::destroy` runs first, after only the arena unbind since the port
of upstream #103):

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

with (`heap/mod.rs:813`, `:3749`-`:3798`; abridged)

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
`heap/thread.rs:173`, through `Heap::destroy`, `heap/mod.rs:5539`)
strands no cached block when it runs (claim (a); whether it runs is
(b)). `tcache.flush` (`Tcaches::flush`, `heap/tcaches.rs:314`), `malloc_trim` (`api/libc.rs:2884`), the mallctl
purge's `flush_caller` (`api/mallctl.rs:2700`) and
`thread.tcache.enabled` off (`Heap::set_tcache_enabled`,
`heap/mod.rs:4616`) run the same function.

The released log (`CachePerms::released`, `heap/mod.rs:3164`, ghost) is
appended to by `Heap::flush` alone (by inspection: no spec keeps it
fixed elsewhere; the theorem's `ensures` are relative, so this does not
weaken them), with the batch it hands to `Shared::release_routed` and
the same tokens. The chain that makes an entry of the log a release:

- `Heap::flush` (`heap/flush.rs:380`) also ensures, for any `n`:
  every other owned header holds what it held, the cache holds a subset
  of what it held, and `log_adds(final.released(), old.released(),
  old.held(cache).to_set() - final.held(cache).to_set())`: the slots
  that left the cache are the log's new entries. `lemma_flush_split`
  (`heap/flush.rs:1040`) proves the split by counting (the cached,
  kept and released addresses are distinct, the kept are cached, the
  released are cached and not kept, and their numbers add up), and
  `Heap::lemma_flush_accounting` (`heap/mod.rs:4249`) lifts it to the
  pairs.
- `Shared::release_routed` (`heap/shared.rs:3352`) takes the bag of the
  batch's tokens by `&mut` and ensures `final(bag)@ == old(bag)@ -
  aset(ptr_addrs(ptrs@), 0, ptrs@.len())`: the bag lost exactly the
  batch's tokens. That each went to a `ClassState::release_owned` call
  (`class/mod.rs:5047`), which passes it, slab by slab, through
  `release_slab` to `AtomicBitmap::release` (`bitmap.rs:1241`), whose
  word invariant (`bitmap::WordPred`, `bitmap.rs:3535`) must take the
  token of every bit it clears, is read from the verified bodies, not
  stated by an `ensures`. The `FOREIGN` cache's and a home cache's
  other-arena slots are released the same way through the owner arena's
  class (`release_routed`'s routing); there is no other "remote" path.
- `Heap::flush_arena` (`heap/mod.rs:6315`) flushes one array whole and
  ensures its headers empty, the log gained what they held, and every
  other header unchanged; `flush_all` composes the home array and every
  node, using `lemma_home_apart` and `lemma_nodes_apart`
  (`heap/mod.rs:4360`, `:4374`) for the headers' distinctness.

**Premises beyond the common ones.**
- `Heap::destroy` is verified since package P10: it calls `flush_all`
  first (after giving a counted heap's arena binding back, which touches
  no cache: since upstream #103, so that a successor thread can be homed
  on the arena while the flush runs), on the heap `tcaches::heap_at`
  hands out for its pointer, `Heap::wf` by that function's premise, and
  not in use elsewhere by `tcaches::axiom_heap_use`
  (`heap/tcaches.rs:144`, trusted), whose `HeapUse` token `destroy`
  consumes: `heap_destructor` and `create_heap_for_thread` each make
  one. The heap `heap_destructor` hands it is the TLS slot's heap, whose
  `wf` `heap_at` assumes (common premise). After the flush
  `free_caches_in` (`heap/mod.rs:6156`) frees each array; it requires
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

**Changed since #138.** The heap's exclusive use is a named
premise on main: `Heap::destroy` and `tcaches::heap_at` take a `HeapUse`
token from `tcaches::axiom_heap_use` (`heap/tcaches.rs:144`); at
#138 it was `heap_at`'s unstated safety contract. `flush_all`
requires `may_abort() || valid_aborts()` in place of `may_abort()`.

## 11. Shuffle is a permutation

The `file:line`s are the `fn` line on main.

**Statement** (`class/mod.rs:332`, `:358`, and the `ensures` of
`shuffle_zero`, `:222`):

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
calls. `shuffle` (`:199`) is a test-only wrapper
(`cfg_attr(not(test), allow(dead_code))`, called only from
`class/tests.rs`) that sits outside `verus!` on main and calls
`shuffle_zero` with assumed ghost arguments; in #138 it was
verified with the same `ensures`. That move is cosmetic: the theorem
holds on main through `shuffle_zero`.

**Proof.** The Fisher-Yates loop (`for i in (1..len).rev()`, swap `i`
with `j = xorshift(rng) % (i + 1)`) keeps `permutes(out@, old(out)@)` as
an invariant; `lemma_swap_permutes` (`class/mod.rs:337`) shows one `<[T]>::swap`
leaves the multiset unchanged (the update-twice spec of `swap` in
`vspec/slice.rs`, with vstd's `to_multiset_update`). `j <= i < len` is
also checked (the `swap` precondition and the `%`), so the swap is in bounds and the remainder by `i + 1` never
divides by zero. Nothing about `xorshift`'s output is used.

**Where it applies.** `ClassState::claim_random_zero` (`class/mod.rs:2776`) is the
only caller outside tests: with `slot_randomize`, `Heap::claim`
(`heap/flush.rs:54`) calls it for every refill and run rebuild, and it
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
`assume_specification` of `<[T]>::swap` (`vspec/slice.rs:102`;
discharged by Kani in main's ledger) and vstd's multiset lemmas.

**Scope.**
- Randomness is out of scope: nothing is claimed about the distribution
  of the permutation, the quality of `xorshift`, or the seed from
  `Heap::rng` (`heap/flush.rs:115`; `seed` is a `getrandom` read, or a
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
  pre-shuffle batch has no name outside the function (exposing it would
  need a ghost return value, a signature change). Callers see only the
  token postconditions, which say nothing about order.
- The flush's grouping (`flush::group_by_slab`, verified to permute the
  batch) is a different reordering, not covered here.

**Changed since #138.** The statement is unchanged and holds on
main: `shuffle_zero`, the shuffle `claim_random_zero` runs, is verified
with the same `permutes`/`from_seq` ensures that #138's `shuffle`
had. The test-only wrapper `shuffle` now sits outside `verus!`
(cosmetic: no production code calls it).

## 12. Zero-on-free (at release)

Read at this section's commit (the `file:line`s are the `fn` line
there, or the release's line where a release is named).

**Statement** (`heap/shared.rs:3553`, `heap/zero.rs:458`, `:479`):

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
  least one, a block of the huge region (on main the run of the free's
  `Live` token, `blk.run_block(page)`, whose length is the run's usable
  size; `Shared::zero_huge` still ensures `huge_run_zero`,
  `heap/shared.rs:2545`), at the moment
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
- `Heap::free_slow_in` (`heap/paths.rs:699`): `zero_slot` first (after
  the rehome test only: since #103 a young heap's `FOREIGN` free may
  first go to `rehome_on_free`, whose push declines under `PUSH_ZERO`
  and reaches `free_slow_in` for the home arena, which zeroes it), then
  either `Shared::free_direct` (no cache for the class, `:771`) or
  `push_cache` (`:808`). It is reached from `Heap::free_slow` (the
  home-arena push of `Heap::free`) and `Heap::free_other` (another
  arena's slot).
- `heap::free_direct` (`heap/paths.rs:1172`), for a thread with no heap
  and `MALLOCX_TCACHE_NONE`: `zero_slot` before `Shared::free_direct`
  (`:1213`), `zero_huge` before `free_huge` (`:1233`).
- `Heap::free_huge` (`heap/paths.rs:390`): `zero_huge` before
  `free_huge` (`:414`).
- `Shared::free_direct` and `Shared::free_huge` have no other callers
  outside tests, and `push_cache` is called only by `push_cache_flags`
  and `free_slow_in`.
- Every other free of a slot is a cache push, and every push declines
  under `PUSH_ZERO`: `Heap::push_cache_flags` (`heap/cache.rs:658`) and
  `push_fast` (`:611`) ensure `flags & PUSH_ZERO != 0 ==> !r`, so
  `free_sized_fast` (`heap/paths.rs:991`) ensures the same and falls
  back to the exact `heap::free` (`free_fast` and `free_fast_class` are
  `#[cfg(test)]` helpers outside `verus!`); every push is handed
  `Shared::push_flags()` (`api/libc.rs` `free`, `free_sized_block`,
  `challoc_free_usable`; `api/jemalloc.rs` `sdallocx`; `Heap::push`;
  `Heap::free_other`), whose result is `flags()`, and
  `Shared::zero_on_free` ensures `r == (flags() & PUSH_ZERO != 0)`. A
  declined push keeps the slot's token for the slow path (`!r ==>
  *final(tk) == *old(tk)`), which is `free_slow_in`.
  Since upstream #78 plain `free` and `challoc_free_usable` push through
  `free_fast_slot`, which hands `free_fast_push` the `free_flags` byte
  in place of `push_flags()`; `free_fast_push` passes `push_fast` 0 for
  the bytes 0 and `FREE_ROUTE` and the byte without `FREE_ROUTE`
  otherwise, so it declines whenever the byte has `PUSH_ZERO`
  (`lemma_free_route_bits`, `heap/paths.rs:968`, states the `PUSH_PAC`
  and `PUSH_ZERO` bit facts, as in #138). Since upstream #87
  `free_fast_tail` makes the same dispatch in place of `free_fast_push`
  (each arm taking the heap from the slot value). `free_fast_slot` and
  `free_fast_tail` (`heap/paths.rs:832`, `:889`) ensure
  `zero_on_free(shared) ==> r is None`, where `zero_on_free`
  (`heap/zero.rs:448`) is the `PUSH_ZERO` bit of `flags()`, and
  `free_fast_tail` also ensures `flags & PUSH_ZERO != 0 ==> r is None`,
  as in #138 (restored by wolfi-dev/challoc#428).
  `Shared::lemma_route_flags` states that the byte's `PUSH_ZERO` bit is
  `flags()`'s, as `lemma_free_flags` did in #138.
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
- `vspec::zero::zero_raw` (`external_body`, theorem 6's token source;
  `write_zero` in #138): `write_bytes(p, 0, n)` makes `n` zero
  bytes. `zero_slot` and `zero_huge` clear through it (through
  `vspec::user::zero_user`, `vspec/user.rs:241`, on main), with the same machine code as a direct
  `write_bytes`.
- The writes take the freed block's permission (`vspec::user::UserMem`),
  which the free path holds in its `Live` token together with the
  slot or run token: `zero_slot` and `zero_huge` borrow it
  (`Live::take_mem`, `put_mem`), and `take_slot` or `into_run` then drop
  it with the token.
  `zero_huge` gets the run's token and clears exactly that token's usable
  pages, through the cast of the page's address
  (`vspec::user::axiom_cast`, the exposed-provenance premise).
  `axiom_slot_writable` and `axiom_huge_writable` are gone.
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
  `READY` but its atomics and, since upstream #110, the cgroup record's
  cell, `sys::cgroup::Deferred`, once); `zero_on_free` is a startup configuration key with no
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
  graph above, not stated as one Verus fact: the decline of `push_fast`,
  `push_cache_flags`, `free_sized_fast` and the plain `free` route's
  `free_fast_slot` is proved, and the slow paths' coverage is by
  inspection. A new release path
  that skipped the obligation would still verify. Making it mechanical
  means a ghost `Tracked<&Zeroed>` argument with the obligation as a
  `requires` on `Shared::free_direct`, `Shared::free_huge` and
  `push_cache` (no machine-code cost, but a signature change on paths
  other work is changing).
- The huge token's length is the free's `Live` token's (`Live::len`, the
  run's pages before the guard), which `zero_huge` ensures
  (`heap/shared.rs:3095`); that this is `huge_usable(size)` of the
  original request rests on the run token's model (theorem 5).

**Changed since #138.** `theorem_zero_on_free` no longer requires
the slot's uniqueness token (a weaker premise, so a stronger theorem),
and `theorem_zero_on_free_huge` is stated over the free's `Live` token
(`run_block`) in place of `Shared::huge_run_zero`, an equivalent
statement with a different premise shape. `zero_on_free` is the exec
`Shared::zero_on_free`, which ensures the same formula. The plain
`free` fast path states its decline as in #138: `free_fast_slot` and
`free_fast_tail` ensure `zero_on_free(shared) ==> r is None`, and
`lemma_free_route_bits` and `lemma_route_flags` state the `PUSH_ZERO`
bit (wolfi-dev/challoc#428, after an interval in which main dropped
them).

## 13. Fork-handler lock handoff

Formerly titled "Fork safety", as the code's comments still call it.
What is proved: under the mutex and handoff premises below,
`fork_prepare` returns holding every lock of the fork lock list
(`listed`, over the arenas it found) and the background state lock,
and the parent and child handlers release each of them once. Not proved: that
`listed` names every lock in the crate (a source test checks it,
`fork_lock_list_matches_the_source_tree`), that the prepare handler
terminates (a `lock` that never returns satisfies everything), and
compatibility with other `pthread_atfork` handlers (those registered
before challoc's run in the fork window, a premise below). Reclaiming the caches of threads the child no longer has is
out of scope (design 9 leaves them marked).

Read at this section's commit (the `file:line`s are the `fn`/`axiom`
line there).

**Statement** (`api/mod.rs:1611`, `:1776`, `:1789`, `:1798`; asserted at
`:2135` and `:2348`):

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

// end of fork_prepare (api/mod.rs:2135):
assert(fork_holds(*s, held, state));
// end of fork_release (api/mod.rs:2348), after every unlock:
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
its key names in `s` (`lock_at`, `:1624`): the value the accessor path
through `Shared` reaches, with `arenas[i]` standing for the arena that
`get_existing(i)` returned.

`fork_release` is the body of `fork_parent` and `fork_child`. It starts
from that hold and hands every token back through
`RawLock::unlock_held` (`lock.rs:178`), which requires the token of the
lock it unlocks and consumes it. A token is not `Copy`, and the map is
empty at the end, so each listed lock is released once, by its holder.
`fork_child` first rewrites the background state through
`Lock::get_unlocked`, which requires the state lock's token
(`lock.rs:776`); `purge::background::fork_child`
(`purge/background.rs:924`) proves the state's lock invariant on the
value it leaves (`:68`; since upstream #128 the invariant also has
`exited ==> !present`, and the child clears both). After either handler
no listed lock is held.
In the child, no lock is owned by a thread that no longer exists: at
the fork the forking thread held every listed lock (the mutex premise
below excludes a second holder), and the child's one thread is that
thread's copy, which releases them. Since upstream #111 the child
handler, under the lazy start, then sums the dirty bytes it inherited
(`policy::empty_bytes`, the lazy start's baseline): after the release,
from a lock-order token of its own (`axiom_entry`, theorem 33), so outside this
statement.

The tokens: `RawLock::lock_held` (`lock.rs:159`) is `lock` plus a
`LockHeld` whose `held()` is the lock. The field is private to
`lock.rs`, so outside it only `lock_held` builds a token; the handoff
axioms below are the other source.

**Premises beyond the common ones.**
- *The mutex.* `lock` returns only once this thread holds the lock, and
  no other thread holds it until `unlock`. This is `RawLock`'s futex
  contract; Verus does not check it (atomics return any value). The
  token records the calls; the mutex turns them into exclusive holds.
- *The handoff* (`axiom_fork_resumed_state`, `api/mod.rs:1820`, and
  `axiom_fork_resumed`, `:2534`; axioms). Ghost state does not cross the
  C calls, so the hold `fork_prepare` ends with is restated where the
  release begins: `axiom_fork_resumed` ensures `fork_holds` at the
  `&Shared` the release reads (`shared` returns the same static). The
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
  on the forking thread, so a `true` read there is this fork's). Each handler calls each axiom once. For
  `axiom_fork_resumed` a second call would prove `false` (the creation
  lock cell's permission it restates is exclusive), and the test
  `fork_hold_is_resumed_once` (`api/mod.rs:2919`) checks that its
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
  proved: `fork_window_open` requires `lock::fork_prepared`, which only
  `RawLock::lock_held` of the lock at `rank_state` ensures (the state
  lock, taken last; `RawLock::new` ranks no lock above it, so lock order
  admits no `lock_held` after it on `fork_prepare`'s one root token, from
  `lock::axiom_fork_entry`), and
  `RawLock::unlock_held` and `Lock::unlock_held_perm` require
  `lock::window_closed`, which only `fork_window_close` ensures. So the
  window opens after the last acquisition and closes before the first
  release.
- *A frozen arena table* is no longer a premise: `arena_held` and
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
  handoff (whose `axiom_fork_resumed` now also restates the cell
  permission as part of the hold, `ForkHeld::wf`).
- `axiom_state_lock` (`purge/background.rs:863`, an axiom, used in
  `STATE`'s initialiser): the static's raw lock is `state_lock()`
  (Verus does not read statics in specifications); its rank is proved,
  and `purge::background::state` verifies over the static's
  postcondition.
- The background state's invariant in the child also rests on
  `axiom_state_lock_inv` (`purge/background.rs:68`) and on
  `axiom_held`, behind `get_unlocked` (the value a holder left).
- *Completeness of the list.* That `listed` names every `Lock` in the
  crate is not proved. The test `fork_lock_list_matches_the_source_tree`
  checks the `Lock` fields and statics in the source tree against the
  test's `FORK_LOCKS` (`api/mod.rs:2825`); that `FORK_LOCKS`, `ForkLock`
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
  holds (with no exception since upstream #96, which takes the slab-set
  locks after the slab table locks). The release order is unstated.
- Partial correctness: a `lock` that never returns (a deadlock in
  prepare) satisfies everything. A prepare handler registered before
  challoc's that waits on another thread that allocates still deadlocks
  (design 9's residuals); one that allocates on its own thread runs in
  the fork window.
- Consistency in the child covers the one value the handlers write (the
  background state); the child also resets the deferred cgroup record
  and the feature line (`cgroup_fork_child`, `feature_line_fork_child`),
  which no listed lock guards, hands the statistics pool's objects the
  forking thread held to its new thread ID (`stats::pool_fork_child`,
  trusted), and reopens the init/fork gate (`fork_gate_exit`, plain
  atomics with no specification). The handlers do not touch the other locked values,
  which keep the invariant their last holder left, as the lock axioms
  assume for every acquire.
- The signal mask's save and restore are not part of this statement;
  they are theorem 39, "Fork handlers restore the signal mask".

**Changed since #138.** The statement is unchanged. `fork_prepare`
now takes its lock-order token from its own axiom,
`lock::axiom_fork_entry` (`lock.rs:467`), the one root token, while
`axiom_entry` (`lock.rs:448`), every other entry point's, ensures
`!o.root()`; in #138 `axiom_entry` gave the root. The axiom
`axiom_fork_resumed` also assumes that the saved signal mask lies inside
the blockable signals (theorem 39). #448 added the fork window and
`FORK_LOCKED` (premises above); the window's order against the hold is
proved through `lock::fork_prepared` and `lock::window_closed`.

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
`slab_owner(c, slab)` (`class/table.rs:628`), and for an address
`slot_owner(c, p) = slab_owner(c, slab_index(c, p))` (`:633`). The
allocation side and the release side are both stated against it: a
slot is released only into the arena that owns its slab, the arena whose
`plan` reserved in that slab, or the free aborts:

```rust
// class/mod.rs:3825, ClassState::plan (the claim's lock hold): every
// slab a refill or claim reserves slots in is this arena's.
ensures picks_owned(self.spec_class() as int, self.spec_arena(), final(picks)@, 0, np as int),

// class/mod.rs:3681
spec fn picks_owned(c: int, a: u16, p: Seq<Pick>, lo: int, hi: int) -> bool {
    forall|j: int| lo <= j < hi ==> slab_owner(c, (#[trigger] p[j]).slab as int) == a
}

// class/mod.rs:5047, ClassState::release_owned (the one function that
// releases slots into an arena's class state).
ensures
    forall|i: int| 0 <= i < r ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[i]@.addr) == self.spec_arena(),
    r < ptrs@.len() ==> slot_owner(self.spec_class() as int, ptrs@[r as int]@.addr) != self.spec_arena(),

// class/mod.rs:4984, ClassState::release_with: all or abort (`wrong_arena`).
ensures
    forall|i: int| 0 <= i < ptrs@.len() ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[i]@.addr) == self.spec_arena(),
```

`spec_arena` (`class/mod.rs:2065`) is the class state's `arena` field;
`Arena::inv` (`arena.rs:1152`) now states that `classes[c]` has arena
`index`, so `Arena::class` (`:1360`) ensures `r.spec_arena() ==
self.spec_index()`, and the lookups by index ensure the arena they
return has that index: `Shared::owner` and `Shared::arena`
(`heap/shared.rs:2846`, `:2803`), `Arenas::get`, `materialise`, `build`
and `get_existing` (`arena.rs`). `Shared::wf` gains `arena0.spec_index()
== 0`, proved by `Shared::init` from `Arena::init`'s new postcondition.

**Per free path.** Every slab free reaches an arena's class state only
through `release_owned`, from one of two places:

| Path | What is proved |
|---|---|
| `Shared::free_direct(arena, class, p)` (`heap/shared.rs:2934`), from `heap::free_direct` (`heap/paths.rs:1172`: `MALLOCX_TCACHE_NONE`, a thread without a heap) and `Heap::free_slow_in` (`:699`, no cache for the class) | ensures `self.wf() ==> slot_owner(class, p) == arena`: it releases through `owner(arena).class(class)`, of arena `arena`, and `release_with` aborts (`wrong_arena`) otherwise. Both callers pass the owner: `heap::free_direct` the arena of `classify_free`, which ensures it is `slot_owner(c, p)` given the block's `Live` token (`heap/shared.rs:2976`, from `check_located`, `class/table.rs:3616`), and `free_slow_in` that of `arena_of_slot` (`heap/shared.rs:3303`); so by their contracts `wrong_arena` cannot fire from them (Verus does not state it at the abort site). |
| `Shared::release_routed(arena, class, ptrs)` (`heap/shared.rs:3352`), from `Heap::flush` (`heap/flush.rs:380`): every cache, home or `FOREIGN` | each run goes through `owner(a).class(class)` (arena `a`), whose `release_owned` releases only `a`'s slots and stops at the first slot of another arena; the next run starts at `arena_of_slot` of that slot, which ensures `slot_owner` (`class/table.rs:633`). Termination is now proved (`decreases 2 * left + [next slot not a's]`), which removes `release_routed`'s `exec_allows_no_decreases_clause`: with owners fixed, the `changed arena during a flush` abort is unreachable, which its branch now asserts (`assert(false)` in a proof block). |

Upstream of those, `Heap::free` (`heap/paths.rs:239`) pushes a slot onto
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
counts its runs. `Shared::free_huge` (`heap/shared.rs:3181`) credits
`owner(run.arena)`, which now provably has index `run.arena`, the arena
`Huge::free_run` read from the run's descriptor, which `alloc_run`
wrote from its `arena` argument.

**How the claim side is proved.** `Inner::base` (`class/mod.rs:1600`), part
of the class lock invariant, gains `owns(s) ==> slab_owner(c, s) ==
arena`. Slabs join an arena's ghost only in `next_fresh_slab`, as the
group `take_group` just returned, and `take_group` (`class/table.rs:3732`)
now ensures that every slab of that group has `slab_owner` the taking
arena, from its owner-word store (`lemma_fresh_group`,
`class/fresh.rs:339`). `take` (`class/mod.rs:4015`) requires `ready`, so
an owned slab, and ensures its pick's owner; `plan` collects
`picks_owned`.

**Premises beyond the common ones.**
- `class::table::store_owner` (`class/table.rs:979`, `external_body`;
  before package OWNER also `load_owner`, `:907`, verified on main):
  `take_group`'s `Relaxed` store of the owner
  word fixes `slab_owner` for the group's slabs, and a `Relaxed` load
  of a handed-out group's word returns it. Together: each group's word
  is written once, under the commit lock, before the watermark passes
  the group, and never again (the only other writer is the test-only
  `corrupt_owner_word`). `owner_at`, `owner`, `check_located` and
  `arena_of_slot` are verified on top of `load_owner`. These replace the
  informal assumption `release_routed`'s termination exemption rested
  on.
- `ClassState::owner_locked` (`class/mod.rs:2487`, verified since
  package OWNER; a trusted bridge before) ensures its result is
  `slab_owner` and that a result naming this arena means the slab is
  owned (see the OWNER bullet below).
- `stats::arena_at` (`stats.rs:879`, verified) ensures the arena it
  returns has index `i`, from `get_existing`'s postcondition; not a
  premise.
- `arena::axiom_arena_assemble` now requires every class state's arena
  to be the parts' index (`Arena::init` proves it from
  `ClassState::new`'s new postcondition); `heap::shared::
  axiom_shared_built` now states `arena0` as written, as it states the
  tables.
- `slab_owner` is keyed by class, which rests on one table per class per
  process: the premise of `table::axiom_slot_loc` (theorem 5), not a new
  kind.
- "Written once" is checked, up to the lock premise: `store_owner`
  uses up the owner-store markers (`table::owner_mark`) of its group's
  slabs, which the class's table minted once each (`table::mark_loc`,
  fixed by `axiom_slot_loc`) and its commit lock holds from the
  watermark on (`TableInner::inv`), so a second store of a slab's owner
  does not verify, given that every holder of the commit lock restores
  its invariant before releasing it (`axiom_table_lock_inv`; not
  checked at the release, `LockGuard::drop`). "Handed out before read"
  is not checked by Verus: `owner_at` requires only `slab < nslabs`,
  and its callers (`owner`, `check_located`) test the watermark first;
  `owner_from` (the class line's path, since upstream #87, through the
  line's copies of `owners_base` and `lg_group`) calls `load_owner` with
  no witness, so it claims nothing of the value (its caller tests the
  class line's watermark copy, which is never ahead of the watermark:
  "Class lines", common premises). In #138 the callers also
  included `owner_unchecked`, gone on main.
- Sequential consistency for the owner words, as for every atomic.
  `check_located` reaches the owner load after a `Relaxed` watermark
  load (`check_live`), the one owner path without `Acquire`.
- **Since package OWNER** the first two premises, and the "handed out
  before read" half of the "written once" one, are smaller.
  `load_owner` verifies: the watermark's invariant
  (`table::WatermarkPred`) holds the owner words' witness markers of
  every slab below it, which a load hands its reader
  (`vspec::atom::load_usize_seen`), and an owner word's invariant
  (`table::OwnerPred`) holds the word's permission and the stored owner
  once the group's markers are persistent, so "handed out before read"
  is checked for `owner` and `check_located`; the fast path's
  `owner_from` claims nothing of the value, which `home_owns` only
  compares. `store_owner` stays trusted (loom-discharged in #138;
  trusted, tool loom/GenMC, in main's ledger) and also
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
  markers), `vspec::atom::load_u16_at` and `load_usize_seen` (loom- and
  C11-discharged in #138, trusted in main's ledger; `u16_value`
  stays the word's value because every store to an owner word is
  `store_owner`'s, which #138's `verus/loom/run.sh` checked by
  name and main's `tests/loom` does not), `axiom_table_lock_inv` (its `seen` and `next_slab`
  conjuncts, as the others, are not checked at the release),
  `axiom_class_lock` (the owned-slab record's location never changes),
  sequential consistency, and one table per class (`axiom_slot_loc`).

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
  `free_fast_tail` (`heap/paths.rs:889`) calls `Shared::home_owns`
  (`heap/shared.rs:3650`) when the `free_flags` byte has `FREE_ROUTE`
  (several arenas), and skips it with one arena. `home_owns` reads the
  owner word through the class line (`ClassLine::owner`,
  `SlabTable::owner_from`, verified, the same `load_owner`) with no
  witness, so it only compares the value. (Before upstream #78 this was
  `checked_slot` through `owner_unchecked`; `checked_slot` is test-only
  on main.)

**Changed since #138.** The statement is unchanged.
`store_owner`, `load_u16_at` and `load_usize_seen`, discharged by loom at
#138, are trusted in main's ledger (`load_u16_at` partly checked by
Kani on one thread), and nothing on main checks by
name that every owner-word store is `store_owner`'s.

## 15. Re-entrant heap registration

Read at this section's commit (the `file:line`s are the `fn` line there).

**Model.** The calling thread's TLS slot is per-thread ghost state: a
`sys::tls::TlsSlot` token (`sys/tls.rs:99`) whose `value()` is what the
slot holds and whose `creates()` counts the `Heap::create` calls made
with it. `get`, `get_tp` (`sys/tls.rs:328`, `:364`, `:383`) take it
shared and return its value; `set` (`:410`) takes it `&mut` and makes
the written value its value. `Heap::create` (`heap/mod.rs:5107`) takes
it `&mut`, leaves the value alone and adds one to `creates`; it is the
only caller of `TlsSlot::note_create` (`sys/tls.rs:161`), the only
writer of `creates`. A function handed only `&TlsSlot` can therefore
neither write the slot nor create a thread heap through that token, and
one handed `&mut TlsSlot` states what it does to both in its `ensures`.
That it does neither at all also needs that nothing it calls makes a
token of its own: `TlsSlot::entry` is `pub(crate)`, and its only callers
are the four top-level users of the slot named in the premises below
(checked by inspection).

The public readers keep their signatures: `heap_for_thread` and
`existing_heap` (`heap/thread.rs:222`, `:382`) make the token at entry
(`TlsSlot::entry`, `sys/tls.rs:299`, value unconstrained until read)
and run the cores `heap_for_thread_in` and `existing_heap_in`
(`heap/thread.rs:245`, `:404`), which carry the contracts.

`ThreadKey::set` (`heap/shared.rs:1006`) is the call that may re-enter:
glibc's `pthread_setspecific` `calloc`s a second-level block of 32 keys
the first time a thread sets a non-null value for a key in it (keys from
index 32 up; the first 32 live in the thread descriptor). Its wrapper
`heap::shared::pthread_setspecific` (`heap/shared.rs:1048`,
`external_body`) models every nested allocator entry as a core holding
the token the outer call lends it: it requires `reentry_benign` of the
slot and ensures the token unchanged, which is what the cores guarantee
at a benign value (below).

**Statement** (`heap/thread.rs:47`, `:245`, `:404`, `:303`;
`heap/shared.rs:1006`):

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
`&mut` on main (it lends the heap through the token's loan,
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
- The slot premise (`sys/tls.rs:99`): a token is not read after
  the slot has changed other than through it. `TlsSlot::entry`
  (`pub(crate)`, verified body, trusted in effect) makes a token with an
  unconstrained value at each top-level user of the slot:
  `heap_for_thread`, `existing_heap`, `heap_destructor`
  (`heap/thread.rs:173`, writes `TLS_EXITED`) and the background thread's
  `create` (`purge/background.rs:507`). An entry nested in another
  stands for the outer token lent to it, which the nested call leaves
  unchanged when the value is benign. Writers of the slot are
  `create_heap_for_thread` (only on an empty slot), `heap_destructor`
  and `create`, all through their tokens. `create` parks a slot above
  the sentinels on `TLS_CREATING` across `pthread_join` and
  `pthread_create` and then restores it. When the slot is empty it
  does not park at first; a nested free in `pthread_join` may create
  this thread's heap through the lent token, and `create` then reads
  its token again and parks that heap across `pthread_create` (since
  upstream #391). A nested allocation in `pthread_create` at a slot
  still empty may create the heap too, and `create` does not read its
  token after that call, so the premise holds. `create`'s contract
  states the result for the slot's value: `TlsCell::kept`
  (`sys/tls.rs:179`), which `create` (`purge/background.rs:507`)
  ensures, as do `pthread_join` and `spawn` (`:483`, `:657`, both
  `external_body`, so assumed there) and the callers up to the slow
  paths that may start the thread, says a non-empty slot ends as it was
  and, since wolfi-dev/challoc#449, that an empty slot ends empty or
  holding a heap, never a sentinel; a `create` that left
  `TLS_CREATING` in the slot fails to verify. It does not say which: a
  `create` that emptied a slot a nested entry filled, losing that heap,
  also satisfies `kept`.
  Since package P7 (READY+HEAPPERM) the token also carries the thread
  pointer (`TlsSlot::tp`) and, while the slot holds a heap, the heap's
  linear permission (`heap::thread::HeapPerm`), which
  `create_heap_for_thread` puts there and the slot readers borrow
  (`heap_from_slot`, through the trusted `slot_heap_ref`); `entry` is
  an axiom returning `&'static mut TlsSlot`, the thread's one slot,
  with `heap::thread::slot_ok`: a heap value comes with the permission
  to the heap it names, well formed (so `api::ready()`), and nothing
  else with one. It is made at the same top-level users and also at
  the `free` entry points' fast paths (`api::libc`: `free`,
  `free_usable`), which read the slot through it. Above the sentinels
  `heap_for_thread_in` and `existing_heap_in` (which now borrows the
  token `&mut`) leave the token as it was but for what its heap holds
  (`TlsSlot::lent`), in place of "unchanged"; at a benign value they
  still leave it exactly as it was.
- `heap::shared::pthread_setspecific` (`external_body`): an FFI call
  whose nested allocator entries reach the slot only through the cores
  (or, at a benign value, `create`, which then writes nothing), holding
  the token it is lent. It replaces the `assume_specification`
  of `libc::pthread_setspecific` (formerly in `vspec/heap.rs`), which
  claimed nothing.
- `sys::tls::get`, `get_tp`, `set` (inline asm, `external_body`): a read
  returns the token's value; a write makes it the token's value (since
  P7: and leaves its thread pointer and heap permission alone; the
  aarch64 `get_tp` returns the token's thread pointer).
- `Heap::create` makes no call that can allocate (above).
- `Heap::destroy` (verified since package P10, theorem 10), on the heapless path:
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

**Changed since #138.** The statement keeps its shape over main's
token types: the slot token is split into the slot's value (`TlsCell`)
and the heap loan (`HeapLoan`, with `slot_ok` tying them), so
`heap_for_thread_in` and `create_heap_for_thread` also ensure the loan
unchanged on the benign values, and `existing_heap_in` takes the
`TlsSlot` `&mut` and lends the heap from it. Explicit caches reach a heap
through `heap_at` with a `HeapUse` token (`axiom_heap_use`,
`heap/tcaches.rs:144`, a new premise: one token per call that reaches an
existing heap). The background thread's `spawn` (trusted) requires an
empty slot or a sentinel, as `pthread_join` does; `create` parks a heap
the join's nested free left in an empty slot before it calls `spawn`
(upstream #391). Stronger: `create` ensures `TlsCell::kept`, which
since wolfi-dev/challoc#449 says an empty slot never ends on a
sentinel; #138's `create` took no slot token and stated nothing about
the slot.

## 16. No allocator lock across external calls (partly premised)

Read at this section's commit (the `file:line`s are the `fn`/`axiom`
line there).

**Statement.** What Verus checks: every call the reports make out of
the allocator goes through `emit`, which requires a lock-order token at
top 0, and a report holds such a token only while no guard it took is
alive. The token is theorem 33's `LockOrder`, so top 0 means the
calling thread holds no allocator lock, as far as the verified lock
order reaches; what stays assumed is the entry premise
(`axiom_entry`, below) and theorem 33's own premises. The reports are
`malloc_stats_print` and `je_malloc_stats_print` (`api/print.rs:1242`,
through `stats_print`, `:1211`), `malloc_info` (`:1351`) and
`malloc_stats` (`:1376`). Their calls out are the caller's `write_cb`,
the `malloc_message` hook through `malloc_write`
(`api/jemalloc.rs:161`; with no hook, `write(2)` to stderr), `fwrite`
on the caller's stream, and `write(2)` to stderr. A callback, a hook or
`fwrite` may allocate (glibc allocates a stream's buffer on its first
write), so a lock held across one deadlocks the allocation.

`emit` is verified (wolfi-dev/challoc#413; wolfi-dev/challoc#440 split
its body). It hands the buffer to one of three trusted functions, which
make the calls out: `call_back` (`api/print.rs:109`, the caller's
callback), `write_file` (`:139`, `fwrite` on the caller's stream) and,
for the `malloc_message` hook, `call_hook` (`api/jemalloc.rs:145`,
called only by the verified `malloc_write`, whose only caller is
`emit`); or, for `malloc_stats` and a `malloc_write` with no hook, to
the verified `sys::write_all` (`sys/mod.rs:1129`, raw `write(2)`, which
allocates nothing). `emit`, the three trusted functions and
`malloc_write` each require the token at top 0:

```rust
#[verifier::external_body]
fn emit(target: &Target, buf: &mut [u8; 1024], len: usize,    // api/print.rs:85
    Tracked(ord): Tracked<&LockOrder>)
    requires crate::sys::may_syscall(), len < 1024, ord.top() == 0,

#[verifier::external_body]
fn call_hook(cb: Hook, buf: &[u8; 512],                       // api/jemalloc.rs:145
    Tracked(ord): Tracked<&LockOrder>)
    requires ord.top() == 0, ...

pub(super) fn malloc_write(s: &[u8], Tracked(ord): Tracked<&LockOrder>)   // api/jemalloc.rs:161
    requires ord.top() == 0, ...
```

Each of the three entry points takes its token from `axiom_entry`
(`lock.rs:448`: `o.top() == 0`, `!o.root()`) once, after
`super::shared()` and right before it makes the report's `Sink`
(`api/print.rs:1227`, `:1367`, `:1384`). The `Sink` (`:159`) keeps the
token from creation to drop (its ghost field `ord`, `:166`, at top 0 by
its invariant). `Sink::flush` (`:199`) is verified and is the only
caller of `emit`; it stores the NUL at `buf[len]` that `emit` requires
and passes the sink's token. Every call of the report
that may take an allocator lock gets a token lent from the sink
(`Report::lend`, `:224`, through `LockOrder::lend`, `lock.rs:356`):
same top, not a root, and borrowing the sink for as long as it lives.
A lock taken with the lent token hands out its guard's token for the
same borrow (`Lock::lock`, `lock.rs:696`), so the sink cannot be
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
inspection; main has no source test for it).

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
`api/mod.rs:1922`; `background::fork_lock`, `purge/background.rs:884`),
which need the root token `axiom_fork_entry` gives, and no report
reaches them.

**Run-time check.** None on main. In #138,
`a_report_callback_may_reenter_the_allocator` and
`a_malloc_info_stream_may_reenter_the_allocator` (`api/print.rs`)
re-entered the allocator from every call out of a text report, a JSON
report and `malloc_info` (through an unbuffered `fopencookie` stream);
they are listed for restoring in wolfi-dev/challoc#397. The source test
`report_lock_premises_hold_in_the_source`, which checked the source side
of the old permit's premises, is obsolete now that Verus checks them.

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
  and a one-line call of the hook, claiming nothing. `emit` is verified
  (wolfi-dev/challoc#413).

**Where the statement is weaker than the code.**
- **The token is only as good as the lock order.** In #138 the
  permit (`Unlocked`, from `axiom_unlocked`) tracked no acquisition, and
  that no lock was held while it was in use rested on a source test and
  Rust's types. On main the token is the verified `LockOrder` every
  acquisition takes, so what remains assumed is theorem 33's premises
  (`axiom_meta_order` and the closure axioms) and the entry premise.
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

**Changed since #138.** Stronger (#390). The `Unlocked` permit
and `axiom_unlocked` are gone: `emit`, `call_back`, `write_file`,
`call_hook` and `malloc_write` require the report's `LockOrder` at top
0, which the sink holds and lends to every call that may lock, so Verus
checks that no report calls out while it holds a lock it took. Also
stronger: #138 trusted all of `emit`; on main `emit` is verified
(wolfi-dev/challoc#440, wolfi-dev/challoc#413) and the trusted part is
the three call-outs, and the NUL a callback or hook reads is checked
(`Sink::flush` stores it; `emit`, `call_back` and `call_hook` require
it). The source test
`report_lock_premises_hold_in_the_source` went with the permit, and the
two run-time re-entry tests are not on main (wolfi-dev/challoc#397).

## Bad input

The B column of the interface contract below; not numbered. The
`file:line`s are the `fn` line at this section's commit.

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
// heap/mod.rs:500, :514 (theorem 3's target; the size hint's class)
pub open spec fn free_target(p: *mut u8) -> bool {
    ||| slot_start(p)
    ||| process_regions().is_huge_block(p@.addr, 1)
}
pub open spec fn hint_ok(p: *mut u8, hint: Option<usize>) -> bool {
    forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) ==> hint == Some(c as usize)
}

// api/libc.rs:927, :985; api/cxx.rs:458, :474; api/jemalloc.rs:673
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

// heap/mallocx.rs:324; api/jemalloc.rs:261, :399, :624
pub open spec fn bad_tcache(t: Tcache) -> bool {
    t matches Tcache::Explicit(i) && i as usize >= MAX_TCACHES
}
pub open spec fn tcache_checked(size: usize, flags: c_int) -> bool {
    let f = mallocx::decode_spec(flags as u32);
    mallocx::bad_tcache(f.tcache) ==> f.arena is Some && spec_class_for(size, 1usize << f.lg_align) is None
}
fn mallocx(size, flags)  ensures tcache_checked(size, flags), /* and theorem 2's fits */
fn dallocx(p, flags)     ensures p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),

// api/cxx.rs:387
fn new_aligned(size, align, nothrow)  ensures !is_pow2(align) ==> r@.addr == 0,

// heap/mod.rs:539; heap/live.rs:1152
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
| The same, through a sized free | sized frees | abort at the cache flush (`double_free`, `straddling_double_free`) | no: excluded by the C contract, *(C)* |
| Double free while the first copy is still in a thread cache | every free | with `double_free_scan` enabled (off by default), abort if among the top four entries (`cached_twice`); else abort at the flush, or the slot is handed out twice first, as under jemalloc | no: excluded by the C contract, *(C)* |
| The same, through `realloc` and the queries | `realloc` family, `malloc_usable_size`, `sallocx`, `xallocx` | no abort at the call: the slot bit is still set, so `check_live` passes. `realloc(p, 1000)` after `p = malloc(16); free(p)` copies from `p` and frees it again, which pushes a second copy (aborts at that push only with `double_free_scan`); a kept block is returned as is | no: excluded by the C contract, *(C)*; the queries take no token |
| Double free of a huge run | every free | abort (`Huge::free_run`: `already_free`, or `not_a_run` once the freed run has been merged or left the table) | no: excluded by the C contract, *(C)* |
| Free of a block that was freed and reallocated (ABA) | every free | frees the new block | undetectable by any allocator |

*(C)*: a pointer that is a slot start or a huge page start but not a live
block is excluded by `heap::live::axiom_c_claim` (`heap/live.rs:913`,
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
- Size hint: `check_size_hint` (`heap/shared.rs:3221`) aborts when
  `Regions::classify` says `Slab { class }` and `hint != Some(class)`.
  `classify` and `slab_slot` (`layout/regions.rs:1120`, `:976`) also
  ensure that with separated regions they find every slot start in its
  class, guard slabs included (`lemma_slot_addr_found`, `:1261`: the
  stride shift gives the class, the offset is inside the region, and
  the slot test of `Class::slot_of_offset` accepts it), so `hint_ok`
  holds whenever `check_size_hint` returns. The fast path
  (`free_sized_fast`, `heap/paths.rs:991`) pushes only when
  `Shared::sized_slot` found `p` a slot of the hinted class, and a slot
  has one class (`lemma_hint_ok`, from `vspec::zero::lemma_slot_unique`).
  `class_index` ensures `r == spec_class_for(n, 1)`, so the hints
  `free_sized` and `sdallocx` (flags 0) pass are `spec_class_for`'s.
  For `Huge { page }`, `check_size_hint` looks the page up
  (`Huge::lookup`, with the caller's run token, which makes the result
  that run's descriptor: `Active`, starting at `page`, its usable bytes
  the token's) and aborts (`huge_hint_mismatch`) unless
  `Huge::usable_for(size)` is the run's usable size, so it ensures
  `huge_hint_ok(blk, p, size)`. `classify` now also ensures that with
  separated regions every page start of the huge region is `Huge`
  (`in_superblock` ensures a hit lies in the strides), so a run is
  never passed over as `Invalid`. The fast path declines a huge pointer
  (`free_sized_fast` ensures a push is a slot start's), and a declined
  push leaves the token's claim and length, so the cores
  (`free_sized_block`, `sdallocx_block`, `sdallocx_flags`,
  `sdallocx_routed`, `free_sized_slow`, `free_sized_unhinted`) ensure
  `huge_hint_ok` of C's token.
- Flags: `Tcaches::get` (`heap/tcaches.rs:288`) ensures `None` for an
  id of `MAX_TCACHES` or more; `mallocx::alloc`, `alloc_huge` (without
  an explicit arena) and `free` (`heap/mallocx.rs:346`, `:472`, `:522`)
  then reach `invalid_tcache`.
- `new_aligned`: `new_aligned_block` tests `align.is_power_of_two()`
  (whose specification is `is_pow2`) before any allocation or handler
  call: a non-power-of-two alignment, 0 included, returns NULL for the
  `nothrow` forms and leaves through `throw_bad_alloc` (`-> !`) for the
  others; `oom` (`api/cxx.rs:256`) is reached only with a power of two.

**Premises beyond the common ones.** None new. The size-hint rows rest
on the class table premises (`class_entry`, `axiom_class_table_wf`)
through `slab_slot`'s completeness; *(C)* is the `free` contract.

**Scope.**
- NULL: `free`, `free_sized`, `dallocx`, `sdallocx` and the `delete`s
  return for NULL (at once, except since upstream #78 `free` and since
  #100 the x86_64 hinted sized frees, after their fast path declines
  it; not stated: there is no model of the
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
  so its `ensures` states `!bad_tcache` only for a move
  (`p@.addr != 0 && r@.addr != 0 && r != p`); a move allocates and frees
  through `alloc_with` and `mallocx::free`, which check it. A kept block,
  and a failed move (NULL), say nothing of the cache id.

**Changed since #138.** Stronger: `rallocx` now ensures
`!bad_tcache` of the flags' cache when it moves the block (there it said
nothing of the cache id).

## 17. Huge coalescing (at the huge lock invariant)

A fact of every value the huge lock admits (`Inner::wf`); nothing is
exported per call, and no entry point's `ensures` mentions it.

Read at this section's commit (the `file:line`s are the `fn`/`spec`
line there).

**Statement** (`huge/mod.rs:3123`, `:1292`):

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
`Huge::audit` (`huge/mod.rs:8366`, the assert in its loop), now a
conjunct of `Inner::wf` (`:2948`), the huge lock's invariant.

**How it is kept.** `wf` gains two conjuncts: `coalesced`, and
`free_marked` (`:1305`), which puts every page of every free extent in
the free-page bitmap's view.
- `Huge::find` (`:4965`) now ensures that it returns fresh pages (no
  extent) only when no free extent ends at the mark (`none_ends_at`). Its
  loop holds that every free extent touching the mark starts at or past
  `from`; `free_marked` puts that start in the bitmap, so
  `lowest_set_from` cannot skip it, and when the scan runs out of set
  bits there is none. `take_ok` (`:2005`) carries the fact into `take`.
- `Huge::take` (`:5213`): `Take::touch_ok` (`:2300`) records where each
  free piece lies: the head ends where the run starts, a tail runs from
  the run's end to the end of the extent it was cut from, and a hole
  runs from the mark, which no free extent touches, to the run.
  `lemma_took_coalesced` (`:2777`) does the case analysis; `took_free`
  states the bitmap after the take: what was set outside the run, plus
  `[mark, run start)`.
- `Huge::free_run` (`:6794`): `free_before` and `free_after` (`:3712`,
  `:3741`) now also ensure completeness: any free extent the table keys
  that ends (starts) at the given page is the result, so the neighbour
  search misses none. `lemma_free_keyed` (`:6554`) shows every free
  extent is still under its keys once the run has left the table, so
  `Merge::nbrs_ok` (`:5772`) holds: without `pred` no free extent ends
  at the run's start, and without `succ` none starts at its end.
  `lemma_freed_coalesced` (`:6024`) concludes for the merged extent.
- `purge_desc` (`:7690`; once `purge_extent`) turns an `Empty` extent `Purged`: still free,
  in the same place.
- `Huge::new` starts with no descriptors.

**Findings.** None. `take`'s alignment holes were the suspected case:
the hole is a fresh `Purged` extent at the old mark, and it could touch
a free extent ending there. It cannot, because `find` returns no extent
only after its scan ran out of free pages, and the scan stops at a free
extent touching the mark, returning it or `None`. Merges of `Empty` and
`Purged` neighbours are coalesced: `free_run` merges both states.

**Premises beyond the common ones.**
- `vspec::huge::axiom_huge_lock_inv` (`vspec/huge.rs:29`): the lock
  protects `Inner::wf`, now including the two new conjuncts (and,
  since, `free_exact`: theorem 29).
- `class::Set::ensure` (`class/sets.rs`) leaves the bitmap view
  unchanged (verified, as is the commit it makes); `alloc_aligned`
  calls it between `find` and `take`. The view's `set_range`,
  `clear_range` and `lowest_set_from` are verified (`bitmap.rs`).

**Where the statement is weaker than the code.**
- It speaks of descriptors, not pages. The converse of `free_marked`
  (a free bit belongs to a free extent), which `audit` also checks, is
  now proved separately (theorem 29).
- It is stated at the lock invariant only. No entry point exports it.
- That `find` returns the lowest fit is theorem 23
  ("Lowest-address huge placement"), built on this one.

**Changed since #138.** The statement is unchanged; its lock
axiom assumes more: `axiom_huge_lock_frame` (`vspec/huge.rs:40`) states
the frame as an equivalence (`==`) where #138 stated an
implication (`==>`).

## 18. Pressure never lengthens decay (at a fixed limit)

A theorem about two pure functions, `band` and `effective_decay_ns`, at
one fixed limit and one configured period. It is not about the stored
policy the purge reads (`Policy`'s atomics, whose round trip is not
proved, below), nor about the purge
being timely: what the decay walks do with the period is out of scope.

Read at this section's commit (the `file:line`s are the `fn` line
there).

**Statement** (`policy.rs:466`):

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

`Band::le` (`policy.rs:329`) is the table's row order `Low` < `Scaled`
< `Immediate`, by position within `Scaled`; `Unlimited` compares only
with itself (for `limit == 0` or `UNLIMITED` both bands are
`Unlimited` and the periods are equal). Band monotonicity alone is
`lemma_band_monotone` (`:407`), and the period's monotonicity in the
band is `lemma_decay_monotone` (`:431`), for any two well-formed bands
in that order.

The spec functions are the code's arithmetic exactly, for every input:
`usage_ppm` (`:224`) ensures `ppm == spec_usage_ppm(limit, current)`,
`band` (`:485`) ensures `b == spec_band(limit, current)` and
`effective_decay_ns` (`:636`) ensures `ns ==
spec_effective_decay_ns(base, band)`. `scaled_ppm` (`:271`) models
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
`severity`), and which `evaluate` (`:1010`, from `purge::on_tick` and the
background thread; since upstream #110 also from the deferred cgroup
discovery, `Shared::discover_cgroup`, and a no-op until the record is
discovered) and `Shared::init` (`heap/shared.rs`; since #110 only for a
record discovered at init, `thp:always`, else the `Unlimited` band)
call through `cgroup_band`; and `effective_decay_ns`, which
`Policy::effective_decay_ns` (`policy.rs:868`) applies to the stored band for
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
  `immediate`, `limited`; `Policy::apply`) and `Policy::band` (`policy.rs:831`)
  rebuilds it: that round trip is not proved. `Policy::position` reads
  `scaled16` inside its atomic invariant (`FracPred`: at most
  `FRAC_ONE + 1`), which bounds the value but does not say which store
  wrote it; that `apply` stores `Scaled(f)` as `f + 1` and `band` reads
  it back as `Scaled(f)` (the 70% edge's `Scaled(0)` included, no longer
  read back as `Low`) is the code, checked by the test
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
  (`policy.rs:1133`) samples the period half of the theorem at run
  time, including limits past `2^43` (restored by
  wolfi-dev/challoc#418).

**Changed since #138.** None. The statement is unchanged, and its
sampled run-time test is on main again (wolfi-dev/challoc#418).

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
// purge.rs: the one place a batch's ranges reach `madvise`; it returns
// whether `madvise` took the range (`false` on `EINVAL`, `mlock`ed pages).
fn discard(addr: usize, len: usize, advice: Advice, Ghost(free): Ghost<spec_fn(int) -> bool>) -> bool
    requires
        frees(free, addr as int, len as int),

// huge/mod.rs:1059
pub open spec fn free_page_byte(base: int, pg: int, np: int, held: Set<int>, b: int) -> bool {
    &&& base <= b
    &&& (b - base) / pg < np
    &&& !held.contains((b - base) / pg)
}

// heap/live.rs:1065, in `impl Live`
pub proof fn theorem_purge_huge(tracked &self, tracked auth: &GhostSetAuth<int>, b: int)
    requires
        crate::heap::regions_ok(),
        self.wf(),
        auth.id() == crate::huge::page_loc(),
        free_page_byte(process_regions().huge_base as int, page_size(),
            (process_regions().huge_len >> process_regions().lg_page) as int, auth@, b),
    ensures
        !(self.addr() <= b < self.addr() + self.len()),

// class/decay.rs:156 (closed; unfolded)
pub closed spec fn idle_byte(t: SlabTable, states: Map<nat, State>, b: int) -> bool {
    let sz = (1usize << class_table()[t.spec_class() as int].lg_slab) as int;
    let s = (b - t.base) / sz;
    &&& t.base <= b
    &&& s < t.spec_nslabs()
    &&& states.contains_key(s as nat)
    &&& states[s as nat] != State::Active
}
```

(Line breaks added.) A `purge::Batch` (`purge.rs:330`) carries a ghost
predicate `free` that its maker fixes (`Batch::new`, `:1138`); `push`
(`:1189`) takes only ranges of bytes `free` admits (`frees`, `:535`),
`Batch::wf` (`:1093`) keeps that of every range held (`range_ok`),
merged ranges included, and `flush` (`:1413`) hands `discard` the ranges
with `free`, and, when `madvise` refuses a merged range (upstream
7a12536a), its first, last and middle pieces, each inside the range
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
  `huge/mod.rs:7442`, `:8090`, `:6794`): `free` is
  `free_page_byte(base, page, npages, S, _)`, where `S` is the set of
  the run-page authority the huge lock's value holds when the batch is
  made (`Inner::wf`: the `Active` descriptors' pages, `active_pages`).
  The walks keep the lock value's authority equal to `S` through every
  push and flush (the huge `Tally::inv`: `c.pgs@@ == held`, kept by
  `Huge::settle` and `purge_desc`), and `push_extent` proves an `Empty`
  extent's pages are not in it: they are apart from every `Active`
  extent. `free_run` hands `purge::discard_one` the freed run's pages
  after the run's token left the authority (`GhostSetAuth::delete`), so
  `S` no longer holds them. `RunToken::theorem_purge_apart`
  (`huge/mod.rs:1024`): a byte `free_page_byte` admits for the
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
- Huge: none beyond the huge lock axioms (`vspec/huge.rs`) and
  `huge::axiom_run_locs` (one huge allocator per process), which
  theorem 5's run tokens already use.
- Class: none beyond the class lock axioms. `page_group_is_idle`
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
  `ClassState::walk_empty` (`class/decay.rs:487`, the abort at `:533`)
  that an empty-set slab's descriptor reads `Empty` with `live() == 0`.
  The missing link is the descriptor's `live` counter, which is not
  modelled. **Unconfirmed** (read from the code and its comments, not
  checked by a proof): `live` counts set bits plus in-flight
  reservations, a claimer reserves under the class lock
  (`ClassState::take`) before it sets bits, and a releaser clears bits
  before it subtracts without the lock
  (`ClassState::dec_live`, `class/mod.rs:4789`; `SlabDesc::sub_live`,
  `slab.rs:500`, verified, which ensures only that the count it read
  was at least `n`), so `live == 0` read under the lock
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
  still takes any range, and `class::axiom_discarded_slots_zero` any
  `Purged` slab whose claim holds a `purge::PurgedZero` witness. That
  witness comes from `purge::purged_zero`, a trusted (`external_body`)
  read of `purge::LAZY_EVER`, a plain `Relaxed` atomic; that the slab
  was discarded with `MADV_DONTNEED` in the hold that set `Purged` and
  is untouched since stays a premise. Discharging it would need
  `Batch::flush` to mint `Zeroed` tokens from an eager discard, the
  class and huge lock values to hold them per `Purged` slab or extent
  (split and joined as extents are carved and coalesced), and a verified
  link from `LAZY_EVER` to the batches that used `MADV_FREE`.
- `MADV_FREE` and `MADV_DONTNEED` are treated alike: the theorem is
  about which bytes are discarded, not what they read afterwards.

**Changed since #138.** The statement is unchanged. `SlabDesc::sub_live`
is verified on main (wolfi-dev/challoc#402): it keeps #138's
`may_abort_pending()` and gains an `ensures` that the count it read was
at least `n`; in #138 it was `external_body`. The `live` counter is
still not modelled, so the class half still stops at the lock state.

## 20. `MALLOCX_TCACHE_NONE` bypasses thread caches

Read at this section's commit (the `file:line`s are the `fn` line
there).

**Model.** A heap's caches are its ghost cache permissions
(`Heap::perms`, a `CachePerms`): the headers it owns, their stacks and
entries, and the slot token under every cached entry
(`CachePerms::tokened`; "Uniqueness: slot tokens" in `verus/TODO.md` at
#138).
`perms` is a private field of `Heap`, so only `heap` code can change a
cache, and only through a `&mut Heap`. A verified function obtains a
heap only from `thread::heap_from_slot` (verified since package P7,
borrowing it from the slot token, `TlsSlot::entry`; through
`heap_for_thread_in` and `existing_heap_in`), `tcaches::heap_at`
(trusted, `external_body`; through `Tcaches::get`, `heap_destructor`
and `Heap::destroy`) or `tcaches::heap_new`
(heap creation, verified since P7 over `HeapPerm::assemble`;
`mallctl::thread_heap` goes through `heap_for_thread`),
and each reference is assumed `Heap::wf` afresh: no ghost state links two
references to the same heap. The theorem is therefore stated per
reference: every heap the direct paths obtain, they leave caching
exactly what it cached.

**Statement** (`heap/mod.rs:3938`, `:3958`; `heap/paths.rs:448`,
`:461`, `:422`, `:435`, `:354`, `:390`):

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
- **allocation**: `mallocx::alloc` (`heap/mallocx.rs:346`) on
  `Tcache::None` calls `existing_heap` (`heap/thread.rs:382`) at most
  twice: once for the home arena when the flags name none
  (`Heap::arena`, `&self`), and once after a successful claim for
  `count_direct_alloc`, whose `ensures` gives `caches_same` of that
  reference. The slot comes from
  `alloc_direct` (`heap/mallocx.rs:442`) and `Shared::alloc_direct`
  (`heap/shared.rs:2889`), which take no heap: `ClassState::claim_tokens`
  takes it out of the class's bitmap words, with its token, and the
  token goes into the caller's `Live` (`Live::from_slot`).
- **deallocation**: `mallocx::free` (`heap/mallocx.rs:522`) on
  `Tcache::None` calls `heap::free_direct` (`heap/paths.rs:1172`):
  `Shared::classify_free` (`heap/shared.rs:2976`), `zero_slot` under
  `zero_on_free`, then `Shared::free_direct` (`:2934`), which hands the
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
with the same tokens: no cache gains or loses a slot. Machine code is
unchanged (the additions are ghost).

The test `api::jemalloc::tests::tcache_none_leaves_every_cache_alone`
(`api/jemalloc.rs:1534`, restored by wolfi-dev/challoc#418) checks the
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

**Changed since #138.** The statement is unchanged. The ghost
`assert(h.caches_same(before))` at the `count_direct_*` call sites is
gone; the `ensures` of the six `Heap` methods still state `caches_same`.
Its run-time test is on main again (wolfi-dev/challoc#418).

## 21. MIB round-trip

Read at this section's commit (the `file:line`s are the `fn` line
there, all in `api/mallctl.rs`).

**Model.** The name tree is the static `ROOT` and the `Node`s it
reaches. `root_nodes()` (`:1166`, uninterpreted) is its top level, and
`root` (`:1173`) returns it; `spec_children` (`:1181`) is what
`Node::children` (`:394`) returns: a directory's children, else none.
One shared recursive spec, `resolve` (`:1203`), says what node a MIB
names: each entry picks a child with `step` (`:1190`), the lone numeric
child of its parent for any entry, else the child at that index, and
the next entry looks among the picked child's children. `walk` (`:1240`)
is the same descent driven by a name's components: `names` (`:1223`)
matches a component under a lone numeric child when it is a nonempty
string of digits whose `decimal` value is the entry, else when the
child at the entry's index is named by it. `split` (`:1275`) says
`comps` are the dotted components of a name: at least one, none holding
a dot, joined with dots back into the name. `level` (`:1285`) is the
sibling list the `j`-th entry picks from, and `entry_named` (`:1316`)
says which entry a component names there.

**Statement** (`:1316`, `:1502`, `:1579`, `:1663`):

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
component. `mallctl` (`:2997`) passes `name_to_mib`'s node with
`&mib[..len]`, the MIB it built; `mallctlbymib` (`:3182`) passes
`mib_to_node`'s node with the MIB it resolved, whatever the caller
supplied (read by the verified `mib_in`). `ctl` (`:2770`) requires that
pairing (`resolve(root_nodes(), mib@) == Some(*node)`, `:2788`), so
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
`lemma_walk_push` (`:1341`) and `lemma_join_push` (`:1376`) extend them by
one component. `mib_to_node` keeps `resolve` of the prefix `mib[..k]`;
`lemma_resolve_split` (`:1478`) continues a resolution among the last
node's children, which also shows that a bad entry leaves the whole MIB
unresolved. `child` (`:1528`) and `child_by_index` (`:1560`) now ensure
`names` and `step` exactly (the former `child` contract was a
disjunction). `lemma_walk_resolves` (`:1300`) turns a walk into a
resolution, `lemma_walk_entries` (`:1325`) reads off each entry, and
`lemma_split_unique` (`:1400`) shows the components are unique: the
first ends at the first dot.

**Premises beyond the common ones.**
- `root` (`external_body`; discharged by Kani on main,
  `api::mallctl::kani_root::root_returns`, as in #138): every call
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
  `every_name_round_trips_through_its_mib` (`:3954`) checks every name
  in the tree (numeric components spelled `0`);
  `names_resolve_to_positional_mibs_and_back` (`:3973`) checks the
  expected MIBs of selected names.
- `mallctlnametomib`, `mallctl` and `mallctlbymib` still have no
  `ensures` tying their return codes to the round trip (interface row
  34; `mallctlnametomib` states only that an error writes nothing); the
  round trip is stated on the cores and required by `ctl`, not stated
  on the C entry points.

**Compatibility difference (documented, not changed).**
`mallctlnametomib(name, mibp, miblenp)` (`:3114`) resolves the whole
name first and then compares its length with the caller's capacity
`*miblenp` (`mib_cap`, `:3052`): when the capacity is smaller it returns
`EINVAL` and writes neither `mibp` nor `*miblenp` (`mib_out`, `:3074`, is
not called). jemalloc 5.3.0's `ctl_lookup` (`src/ctl.c`) loops
`for (i = 0; i < *depthp; i++)` instead: when the capacity runs out
before the name does, it returns 0 with the first `*miblenp` entries
written, leaves `*miblenp` unchanged, and ignores the rest of the name
without checking it. With a capacity of 2, `stats.arenas.0.pactive`
gives `[stats, arenas]` and 0 in jemalloc, `EINVAL` here; a caller that
passes room for the full MIB gets the same MIB from both. Not in the
theorem: the capacity read is trusted and the return code has no
`ensures`.

**Changed since #138.** None. `ctl` requires
`resolve(root_nodes(), mib@) == Some(*node)` (`api/mallctl.rs:2788`),
as in #138, so Verus checks both call sites (wolfi-dev/challoc#424
restored it). The cores' statements and the theorem are unchanged.

## 22. Exact stats (ledger model, at quiescence)

The `file:line`s are the `fn` line at this section's commit.

**Statement.** At a quiescent state, `stats.allocated` (read live by
`collect`, `collect_all`, `mallinfo2` and `malloc_stats`; at the last
`epoch` by `mallctl`) is the usable bytes of the live blocks plus the
class size of every slot in a thread cache. Three layers:

1. **The figures are true sums of what was read** (code, `stats.rs`).
   `collect_arena_with` (`:897`) hands out, through a ghost `pg`, the
   pages its load of the arena's `large` word returned, and ensures
   `arena_exact(r, pg)` (`:1466`): `small_allocated + large_allocated` is
   `Σ_c curregs[c] * size(c)` plus `(pg - large_runs * GUARD_PAGES) *
   page` whenever that true sum fits a `usize`. `collect_into` (`:1130`)
   and `collect_all` (`:1343`) hand out, through a ghost `rd`, the figures
   of each arena they found (in index order, each keyed by its registry
   ID, `ArenaStats::index`, with the IDs ascending: `ids_ascending`, see
   theorem 36) and those pages, and ensure
   `totals_exact` (`:1550`) and `summed` (`:1538`): `totals.allocated`,
   and `allocated`, every bin's `curregs` and `large_runs` of the sum, are
   the true sums of those reads (`reads_allocated`, `:1512`) when they
   fit. `mallinfo2` (`api/glibc.rs:108`) hands out what `collect_all` read
   and summed, and ensures `uordblks` is the sum's `small_allocated`,
   `hblkhd` its `large_allocated` (so `uordblks + hblkhd` is the sum's
   `allocated`) and `hblks` its `large_runs`. Inside each function the ghost outputs are
   the values its loads returned (the body sets them from the loaded
   locals); to a caller they are only values for which the ensures hold
   (see "Scope").
   The figures are added with `fig_add`/`fig_mul`, whose spec is `+`/`*`
   when the result fits; `ArenaStats::add` (`stats.rs:619`) and
   `Totals::add_arena` (`stats.rs:452`) now state each needed sum alone, so an
   unrelated figure's overflow does not hide it. `collect` and `epoch`
   call `collect_into`; `collect_arena` and `malloc_stats` add no
   contract of their own.

2. **The counters track the blocks** (ghost model, `stats/ledger.rs`).
   `Books` (`:50`) has one step per counter update and per slot move:
   `reserve` (`ClassState::take`, `class/mod.rs:4015`: `claimed += n`
   under the class lock), `claim` (one bit won by `finish`, or by
   `claim_prefix` in the same lock hold as the reservation; the slot's
   token leaves its word, so no arena has it out), `move_slot` between
   flight, a cache and a live block (refill push, hand-out, free push,
   flush or direct-free take), `release` (`release_word`: the token back
   in its word, through the arena whose slab it is), `count`
   (`release_owned`'s `released.fetch_add(end)`, `class/mod.rs:5047`),
   `take_run` (`Huge::take`: the run token's pages come from the page
   authority), `note` (`Shared::alloc_huge`'s `note_huge_alloc(r.pages)`),
   `free_run` (`Huge::free_run`) and `unnote` (`Shared::free_huge`'s
   `note_huge_free(run.pages)`), plus `new_arena` (`Arenas::create`) and
   `materialise` (`Arenas::build` of an automatic index); every counter
   step names a built arena, and an unbuilt index keeps zero counters
   (`unbuilt_fresh`; these, `start(auto)` and the new lines are in
   theorem 36; the line numbers here predate
   them). `Books::inv` (`stats/ledger.rs:212`) holds at `Books::start` (`:235`,
   `lemma_start`, `:1103`) and after every step (`lemma_step_*`,
   `:666`-`:1078`), in any interleaving:
   `claimed - released` of each `(arena, class)` is its out slots plus
   the reservations not yet claimed plus the releases not yet counted;
   the out slots of a class are each in exactly one of flight, a cache
   and a live block, and out through one arena; each `large` word is its
   arena's live runs and their pages, less those not yet added, plus
   those retired and not yet subtracted; live runs share no page.
   `note` and `unnote` take any page count within the pending total, not
   the run's own: only the totals matter, and quiescence zeroes them.
   `Books::theorem_quiescent` (`stats/ledger.rs:1202`): when no update is in progress
   (`quiescent`, `:224`), `Σ_arenas (claimed - released)` of a class is
   its cached plus live slots, each arena's `claimed - released` is its
   out slots, each arena's word is its live runs and pages, and the words
   summed are all live runs and pages.

3. **Over the live tokens** (`stats/ledger.rs:1638`):

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
tokens count once each: `heap::live::lemma_all_apart` (`heap/live.rs:803`)
shows two tokens held at once stand for different slots, or for runs
whose pages are apart (`lemma_keys_apart`, `:778`, from the slot tokens'
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

**Huge counters, at the code level** (`huge/mod.rs:1709`,
`theorem_huge_stats`, private to `huge`): in every value the huge lock
admits, `stats.runs` is the number of extents the run authority holds and
`stats.run_pages` the number of pages the page authority holds: one
extent per live `RunToken`, and its pages (`theorem_run_counted`,
`:1755`: a token's extent and pages are among them). These are the
`huge:` counters of `malloc_stats_print`; no quiescence premise is
needed, since they change under the huge lock with the authorities.
`free_run` (`:6794`) now ensures `r.pages == tok.npages()`, and
`Shared::alloc_huge` and `free_huge` assert that the pages they add to
and subtract from the `large` word are the token's
(`heap/shared.rs:3040`, `:3181`).

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
- Arena IDs (a stable registry): the ledger's `k` is the `arenas.count()`
  the scan loaded, and the scan's `get_existing` finds every built
  arena, so its reads name every built arena (`reads_keyed`). Unbuilt
  indices may be skipped: `reads[j]` is the arena whose registry ID is
  `reads[j].index`, not arena `j` (formerly a premise that the scan
  found an arena at every index below the count, false while an
  automatic arena is unbuilt; theorem 36).
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
  is the local the code loaded), not on the contract. Closing it needs the page count of the `large` word in
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

**Changed since #138.** Stronger: `theorem_exact_stats` no longer
requires `reads_ok(reads, pages)`. `mallinfo2` (`api/glibc.rs:108`, verified
since wolfi-dev/challoc#421) states what #138's did and, beyond it, how
`arena` and `fordblks` are computed (the interface contract's
row 31).

## 23. Lowest-address huge placement

`Huge::find`'s contract, under the huge lock invariant: no entry point
exports it ("Where the statement is weaker" below). Built on theorem 17.

Read at this section's commit (the `file:line`s are the `fn`/`spec`
line there). Numbered at merge, and placed last to keep the numbers in
order.

**Statement** (`huge/mod.rs:1356`, `:4323`, `:4965`):

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

(Line breaks and triggers dropped.) `in_free(descs, x, p)` (`:1330`):
`x` is a free (`Empty` or `Purged`) descriptor with page `p`.
`aligned_at(q, align)` (`:4316`): `align` is at most a page, or page
`q`'s address is a multiple of `align`. `need` counts the guard page.

In every value of the huge lock, the start `find` returns for `need`
pages at `align` is the lowest page with the run's pages aligned,
inside the region, and each free: in a free extent or at or past the
high-water mark. `None` means no page fits at all. "Free" is page by
page: a fit may span pages of several free extents and the untouched
suffix; maximal coalescing (`Inner::wf`, "Huge coalescing") is what
reduces that to the one extent `find` looks at.

**How it is proved.**
- `align_page` (`:4471`) now also ensures that no page in
  `[page, r)` is aligned: those pages start in `[a, align_up(a))`,
  below the first multiple of `align` (`lemma_below_align_up`, `:1362`).
- `find`'s loop holds two more facts at `from`: every free extent
  starting below `from` ends by it (`ends_by`, `:1337`), and no fit
  starts below `from`. Each round, `lemma_below` (`:4334`) extends "no
  fit" to below `s`: no page in `[from, a)` is free
  (`Inner::lemma_none_free`, `:3015`: an extent holding one would start
  below `from` and so end by it, or start at a free bit `lowest_set_from`
  would have returned), and none in `[a, s)` is aligned.
- The pass to the next extent: a fit at `q` in `[s, e_end)` starts in
  `e` (extents are apart), so by `Inner::lemma_fit_extent` (`:3037`) it
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
- `vspec::huge::axiom_huge_lock_inv` (`vspec/huge.rs:29`): the lock
  protects `Inner::wf`, with `coalesced`, `free_marked` and (for
  `extent_at`'s abort) `free_exact`.
- `TwoLevelBitmap::lowest_set_from` (`bitmap.rs:2969`) is verified: the
  lowest set bit at or past `from`, or none.

**Where the statement is weaker than the code.**
- It is `find`'s contract under the lock; no entry point exports it.
  `alloc_aligned` (`api/libc.rs:2565`) returns NULL before `find` when the size
  overflows or the run or the alignment exceeds the region, and after
  it when a descriptor, the table or bitmap growth, or the kernel
  commit fails, so "huge `malloc` returns NULL only when nothing fits"
  is not claimed.
- A free bit that starts no free extent would abort in `extent_at`;
  with the converse of `free_marked` now in `wf` ("Huge free bitmap is
  exact"), that abort is proved unreachable: under the lock invariant
  `find` never reaches it.
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
// layout/regions.rs:269: `separated` now carries both bases
&&& forall|c: int| 0 <= c < NCLASSES ==> { ... &&& 0 < self.class_base[c] ... }
&&& 0 < self.huge_base

// layout/regions.rs:1392
pub proof fn lemma_block_nonzero(self, c: int, p: usize, len: int, size: int)
    requires self.wf(), self.separated(),
    ensures
        self.is_slot(c, p) ==> p != 0,
        self.is_huge_block(p, len) ==> p != 0,
        self.user_block(p, size) ==> p != 0,

// heap/mod.rs:563
pub proof fn theorem_no_block_at_zero(p: *mut u8, c: int, size: int)
    requires regions_ok(),
    ensures
        process_regions().is_slot(c, p@.addr) ==> p@.addr != 0,
        process_regions().is_huge_block(p@.addr, 1) ==> p@.addr != 0,
        process_regions().user_block(p@.addr, size) ==> p@.addr != 0,
        free_target(p) ==> p@.addr != 0,

// heap/live.rs:1019, :1038
pub proof fn lemma_nonnull(&self)                                  // Live
    requires regions_ok(), self.wf(),
    ensures self.addr() != 0, 0 < self.len(),
pub proof fn theorem_distinct(tracked &mut self, tracked other: &Live)
    requires regions_ok(), old(self).wf(), other.wf(),
    ensures final(self).addr() != 0, final(self).addr() != other.addr(), /* and theorem_apart's frame */

// vspec/usable.rs:179, :222, :257
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

// api/libc.rs:1044 (usable_size), api/jemalloc.rs:600 (sallocx), :585 (xallocx)
fn usable_size(shared, p, blk)        ensures p@.addr != 0, 0 < r, /* and its U ensures */
fn sallocx(p, flags), xallocx(p, ..)  ensures p@.addr != 0, 0 < r,
// api/libc.rs:1140, :2366, :2388, :2430
fn malloc_usable_size(p), free_usable(p), free_usable_block(p, blk)
                                      ensures p@.addr != 0 ==> 0 < r,
fn free_usable_slow(p, blk)           ensures 0 < r,

// api/cxx.rs:333 (new); :256, :307, :362, :387, :406 (oom, new_slow, new_block,
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
pops (`malloc_block`, `malloc_fast`, `malloc_usable_stored`, `api/libc.rs:320`,
`:431`, `:1838`) prove their non-null results with it, in place of the
former `libc::lemma_slot_nonnull`.

**Proof.** `from_parts` (`layout/regions.rs:548`) proves `0 <
class_base[c]` from `0 < superblock` (`class_base[c]` is at least
`superblock + c * stride`), and requires `0 < huge.0`; `reserve`
(`:2425`) proves `0 < superblock` and `0 < huge.0` from the reserved
mappings' `addr > 0` (`try_reserve`'s `0 < a`, `:2002`), as
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
  cannot have acted on a block there. Since upstream #78 `free` has no
  null test on its fast branch: null never passes the bit test (what
  `set_slot` returns is a slot start, never at 0; since upstream #87
  through the class line, the same ensures) and `free_slow`
  returns for it, so the no-op
  is the slow path's (`free_slow`'s `p@.addr != 0 ==>` ensures). Since
  upstream #100 the hinted sized free on x86_64 has no null test either
  (`free_sized_block`'s is `cfg(not(target_arch = "x86_64"))`): null
  never passes `sized_slot` (what it returns is a slot start of the
  hinted class, `ClassLine::lemma_sized_ok`, never at 0) and
  `free_sized_slow` returns for it on x86_64, whose `ensures` are now
  `p@.addr != 0 ==>` as `free_slow`'s are.

## 25. Realloc follows the class/page keep rule

The `file:line`s are the `fn`/`spec` line at this section's commit.

**Statement** (`api/libc.rs:1217`, `:1432`, `:2257`, `:1613`, `:2182`,
`:1365`, `:1682`; `api/jemalloc.rs:477`):

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

(Abridged: the `ensures` above are added to the existing ones.) In
words: `realloc(p, size)` with `size != 0` returns `p` exactly when `size`
maps to `p`'s class (`class_of(size)` is `p`'s class: `size` lies in
`(size of the class below, size of p's class]`), or when `p` is a huge run
start, `size > MEDIUM_MAX` and `size` rounded up to whole pages is the
run's usable size; otherwise it moves (or fails, returning null). The
earlier `kept` (`api/libc.rs:1196`) remains the forward half: a kept block fits.
`rallocx` adds the alignment: a slot of the class `class_for(size, align)`
picks, or a run start that is a multiple of `align`. The move case is
`r != p` because the new block's token and `p`'s are apart
(`Live::theorem_apart`) and both have usable bytes (`Live::lemma_len_pos`,
`heap/live.rs:236`).

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
`realloc_to_usable_size_keeps` (in #138; not on main) checks the kept cases for every class and
three run sizes. Keeping it would change the run half of the rule (keep a
run whenever `huge_usable(size) == len`, whatever `class_of(size)` says),
an exec change on `realloc`'s path left to the coordinator.

**Proof.** `lemma_keeps_slot` (`api/libc.rs:1228`) and `lemma_keeps_run` (`:1242`)
reduce `keeps` to its slot or run half (one class per slot,
`vspec::zero::lemma_slot_unique`; no slot is a huge page start,
`lemma_slot_not_huge`). The slot half is the test the code makes:
`class_for`'s `spec_class_for` postcondition, and for `realloc_old`'s
`class_of` `lemma_class_of_for` (`:1332`: the class at or above `size`
with every class below smaller is the least serving class at alignment
1, and none serves above `MEDIUM_MAX`). The run half is the code's
`usable_for(size) == Some(old)`: `lemma_usable_for_run` (`:1265`) shows
`usable_for`'s value is `huge_usable(size)` whenever it is defined, and
that when `size` rounded up overflows no run's usable size equals it (a
run and its guard page lie in the huge region, so `old <= usize::MAX -
page`). `old` is `blk.len()` by `Huge::usable_size`'s token
postcondition. The corollary: a slot's usable size is its class's size,
which `lemma_class_size_mono` makes the least serving class; a run's is
whole pages (`run_bytes`), which round up to themselves, and maps to no
class exactly above `MEDIUM_MAX` (`lemma_class_for_one`, `:1313`, finds
a serving class below the last, whose size is `MEDIUM_MAX`).

**Premises beyond the common ones.**
- The C contract `heap::live::axiom_c_claim` (`heap/live.rs:913`): the
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

**Changed since #138.** The statement is unchanged. Its
run-time test is not on main.

## 26. Cache routing

**Model.** A heap's caches are its home headers (`Heap::home_hdrs`, one
per class) and the headers of the nodes on its `others` list
(`Heap::list`, ghost, head first). The ghost `Heap::keys`
(`heap/mod.rs:3226`) gives each node its key, `keys[i]` the `arena`
field of `list[i]`: the arena whose cache the node is (an explicit
`MALLOCX_ARENA` arena, or a home arena `thread.arena` moved away from),
or `FOREIGN` (`u16::MAX`, `heap/mod.rs:161`) for the one flush-only node
that plain frees of other arenas' slots go onto (design 6.3 step 4).
`Heap::list_ok` (`heap/mod.rs:3366`, part of `Heap::wf` and
`Heap::keeps`) now states that the keys are pairwise distinct and none
is the home arena (before, it said only that the node pointers are
distinct); `Heap::keeps` (`:3295`) adds that the keys keep their places
counted from the end, as the nodes do, and that the home arena is
unchanged. A pop is a call of `Heap::pop_top` (`heap/cache.rs:454`), the
one function that takes an entry off a cache stack to hand it out; its
callers are `Heap::pop_fast`, `Heap::pop`, `Heap::pop_compact` and
`Heap::pop_compact_run`. An explicit tcache (`MALLOCX_TCACHE`) is a
heap of its own (`Tcaches::get`), popped through the same functions.

**Statement** (`heap/mod.rs:3463`, `:3474`, `:3568`):

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

// Heap::pop (heap/cache.rs:376), Heap::pop_compact (heap/compact.rs:195),
// Heap::pop_compact_run (:303):
    requires old(self).poppable(class as int, cache),

// Heap::other_caches (heap/others.rs:782):
    ensures
        !create ==> *final(self) == *old(self),
        r@.addr != 0 ==> final(self).keyed(r, arena),
        r@.addr == 0 ==> !final(self).has_key(arena),
// Heap::cache_slot (heap/others.rs:717), Heap::ensure_cache (heap/cache.rs:969):
    ensures r@.addr != 0 ==> final(self).routes(arena, class as int, r),

// Heap::alloc_in (heap/paths.rs:308), Heap::alloc_other (heap/others.rs:603):
    requires arena != FOREIGN,
// Heap::alloc_slow_in (heap/paths.rs:548):
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
- `Heap::pop_fast` (`heap/paths.rs:125`, the `malloc`/`calloc` fast
  path): `cache_ptr(class)`, the home header (`cache_ptr`'s `ensures`,
  `heap/cache.rs:338`). This one is read off its body: `pop_top` takes
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
- `Heap::set_arena` (`heap/mod.rs:4787`): `other_caches(arena, true)`
  returns the node keyed `arena`, the only one; after the header
  exchange (`swap_hdrs`, which keeps `same_shape`, now with the keys and
  the home arena) the node's key becomes the old home, which no node
  had, and the home becomes `arena`, which no other node has. When no
  node could be had, `other_caches` ensures no node is keyed `arena`,
  and `drop_home_caches` now ensures `same_shape`.
- `Heap::try_rehome` (`heap/mod.rs`, since the port of upstream #103:
  a young thread heap moves its home onto the arena of a slot it frees
  on the `FOREIGN` slow path): it moves only when `other_caches(to,
  false)` returned null, so no node is keyed `to` (`other_caches` now
  also ensures `!create ==> *final(self) == *old(self)`: a lookup
  changes nothing). The flushes that follow keep the keys
  (`flush_arena` ensures `same_shape`), the home headers are re-keyed
  through their permissions (`hdr_mut`, `lemma_set`, `same_shape`), and
  the home becomes `to`, which no node has; the old home was no node's
  key either. The move is the one change of the home arena besides
  `set_arena`, so `try_rehome` and its callers (`rehome_on_free`,
  `free_slow_in` with `FOREIGN`, `free_other`, `Heap::free`) ensure
  `Heap::wf` rather than `Heap::keeps`; `free_slow_in` still ensures
  `keeps` for the home arena (`free_slow`, the home push's slow path).
- Creation starts with no keys (`thread::create_heap_for_thread`,
  `Heap::lemma_fresh_wf`).
- `Flags::decode` (`heap/mallocx.rs:267`) now ensures an explicit arena
  is below `0xfff`, so `mallocx::alloc` (`:346`, `requires arena matches
  Some(a) ==> a != FOREIGN`) and `api::jemalloc::alloc_with`
  (`api/jemalloc.rs:275`) discharge `alloc_in`'s requires.

**`FOREIGN` is released whole.** In `Heap::free_slow_in`
(`heap/paths.rs:699`) a full `FOREIGN` cache is flushed by its capacity,
after which `cached_len(cache) == 0` (a proof assertion, from `flush`'s
`ensures`, `len` decreasing by `n`), before the push. `Heap::gc_arena`
(`heap/mod.rs:6758`) ensures it: each header of a `FOREIGN` array is
left as `gc_left(old, p, true)`, which is `cached_len(p) == 0`.

**Explicit non-claim: arena purity.** Routing does not give "every slot
`malloc` returns belongs to the heap's home arena" (nor the explicit
arena's, for `MALLOCX_ARENA`). A sized free (`free_sized`, `sdallocx`,
`free_aligned_sized`: `heap::free_sized_fast`, `heap/paths.rs:991`)
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
  now including the keys.

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
  `Heap::homed_ok` now states for every counted heap);
  `set_arena` requires the new one below `MAX_ARENAS`, which its only
  caller, `mallctl`'s `thread.arena`, checks at run time;
  `try_rehome` moves it only to an automatic arena (`to < auto`, checked
  at run time, so below `MAX_ARENAS`, and `homed_ok` holds after); and
  `Tcaches::create` takes
  any `arena` (its callers pass the thread's home arena or 0).
- Routing names the header a pop takes from, not the arena a refill
  inside the pop draws from: `pop_compact` and `pop_compact_run` refill
  an emptied cache through `shared.arena(self.hdr_ref(cache).arena)`,
  the header's own `arena` field, which `ensure_cache` and (since #103)
  `CacheHeader::rekey` set and
  `swap_hdrs` moves with the header, but which no spec ties to the
  routed key.

## 27. Stats identities

The
`file:line`s are the `fn` line at this section's commit.

**Statement.** Two parts; the first is proved, the second holds by
construction (read from the code, not stated as an `ensures`).

1. **Identities** (proved). Every `ArenaStats` an arena stores, and
   every one `collect_arena`, `collect_into` and `collect_all` compute,
   satisfies `ArenaStats::identities` (`stats.rs:370`):

   ```rust
   small_allocated <= small_active && large_allocated <= large_active
   resident == small_active + large_active + small_dirty + large_dirty
               + huge_dirty + metadata
   retained == small_retained + large_retained + huge_retained
   mapped == resident + retained
   ```

   so `resident >= active + metadata`, `active >= allocated` and
   `mapped == resident + retained` per arena. Every `Snapshot` stored in
   `shared.stats` satisfies `Snapshot::inv` (`:596`): with up to 256
   arenas, `Totals::identities` (`:419`), the same three relations for
   the totals. `collect_into` (`:1130`) and `collect_all` (`:1343`) also
   ensure the identities, and `mapped` equal to the arenas' `mapped`
   summed, whenever that sum fits a `usize` (`sum_mapped`, `:1079`),
   for any number of arenas. The contracts:

   ```rust
   // Arena::stats (arena.rs:1233), stats::arena_stats (stats.rs:1292)
   ensures r.identities()                 // r matches Some(a) ==> a.identities()
   // Arena::store_stats (arena.rs:1250)
   requires s.identities()
   // stats::snapshot (:1272), stats::collect (:1257)
   ensures r.inv()                        // narenas <= 256 ==> totals.identities()
   // collect_arena_with (:897)
   ensures r.identities(), r.mapped <= arena_mapped_max(r.index), r.index == arena.spec_index()
   // collect_into (:1130); collect_all (:1343) likewise for its sum
   ensures r.narenas == rd.0.len(), forall j: rd.0[j].identities(),
       sum_mapped(rd.0) <= usize::MAX ==> r.totals.mapped == sum_mapped(rd.0) && r.totals.identities(),
       r.inv()
   ```

2. **Coherence** (by construction, since upstream #91 and #92). After
   an `epoch`, the stored per-arena figures sum to the stored totals
   (they come from one collection), and `mallctl("epoch")` returns the
   number its own refresh stored.
   - Arenas and totals (by construction, since upstream #91):
     `collect_into` takes the process statistics lock
     (`shared.stats.lock()`) first and holds it across its whole scan,
     storing each arena's figures (`arena.store_stats(&a)`, under that
     arena's statistics lock) and then the totals under the one hold.
     It sums into the totals exactly the figures it stores for each
     arena (the same local `a`, read once), and `Arena::store_stats` has
     no other caller, so the arenas' stored figures change only under
     the process statistics lock: two `epoch`s cannot interleave their
     stores, and a reader holding that lock (`snapshot`, and
     `arena_stats_all`, which takes it across its sum) sees one whole
     collection. The test
     `stats::tests::concurrent_epochs_leave_arenas_summing_to_the_totals`
     (16 arenas, one thread allocating and freeing, two epoch writers
     per round, 2000 rounds) failed within 20 rounds before the fix and
     passes now. `arena_stats` (one arena's figures) does not take the
     process statistics lock, so a reader of `stats.arenas.<i>.*`
     beside `stats.*` during an `epoch` may still see one arena of the
     new collection beside the old totals. The lock order this needs
     (the process statistics lock ranked below the class locks) is
     theorem 33's.
   - The epoch number (by construction, since upstream #92): the
     `Epoch` arm of `mallctl::ctl` reads back, on a write, the number
     `stats::epoch` returned, which is the epoch number its own call
     stored (`collect_into` stores and returns one `Snapshot` under one
     hold of the snapshot lock), and on a read alone
     `stats::snapshot(shared).epoch`. Before the fix it discarded
     `stats::epoch`'s number and re-read the snapshot, which another
     writer may have bumped in between; the test
     `api::mallctl::tests::concurrent_epoch_writes_read_back_their_own_numbers`
     (four threads, 2000 epoch writes each, every number read back
     distinct) read the same number back twice on every run (7988 to
     7999 distinct of 8000) and passes now. jemalloc holds `ctl_mtx`
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
`stats.arenas.4096.*` (`arena_stats_all`, `stats.rs:1306`) sums stored figures
that each satisfy the identities, but carries no bound on how many it
sums or ghost record of them, so its result has no identity contract.

**Why no figure wraps.** The figures are added with `fig_add`/`fig_mul`,
exact only when the result fits (theorem 22). Each sum is bounded:
- per bin, `collect_bin` (`stats.rs:805`) ensures `BinStats::fits` (`stats.rs:213`):
  `curregs * size <= curslabs * slab_size`, from the clamp
  (`ClassState::bin_stats`, `class/mod.rs:5476`, now ensures
  `curregs <= curslabs * nslots`) and `nslots * size <= slab_size`
  (`Class::wf`); and `(curslabs + empty + purged) * slab_size <= 2^38`,
  the slabs being among the class region's
  (`ClassState::nslabs_are_the_region_s`, `lg_region <= 38`);
- over at most 64 classes, the slab bytes are at most `2^44`;
- a huge run count and page count come from the halves of the `large`
  word, below `2^32` (`Arena::large`, `arena.rs:1216`, now ensures it),
  and a page is at most `2^22` (`lg_page <= 22`,
  `Regions::separated`), so an arena's
  huge bytes are below `2^54`;
- arena 0 adds the free extents' pages, at most `u32::MAX`
  (`Huge::stats`, `huge/mod.rs:8327`, now ensures it from the huge lock
  invariant), and the committed metadata, at most the region's length,
  below `meta::va_limit() = 2^57` (`Meta::stats`, `meta.rs:1042`, now
  ensures it).
So arena 0's `mapped` is below `2^58` and any other's below `2^55`
(`arena_mapped_max`, `stats.rs:385`); every per-arena sum fits. The scan visits
arena `i` at index `i` (`arena_at`, `:879`, now ensures it), so arena 0
is counted once and `k` arenas sum below `2^58 + k * 2^55`, which fits
for `k <= 256`.

**Premises beyond the common ones.**
- The two `lock_inv` axioms (`axiom_arena_stats_lock_inv`,
  `vspec/arena.rs:54`, and `axiom_snapshot_lock_inv`, `stats.rs:605`)
  now say the stored value satisfies `ArenaStats::identities` and
  `Snapshot::inv` (they said "any value" before). Their writers:
  `Arena` initialisation (`ArenaStats::empty`, which ensures the
  identities) and `Arena::store_stats` (which requires them);
  `Shared::init` (`Snapshot::EMPTY`, written in `Shared::put_tail`,
  `heap/shared.rs:1961`) and
  `collect_into` (which asserts it before releasing the guard). The
  fork handlers take the raw locks only and write nothing. As for every
  lock but the metadata lock (whose holders end with
  `LockGuard::unlock`, wolfi-dev/challoc#396), that each holder leaves
  the invariant true is checked at the holder's last write, not at the
  guard's drop (`lock.rs`, `lock_inv`).
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
- Coherence (part 2) is read from the code, not proved: no `ensures`
  ties the figures stored under the arenas' statistics locks to the
  totals stored under the process statistics lock, and a reader of one
  arena's `stats.arenas.<i>.*` (`arena_stats`, which does not take the
  process statistics lock) may see it from a newer collection than the
  `stats.*` totals it read before.

**Changed since #138.** The statement is unchanged; the lock-frame
axioms it rests on (`axiom_arenas_lock_frame`, `axiom_huge_lock_frame`,
`axiom_meta_lock_frame`) state the frame as an equivalence (`==`) where
#138 stated an implication (`==>`), so they assume more.
`mallinfo2` gains the `arena >= uordblks` and `fordblks` identities
(wolfi-dev/challoc#421).

## 28. Cache flush completeness

The
`file:line`s are the `fn` line at this section's commit.

**Statement.** Every flush of a whole heap empties every cache the heap
holds: after `Heap::flush_all` each header of the home array and of
every node on the heap's `others` list has `len == 0`, so the mallctl
flush operations strand no cached slot in a live heap. Turning
`thread.tcache.enabled` off also frees every one of those caches (no
stack left), and a cache GC step leaves each cache one of two stated
lengths (`gc_arena` states the exact one per array).

```rust
// heap/mod.rs:6441 (theorem 10's contract, unchanged; `tcache_on` added)
pub fn flush_all(&mut self, shared: &Shared)
    ensures
        final(self).keeps(*old(self)),
        final(self).same_shape(*old(self)),
        final(self).tcache_on() == old(self).tcache_on(),
        final(self).drained(),
        log_adds(final(self).released(), old(self).released(), old(self).held_all()),

// heap/mod.rs:4616
pub fn set_tcache_enabled(&mut self, shared: &Shared, on: bool)
    ensures
        final(self).wf(),
        final(self).tcache_on() == on,
        on ==> final(self).caches_same(*old(self)),
        !on ==> final(self).same_shape(*old(self)),
        !on ==> final(self).cleared(),
        !on ==> final(self).drained(),
        !on ==> log_adds(final(self).released(), old(self).released(), old(self).held_all()),

// heap/mod.rs:6599, :6758
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

// heap/cache.rs:969
pub(super) fn ensure_cache(&mut self, shared: &Shared, arena: u16, class: usize) -> (r: *mut CacheHeader)
    ensures
        ...,
        !shared.config.tcache || !old(self).tcache_on() ==> r@.addr == 0 && *final(self) == *old(self),
```

with (`heap/mod.rs:3798`-`:3813`; abridged)

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
  (`ThreadTcacheFlush`, `api/mallctl.rs:2919`, through `flush_caller`,
  `:2700`, which also serves `arena.<i>.purge` and `arena.<i>.destroy`,
  `:2858`, `:2882`), `tcache.flush` (`TcacheFlush`, `:2962`, through
  `Tcaches::flush`, `heap/tcaches.rs:314`), `malloc_trim`
  (`api/libc.rs:2884`), and `Heap::destroy` (theorem 10).
  `Tcaches::flush` restates `drained()` after the call as a ghost
  `assert` (`flush_caller` did in #138; on main it relies on
  `flush_all`'s `ensures` alone); `Tcaches::flush` also ensures `false` for an id of
  `MAX_TCACHES` or more.
- **Caches off.** `set_tcache_enabled(false)` (`thread.tcache.enabled`
  written false, `ThreadTcacheEnabled`, `api/mallctl.rs:2924`) runs `flush_all`, then
  `free_caches_in` (`heap/mod.rs:6156`) over the home array and every
  node, which now requires the array flushed (so its `assert_flushed`
  abort is proved unreachable there) and ensures each of the array's
  headers has no stack and no other header changed. So afterwards every
  cache is freed (`cleared`), hence empty (`drained`), the released log
  holds what they held, and `tcache_on()` is false. Writing it true
  changes no cache (`caches_same`).
- **Caches never come back while off.** `ensure_cache`, the only place a
  header gets a stack (`lemma_fresh_stack`, `axiom_fresh_stack` in #138, `heap/cache.rs`, by
  inspection), returns null and leaves the heap unchanged when
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

Machine code is unchanged (x86_64 disassembly identical to the base
commit; the additions are ghost, and the two trusted mallctl wrappers
below were `#[inline(always)]` calls).

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
  read from the code (one `lemma_fresh_stack` call; `axiom_fresh_stack` in #138); `set_arena` only
  exchanges headers between the home array and a node
  (`Heap::swap_hdrs`).

**Scope.** The mallctl keys `thread.tcache.flush`, `tcache.flush`,
`thread.tcache.enabled`, `arena.<i>.purge`, `arena.<i>.destroy`, and
`malloc_trim`, on the calling thread's heap or the named explicit
cache; the configuration option `tcache:false`; the periodic GC. The
mallctl call sites of `Heap::set_tcache_enabled` and `Tcaches::flush`
were trusted wrappers claiming nothing (`heap_set_tcache_enabled`,
`tcaches_flush`, `api/mallctl.rs`); they are gone, so the calls are
checked against the callees' preconditions (`thread_heap` now ensures
its heap `wf`), which makes both keys' contracts the callees'.

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

**Changed since #138.** The statement is unchanged; a header's
fresh stack is proved (`CachePerms::lemma_fresh_stack`) where
#138 assumed it (`axiom_fresh_stack`). Stronger: `gc_all` states
each node's case by its key (#138: emptied or trimmed).
`flush_caller` no longer restates `drained()` as an assertion; the
result is still `flush_all`'s `ensures`.

## 29. Huge free bitmap is exact (at the huge lock invariant)

A fact of every value the huge lock admits (`Inner::wf`), like theorem
17; no entry point's `ensures` mentions it. Built on theorems 17 and 23.

Read at this section's commit (the `file:line`s are the `fn`/`spec`
line there). Numbered at merge.

**Statement** (`huge/mod.rs:2996`, `:1315`):

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
bit set. `free_marked` (theorem 17) was the
`<==` half; `free_exact` (`:1315`), the `==>` half, is a new conjunct of
`Inner::wf` (`:2948`). `Huge::audit` checks the same at run time ("page
of run marked free", "page of free extent not marked free", and
`lowest_set_from(next_page)` is `None`); as extents cover every page
below the mark (the page tallies sum to it), the three checks are this
statement.

**Consequence: `extent_at`'s abort is unreachable.** `Inner::extent_at`
(`:3683`), which `find` calls at every free bit its scan stops at, now
requires the bit set and every free extent starting below the page
ending by it (`ends_by`), and proves its "huge: free page {page} has no
extent" abort branch unreachable (`assert(false)` there, ghost): the
bit's page is in a free extent, which cannot start below the page, so
starts at it, and the table keys every extent's start (`keyed`).
`find` (`:4965`) establishes `ends_by(a)` at the lowest set bit `a` at or
past `from` from its loop's `ends_by(from)`: a free extent starting in
`[from, a)` would put its first page's bit there. This closes theorem
23's gap: under the lock invariant `find` never reaches that abort.

**How it is kept.**
- `Huge::new` (`:3801`): `class::Set::new` (`class/sets.rs:794`) now
  ensures the fresh bitmap's view is empty (from `lemma_fresh`, already
  called), and there are no descriptors.
- `Huge::take` (`huge/mod.rs:5213`): `took_free` (`:2766`) is now an equivalence:
  after a take the set is exactly the old set outside the run plus the
  alignment hole `[mark, run start)`. Its new direction needs no old bit
  in the run's pages outside the cleared range `[max(s, a), min(end,
  e_end))`: those pages are past `ext`, which then touches the mark
  (`take_ok`), or, with no `ext`, at or past the mark; and every old bit
  is below the mark (`lemma_free_below`,
  `:3060`, from `free_exact`). `Take::touch_ok` (`:2300`) gains three
  coverage facts (a head is kept when `ext` starts below the run, a tail
  when it ends past it, a hole whenever the run starts past the mark), and
  `lemma_took_coalesced` (`:2777`) concludes `free_exact`: an old bit
  outside the run is in an untouched free extent or in `ext`'s head or
  tail, and a hole bit in the hole or in the head that absorbed it.
- `Huge::free_run` (`:6794`): `freed_free` (`:6013`) is now an
  equivalence (the set gains exactly the run's pages, `set_range`), and
  `lemma_freed_coalesced` (`:6024`) concludes `free_exact`: an old bit's
  extent is `pred`, `succ` (both now the merged survivor) or untouched.
- `purge_desc` (`:7690`; once `purge_extent`) only turns an `Empty` extent `Purged`, which
  stays free in the same place.

**Findings.** None: the code already keeps the bitmap exact. The
invariant used to admit an extra free bit on an `Active` page or past
the mark, under which `find` could reach `extent_at`'s abort without
breaking its contract; no path sets such a bit.

**Premises beyond the common ones.**
- `vspec::huge::axiom_huge_lock_inv` (`vspec/huge.rs`): the lock
  protects `Inner::wf`, now including `free_exact`.
- `class::Set::ensure` (verified) leaves the bitmap view unchanged, as
  in theorem 17. The view's `set_range`, `clear_range`,
  `lowest_set_from` and `lemma_fresh` are verified (`bitmap.rs`).

**Where the statement is weaker than the code.**
- It is stated at the lock invariant only; no entry point exports it,
  and nothing per call beyond `find`'s abort-freedom uses it.
- It speaks of the bitmap's view (`TwoLevelBitmap::view`), whose tie to
  the words in memory is the bitmap module's verified model; the second
  level's summary bits are part of that model, not of this statement.

**Changed since #138.** The statement is unchanged. Its table storage
comes from `huge::table::Table::storage`, verified as in #138, over the
Kani-discharged `axiom_zero_entry` and a `ZeroRaw::axiom_new` pairing for
a binned table (theorem 4).

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

The `file:line`s are the `fn` line at this section's commit.

**Statement** (`purge.rs:1944`, over the spec of `decay` at `:1903`):

```rust
// purge.rs:1813
pub fn decay(&self, now: u64, decay_ns: u64, walk: impl FnOnce(u64, &Curve)) -> (r: Option<u64>)
    ensures
        r matches Some(t) ==> t >= now,
        exists|oldest: u64, retry: u64, next: u64, after: u64, retry2: u64|
            r == #[trigger] Decayer::spec_decay(now, decay_ns, oldest, retry, next, after, retry2),

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
after it (upstream 7a12536a); a load the call does
not make is a free variable of the existential. `spec_step(d)` is
`d / STEPS`, `Curve::step_ns`'s exact spec (`purge/curve.rs:314`) for
`Curve::new(d)`. `spec_walks` (`purge.rs:1889`) is the walk's condition:
something `Empty`, its first step over, and `next` not within one step
after `now`. `spec_decay` (`:1903`) is the code's branches with
`saturating_add` spelled as `sat_add` (`:1874`); it is `closed` (only
`purge.rs` sees its body), so callers reason through the theorem.

In words: `decay` returns `None` exactly when the oldest-`Empty` load is
0 or a walk ran and the load after it reads 0. A deadline is at most
`max(oldest, now) + step`, so, with the oldest member stamped no later
than `now`, at most one step past `now`. That is the period-change case
of `decay` (`next - now <= step`): after `set_decay_ms` (`arena.rs`) or
a pressure band (theorem 18) lowers the period, the next call no longer
returns a deadline computed with the old step: it returns the end of the
oldest member's first (new) step, or walks and returns one new step from
`now`. A deadline equal to `now` (the background thread then sleeps
`MIN_SLEEP_NS`) needs a zero step (`decay_ns < STEPS`) or saturation,
and only follows a walk.

Since upstream 7a12536a a walk whose discard `madvise` refused (`mlock`ed
memory) calls `Decayer::back_off(now)`, which stores `now +
DISCARD_RETRY_NS` (one second) in `retry_at`: no walk runs before it
(`spec_walks` gains `now >= retry`), a call during it returns it, and a
walk's deadline is the later of the next step and the back-off. A
back-off deadline is the one deadline the step bound does not cover:
the theorem names it (`t == retry || t == retry2`), a value `retry_at`
held, and nothing bounds it against `now` beyond `now <= t` (it is a
stamp `back_off` took from the walk's clock, one second ahead).

With theorem 18 (`purge.rs:1979`):

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
`Policy::effective_decay_ns` (`policy.rs:868`), the period
`purge::decay` and `purge::decay_arena` pass to every gate, ensures
`ns <= base`: the stored
band never lengthens a configured step either.

**Covers.** Both gates: `ClassState::decay_with` (`class/decay.rs:1694`)
and `Huge::decay_with` (`huge/mod.rs:8274`) ensure `decay`'s contract
for the `decay_ns` they are given. Since upstream #97 `mallctl
arena.<i>.decay` (`purge::decay_arena`) gives them the periods
`purge::decay` does, through the same `decay_one` and `decay_huge`
(the policy-scaled `effective_decay_ns`, and for the huge extents the
huge tier's own period); before, it passed the unscaled periods, and
arena 0's period rather than the huge tier's to the huge walk. `Arena::decay_with`
and `purge::decay` (`purge.rs:2147`, through `earliest`) take the minimum
of the per-gate deadlines; the background thread (`purge/background.rs:801`)
sleeps until `purge::decay`'s result, and `on_tick` keeps only whether it
is `Some` (since upstream #111: dirty memory still waiting, which runs
the lazy start's look, `background::auto_tick`). No C
symbol's `ensures` changes.

**Premises beyond the common ones.** None for the deadline: no axiom,
`external_body` or `assume_specification` is used by the theorem; the
gates' walk closures take the lock-order premises
`axiom_below_class`/`axiom_below_huge` (theorem 33). The loads are `core` atomics,
which vstd specifies with any value, which is why the statement is over
the values seen. The walk closure's `ensures` are not used.

**Scope and where the statement is weaker than the code.**
- What callers can use. The witnesses are existential, so a caller
  cannot discharge `oldest <= now`: what `decay`'s and `decay_with`'s
  `ensures` give a caller is `t >= now`, and (through the theorem)
  `t == now` only when `decay_ns < STEPS` or `now == u64::MAX`. The
  one-step bound states what the body does with what it loaded; tying
  `oldest` to a clock reading no later than `now` needs the stamps and
  the coarse clock as ghost state.
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
  and the guard are in `decay`'s body, but no ghost history of walks is
  kept, so "at most once per step" is not an `ensures`.
- The `NEVER` period: callers skip it (`Arena::decay_with`, the huge
  branch of `purge::decay`, `mallctl arena.0.decay`'s huge call);
  `decay` itself would return deadlines up to
  `sat_add(max(oldest, now), NEVER / STEPS)`, within the bound.
- The period is the one passed in: that it is the arena's current
  `decay_ns` after `set_decay_ms` is the caller's load (any value, as
  above), and theorem 18's link from usage to period is for the pure
  functions only (theorem 18's own scope, including the `Policy` atomics
  round trip).

## 31. Huge retain cap (at the free's lock release)

Stated at the huge lock's release inside `Huge::free_run`, over the cap
and flag values the free read: no `ensures` exports it (below). On main
since wolfi-dev/challoc#416 (`25e8e4f6`).

**Statement** (`huge/mod.rs:6682`, `:6716`; proved by
`lemma_retain_cap`, `:6730`, called in `free_run`, `:6794`, at `:7245`,
before `drop(g)`):

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

// purge.rs:2489, :2506 (closed): the kernel's answer to some discard advice.
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
itself (`huge/mod.rs:6910`-`:6913`): `cap` is the `retain_max()` it
read, `over` is `usable > cap`, `pof` is `!over && self.purge_on_free()`
(the flag's value when it was read, at or below the cap, and `false`
when it was not), and `purge` is `over || pof`. `now` is its clock
reading (`now_ns().max(1)`), and `discarded` whether the run was
discarded: the ghost `st == Purged` (`:6935`), which `free_run` sets
only when `purge` holds and `purge::discard_one` of the run's whole
range, `[addr, addr + n * page_size)`, returns true (`:6929`; upstream
7a12536a: a discard `madvise` refuses, on `mlock`ed pages, leaves the
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
(`huge/mod.rs:90`).

What is proved and what is not:
- Proved: the first conjunct checks `free_run`'s cap comparison against
  the values it bound (a discard needs `over || pof`), and the last
  states the extent's state, age and place, per outcome of the discard,
  from `free_run`'s merge (`Merge`, theorem 17).
- Proved since wolfi-dev/challoc#450: the second and third conjuncts
  give the kernel's answer. A discarded run's range was answered with
  success by some `MADV_DONTNEED` or `MADV_FREE` `madvise` of exactly
  that range (`discard_taken`); a run with `usable > cap || pof` that is
  not discarded lies in a range some such `madvise` answered with an
  error (`discard_refused`). So a run above the cap is discarded unless
  the kernel refused a discard advice on its bytes. `discard_one`
  (`purge.rs:1707`) ensures `r ==> discard_taken` and
  `!r ==> discard_refused` of its range, verified from the `One`
  settler's invariant (the pushed range pending alone, or settled with
  `discarded` the kernel's answer), `Settle::settle`'s `requires` on
  `done`, `discard` (`purge.rs:2568`) and `sys::madvise`
  (`sys/mod.rs:718`), whose `ensures` names the word `raw::syscall3`
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
  `purge_on_free` (`huge/mod.rs:4219`, `:4229`) are `Relaxed` loads of
  std atomics, whose values vstd leaves unconstrained, so the theorem
  is over the values read. By inspection, `set_retain_max` and
  `set_purge_on_free` are called only in `Shared::init`
  (`heap/shared.rs:2274`, `:2277`), on the `Huge` before `put_huge`
  publishes it, and by tests (no mallctl or other path calls them;
  `opt.huge_retain_max` reads the config), so every free reads
  `huge_retain_max` and `huge_dirty_decay_ms == Some(0)` as configured.
  `purge_on_free` is set only for an explicit `huge_dirty_decay_ms:0`;
  with it unset and `dirty_decay_ms:0`, the flag is false and a run at
  or below the cap waits for the (zero-period) decay walk.
- The kernel's answer: `madvise_ok` and `madvise_err`
  (`sys/mod.rs:701`, `:708`) read the word `raw::syscall3` returned
  (`raw::returned3`, which only `syscall3`'s trusted `asm!` body
  ensures: `sys/raw.rs:82` on x86_64, `:215` on aarch64; partly checked
  by Isla on both, up to the system call, not the kernel's side). The
  discard step itself is verified (above).
- `madvise` does what it is asked when it succeeds (the kernel
  contract, a common premise); `EINVAL` leaves the range in place and
  is reported, any other failure aborts in `discard`.
- The huge lock axioms (`vspec/huge.rs`), as for theorems 17 and 19.

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
  (`huge/tests.rs:498`, `:561`) pin both outcomes, the discarded byte
  counts and the ages (the second waits for the clock to pass the
  neighbour's age first).
- At the lock release, not at `free`'s return: after `drop(g)` another
  thread may take the extent again, so no `ensures` can state the
  extent's state at return. The discard does happen before the free
  returns (it is inside the lock). `free_run`'s signature is unchanged;
  exporting the statement needs a ghost result, which the callers
  (`free_huge` and the tests) would have to take.
- Over the values read. A concurrent `set_retain_max` (tests only) or a
  run-time change of the configured values is outside the premise
  above. The `usable` compared is the descriptor's usable bytes (whole
  pages, guard excluded), not the size the caller asked for: a run of
  `cap + 1` requested bytes has `usable > cap` and is discarded; one
  whose request rounds up to exactly `cap` is retained.
- `now` is the coarse clock as read; nothing orders it against the
  neighbours' ages, and the survivor between two `Empty` neighbours is
  chosen by the code's `<` on their stored stamps, not by a clock fact.

**Changed since #138.** Stronger on main: `free_run` binds the
flag read itself (`over`, `pof`, `purge`), so the first conjunct checks
the cap comparison. In #138 the condition was
`usable > cap || self.purge_on_free()` and `pof` was derived from it
(`purge && usable <= cap`), which made the first conjunct a tautology
and left "`pof` is the flag's value" read from the code. Also stronger
since wolfi-dev/challoc#450: `discard_one` and `discard` ensure what
their result says of the kernel's answer, and `retain_cap` states it,
so a run above the cap is discarded unless a discard advice on its
bytes was refused; in #138 `discard_one` had no `ensures` and that was
read from its body. The extent conjunct is the same.

## 32. Read-only APIs never abort

The `file:line`s are the `fn` line at this section's commit.

**Statement** (`sys/mod.rs:139`, `:148`, `:160`, `:164`;
`api/mod.rs:1126`, `api/mallctl.rs:3276`, `:3367`):

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
`sys::abort_integrity` (`sys/abort.rs:166`). The first witness holds
only `integrity_aborts()`, which nothing grants, the other two hold no
abort capability at all; all hold `may_syscall()` (the read-only paths
take locks) and a well-formed `Shared` (the readers require the
`ReadyMark` the witnesses take): they make the calls below and verify. So these
calls abort at most at an integrity check, whatever their arguments
(for the `mallctl*` pointers, whatever arguments keep the C contract,
as the premises below say), and only `mallinfo`/`mallinfo2` reach one:

- `get_defrag_hint(p)` (`api/jemalloc.rs:1016`), whole, for every `p`
  and before init. It requires no abort capability at all (only
  `may_syscall()`): it never aborts.
- `malloc_usable_size(NULL)` (`api/libc.rs:1140`, `requires may_abort()
  || p@.addr == 0`): returns 0 and never aborts.
- `nallocx` (`api/jemalloc.rs:1225`) after its `shared()`:
  `Flags::decode` and `usable_for` (`:232`); never aborts.
- `mallinfo2` and `mallinfo` (`api/glibc.rs:108`, `:166`) after
  `shared()`: their bodies call `stats::collect_all`, `page_size`,
  `fig_add`, `fig_mul` and `saturating_sub`, and `mallinfo` clamps; the
  witness makes the calls (both bodies are verified on main since
  wolfi-dev/challoc#421, and require `may_abort()` for their
  `shared()`; `api::restated::read_only_restatements_match_their_bodies`
  checks that the witness and `mallinfo2`'s body make the same calls). Only the integrity check below.
- `challoc_features` (`api/mod.rs:1090`) after `shared()`: since
  upstream #110 the feature line's build at the first request
  (`api::line_ready`: the per-size THP probe and `FeatureLine::build`,
  through `build_line`, verified since ONCECELL, which requires only
  `may_syscall()`) or the wait for another thread's (termination not
  claimed), then the static line; never aborts.
- `mallctl` and `mallctlbymib` after their `super::shared()`: `cstr`,
  `name_to_mib`, `mib_in`, `mib_to_node` and `ctl` (`api/mallctl.rs:2770`)
  for every node where `ctl_never_aborts` (`:349`) holds: an interior
  node, a read whose reader is not one of `thread.*`'s four
  (`Reader::aborts`): every key under `opt`, `arenas` and `stats`,
  `version` and `config.*`; or `experimental.utilization.query`
  and `batch_query` (`utilization`, `:2250`, `batch_query`, `:2506`).
  `ctl` requires `may_abort() || ctl_never_aborts(*node, mib@)`. Every
  reader is a named verified function (`opt_value` and the 51 others,
  formerly closures outside `verus!`), each called by the witness for
  the readers that do not abort; `call`, which runs the reader of the
  node the MIB resolves to, is a verified `match` over the `Reader` enum
  and requires `may_abort()` or `!read.aborts()`. Every such read but
  both utilization queries never aborts, `stats.arenas.<i>.*` with `i`
  = `MALLCTL_ARENAS_ALL` (`stats::arena_stats_all`) included: the
  witness requires no `integrity_aborts()`. The seven `stats.cgroup.*`
  reads behind `mallctl::tracked` (`limit`, `high`, `current`,
  `decay_ms`, `retention_cap`, `empty_bytes`, `full_purges`) run the
  deferred cgroup discovery first (`Shared::cgroup_now`, since upstream
  #110) and are among them: a waiting `Deferred::discover` returns the
  cell's frozen permission (from its own release store of `DONE` when
  it discovered, else from the acquire load of `DONE` that ended its
  wait; the phase's invariant `PhasePred` holds that permission from
  `DONE` on), and `cgroup_now` reads the record through it
  (`Deferred::published`), with no second load of the phase and no
  check. (From #110 until this change they had witnesses of
  their own that required `integrity_aborts()`, for `cgroup_now`'s check
  that the record was published after the wait.)
- `mallctlnametomib` (`:3114`) requires no abort capability: it never
  aborts, before init too (it never runs it), for a `name`, `mibp` and
  `miblenp` that keep their C contract.

A corollary needs no new witness: theorem 8's
`theorem_no_syscall_on_fast_paths` (`api/libc.rs:673`) requires only
`integrity_aborts()` (and its call's block token), so the cache fast
paths of `malloc`, `free`, `free_sized` and `challoc_free_usable` it
calls abort at most at an integrity check; their checks that
fail leave the fast path for the slow one, which requires `may_abort()`.

The integrity checks the covered calls reach (read from the code; at
#138 `api::tests::abort_grants_only_at_roots` listed the callers
of `check` and `abort_integrity` and the trusted bodies that require
`may_abort_corrupt()` alone):
- `stats::huge_usable_pages` (`stats.rs:521`, abort in
  `huge_pages_short`, `:542`): an arena's `large` word holding fewer
  pages than its runs' guards. The word is a plain `AtomicU64` that
  `note_huge_alloc`/`note_huge_free` update together, so it fails only
  on a corrupt counter. Reached by `mallinfo`/`mallinfo2`
  (`collect_all`), not by any mallctl read (the `stats.*` totals read the
  stored snapshot).

`Shared::cgroup_now`'s integrity check (since upstream #110,
`abort_integrity` with "cgroup: discovery did not publish": the record
not published after a waiting discovery, reached by the seven
`stats.cgroup.*` reads and by `malloc_stats_print`'s `text_general`,
`policy_line` and `json_general`) is gone: `cgroup_now` reads the record
through the frozen permission the waiting discovery hands back (a
waiting `Deferred::discover` ensures it has one), so neither those reads nor
`policy_line` and `json_general` require `may_abort_corrupt()` any more.
This is an exec change of `cgroup_now` (and the cold functions it is
inlined into; its second load of the phase, `Deferred::get`'s, is
gone) and, in source only, of `discover`'s wait loop, which loads
through `load_phase` (the same `Acquire` load); the hot set is
unchanged.

Two integrity sites of the first version of this theorem are gone, both
over the arena count, which is now an atomic invariant (`NarenasPred`,
`arena.rs`): the counter's permission (`vspec::count::CountPerm`) has a
value at most `MAX_ARENAS`. `Arenas::init` (verified) makes it with the
permission `vspec::count::new` mints for the clamped automatic count,
`Arenas::create` stores `index + 1` inside it for an `index` below
`MAX_ARENAS`, and `Arenas::count` (verified, no longer `external_body`)
loads inside it, so its `ensures r <= MAX_ARENAS` is proved. With it, `stats::arena_at` requires `i <
MAX_ARENAS`, which every caller's loop below the count shows, and its
`check(i < MAX_ARENAS)` is gone (it was reached by `collect_all` and
`arena_stats_all`); `policy::empty_bytes` (trusted) requires only
`may_syscall()`: its one panic, the bounds check of `Arenas::iter`'s
table index below `arenas.count()`, cannot fail (it required
`may_abort_corrupt()` for that, and nothing before theorem 32, not even
`may_syscall()`, though it takes the class and huge locks: a gap in
theorem 8's list of trusted bodies, closed then; it is verified since,
as an index loop over `Arenas::get_existing` with `fig_add64` sums, and
has no table index left to check). With both, no mallctl
read needs `may_abort_corrupt()`, so `ctl`, `call`, the readers,
`policy::evaluate`, `purge::sample`, `print::policy_line` and
`print::json_general` no longer require it.

The `large` word gets no invariant: no honest one rules out the
remaining check. `Shared::free_huge` subtracts a run from its arena's
word after `Huge::free_run` retired it, so between the two the run's
pages can be reallocated and added to the same arena's word again. The
word therefore counts the live runs plus one per free in progress, and
nothing bounds the frees in progress (one per thread), so nothing proves
the page half below `2^32`; past it the low half carries into the run
half and the guard test can fail. Ruling it out needs an exec change
(the subtraction made at the retirement, under the huge lock inside
`free_run`, before the pages can be reused; an addition under the lock
alone would not do) or a trusted bound on concurrent frees.

The other integrity checks (`SlabTable::new`'s `check`, and
`AtomicBitmap::allocated`'s, `bitmap.rs:1383`: a cleared padding bit,
reached only by `malloc_stats_print`'s `slab_histogram`, which requires
`may_abort()`) are on no covered path. (Theorem 44's pending checks have their own capability,
`pending_aborts()`, which no witness of this theorem holds.)
`arena::alloc_sets` has no check on main (`global layout SetTable`,
`arena.rs:571`, gives the size). `print::Sink::flush` has no check on
main: `Sink`'s type invariant states the report buffer's bound
(`len < 1024`), which also puts the NUL `flush` stores at `buf[len]` in
bounds, and `Sink::write_str` (verified) keeps it.

**Why the capability can be trusted.**
- Every abort goes through `abort_fmt` (`sys/abort.rs:139`): `abort`
  (`:129`), every `-> !` helper, and the `#[panic_handler]`
  (`lib.rs:98`) that a `panic!` in a trusted body reaches. `abort_fmt`
  and `abort` require `may_abort()`; `abort_integrity` (trusted, a call
  of `abort_fmt`) and `check` (trusted, an `assert!`) require
  `may_abort_corrupt()`, or `c` for `check`. `die`, `exit_group` and
  `trap` (the process's end) require `may_abort()` too. (Since theorem
  44 two more trusted calls of `abort_fmt` take narrower capabilities
  that no witness of this theorem holds: `abort_kernel`
  (`may_abort_kernel()`) and `abort_pending` (`may_abort_pending()`).)
- About 340 `requires` and loop-invariant clauses name `may_abort()`
  and about 25 name `may_abort_corrupt()` (counted on main).
  Abort-site specs that the caller can discharge say so:
  `RunState::from_u8` and `HugeDesc::state` (`huge/mod.rs:110`, `:240`)
  require `v <= 2` (`spec_state() <= 2`) with no abort capability, which
  the huge lock invariant (`desc_ok`) gives `Huge::lookup` (`:7379`);
  `assert_flushed` and `assert_refill_empty` (`heap/mod.rs`) require
  `may_abort() || *len == 0`; `huge_usable_pages` requires
  `may_abort_corrupt()` or its condition, and `arena_at` its condition
  alone.
- Every trusted body that can abort requires a capability: those that
  call an abort or a function that requires it
  `background::spawn`,
  `call_throw_bad_alloc`; `owner_locked` until package OWNER) and those
  that `assert!` or
  `panic!` (`check`, `assert_flushed`, `assert_refill_empty`;
  `assert_not_home`'s `assert_ne!` is its `requires`). This is read off
  the code; in #138 a source test flagged a trusted body that
  aborts without the capability. (Since theorem 44 `owner_locked`
  requires `may_abort_pending()`, which no witness of this theorem
  holds.)
- `may_abort` is closed and `integrity_aborts` uninterpreted, so a verified function
  can prove `may_abort()` only from its own `requires` (or a loop
  invariant that carries it), or from `grant_aborts`, and
  `may_abort_corrupt()` only from those or `integrity_aborts()`, which
  only the verification-only witnesses require (this theorem's,
  theorem 8's fast-path witness and theorem 44's two direct-free
  witnesses).
- `grant_aborts` is called only in `extern "C"` bodies (the
  `verified_export_pair!` macro, `api/mod.rs:63`, and every hand-written
  export: 42 grants) and in the `#[panic_handler]`, which the panic
  runtime calls and no Rust code does; never in `abort_fmt` or another
  `-> !` helper, unlike `grant_syscalls`.
  `sys::tests::abort_grants_only_at_roots` (`sys/mod.rs:1668`) checks the
  grants' sites; the rest (`may_abort*` only as `requires`/invariant
  clauses, `integrity_aborts` only in the witnesses' `requires`, the
  integrity checks only in the functions listed above, the trusted
  bodies) is read from the code (in #138 a source test checked
  it).
  Outside tests no Rust code calls an `extern "C"` function of the
  crate.
- `mallctl`'s readers are named by the `Reader` enum and called by the
  verified `call`, a `match`; the statement names the reads by reader,
  not by subtree. That every key under `opt`, `arenas` and `stats` is
  covered (no `thread.*` reader under them) is read off the static tree,
  and `mallctl::tests::read_only_subtrees_are_roots_4_5_and_7`
  (`api/mallctl.rs:3627`, restored by wolfi-dev/challoc#418) checks it
  at run time.

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
- Init is complete. `shared()` (`api/mod.rs:793`) runs init the first
  time, and init aborts (`init_slow`, `:1247`: re-entrant init; `init`,
  `:1448`, and below: a metadata region too small or a refused table
  commit, a failed reservation, a region with less than `MAX_SLAB` of
  slack (`slack_broken`, `layout/regions.rs:2354`), `getrandom` or
  page-size probe, ...), so `shared()` requires
  `may_abort()`. The witness takes `shared: &Shared` with `shared.wf()`
  and calls what `nallocx`, `mallinfo`, `mallinfo2`, `challoc_features`,
  `mallctl` and `mallctlbymib` call after `shared()`. A well-formed
  `Shared` is not a proof that init returned (it is well formed inside
  `init`, before `READY` is published); the entry points reach that code
  only after `shared()` returned. Inside the readers the local
  `mallctl::shared` was trusted (it claimed `super::shared()` returns
  without aborting, true because readers run only inside
  `mallctl`/`mallctlbymib`, after their own `super::shared()`
  returned). Since package RDE it is verified: on main it requires
  `marks_ready(*mark)`, the witnesses' `ReadyMark`, which `ctl` passes
  through `call` to every reader that reads the state, and it reads
  through `api::shared_marked`, whose load of `STATE` is `READY` under
  the mark, so its init path is dead and it never aborts.
  So the witnesses' `shared.wf()` is the theorem's one assumption
  about init here: the readers add none.
- The trusted bodies these calls reach do not panic (read from their
  bodies at this commit): the futex wait and wake (`sys/mod.rs`; their
  results are discarded, so `EAGAIN` or `EINTR` loops) under
  `RawLock::lock`/`unlock`, `LockGuard::drop` and `deref`, the static
  projections and `shared_unchecked` (since upstream #77 `shared_base`
  and `shared_deref`), the `STATE`
  wrappers (`state_load`, `state_claim`, `state_publish`: one bare
  atomic operation each), the `LINE_STATE` wrappers (`line_load`,
  `line_compare_exchange`, `line_store`) and the deferred record's
  phase wrappers (`vspec::byte::load`, `compare_exchange`, `store`: one bare
  atomic operation each), `ClassTable::table`, `class_entry`,
  the watermark's load `vspec::atom::load_usize` (the registry reader
  `arena_at`, `stats.rs:879`, was retired by package TSLOT:
  `Arenas::get_existing` reads through the verified `arena_ref`),
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
- `malloc_trim` (`api/libc.rs:3258`) requires `may_abort()`, by two
  paths. `Heap::flush_all` returns the thread's cached slots, and the
  release aborts on a double free that sat in the cache
  (`straddling_double_free`, `heap/flush.rs:1911`; the class release's
  `double_free`/`invalid_free`), the program's earlier invalid free
  detected late. `purge::purge_all` (`purge.rs:2286`) discards through
  `discard` (`:2568`), which aborts when `madvise` fails.
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
  excludes it). The seven `stats.cgroup.*` reads, which from upstream
  #110 reached `Shared::cgroup_now`'s check, reach none now: that the
  record is published after a waiting discovery is proved from the
  phase's invariant (`sys::cgroup::PhasePred`), over the trusted
  `vspec::byte` wrappers (discharged by loom records in #138,
  trusted in main's ledger, partly checked by Kani on one thread) and the fork
  child's `axiom_cell_orphaned`. Its residue (a fork from a signal
  handler on the discovering thread) is outside the theorem: run one
  after the other, the two discoveries rewrite a published record
  without an abort, but run concurrently (the child has started another
  thread) they race on the cell, as that axiom's doc says.

Machine code: the hot set is unchanged (x86_64 disassembly identical to
the base commit outside the readers). The 43 `stats.*` and `arenas.*`
reader closures became named functions with the same normalized
disassembly (one `FnOnce::call_once` body each before, a named symbol
now); nothing else changed.

Machine code of the arena count invariant: the hot set is unchanged.
`count`'s load and `create`'s store are the bare `core` calls (the
`vspec::count` wrappers are `#[inline(always)]`), and the ghost field
has no bytes. Removing `arena_at`'s `check` changed, on x86_64 against
the base commit, only the functions it is inlined into, all statistics
and reports off the allocation paths: `stats::collect_all`,
`stats::collect_into`, `mallctl::arena_stats`, `malloc_stats`,
`malloc_info` and `malloc_stats_print` (the compare and abort branch
gone, registers allocated differently).
**Changed since #138.** The mallctl half moved out of
`theorem_read_only_never_aborts` into `theorem_mallctl_reads_never_abort`
and `theorem_mallctlbymib_reads_never_abort` (#403). Each now takes a
`ReadyMark` (`api/mod.rs:292`, `marks_ready`: the token for a `READY`
the caller saw, which `STATE`'s invariant keeps final), so a reader may
rely on `STATE` staying `READY`, which the `ready()` fact alone does not
give. `may_abort` is `closed` and `integrity_aborts` is `uninterp` on
main, where #138 had `open` and `opaque` definitions; the
capability argument is the same. The run-time scope test
`read_only_subtrees_are_roots_4_5_and_7` is on main again
(wolfi-dev/challoc#418); it checks each subtree's reader count
(`KEYS.len()`, 8, 35) where #138 checked their sum (`KEYS.len() + 43`).
`mallinfo` and `mallinfo2` (`api/glibc.rs:108`, `:166`) are verified on
main, as in #138 (since wolfi-dev/challoc#421); the witness restates
`mallinfo2`'s calls, which
`api::restated::read_only_restatements_match_their_bodies` checks.
Weaker on main: `theorem_no_syscall_on_fast_paths` requires
`integrity_aborts()`; the source test checks only the grant sites (the
integrity-check callers and trusted bodies are read off the code).
Stronger or simpler:
`RunState::from_u8` and `HugeDesc::state` require their condition
alone; `alloc_sets` and `Sink::flush` have no `check`;
`AtomicBitmap::allocated` requires `may_abort_corrupt()` where #138
required `may_abort()` (wolfi-dev/challoc#402), and `SlabDesc::sub_live`,
`external_body` in #138, is verified.

## 33. Lock order is acyclic

Read at this section's commit (the `file:line`s are the `fn`/`axiom`
line there).

**Statement.** Every allocator lock has a fixed rank, and a blocking
acquisition ranks above every allocator lock its thread holds, so no
set of threads can each wait for a lock the next one holds (around such a
cycle the ranks would climb back to where they started). There is no
exception: `fork_prepare` took the slab-set locks before the slab table
locks, against the run-time order, until upstream #96 (below).

```rust
// lock.rs: the ranks, lowest first (spec fns, lock.rs:230-289).
rank_tcaches()                 0x100_0000              explicit-cache table lock
rank_arenas()                  0x200_0000              arena creation lock
rank_stats()                   0x280_0000              process statistics lock
rank_class(i, c)               0x300_0000 + 0x100 i + c   class c of arena i
rank_table(c)                  0x400_0000 + c          slab table of class c
rank_sets(i)                   0x500_0000 + i          slab sets of arena i
rank_huge(), rank_meta()       0x600_0000, 0x700_0000
rank_arena_stats(i)            0x900_0000 + i
rank_state()                   0xA00_0000              background thread state

// lock.rs:696: a guard acquisition.
pub fn lock<'a>(&'a self, Tracked(ord): Tracked<&'a mut LockOrder<'_>>,
                Tracked(under): Tracked<&mut LockOrder<'a>>) -> (g: LockGuard<'a, T>)
    requires old(ord).allows(self.rank()),          // ord.top() < rank
    ensures  *final(ord) == *old(ord), final(under).top() == self.rank(), ...

// lock.rs:159: a raw acquisition (the fork handlers).
pub fn lock_held(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (t: Tracked<LockHeld>)
    requires old(ord).root(), old(ord).allows(self.rank()),
    ensures  t@.held() == *self, final(ord).root(), final(ord).top() == self.rank(),
```

A `LockOrder` (`lock.rs:316`, ghost, fields private to `lock.rs`) bounds
the ranks of the locks its owner holds (`top()`). `Lock::lock` borrows
the caller's token for the guard's lifetime `'a` and hands out another
at the lock's rank, also of lifetime `'a`, for the code under the guard;
Verus's lifetime checker rejects any use of the caller's token while the
guard or the handed-out token is in use (checked on a scratch file: a
second `lock` with the frozen token fails `E0499`). A handed-out token
used after its guard is dropped bounds a lock no longer held, which only
forbids more. Raw holds outlive any guard, so `lock_held` needs a root
token (`root()`: from `axiom_fork_entry` on main, from `axiom_entry` at
#138; never handed out under a guard) and
raises it in place. `RawLock::lock` itself requires `lock_rs_only()`, a
`true` whose body is closed, so verified code outside `lock.rs` cannot
acquire except through these two. Every function that may take a lock
takes its caller's token (`Tracked<&mut LockOrder>`, `requires
old(ord).allows(rank_x())` for the lowest rank it takes, `ensures
*final(ord) == *old(ord)`), so the bound follows the call chain, with
one exception on main: of `meta::Meta`'s methods only `Meta::stats`
(`meta.rs:1042`) takes the caller's token; the others take the
metadata lock with a token from `axiom_meta_order` (below).

The ranks are ghost fields of `RawLock` (`#[cfg(verus_keep_ghost)]`: a `Ghost`
field breaks `repr(transparent)`, which ordinary builds keep), set by
`RawLock::new(Ghost(rank))` / `Lock::new(v, Ghost(rank))`, and tied to
each lock by its owner's type invariant: `ClassState` (class `c` of
arena `arena`), `SlabTable`, `SetTable` (a slab-set rank), `Huge`, `Meta`,
`Arena` (sets and statistics), `Arenas`, `Tcaches`, and `Shared::wf`
(the process statistics lock). The fork accessors (`raw_lock`,
`stats_raw_lock`, `Arena::sets`, `Shared::*_lock`) state the rank.

**What is threaded** (every acquisition below is checked):

| Path | Nesting checked |
|---|---|
| slow allocation: `alloc_slow_in` (`heap/paths.rs:548`) → `tick`, `Shared::arena`, `ensure_cache`, `alloc_direct`, `refill` → `Heap::claim` → `claim_with` → `plan` (class) → `next_slab` → `next_fresh_slab` → `take_group` (table) → `ensure_sets` → `Set::ensure` → `SetTable::ensure` (sets) → `Lazy::ensure` → `Meta::commit` (meta); `commit_run` (since upstream #114: the run's `mprotect` and `install_guard` with no lock beyond the class lock, then the table for the guard count) → `commit_slab` (table; page groups above a slab, `thp:always` chunks) | Class(i,c) < Table(c) < Sets(i) < Meta; Arenas < Meta (`materialise` → `build` → `Arena::init`); the GC and decay of `tick` |
| slow free: `free_slow_in` (`:699`) → (since #103, `rehome_on_free` → `try_rehome` → `flush_arena` →) `flush` → `release_routed` → `release_owned` → `release_slab` → `dec_live` → `on_live_zero` (class), `reopen` (class); `Shared::free_direct` | class locks one at a time |
| huge: `Shared::alloc_huge` → `alloc_run` → `alloc_aligned` (huge; the lowest-fit `find` runs under it) → `alloc_desc`, `free_desc`, `Table::ensure`, `Set::ensure` (own store), `take` → `retire` (meta); `free_run` (huge) → `Table::ensure`, `retire` (meta) | Huge < Meta |
| cache flushes and routing: `Heap::flush_all` (`tcache.flush` through `Tcaches::flush`, `thread.tcache.flush` and `arena.<i>.purge` through `flush_caller`, `thread.tcache.enabled` through `set_tcache_enabled`) → `flush_arena` → `flush` → `release_routed` (class) and `free_caches_in` → `Meta::free` (meta); `gc` → `gc_arena` → `flush`; `set_arena` → `other_caches` (meta, when it creates a node) | class locks one at a time, then Meta |
| arena creation `Arenas::create`, heap creation `Heap::create` → `assign_home` (arenas) and `create_detached` (meta), explicit caches `Tcaches::create`/`destroy` (tcaches → meta / class, meta, huge) | Tcaches < Arenas, Meta, Class, Huge |
| purge `purge_all`, `purge_arena`, `decay` and `decay_arena` (through `decay_one`, `decay_huge`) → `Arena::decay_with`, `ClassState::decay_with`, `Huge::decay_with`, `on_tick`, `sample` → `policy::evaluate`; background `start` (state, then meta) and `main`; since upstream #111 the lazy start: `on_tick` → `auto_tick` (`policy::empty_bytes`: class and huge locks, then state, then, on an allocation's tick, `start_requested`), `start_requested` (`start`, then state), `request` (state; from `ctl`'s `background_thread` write) | class, huge, state locks one at a time |
| statistics `epoch`, `collect` (`collect_into`, with its exact-stats ghost outputs) and `arena_stats_all`, under the process statistics lock held across the scan (upstream #91) | Stats < Class(i, c) < Table(c), and Stats < Huge, Meta, ArenaStats(i), each released before the next is taken; `arena_stats_all`: Stats < ArenaStats(i) |
| statistics `collect_all`, `snapshot`, `arena_stats` (class → table, huge, meta, arena and process statistics), `mallctl` `ctl` and its named readers (`stats.*`, `stats.cgroup.empty_bytes`), `utilization`, `owner_of`, `get_defrag_hint`, `malloc_trim` | each lock with nothing above it held |
| `fork_prepare` (`api/mod.rs:1922`) | Tcaches < Arenas < Stats < Class(i, c) (arenas ascending, classes ascending) < Table(c) < Sets(i) (arenas ascending) < Huge < Meta < ArenaStats(i) < State, through one root token; before them (since upstream #110) the deferred cgroup discovery's `policy::evaluate` from the same root token, each class lock with nothing above it held, all released on return (`*final(ord) == *old(ord)`) |
| `fork_child` after `fork_release` (since upstream #111, under the lazy start: `policy::empty_bytes`, the inherited dirty bytes) | class and huge locks one at a time, from a token of its own |

`fork_prepare` proves it with loop invariants `ord.top() <
rank_class(i, 0)`, `< rank_class(i, c)`, `< rank_table(c)`, `<
rank_sets(i)`, `< rank_arena_stats(i)`; the arena indices it visits are
below `MAX_ARENAS` (`Arenas::count`, trusted for that bound then,
verified under its invariant `NarenasPred` since), so the `u16` arena
indices of the arenas it finds increase with `i`.

**The slab-set locks** (upstream #96). At run time `take_group` holds
Class(i,c) and Table(c) and takes Sets(i) (`SetTable::ensure` through
`ensure_sets`), so the ranks put Table below Sets. Until #96
`fork_prepare` took each arena's Sets(i) right after its class locks,
before every Table lock, which no total order fits; it was checked as
a *gated* acquisition (`RawLock::lock_held_gated`, which required the
tokens of all of arena `i`'s class locks and did not raise the token),
premised on every other holder of Sets(i) holding one of that arena's
class locks. Since #96 `fork_prepare` takes the Sets locks after all
the Table locks, in arena order, through `lock_held` like every other
lock: `lock_held_gated`, `lock::gated`, `lock::gate_rank` and the gate
premise are gone, and `SetTable`'s type invariant states its rank's
range (`rank_sets(0) <= rank < rank_sets(0x1_0000)`) instead of
`gated`.

**Premises beyond the common ones.**
- *Entry points hold no allocator lock* (`axiom_entry`, `lock.rs:448`;
  ledger category "C-program contract"). The roots: `alloc_slow_in`,
  `free_slow_in`, `alloc_huge_in`, `Heap::free_huge`, `paths::free_direct`
  and the no-heap `alloc_direct` arms of `paths::alloc`,
  `alloc_declined`, `alloc_zeroed` (`heap/paths.rs`); `pop_compact`,
  `pop_compact_run`, `rebuild_run` (`heap/compact.rs`); the `mallocx`
  routes (`heap/mallocx.rs:460`, `:509`); `libc::alloc_huge`,
  `malloc_trim` (`api/libc.rs`); `Huge::usable_size` (`huge/mod.rs`,
  called from the `api::libc` and `api::jemalloc` entry points and
  `Shared::zero_huge` on the free paths); `Shared::check_size_hint`
  (`heap/shared.rs`, the sized frees' slow paths, since #127); `ctl`,
  `utilization`, `owner_of`, `arena_stats`, and the named readers
  `cgroup_empty_bytes`, `tracked` (the seven `stats.cgroup.*` readers'
  cgroup discovery) and
  `stats_allocated` to `stats_retained` (`api/mallctl.rs`; `mallctl`
  calls them through its node table); `get_defrag_hint`; the report
  call sites (`api/print.rs`), and the same
  call in theorem 32's verification-only witness
  `theorem_read_only_never_aborts` (`api/mod.rs`);
  `Shared::init` (no lock exists yet); `create_heap_for_thread` and
  `heap_destructor` (`heap/thread.rs`); the background thread's `main`;
  `fork_prepare`; and, since upstream #111, `fork_child` once
  `fork_release` has handed back every token of the fork hold. The fast paths take no lock (theorem 9), and no
  allocator entry point is re-entered while a lock is held: allocator
  code calls nothing that may allocate with a lock held (theorem 16 for
  the reports' calls out, by inspection elsewhere), and a signal
  handler that allocates is outside the C contract (the fork handlers
  block signals).
- *The metadata lock* (`axiom_meta_order`, `lock.rs:500`, 8 call sites,
  all in `meta.rs`): the calling thread holds only locks ranked below
  the metadata lock. It stands in for the token `Meta`'s methods would
  take from their callers; in #138 they took it (13 `Meta` methods
  had a `Tracked<&mut LockOrder>` parameter), so the Meta < everything
  rows of the table above rest on this premise on main.
- *Closures* (`axiom_below_class`, `lock.rs:478`, and `axiom_below_huge`,
  `:489`; one `axiom_held_below` in #138): the decay walks'
  closures in `ClassState::decay_with` and `Huge::decay_with`
  (`Decayer::decay` takes a closure, which cannot carry a token). Both
  functions take the caller's token and require it below the lock the
  closure takes, so the premise restates a checked precondition for code
  that runs during the call.
- *The fork prepare handler's root* (`axiom_fork_entry`, `lock.rs:467`):
  the one root token; every other entry point's `axiom_entry` token is
  not a root on main.
- *Trusted bodies with honest specs*: `policy::empty_bytes`:
  it states the
  lowest rank its body takes (`assign_home`, trusted at this theorem's
  merge, is verified since theorem 34, `Meta::commit` since package
  META, `Heap::destroy` since package P10, and so are `Set::ensure` and
  the call `thread.arena` makes, `Heap::set_arena`; `policy::empty_bytes`
  is verified since it became an index loop over `Arenas::get_existing`);
  `purge::background::state`, which names `STATE`'s rank, verifies over
  the static's postcondition, which its initialiser proves.
- *Ranks set in constructors*: none is trusted. `Tcaches::init` is
  verified (one write of the whole value, whose lock it creates with
  the rank the type invariant states), and so is `Arenas::init`
  (`axiom_arenas_assemble` requires the creation lock's rank that
  `Arenas::type_inv` states).
- *The mutex*, as for fork safety: `lock` returns only once this thread
  holds the lock.

**Where the statement is weaker than the code.**
- Partial correctness and per-thread: the theorem rules out circular
  wait among allocator locks, not a lock held forever (a holder that
  never returns), and it says nothing about the futex itself.
- The ranks of a held set are bounded by `top()`, not tracked exactly;
  a token does not know which locks its owner holds (which is why the
  pre-#96 gate premise was by inspection).
- Code outside `verus!` and trusted bodies are covered by the premises
  above, not checked (`Heap::destroy` is verified since package P10).
- `fork_release` takes no lock and is not part of the statement; the
  release order is unstated.
**Changed since #138.** Weaker. #138 threaded the caller's
`LockOrder` through 13 `Meta` methods and trusted one closure axiom,
`axiom_held_below` (2 uses). Main threads it into `Meta::stats` only and
mints the metadata lock's tokens from `axiom_meta_order` (8 call
sites), with `axiom_below_class` and `axiom_below_huge` for the decay
closures and `axiom_fork_entry` for `fork_prepare`'s root. Threading the
token through `Meta` again and retiring `axiom_meta_order` is a
wolfi-dev/challoc#397 item. `mallinfo2` (`api/glibc.rs:108`) takes its
token from `axiom_entry`, as in #138 (verified again since
wolfi-dev/challoc#421; its lock-order premise is listed in `lock.rs`'s
`axiom_entry` doc).

## 34. Balanced arena assignment (bound counts at their invariants)

The `file:line`s are the `fn`/`spec` line at the commit that merged
this section onto the integration tip `64d801a`. Upstream issue #43
(arena spread) is about which arena a new thread lands on; this states
both halves of that choice: the count it reads, and the pick it makes
from the counts.

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
// arena.rs:3110
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

// arena.rs:1810, :1816, :1827
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

// arena.rs:1742: the invariant of bound count i (one per index, Arenas::wf)
impl InvariantPredicate<BoundKey, BoundVal> for BoundPred {
    open spec fn inv(k: BoundKey, v: BoundVal) -> bool {
        &&& v.0.id() == k.id                            // the counter's permission
        &&& v.1.id() == k.loc                           // the arena's ticket set
        &&& forall|t: int| v.1@.contains(t) ==> t < v.2@
        &&& v.0.value() == wrap32(v.1@.len() as int)    // count == #tickets mod 2^32
    }
}

// arena.rs:2807, :3250
pub fn unbind(&self, index: u16, Tracked(t): Tracked<Homed>)
    requires crate::sys::may_abort_pending(), (index as usize) < MAX_ARENAS, self.wf(), self.ticket_of(t, index as int),
    // body: assert(held >= 1 && prev == wrap32(held));
    //       assert(prev == 0 ==> held % 0x1_0000_0000 == 0);   (held: the tickets out)
pub fn bind(&self, index: u16) -> (r: Tracked<Homed>)
    requires index < MAX_ARENAS, self.wf(),
    ensures self.ticket_of(r@, index as int),
// arena.rs, since the port of upstream #103 (Heap::try_rehome's bind)
pub fn try_bind_idle(&self, index: u16) -> (r: (bool, Tracked<Homed>))
    requires index < MAX_ARENAS, self.wf(),
    ensures r.0 ==> self.ticket_of(r.1@, index as int),
    // body: load_bound, then bind_if_idle: compare_exchange(0, 1) inside
    //       the invariant, a fresh ticket only on Ok (a placeholder,
    //       no_ticket, of a fresh set otherwise)

// heap/mod.rs:3284, a conjunct of Heap::wf (and of Heap::keeps)
pub closed spec fn homed_ok(&self) -> bool {
    &&& self.bound == self.homed@ is Some
    &&& self.homed@ matches Some(t) ==> homed_on(t, self.arena as int)
    &&& self.rehome ==> self.bound      // since the port of upstream #103
}
```

`snap` is the ghost sequence of keys `assign` read, `snap[i]` arena `i`'s
`(bound[i], table[i] is null)`. A `Homed` ticket (`arena.rs:1720`) is a
`GhostSingleton<int>` of its arena's ticket set and the ghost index of the
arena; `homed_on(t, i)` (`:1727`) says it is arena `i`'s in the process's
registry (`bound_loc`, `:1757`: the ticket-set locations of
`Shared::built_at(Shared::storage()).arenas`). `Shared::wf`
(`heap/shared.rs:271`) now carries `arenas.wf()` (`arena.rs:2739`:
between 1 and `MAX_ARENAS` automatic arenas, every invariant present over
its counter in namespace `bound_ns`, ticket sets at distinct locations)
and `arenas.is_process()` (`:3397`: its invariants are the process
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
  (`:3194`: `assign` and `bind`, with a fresh ticket), `bind_if_idle`
  (`try_bind_idle`'s compare-and-swap from 0 to 1, with a fresh ticket
  when it succeeds and no change when it fails) or `unbind` (with
  the caller's ticket for that arena, which the ticket set contains, so
  the count it decrements counted it), each inside the invariant, so at
  every instant each count is its arena's ticket count mod `2^32`. The
  counts `assign` reads go through the invariant too (`load_bound`,
  `:3170`, which asserts the value read is the ticket count there), as
  do `bound` (`:3326`) and `try_bind_idle`'s first load; that is proved
  at the read and not exported, since no `ensures` can name the instant
  of a read.
- **Who holds the tickets.** `Heap::create` (`heap/mod.rs:5107`) gets the
  ticket with the arena from `assign_home` (`:4552`, now verified) and
  either gives it back (`unbind`, when the metadata region is exhausted)
  or returns it with the heap; `create_heap_for_thread`
  (`heap/thread.rs:303`) installs it in the heap's ghost `homed` field
  with the permissions, which `Heap::lemma_fresh_wf` (`heap/mod.rs:3400`)
  turns into `homed_ok`; when `ThreadKey::set` refuses the heap
  (theorem 15's heapless path) it drops the ticket and hands the heap to
  `destroy`, which unbinds it. `set_arena` (`:4787`, `thread.arena`) hands the
  old home's ticket to `unbind` and stores `bind`'s ticket for the new
  home. `Heap::try_rehome` (since the port of upstream #103, the late
  move of a young heap, `rehome` set, onto an idle automatic arena)
  does the same with `try_bind_idle`'s ticket for the new home, only
  when it won the compare-and-swap; a lost one hands back a placeholder
  that is dropped, and the heap stays where it is. `destroy` (`:5539`,
  verified) unbinds a counted heap's home arena, before its flush since
  #103. No other code makes or spends a ticket, so the tickets out for
  arena `i` are the counted heaps homed on `i`, plus a `create` in
  flight between `assign` and `create_heap_for_thread`.

With theorems 32 and 33: `assign` takes the creation lock with a
`LockOrder` token that allows `rank_arenas()`, which `assign_home` and
`Heap::create` take from `create_heap_for_thread`'s `axiom_entry` root
and pass down; the counter operations take no lock. `unbind` and its
abort (`unbind_unbound`) require `may_abort_pending()`, which `create`
carries and which `set_arena`'s and `destroy`'s `may_abort()` and
`try_rehome`'s `valid_aborts()` each imply.

Machine code is unchanged (x86_64 disassembly identical to the lane's
base commit, and after the merge identical to `64d801a`): the counter
wrappers are `#[inline(always)]` around the same `core` calls, the
tickets, the tokens and the snapshot are zero-sized, and the tuple
comparison stays `core`'s (`key_less`, trusted, below).

**How it is proved.** The loop of `assign` carries `least_of(snap,
best)` and `best_key == snap[best]` once a key is read, with `snap.len()`
the loop index except at the `break` (`invariant_except_break`), and its
`ensures` gives the `(0, false)` case at the `break`. `count_in` and
`unbind` open the arena's invariant around the counter operation:
`new_ticket` (`arena.rs:2498`) inserts the bound `v.2` as a fresh ticket
and raises it, `delete_singleton` removes the caller's (which `agree`
places in the set, so the set shrinks by one), and `lemma_wrap_step`
(`:2523`) moves the wrapped count by the same one. `Shared::init` gets
`arenas.wf()` from `Arenas::init` and `is_process()` from
`Arenas::lemma_is_process` (`:3403`; `init` builds the process's
`Shared`).

**Findings.** None: the code matches the statement. The tests
`automatic_arenas_are_assigned_least_loaded_and_built_lazily` and
`heap_churn_reuses_the_vacated_arena` (`heap/tests.rs`) exercise the
order and the counts; since #103, `rehoming_only_takes_an_idle_automatic_arena`
and `a_young_heap_moves_onto_the_idle_arena_it_frees_into` exercise
`try_bind_idle` and the moved binding.

**Premises beyond the common ones.**
- `vspec::count` (`vspec/count.rs`, the `vspec::word` pattern): a
  `CountPerm` is the counter with its `id` (`count_id`), and `fetch_add`,
  `fetch_sub` (wrapping), `load` and, since the port of upstream #103,
  `compare_exchange` (the strong form: it fails only on a different
  value, by C++20's semantics; S304, discharged with loom and those at
  #138, trusted in main's ledger, all partly checked by Kani on one
  thread) are one atomic operation
  on it, sequentially consistent in the model (the orderings, all
  `Relaxed`, are ignored, as for the bitmap words).
- `Arenas::init` (verified after this theorem's merge) ensures `wf` of the
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
- `key_less` (`arena.rs:1841`, `external_body`, discharged by Kani in
  main's ledger): `core`'s `<` on `(u32,
  bool)` is lexicographic with `false < true`. vstd specifies no order on
  `bool`; a verified spelling of the comparison changed
  `create_heap_for_thread`'s machine code.
- The fresh heap's fields, read back: one linear ghost token,
  `FreshHeap` (`heap/fresh.rs`, a tracked struct, not `Copy`; an
  `external_body` type until P6), holds the permission to the heap's
  128 bytes of plain fields (`head()`, `heap_head`) and records what
  the field writers stored in `shared`, `arena` and `bound`. `init_fields`
  (verified since P6) takes the head permission that `create_detached`
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
  and keeps what the token records), both verified on main.
  `mark_bound` (`heap/fresh.rs:242`) needs the permission and hands back
  one to the same bytes (`heap_head`), and records `bound` (its body, verified in
  Verus on main, stores `true` there through a permission carved from
  the token's; in #138 RefinedRust proved it); `tcaches::heap_new`
  (`heap/tcaches.rs:209`) uses the token up (through
  `HeapPerm::assemble`, below), giving the permission to the heap's,
  whose `others`, `shared`, `arena` and `bound` are what it records.
  `create_detached` and `create` pass it on. The memory-requires
  audit (`bdce79a`) had these bodies take and return the bare
  `PointsToRaw`; the token now carries it, so one linear object, not
  two, stands for the fresh heap.
  A token is valid while its heap is live: `destroy` does not take it,
  so one kept past `destroy` would be stale for a later heap in the same
  block; no code keeps one (`create_heap_for_thread` uses it right after
  `create`, `Tcaches::create` drops it). Since package P7 `heap_new` is
  verified: the token is used up by the trusted
  `thread::HeapPerm::assemble`, which makes the heap's linear permission
  with those claims (and, from `FreshHeap::shared`, that its `shared`
  field is the process's `Shared`), and `heap_new` puts it in the
  thread's slot token and borrows the heap from there.
- `Heap::destroy` (verified since package P10) unbinds a counted heap's
  home arena with the ticket the heap holds (`Heap::homed_ok`), first,
  and clears the heap's `bound` and `rehome` with it, so the dying heap
  holds no ticket and is not counted for the rest of `destroy` while
  another heap may be homed on (and win, through `try_bind_idle`) the
  same arena: "the tickets out are the counted heaps" holds throughout.
- `homed_ok`'s `rehome ==> bound` (since #103): a new thread heap is
  counted, so it holds whatever `mark_rehome` (`heap/fresh.rs:278`,
  verified on main; S122, RefinedRust in #138: `true`) stored;
  an explicit cache, assumed `wf` (`heap_at`), rests on `init_fields`
  storing `false` (verified since P6) and on `create` being the one writer that
  sets `rehome` (by inspection; `set_arena` and `try_rehome` clear
  it). `same_shape`, `caches_same`, `updates` and the field lists
  of `claim` and the accounting lemmas now keep `rehome` too.
- Every heap a verified path is handed is `Heap::wf`, now with
  `homed_ok` (common premises: `TlsSlot::entry`, `heap_at`): a thread
  heap's ticket is the one `create_heap_for_thread` installed, kept by
  every verified writer (`keeps`, `same_shape`, `caches_same` and
  `updates` now keep `bound` and `homed`); an explicit cache is not
  counted and holds none. One reference at a time (their safety
  contracts) is what keeps a ticket from being spent twice.
- `axiom_shared_built` (`heap/shared.rs:1459`) now also says the built
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

**Changed since #138.** The statement is unchanged.
`Arenas::unbind` requires `may_abort_pending()` in place of
`may_abort() || valid_aborts()`, and `Heap::create` `may_abort_pending()`
in place of `may_abort()`; both ask less of callers. `mark_bound` and
`mark_rehome` moved to `heap/fresh.rs` and are verified in Verus (at
#138 they were `external_body`, proved in RefinedRust), and
`mark_bound`'s `ensures` now gives a permission to the same bytes
(`heap_head`) rather than the same permission. `Heap::destroy` takes a
`tcaches::HeapUse` token and requires `h@.addr != 0`.

## 35. Utilization and defrag hint are exact (over the values read)

The `file:line`s are the `fn`/`spec` line at this section's commit.
Valkey's and KeyDB's defragmenters depend on these shapes
(`allocatorShouldDefrag` divides `size` by `nregs` and asserts a bin
size; `activeDefragAlloc` moves an object when the hint is 1).

**Statement.** Over the values each call loaded (a ghost record its body
fills in from the loaded locals; Verus does not constrain the loads,
so "exact" is over those values, as in layer 1 of theorem 22):
`experimental.utilization.query` (`utilization`, `api/mallctl.rs:2250`)
and every triple `batch_query` (`:2506`) stores report, for a live slot
(bitmap bit set, slab below the watermark) of a bin class, `(nslots -
live, nslots, nslots * size)`, plus, verbose and with the owning arena
built, `bin_nregs = curslabs * nslots`, `bin_nfree = bin_nregs -
curregs` and the lowest partial slab's address as `slabcur_addr` (null
when there is none); `(0, 1, size)` for a live slot of a class above
`BIN_MAX`; `(0, 1, usable)` for the start of an active huge run; zeros
for everything else. `get_defrag_hint(p)` (`api/jemalloc.rs:1016`; exported as
`je_get_defrag_hint`, `:1231`) is 1
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
// api/mallctl.rs:2250
pub fn utilization(shared: &Shared, p: *const c_void, verbose: bool) -> (u: Utilization)
    requires crate::sys::may_syscall(), shared.wf(),
    ensures
        exists|rd: UtilRead| #[trigger] util_read(crate::layout::process_regions(), p as usize, verbose, rd, u),

// api/mallctl.rs:2201 (opaque, revealed in utilization; abridged; c = class_table()[class])
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
// :2237: util_read = classified(rg, p, rd.kind) (:2146, classify's
// postcondition), the bin_stats bounds on rd.st and rd.curregs
// (curregs <= curslabs * nslots), rd.lowest a slab of the class, and util_from.

// api/mallctl.rs:2464 (verified over vspec::cbuf; the util_triple precondition is checked at the call)
unsafe fn store_triple(out: *mut usize, i: usize, nfree: usize, nregs: usize, size: usize,
    Ghost(oldlen): Ghost<usize>, Ghost(q): Ghost<*const c_void>)
    requires
        (3 * i + 3) * 8 <= oldlen,
        util_triple(crate::layout::process_regions(), q as usize, nfree, nregs, size),
        c_buffer(out as *mut u8, oldlen as int), c_writes(out@.provenance),
// batch_query's loop body (:2560):
//     let q = query_ptr(ptrs, i, Ghost(io.newlen));
//     let u = utilization(shared, q, false);
//     store_triple(out, i, u.nfree, u.nregs, u.size, Ghost(oldlen), Ghost(q));

// api/jemalloc.rs:1016, :956 (opaque, revealed in get_defrag_hint), :982
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

`UtilRead` (`api/mallctl.rs:2164`) and `HintRead` (`api/jemalloc.rs:935`) are ghost
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
  (`SlabDesc::nfree` now ensures `r <= class.nslots`), `nregs`, `size`
  and `bin_nregs` exact products, `bin_nfree` an exact difference
  (`bin_stats`' clamp keeps `curregs` within `bin_nregs`, so the
  `saturating_sub` never clamps), the slab address `slab_addr`'s. The
  `BIN_MAX` boundary is by size: `NBINS` now ensures that the classes
  below it are exactly those whose `spec_class_size` is at most `BIN_MAX`
  (`layout/mod.rs:130`; `count_bins`, `:139`, proves it with
  `lemma_class_size_mono`), and `Utilization::ZERO`
  (`api/mallctl.rs:2113`) ensures its zero figures.
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

Machine code is unchanged (x86_64 disassembly identical to the base
`0c9cd77`): the read records and the new parameter of `store_triple` are
ghost, `NBINS`, `count_bins` and `ZERO` keep their bodies (an `exec
const` with an `ensures` evaluates the same expression), and the one
exec edit, `batch_query` binding `query_ptr`'s result to `q` before
passing it to `utilization`, is the same expression evaluated in the same
order.

**How it is proved.** A ghost `rd` is filled in as each value is read
(`rd.live = cls.nslots - u.nfree` from `nfree`'s bound in `utilization`;
`rd.cur_live` is `curregs` before less `curregs` after the subtraction
in the hint), and each `return` asserts `util_read`/`hint_read` of it,
the trigger of the existential `ensures`. `util_from` and `defrag_rule`
are opaque, revealed only in the two bodies, so their callers' queries
hold them as atoms. The `BIN_MAX` branch uses `axiom_class_table_wf`
(`size == spec_class_size`) and `NBINS`' new `ensures`. `util_triple`'s
existential has one trigger, `util_read(.., rd, u)`, over a conjunction
whose other conjunct (`triple_is(u, ..)`) does not mention `rd`, so it
cannot carry a trigger of its own; it is a goal (proved from
`utilization`'s `ensures` at the call), not a hypothesis instantiated
elsewhere.

**Findings.** None: the code matches the statement. The tests
`utilization_queries_answer_valkeys_defragmenter` (`api/mallctl.rs:4773`)
and `get_defrag_hint_follows_the_utilization_data`
(`api/jemalloc.rs:2190`) exercise the shapes and the rule at run time.

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

The `file:line`s are the `fn`/`spec` line on top of the integration tip
`0c9cd77`. The registry's indices can have holes: an automatic arena is
built only when something first asks for it (`Arenas::get`, through
`Arenas::materialise`: a refill of a heap homed there, `mallctl`'s
`arena.<i>.*`, an explicit-arena allocation), and `arenas.create` hands
out the next index after every automatic one, built or not. With
`narenas:2`, only arena 0 built and one explicit arena, the registry
holds IDs 0 and 2 and nothing at 1.

**Statement.** Two layers.

1. **The scan keys each arena by its registry ID** (code, `stats.rs`).
   In `collect_into` (`:1130`) and `collect_all` (`:1343`) each read is
   built with `index` the ID of the arena it was read from
   (`collect_arena_with`, `:897`, ensures `r.index ==
   arena.spec_index()`; `arena_at`, `:879`, ensures `a.spec_index() ==
   i` for the table index `i` it was asked for, from
   `Arenas::get_existing`, `arena.rs:2853`, verified since package
   TSLOT through `arena_ref`), and the loop invariant keeps
   every earlier read's ID below `i`. The `ensures` (and `mallinfo2`'s,
   `api/glibc.rs:108`, from `collect_all`'s) export what a caller can
   name: the IDs of the reads handed out through the ghost `rd` ascend,
   so no arena is read twice (`ids_ascending`, `stats.rs:1521`), and each is
   below `MAX_ARENAS`:

   ```rust
   // stats.rs:1521
   pub open spec fn ids_ascending(reads: Seq<ArenaStats>) -> bool {
       forall|i: int, j: int| 0 <= i < j < reads.len()
           ==> #[trigger] reads[i].index < #[trigger] reads[j].index
   }
   // collect_into and collect_all, added ensures
   ids_ascending(final(rd)@.0),
   forall|j: int| 0 <= j < final(rd)@.0.len()
       ==> ((#[trigger] final(rd)@.0[j]).index as usize) < MAX_ARENAS,
   ```

   `collect_into` stores each arena's figures in that arena
   (`arena.store_stats(&a)`, with `a.index == arena.spec_index()`), so
   `stats.arenas.<i>.*` reads arena `i`'s own figures.

2. **Exact stats over a sparse registry** (ghost model,
   `stats/ledger.rs`). The ledger `Books` (`:50`) now has `built`, the
   indices whose entry is published, beside `k`, the registry's count.
   `Books::start(auto)` (`lemma_start`, `:1103`) has `k = auto` and only
   arena 0 built; `new_arena` (`Arenas::create`, `:261`) builds index
   `k` and adds one to `k`; `materialise(a)` (`Arenas::build` of an
   automatic index, `arena.rs:3561`, `lemma_step_materialise`, `stats/ledger.rs:1078`) builds
   `a`. Every counter step (`reserve`, `claim`, `release`, `count`,
   `take_run`, `note`, `free_run`, `unnote`) requires its arena built,
   and `Books::inv` now includes `unbuilt_fresh` (`:207`): every unbuilt
   index below `k` has zero counters and no slots or runs (`fresh`,
   `:272`, now one quantifier per conjunct), and bounds `k` by
   `MAX_ARENAS` (`new_arena` requires `k < MAX_ARENAS`, as `create`
   checks; `start` requires `1 <= auto <= MAX_ARENAS`, as `init`
   clamps), so a ledger never has a built index a `u16` ID cannot name
   and `reads_keyed` can always be met. `theorem_exact_stats`
   (`:1638`) takes the reads keyed by ID:

   ```rust
   // stats/ledger.rs:1503
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

   `lemma_reads_keyed` (`:1523`) shows such reads sum to the ledger's
   counters over all of `0..k`: by induction over the arenas, the reads
   of IDs below `a` are a prefix (the IDs ascend), and an index no read
   names is not built (every built one is named), so its counters are
   zero. Theorem 22's conclusion (the reported `allocated` is the live
   blocks' usable bytes plus the cached slots') therefore holds with
   holes; it used to require `reads.len() == b.k` and `reads[i]` to be
   arena `i`, which the scan of the registry above (IDs 0 and 2, two
   reads, `k = 3`) does not satisfy.

The test `stats::tests::collection_keys_arenas_by_registry_id_across_a_hole`
(`stats.rs:2066`) runs the example: `narenas:2`, `create` returns 2,
arena 1 stays unbuilt through `collect` and `epoch`, both scans count 2
arenas, `arena_stats(1)` is `None`, arenas 0 and 2 report index 0 and 2
with their own `curregs` (1 and 2), and the totals are their sum.
No runtime bug: the code always keyed by ID; the gap was in theorem 22's
premise.

**Covers / stated in.** `collect_into` (so `epoch`, `collect`,
`malloc_stats_print`'s totals and the `stats.*` snapshot), `collect_all`
(`mallinfo`, `mallinfo2`, `malloc_stats`), and `theorem_exact_stats`.
`Arenas::create` (`arena.rs:3715`) now also ensures the ID it returns is
below `MAX_ARENAS`.

**Premises beyond theorem 22's.**
- A stable registry during the scan: the `arenas.count()` the scan
  loads is the ledger's `k`, and each entry load in `get_existing` sees
  the entry as published (non-null exactly for the built indices).
  `get_existing`'s `ensures` gives nothing on `None` (a load outside
  the creation lock may read a stale null), so "every
  built arena is read" is `reads_keyed`'s fourth conjunct, a premise,
  like the quiescence link of the counters. Nothing frees or unpublishes
  an arena, so an entry, once non-null, stays the same arena. Since
  package TSLOT `get_existing`'s `spec_index() == index` is proved: it
  loads the entry inside the registry's invariant (`vspec::entry::load`)
  and reads the arena through its frozen permission (`arena_ref`); the
  trusted `arena::arena_at` it rested on is gone.
- The ledger's `materialise` and `new_arena` steps abstract
  `Arenas::build`'s release store of the entry and `Arenas::create`'s
  `count` store (read from the code, as theorem 22's other steps).

**Scope and where the statement is weaker than the code.**
- "Visits every published arena once" is: once is proved (`ids_ascending`);
  every is the stable-registry premise above. The ensures says the
  IDs are below `MAX_ARENAS`, not below the count the scan loaded (a
  local the `ensures` cannot name); `reads_keyed` requires `< k`.
- `Arenas::init` is verified, and the registry's entries are under its
  invariant (`NarenasPred`, `arena.rs:1651`): `build` (`:3561`) requires
  that the creation lock's snapshot has no arena at the index, so each
  index is published at most once (proved), and `create`'s index is at
  or above `auto` by `axiom_arenas_lock_frame` (`:3387`: the lock's `n`
  never drops below its initial, automatic count; its writers are
  verified against it). The ledger's `built` set is not tied to that
  snapshot (read from the code).
- `Snapshot::narenas` is the number of arenas the scan found, so it is
  below `arenas.narenas` while an automatic arena is unbuilt
  (`malloc_stats_print`'s "Arenas: 2 (2 automatic)" with IDs 0 and 2,
  where `arenas.narenas` reads 3); its doc comment said
  "`arenas.narenas` at the time" and now says what it counts.
- `arena_stats_all` (`stats.arenas.4096.*`) and the report scans in
  `api/print.rs` have no keyed contract; they key their output by
  `a.index()` too.

**Changed since #138.** `theorem_exact_stats` no longer requires
`reads_ok(reads, pages)` (a weaker premise). `mallinfo2`
(`api/glibc.rs:108`) ensures `summed`, `ids_ascending` and the
`< MAX_ARENAS` bound of its reads, as in #138 (verified again on main
since wolfi-dev/challoc#421).
## 37. Slab claim never misses free capacity (at quiescence)

Numbered at merge. The `file:line`s are the `fn`/`spec` line at this
section's commit.

**Statement.** A non-empty claim of an arena's state of a class returns
no slot only after a lock hold that ran dry, in whose lock value every
committed slab of the arena is owned, `Active` and not a guard (proved
of the code). If the arena was quiescent at that hold, no committed slab
had a free slot: in the ghost protocol model linked to that hold by
premise, every one has every slot's bit set and `live == nslots`, so
none had an unreserved free slot either. Two layers.

1. **The code's half: what a dry hold saw** (`class/mod.rs`, verified).

```rust
// class/mod.rs:2870
fn claim_with(&self, meta: &Meta, out: &mut [*mut u8], mut rng: Option<&mut u64>,
    Tracked(toks): Tracked<&mut SlotToks>, Tracked(ex): Tracked<&mut Ghost<(Inner, Map<nat, usize>)>>,
    pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (Claim, Tracked<Map<int, Zeroed>>))
    ensures
        ...
        r.0.n == 0 && old(out)@.len() > 0 ==> self.exhausted(final(ex)@.0, final(ex)@.1),

// class/mod.rs:3825
fn plan(&self, meta: &Meta, want: usize, picks: &mut [Pick; MAX_PICKS], ...,
    Tracked(ex): Tracked<&mut Ghost<(Inner, Map<nat, usize>)>>, Tracked(ord): Tracked<&mut LockOrder>) -> (np: usize)
    ensures
        np == 0 && want > 0 ==> self.exhausted(final(ex)@.0, final(ex)@.1),
        ...

// class/mod.rs:3763
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
   which now ensures both idle sets empty and the partial set unchanged
   (`class/fresh.rs:36`). `lemma_exhausted` (`class/mod.rs:3785`) draws
   the consequence in the lock value: every committed slab is owned, not a
   guard and `Active` (neither idle set holds it, and a committed slab is
   never `Fresh`, the new conjunct of `Inner::slab_core`,
   `class/mod.rs:1639`), and a partial one was read with `live >= nslots`.

2. **The lock-free half: a ghost protocol model** (`class/complete.rs`,
   verified). `Proto` (`:130`) is one arena's state of one class: the
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
   `Purge::PurgeLocked` step; formerly `purge_group`). `Proto::inv` (`:216`) holds at `Proto::start`
   (`lemma_start`, `:489`) and after every step (`lemma_step_*`,
   `:502`-`:742`), in any interleaving. Its core (`slab_inv`, `:190`): `live`
   is the set bits plus the unclaimed reservations plus the unsubtracted
   releases, at most `nslots`; and a *closed* slab (`Active`, out of the
   partial set, no `Reopen` entry pending) has every slot set or
   reserved, and while a reservation is out it is all its closer's (the
   refill whose `take` closed it), whose flight holds every set bit. So a
   release of a closed slab's slot finds nothing reserved (the slot is in
   no flight), the slab and so the slot's word full, and records an
   entry: the slab is no longer closed. `theorem_quiescent` (`:758`): with
   nothing in flight, `live` is the set bits and a closed slab is full.
   `theorem_dry` (`:782`): at a quiescent state with both idle sets empty
   and every partial slab's `live` at `nslots` or more, every committed
   slab is `Active` with every bit set and `live == nslots`.
   `lemma_example` (`:812`) runs one slot through `own`, `take`, `claim`,
   `hand_out`, `release`, `reopen_step`, `dec`, `live_zero` and to
   `purge`'s precondition, chaining the step lemmas into a state that
   meets `theorem_dry`'s premises with a committed slab;
   `lemma_example_clear` (`:860`) reaches `clear`'s (its `bits == slots`
   case). So the steps' preconditions and the theorem's premises are not
   vacuous, though `lemma_example`'s state has no partial slab, so the
   partial-`live` premise holds there trivially.

The two layers meet in:

```rust
// class/complete.rs:915
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
(`class/mod.rs:2870`), `plan` (`:3825`), `take` (`:4015`), `next_slab`
and `next_fresh_slab` (`class/fresh.rs:36`, `:127`). Modelled, as
steps of `Proto` (the code's `protocol::Refill`, `Release` and `Purge`
steps, `class/mod.rs`), and related to the code by premise: `finish`,
`claim_prefix`, `release_word` (`class/mod.rs:4669`), `reopen`
(`:4739`), `dec_live`, `on_live_zero` (`:4816`), `push_group` and
`settle` (`class/decay.rs:1165`, `:1280`, which replaced `purge_group`)
and `take_group`. `claim_tokens` (`class/mod.rs:2749`) and
`claim_random_zero` (`:2776`) call `claim_with` with a local `ex`,
return its claim count unchanged and do not export the ensures (their
signatures are the heap's interface). `claim` (`:2670`, which calls
`claim_tokens`) and `claim_random` (`:2694`, which calls
`claim_random_zero`) are test-only wrappers
(`cfg_attr(not(test), allow(dead_code))`) outside `verus!` on main, with
placeholder tokens (`Tracked::assume_new()`); production code calls the
verified `claim_tokens` and `claim_random_zero` directly
(`heap/flush.rs:96`, `:98`; `heap/shared.rs:2907`). Machine code is unchanged on x86_64 (identical to a build of
`0c9cd77`); aarch64 was not linked here. The one exec edit binds
`plan`'s `desc.live()` load to a local the ghost `seen` records.

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
- Partial correctness: `claim_with`'s retry loop's termination stays
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
  value by premise. A code-level version would need the descriptor's
  `live` and the `Reopen` lists as ghost state inside the bitmap words'
  invariants (TODO, theorem 19's still-open item). Unlike `src/model`,
  whose steps mirror the code's `protocol` enums by construction, the
  steps here are tied to the code by reading it; a run-time cross-check
  (in `src/model`'s `check`: at a quiescent state, a refill that comes up
  short leaves no free slot) is not written.
- The public claims do not export the fact (their contracts are
  unchanged); it is stated at `claim_with`, which `claim_tokens` and
  `claim_random_zero` call and return directly.

**Findings.** None: the protocol keeps the sets complete. The new
`Inner::slab_core` conjunct (a committed slab is not `Fresh`) needed
`next_fresh_slab` to hand its committed slab out under `inv_but`, which
`take` already required; `next_slab` and `next_fresh_slab` now state that
they leave the partial set alone and that `None` means both idle sets
are empty.

**Changed since #138.** The theorem holds on main through the
verified `claim_tokens` and `claim_random_zero`, which production code
calls and whose contracts match #138's. The test-only wrappers
`claim` and `claim_random` now sit outside `verus!` with placeholder
tokens (`claim` calls `claim_tokens`); that is cosmetic. Stronger: `exhausted` gained
`g.oauth@.id() == self.spec_oloc()`, and `take` (`class/mod.rs:4015`)
now also ensures that a slab its hold did not fill stays in the partial
set. The model (`class/complete.rs`) is unchanged apart from
`lemma_example_clear`, which now also proves its states keep
`Proto::inv`, and the documented padding-bit assumption above.
## 38. Explicit purge leaves no empty slabs (at each lock release; one slab per page)

Stated per lock release, over a ghost record (`purge::Left`) the purges
set themselves: when `purge::purge_all` or `purge_arena` returns, no
class of an arena it visited had an `Empty` slab in the class lock value
its purge released, except in a class with several slabs per page or in
a range `madvise` refused during the purge (`mlock`ed memory, upstream
7a12536a), and the huge allocator had no `Empty` extent in the huge lock
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
The `file:line`s are the `fn`/`spec` line at this section's commit.

**Statement.**

```rust
// purge.rs:2014
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

// purge.rs:2044: class `c` has several slabs per page.
pub open spec fn paged(c: int) -> bool { crate::layout::guard_geom(c).0 != 0 }

// purge.rs:2083, :2090
pub open spec fn clean(self) -> bool {
    &&& forall|t: (int, int, int)| #[trigger] self.slabs.contains(t) ==> paged(t.1) || self.refused.contains(t)
    &&& forall|a: int| #[trigger] self.extents.contains(a) ==> self.refused_extents.contains(a)
    &&& self.bytes >= 0
}
pub open spec fn covers(self) -> bool {
    forall|a: int, c: int| #[trigger] self.classes.contains((a, c))
        <==> self.arenas.contains(a) && 0 <= c < NCLASSES
}

// purge.rs:2286
pub fn purge_all(shared: &Shared, Tracked(left): Tracked<&mut Ghost<Left>>,
    Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
    ensures
        (r > 0) == (final(left)@.bytes > 0),
        forall|i: int| #[trigger] final(left)@.arenas.contains(i) ==> 0 <= i < MAX_ARENAS,
        final(left)@.covers(),
        final(left)@.huge,
        final(left)@.clean(),

// purge.rs:2359
pub fn purge_arena(shared: &Shared, index: u16, Tracked(left): ..., Tracked(ord): ...)
    -> (r: Option<usize>)
    ensures
        r is None ==> final(left)@ == Left::none(),
        r matches Some(n) ==> (n > 0) == (final(left)@.bytes > 0),
        r is Some ==> final(left)@.arenas == set![index as int],
        final(left)@.huge == (r is Some && index == 0),
        final(left)@.covers(),
        final(left)@.clean(),

// api/libc.rs:2884
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
  `ClassState::walk_empty` (`:487`) ensures `none_above(g, last)` (`:201`,
  opaque): no member of the empty set lies above the last slab visited;
  each visit is handed `next_after(g, last, slab)` (`:195`, opaque):
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
- **Composition.** `Arena::purge_all` (`arena.rs:1462`) joins its
  classes' records (`Left::join`, `lemma_join`, `purge.rs:2105`), with the
  arena invariant giving `classes[c]` class `c` and the arena's index;
  `purge_all` and `purge_arena` join the arenas' and the huge records;
  `malloc_trim` returns `(purge_all(..) > 0) as c_int`, and `Left::none()`
  before init.

**Covers.** `malloc_trim`; `mallctl` `arena.<i>.purge` (both forms) and
`arena.<i>.destroy`; the full purge `purge::decay` runs on a cgroup
`high` rise; the exit purge in `Heap::destroy` (verified since package
P10; its call is through
`purge_arena`'s contract).

**Premises beyond the common ones.**
- The class and huge lock axioms (`axiom_class_lock_inv`,
  `vspec/huge.rs`), as for theorems 17, 19 and 29: the value a lock
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
  supported page size (`layout::lg_slab_at`, `layout/mod.rs:1204`,
  raises every slab to the page up to 64 KiB, and the process page is at
  most 64 KiB: `Regions::separated`, `layout/regions.rs:269`, carries
  `lg_page <= 16`, proved from `getauxval`'s specification at init on
  main, where #138 assumed it with `axiom_process_page_at_most_64k`), but nothing
  proves it of the process's table: `guard_geom` is uninterpreted and
  `class_table()` is not tied to `classes_for_page` (`SlabTable::new`
  computes `slabs_per_page = (page / slab).max(1)`, `class/table.rs:1618`).
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
- Ghost only: the release library is identical to the base commit's
  (440 of 440 functions, x86_64; aarch64 not compared in this lane, it
  cannot link aarch64 here). `purge_all` binds the huge purge's
  count to a local before the sum (`let h = ..; n + h`), the same code.
  One unrelated proof step needed a hint under the new context:
  `api/libc.rs`'s huge-usable lemma now proves `n != 0` by
  `nonlinear_arith` (proof only).

**Changed since #138.** Stronger (#385): the process page's 64 KiB
bound is no longer an axiom (`axiom_process_page_at_most_64k`); it is a
conjunct of `Regions::separated`, proved at init from `getauxval`'s
specification.

## 39. Fork handlers restore the signal mask

The `file:line`s are the `fn`/`axiom`/`spec` line at this section's
commit (on top of the integration tip `dbf2461`). The handlers block signals
so that a signal handler on the forking thread cannot re-enter the
allocator, or fork again, while it holds every allocator lock (module
doc of `api`, design section 9); this states that they do, and that
the thread gets back the mask the slot holds, on both sides of the
fork.

**Statement.** `fork_prepare` blocks every blockable signal but glibc's
internal `SIGCANCEL` and `SIGSETXID` (32 and 33; since upstream #133,
e721a885: a thread holding `SIGSETXID` blocked hangs every
`setuid`-family call) before its
first acquisition of a lock of the fork lock list and keeps them
blocked through its last (since upstream #110 its deferred cgroup
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
// sys/mod.rs:220, :226, :305
pub open spec fn effective(m: u64) -> u64 { m & !unblockable() }   // less SIGKILL, SIGSTOP
pub open spec fn all_blocked() -> u64 { effective(BLOCKABLE_SIGNALS) }   // every bit but 8, 18 (SIGKILL, SIGSTOP), 31 and 32
pub open spec fn wf(self) -> bool { effective(self.mask()) == self.mask() && self.mask() & !BLOCKABLE_SIGNALS == 0 }   // SigMask

// sys/mod.rs:1380, :1407
pub fn block_all_signals(Tracked(sig): Tracked<&mut SigMask>) -> (r: u64)
    requires ..., old(sig).wf(),
    ensures r == old(sig).mask(), final(sig).mask() == all_blocked(), final(sig).wf(),
pub fn set_signal_mask(mask: u64, Tracked(sig): Tracked<&mut SigMask>)
    requires ..., mask & !BLOCKABLE_SIGNALS == 0,
    ensures final(sig).mask() == effective(mask), final(sig).wf(),

// api/mod.rs:1830
pub(crate) open spec fn fork_sig_holds(sig: SigMask, held: ForkHeld, orig: u64) -> bool {
    &&& sig.wf()
    &&& sig.mask() == all_blocked()
    &&& held.saved() == Some(orig)
    &&& effective(orig) == orig
    &&& orig & !crate::sys::BLOCKABLE_SIGNALS == 0
}

// fork_prepare (api/mod.rs:1922): `orig` is the entry token's mask,
// read before `block_all_signals`; every lock loop has the invariant
// `sig.mask() == all_blocked()`; at the end (:2147):
assert(fork_holds(*s, held, state));
assert(fork_sig_holds(sig, held, orig));

// api/mod.rs:2150, :2180 (both verified): the slot is written and read
// only under every allocator lock; the store, under the prepare's hold
// (no permission to the slot, `no_slot`, :1727), is recorded in the
// hold with the word it went to (`slot_addr`), and the release's hold
// carries the permission to that word (`has_slot`, :1720: a
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

// fork_release (api/mod.rs:2209): from the handoff, `saved` is the
// hold's slot content (:2230) and `fork_sig_holds(sig, held, saved)`
// (:2233); every release loop has `sig.mask() == all_blocked()`; after
// the last unlock and `set_signal_mask` (:2368):
assert(sig.mask() == saved);
```

**Covers / stated in.** `fork_prepare`, `fork_release` (the body of
`fork_parent` and `fork_child`), `save_sigmask`, `saved_sigmask`, `fork_slot`
(`api/mod.rs`); `block_all_signals` and `set_signal_mask`
(`sys/mod.rs`), whose new `ensures` rest on the kernel's
`rt_sigprocmask` semantics. `purge::background::spawn`
(`external_body`) calls both with placeholder tokens
(`Tracked::assume_new()`); it is not part of the statement.

**Proof.** `block_all_signals` returns the entry token's mask (`orig`)
and leaves `all_blocked()`; nothing else touches `sig` until the
release's last unlock, which the loop invariants restate at each
acquisition and release. `save_sigmask` is called after the last
acquisition, so its requirement (`fork_holds`, every listed lock and the
state lock) holds; it stores `orig` and records it in the hold, and
`ForkHeld::lemma_with_saved` (`api/mod.rs:1725`) keeps `fork_holds`
across the store. `saved_sigmask` is called before the release's first
unlock (`fork_unlock(state)` consumes the state token, so a later call
could not meet the requirement); it loads through the hold's slot
permission, so `word::load`'s `r == perm.value()` and `has_slot` give
`held.saved() == Some(r)`. The read mask has `SIGCANCEL` and
`SIGSETXID` clear (`fork_sig_holds`, from the handoff), as
`set_signal_mask` requires. Restoring with `set_signal_mask` leaves
`effective(saved)`, which is `saved` because the slot holds a mask the
kernel held (`lemma_effective_idempotent`, `sys/mod.rs:233`, a
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
`concurrent_fork_handlers_restore_each_threads_mask` (in #138; not
on main) ran two threads with different masks through the prepare and parent
handlers 200 times each and checked each got its own mask back;
`fork_handlers_block_signals_while_locked` (`api/mod.rs:3757`) checks both sides
for one thread.

**Findings.** None: the code matches the statement.

**Premises beyond the common ones** (and theorem 13's mutex and
handoff premises).
- *The calling thread's mask* (`sys::axiom_thread_mask`,
  `sys/mod.rs:260`; axiom). The token minted at `fork_prepare`'s entry
  stands for the calling thread's mask (its value unnamed: a mask the
  kernel can hold, with glibc's `SIGCANCEL` and `SIGSETXID` unblocked,
  as glibc's `sigprocmask` and `pthread_sigmask` strip them and
  challoc's raw calls never block them), and nothing on the thread
  changes the mask while the token is held except through it: challoc
  calls `rt_sigprocmask` only in `block_all_signals`, `set_signal_mask`
  and `abort` (which unblocks `SIGABRT` and never returns), and no other
  code runs on the thread inside the handler. One call per handler
  entry; nothing enforces it at run time (`fork_hold_is_resumed_once`,
  `api/mod.rs:2919`, checks there is one call site). `SigMask`'s field is private to a module of its own
  (`sys::sigmask`), so no code builds a token.
- *The kernel's `rt_sigprocmask`* (`sigprocmask_block`, `sys/mod.rs:1443`, and
  `sigprocmask_setmask`, `:1476`, in `sys::linux`, private;
  `external_body`, each the one `raw::syscall4` call its caller made
  before). A successful `SIG_BLOCK` of `BLOCKABLE_SIGNALS`, every
  signal but `SIGCANCEL` and `SIGSETXID`, wrote the mask in force before
  it to the old-set buffer and left exactly `all_blocked()`: `SIG_BLOCK`
  adds to the mask in force, and no thread has those two blocked
  (glibc's `sigprocmask` and `pthread_sigmask` refuse to block them, and
  challoc's raw calls block only `BLOCKABLE_SIGNALS` and restore masks
  saved from those; a premise since #133); a successful `SIG_SETMASK` leaves the requested mask less
  `SIGKILL` and `SIGSTOP` (signals 9 and 19, in the 64-bit kernel
  sigset on both architectures). Success is the word `decode` takes for
  success; on failure nothing is said (the callers abort). The value
  comes from the call, so no caller can choose it.
- *The handoff, its signal half* (`api::axiom_fork_resumed_sig`,
  `api/mod.rs:1848`, and `axiom_fork_resumed`'s new conjuncts, `:2534`;
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
  `&FORK_SIGMASK`; a static has one address) is the handoff premise, as
  for the `&Shared` both handlers read. Passed another word, the axiom
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

**Changed since #138.** The premises assume more: `SigMask::wf`
(`sys/mod.rs:305`) now also says glibc's `SIGCANCEL` and `SIGSETXID`
are unblocked (`mask & !BLOCKABLE_SIGNALS == 0`), so
`axiom_thread_mask` (`:260`), which ensures `wf`, assumes it of the
calling thread's mask; and `axiom_fork_resumed` (`api/mod.rs:2534`)
also ensures that the saved mask lies inside the blockable signals.
`set_signal_mask` (`sys/mod.rs:1407`) now requires the mask to leave
those two unblocked, which the release meets through `fork_sig_holds`'
new conjunct. `purge::background::spawn` passes placeholder tokens
(`Tracked::assume_new()`) instead of one from `axiom_thread_mask`.
`fork_hold_is_resumed_once` now also checks the call sites of the
thread-mask and handoff axioms. The concurrent run-time test
`concurrent_fork_handlers_restore_each_threads_mask` is not on main.

## 40. Purge batches are well formed

Every range a purge hands to `madvise` (`purge::discard`, the one place
a batch's ranges reach the kernel) is non-empty, starts and ends on a
page boundary of the process's page, and lies, byte by byte, in the
huge region or in a class region outside that class's guard slabs; so
no byte of it is in the metadata region. For every batch: the huge purges
(`Huge::purge_empty`, `purge_curve`, `free_run`) and the class purges
(`ClassState::purge_empty`, `purge_curve`, through `push_group`), each
flush, including the one inside a `push` to a full batch and the
re-issue of a refused range's pieces (upstream 7a12536a).
The `file:line`s are the `fn`/`spec` line at this section's commit.

**Statement.**

```rust
// purge.rs:383 (opaque; `lemma_well_formed`, :392, unfolds it)
pub open spec fn well_formed(addr: int, len: int) -> bool {
    &&& page_bytes() > 0                      // `1 << process_regions().lg_page`
    &&& len > 0
    &&& addr % page_bytes() == 0
    &&& len % page_bytes() == 0
    &&& forall|b: int| addr <= b < addr + len ==> #[trigger] discardable(b)
}

// purge.rs:373, :355, :363
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

// purge.rs:472
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
(for every `c` and `s`, as its arguments). A `Batch` (`purge.rs:330`)
also records the ranges it handed over, in call order (a ghost `gone`,
`Batch::discarded`): each `discard` call appends its range, `push` and
`flush` only extend the record. In #138 the public
`Batch::lemma_discarded` read `well_formed` of each recorded range out
of `Batch::wf`; main drops that lemma (nothing called it). At both
commits `discard` (`purge.rs:2568`) requires `well_formed(addr, len)`,
so every call is checked against it.

How it is proved:
- **The batch.** `Batch::wf` (closed) keeps `well_formed` of every range
  it holds, of each piece of a merged range (`runs_wf`: a merge only
  joins ranges of one length, `lemma_run_merge`), and of every range in
  `discarded()`; a piece of a range is inside it
  (`lemma_piece_ok`); `push` (`:1189`)
  requires it of the pushed range, and its merge with the last range
  (adjacent ranges go out as one) keeps it (`lemma_well_formed_join`,
  `:406`: two adjacent page-aligned ranges of discardable bytes are
  one). `well_formed` is opaque, so the batch's invariant carries one
  fact per range.
- **Class purges.** `push_group` (`class/decay.rs:1165`) pushes the page
  group of `slab` (`spp` slabs from `first = slab & !(spp - 1)`) and now
  requires `slab` in the empty set and `spp` the table's slabs per page,
  which `Purger::visit` has. `lemma_group_well_formed`
  (`class/decay.rs:1084`): the table's base and a page group's bytes are
  page multiples (`SlabTable::inv` gains `lg_page ==
  process_regions().lg_page` and the opaque `groups_paged`,
  `class/table.rs:427`, which `SlabTable::new` proves through
  `lemma_new_geometry`, now ensuring `spp * slab % page == 0`, and
  `lemma_groups_paged`, `:2058`), so `first * slab` is a multiple of the
  group's bytes; the group's slabs lie below the table's `nslabs`, the
  process's (`SlabTable::inv`), and share `slab`'s page group
  (`x >> lg_spp == slab >> lg_spp`), so none is a guard slab
  (`guard_of` reads only the page group) because `slab`, `Empty`, is
  not one (`Inner::slab_core`: an owned guard slab is `Fresh`).
- **Huge purges.** `Huge::is_process` (`huge/mod.rs:4073`, closed): the
  allocator's base, page and page count are the process's huge
  region's, its base a page start, and the regions well formed and
  separated. `Shared::lemma_huge_process` (`heap/shared.rs:2420`) proves
  it from `Shared::wf`; `purge_empty`, `purge_curve`, `decay`,
  `decay_with` and `free_run` require it, and `push_extent` has it
  through the purge tally's invariant (`Tally::inv`), and `purge::purge_all`,
  `purge_arena`, `decay` and `decay_arena` now require `shared.wf()` (every caller has
  it). `Huge::lemma_purge_range` (`huge/mod.rs:4100`): pages `start..start
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
and the exit purge in `Heap::destroy` (verified since package P10,
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
  (`guard_geom`, `class::table::axiom_slot_loc`), as for theorem 7.
- The class lock axioms (`axiom_class_lock_inv`): the value `push_group`
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
- Ghost only: the release library is identical to the base commit's
  (440 of 440 functions, x86_64); aarch64 not compared in this lane.

**Changed since #138.** The statement is unchanged.
`Batch::lemma_discarded` is gone. `Batch::wf` (closed) still keeps
`well_formed` of every range in `discarded()`, but nothing outside the
batch can read it out now (a small weakening of what is exported).
`discard` required `well_formed` of its range in #138 too, so
every call is still checked. `lemma_well_formed` is now private (it was
`pub`).

## 41. Seal targets stay inside challoc's reservations

Every range `Regions::seal` hands `mseal` (`never_committed`'s ranges,
the one place init seals) is non-empty, lies inside one of challoc's
three reservations (the class superblock's, the huge region's or the
metadata region's `mmap`), and shares no byte with any class region,
the huge region, the metadata region or the process's `Shared` static.
So no range `mseal` gets overlaps a region the allocator commits, purges
or reprotects in, or lies outside the address space challoc reserved.
The `file:line`s are the `fn`/`spec` line at this section's commit.

**Statement.**

```rust
// layout/regions.rs:345 (opaque; each proof that needs it reveals it, e.g. :815)
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

// layout/regions.rs:332: inside `superblock_map`, `huge_map` or `meta_map`.
pub open spec fn in_mapping(self, a: int, len: int) -> bool

// layout/regions.rs:882
pub fn never_committed(&self) -> (r: ([Mapping; MAX_SEALED], usize))
    requires
        self.wf(),
    ensures
        r.1 <= MAX_SEALED,
        self.separated() && self.mapped() ==> forall|i: int| 0 <= i < r.1 ==> self.seal_target(#[trigger] r.0[i]),

// layout/regions.rs:2749, :882: before each `mseal(m.addr, m.len, 0)` (:2773).
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
their mappings. `Regions::mapped` (`:314`) is the new reservation
arithmetic: the superblock's `NCLASSES` strides inside `superblock_map`,
the huge region inside `huge_map`, the metadata region inside
`meta_map`, the three mappings pairwise disjoint and none holding a byte
of the `Shared` static. `Regions::from_parts` (`:548`) and
`Regions::reserve_owning` (`:2451`) now ensure it, so `seal`'s one call
(`api::init`, `api/mod.rs:1448`, at `:1494`, on the regions
`reserve_owning` returned at `:1492`; `init_slow` runs `init` once) has
its precondition.

How it is proved:
- **The mappings.** `from_parts` already required each region inside
  its mapping, the mappings pairwise disjoint and apart from the static
  (`reserve_owning` proves these from `align_base`, `slab_multiple`,
  `class_offset` and
  the trusted `Reserved::is_disjoint` and `is_apart_from_static`); it
  now ensures them as `mapped`, with `NCLASSES << lg_stride` as
  `NCLASSES * stride` (`lemma_shl_mul`).
- **The class gaps.** `never_committed`'s loop keeps, for separated and
  mapped regions, `superblock_map.addr <= start <= superblock + c *
  stride`, every class below `c` ending at or before `start`, and
  `seal_target` of every range pushed so far (`push_range`, `layout/regions.rs:1886`,
  now ensures exactly what it appends and that it keeps the earlier
  entries). A gap `start..class_base[c]` lies inside the superblock's
  mapping (`lemma_class_in_strides`, `:376`) and before every class
  from `c` on (`lemma_class_before`, `:358`: class `j`'s region ends
  before class `k`'s starts for `j < k`, each inside its stride);
  `lemma_superblock_gap` (`:803`) concludes, and covers the slack after
  the last class (`k == NCLASSES`). The huge and metadata regions are
  inside their own mappings, disjoint from the superblock's.
- **The side slack.** `lemma_huge_slack` (`:827`) and
  `lemma_meta_slack` (`:847`): a range in the huge (metadata) mapping
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
  checks `Regions::slack_ok` (`layout/regions.rs:2399`: `never_committed`
  returns `MAX_SEALED` ranges, each at least `MAX_SLAB` long) and aborts
  if it fails (`slack_broken`, `:2354`). For the huge and metadata
  regions `reserve_owning` also proves the slack, from `slab_multiple`'s
  lower bound (`:2681`); for the class regions it rests on
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
- Ghost only: the release library is identical to the base commit's
  (414 of 414 functions, x86_64); aarch64 not compared in this lane.

**Changed since #138.** The statement is unchanged.
`lemma_seal_target` is gone: `seal_target` is opaque and each proof
that needs it reveals it. Main adds the slack check above
(wolfi-dev/challoc#438), which #138 does not have; the statement does
not use it.

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
`mallctlnametomib` writes nothing. Upstream #125 made the first two true
(the theorem candidate's counterexamples: `arena.<i>.dirty_decay_ms`
applied the period before a read that then failed, `arenas.create` and
`tcache.create` created before it, and four keys copied out the new
value). The `file:line`s are the `fn`/`spec` line at this section's
commit, all in `api/mallctl.rs` unless named.

**Statement.**

```rust
pub ghost enum Change {                                             // vspec/ctl.rs:17
    Unchanged,
    Set { read_first: bool },   // a setting took the key's new value
    Created,                    // arenas.create, tcache.create
    Refreshed,                  // epoch's write
}

spec fn error_after_change(node: Node, r: Result<(), c_int>, c: Change) -> bool { // :2753
    ||| c is Refreshed
    ||| node.entry matches Entry::Op(Op::BackgroundThread) && r == Err::<(), c_int>(EAGAIN)
}

fn ctl(shared: &Shared, Tracked(mark): Tracked<&'static ReadyMark>, node: &Node, mib: &[usize], io: &Io, // :2770
    Tracked(cc): Tracked<&mut CCall>, Tracked(fx): Tracked<&mut Ghost<Change>>,
    Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: Result<(), c_int>)
    requires ..., crate::api::marks_ready(*mark), shared.wf(), io.handed(*old(cc)), !old(cc).written(),
        resolve(root_nodes(), mib@) == Some(*node),
    ensures
        r is Err && !error_after_change(*node, r, final(fx)@) ==> final(fx)@ is Unchanged,
        io.oldp@.addr != 0 && io.oldlenp@.addr != 0
            ==> (final(fx)@ matches Change::Set { read_first } ==> read_first),

pub open spec fn reads_ok<T>(self, call: CCall) -> bool {          // :1857 (Io)
    self.oldp@.addr == 0 || self.oldlenp@.addr == 0
        || call.len(self.oldlenp as *const usize) == vstd::layout::size_of::<T>()
}
fn read<T: Copy>(&self, v: T, Tracked(call): Tracked<&mut CCall>) -> (r: Result<(), c_int>) // :1961
    ensures
        r matches Err(e) ==> e == EINVAL,
        r is Ok <==> self.reads_ok::<T>(*old(call)),
        self.oldp@.addr != 0 && self.oldlenp@.addr != 0 ==> final(call).written(),
fn verify_read<T: Copy>(&self, Tracked(call): Tracked<&CCall>) -> (r: Result<(), c_int>) // :2042
    ensures
        r matches Err(e) ==> e == EINVAL,
        r is Ok <==> self.reads_ok::<T>(*call),

pub fn mallctlnametomib(name: *const c_char, mibp: *mut usize, miblenp: *mut usize, // :3112
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
cannot fail. `mallctl` and `mallctlbymib` (`api/mallctl.rs:2997`, `:3182`) pass
`ctl`'s result unchanged (`result`: `Ok` is 0, `Err(e)` is `e`).

- **Reads write at most `*oldlenp` bytes.** The writes through `oldp`
  are `copy_prefix` (`:1894`: `n <= oldlen`, `n = min(oldlen,
  size_of::<T>())`, through `ptr::copy_nonoverlapping`), `store`
  (`:1921`: `oldlen == size_of::<T>()`, through `write_unaligned`) and
  `batch_query`'s triples (each inside `oldlen == 24 * n`), with
  `oldlen` the value `old_len` (`:1869`) read from `*oldlenp`: their
  `requires` (`c_buffer(oldp, oldlen)`, `c_writes`) bound every byte
  written to the caller's `oldlen`. `read` copies out only when the
  caller asked (`oldp` and `oldlenp` non-null), and fails `EINVAL`
  after the prefix copy when the sizes differ.
- **`mallctlnametomib` writes nothing on failure.** Its writes go
  through `mib_out` (`:3074`), which needs `c_writes` of `mibp` and
  `miblenp`; only `open_writes` gives those, and it runs after every
  check (`cstr`, the null tests, `name_to_mib`, the capacity), so a
  nonzero result leaves the token unwritten. The export wrapper now
  passes the token by `&mut` (`let Tracked(mut cc) = nametomib_call(..)`),
  so the ensures can name its final state.

**Covers.** `mallctl` and `mallctlbymib` for every key `ctl` performs
(`Op` arms; a `Read` key is read-only and changes nothing), and
`mallctlnametomib`.

**Premises beyond the common ones.**
- `CCall::sized` and `CCall::counted` (`vspec/cbuf.rs:494`, `:512`,
  trusted, the C contract of a sized buffer and a counted array) now
  also ensure that the `size_t` read holds the token's view
  `CCall::len(lenp)` (`:411`): C does not change `*oldlenp` (or
  `*miblenp`) during the call before challoc writes, as for the
  string argument's view. This is what makes `verify_read`'s check the
  one `read` makes. No other trusted item is added.
- The changes `fx` records are the calls named above, by inspection of
  `ctl`'s arms and those callees: each callee's `ensures` states what it
  records, but the theorem does not prove that `request`, `set_decay_ms`
  and the rest change state, nor that no other call in an arm does (see
  Scope).

**Scope.**
- "Allocator state" here is the settings and objects the keys write:
  `background_thread`'s request (since upstream #111 the write also
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
  comes before the request (#125).
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
  `Tracked` parameters, erased in the build (the release library is
  identical to the third port's, x86_64). The upstream tests
  `contract_*` (`api::mallctl::tests`) check the behaviour at run time.

**Changed since #138.** The ensures are unchanged, and `ctl` requires
`resolve(root_nodes(), mib@) == Some(*node)` as in #138 (theorem 21).
It gains the `ReadyMark` and `errno` parameters and `requires
marks_ready(*mark)`. `Change` moved to `vspec/ctl.rs`, and each change
is recorded by the callee that makes it (whose `ensures` states the
record), not by a ghost assignment in `ctl`'s arm. The `mallctl` and
`mallctlbymib` C exports (`api/mallctl.rs:3460`, `:3488`) are verified,
as in #138: each mints its call's token with `axiom_c_call` from its own
parameters and lends `errno` (`vspec::errno::lend`), so Verus checks
that the C symbols reach the verified `mallctl` and `mallctlbymib` with
a fresh, unwritten token (wolfi-dev/challoc#424).

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
then returns NULL in every state. Upstream #126 and #127 made both true
(the candidate's counterexamples: a sized free of a huge run ignored
the size, and `nallocx(1, MALLOCX_LG_ALIGN(63))` was a page). The
`file:line`s are the `fn`/`spec` line at this section's commit.

**Statement.**

```rust
// heap/mod.rs:514, :529; the sized frees' cores (api/libc.rs:829
// `free_sized_block`, api/jemalloc.rs:690 `sdallocx_block`) ensure both
// for C's token `blk` (the free paths' `hint_ok` is theorem "Bad input")
pub open spec fn hint_ok(p: *mut u8, hint: Option<usize>) -> bool
pub open spec fn huge_hint_ok(blk: Live, p: *mut u8, size: usize) -> bool {
    blk.claims(p) && !slot_start(p) && process_regions().is_huge_block(p@.addr, 1) ==> {
        &&& size + live::page_size() - 1 <= usize::MAX
        &&& crate::vspec::zero::huge_usable(size as int) == blk.len()
    }
}

// api/jemalloc.rs:892, :911
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

// vspec/zero.rs:228, :259
pub open spec fn run_placeable(base: int, pg: int, np: int, size: int, align: int) -> bool {
    &&& size + pg - 1 <= usize::MAX
    &&& align <= np * pg
    &&& exists|q: int| 0 <= q && q + run_need(pg, size) <= np && #[trigger] page_aligned(base, pg, q, align)
}
pub open spec fn huge_placeable(size: usize, align: usize) -> bool  // the process's huge region

// api/jemalloc.rs:215, :828, :867
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

// huge/mod.rs:4455, :4587
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
  `Huge::placeable` (`huge/mod.rs:4410`) ensures it is
  `spec_placeable`, `run_placeable` over the huge allocator's own
  region (`Shared::lemma_huge_placeable`, `heap/shared.rs:2568`, ties
  the two), and `theorem_placeable_fits_empty` gives a fit
  (`Huge::fits`) under a lock value with nothing placed, which `find`
  (`huge/mod.rs:4965`) then returns (its `None` means no page fits). A
  run `alloc_run` places in any state was placeable (its start is an
  aligned page the run and its guard fit from), and the
  `alloc_huge` layers carry that up to `mallocx`.

How it is proved:
- `Huge::run_pages` (`huge/mod.rs:4365`, #126's shared check) ensures
  its result is `run_need(page, size)` and that `None` means not
  placeable; `placeable` compares the first aligned page `align_page(0,
  align)` (which ensures no lower page is aligned) plus the run's pages
  with the region: any placement starts at or after it.
- `check_size_hint` (`heap/shared.rs:3221`): for a huge page start it
  looks the run up with the caller's run token (`Huge::lookup`,
  `huge/mod.rs:7379`, now ensures that with a token of a run starting
  at `page` the result is that run's `Active` descriptor with the
  token's usable bytes: the token's extent is in the lock's run
  authority, which names one `Active` descriptor starting there, and
  the table keys it at `page`) and aborts (`huge_hint_mismatch`,
  `heap/shared.rs:3855`) unless `Huge::usable_for(size)` is the run's
  usable size. `Regions::classify` (`layout/regions.rs:1120`) now also
  ensures that with separated regions every huge page start is `Huge`
  (a hit of `in_superblock` lies in the strides), so no run is passed over.
- The fast path (`free_sized_fast`, `heap/paths.rs:991`) now ensures a
  push is a slot start's, and a declined push keeps the token's claim
  and length, so `huge_hint_ok` of the token reaches the cores.
- Since upstream #101 `free_sized` claims C's token itself and hands it
  to `free_sized_with` (also `sdallocx_block`'s no-flags branch), which
  passes it to `free_sized_block`, or, above `SMALL_MAX`, to
  `free_sized_medium`, which does; both ensure `huge_hint_ok` of the
  token as `free_sized_block` does, so the statement covers the new
  sized path (the huge sizes are above `SMALL_MAX` and reach
  `check_size_hint` through `free_sized_medium`).

**Premises beyond the common ones.** None new: the run token and
`axiom_c_claim` (the C contract of a live block, "Bad input"), and the
lock invariant of the huge allocator (theorems 23 and 29); `check_size_hint`
is a new root of the lock-order premise (`axiom_entry`, theorem 33: the
sized frees' slow paths hold no lock), used, not added.

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
  which the exports mint with `axiom_c_call`, `axiom_c_block` in #138: `free_sized`,
  `free_aligned_sized`, `sdallocx`) state only `hint_ok`. A freed or
  foreign run is excluded by the C contract, as
  for every free (*(C)*).
- "Rejects" is about the size and alignment: a non-zero `nallocx` does
  not promise success (the region may be full, metadata or the kernel
  may refuse, an explicit arena or tcache may not exist), and the class
  path's failures are of that kind. "Empty region" is a lock value whose
  mark is page 0; that `find` returns a placement then is its `ensures`
  (no `None` while a page fits), not a run of `alloc_run`.
- Ghost only, beyond the three ports' exec changes.

**Changed since #138.** The statements are unchanged; the exports
mint C's token with `axiom_c_call` (`axiom_c_block` in #138), and
the sized frees, `mallocx` and `theorem_nallocx_zero_rejects` take the
thread's `errno` token.

## 44. Valid programs never abort (two tranches: the frees after init, through the caches or none, sized or not, and the lookups of `realloc` and `malloc_usable_size`, up to resource failures and named pending checks)

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
pending checks, and that the trusted bodies on these paths abort only
as their capability says, rests on reading the code (below): on main
no source test checks the two sinks (`sys::tests::abort_grants_only_at_roots`,
`sys/mod.rs:1668`, checks only where `grant_aborts` is called). Covered, after init (`shared.wf()`; the prefix before it is
not): the free through no cache (`heap::free_direct`: a thread with no
heap, `MALLOCX_TCACHE_NONE`), slot and run (first tranche); a free on a
heap (`Heap::free`: the push onto the home or the `FOREIGN` cache, the
slow path with its tick, cache GC, decay walks and purges, the arena
it may build and the cache it may create, the flush and its release,
the move of a young heap's home), and a sized free (`free_sized`,
`sdallocx`, `free_aligned_sized`) of a size that names the block, on a
heap or none (second tranche); and the lookups of the block that
`malloc_usable_size` and `realloc` make (second tranche). The
`file:line`s are the `fn` line at this section's commit.

**Statement** (`sys/mod.rs:173`, `:177`, `:188`, `:193`, `:202`;
`sys/abort.rs:181`, `:195`; `heap/paths.rs:1255`, `:1278`, `:1320`,
`:1353`, `:1395`; `api/libc.rs:1715`):

```rust
pub uninterp spec fn kernel_aborts() -> bool;
pub open spec fn may_abort_kernel() -> bool { may_abort() || kernel_aborts() }

pub uninterp spec fn pending_aborts() -> bool;
pub open spec fn may_abort_pending() -> bool { may_abort() || pending_aborts() }

pub open spec fn valid_aborts() -> bool { kernel_aborts() && pending_aborts() }

#[verifier::external_body]
pub fn abort_kernel(args: fmt::Arguments<'_>) -> !
    requires
        crate::sys::may_abort_kernel(),

#[verifier::external_body]
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
uninterpreted, granted by nothing, each accepted only by its trusted sink
(`abort_kernel`, `abort_pending`: `abort_fmt` under the narrower
`may_abort_kernel()` or `may_abort_pending()`, as `abort_integrity` is
under `may_abort_corrupt()`). The witnesses hold the two, not
`may_abort()`, and verify. The two direct-free witnesses
(`theorem_valid_free_never_aborts`,
`theorem_valid_sized_direct_free_never_aborts`) also hold
`integrity_aborts()` on main (#373): `free_direct` reads the thread's TLS
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
`Arena::init`, `alloc_sets`; in #138 these required
`may_abort() || valid_aborts()`). Every other abort it reaches needs `may_abort()`,
which the witnesses lack, so each is proved unreachable with the token,
or it is a resource failure (`abort_kernel`) or a pending check
(`abort_pending`). So for a live block's
token, whatever the allocator's state that the proofs' invariants
allow, these cannot fire:

- **Not an allocation** (`Shared::classify_free`, `heap/shared.rs:2976`,
  `invalid_free`; `rehome_on_free`'s own `invalid_free` after the move,
  `heap/mod.rs:5933`; and the lookups' `not_an_allocation` in
  `usable_size`, `api/libc.rs:1121`, and `old_size_or_keep`'s and
  `realloc_old`'s, `:1534`, `:2352`): a live block is a slot start or a
  page start of the huge region (`Live::lemma_block_target`,
  `heap/live.rs:543`), which `Regions::classify` never calls `Invalid`.
- **Slot not live, guard slab** (`SlabTable::check_live`,
  `class/table.rs:3661`, `not_live`: `invalid_free` or `guard_free`):
  the bit test `is_set_unchecked` (`:3116`) reads the word through
  `bit_word_held` (`:3158`) with the caller's slot token
  (`Live::slot_ref`, `heap/live.rs:504`; `holds_slot`,
  `class/table.rs:649`). Inside the word's invariant, which holds the
  tokens of exactly its clear bits (`bitmap::WordPred`), the token is
  disjoint from the word's (`GhostSubset::disjoint_singleton`), so the
  bit is set. `classify_free`, and now the lookups (`usable_size`,
  `old_size_or_keep`, `realloc_old`), hand `check_live` the token of the
  live slot at the address `classify` located
  (`Live::lemma_block_slot_addr`, `heap/live.rs:516`: a live block at a
  slot address of class `c`, a guard slab's too, is the slot of class
  `c`, so `is_slot`; for it `vspec::zero::lemma_slot_unique` takes slot
  addresses).
- **Not a slot start, outside the class region** (`SlabTable::locate`,
  `class/table.rs:3521`, and `ClassState::locate`, `class/mod.rs:2455`,
  `invalid_pointer`): neither fires for a slot address
  (`Regions::lemma_slot_addr_found`); both require `may_abort()` or that
  (with the regions well formed), and need no capability then.
  `Shared::arena_of_slot` (`heap/shared.rs:3303`) requires `may_abort()`
  or a slot address with `valid_aborts()`.
- **Released through another arena** (`ClassState::release_with`,
  `class/mod.rs:4984`, `wrong_arena`): `free_direct` passes the owner
  `classify_free` read (`slot_owner`), so the batch is `owned_slots`
  (`class/mod.rs:4470`) and `release_owned` leaves no slot of another
  arena. `release_routed` (`heap/shared.rs:3352`) follows the owner
  words, and its "changed arena" abort was unreachable already.
- **Guard slab, double free in the batch** (`release_owned`,
  `class/mod.rs:5047`: `guard_free`, the scratch bit's `double_free`):
  a live slot is in no guard slab (`live_slots`, `:4464`; theorem 7),
  and a set scratch bit is a slot whose token the batch's accumulated
  tokens hold, disjoint from the one split off for this pointer (for
  every caller: the batch's tokens are distinct). A flush's batch is
  `live_slots`: every cached entry holds a slot of its header's class
  (`lemma_released`, `heap/flush.rs:862`).
- **Double free at the word** (`release_word`, `class/mod.rs:4669`,
  `double_free`): `AtomicBitmap::release` ensures the released bits
  were set (their tokens were not in the word), so the check fails for
  no caller.
- **Double free in the cache** (`Heap::scan_or_abort`,
  `heap/paths.rs:655`, `cached_twice`, `double_free_scan`'s compare in
  `free_slow_in` and `rehome_on_free`): no entry of the cache stands for
  `p` while the free holds `p`'s slot token: each entry holds its own
  slot's token (`CachePerms::tokened`), of the same authority, so of
  another slot (`Heap::lemma_not_cached`, `heap/cache.rs:790`, by
  `Live::lemma_slot_apart`); and a hit of `cached_recently`
  (`heap/cache.rs:740`, trusted, whose contract now states the hit, by
  address) is an entry standing for `p` (`lemma_plain_stored`).
- **A straddling pair at the flush** (`Heap::flush`,
  `heap/flush.rs:380`, `straddling_double_free`): the lowest released
  address is a released entry's and `kept_max` a kept entry's
  (`lemma_kept_index`, `:827`, follows it through the kept sort), and
  the cache's entries stand for distinct slots (their tokens are
  distinct), so the compare fails for every caller (`lemma_released`).
- **A size hint that does not name the block**
  (`Shared::check_size_hint`, `heap/shared.rs:3221`, `hint_mismatch` and
  `huge_hint_mismatch`): with the sized free's contract (the premise
  below) the hint names `p`'s class and `size` rounds to the run's
  pages.
- **Re-keying a cache that still holds slots** (`CacheHeader::rekey`,
  `heap/cache.rs:173`, the trusted `assert_rekey_flushed`,
  `heap/mod.rs:4527`): `try_rehome` (`heap/mod.rs:4955`) re-keys the
  home headers after the home flush emptied them and the `FOREIGN`
  flush kept them.
- **Not a run, already free** (`Huge::free_run`, `huge/mod.rs:6794`, two
  `not_a_run` and `already_free`; `Huge::usable_size`, `:7289`, which
  `zero_on_free`'s `Shared::zero_huge`, `heap/shared.rs:3095`, and the
  lookups reach): the run token's extent is in the lock's run
  authority, which names the `Active` descriptor that starts at `page`,
  keyed there in the table (as `Huge::lookup` shows it, theorem 43).
  `free_run` always holds the token, so its three checks fail for no
  caller (`assert(false)`, as theorem 29's `Inner::extent_at`);
  `usable_size` holds it when its caller does (`holds_run`,
  `huge/mod.rs:1049`). With that, the abort stand-in `not_a_run_page` is
  gone (common premises, "Abort stand-in"): its site is unreachable, so
  Verus checks the build's `not_a_run(self.addr(page))` there itself.
- **A run state byte that is no state** (`HugeDesc::state` in
  `Inner::free_before`, `free_after`, `retire`, `huge/mod.rs:3712`,
  `:3741`, `:3421`, `bad_run_state_byte`): every live descriptor's byte
  is a state (`desc_ok`; `states_ok`, `:905`).
- **An empty count below zero on a purge** (`ClassState::settle`,
  `class/decay.rs`, `slab_count_underflow` in its `unwrap_or_else`
  closure, now `requires false`): the slab it purges is `Empty`, so in
  the empty set (`slab_core`), whose size the count is (`Inner::base`).
- **An extent count below zero** (`huge_count_underflow`,
  `huge/mod.rs:434`, through `HugeStats::sub` in `drop_extent`, now
  `requires false`): `sub` requires the extent counted (`can_sub`).
- `Arena::alloc_sets` (`arena.rs:877`) has no size check on main: the
  table fits an object by `global layout SetTable` (`arena.rs:571`,
  which Verus checks against the compiler's), and it requires no abort
  capability.

The resource failures the covered paths reach (`abort_kernel`; its
callers are read from the code):
- `Huge::free_run`'s `madvise(MADV_GUARD_REMOVE)` of the run's guard,
  with `guard_install`.
- `purge::discard`, the `madvise` of the purged pages: of a huge run
  purged at once (above `huge_retain_max`, or under
  `huge_dirty_decay_ms:0`), and of the slabs and extents the tick's
  decay walks purge (second tranche); `discard` requires
  `may_abort_kernel()` for it. Since upstream 7a12536a it aborts only
  on an errno other than `EINVAL`: `madvise` fails with `EINVAL` on
  pages the program locked (`mlock`, `mlockall(MCL_FUTURE)`), and the
  range now stays `Empty` and dirty, to be retried, so a valid program
  that locks its memory no longer aborts its free here. What is left,
  `ENOMEM` (part of one of the allocator's reservations unmapped) or
  `EPERM` (the program sealed the allocator's free memory), is a
  program that touched memory it does not own; that a valid program
  reaches neither is read from the errno list, not proved (`madvise`'s
  errors have no specification). The page-alignment check before the
  call (upstream's `assert!` with `sys::features::page_size()`) is
  proved never to fail, from `well_formed` and the premise that
  `getauxval(AT_PAGESZ)` is the process regions' page (S208/S209, the
  `getauxval` specification; init reserves the regions with it), which
  also makes `page_size`'s own abort unreachable, so `page_size` no
  longer requires `may_abort()`.
- `huge::table::Table::insert`'s "descriptor table full"
  (`huge/table.rs:742`): `free_run`'s `ensure` is best effort, so once
  the table's growth found no metadata (the reservation exhausted, or
  the kernel refusing to commit), frees fill it. A valid program
  reaches it under metadata exhaustion; that it is unreachable after an
  `ensure` that grew the table is not proved.
Metadata the arena build, the cache stack and the arena's set table
cannot get is no abort: `Shared::arena` and `ensure_cache` return none,
and the free goes straight to the slab.

The pending checks the covered paths reach (`abort_pending`, which
requires `may_abort_pending()`; the helpers' other callers
require `may_abort()`, as before). Each tests
state no verified invariant constrains as the check needs; each fires on an
invalid free, and on a valid one only if that state is corrupt:
- `class::abort::never_committed` (`class/abort.rs:17`), through
  `SlabTable::check_live`, `desc`, `owner` and `bits`: the slab at or
  above its class's watermark. A live slot's slab was handed out, but
  no invariant ties the watermark to the tokens.
- `owner_mismatch` and `release_into_fresh_slab` (`class/abort.rs:31`,
  `:89`), in `release_owned`: the descriptor's arena against the owner
  word, and a `Fresh` slab (one nothing was claimed from).
- `SlabDesc::sub_live` (`slab.rs:500`), through
  `dec_live`: the live count would go below zero.
- `slab::bad_state_byte` (`slab.rs:45`), through `State::from_u8`,
  `SlabDesc::state` and `state_held` (`ClassState::state_locked`,
  `class/mod.rs:2518`: the class lock holder's reads in
  `reopen`, `on_live_zero` and the decay walks): a slab state byte that
  is no state.
- `heap::shared::no_such_arena` (`heap/shared.rs:3826`), through
  `Shared::owner`: the owner word names an arena that was never built.
  (`alloc_huge`'s call, of its caller's arena index, holds `may_abort()`;
  on the free paths `ensure_cache` reads the home arena's and `try_rehome`
  the one an owner word names.)
- The `check_zero` debug mode's `shadow_written` and `release_into_fresh`
  (`class/verify.rs:82`, `:237`), through `check_shadow` and
  `dec_live`: a descriptor written outside its API, a release into a
  slab that reads `Fresh`.
- Second tranche: `ClassState::walk_empty`'s "slab in the empty set is
  not empty" (`class/decay.rs:487`, the tick's decay walk): a slab of
  the empty set whose descriptor is not `Empty` with no live slot (the
  walk reads the state byte raw, not through `state_locked`, and no
  invariant relates the live count to the sets); and
  `arena::unbind_unbound` (`arena.rs:2489`), through
  `Arenas::unbind` in `try_rehome`: the heap holds its arena's ticket,
  so the bound count reads the tickets out as a `u32` wraps it, and
  reads 0 only with a multiple of `2^32` heaps homed there, which no
  verified invariant bounds.

On main no source test checks these capabilities:
`sys::tests::abort_grants_only_at_roots` (`sys/mod.rs:1668`) checks only
where `grant_aborts` is called. That `kernel_aborts` and
`pending_aborts` appear only in their definitions, in `valid_aborts`'
and in the witnesses' `requires`, and that `abort_kernel` and
`abort_pending` are called only in the functions listed above, is read
from the code (in #138 `api::tests::abort_grants_only_at_roots`
checked it).

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
- `abort_kernel` and `abort_pending` (`sys/abort.rs:181`, `:195`, S207,
  S208, `external_body`; discharged by Kani in #138, partly
  checked on main: the ledger leaves the capability narrowing
  unchecked): the relabels of
  `abort_fmt`'s capability, as `abort_integrity`'s (S206); their
  harnesses (`abort_kernel_reaches_abort_fmt`,
  `abort_pending_reaches_abort_fmt`, `src/kani/abort.rs`) show each
  body reaches `abort_fmt` with nothing failing on the way, and the
  relabel itself is this theorem's policy (the records' residual).
- `Heap::cached_recently` (`heap/cache.rs:740`, S112, discharged by
  Kani in main's ledger): its contract now ensures that a hit is one of the `len`
  entries whose address is `e`'s, read off its body (the four compares
  `*top.sub(k) == e`, `k` in `1..=4`, or the iterator over `1..=len`,
  each an entry below `len`; pointer `==` compares addresses).
- The trusted bodies on these paths abort only as their capability says
  (read off the code; `owner_locked`, verified,
  requires `may_abort_pending()` for its call of `SlabTable::owner`).
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
  against the registry, and the arena bound counts below `2^32`.
- Resource failures: they are the statement's exception (above).

**Where the statement is weaker than the code.**
- The witnesses replicate the calls `free` and the sized frees make
  after their `shared_if_ready` (`free_direct`; `Heap::free` on the
  thread's or an explicit cache's heap; `free_sized_fast`,
  `check_size_hint`, then `Heap::free` or `free_direct`), and
  `Heap::free`'s huge branch,
  not those functions, as theorem 32's do.
- "Never aborts" is about the verified source, as in theorem 32.

Machine code: the x86_64 disassembly of the release library is
identical to the base commit's (4f75a5b7 for the first tranche, 9aca3bad
for the second), function for function. The first tranche's exec
changes are the abort sinks (`abort_fmt` to `abort_pending` or
`abort_kernel`, both `#[inline(always)]` calls of `abort_fmt`), the
match arm of `classify_free`'s `Invalid` made a block, a `&Inner` borrow
of the huge lock guard moved (`usable_size`) or added (`free_run`) for
the token proof, ghost (`Tracked`) parameters on `classify_free`,
`check_located`, `check_live`, `is_set_unchecked` and `bit_word_held`,
and `bit_word` as a call of `bit_word_held` with no token. The second
tranche's: two more sinks relabelled (`walk_empty`, `unbind_unbound`
to `abort_pending`), the
`Invalid` arms of `usable_size`, `old_size_or_keep`, `realloc_old` and
`rehome_on_free`'s `classify_free` made blocks (proof and the same
call), the `unwrap_or_else` closure in `purge_group` (now in `ClassState::settle`) given a return
type and `requires false`; no exec parameter added (the cache entries' disjointness is a
precondition `free_slow_in` and `rehome_on_free` prove, not a token
passed to `scan_or_abort`).

**Changed since #138.** `kernel_aborts()` and `pending_aborts()`
are `uninterp` on main (opaque `true` definitions in #138; still
granted by nothing). The two direct-free witnesses add the
`integrity_aborts()` premise (#373: a corrupt TLS slot on the direct
path stays an integrity abort), so for a thread with no cache the
theorem now excludes the not-an-allocation, invalid-free and
double-free checks under that extra capability. The cache and sized
witnesses take the thread's `errno` token.
Weaker on main: the source test is now
`sys::tests::abort_grants_only_at_roots`, which checks only
`grant_aborts`, no longer the two capabilities or their sinks.
Stronger: `huge_count_underflow` requires `false`; `alloc_sets` has no
integrity check; `Table::ensure`, `Arenas::get`, `materialise`, `build`
and `Arena::init` never abort; `Arenas::unbind` requires
`may_abort_pending()` alone; `SlabDesc::sub_live`, `external_body` in
#138, is verified (wolfi-dev/challoc#402). `slab::bad_state_byte` and
the slab state reads require `may_abort_pending()`, as in #138
(wolfi-dev/challoc#445).

## 45. Fast-path cost bounds (machine level, pinned build)

**Changed since #138.** Main's tables (`verus/perf-budget.txt`,
`verus/perf-budget-q8.txt`) are measured with rustc 1.98.1 at
`98790244`, #138's with rustc 1.95.0, and the `perf-budget` check
(`.chainguard/ci/perf-budget.yaml`) runs `scripts/perf-budget.sh` at
both quanta on every pull request. `malloc`'s row and the rows with a
fixed cell are #138's. The "any" rows of the frees are lower, after the
double-free scan's rewrite (wolfi-dev/challoc#447, which drops the
top-four scan) and later changes: #138 has `free` 85/25/2/20/11 over 16
fast paths, `free_sized` 61/16/2/15/9 over 7, `sdallocx` 63/16/2/16/9
over 7 and `challoc_free_usable` 91/26/2/21/12 over 17 (insns, loads,
stores, conditional branches, lines). aarch64 stays open work
(wolfi-dev/challoc#397).

On the pinned x86_64 release build, every path through the fast branch
of `malloc`, `free`, `free_sized`, `sdallocx` and `challoc_free_usable`
executes at most the table's instructions, explicit memory loads,
explicit memory stores and conditional branches, and touches at most the
table's 64-byte cache lines (P1, P4); no such path has a back-edge (P3);
and no export's whole body holds an atomic read-modify-write, a fence, a
call, a system call or an indirect jump (P2). This is a statement about
one binary, checked by a CFG walker over its disassembly
(`scripts/perf-budget.rs`), not by Verus; it gives no cycle counts.

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
  counted either; the bodies have none today. An edge anywhere else out
  of the body fails the check.
- **P2, no synchronisation or kernel entry.** Over the whole body of
  each export (fast and slow-entry branches, from the symbol's address
  to address + size in the dynamic symbol table): no `lock` prefix, no
  `xchg`, `xadd` or `cmpxchg*`, no `mfence`/`lfence`/`sfence`, no
  `call`, no `syscall`/`sysenter`/`int N`, no indirect jump. The source
  atomics on these paths are all `Relaxed` loads, which compile to plain
  `mov`s: `free`'s watermark copy (`class::load_mirror`), bitmap word
  (`SlabTable::is_set_unchecked`), `Shared::free_flags`
  (`heap/shared.rs:3501`) and, routed, the owner word
  (`class::load_owner`, `class/table.rs:907`). `malloc`, `free_sized` and
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
  `heap/cache.rs:51`, `:95`), so `Heap` is 64-byte aligned and
  `caches[c]` is `heap + 128 + 64 * c`; `ClassLine` is
  `repr(C, align(64))` and `CLASS_STRIDE` (64) bytes, at offset 0 of
  `Shared` (`heap/shared.rs:367`, the `const` asserts after it);
  `SMALL_LOOKUP`'s entries are multiples of `CLASS_STRIDE`
  (`small_entry_ok`, `layout/mod.rs:1656`, proved).

**Measured table** (`verus/perf-budget.txt`; rustc 1.98.1, `quantum16`,
`cargo build --release --locked`, on main at `98790244`). `-` is every
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

Read at the commit that adds this section (on the integration tip
`d161c911`); the `file:line`s are the `fn` line there unless marked
as a call.

**Statement** (`vspec/user.rs:42`, `:53`; `purge.rs:220`;
`api/libc.rs:754`):

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

- `UserMem::lemma_valid_reads` (`vspec/user.rs:143`), the one way
  verified code outside `vspec::user` gets a read bound (`valid_reads`)
  on bytes a `UserMem` covers;
- `copy_user` (`vspec/user.rs:304`), the `copy_nonoverlapping` out of
  the old block;
- and, through them, `class::verify::read_word` (`class/verify.rs:145`,
  a slot's word), `Huge::read_run_word` (`huge/mod.rs:4918`, a run's
  word, through the permission the hand-out will mint) and
  `libc::copy_moved` (`api/libc.rs:1578`, realloc's copy over the two
  blocks' `Live` tokens). The `check_zero` walks that call the word
  reads require it too: `ClassState::check_zero` and
  `check_fresh_slab_is_zero` (`class/verify.rs:295`, `:427`), and
  `Huge::check_zero` (`huge/mod.rs:4768`).

It is granted in two places:

- the realloc cores `libc::realloc_block` (`api/libc.rs:1613`),
  `libc::realloc_usable_stored` (`:2182`) and `jemalloc::rallocx_zeroed`
  (`api/jemalloc.rs:477`), in the proof block just before their
  `copy_moved`. By then the move is decided: the keep test
  (`old_size_or_keep`, `realloc_old`) and the new allocation read no
  byte of `p`.
- `purge::check_zero()`, whose `true` (the `check_zero` config key is
  on) grants it. Its callers that read are the class claim's pick loop
  (`ClassState::finish`, `class/mod.rs:3007`, call at `:3053`, under a
  loop invariant `check_zero ==> may_read_user()`), the class take's
  `Fresh`-slab check (`ClassState::take`, `:4015`, call at `:4085`) and
  the huge allocation (`Huge::alloc_aligned`, `huge/mod.rs:4615`, call
  at `:4708`, under the `prior` that is `Some` only when `check_zero()`
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
  of a `PointsToRaw` (`axiom_raw_valid` is gone since
  wolfi-dev/challoc#400), from `vspec::cbuf`'s facts about a C argument, or
  from `vspec::ptr::ref_ptr` (a Rust reference, which verified code makes
  only through a permission).
- The only permissions to a user block's bytes are `UserMem`s. They are
  minted at hand-out (`heap::live::axiom_hand_out`, `heap/live.rs:663`),
  at the C boundary (`axiom_c_claim`, `:913`) and, lent for reads, to a
  `Fresh` slab's slots (`class::axiom_fresh_slab_mem`,
  `class/mod.rs:3584`, whose one caller is the `check_zero` branch of
  `take`). This is theorem 4's premise "no `PointsToRaw` is ever made
  for the class or huge regions but there". Every other permission
  verified code holds is to metadata, apart from every block (theorem 4),
  or to the `Shared` static.
- A `UserMem`'s `PointsToRaw` is a private field of `vspec::user`, so
  only that module turns it into a bound. On main the read bound is
  `lemma_valid_reads` (`vspec/user.rs:143`, through
  `vspec::ptr::axiom_raw_readable`), which requires the capability, and
  the private `lemma_raw_range` (`:122`) gives the permission's raw
  range to `zero_raw` and `copy_raw`. Its callers are `zero_user`
  (`:241`), whose body is one `write_bytes` and reads nothing, and
  `copy_user` (`:304`), which requires the capability. In #138 the
  module called `axiom_raw_valid` in `lemma_valid_reads` and in a
  private `lemma_valid`, a write bound.
- `may_read_user` is opaque. A verified function can prove it only
  from its own `requires` or a loop invariant that carries it in from
  there, from `grant_user_reads`, or from a `true` that
  `purge::check_zero()` returned.
  `sys::tests::user_read_grants_only_in_listed_callers`
  (`sys/mod.rs:1687`) checks the source: each of the four grant sites
  calls `grant_user_reads` exactly once, and nothing else calls it. At
  #138 its predecessor, `api::tests::user_reads_only_at_grants`,
  also checked that `may_read_user` appears in an `ensures` only as
  `check_zero`'s `r ==> may_read_user()` and in the grant's own
  `ensures`, and otherwise only as a `requires` or loop invariant clause
  that ends with it, apart from its definition and the grant's
  `reveal`; main does not check that (wolfi-dev/challoc#397).

**Premises beyond the common ones.**
- Theorem 4's: metadata permissions lie apart from every block, and
  the mints (`axiom_hand_out`, `axiom_c_claim`,
  `axiom_fresh_slab_mem`) are the only sources of a permission to the
  class and huge regions.
- The trusted bodies read no user block. This is read from the code,
  not proved (a sweep of every `external_body` body and of the code
  outside `verus!`, at this section's commit). The trusted bodies the
  allocator runs (`verus/SEAMS.md`) load:
  - metadata: the cache stacks through `cached_recently`, `others`
    nodes, `arena_volatile`, descriptor
    and watermark loads, the load wrappers of `vspec::word`, `count`,
    `entry` and `atom`, and `slice_from_perms`'s bitmap words;
  - the statics: `ClassLine::guard_word`, the class
    table, the feature line, `malloc_message`, `malloc_conf`;
  - TLS (`sys::tls::get`, `get_tp`, `errno`);
  - the rseq area (`rseq::read_cpu_id`) and the loader's rseq globals;
  - the environment (`config::linux::environ_array` and `read_slot`,
    and `byte_at` through `cstr`; `env_var`, which walks it through
    them, is verified since wolfi-dev/challoc#413).
  Kernel files are read through verified wrappers over
  `raw::syscall*`. The C arguments of `mallctl` are read by verified
  code. `Heap::destroy` is verified on main (`heap/mod.rs:5539`); its
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
- No new trusted item: the capability, its grant, `check_zero`'s
  contract and the witness are verified.

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

**Changed since #138.** The statement is unchanged; its source
guard is narrower. `sys::tests::user_read_grants_only_in_listed_callers`
checks the 4 grant sites, as `user_reads_only_at_grants` did, but not
where `may_read_user` appears in an `ensures` (wolfi-dev/challoc#397).
`may_read_user` is `closed` (opaque `true` in both), and the module's
private write bound `lemma_valid` became `lemma_raw_range`, and
`lemma_valid_reads` takes its bound from `axiom_raw_readable` instead of
`axiom_raw_valid`.

## Thread exit never reuses a destroyed heap (main only; rests on the slot premise)

Not numbered: #138 does not have it. The `file:line`s point into main
at this section's commit. A thread's heap is freed by its thread-exit
key destructor (`heap_destructor`); this states that the thread's TLS
slot never names it again and that the thread never builds a second
cache, the shape of glibc BZ 22111 and BZ 28028 (thread exit rebuilt
the tcache after shutdown and leaked it).

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
// heap/thread.rs:57, :70, :93, :100
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

// sys/tls.rs:424 (trusted asm; the requires binds its verified callers)
pub fn set(value: *mut c_void, Tracked(slot): Tracked<&mut TlsCell>)
    requires crate::heap::slot_step(old(slot).value(), value as usize),
    ensures final(slot).set_to(*old(slot), value as usize),

// heap/thread.rs:256, in heap_destructor after the store, before Heap::destroy
assert(tls.value() == TLS_EXITED && tls.loan.heap() is None && slot_ok(tls.cell, tls.loan));

// heap/thread.rs:509 (verification only, cfg(verus_keep_ghost))
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
`Exited`), `create_heap_for_thread` (`heap/thread.rs:370`: empty to
`Creating` after `Heap::create`; `Creating` to the heap value, or to
null when the thread-exit key cannot take the heap), and the background
thread's `create` (`purge/background.rs:507`: a heap value parked on
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
- The slot premise (`TlsSlot::entry`, `sys/tls.rs:310`): each top-level
  user's token holds the slot as the last verified writer left it.
  Ghost state does not cross from one C call to the next, so the step
  from "`heap_destructor` left `TLS_EXITED`" to "every later entry's
  token holds `TLS_EXITED`" is this premise plus `lemma_exited_final`
  at every write in between; Verus checks each write, not the sequence.
- One token per user: two `TlsSlot::entry` calls in one verified
  function would give two tokens, and a write through a stale one would
  be checked against a value the slot no longer holds. The source test
  `sys::tests::tls_entries_only_in_listed_users` (`sys/mod.rs:1719`)
  checks that only the listed users call it, once each (twice in
  `heap::mallocx::alloc` and in `theorem_no_syscall_on_fast_paths`, one
  per `match` arm). Not checked: that a nested user (the allocator
  re-entered from `pthread_setspecific`, `pthread_join` or
  `pthread_create`) is lent the outer token, and that a user is done
  with its token before another top-level user runs (the two frees'
  slow paths).
- The nested-entry specs, trusted: `pthread_setspecific`
  (`heap/shared.rs:1048`) leaves a sentinel unchanged; `pthread_join`
  and `spawn` (`purge/background.rs:483`, `:657`) keep a non-empty cell
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

## Interface contract

The per-call theorems above (1-3, 5-7 and the "Bad input" section)
checked against challoc's exported C ABI; the whole-allocator ones are
the internal rows below (I1-I7: theorems 1, 4, 5, 7, 19, 8 and 10). One
row per exported symbol (`je_` twins and C++ array forms share their
row), one column per interface property. Read from the `ensures` of
each entry point's Rust function (first at `ccbf1dd`); the `file:line`s
in this section are the `fn` line on main. The U column, row 13's Z and row 22's F were revised
later, by the usable-size work (gap 3 below), the R cells of rows 3,
4, 14 and 17 by the huge `kept` bound (gap 6), and the F column by the
`errno` work (gap 2); items they added are cited by name. All the
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
reads (`c_errno()`); glibc's exports (`api/glibc.rs:195`) and the
`mallctl` exports (`api/mallctl.rs:3459`) state none of their own.
`api::tests::exports_return_their_callees_result` (`api/mod.rs:3995`)
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
cells. They were updated for gap 1 (below).

Properties:

- **R**: the result fits and is aligned (`heap::fits`, `heap/mod.rs:198`);
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
  which `axiom_c_claim` excludes. Since theorem 7 a free target is
  never in a guard slab, so a free into one does not return. The
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
| 3  | `realloc`                                   | P | P | . | - | P | p | . |
| 4  | `reallocarray`                              | P | P | . | - | P | p | . |
| 5  | `aligned_alloc`, `memalign`                 | P | P | . | . | P | P | . |
| 6  | `posix_memalign`                            | p | P | . | . | p | P | . |
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
  tokens are disjoint (`Live::theorem_apart`, `heap/live.rs:969`), and a
  cached slot is no live block (`Heap::theorem_uniqueness_live`,
  `heap/mod.rs:4033`), so the result shares no byte with any other
  block the program holds, under the `free` contract for C's blocks.
  Slot tokens come from the class authorities (theorem 5); run tokens
  from the huge allocator's (`huge::RunToken`, `huge/mod.rs:962`), in
  place of the former uninterpreted `huge::allocated`.
- *(errno)*: the model is `vspec/errno.rs`. One ghost resource,
  `ThreadErrno`, stands for the calling thread's `errno` for one call;
  `val()` is the value challoc last wrote to it. A token comes only from
  `lend` (`external_body`, no `ensures`: the caller's `errno` is
  unknown), which each export wrapper that takes one calls once before
  the call (the `mallctl` and `mallctlbymib` exports included), and the only write is `set_errno` (`external_body`,
  `ensures final(errno).val() == e`), which takes it `&mut`; the copy in
  `api/print.rs` is gone. Every Rust function between a wrapper and a
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
  reads: `axiom_errno_at_return` (`vspec/errno.rs:90`, trusted, a
  C-program contract) uses up the lent token right after the call and,
  when the export's own `failed` condition holds, ensures `c_errno()`
  (`:54`, uninterpreted: `errno` as C reads it after this return) is
  the token's value. So `malloc`'s export ensures `r@.addr == 0 ==>
  c_errno() == ENOMEM` (`api/libc.rs:2986`), `realloc`'s the same
  except for a block resized to 0 (`:3123`), `aligned_alloc`'s and
  `memalign`'s `EINVAL` and `ENOMEM` (`:3135`, `:3148`),
  `posix_memalign`'s `r == ENOMEM ==> c_errno() == ENOMEM` (`:3161`),
  and `reallocarray`'s overflow `ENOMEM` (`:3211`). The unchanged-`errno`
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
  `api::tests::exports_return_their_callees_result` (`api/mod.rs:3995`)
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
  (`purge/background.rs:251`).
- *(live)*: `free_target` (`heap/mod.rs:500`, theorem 3) is proved. A
  call with a pointer that is not a slot start or a huge page start does
  not return. A free core takes the caller's token for `p` (`Live::claims`) and ensures
  `p@.addr != 0 ==> blk.block(p)`: what it freed was a live block, whose
  token it gave back to the cache or the bitmap word (slots) or the run
  authorities (runs). A non-live pointer that is a slot or huge page
  start has no token in C's hands; the boundary's `axiom_c_claim` (the
  `free` contract) makes such a call's context inconsistent, and the
  run-time checks (`SlabTable::check_live`, `class/table.rs:3661`, the
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
  plus the guard); this replaces the former trusted
  `vspec::huge::axiom_owned_run`.

Per row (cells marked `P` without a note are the cited `ensures`
alone):

1. `malloc` (`api/libc.rs:305`). R: `fits(r, size, QUANTUM)`. D: *(D)*.
   F: `r@.addr == 0 ==> final(errno).val() == ENOMEM` (*(errno)*): every
   NULL is a failure that set `ENOMEM`, and no block is NULL ("No block
   at address 0"). A non-NULL `malloc(0)` is a distinct block, at an
   address no other live block has (`Live::theorem_distinct`), as the
   module doc promises. Not stated: NULL only on exhaustion (exhaustion
   has no spec), so `malloc(0)` may be NULL with `ENOMEM` like any
   allocation.
2. `calloc` (`api/libc.rs:502`, `.0` of `calloc_zeroed`, `:525`). R:
   `fits(r, n * m, QUANTUM)`. Z: `r.1@.covers(r.0, n * m)` (theorem 6).
   F: `n * m > usize::MAX ==> r@.addr == 0` and `r@.addr == 0 ==>
   final(errno).val() == ENOMEM`, so an overflow is NULL with `ENOMEM`.
3. `realloc` (`api/libc.rs:1548`). R: `fits(r, size, QUANTUM)`, a kept
   block included (gap 6: `kept`, `:1196`, now bounds a kept run by
   `size`, and unfolds to `fits`; `lemma_kept_fits` in #138). D: *(D)*
   (`realloc_block`); a kept `p` comes back with the caller's own token.
   The move's disjointness from `p` is proved (`Live::theorem_apart` on
   the two blocks' tokens). The copy takes both blocks' permissions
   (`libc::copy_moved`, `vspec::user::copy_user`), which also makes the
   ranges disjoint. C: the code copies `min(old, size)` bytes, with
   `usable_of(p, old)` and `old == blk.len()`, the block's usable bytes
   (`old_size_or_keep`, `:1432`): the class size for a slot, a run token's
   usable pages for a huge `p`. Contents are modelled:
   `realloc_block` (likewise `realloc_usable_stored` and `rallocx_zeroed`)
   ensures that a moved block's first `min(blk.len(), size)` bytes read
   as `p`'s did when the copy was made (`Live::copied_from`, over the
   byte states). The copy's effect on the byte states is the trusted
   `vspec::user::copy_raw` (`vspec/user.rs:280`, `external_body`,
   Kani-checked; `axiom_copied_bytes` in #138); its safety is
   verified. F:
   `p != 0 && size == 0 ==> r@.addr == 0` with `errno` unchanged (the
   frame across `free`, *(errno)*), every other NULL with
   `ENOMEM`, and the core (`realloc_block`) gives the caller back its own
   token for `p` on a NULL from the move (`p` is still live; the code
   returns before `heap::free`). B: `p != 0 ==> free_target(p)`, *(live)*.
4. `reallocarray` (`api/libc.rs:1734`). As `realloc` for `n * m`. F also
   proves `n * m > usize::MAX ==> r@.addr == 0 && final(errno).val() ==
   ENOMEM`. B: `free_target(p)` only when `n * m` does not overflow (an
   overflowing call returns NULL without looking at `p`).
5. `aligned_alloc`, `memalign` (`api/libc.rs:2653`). R:
   `is_pow2(align) ==> fits(r, size, align)`. B:
   `!is_pow2(align) ==> r@.addr == 0 && final(errno).val() == EINVAL`.
   challoc's `memalign` rejects a non-power-of-two alignment, unlike
   glibc's, which rounds the alignment up (the module doc names the
   `EINVAL`). F: a NULL for a power-of-two alignment comes with `ENOMEM`.
6. `posix_memalign` (`api/libc.rs:2711`). R: `alloc_aligned`
   (`:2565`) ensures `fits(p, size, align)`, and `store_out` (verified,
   through the call's `vspec::cbuf::COut`) stores `p` through `out`. F:
   the return code is `0`, `ENOMEM` or `EINVAL`; `EINVAL` leaves `errno`
   unchanged, and
   `ENOMEM` comes from `alloc_aligned`'s `enomem`, so it also sets
   `errno = ENOMEM` (the function's doc says only that the `EINVAL` path
   leaves `errno` alone; the module doc's "leaves `errno` alone" is wider
   than the code). Not stated: `*out` untouched on failure (gap 4). B:
   `r == EINVAL <==> !(is_pow2(align) && align % 8 == 0)`.
   D: `posix_memalign_block` ensures, on a `0` return, the token of a
   block of at least `size` bytes aligned to `align`, and that it is the
   one stored through `out` (`*out`'s value through the `COut`), so `P`
   by the `_block`-core rule above (`posix_memalign` itself states only
   its return code and `errno`, so R stays `p`).
7. `valloc` (`api/libc.rs:2790`). R: `fits` at the page
   (`1 << lg_page`). F: NULL with `ENOMEM`.
8. `pvalloc` (`api/libc.rs:2818`). R: `fits(r, size, page)`. The
   page-rounded size the code allocates is not stated. F: NULL with
   `ENOMEM`, the `checked_add` overflow included.
9. `malloc_usable_size` (`api/libc.rs:1140`). F: `p == 0 ==> r == 0` and
   `p != 0 ==> 0 < r` (0 exactly for NULL, "No block at address 0"). B:
   `p != 0 ==> free_target(p)`, *(live)*. Before init, a non-NULL `p`
   aborts (`free_before_init`, `:1026`). U: `p != 0 ==> usable_of(p, r)`,
   exact for a slot, the descriptor's shape for a huge run, *(U)*.
10. `free` (`api/libc.rs:597`). B: `p != 0 ==> free_target(p)`, *(live)*;
    NULL is a no-op (not in the `ensures`; since upstream #78 the return
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
11. `free_sized`, `free_aligned_sized` (`api/libc.rs:927`, `:985`). B:
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
12. `challoc_malloc_usable` (`api/libc.rs:1819`, `.0` of
    `malloc_usable_stored`, `:1838`). R, D, F: as `malloc`. U: the `Stored` token
    of the last store through `usable` has `ptr() == usable` and, for a
    non-null result, `usable_is(r, size, val())`: the class's size, or
    `size` rounded up to whole pages for a huge run (*(U)*, from the
    run token `alloc_huge` returns); for a null result, `val() == 0`
    (the fast pop's slot is not null, `theorem_no_block_at_zero`). So
    `val() == 0` exactly for NULL (`theorem_stored_zero_iff_null`: a
    block's usable size is positive, "No block at address 0"). That the
    class is `nallocx(size, 0)`'s (the smallest that holds `size` bytes)
    is true of `class_index` but not exported here.
13. `challoc_calloc_usable` (`api/libc.rs:2003`, `calloc_usable_zeroed`,
    `:2028`). R, F: as `calloc`. Z: the token covers `n * m` bytes and
    `covers_usable(r.1, r.0, 0, n * m)`, the whole usable size the doc
    promises and the code zeroes (`zero_user(p, u)`, `api/libc.rs:2113`, or the known-zero
    token of `heap::alloc_zeroed` or `alloc_huge`). For a huge run the
    `memset` length `u` is the usable size of the block's own token
    (`usable_size` with the token `alloc_huge` returned,
    `huge_usable(n * m)`), so no trusted premise remains
    (`axiom_owned_run` is gone). U: as row 12, for `n * m`, and the null
    result's 0 is stated (0 exactly for NULL, as row 12).
14. `challoc_realloc_usable` (`api/libc.rs:2157`, `.0` of
    `realloc_usable_stored`, `:2182`). R, D, C, F, B: as `realloc` (the keep
    decision is `realloc_old`, `:2257`). U: as row 12. A kept block's
    size is `realloc_old`'s, which ensures `usable_of(p, old)` and, when
    kept, `usable_is(p, size, old)` (for a run, `usable_for(size) ==
    Some(old)` is the keep test); a moved block's is
    `malloc_usable_stored`'s.
15. `challoc_free_usable` (`api/libc.rs:2366`). F: `p == 0 ==> r == 0`
    and `p != 0 ==> 0 < r`, so 0 exactly for NULL ("No block at address
    0").
    B: as `free` (`free_usable_block`). U: `p != 0 ==> usable_of(p, r)`,
    *(U)*, and `r == blk.len()`, the freed block's usable bytes (the
    class size, or the run's usable pages, from its token on both paths):
    the class size from `heap::free_fast_slot` (`heap/paths.rs:832`),
    which ensures `is_slot(c, p)`, or from `free_usable_slow`'s classification
    (`api/libc.rs:2430`), or `Huge::usable_size` there. The `r == blk.len()` half
    is the core's; at the export `blk` is the token `axiom_c_claim` gives
    for C's `p`, so for a huge run it holds under the `free` contract
    (the `usable_of` half needs no token).
    `errno` is preserved as by `free`: `free_usable` and
    `free_usable_block` ensure `final(errno).val() == old(errno).val()`
    (over `free_usable_slow` and `heap::free`).
16. `mallocx` (`api/jemalloc.rs:399`, `.0` of `mallocx_zeroed`, `:417`).
    R: `fits(r, size, 1 << (flags & 0x3f))`. With no `MALLOCX_LG_ALIGN`
    this is alignment 1, weaker than the quantum the code gives. Z: with
    `MALLOCX_ZERO`, `covers(r, size)` and `covers_usable(.., 0, size)`
    (`vspec/zero.rs:270`, the whole usable size; theorem
    `vspec::zero::theorem_zeroed_memory`, `:380`). Also, for a non-NULL
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
17. `rallocx` (`api/jemalloc.rs:448`, `rallocx_zeroed`, `:477`). R:
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
    `errno` untouched is not stated (the token is passed on, as row 16);
    NULL `p` allocates.
18. `xallocx` (`api/jemalloc.rs:585`). Never resizes; returns
    `sallocx(p, 0)`. B: `free_target(p)` and `p@.addr != 0`
    unconditionally, so NULL aborts. U: `usable_of(p, r)` and `0 < r`,
    as row 19.
19. `sallocx` (`api/jemalloc.rs:600`). B: `free_target(p)` and
    `p@.addr != 0` unconditionally: NULL aborts (`not_an_allocation`,
    `:201`), now stated ("No block at address 0"); *(live)*.
    U: `usable_of(p, r)` and `0 < r`, *(U)*. For a slot `mallocx(size, flags)`
    returned, `theorem_usable_consistent` gives `r == nallocx(size,
    flags)`, jemalloc's contract.
20. `dallocx` (`api/jemalloc.rs:624`). B: `p != 0 ==> free_target(p)`,
    *(live)*. With `MALLOCX_TCACHE_NONE` the release tests the bit
    immediately (**unconfirmed**, from the doc comment). An explicit
    cache id of `MAX_TCACHES` or more aborts (`!bad_tcache`, "Bad
    input").
    `dallocx` and `dallocx_block` ensure `errno` unchanged (through
    `heap::mallocx::free`, which ensures it for every cache choice).
21. `sdallocx` (`api/jemalloc.rs:673`). B: `p != 0 ==> free_target(p)`,
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
22. `nallocx` (`api/jemalloc.rs:811`). F, U: `r == 0 || size <= r` and
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
    `class_of`'s and `class_index`'s new minimality: no smaller class
    holds `size` bytes), and `Huge::usable_for` is `Huge::spec_usable`.
    F: `r == 0` exactly when no class serves the request and no run for
    it is placeable (class sizes and `huge_usable` are positive);
    `mallocx` then returns NULL (`alloc_huge` ensures `huge_placeable`
    of a non-NULL run).
23. `operator new`, `new[]` (`api/cxx.rs:333`). R:
    `fits(r, size, QUANTUM)`. F: the C++ contract, never NULL: `new`
    ensures `r@.addr != 0` ("No block at address 0"), from `new_block`,
    `new_slow` and `oom`'s `!nothrow ==> r@.addr != 0` and the fast
    pop's non-null slot. `oom` returns a pointer only after a non-null
    test or, with `nothrow`, NULL; otherwise it leaves through
    `throw_bad_alloc` (`-> !`, which throws `std::bad_alloc` or aborts
    when libstdc++ is absent). `oom` has no `decreases` (it waits on the
    program's handler).
24. `new` `nothrow_t` (`api/cxx.rs:348`). R: as row 23. F: NULL after
    the handler loop, or at once when the handler throws (`oom` calls it
    through `call_handler_caught`, `cxx_handler.cpp`'s catch, which
    reports a throw as `false`); not stated beyond `fits`.
25. `new` `align_val_t` (`api/cxx.rs:387`). R: `is_pow2(align) ==>
    fits(r, size, align)`. F: the throwing forms as row 23
    (`new_aligned` ensures `!nothrow ==> r@.addr != 0`); `p` because
    the `nothrow_t` forms' NULL is not stated beyond `fits` (row 24). B: a
    non-power-of-two alignment (undefined in C++), 0 included, fails at
    once, without the handler loop; `new_aligned` ensures
    `!is_pow2(align) ==> r@.addr == 0` (NULL for the `nothrow_t` forms,
    `bad_alloc` for the others).
26. Unsized `delete` forms, with or without `align_val_t` or `nothrow_t`
    (`cxx::delete`, `api/cxx.rs:443`). B: as `free`; the alignment is
    ignored. `errno` unchanged, as `free`.
27. Sized `delete` (`cxx::delete_sized`, `api/cxx.rs:459`). B: as
    `free_sized`, `hint_ok` included. `errno` unchanged, as `free_sized`.
28. Sized aligned `delete` (`cxx::delete_sized_aligned`,
    `api/cxx.rs:474`). B: as `free_aligned_sized`, `hint_ok` included.
    `errno` unchanged, as `free_aligned_sized`.
29. `malloc_trim` (`api/libc.rs:2884`). F: `r == 0 || r == 1`. Not stated:
    whether anything was purged.
30. `mallopt` (`api/glibc.rs:82`). F: `r == 1` (accepted and ignored).
31. `mallinfo2` (`api/glibc.rs:108`) ensures the three zero fields,
    `uordblks`, `hblkhd` and `hblks` from what `collect_all` read and
    summed, `arena` as the sum's `resident` less its `large_allocated`
    (0 where negative), `fordblks` as `arena` less `uordblks` (0 where
    negative, so `fordblks <= arena`), and, when the arenas' `mapped`
    sum fits, `arena >= uordblks` and `fordblks == arena - uordblks`;
    it leaves out `ordblks` and `keepcost`. `mallinfo` (`:166`) is
    verified with no contract. Both require `may_abort()` and
    `may_syscall()`, which their exports grant. The figures are
    unbounded counters (`fig_add`, `fig_mul`).
32. `malloc_stats`, `malloc_info` (`api/print.rs:1376`, `:1351`):
    verified bodies. F (`malloc_info`; `malloc_stats` returns nothing
    and states nothing): `malloc_info` ensures `r == 0 || r == -1`,
    `r == -1 <==> options != 0 || stream` is null, and `r == -1 ==>
    final(errno).val() == EINVAL` (*(errno)*); a `0` may leave `errno`
    changed by the sink's `fwrite`, as glibc's may. The targets
    (`file_target`, `stderr_target`, `:1430`, `:1436`) are verified
    constructors; the write goes through the verified `emit` (`:85`) to
    the trusted `write_file` (`:139`, `fwrite`) or the verified
    `sys::write_all`.
33. `malloc_stats_print` (`api/print.rs:1242`): verified body, no
    `ensures`. Its `verified_export_pair!` exports (`:1486`) take the
    callback as the opaque, `#[repr(transparent)]` `WriteCallback`
    (the C function pointer's ABI) and call the verified
    `exports::stats_print_entry`, which mints the call's token. B: the
    program's `opts` string is read by `opt_byte`, verified over the C
    contract of a string argument (`vspec::cbuf`; the token `CCall` from
    the private `axiom_c_call` of `print.rs`'s `exports`, and its
    `str_c_byte`), and the callback through trusted code (`call_back`,
    `api/print.rs:109`; with no callback, `call_hook` through `malloc_write`).
34. `mallctl`, `mallctlnametomib`, `mallctlbymib` (`api/mallctl.rs:2997`,
    `:3114`, `:3182`): verified bodies, no `ensures` on the return code
    beyond `mallctlnametomib`'s `r != 0 ==> !final(cc).written()`
    (`result`, `:2982`, maps `Err(e)` to `e`); the C exports (`:3460`,
    `:3484`, `:3488`) are verified and state no `ensures`: each mints
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
    Those call sites are verified against their callees; the trusted
    wrappers `heap_set_tcache_enabled` and `tcaches_flush` are gone. Per
    heap reference, as theorem 20; no cell changes (the key's effect is
    not one of the columns).
35. `get_defrag_hint` (`api/jemalloc.rs:1016`). F: `r == 0 || r == 1`;
    in its body, 1 exactly when the design 13.5 rule holds over its reads
    (section "Utilization and defrag hint are exact"; the `ensures`
    exports it existentially). B:
    never aborts, for every pointer and before init (theorem 32): it
    requires no `may_abort()`, which an abort would need, and the
    theorem's witness calls it without (`theorem_read_only_never_aborts`,
    `api/mod.rs:1126`; the call is at `:1136`). The body is verified.
36. `challoc_version` (`api/mod.rs:151`), `challoc_features` (`:1090`):
    verified, no contract.
37. Data symbols `malloc_conf`, `malloc_message`, `je_malloc_message`
    (`api/jemalloc.rs:86`-`:100`): no code. `message_hook`, which
    reads the hooks, is trusted; `malloc_write` is verified, over the
    trusted one-line `call_hook`.

**Changed since #138.** Main's `extern "C"` wrappers restate their
cores' `fits` in their own `ensures`; the copy's byte-state effect is
`vspec::user::copy_raw`, discharged by Kani, in place of
`axiom_copied_bytes`; `write_zero` is `zero_raw`; and `lemma_kept_fits`
is gone because `kept` is open. The `errno` model changed:
- The frees, the `*allocx` family and the C++ deletes take the caller's
  token (#368, #371, #375). Every free ensures it unchanged (`free_block`,
  the sized frees, `challoc_free_usable`, `dallocx`, `sdallocx` and the
  C++ deletes; row 11's F is `P`, as in #138), while `mallocx` and
  `rallocx` state nothing.
- `background::start` uses the caller's token in place of its own
  `lend`.
- `get_errno` ensures it reads the token's value (a new trusted claim).
- The nullable `api::libc` exports (rows 1-8, 12-14) state their failure
  `errno` as the value the C caller reads (`c_errno()`), through the
  new trusted `axiom_errno_at_return`; #138 stated `errno` only on the
  Rust functions' tokens. The source test
  `api::tests::exports_return_their_callees_result` checks every export
  body's shape and the `lend` and axiom call sites
  (wolfi-dev/challoc#394).

Weaker on main:
- `theorem_no_syscall_on_fast_paths` requires `integrity_aborts()` (I6).

Stronger: `file_target` and `stderr_target` are verified. `mallinfo2`
also states `arena`, `fordblks` and, when the `mapped` sum fits, glibc's
identities (wolfi-dev/challoc#421; `mallopt`, `mallinfo` and their
exports are verified, as in #138). Row 6's D is
`P` by the `_block`-core rule (`posix_memalign_block` ties the token to
`*out`; the cell was `p` in #138 too, by mistake).

### Internal properties under every row

| Property                                 | Code | Where                  |
|------------------------------------------|------|------------------------|
| I1 region separation                     | P    | th. 1, heap/mod.rs:210 |
| I2 user blocks apart from metadata       | P    | th. 4, heap/mod.rs:335 |
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
  `theorem_guard_free` (`heap/mod.rs:236`, `:261`). For huge runs it is
  stated only of the huge lock's values (`Inner::theorem_run_guards`,
  `huge/mod.rs:3140`): `alloc_run` returns the run's token (gap 1), but
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
  row 2's pop, make no system call, except the write and `SIGABRT` of
  an integrity abort (`theorem_no_syscall_on_fast_paths` requires
  `integrity_aborts()` on main); a call that returns made none, and
  everything else they do is behind a call that requires
  `may_syscall()`. Other rows may enter the kernel.
- I7: theorem 10 (`Heap::flush_all`, lines as in section 10): the flush
  `destroy` runs first empties every home and node header and appends
  exactly what they held to the released log, each slot's token handed
  to `release_routed` with it. `destroy` itself is verified since package
  P10 (its heap block over `heap::axiom_heap_block`), section
  10's "weaker" list applies, and that every heap gets a destructor is
  not proved (with an invalid thread key none does), hence `p`.

### Gaps, by cells turned to `P`

1. **Live-block ghost model**: done. The allocation cores hand the caller
   the block's `Live` token (the slot's, or a `RunToken` from the huge
   lock's run authorities in place of the uninterpreted
   `huge::allocated`), every free core consumes one, and the C boundary
   is the one `axiom_c_claim`. The D column (16 of 16 rows; row 6's
   through `posix_memalign_block`, which ties the token to `*out`), I3
   and row 15's U are `P`. The
   *(live)* B cells of the rows that free (12 rows; the queries 9, 18,
   19 take no token) stay `p`: their cores require and consume the live
   block's token, but a non-live free is excluded by the C-boundary
   assumption (`axiom_c_claim`), not proved to abort. `axiom_free_token`
   and `axiom_owned_run` are gone, and `axiom_moved_block` no longer
   states the disjointness, which its callers prove (it was unsound for
   `p == q`).
2. **`errno` as ghost state**: done for rows 1-5, 7, 8, 10-15 and 32
   (F to `P`), *(errno)*. `set_errno` ensures the value it writes to
   the `ThreadErrno` token the export wrapper lends (`lend`); the
   allocating entry points ensure `r@.addr == 0 ==> final(errno).val() ==
   ENOMEM` (or `EINVAL`, or unchanged, where the API says so), and
   `free`'s core states that `errno` is unchanged. The exports of rows 1-8
   and 12-14 restate the failure values at the C boundary (`c_errno()`,
   `axiom_errno_at_return`, *(errno)*). The hot set's machine code is unchanged (the
   token is ghost). Rows 3, 4 and 14 are `P` with gap 1: their cores
   also give the caller back the old block's token on a failed move.
   Left: `posix_memalign`'s `*out` (row 6, gap 4); rows 16 and 17 take
   the token but state nothing about it (their first call runs init's
   libc calls); the frees' frame (rows 10, 11, 15, 20, 21, 26-28) rests
   on the premise that no libc call in trusted code on their paths
   leaves `errno` changed.
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
   token); what is left is that `posix_memalign` leaves `*out` untouched
   on failure (row 6: `store_out` writes through the call's `COut`, so a
   `0` return states `*out`). `!nothrow ==> r@.addr != 0` for `operator new`
   is done ("No block at address 0": row 23's F to `P`).
5. **Byte contents**: byte contents are modelled (`Live::copied_from`,
   over `copy_raw`, which Kani checks: `realloc_block`,
   `realloc_usable_stored` and `rallocx_zeroed` ensure, for a moved
   block, `final(out).copied_from(blk, min(blk.len(), size))`); the C column
   (4 rows) is not updated to match.
6. **Huge `kept` bound**: done for R (rows 3, 4, 14 and 17 to `P`).
   `Huge::usable_size` already ensures `spec_run_usable` from the run's
   descriptor (usable pages and guard inside the region);
   `vspec::usable::lemma_run_bytes_block` puts any `size <= old` of a
   huge page start in the region, so `kept`'s huge half carries
   `is_huge_block(p, max(size, 1))` and every realloc-family result
   `fits` (`kept` unfolds to it; `lemma_kept_fits` in #138). Ghost-only: no
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
   above). Row 16 goes from `-` to `p` and row 25 to `P` (cache ids of
   `MAX_TCACHES` or more abort; a non-power-of-two `align_val_t`, 0 included,
   gets no block), and rows 11, 21, 27 and 28 gain `hint_ok`: a slot
   freed with another class's size aborts. The *(live)* cells stay `p`:
   a stale or double free of a slot or huge page start is excluded by
   `axiom_c_claim`, not proved to abort. Left: model arena and cache
   existence (rows 16, 17, 20, 21). A huge run's size is tied to the
   sized frees' hint since #127 (`huge_hint_ok`, "Bad input").
