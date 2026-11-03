# challoc hardening against jemalloc

What challoc detects and prevents, compared with jemalloc 5.3, and the
exact windows the defaults leave open. The design view is
`docs/design.md` section 14; the price of each opt-in key is in
`docs/tuning.md` section 2.

| Property                                 | jemalloc 5.3 | challoc         |
|------------------------------------------|--------------|-----------------|
| Metadata separate from user memory       | No           | Yes             |
| Free-list pointers in freed blocks       | Yes          | None (5)        |
| Invalid free detected                    | No           | Partly: plain `free` aborts except on slots a thread cache holds; sized frees defer to a flush unless `exact_sized_free` is set; exact on every route with `double_free_scan` (1) |
| Double free detected                     | Debug only   | Partly: aborts once the slot has left the caches; a cached copy only if a flush reaches it unclaimed; exact on every route with `double_free_scan` (1) |
| Size classes in isolated address ranges  | No           | Yes, randomised |
| Guard regions                            | No           | Yes (kernel 6.13+ for guard slabs) |
| Sealed reservation gaps (`mseal`)        | No           | Yes where the kernel (6.10+) and seccomp allow it |
| Zero on free                             | Opt-in       | Opt-in: `zero_on_free` clears a block on free (2) |
| Zeroing allocations zero after a heap bug wrote not-yet-allocated memory | Small tcache allocations | Yes (5) |
| Slot order randomisation                 | No           | Opt-in: `slot_randomize` (2) |
| Signed allocator pointers (arm64 PAC)    | No           | Opt-in where the CPU has it: `pac_pointers`, cache entries and the TLS heap pointer `pacda`-signed, authenticated on every `malloc` (3) |
| Tagged pointers                          | No           | Planned (4)     |
| Implementation language                  | C            | Rust            |

## (1) Invalid and double frees

Through `free`: an invalid pointer aborts at the call, unless it is a
slot start some thread cache holds (a refill claimed it, setting its
bit, and `malloc` has not handed it out yet: that free is accepted and
the slot can be handed out twice; `double_free_scan` catches it), and so
does a second free of a slot that has already left the thread cache (its
slot bit is clear). A second free while the slot is still held in a
thread cache is not checked at the call. A flush catches it only if the
slot is still unclaimed when the flush reaches the copy: when both
copies are in one cache and released together, the flush groups the
released half by slab, so the copies land in one slab's batch and the
release aborts on the duplicate, and the one case where the split would
release one copy and keep the other aborts at the split. A pair the
split keeps together stays in the cache until a later flush releases it.
If a `malloc` pops one copy first, or the copies are in two threads'
caches and a refill claims the slot between the two flushes (the second
flush then releases the slot under its new owner), the duplicate is not
detected and the slot is live twice.

With the `double_free_scan` tunable set, every free route checks the
slot's state at the call: one byte per slot (free, cached or live) next
to the bitmap, which every route moves and checks, so a free of a slot
that is not live aborts at that free, wherever the first copy sits (this
thread's cache, another thread's, the foreign cache, or the slab) and
whichever route either free took (`free`, `sdallocx`, `free_sized`,
`free_aligned_sized`, `dallocx`, `MALLOCX_TCACHE_NONE`,
`challoc_free_usable`). A sized free of a slot start challoc has not
handed out aborts too, and `malloc_usable_size`, `realloc` and `sallocx`
reject a pointer freed into a cache. A free moves the byte with one
compare-and-exchange, so of two frees of one slot on two threads at the
same instant exactly one passes. The bytes are trusted, not verified
(`verus/ledger/src/heap/slotstate.rs.md`): Verus proves that a valid
free reaches none of the verified paths' aborts, but not that it never
reaches one of these; a bookkeeping bug would show as a false abort,
never as a missed check the default makes. With the key every free and
every `malloc` takes the slow path, which costs 1.8..2.9x lower
throughput on the quick rows of the GCE gate cells, `alloc-test1`
46..68% slower (cb run `20261102t0229z-caad429-quick-m5fdy`): it is for
debugging and high-assurance deployments, not a default.

Through the sized path, `sdallocx(p, size, flags)`, C23 `free_sized` and
`free_aligned_sized` (what Valkey frees with), by default (`exact_sized_free`,
below, changes it): the size is checked
against the pointer's class, or a huge run's page count, at the call (a
size that names another class or page count aborts, `nallocx(size,
flags) == sallocx(p)` is the contract), a slot start inside a guard slab
aborts at the call, the slot bit is not read, and any other invalid or
double free is left to a flush. It is caught when a flush releases the
slot to its slab while the slot is still unclaimed (a full cache or
`MALLOCX_TCACHE_NONE` runs plain free's check at the call instead: it
catches a slot whose bit is clear, not a copy still held in a cache;
with `double_free_scan` the state byte catches that too). That is not
bounded by a number of frees: a partial flush releases the cache's
highest addresses and keeps the lowest, so a slot below the rest of the
cache can stay cached through any number of same-class frees until a
whole flush (thread exit, `thread.tcache.flush`, `tcache.flush`) or a
partial one that reaches it. Between the call and that flush the slot
can be handed out twice, which is jemalloc's behaviour for every free:
if a `malloc` takes one copy before the flush the duplicate is not
detected. A sized free of a slot start challoc has not handed out enters
the cache too: the thread's next `malloc` of the class pops it (a slot
the caller chose, writable at once wherever its slab's memory is
committed, which includes the run committed ahead of the newest slab),
and if a refill claims that slot before the flush, the flush releases it
under its new owner and it is live twice. `double_free_scan` closes all
of this at the call (above). This trade is made for jemalloc parity: on
Valkey's write path the exact check's bitmap load is a cache miss per
free, most of what once separated challoc from jemalloc there.

The `exact_sized_free` tunable (`MALLOC_CONF=exact_sized_free:true`)
makes sized frees run plain `free`'s allocation check: after the size
check, every `sdallocx`, `free_sized`, `free_aligned_sized` and sized
`operator delete` goes through the exact `free`, so an invalid free, or a
second free of a slot that has left the thread cache, aborts at the call
as it does through `free`. It is off by default because every sized free
then takes the slow path, the bitmap load included. The sized fast
path's code is the same with the key set or not (with the key set, the
sized check's guard mask is 0, so the check declines every pointer), so
the default pays nothing for it. `double_free_scan` covers this check
too (above), and more: it also catches a slot a refill claimed and a
copy still held in a cache. What `exact_sized_free` adds is the sized
route's check without `double_free_scan`'s costs: plain `free` and
`malloc` keep their fast paths, the compacting cache stays on, and there
is no per-slot state (its own price is not measured; `double_free_scan`
costs 1.8..2.9x). Pick `exact_sized_free` when sized frees must check the
slot at the call and `double_free_scan`'s price is too high. With both
keys on, a sized free takes the size check, then plain `free`'s bit test and the state byte:
the route `double_free_scan` alone gives it, so `exact_sized_free` adds
nothing there.

## (2) Zero on free and slot randomisation

Both keys are off by default and are the operator's choice
(`MALLOC_CONF=zero_on_free:true`, `slot_randomize:true`). `zero_on_free`
clears a slot's class-size bytes on every free (plain, sized and direct)
and a huge run's usable bytes on its release, so freed data never waits in
the cache or the slab for the next owner; it costs a `memset` per free,
which is more than the 3% hardening budget on Valkey's 1 KiB values.
`slot_randomize` makes every cache refill claim from a random bitmap word
and shuffle its batch, so the order in which one class hands out
neighbouring slots is not predictable from the previous allocations; it
works against the compaction that keeps the heap dense (design 6.4), which
is why it is not the default.

## (3) Pointer authentication

On an arm64 host whose kernel exposes pointer authentication
(`HWCAP_PACA`/`PACG`: Google Axion, Apple silicon; not Ampere Altra, not
x86_64), every pointer a thread cache holds is stored signed with the data
key A (`pacda`, the cache's stack address as the modifier) and
authenticated (`autda`) when `malloc` pops it, and the heap pointer in TLS
is signed with the thread pointer; so a write into the allocator's
metadata region cannot make `malloc` hand out a chosen address: the pop
faults (CPUs with FEAT_FPAC, Neoverse V2, Apple M2+) or returns a
non-canonical pointer that faults on first use (Neoverse N1, Apple M1).
Probed at run time, one predicted branch on a byte the fast paths load
anyway, the same binary everywhere; `MALLOC_CONF=pac_pointers:true` turns
it on. It is off by default because the two PAC instructions an entry
costs (one signing it, one authenticating it) are 11% of xmalloc-test's
cross-thread frees and 1..2% of larson on Axion, over the 1% budget for a
default-on hardening (design 6.1, 6.2, 17). Of the reference hosts, the
C4A Axion VM exposes PAC.

## (4) Tagged pointers

Designed, not implemented (`docs/design.md` section 18, #50): on MTE
hosts every block gets a random 4-bit hardware tag, retagged on free, so a
read through a freed pointer faults; elsewhere a 4-bit software tag (a
per-slot generation keyed per class) in the ignored top bits (TBI on
aarch64, LAM on x86_64 should Linux enable it again) is verified by
`free`, `realloc`, `malloc_usable_size` and the sized path, so a stale,
forged or cross-class pointer aborts (15 times in 16). Four bits is the
width everywhere, since the LAM kernel series cuts the user tag to 4 bits
and MTE has 4. AMD UAI has no Linux enable path and is not a target. The
MTE half waits for a host that exposes MTE (AmpereOne, #51).

## (5) No allocator state in user blocks

challoc keeps all of its state (free lists, cache stacks, slab
descriptors, bitmaps, huge run tables) in metadata apart from every
block (`verus/THEOREMS.md` theorem 4). Beyond that, no allocator decision
reads a byte of a user block (theorem 46). The two exceptions are
`realloc`'s copy of the old block into the new one, which does not branch
on the bytes, and the opt-in `check_zero` debug mode, which aborts on a
non-zero word. So an overflow into a live or freed neighbour, or a write
through a dangling pointer, corrupts that block's data but cannot steer
`malloc` or `free`. Not covered: memory the program hands challoc as an
argument (`mallctl` buffers), the program's own callbacks, a thread
stack or TLS block the program placed in a `malloc`ed buffer (challoc's
per-thread slot is then inside it), and hardware side channels.

## Guard slabs and the run ahead of the cursor

With `MADV_GUARD_INSTALL` (kernel 6.13+) a class region has one guard
slab per `guard_slab_interval` x 64 KiB of slabs (8, so 512 KiB, by
default): a guard page at its start, never handed out. An arena commits
its fresh slabs a run at a time, up to the next guard slab and at most
512 KiB (design section 5), so the slabs between its newest slab and the
end of the run are read-write before they are handed out: less than
512 KiB (up to 7 of the 64 KiB slabs) with guards on, up to 512 KiB with
guards off (every kernel below 6.13). A linear overflow off the newest
slab therefore runs into that committed, unused memory and faults only
at the run's end (the guard slab, or the uncommitted slab after the
run), where with one commit per slab it faulted at the next slab. Those
slabs are not guards and were never promised as such; the guard slabs
themselves are unchanged.
Verus proves that no commit reopens a live guard
(`class::table::theorem_slab_commit_avoids_guards`,
`huge::theorem_huge_commit_avoids_guards`): a class commit shares no
byte with a guard slab, except one whose first page carries a marker
the commit keeps (the rest of that slab is committed with it), and a
huge commit or marker removal, made holding the huge lock, shares no
byte with the guard page of a live run other than the one being freed,
whose run token has come back. A class in `thp:always`'s PMD-chunk
mode places no guards.

## Region edges

Every region (each class region, the huge region and the metadata
region) has at least 4 MiB (`MAX_SLAB`) of its own reservation, never
committed and sealed where `mseal` is available, directly below and
directly above it, whatever the random offsets come out as: the huge and
metadata slides are 1..255 steps of 4 MiB inside their 1 GiB slide
window, and a class offset is 1..(gap / 4 MiB - 2) steps plus its
colour. A linear overflow or underflow off a region's first or last page
therefore faults inside challoc's reservation; it never reaches a
mapping the kernel placed next to it (a thread stack, a library's data).
Inside the huge region each run is followed by its guard page (none
under `guards:false`); the run at the bottom of the region is preceded by
that slack. `Regions::slack_ok`
checks the property once at init and aborts if a layout ever breaks it.
Verus proves it of the layout `Regions::reserve_owning` draws
(`Regions::padded`), and `theorem_overflow_reach` (`heap/mod.rs`) proves
the consequence for a block: between any of its bytes and the metadata
region, the `Shared` static or another region, in either direction, lies
the whole 4 MiB of that slack.

## Resource failures

The rule (`docs/design.md` 14.1): an abort means misuse or an
inconsistent heap, or init below the minimum layout. When the kernel or
a limit (`RLIMIT_DATA`, `RLIMIT_AS`, `vm.max_map_count`, `mlock`)
refuses memory after init, `malloc` returns NULL with `ENOMEM` and the
next call tries again, and `free` keeps what it cannot safely recycle
out of circulation.
None of this weakens a check: a resource failure never makes challoc
hand out a block whose metadata it could not record.
The mappings challoc creates count against `vm.max_map_count` only for
what is live or fixed (`docs/design.md` 14.1): a `PROT_NONE` huge guard
(kernels before 6.13, or a marker refused under `mlock`) costs at most
two VMAs while its run is live and is merged back when the run is freed, so a
program that freed its blocks gets its mappings back.

## Stricter than jemalloc

Two things are stricter than jemalloc: the size passed to `sdallocx`,
`free_sized`, `free_aligned_sized` or a sized `operator delete` must name
the class the pointer was allocated from (for a huge run, its page count), and `malloc_usable_size`,
`realloc` and `sallocx` on a freed or foreign pointer abort where
jemalloc's behaviour is undefined. A program that leaned on that leniency
dies with `SIGABRT` and a `challoc:` message instead of corrupting the
heap.

## (5) Zeroing allocations

`calloc`, `challoc_calloc_usable` and `MALLOCX_ZERO` never skip their
clear on the strength of allocator state that user code can falsify. A
slab slot is cleared on every zeroing allocation, including a slot
claimed from a `Fresh` or `Purged` slab: those slots are read-write
memory of the committed run, which a linear overflow from a live slot
below, or a write through a stale pointer into a purged slab, reaches
without challoc seeing it. A huge run skips the clear for pages past the
huge region's high-water mark (no run has covered them, and the last
run's guard page stops an overflow before them) and for a reused
`Purged` extent only after `MADV_DONTNEED`ing the run's pages again under
the huge lock as it places the run: the extent's own purge does not
count, since a stale pointer or an underflow from the live run above (a
freed run's guard goes with it) can have written it since. The caller
clears instead under 16 KiB (`REDISCARD_MIN`, where the `madvise` costs
more than the clear), when the kernel refuses the call (`mlock`ed
pages), after any lazy `MADV_FREE` purge (the extent may still be
resident, and discarding it would make every reuse fault it back), and
for a dirty (`Empty`) extent. jemalloc 5.3 always clears a small
allocation from the tcache.
The cost is the clear itself: on a `calloc` from fresh slab memory it
moves the first-touch page faults into the call (a program that writes
the block pays them anyway; one that leaves a large `calloc` sparse now
has those pages resident). A zeroing huge allocation (`calloc`,
`MALLOCX_ZERO`) that reuses a purged extent of 16 KiB or more pays one
`madvise` call (about 1 us, under the huge lock) in place of the clear.
It drops only the pages a stale write faulted back in since the eager
purge, so the run stays unpopulated. In lazy mode there is no call and
the caller clears the run, as before this rule. `malloc`, `realloc` and
`mallocx` without `MALLOCX_ZERO` never make the call.
