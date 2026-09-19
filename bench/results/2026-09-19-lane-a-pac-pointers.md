# Phase 2.5 task 7: PAC-signed cache and heap pointers on arm64 (lane A)

Date 2026-09-19. Reference VMs `challoc-bench-arm64` (c4a-standard-16,
Google Axion, Neoverse V2, `pac=yes`, FEAT_FPAC) and `challoc-bench-amd64`
(c4-standard-16, Xeon 8581C, no PAC), kernel 7.0.0-1011-gcp, dev container
(Wolfi, glibc 2.44). Parent commit 72bea34. Lane A works on `main`.

## 1. The change and the mechanism

`sys::pac` (new): `pacda`/`autda`/`xpacd` through `asm!` with
`.arch_extension pauth` (the compiler baseline stays `armv8-a`; the
instructions are UNDEFINED without FEAT_PAuth, so they sit behind a
runtime flag). `Shared::push_flags` gains `PUSH_PAC` (4), set by `init`
when `HWCAP_PACA` and `HWCAP_PACG` are both present and `pac_pointers`
(new key, default `true`) is not `false`. With it:

- every slot pointer stored in a thread cache's stack is signed on the
  way in (push: `pacda p, slots`; refill and run rebuild: one `pacda` per
  claimed entry) and authenticated on the way out (pop, scheduled pop:
  `autda p, slots`); the modifier is the stack's base address, which the
  pop and the push load anyway, so the fast paths gain exactly one
  instruction each plus a predicted branch on a byte `free` already reads
  (`malloc` now reads it too: one L1 hit from a static's line);
- a flush orders the stack by the address bits alone (the PAC field,
  `pac::mask()` from one `xpacd`, masked off: one `and` per compare in the
  sortedness test, the selection and the kept sort) and authenticates only
  the half it releases (`release_owned` needs plain pointers), so an entry
  costs one `pacda` in and one `autda` out however it leaves. The first
  form, authenticating the whole stack and re-signing the kept half
  (`len + rest_len` PAC instructions per flush), failed the gate: section
  4.1;
- the heap pointer in TLS is signed with the thread pointer (`tpidr_el0`,
  which `tls::get` loads anyway): `heap_for_thread` and `existing_heap`
  compare against the sentinels first, then `autda`; the sentinels are
  never signed.

Failure mode: on FEAT_FPAC (Axion, Neoverse V2) a corrupted entry faults at
the `autda` (`SIGILL`); on the original PAuth the pointer comes out
non-canonical and faults on first use (`SIGSEGV`). x86_64 compiles the
helpers to the identity and never sets the flag.

The cost the task expects: two PAC instructions per malloc (TLS `autda`,
entry `autda`) and one per free (entry `pacda`), each on the dependent
load chain, plus the flush passes.

## 2. `quick-perf` on the C4A, before (72bea34, one run: `new` = parent)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | new (parent) | 42.05 | 71.2 | 249.3 | 2.099 | 4.012 |
| larson1 | jemalloc | 44.83 | 66.4 | 224.8 | 2.062 | 1.729 |
| valkey1 | new (parent) | 13.73 | 220.4 | 229.5 | 3.058 | 2.743 |
| valkey1 | jemalloc | 13.57 | 222.7 | 171.2 | 2.975 | 2.701 |
| valkey1-free | new (parent) | 13.34 | 227.0 | 236.0 | 4.057 | 3.891 |
| valkey1-free | jemalloc | 13.30 | 227.4 | 173.2 | 3.411 | 3.302 |

## 3. `quick-perf` on the C4A, after (same run: `old` = 72bea34, `new` = this change with PAC on, `new:pac_pointers:false` = the same binary with the flag off, jemalloc)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 42.00 | 71.3 | 249.3 | 2.098 | 3.998 |
| larson1 | new (PAC on) | 41.27 | 72.6 | 260.7 | 2.099 | 3.996 |
| larson1 | new, `pac_pointers:false` | 42.09 | 71.2 | 257.4 | 2.090 | 4.002 |
| larson1 | jemalloc | 44.70 | 66.6 | 224.8 | 2.067 | 1.694 |
| valkey1 | old | 13.55 | 223.1 | 230.7 | 3.069 | 2.781 |
| valkey1 | new (PAC on) | 12.59 | 240.7 | 252.8 | 3.111 | 2.815 |
| valkey1 | new, `pac_pointers:false` | 12.89 | 235.4 | 243.5 | 3.081 | 2.787 |
| valkey1 | jemalloc | 14.14 | 213.9 | 170.8 | 2.967 | 2.693 |
| valkey1-free | old | 12.89 | 235.0 | 236.0 | 4.053 | 3.890 |
| valkey1-free | new (PAC on) | 12.71 | 238.6 | 253.6 | 4.089 | 3.895 |
| valkey1-free | new, `pac_pointers:false` | 12.98 | 233.5 | 246.3 | 4.067 | 3.873 |
| valkey1-free | jemalloc | 13.74 | 220.2 | 173.8 | 3.427 | 3.357 |

What the instructions say (the exported `malloc` and `free` disassembled
from the aarch64 release build, `.tmp-objdump.sh`): `malloc` gains
`adrp; ldrb` (the `push_flags` byte, which `malloc` did not load before),
`tbz; autda x8, x10` (the TLS heap pointer against `tpidr_el0`, already in
`x10`), a `cbz` the compiler adds because the authenticated value is
opaque to it, and `tbz; autda x0, x10` (the popped entry against the stack
base, already in `x10`): six instructions with PAC on, four with it off.
`free` gains the same byte load, `tbz; autda` for the heap pointer, a
`cmp w8, #4; b.eq` that selects the signed push, and `pacda x0, x10` (the
stack base, already loaded for the store): seven with PAC on, five with it
off. Per larson operation (one malloc and one free) that is +11.4
instructions with PAC on and +8.1 with it off, per Valkey SET (two mallocs
and two sized frees) +22.1 and +12.8; the difference between the two
variants, 3..9 instructions, is the four PAC instructions and the flush
passes.

Cycles, one run each: larson1 +1.3 cycles per operation with PAC on
(+1.8%) and -0.1 with it off; valkey1-free +3.6 (+1.5%) and -1.5;
valkey1 +17.6 (+7.9%) and +12.3 (+5.5%) for the flag-off binary of the
same code, which puts that row's reading inside its run-to-run band (the
same parent code read 220.4 in section 2 and 242.5..250.8 in
`2026-09-19-aarch64-gce-sized-path.md`); the `quick` tier below is the
gate.

## 4. The `quick` gate on both VMs (one round each, `old` = 72bea34, PAC on in `new` on the C4A)

### 4.1 First form (the flush authenticates the whole stack and re-signs the kept half), C4A: NO-GO

| row | unit | old | new | jemalloc | new vs old |
|---|---|---:|---:|---:|---:|
| larson1 | M ops/s | 42.2 | 41.7 | 45.0 | +1.2% |
| larson10 | M ops/s | 365.7 | 359.4 | 388.4 | +1.8% |
| xmalloc | M frees/s | 598.136 | 516.654 (median of 3) | 480.680 | +15.8% |
| alloc-test1 | s | 2.616 | 2.666 | 2.499 | +1.9% |
| valkey1 | M ops/s | 7.270 | 7.245 | 7.296 | +0.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | compacts |

xmalloc-test's consumer thread frees what the producers allocated: its
caches fill and flush every `cap / 2` frees on the sorted path (1.25k
cycles per flush before this change, `2026-09-19-*-gce-flush-sort.md`),
and authenticating 1024 entries plus re-signing 512 per flush doubled
that. The `make test` run on the same VM before it: 349 + 108 + 23 tests
and the four cgroup churn outcomes green, `corrupted entry -> signal 4`,
`corrupted tls -> signal 4` (FEAT_FPAC on Axion: `SIGILL` at the `autda`).

### 4.2 Second form (order by address bits, authenticate only the released half), C4A: NO-GO

| row | unit | old | new | jemalloc | new vs old |
|---|---|---:|---:|---:|---:|
| larson1 | M ops/s | 42.2 | 41.7 | 45.1 | +1.2% |
| larson10 | M ops/s | 366.0 | 359.2 | 390.5 | +1.9% |
| xmalloc | M frees/s | 598.418 | 539.320 (median of 3) | 467.308 | +11.0% |
| alloc-test1 | s | 2.620 | 2.699 | 2.510 | +3.0% |
| valkey1 | M ops/s | 7.221 | 7.076 | 7.267 | +2.0% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | compacts |

The remaining xmalloc cost is the floor of the design: every cached entry
is signed once (the consumer's push) and authenticated once (the flush
that releases it), two PAC instructions per cross-thread free on a path
that is otherwise a store and a counter, plus the `autda` of the heap
pointer per call. `quick-perf` of this form, same run (`old` 72bea34,
`new` PAC on, `new:pac_pointers:false` the same binary with the flag
off):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 41.90 | 71.5 | 249.4 | 2.095 | 4.117 |
| larson1 | new (PAC on) | 41.30 | 72.5 | 260.8 | 2.099 | 3.995 |
| larson1 | new, `pac_pointers:false` | 41.67 | 71.9 | 257.5 | 2.093 | 4.010 |
| larson1 | jemalloc | 44.74 | 66.6 | 224.8 | 2.077 | 1.728 |
| valkey1 | old | 13.47 | 224.9 | 232.2 | 3.079 | 2.813 |
| valkey1 | new (PAC on) | 12.75 | 237.9 | 250.9 | 3.103 | 2.818 |
| valkey1 | new, `pac_pointers:false` | 13.37 | 226.7 | 244.3 | 3.084 | 2.816 |
| valkey1 | jemalloc | 13.99 | 216.2 | 171.2 | 2.979 | 2.732 |
| valkey1-free | old | 13.47 | 225.0 | 233.5 | 4.048 | 3.893 |
| valkey1-free | new (PAC on) | 12.57 | 241.2 | 253.9 | 4.085 | 3.881 |
| valkey1-free | new, `pac_pointers:false` | 12.94 | 234.3 | 245.7 | 4.056 | 3.891 |
| valkey1-free | jemalloc | 13.49 | 224.3 | 173.7 | 3.437 | 3.329 |

With the flag off the binary is within noise of the parent on cycles
(larson1 +0.4, valkey1 +1.8, valkey1-free +9.3 in a row whose band is
+-10; instructions +8.1 and +12.1 per operation for the byte load and the
branches). With PAC on: larson1 +1.0 cycle (+1.4%), valkey1 +13.0 (+5.8%),
valkey1-free +16.2 (+7.2%); the two `autda` on `malloc`'s dependent chain
(heap pointer, then entry) and the `pacda` on `free`'s are the whole
difference in instructions (+3.3 per larson operation, +6.6 per SET) and
sit on the load-to-use chains the Valkey model is bound by.

### 4.3 C4 (x86_64, no PAC; the flag is never set), second form: go

| row | unit | old | new | jemalloc | new vs old |
|---|---|---:|---:|---:|---:|
| larson1 | M ops/s | 43.9 | 43.8 | 43.6 | +0.2% |
| larson10 | M ops/s | 275.5 | 278.9 | 278.8 | -1.2% |
| xmalloc | M frees/s | 320.476 | 324.107 | 320.015 | -1.1% |
| alloc-test1 | s | 3.390 | 3.413 | 3.221 | +0.7% |
| valkey1 | M ops/s | 14.546 | 14.721 | 14.650 | -1.2% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6679 (49.8%) | compacts |

(The first form on the C4, also go: larson1 +1.1%, larson10 +1.5%,
xmalloc +0.6%, alloc-test1 +0.2%, valkey1 +2.9%.) The x86_64 build keeps
the byte load and the branches (the helpers compile to the identity, the
flag is never set), which is what the +0.2..-1.2% measure.

### 4.4 Decision: opt-in

The plan's condition for a default was `quick` on the C4A within noise;
the second form is over the 5% gate on xmalloc (+11.0%) and 1..3% behind
on the other rows, and Phase 2 task 3's rule for a default-on hardening
is 1% on larson1. `pac_pointers` therefore defaults to `false`; the
signing path is complete and tested (`MALLOC_CONF=pac_pointers:true`),
and the tests build their `Shared` with the key on so every heap test
runs signed on a PAC host (the Mac's Docker VM and the C4A both report
`fpac=true`).

### 4.5 For the record: cache entries signed, heap pointer plain (a measurement build, not shipped)

The same tree with the three TLS sites made plain (`heap_for_thread`,
`existing_heap`, `tls_value_for`), so only the cache entries are signed;
PAC on. `quick-perf`, same run against 72bea34:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 41.98 | 71.3 | 249.3 | 2.094 | 4.000 |
| larson1 | entries only | 41.59 | 72.0 | 254.7 | 2.097 | 4.008 |
| valkey1 | old | 13.65 | 222.0 | 229.1 | 3.060 | 2.781 |
| valkey1 | entries only | 13.21 | 229.4 | 243.0 | 3.091 | 2.817 |
| valkey1-free | old | 13.44 | 225.8 | 235.2 | 4.044 | 3.869 |
| valkey1-free | entries only | 12.97 | 233.8 | 245.6 | 4.078 | 3.913 |

`quick`: larson1 +0.5%, larson10 +1.8%, xmalloc +5.5% (599.588 ->
568.244, median of 3), alloc-test1 +2.0%, valkey1 +0.2%, churn compacts:
still NO-GO on xmalloc. So the heap pointer's `autda` per call is about
half of the full form's cost on the Valkey rows (+7.4 vs +13.0 cycles per
SET) and half of xmalloc's (+5.5 vs +11.0%), and the entries alone are
already over the gate on xmalloc. The shipped key signs both; a
`pac_pointers` variant that signs entries only was not added (one key,
one meaning), the numbers are here if the human wants that trade.

### 4.6 The shipped tree (`pac_pointers` off by default): the plain path's gate

Two more terms were found and removed on the way to the shipped tree,
both paid by every host, PAC or not:

- The keyed sorts. The second form's flush used `is_sorted_by_key`,
  `select_nth_unstable_by_key` and `sort_unstable_by_key` with the address
  mask as the key for everyone (the mask being `!0` without PAC). The
  comparator closure loses the primitive pointer sort's specialisations,
  and the flag-off binary read xmalloc +5.3% (599.386 -> 569.034, median of
  3) in `quick`, the same as the entries-only signed build (+5.5%): the PAC
  instructions were not the term, the sorts were. The shipped flush takes
  the keyed forms only when signing is on and the original calls otherwise.
- A `cbz` on the heap pointer after the authenticate-or-not merge (the
  `Option` niche test the compiler could no longer fold), removed with
  `hint::assert_unchecked`. Branching once at the entry point into two
  copies of the whole fast path was also tried: LLVM merged the copies'
  tails and gave `malloc` a stack frame, so the entry points keep the one
  path with the `autda`s behind their own `tbz`s (`malloc`: `adrp; ldrb;
  tbz; tbz` over the parent, `free`: `tbz` and a `cmp #4; b.eq` in the push
  selection).

`quick`, C4A, `old` 72bea34, `new` the shipped tree (flag off): go.

| row | unit | old | new | jemalloc | new vs old |
|---|---|---:|---:|---:|---:|
| larson1 | M ops/s | 42.3 | 42.2 | 45.1 | +0.2% |
| larson10 | M ops/s | 371.7 | 360.4 | 383.8 | +3.1% |
| xmalloc | M frees/s | 598.218 | 586.338 | 480.824 | +2.0% |
| alloc-test1 | s | 2.637 | 2.634 | 2.511 | -0.1% |
| valkey1 | M ops/s | 7.332 | 7.206 (median of 3) | 6.799 | +1.7% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | compacts |

(larson10 reads 359..374 M ops/s for the parent across this file's runs;
+3.1% is inside that band. xmalloc's +2.0% is the plain path's remaining
cost, the four instructions above on a free that is otherwise a store and
a counter.)

`quick`, C4, same: go (larson1 +2.0%, larson10 +1.3%, xmalloc +3.1%,
alloc-test1 +0.8%, valkey1 -0.7%, churn compacts; the C4's xmalloc reads
316..337 across this file's runs).

## 5. Tests

- Mac Docker VM (Apple silicon, linuxkit 6.10, `pac=yes`, `fpac=true`):
  `make test` green, 349 + 110 + 23 tests and the four cgroup churn
  outcomes; `pac: corrupted entry -> signal 4`, `pac: corrupted tls ->
  signal 4` (FEAT_FPAC: `SIGILL` at the `autda`), `pac: mismatched
  modifier -> signal 4`, `pac: mask 0x7f000000000000 (7 bits)`,
  `tls_threads (pac_pointers:true): ok 64 threads, distinct heaps`.
- C4A (Axion, `pac=yes`, `fpac=true`) and C4 (x86_64, no PAC): `make test`
  on the shipped tree, see PROGRESS; on the C4 the PAC tests report `pac:
  off on this host, skipping` and the key is accepted and inert.
- The unit tests build their `Shared` with `pac_pointers: true`, so on a
  PAC host every heap test (refill, flush, compaction, GC, thread exit,
  arenas, explicit caches) runs over signed entries and a signed TLS heap
  pointer; the `Config::DEFAULT` builders (`shared_with`) run the plain
  path.
