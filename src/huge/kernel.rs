//! The huge allocator's page-protection calls (design 7): commits, guard
//! pages (`MADV_GUARD_INSTALL`/`REMOVE` or `PROT_NONE`) and the zeroing
//! write under the huge lock, and the theorem that a commit spares every
//! live run's guard.

use super::*;

verus! {

/// A commit, or a guard marker's removal, of `[addr, addr + len)` leaves
/// the guard page of every live run of `g` (the huge lock's value) but
/// `freed` inaccessible: the range lies in `h`'s region and shares no byte
/// with the [`GUARD_PAGES`] that end such a run (`Active`, `run_ok`).
/// `freed` is the run [`Huge::free_run`] is freeing, whose guard goes
/// with it: when it names a descriptor of `g`, the range is exactly that
/// run's guard. Elsewhere it is null, which names no descriptor
/// (`perms_ok`).
pub(super) open spec fn spares_guards(h: Huge, g: Inner, freed: *mut HugeDesc, addr: int, len: int) -> bool {
    let p = pow2(h.lg_page as nat) as int;
    &&& h.base <= addr
    &&& addr + len <= h.base + h.npages * p
    &&& forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && d != freed && g.descs@[d].value().state == RunState::Active as u8 ==> {
            let v = g.descs@[d].value();
            crate::layout::disjoint(addr, len, h.base + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
        }
    &&& g.descs@.contains_key(freed) ==> {
        let v = g.descs@[freed].value();
        addr == h.base + (v.start + v.pages - GUARD_PAGES) * p && len == GUARD_PAGES * p
    }
}

/// `[addr, addr + len)` shares no byte with any live run of `g` (`Active`),
/// its guard page included: bytes of a run being placed, which nobody
/// holds.
pub(super) open spec fn apart_from_runs(h: Huge, g: Inner, addr: int, len: int) -> bool {
    let p = pow2(h.lg_page as nat) as int;
    forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && g.descs@[d].value().state == RunState::Active as u8 ==> {
            let v = g.descs@[d].value();
            crate::layout::disjoint(addr, len, h.base + v.start * p, v.pages * p)
        }
}

/// `lk` is the permission of `h`'s lock cell ([`Lock::has_perm`]): only
/// a holder of the lock has it, from its guard
/// ([`LockGuard::perm`](crate::lock::LockGuard::perm)), so
/// its value is the lock's. And when `freed` names a descriptor of that
/// value, the run's [`RunToken`] has come back: its extent is out of the
/// run authority (`runs`), as [`Huge::free_run`] leaves it before it
/// frees the guard. A run still handed out keeps its token's extent there
/// (the authority agrees with every outstanding token, `tok.ext.agree` in
/// `free_run`), so `freed` names no such run.
pub(super) open spec fn held_by(h: Huge, lk: LockPerm<Inner>, freed: *mut HugeDesc) -> bool {
    let g = *lk.value();
    &&& h.inner.has_perm(lk)
    &&& g.descs@.contains_key(freed) ==> {
        let v = g.descs@[freed].value();
        !g.runs@@.contains((v.start as int, v.pages as int))
    }
}

/// Make `[addr, addr + len)` read-write. Every caller holds the huge lock
/// (`lk`, [`held_by`]) and proves the range apart from its live runs'
/// guards (`spares_guards`). On `Ok` its bytes are
/// [`committed`](crate::vspec::unbacked::committed).
pub(super) fn commit(addr: usize, len: usize, Ghost(h): Ghost<Huge>, Tracked(lk): Tracked<&LockPerm<Inner>>, Ghost(freed): Ghost<*mut HugeDesc>) -> (r: Result<(), Errno>)
    requires
        crate::sys::may_syscall(),
        held_by(h, *lk, freed),
        spares_guards(h, *lk.value(), freed, addr as int, len as int),
    ensures
        held_by(h, *lk, freed),
        spares_guards(h, *lk.value(), freed, addr as int, len as int),
        r is Ok ==> crate::vspec::unbacked::committed_range(addr as int, len as int),
{
    // SAFETY: inside our own reservation; adding permissions to pages
    // nobody holds cannot break anything.
    unsafe { sys::mprotect(addr, len, PROT_RW) }
}

pub(super) fn protect_none(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: the page is a guard that no caller may touch.
    unsafe { sys::mprotect(addr, len, PROT_NONE) }
}

/// Make the guard page at `guard` `PROT_NONE` for the run at `run`,
/// whose first page and the guard are committed and in one VMA. The
/// split VMAs must be able to merge again when the run is freed and the
/// guard made read-write (`free_run`), and the kernel merges two anonymous
/// VMAs only when they share an `anon_vma`, which a VMA receives at its
/// first write fault (from a compatible neighbour when one has it, the
/// previous run's guard here, else a new one). Split before that fault,
/// the run and its guard would each get their own on first touch and
/// never merge back, leaving one VMA per run ever placed after every run
/// was freed (measured). So the run's first page is populated for write
/// first (`MADV_POPULATE_WRITE`, kernel 5.14, the floor; it writes
/// nothing, the page still reads as zero, and the caller is about to
/// touch it), and both halves of the split inherit the `anon_vma`. If
/// the kernel refuses the populate (a seccomp filter: `EINVAL`, `EPERM` or
/// `ENOSYS`), a zero written to
/// the run's first byte faults the page in the same way ([`write_zero`]).
/// The cost is the run's first page resident from the start (a whole
/// 2 MiB page for a PMD-aligned run under host THP `always`). `populate`
/// may be false for a run inside the mark in the fallback mode: every VMA
/// there was split off one populated this way, so it has its `anon_vma`.
///
/// The caller holds the huge lock (`lk`, [`held_by`]), and the run's first
/// page lies in the region apart from every live run's guard
/// (`spares_guards`) and from every live run (`apart_from_runs`); when it
/// populates, the run's first byte is
/// [`committed`](crate::vspec::unbacked::committed).
pub(super) fn split_guard(run: usize, guard: usize, page: usize, populate: bool, Ghost(h): Ghost<Huge>, Tracked(lk): Tracked<&LockPerm<Inner>>) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
        held_by(h, *lk, vstd::raw_ptr::ptr_null_mut()),
        !lk.value().descs@.contains_key(vstd::raw_ptr::ptr_null_mut()),
        spares_guards(h, *lk.value(), vstd::raw_ptr::ptr_null_mut(), run as int, page as int),
        apart_from_runs(h, *lk.value(), run as int, page as int),
        1 <= page,
        populate ==> crate::vspec::unbacked::committed(run as int),
{
    proof {
        // The first byte of that page spares the guards as the page does.
        let g = *lk.value();
        assert forall|d: *mut HugeDesc|
            #[trigger] g.descs@.contains_key(d) && d != vstd::raw_ptr::ptr_null_mut() && g.descs@[d].value().state == RunState::Active as u8 implies {
                let v = g.descs@[d].value();
                let p = pow2(h.lg_page as nat) as int;
                crate::layout::disjoint(run as int, 1, h.base + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
            } by {}
        assert(spares_guards(h, g, vstd::raw_ptr::ptr_null_mut(), run as int, 1));
    }
    if populate {
        // SAFETY: the first page of a run being placed, which nobody
        // holds yet; populating leaves its contents alone.
        if let Err(e) = unsafe { sys::madvise(run, page, MADV_POPULATE_WRITE) } {
            // A filter's refusal (`EINVAL`, `EPERM`, `ENOSYS`) falls back
            // to the write; anything else (`ENOMEM` from a memory cgroup,
            // `EHWPOISON`) is a resource failure the write would only
            // meet again as a fault, so the run is not handed out
            // (design 14.1).
            if e.0 == Errno::EINVAL.0 || e.0 == Errno::EPERM.0 || e.0 == Errno::ENOSYS.0 {
                write_zero(run, Ghost(h), Tracked(lk));
            } else {
                return Err(e);
            }
        }
    }
    protect_none(guard, GUARD_PAGES * page)
}

/// Write a zero to the byte at `addr`, the first byte of a run being
/// placed (`split_guard`), to give its VMA an `anon_vma` by a write fault
/// when `MADV_POPULATE_WRITE` is refused.
///
/// Trusted only under its `requires`: the caller holds the huge lock
/// (`lk`, [`held_by`]), `addr` was made read-write at some point
/// ([`committed`](crate::vspec::unbacked::committed)), the byte lies in
/// the region apart from every live run's guard (`spares_guards`), and it
/// belongs to no live run (`apart_from_runs`), so nobody holds it. A zero
/// leaves a page that read as zero reading as zero, so the zero tokens
/// (`axiom_fresh_anon_zero`, `axiom_discarded_zero`) stay true, and a
/// dirty `Empty` page's contents are not needed.
///
/// Limit, the trusted part: `committed` never expires, so the `requires`
/// does not rule out a byte made `PROT_NONE` after its commit and outside
/// every live run's guard. Below the mark in guard-install mode, with no
/// commit in the same `prepare`, that the byte is read-write rests on
/// [`Huge::mixed_guards`] being set whenever such a page may exist (a
/// guard refused its marker, or a guard whose release failed).
#[verifier::external_body]
pub(super) fn write_zero(addr: usize, Ghost(h): Ghost<Huge>, Tracked(lk): Tracked<&LockPerm<Inner>>)
    requires
        held_by(h, *lk, vstd::raw_ptr::ptr_null_mut()),
        crate::vspec::unbacked::committed(addr as int),
        spares_guards(h, *lk.value(), vstd::raw_ptr::ptr_null_mut(), addr as int, 1),
        apart_from_runs(h, *lk.value(), addr as int, 1),
{
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(addr as *mut u8, 0) };
}

pub(super) fn guard_install(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: as `protect_none`.
    unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
}

/// Remove the guard marker on `[addr, addr + len)`, the guard of the run
/// being freed. Every caller proves the range apart from every other live
/// run's guard (`spares_guards`), holding the huge lock (`lk`,
/// [`held_by`]).
pub(super) fn guard_remove(addr: usize, len: usize, Ghost(h): Ghost<Huge>, Tracked(lk): Tracked<&LockPerm<Inner>>, Ghost(freed): Ghost<*mut HugeDesc>) -> (r: Result<(), Errno>)
    requires
        crate::sys::may_syscall(),
        held_by(h, *lk, freed),
        spares_guards(h, *lk.value(), freed, addr as int, len as int),
    ensures
        held_by(h, *lk, freed),
        spares_guards(h, *lk.value(), freed, addr as int, len as int),
{
    // SAFETY: the run is being freed; the guard protects nothing now.
    unsafe { sys::madvise(addr, len, MADV_GUARD_REMOVE) }
}

/// Null names no descriptor of `g`, and every live run of `g` ends by
/// `watermark`, has its guard page, and shares no page with
/// `[s, s + need)`: what the placement [`Huge::find`] returns has, at the
/// mark.
pub(super) open spec fn runs_apart(g: Inner, s: int, need: int, watermark: int) -> bool {
    &&& !g.descs@.contains_key(vstd::raw_ptr::ptr_null_mut())
    &&& forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && g.descs@[d].value().state == RunState::Active as u8 ==> {
            let v = g.descs@[d].value();
            &&& v.pages >= GUARD_PAGES
            &&& v.start + v.pages <= watermark
            &&& (v.start + v.pages <= s || s + need <= v.start)
        }
}

/// The first page of a placement at `s` ([`runs_apart`]) shares no byte
/// with any live run of `g` ([`apart_from_runs`]): each such run ends by
/// `s` or starts at or past `s + need`, with `need` a page or more.
pub(super) proof fn lemma_page_apart_from_runs(h: Huge, g: Inner, s: int, need: int, watermark: int)
    requires
        h.inv(),
        0 <= s,
        1 <= need,
        runs_apart(g, s, need, watermark),
    ensures
        apart_from_runs(h, g, h.base + s * pow2(h.lg_page as nat), pow2(h.lg_page as nat) as int),
{
    let p = pow2(h.lg_page as nat) as int;
    vstd::arithmetic::power2::lemma_pow2_pos(h.lg_page as nat);
    assert forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && g.descs@[d].value().state == RunState::Active as u8 implies {
            let v = g.descs@[d].value();
            crate::layout::disjoint(h.base + s * p, p, h.base + v.start * p, v.pages * p)
        } by {
        let v = g.descs@[d].value();
        let (a, n) = (v.start as int, v.pages as int);
        assert(a * p + n * p == (a + n) * p) by (nonlinear_arith);
        assert(s * p + p == (s + 1) * p) by (nonlinear_arith);
        if a + n <= s {
            vstd::arithmetic::mul::lemma_mul_inequality(a + n, s, p);
        } else {
            vstd::arithmetic::mul::lemma_mul_inequality(s + 1, a, p);
        }
    }
}

/// Pages `x..y` of `h`'s region spare the guards ([`spares_guards`]) of
/// the live runs of `g` but `freed` when each of those ends by `x` or has
/// its guard page at or past `y`.
pub(super) proof fn lemma_pages_spare_guards(h: Huge, g: Inner, freed: *mut HugeDesc, x: int, y: int)
    requires
        h.inv(),
        0 <= x <= y <= h.npages,
        g.descs@.contains_key(freed) ==> {
            let v = g.descs@[freed].value();
            x == v.start + v.pages - GUARD_PAGES && y == v.start + v.pages
        },
        forall|d: *mut HugeDesc|
            #[trigger] g.descs@.contains_key(d) && d != freed && g.descs@[d].value().state == RunState::Active as u8 ==> {
                let v = g.descs@[d].value();
                v.start + v.pages <= x || y <= v.start + v.pages - GUARD_PAGES
            },
    ensures
        spares_guards(h, g, freed, h.base + x * pow2(h.lg_page as nat), (y - x) * pow2(h.lg_page as nat)),
{
    let p = pow2(h.lg_page as nat) as int;
    vstd::arithmetic::power2::lemma_pow2_pos(h.lg_page as nat);
    assert(x * p + (y - x) * p == y * p) by (nonlinear_arith);
    vstd::arithmetic::mul::lemma_mul_inequality(y, h.npages as int, p);
    assert forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && d != freed && g.descs@[d].value().state == RunState::Active as u8 implies {
            let v = g.descs@[d].value();
            crate::layout::disjoint(h.base + x * p, (y - x) * p, h.base + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
        } by {
        let v = g.descs@[d].value();
        let e = v.start + v.pages;
        assert((e - GUARD_PAGES) * p + GUARD_PAGES * p == e * p) by (nonlinear_arith);
        if e <= x {
            vstd::arithmetic::mul::lemma_mul_inequality(e, x, p);
        } else {
            vstd::arithmetic::mul::lemma_mul_inequality(y, e - GUARD_PAGES, p);
        }
    }
}

/// The guard page of the live run `d` of `g` ([`spares_guards`] with `d`
/// as the run being freed): apart from every other live run's guard, as
/// live extents are pairwise apart (`Inner::wf`).
pub(super) proof fn lemma_freed_guard_apart(h: Huge, g: Inner, d: *mut HugeDesc)
    requires
        h.inv(),
        g.wf(),
        g.spec_np() == h.npages,
        g.descs@.contains_key(d),
        g.descs@[d].value().state == RunState::Active as u8,
    ensures
        ({
            let (v, p) = (g.descs@[d].value(), pow2(h.lg_page as nat) as int);
            spares_guards(h, g, d, h.base + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
        }),
{
    g.lemma_wf_for_take();
    let v = g.descs@[d].value();
    assert(desc_ok(v, g.next_page));
    assert(run_ok(v, g.lg@));
    assert forall|x: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(x) && x != d && g.descs@[x].value().state == RunState::Active as u8 implies {
            let w = g.descs@[x].value();
            w.start + w.pages <= v.start + v.pages - GUARD_PAGES || v.start + v.pages <= w.start + w.pages - GUARD_PAGES
        } by {
        assert(apart(g.descs@[x].value(), v));
        assert(run_ok(g.descs@[x].value(), g.lg@));
    }
    lemma_pages_spare_guards(h, g, d, v.start + v.pages - GUARD_PAGES, v.start + v.pages);
}

/// Commits keep the guard pages of live huge runs inaccessible: every
/// call of [`commit`] (`mprotect` read-write) or [`guard_remove`]
/// (`MADV_GUARD_REMOVE`) is made holding the huge lock, whose cell's
/// permission `lk` it takes from the caller's guard (`held_by`, so `g`,
/// the value `lk` holds, is the lock's), and makes accessible only bytes
/// of `h`'s region, and none of the guard page of any live run of `g`
/// other than `freed`, the run [`Huge::free_run`] is freeing. When
/// `freed` names a descriptor of `g`, its run's token has come back to
/// the lock (`held_by`: no live run's has), and the range is exactly that
/// run's guard page (`spares_guards`), so the exception covers nothing
/// else. Both ensure what their precondition asks of each caller
/// ([`Huge::prepare`] for a run being placed, [`Huge::free_run`]), so
/// Verus rejects any caller that has not proved it. For the
/// process's huge allocator ([`Huge::is_process`]) that region is the huge
/// region, apart from every class region and the metadata region
/// (`Regions::separated`), so such a commit touches no class guard slab
/// either. Under `guards:false` no run has a guard to keep; the commits
/// stay apart from the pages all the same.
///
/// Not covered: a freed run's guard, which `free_run` makes read-write
/// again (or whose marker it removes) so that its VMA merges back; and
/// that `prepare` puts each run's guard on the page this names (its
/// `guard_install` and `split_guard` calls, read from the code). Trusted:
/// `mprotect` and `madvise` change only the range they are given.
pub(super) proof fn theorem_huge_commit_avoids_guards(h: Huge, lk: LockPerm<Inner>, freed: *mut HugeDesc, addr: usize, len: usize, r: Result<(), Errno>, d: *mut HugeDesc)
    requires
        call_ensures(commit, (addr, len, Ghost(h), Tracked(&lk), Ghost(freed)), r)
            || call_ensures(guard_remove, (addr, len, Ghost(h), Tracked(&lk), Ghost(freed)), r),
        lk.value().descs@.contains_key(d),
        d != freed,
        lk.value().descs@[d].value().state == RunState::Active as u8,
    ensures
        held_by(h, lk, freed),
        h.spec_base() <= addr,
        addr + len <= h.spec_base() + h.spec_npages() * h.spec_page(),
        ({
            let (v, p) = (lk.value().descs@[d].value(), h.spec_page());
            crate::layout::disjoint(addr as int, len as int, h.spec_base() + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
        }),
        h.is_process() ==> ({
            let (r, region) = (crate::layout::process_regions(), (1usize << crate::layout::process_regions().lg_region) as int);
            &&& forall|c: int| 0 <= c < crate::layout::NCLASSES ==> crate::layout::disjoint(addr as int, len as int, #[trigger] r.class_base[c] as int, region)
            &&& crate::layout::disjoint(addr as int, len as int, r.meta_base as int, r.meta_len as int)
        }),
{
    if h.is_process() {
        let r = crate::layout::process_regions();
        let (stride, region) = ((1usize << r.lg_stride) as int, (1usize << r.lg_region) as int);
        let lg = r.lg_page as usize;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(r.huge_len >> lg, lg, r.huge_len);
        vstd::bits::lemma_usize_shl_is_mul(1, lg);
        assert(h.npages * h.spec_page() <= r.huge_len);
        assert forall|c: int| 0 <= c < crate::layout::NCLASSES implies crate::layout::disjoint(addr as int, len as int, #[trigger] r.class_base[c] as int, region) by {
            assert(r.superblock + c * stride <= r.class_base[c] && r.class_base[c] + region <= r.superblock + (c + 1) * stride);
            vstd::arithmetic::mul::lemma_mul_inequality(c + 1, crate::layout::NCLASSES as int, stride);
        }
    }
}

} // verus!
