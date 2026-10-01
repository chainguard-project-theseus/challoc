# challoc hardening against jemalloc

What challoc detects and prevents, compared with jemalloc 5.3, and the
exact windows the defaults leave open. The design view is
`docs/design.md` section 14; the price of each opt-in key is in
`docs/tuning.md` section 2.

| Property                                 | jemalloc 5.3 | challoc         |
|------------------------------------------|--------------|-----------------|
| Metadata separate from user memory       | No           | Yes             |
| Free-list pointers in freed blocks       | Yes          | None            |
| Invalid free detected                    | No           | Yes, aborts (1) |
| Double free detected                     | Debug only   | Yes, aborts (1) |
| Size classes in isolated address ranges  | No           | Yes, randomised |
| Guard regions                            | No           | Yes (kernel 6.13+ for guard slabs) |
| Sealed reservation gaps (`mseal`)        | No           | Yes where the kernel (6.10+) and seccomp allow it |
| Zero on free                             | Opt-in       | Opt-in: `zero_on_free` clears a block on free (2) |
| Slot order randomisation                 | No           | Opt-in: `slot_randomize` (2) |
| Signed allocator pointers (arm64 PAC)    | No           | Opt-in where the CPU has it: `pac_pointers`, cache entries and the TLS heap pointer `pacda`-signed, authenticated on every `malloc` (3) |
| Tagged pointers                          | No           | Planned, hardware-gated: `tag_pointers` (4) |
| Implementation language                  | C            | Rust            |

## (1) Invalid and double frees

Through `free`: an invalid pointer aborts at the call, and so does a
second free of a slot that has already left the thread cache (its slot bit
is clear). A second free while the slot is still cached is caught when a
flush releases the copies to the slab: the flush groups the released half
by slab, so the copies land in one slab's batch, the release aborts on the
duplicate, and the one case where the split would release one copy and
keep the other aborts at the split. A pair the split keeps together stays
in the cache until a later flush releases it; if a `malloc` pops one copy
before then, the duplicate is not detected and the slot is live twice,
unless the `double_free_scan` tunable is set: then every free (plain and
sized) compares the slot against the four most recently cached entries
and aborts on a match at the call. It is off by default because it costs
about 3..4% on larson and up to 14% on the Valkey model on x86_64
(`bench/results/2026-09-18-*-gce-detection-gaps.md`).

Through the sized path, `sdallocx(p, size, flags)`, C23 `free_sized` and
`free_aligned_sized` (what Valkey frees with): the size is checked against
the pointer's class at the call (a size that names another class aborts,
`nallocx(size, flags) == sallocx(p)` is the contract), a slot start inside
a guard slab aborts at the call, the slot bit is not read, and any other
invalid or double free is caught when the thread cache flushes the slot to
its slab: at most a cache's worth of frees later (at once when the cache is
full or with `MALLOCX_TCACHE_NONE`). Between the call and that flush the
slot can be handed out twice, which is jemalloc's behaviour for every free;
if a `malloc` takes one copy before the flush the duplicate is not
detected. This trade is made for jemalloc parity: the exact check costs
one slab-descriptor cache miss per free, 200 of the 273 cycles per SET that
separated challoc from jemalloc on Valkey's write path.

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
is why it is not the default. The price of each on `larson`, `redis` and
the Valkey model is in `bench/results/2026-09-18-*-gce-tunables.md`.

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
default-on hardening (`bench/results/2026-09-19-lane-a-pac-pointers.md`;
design 6.1, 6.2, 17). Of the reference hosts, the C4A Axion VM exposes
PAC.

## (4) Tagged pointers

Designed, not implemented (`docs/design.md` section 18):
`tag_pointers:mte` gives every block a random 4-bit MTE tag retagged on
free, so a read through a freed pointer faults in hardware;
`tag_pointers:lam` carries a six-bit class-and-generation tag in the
ignored top bits (LAM on x86_64, TBI on aarch64) that `free`, `realloc`,
`malloc_usable_size` and the sized path verify, so a stale, forged or
cross-class pointer aborts. Default `off`. No GCE CPU exposes MTE, LAM or
UAI to a guest (`bench/results/2026-09-19-cpu-matrix.md`), so the
implementation waits for a host that does.

## Stricter than jemalloc

Two things are stricter than jemalloc: the size passed to `sdallocx`,
`free_sized`, `free_aligned_sized` or a sized `operator delete` must name
the class the pointer was allocated from, and `malloc_usable_size`,
`realloc` and `sallocx` on a freed or foreign pointer abort where
jemalloc's behaviour is undefined. A program that leaned on that leniency
dies with `SIGABRT` and a `challoc:` message instead of corrupting the
heap.
