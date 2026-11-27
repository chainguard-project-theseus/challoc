# Vulnerability classes: historical allocator CVEs against challoc's theorems

This file maps published CVEs and upstream advisories in the C allocators
challoc replaces or mimics (glibc ptmalloc, jemalloc, TCMalloc, musl,
newlib, uClibc-ng, NuttX, Scudo) to the bug class each one belongs to,
and names, for every class, the theorem in [THEOREMS.md](THEOREMS.md)
that rules it out for all inputs, or says plainly that none does. It
extends the "Vulnerability classes ruled out" table in
[RESULTS.md](RESULTS.md#vulnerability-classes-ruled-out), which covers
the size-overflow CVEs, with the classes that table does not: invalid and
double frees, metadata corruption, sized-free mismatch, zeroing, realloc
over-read, usable-size queries, thread exit and allocation
predictability. Every claim below holds under the
[common premises](THEOREMS.md#common-premises) and the
[trust base](RESULTS.md#trust-base): in particular the C allocation
contract (`heap::live::axiom_c_claim`: C frees only live blocks, once),
the lock invariants, and the trusted seams. Where a theorem rests on
more than that (an opt-in key, a slot-state premise, a lock invariant
not exported per call), the row says so. "Gap" means no theorem states
the property for all inputs; it does not mean challoc has the bug. The
run-time checks behind several gaps exist and are tested
(`tests/c/negative.c`); they are code, not theorem.

A note on "for all inputs": `heap::fits(r, size, align)` uses unbounded
specification integers, so `n * m` and `size + align` cannot wrap in the
statement, and Verus checks every operation of a verified function for
arithmetic overflow (RESULTS.md, "Coverage"). That is what closes the
integer-overflow class, which is the largest group of allocator CVEs.
The proofs cover the x86_64 and aarch64 targets, each at `quantum16` and
`quantum8` (THEOREMS.md, "Configuration"); 32-bit and powerpc builds,
where some of the CVEs below lived, are not targets.

## CVE and advisory table

| Id | Year | Class | Upstream function | challoc counterpart | Ruling theorem |
| --- | --- | --- | --- | --- | --- |
| [CVE-2005-4895](https://nvd.nist.gov/vuln/detail/CVE-2005-4895) | 2005 (published 2012) | integer overflow in a size computation | TCMalloc (`tcmalloc.cc`, gperftools before 0.4) allocation paths | `api::libc::malloc` (`malloc_block`, `alloc_huge`), `api::jemalloc::mallocx` (`alloc_with`) | 2 Fit and alignment: `malloc` ensures `heap::fits(r, size, QUANTUM)`; `alloc_huge` ensures `size <= shared.huge_usable(size)`; 43: `theorem_nallocx_zero_rejects`. Test `c_cve_2005_4895` |
| [CVE-2006-7252](https://nvd.nist.gov/vuln/detail/CVE-2006-7252) | 2006 (published 2012) | integer overflow in a size computation (`n * m`) | jemalloc `calloc` in FreeBSD 6.4 and NetBSD libc | `api::libc::calloc` (`calloc_zeroed`), `api::libc::reallocarray`, `api::libc::calloc_usable` | Interface contract rows 2, 4, 13 (F) with 2: `calloc_zeroed` ensures `n * m > usize::MAX ==> r.0@.addr == 0` and `heap::fits(r.0, n * m, QUANTUM)`; `reallocarray` ensures the same with `ENOMEM`. Test `c_cve_2006_7252` |
| [CVE-2007-6754](https://nvd.nist.gov/vuln/detail/CVE-2007-6754) | 2007 (published 2012) | integer rounding and overflow in an aligned allocation | jemalloc `ipalloc` in FreeBSD 6.4 and NetBSD libc | `api::libc::alloc_aligned` (behind `aligned_alloc_block`, `posix_memalign_block`, `valloc_block`, `pvalloc_block`), `api::jemalloc::mallocx` with `MALLOCX_LG_ALIGN` | 2: `alloc_aligned` ensures `heap::fits(r, size, align)` for every power-of-two `align`; `mallocx` ensures `fits(r, size, 1 << (flags & 0x3f))`. No CVE-named test; the `c_cve_2018_6485` shapes cover it |
| [CVE-2013-4332](https://nvd.nist.gov/vuln/detail/CVE-2013-4332) | 2013 | integer overflow in a size computation (alignment and page rounding) | glibc `pvalloc`, `valloc`, `posix_memalign`, `memalign`, `aligned_alloc` (2.18 and earlier) | `api::libc::pvalloc_block`, `valloc_block`, `posix_memalign_block`, `aligned_alloc_block` | 2: each ensures `fits(r, size, align)` (the page for `valloc`, `pvalloc`); `pvalloc` also ensures `fits(r, page_round(size, page), page)` and NULL when the rounding passes `usize::MAX` (interface row 8, [#735](https://github.com/chainguard-project-theseus/challoc/issues/735)). Test `c_cve_2013_4332` |
| [CVE-2017-17426](https://nvd.nist.gov/vuln/detail/CVE-2017-17426) | 2017 | integer overflow in a size computation (tcache path near `SIZE_MAX`) | glibc 2.26 `malloc` | `api::libc::malloc` (`malloc_block`, `malloc_class`, the thread-cache pop) | 2: `fits(r, size, QUANTUM)` on every result, fast pop included. Test `c_cve_2017_17426` |
| [CVE-2018-6485](https://nvd.nist.gov/vuln/detail/CVE-2018-6485) | 2018 | integer overflow in a size computation (`size + align`) | glibc `posix_memalign`, `memalign` (2.26 and earlier) | `api::libc::alloc_aligned`, `posix_memalign_block` | 2: as CVE-2013-4332; `posix_memalign` states `fits` of `*out` through `axiom_out_at_return`. Test `c_cve_2018_6485` |
| [CVE-2018-6551](https://nvd.nist.gov/vuln/detail/CVE-2018-6551) | 2018 | integer overflow in a size computation (near `SIZE_MAX`) | glibc `malloc`, 2.24 to 2.26 on powerpc, 2.26 on i386 | `api::libc::malloc` | 2: `fits(r, size, QUANTUM)`. The CVE's targets are not challoc's (x86_64, aarch64 only). Test `c_cve_2018_6551` |
| [CVE-2021-3420](https://nvd.nist.gov/vuln/detail/CVE-2021-3420) | 2021 | integer overflow in a size computation (alignment and page rounding) | newlib `memalign`, `pvalloc`, `nano_memalign`, `nano_valloc`, `nano_pvalloc` | `api::libc::alloc_aligned`, `pvalloc_block`, `valloc_block` | 2, as CVE-2013-4332, `pvalloc` at the page-rounded size ([#735](https://github.com/chainguard-project-theseus/challoc/issues/735)) |
| [CVE-2021-26461](https://nvd.nist.gov/vuln/detail/CVE-2021-26461) | 2021 | integer wrap-around in a size computation ("BadAlloc") | Apache NuttX `malloc`, `realloc`, `memalign` | `api::libc::malloc`, `realloc` (`realloc_block`), `alloc_aligned` | 2: `fits` on each, a kept `realloc` block included (`kept`, theorem 1) |
| [CVE-2021-27419](https://nvd.nist.gov/vuln/detail/CVE-2021-27419) | 2021 | integer wrap-around in a size computation ("BadAlloc") | uClibc-ng `malloc-simple` | `api::libc::malloc` | 2: `fits(r, size, QUANTUM)` |
| [CVE-2023-4911](https://nvd.nist.gov/vuln/detail/CVE-2023-4911) | 2023 | configuration read in a secure-execution process (malloc tunables through `GLIBC_TUNABLES`) | glibc `ld.so` tunables parser | `config::load`, `config::linux::env_var` | Secure-exec configuration (`config::linux::theorem_secure_conf`): no `env_var` call returns when `AT_SECURE` is set. Test `c_cve_2023_4911` |
| [CVE-2023-21366](https://source.android.com/docs/security/bulletin/android-14) | 2023 | predictable allocation pattern | Android Scudo (design) | `class` refill and `shuffle_zero` under the opt-in `slot_randomize` key | Theorem 11 (order): the shuffle's order is `fisher_yates` of the batch over the generator's `draws`, a bijection from draws to orders (`theorem_shuffle_order_injective`, `theorem_shuffle_order_reach`), and the state is never zero (`theorem_xorshift_nonzero`, `Heap::rng`). Unpredictability itself is probabilistic and not claimed (#738, intentionally not pursued). The start-word draw and the non-zero state through `claim_with`/`finish` (#749): `finish` ensures one `xorshift` step per scanning pick (`xorshift_iter`), and `claim_random_zero` carries a non-zero state to the shuffle's entry. The bulletin gives no technical detail; mapping to this class is from the CVE description |
| [CVE-2023-21367](https://source.android.com/docs/security/bulletin/android-14) | 2023 | heap out-of-bounds read or write turned into an exploit (allocator design) | Android Scudo (design) | every allocation entry point | Partial: `heap::theorem_overflow_reach` (metadata and other regions), 7 Guard pages, 4; inside the block's own region, `heap::theorem_guard_reach` (a stop within the class's guard interval, guards on) and `huge::RunToken::theorem_token_guards` ([#737](https://github.com/chainguard-project-theseus/challoc/issues/737)). Neighbours inside that bound are reachable. The bulletin gives no technical detail |
| [CVE-2026-0861](https://nvd.nist.gov/vuln/detail/CVE-2026-0861), [GLIBC-SA-2026-0001](https://sourceware.org/git/?p=glibc.git;a=blob_plain;f=advisories/GLIBC-SA-2026-0001) | 2026 | integer overflow in a size computation (`size` near `PTRDIFF_MAX` with alignment up to `1 << 63`) | glibc `memalign`, `posix_memalign`, `aligned_alloc` (2.30 to 2.42) | `api::libc::aligned_alloc_block`, `posix_memalign_block`, `alloc_aligned` | 2: `fits(r, size, align)` for every power-of-two `align`, `1 << 63` included; "Bad input": a non-power-of-two `align` (memalign's `[2^62 + 1, 2^63)`) is `EINVAL`. No CVE-named test (see "Reproducers") |
| jemalloc 4.0.0 ChangeLog | 2015 | integer overflow in a size computation (size classes, profiling on) | jemalloc `malloc`, `posix_memalign`, `memalign`, `calloc`, `realloc` | the same entry points in `api::libc` | 2 (challoc has no heap profiler) |
| jemalloc 4.1.0 ChangeLog ("Make *allocx() size class overflow behavior defined") | 2016 | integer overflow in a size computation | jemalloc `mallocx`, `rallocx`, `nallocx` | `api::jemalloc::mallocx`, `rallocx`, `nallocx` | 2 and 43: `fits` at the flags' alignment; `nallocx` ensures `r == nalloc_usable(..)`; `theorem_nallocx_zero_rejects` |
| jemalloc 5.4.0 ChangeLog, commit [6b245225](https://github.com/jemalloc/jemalloc/commit/6b245225459fd09132c877b83a91725cb56097da) ("Fix numeric overflow checks in size classes") | 2026 | integer overflow in a size computation (size-class lookup) | jemalloc size-class computation | `class_index`, `class_for` (`spec_class_for`), `api::jemalloc::nallocx` | 2 and 43: `class_index` ensures `r == spec_class_for(n, 1)` and every slab result `fits`; Verus checks the lookup's arithmetic |
| jemalloc 4.0.1 ChangeLog ("Fix xallocx() bugs related to size+extra exceeding HUGE_MAXCLASS") | 2015 | integer overflow in a size computation (`size + extra`) | jemalloc `xallocx` | `api::jemalloc::xallocx` | Not applicable by construction: challoc's `xallocx` never resizes; it returns `sallocx(p)` and ensures `free_target(p)` and `usable_of(p, r)` |
| jemalloc 4.0.3 and 4.0.4 ChangeLog (`xallocx(..., MALLOCX_ZERO)` left trailing bytes unzeroed) | 2015 | zeroed memory not zero | jemalloc `xallocx` | `api::jemalloc::xallocx` (no growth); `rallocx_zeroed` | 6 Zeroed memory: `rallocx_zeroed` ensures `covers_usable(.., k, size)` for a moved block; `xallocx` grows nothing |
| jemalloc 3.0.0 ChangeLog ("Fix large calloc() zeroing bugs"; "Fix chunk_alloc_dss() to stop claiming memory is zeroed") | 2012 | zeroed memory not zero | jemalloc `calloc` | `api::libc::calloc_zeroed`, `calloc_usable_zeroed`, `heap::alloc_zeroed` | 6 (`vspec::zero::theorem_zeroed_memory`, `Live::reads_zero`): token coverage at return, premised on the kernel zero axioms (see class H) |
| jemalloc 3.0.0 ChangeLog ("Work around TLS deallocation via free() on Linux. This bug could cause write-after-free memory corruption.") | 2012 | use of a thread's allocator state after thread-exit teardown | jemalloc thread-exit path | `heap::thread` destructor, `heap_for_thread`, fast paths | Thread exit (`heap::thread::theorem_exit_no_rebuild`), premised on the slot premise |
| glibc [BZ 22111](https://sourceware.org/bugzilla/show_bug.cgi?id=22111), [BZ 28028](https://sourceware.org/bugzilla/show_bug.cgi?id=28028) | 2017, 2021 | thread-exit cache rebuilt after shutdown (leak) | glibc `tcache` thread shutdown | as above | Thread exit (`theorem_exit_no_rebuild`); cited in RESULTS.md |
| glibc commit [bcdaad21](https://sourceware.org/git/?p=glibc.git;a=commitdiff;h=bcdaad21d4635931d1bd3b54a7894276925d081d) ("malloc: tcache double free check", 2.29); [how2heap fastbin_dup](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/fastbin_dup.c) | 2018 | double free | glibc `free` (`_int_free`, tcache and fastbins) | `api::libc::free` (`free_block`), `heap::paths::free_fast_slot`, `Heap::free`, sized frees | Gap, [#733](https://github.com/chainguard-project-theseus/challoc/issues/733) and [#734](https://github.com/chainguard-project-theseus/challoc/issues/734). Excluded by the C contract; the opt-in `double_free_scan` window is `heap::slotstate::theorem_double_free_window`, premised on the slot-state invariant |
| glibc commit [a1a486d7](https://sourceware.org/git/?p=glibc.git;a=commitdiff;h=a1a486d70ebcc47a686ff5846875eacad0940e41) ("Add Safe-Linking to fastbins and tcache", 2.32); how2heap [tcache_poisoning](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/tcache_poisoning.c), [fastbin_dup_into_stack](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/fastbin_dup_into_stack.c) | 2020 | heap metadata corruption (free-list pointer poisoning in freed blocks) | glibc tcache and fastbin lists | every allocation and free core | 46 (`api::libc::theorem_no_user_reads`) and 4 (`heap::theorem_metadata_integrity`): no free-list link lives in, or is read from, a user block |
| how2heap [unsafe_unlink](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/unsafe_unlink.c) | technique | heap metadata corruption (inline chunk headers, unlink) | glibc `free` consolidation | every free core | 4 and 46: challoc keeps no header or link in a user block |
| how2heap [house_of_spirit](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/house_of_spirit.c), [tcache_house_of_spirit](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/tcache_house_of_spirit.c) | technique | invalid free (forged chunk outside the heap) | glibc `free` | `api::libc::free` and every free route | 3 Invalid free and "Bad input": every free ensures `p@.addr != 0 ==> free_target(p)`, with no C contract needed for a pointer that is no block start. Test `negative stack_free` |
| how2heap [house_of_einherjar](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/house_of_einherjar.c), [poison_null_byte](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/poison_null_byte.c) | technique | heap overflow into a neighbour's metadata, overlapping chunks | glibc `free` consolidation, `malloc` | every allocation | 4, 46, `Live::theorem_apart` (theorem 5 and interface D), `theorem_overflow_reach` |

## Classes

### A. Integer overflow in a size computation

**Property.** For every `size`, `n`, `m` and power-of-two `align`, a
non-null result of an allocating entry point is a slot of a class at
least `size` bytes (`n * m` for `calloc` and `reallocarray`) or a huge
page start with that many bytes of the huge region after it, at a
multiple of `align`: `heap::fits(r, size, align)` (theorems 1 and 2,
`heap/theorems.rs`). `calloc_zeroed` and `reallocarray` also ensure
`n * m > usize::MAX ==> r@.addr == 0` with `ENOMEM`. `pvalloc_block`
rounds with `size.checked_add(page - 1)`, and it, `api::libc::pvalloc`
and the export ensure `fits(r, page_round(size, page), page)`, with
`size + (page - 1) > usize::MAX ==> r@.addr == 0` (bit-vector
assertions tie the mask to `page_round`). `theorem_nallocx_zero_rejects` (theorem 43) proves a request
`nallocx` sizes at 0 makes `mallocx` return NULL in every state.

**Premises and limits.**
- Slab results rest on `CachePerms::tracks` inside the assumed
  `Heap::wf`; huge results on the huge lock invariant (theorem 1,
  "Premises").
- `mallocx` without an alignment flag is stated at alignment 1, not the
  quantum (theorem 1, "weaker").
- `fits` names `size` bytes; the usable size comes from `usable_is`
  (interface U column).
- Verus checks verified code for arithmetic overflow; trusted bodies
  (`external_body`) are not checked that way, and the shipped release
  profile has overflow checks off, so a trusted body's arithmetic rests
  on its seam's status in the ledger.

### B. Alignment bugs

**Property.** `aligned_alloc` and `memalign` ensure
`!is_pow2(align) ==> r@.addr == 0 && errno == EINVAL`;
`posix_memalign` ensures `r == EINVAL <==> !(is_pow2(align) && align % 8 == 0)`
and leaves `*out` as it was on failure; `cxx::new_aligned` ensures
`!is_pow2(align) ==> r@.addr == 0` (or the throw); `mallocx` and
`rallocx` give a block aligned to `1 << lg_align` for every `lg_align`
from 0 to 63, or NULL ("Bad input"; theorem 2). Alignment that wraps a
size (CVE-2013-4332, CVE-2018-6485, CVE-2026-0861) is class A.

**Limits.** challoc's `memalign` rejects a non-power-of-two alignment
where glibc rounds it up (interface row 5); this is stricter, not a
hole. `aligned_alloc` accepts a size that is not a multiple of the
alignment, as glibc does (doc comment on `api::libc::aligned_alloc`).

### C. Heap metadata corruption (inline headers, free-list poisoning, House-of-*)

**Property.** No byte of a returned block lies in the allocator's
metadata listed in `heap::theorem_metadata_integrity` (theorem 4: the
metadata region, the metadata allocator's link words, the `Shared`
static, class-table descriptors, owner and bitmap words, cache headers
and stack entries, huge descriptors, descriptor-table slots, free-page
bitmap words). No allocator decision reads a byte of a user block,
except `realloc`'s copy and the opt-in `check_zero` reads
(`api::libc::theorem_no_user_reads`, theorem 46), so a forged link or
size written into a freed or live block never steers `malloc` or
`free`. A linear write off a block reaches a 4 MiB `PROT_NONE` run of
challoc's reservation before any metadata, the `Shared` static or
another region (`heap::theorem_overflow_reach`), and no commit reopens
a live guard (`theorem_slab_commit_avoids_guards`,
`theorem_huge_commit_avoids_guards`). Inside a class region, with
guards on, a linear write off any byte of a block meets a guard slab's
first byte or the region's edge within `m` page groups going up and
`m + 1` going down, `m + 1` the class's guard interval
(`heap::theorem_guard_reach`). For huge runs only page separation is
proved: a run's guard pages are no other live run's usable page
(`huge::RunToken::theorem_token_guards`), and the page a live block's
run token counts as its guard starts at the block's end
(`heap::live::Live::theorem_run_guards`). That `Huge::prepare` guards
that page, and that it faults, is read from `Huge::prepare`, and under
`guards:false` the guard page is read-write.

**Premises and limits.**
- Theorem 46 covers verified code; trusted bodies are covered by reading
  their code. It excludes callbacks, caller-supplied buffers, and a
  thread whose stack or static TLS lives in a `malloc`ed buffer (the TLS
  heap slot is then inside a user block and an overflow into it is not
  excluded; theorem 4 leaves TLS out too).
- `theorem_overflow_reach` trusts that the reservations are mapped
  `PROT_NONE` and, without `mseal`, that nothing else remaps the slack.
  It does not cover allocator statics outside `Shared`, or a freed
  run's guard.
- `theorem_guard_reach` bounds the reach to the first stop, not to the
  next block: neighbours in the same slab or page group, and the
  committed slabs ahead of an arena's cursor, lie inside the bound. With
  guards off (no `MADV_GUARD_INSTALL`, or `guard_slab_interval:0`) or
  under `thp:always` with PMD chunks it gives no class-region stop but
  the region's edges.
- Theorem 4 is spatial only: it does not prove every allocator write
  stays in its object, nor the mapping permissions of returned bytes.

### D. Invalid free (pointer shape)

**Property.** Every free route, the `realloc` family,
`malloc_usable_size`, `sallocx` and `xallocx` ensure
`p@.addr != 0 ==> free_target(p)` (theorem 3, `heap/theorems.rs`), and
`reallocarray` ensures it when `n * m <= usize::MAX` (an overflowing
product returns NULL with `ENOMEM` before `p` is classified): a
call with a pointer outside every region, in a gap, in metadata, in a
guard slab, misaligned to the quantum, interior to a slot, or off a page
boundary of a huge run does not return (`theorem_guard_free`,
`theorem_free_target_aligned`, `Live::theorem_interior`). This needs no
C contract for the call itself: a non-block pointer's token claims
nothing.

**Premises and limits.**
- "Does not return" is proved, not "reaches `SIGABRT`": lock waits are
  not proved to terminate, so a hang is not ruled out ("Bad input",
  "weaker").
- An interior page start of a huge run: for frees, the check is proved
  at the huge lock (THEOREMS.md "Bad input", *(lock)*:
  `Huge::live_run_checks` returns only if no live run has the page as
  a later page), and that a free of a pointer C holds no token for
  reaches it is the C contract (*(C)*). For the queries it is proved not
  to return over any live run's token: `api::libc::usable_size` ensures `!run_interior(t, p)`
  for the run token its ghost `blk` holds, which the code never reads
  (THEOREMS.md "Bad input", *(ghost token)*; #736).
- Holds only in a run with no earlier non-live free (the C contract).
- A thread's first `free` of a bad pointer may create its heap before
  the abort.

### E. Double free and stale free

**Property.** None for all inputs in the default configuration. A stale
or double free of a slot start or huge page start is excluded by the C
contract (`axiom_c_claim`), not proved to abort (theorem 3, "Bad input"
rows marked *(C)*, interface *(live)* cells). What is proved:
- In every configuration, the huge free's checks
  (`Huge::live_run_lookup` and `Huge::live_run_checks`, which
  `Huge::free_run` calls; THEOREMS.md
  "Bad input", *(lock)*): a free returns past them only if a live run
  starts at the page at the huge lock, so a double free of a huge run
  whose start no later allocation has reused, or a free of a live run's
  interior page or guard, does not pass. Once another run starts at that
  page, a stale pointer to it is that run's start and passes (ABA,
  undetectable by any allocator). A fact about the lock's state, not
  C's view.
- With the opt-in `double_free_scan` key,
  `heap::slotstate::theorem_double_free_window`: after a free of a slot
  returns, every later free of it aborts until the slot is handed out
  again. It rests on the slot-state premise (each `step` finds the
  value the last step left; the bytes and the address-to-byte cast are
  trusted, `verus/ledger/src/heap/slotstate.rs.md`).
- With the opt-in `exact_sized_free` key,
  `heap::paths::theorem_exact_sized_free`: a sized free reaches plain
  `free`'s bit test; that the test aborts on a clear bit is read from
  the code.
- In every configuration, the bit test itself
  (`SlabTable::check_live`, `check_located`; THEOREMS.md "Bad input",
  *(load)*), in part: a call returns only if the slot's bit was set in
  some value its bitmap word held. The load hands back a witness of the
  value it read, a persistent fragment of the word's seen values that
  only the word invariants hold (`load_seen`,
  [#847](https://github.com/chainguard-project-theseus/challoc/issues/847)).
  So a free of a slot no refill ever claimed does not pass. A stale
  double free is not excluded by the contract: the value before the
  first free had the bit set, and that the witness is this load's
  value is `bit_word_held`'s body. Before #847 the contract stated only
  `word_tokens`, an `exists v` that `v = u64::MAX` met without a load
  ([#843](https://github.com/chainguard-project-theseus/challoc/issues/843),
  open for the stale half). The stale half in part: a check ordered
  after the slot's release that passes saw a later claim of the slot
  (`lemma_claimed_again`,
  [#850](https://github.com/chainguard-project-theseus/challoc/issues/850));
  that the second free's check is ordered after the release is C's,
  except at the fast and slow paths' checks after a release by the same
  heap's flush, whose records `Heap::free` passes to
  `Shared::classify_free_after` (`CachePerms::rels`,
  `SlabTable::claimed_since`) and whose copy in the thread's slot token
  `free_fast_slot` seeds its test with (`TlsSlot::rels`,
  [#912](https://github.com/chainguard-project-theseus/challoc/issues/912)),
  and to which the retest after a rehome's flush is ordered too
  (`Heap::rehome_on_free`,
  [#918](https://github.com/chainguard-project-theseus/challoc/issues/918)).
  A heap's direct release (`Heap::free_slow_in`, no cache for the
  class) is kept as a record the same way
  ([#920](https://github.com/chainguard-project-theseus/challoc/issues/920)).
  `heap::free_direct` (no heap, or `MALLOCX_TCACHE_NONE`) carries no
  record. A fact about the load, not C's view. The plain frees (`free`,
  `delete`), `challoc_free_usable`, `dallocx` and the reallocations
  (`realloc`, `challoc_realloc_usable`, `rallocx`) take C's token for a
  slot start only past this test, with its record:
  `heap::Claim::settle` requires `slot_checked(p, obs)`
  ([#835](https://github.com/chainguard-project-theseus/challoc/issues/835)),
  so `axiom_c_claim` is not used before the check. The
  huge half: the huge free's
  checks (THEOREMS.md "Bad input", *(lock)*) return only for a page
  where a live run starts at the lock, and the same entry points take
  C's token for a huge page start only past them, under the lock:
  `heap::Claim::settle_run` requires `run_checked(page, runs)`, the
  process's run authority (which only the huge lock's value holds)
  with an extent starting at the page (`Huge::free_run`, or
  `Huge::usable_size` for a `zero_on_free` clear, which runs first, and
  for the size `challoc_free_usable` returns and the reallocations
  read). The sized frees (`free_sized`, `free_sized_hint`,
  `free_aligned_sized`, `sdallocx`, the sized `delete`s) still claim
  first: their bit test runs only at a cache flush unless
  `exact_sized_free` is set.

**What the code does (read, not proved; `docs/hardening.md` (1)).** An
unsized free of a slot whose bitmap bit is clear aborts at the call
(`SlabTable::check_live`); a sized free defers that test to a cache
flush unless `exact_sized_free` is set; a free while the first copy is still in a
thread cache passes and is caught at a flush only if no `malloc` pops
the slot first, so the slot can be handed out twice, as under jemalloc;
a free of a never-handed-out slot a refill claimed into a cache passes
the bit test; a huge double free aborts in `Huge::free_run`. A free of
a block that was freed and handed out again (ABA) is undetectable by
any allocator. Tests: `negative double_free_stale`,
`double_free_cached`, `sized_double_free`, `double_free_straddle`,
`huge_double_free`.

### F. Size-mismatched sized free

**Property.** For a live block, a sized free returns only with a size
that names it: `free_sized`, `free_aligned_sized`, `sdallocx` and the
sized `delete`s ensure `hint_ok(p, spec_class_for(size, align))`, and
the cores (`free_sized_block`, `sdallocx_block`) ensure
`huge_hint_ok(blk, p, size)` ("Bad input", theorem 43).
`theorem_sized_slot_nallocx` turns this into jemalloc's
`nallocx(size, flags) == sallocx(p)` for a slot, and
`theorem_sized_run_nallocx` for a huge run at an alignment that leaves
`size` huge and placeable (`spec_class_for(size, 1 << lg_align) is
None` and `huge_placeable(size, 1 << lg_align)`). A
`MALLOCX_TCACHE(t)` with `t >= MAX_TCACHES` aborts (`!bad_tcache`).

**Limits.** A huge run's hint is checked by page count, so any size that
rounds to its pages passes, and the `nallocx` equality does not hold
outside those alignment premises: `p = mallocx(64,
MALLOCX_LG_ALIGN(20))` is a huge run, `sdallocx(p, 64, 0)` passes the
page-count check, but `nallocx(64, 0)` is a slab class's size, not the
run's usable size (theorem 43). For a non-live pointer the hint check rests
on the C contract. A `MALLOCX_TCACHE(t)` of a cache never created and a
`MALLOCX_ARENA(a)` that names no arena are run-time lookups the proofs
do not model.

### G. Overlapping blocks (one block handed out twice)

**Property.** Two live blocks' usable bytes are disjoint
(`Live::theorem_apart`, `heap/live.rs`); a cached slot is no live block
(`Heap::theorem_uniqueness_live`, `heap/mod.rs`); two cached entries of
one class name different slots (`CachePerms::theorem_uniqueness_cached`,
theorem 5); a non-null `malloc(0)` is a distinct block
(`Live::theorem_distinct`); no block is at address 0
(`heap::theorem_no_block_at_zero`, theorem 24).

**Limits.** All of this is for a program that obeys the C contract. In
the default configuration a double free while the slot is cached can
make a slot live twice (class E); that is exactly the path these
theorems assume away.

### H. Zeroed memory that is not zero

**Property.** `calloc_zeroed`, `calloc_usable_zeroed`, `mallocx_zeroed`
(with `MALLOCX_ZERO`) and `rallocx_zeroed` return a `Zeroed` token
covering the bytes the API promises zero
(`vspec::zero::theorem_zeroed_memory`, theorem 6), and the block's byte
state records them (`Live::reads_zero`). With `zero_on_free`, every free
path clears the whole usable size before giving the block up
(`heap::zero::theorem_zero_on_free`, `theorem_zero_on_free_huge`,
theorem 12): the cache push and the two direct releases
(`Heap::push_cache`, `Shared::free_direct`, `Shared::free_huge`) require
the clear's token.

**Premises and limits.** Coverage, not proved contents: the token's
meaning rests on `zero_raw` (Kani-discharged) and the kernel axioms
`huge::axiom_fresh_anon_zero`, `class::axiom_discarded_slots_zero` and
`huge::axiom_discarded_zero`, each with a protocol premise (no writer
before the claim; a `Purged` slab is claimed zero only while no batch
used `MADV_FREE`). Plain `calloc` promises only its `n * m` bytes. A
kept `rallocx` block gets no zero claim (as in jemalloc).
`zero_on_free` is off by default, and its state is not carried to the
slot's next hand-out.

### I. Heap over-read in realloc and usable-size queries

**Property.** `realloc` copies `min(old, size)` bytes with
`libc::copy_moved`, which requires `n <= blk.len()` and `n <= out.len()`
over both blocks' permissions, so the copy reads only the old block's
usable bytes and writes only the new one's (interface row 3, C cell).
`realloc` keeps a block in place exactly by the class/page keep rule
(`keeps`, theorem 25; `theorem_realloc_to_usable_size`). For a slot,
`malloc_usable_size`, `sallocx` and `xallocx` are exact
(`usable_of`, `vspec::usable::theorem_usable_consistent`).

**Limits.** The C cell is `p`: for a huge `p`, `old` is exact only
through the caller's token (the C contract). The queries take no token,
so their own `ensures` state only the descriptor's shape for a huge run
(`run_bytes`: whole pages, at least one, guard inside the region).
`usable_size`, which they call, ensures `r == blk.len()` for a token
that claims `p` and reads no token in its code, so the value is the
run's usable size for every live run (THEOREMS.md U note, *(ghost
token)*; #736).

### J. Use after free by the program

Out of scope for an allocator: challoc has no quarantine or type
isolation (RESULTS.md, "Outside these guarantees"). What it does
guarantee: a write through a dangling pointer never steers an allocator
decision (theorem 46), never reaches allocator metadata in the
metadata ranges theorem 4 lists, and with `zero_on_free` the freed bytes
are cleared at release (theorem 12). `slot_randomize` (off by default)
makes reuse order harder to predict: theorem 11 proves the order is a
function of the batch and the generator's draws alone, every order is
reachable, and the state never sticks at zero; nothing is claimed about
the distribution.

### K. Thread-exit lifecycle

**Property.** At `TLS_EXITED` the slot readers hand out no heap and make
no `Heap::create` call (`heap::thread::theorem_exit_no_rebuild`);
a lookup holding destroy's bound finds no destroyed explicit cache
(`heap::tcaches::theorem_tcache_destroyed_none`).

**Limits.** Rests on the slot premise (a source test checks it in part)
and on the `pthread_join` and `pthread_create` specs; the
`MALLOCX_TCACHE` paths' tokens come from `axiom_heap_use` with no bound.

### L. Secure-execution configuration

**Property.** `config::linux::theorem_secure_conf`: with `AT_SECURE`
set, no `env_var` call returns, so `config::load` reads neither
`MALLOC_CONF` nor `CHALLOC_CONF`; `load` is `load_sources` then
`load_env`, and `load_env`'s contract adds that it then leaves the
`Config` and the issue count unchanged, so the loaded `Config` is the
non-environment sources' alone (the one with neither variable set). It
trusts `getauxval(AT_SECURE)`; that `env_var` is the only reader of the
environment is a source test.

## Gaps

**[#733](https://github.com/chainguard-project-theseus/challoc/issues/733): double free while the first copy is cached (no CVE id; glibc tcache double-free check bcdaad21, how2heap fastbin_dup)**

> Property: under `double_free_scan`, every free route given a slot
> start whose slot is not live at the call does not return, for all
> inputs, without the slot-state premise (discharge the state bytes'
> values and the address-to-byte cast). Targets:
> `heap::slotstate::step`, `Shared::check_not_cached`, the free cores
> in `api::libc`, `api::jemalloc`, `api::cxx`. Also state, as a theorem,
> the default-mode residue `docs/hardening.md` (1) describes, so the
> README cannot claim more than it.

**[#734](https://github.com/chainguard-project-theseus/challoc/issues/734): stale free of a slot or huge run whose bit is clear (no CVE id; how2heap fastbin_dup)**

> Property: in the default configuration, for all pointers, `free`,
> `realloc`, `malloc_usable_size`, `sallocx`, `xallocx` and
> `challoc_free_usable` of a slot start whose bitmap bit is clear at the
> load, or of a huge page start with no `Active` run, do not return.
> Today these rows rest on `axiom_c_claim`; lift `bitmap::WordPred`'s
> fact out of the load so the abort is a theorem. Targets:
> `SlabTable::check_live`, `Shared::classify_free`, `Huge::free_run`
> (`already_free`, `not_a_run`), `Huge::usable_size`.

Partly proved: the slab bit test at its load for a never-claimed slot
(`check_live`, `check_located`: `slot_free_at` against a witnessed
value, `load_seen`, THEOREMS.md "Bad input" *(load)*,
[#847](https://github.com/chainguard-project-theseus/challoc/issues/847)),
and the huge free's checks at the lock (`Huge::live_run_checks`,
THEOREMS.md "Bad input" *(lock)*). In part for a stale slot: a check
ordered after the slot's release that passes saw a later claim of it
(`lemma_claimed_again`,
[#850](https://github.com/chainguard-project-theseus/challoc/issues/850)).
The C contract is no longer used before the check
([#835](https://github.com/chainguard-project-theseus/challoc/issues/835)):
the plain frees, `challoc_free_usable`, `dallocx` and the reallocations
take C's token (`axiom_c_claim`) only past the bit test of a slot or
the huge lock's checks of a run, with the check's record
(`Claim::settle`, `Claim::settle_run`; THEOREMS.md "Frees claim C's
token only past their check"), so a free of a slot no refill ever
claimed, or of a page where no live run starts, aborts in a consistent
proof context. Open: that the second free's check is ordered after the
release, which the program carries no record of
([#843](https://github.com/chainguard-project-theseus/challoc/issues/843));
C's view of a stale pointer past the checks; the sized frees, which
still claim first; and the queries (`malloc_usable_size`, `sallocx`,
`xallocx`), which take no token.

**[#735](https://github.com/chainguard-project-theseus/challoc/issues/735): integer overflow in pvalloc's page rounding (CVE-2013-4332, CVE-2021-3420)**

> Property: `pvalloc(size)`'s non-null result fits the page-rounded
> size: `fits(r, round_up(size, page), page)`, with NULL and `ENOMEM`
> when the rounding overflows. `pvalloc_block` already proves
> `m >= size` for `m = (size + page - 1) & !(page - 1)` and allocates
> `m`; export it in the `ensures` of `api::libc::pvalloc`,
> `pvalloc_block` and the export, and mark interface row 8's R `P`.

Proved (#740): `pvalloc_block`, `api::libc::pvalloc` and the export
ensure `fits(r, page_round(size, page), page)` and
`size + (page - 1) > usize::MAX ==> r@.addr == 0`, and
`pvalloc_block`'s token gives `page_round(size, page)` bytes;
THEOREMS.md interface row 8's R is `P`.

**[#736](https://github.com/chainguard-project-theseus/challoc/issues/736): usable-size and interior-page queries on huge runs (no CVE id)**

> Property: for all pointers, `malloc_usable_size`, `sallocx` and
> `xallocx` of a huge page start return the run's usable bytes
> (`blk.len()` of its live token) or do not return, and an interior page
> start of a live run does not return. Done: `Huge::usable_size` and
> `api::libc::usable_size` state both over any live run's token, and the
> token is ghost (THEOREMS.md *(ghost token)*). Targets: `api::libc::usable_size`, `malloc_usable_size`,
> `api::jemalloc::sallocx`, `xallocx`, `Huge::usable_size`.

**[#737](https://github.com/chainguard-project-theseus/challoc/issues/737): linear overflow into a neighbour in the same region (CVE-2023-21367)**

> Property: a bound on how far a linear write off a slab block can reach
> before a guard slab (by class and guard geometry), and, for huge runs,
> a per-call export of `Inner::theorem_run_guards` (today it holds only
> at the huge lock invariant). Targets: `heap::theorem_overflow_reach`,
> `layout::guard_geom`, `huge::Inner::theorem_run_guards`,
> `Huge::alloc_run`. Note the PMD-chunk `thp:always` mode places no
> guards and must stay outside the claim.

Proved: `heap::theorem_guard_reach` (class regions, guards on) and
`huge::RunToken::theorem_token_guards` (huge runs, any two tokens),
THEOREMS.md "Same-region overflow reach".

**[#738](https://github.com/chainguard-project-theseus/challoc/issues/738): predictable allocation order (CVE-2023-21366)**

> Property: with `slot_randomize`, the order a refill hands out slots
> depends on the generator state (not only that the shuffle is a
> permutation, theorem 11). Unpredictability is probabilistic and likely
> outside Verus; the issue may end as documentation of what
> `slot_randomize` claims and why it is off by default. Targets:
> `class::shuffle_zero`, the refill's random word choice. Done for the
> shuffle: `shuffle_zero` ensures its result is `fisher_yates` over
> `draws(n, rng)`, Fisher-Yates is a bijection from valid draws to
> orders, and `Heap::rng` hands out only non-zero states, which
> `xorshift` keeps non-zero (THEOREMS.md 11). Done in
> [#749](https://github.com/chainguard-project-theseus/challoc/issues/749):
> `finish` ensures the state it returns is `xorshift_iter` of the state
> it took, one step per scanning pick, and asserts that the `j`-th
> scanning pick starts at word `xorshift_iter(j, st) % words`;
> `claim_with` ensures the state it returns is an iterate of the
> one it took; and `claim_random_zero` requires and ensures a non-zero
> state, which reaches the shuffle's entry non-zero. Intentionally not
> pursued: the distribution of the draws (probabilistic, outside what
> Verus states).

Not proposed as gaps: use after free by the program (class J, out of
scope by design) and the termination of abort paths (every abort is
proved as "does not return"; lock termination is a common premise).

## Reproducers that could become regression tests

- CVE-2026-0861: glibc's fix
  [c9188d33](https://sourceware.org/git/?p=glibc.git;a=commitdiff;h=c9188d333717d3ceb7e3020011651f424f749f93)
  extends `malloc/tst-malloc-too-large.c` to every power-of-two alignment
  up to `1 << 63`. The existing `c_cve_2018_6485` covers sizes near
  `SIZE_MAX` for alignments up to `2^62` and small sizes at `2^63`, but
  not sizes near `PTRDIFF_MAX` at `2^63` or `memalign` with
  non-power-of-two alignments in `[2^62 + 1, 2^63)`. A `c_cve_2026_0861`
  shape: `aligned_alloc` and `posix_memalign` at `1 << 63` with
  `PTRDIFF_MAX - k` (NULL, `ENOMEM`), and `memalign` at
  `(1 << 62) + 1` (NULL, `EINVAL`).
- CVE-2007-6754: no public reproducer found; the `c_cve_2018_6485`
  shapes exercise the same `size + align` rounding.
- how2heap
  [fastbin_dup](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/fastbin_dup.c)
  (`free(a); free(b); free(a)`): under `double_free_scan` the third free
  must abort at the call; without the key, document the flush behaviour.
  The existing `negative double_free_cached` is the back-to-back form.
- how2heap
  [house_of_spirit](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/house_of_spirit.c):
  a free of a forged, aligned fake chunk on the stack; `negative
  stack_free` covers the shape.
- how2heap
  [tcache_poisoning](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/tcache_poisoning.c):
  write a forged pointer into a freed block's first word, then `malloc`
  twice and check neither result is the forged address (theorem 46's
  shape as a run-time test).
- The other CVEs already have tests: `c_cve_2005_4895`,
  `c_cve_2006_7252`, `c_cve_2013_4332`, `c_cve_2017_17426`,
  `c_cve_2018_6485`, `c_cve_2018_6551`, `c_cve_2023_4911`
  (`tests/c/cve_shapes.c`, `tests/c/secure_conf.c`).

## Checked and left out

- musl: no CVE or advisory against `oldmalloc` or `mallocng` was found;
  musl's recent CVEs (regcomp, iconv, qsort) are not allocator bugs.
- glibc advisories 2023 to 2026: of the 28 `GLIBC-SA` files, only
  GLIBC-SA-2026-0001 (CVE-2026-0861) is in `malloc/`.
- CVE-2015-1781, CVE-2017-1000408, CVE-2017-1000409: not malloc bugs
  (resolver, `ld.so`), left out.
- CVE-2002-0391: `xdr_array`, an allocation-size overflow in a caller,
  not in an allocator; left out.
- CVE-2021-3470 (Redis with a non-jemalloc, non-glibc allocator): a
  caller's size bookkeeping, not an allocator bug.
- jemalloc 3.4.1 "race in the arenas.extend mallctl": not mapped; no
  theorem was checked against it.

## Sources

- CVE records: https://cveawg.mitre.org/api/cve/ (each id above), NVD
  pages linked in the table.
- glibc advisories: https://sourceware.org/git/?p=glibc.git;a=tree;f=advisories
- glibc commits: bcdaad21, a1a486d7, c9188d33 (links above).
- jemalloc ChangeLog: https://github.com/jemalloc/jemalloc/blob/dev/ChangeLog
- Android Security Bulletin, Android 14:
  https://source.android.com/docs/security/bulletin/android-14
- CISA ICSA-21-119-04 (BadAlloc):
  https://www.cisa.gov/uscert/ics/advisories/icsa-21-119-04
- FreeBSD r167872 (jemalloc `ipalloc`):
  http://svnweb.freebsd.org/base?view=revision&revision=167872
- how2heap: https://github.com/shellphish/how2heap
