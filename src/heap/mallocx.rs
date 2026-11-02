//! `MALLOCX_*` flags (design 13.2): the jemalloc flag layout
//! ([`Flags::decode`], [`Tcache`], the `mallocx_*` builders) and the
//! routing of a `mallocx` / `dallocx` / `sdallocx` call onto the calling
//! thread's heap, an explicit cache ([`Tcache::Explicit`], a `Heap` owned
//! by no thread, `heap/tcaches.rs`) or the direct paths
//! ([`Tcache::None`], straight against the slab bitmaps).

use core::ptr;
use vstd::prelude::verus;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked};

use super::Shared;
#[cfg(verus_keep_ghost)]
use crate::lock::axiom_entry;
use crate::vspec::errno::ThreadErrno;
use crate::vspec::zero::Zeroed;

verus! {

/// `MALLOCX_ZERO`.
pub const MALLOCX_ZERO: u32 = 0x40;
/// `MALLOCX_TCACHE_NONE` = `MALLOCX_TCACHE(-1)`.
#[cfg(test)]
pub const MALLOCX_TCACHE_NONE: u32 = 1 << 8;
const LG_ALIGN_MASK: u32 = 0x3f;
const TCACHE_SHIFT: u32 = 8;
const TCACHE_MASK: u32 = 0xfff;
const ARENA_SHIFT: u32 = 20;
const ARENA_MASK: u32 = 0xfff;

// jemalloc.h builds the flags with these macros (tests/c/common.h
// copies all but MALLOCX_ALIGN):
//
//     #define MALLOCX_LG_ALIGN(la) ((int)(la))
//     #define MALLOCX_ALIGN(a)     /* MALLOCX_LG_ALIGN(ffs(a) - 1) */
//     #define MALLOCX_ZERO         ((int)0x40)
//     #define MALLOCX_TCACHE(tc)   ((int)(((tc) + 2) << 8))
//     #define MALLOCX_TCACHE_NONE  MALLOCX_TCACHE(-1)
//     #define MALLOCX_ARENA(a)     ((((int)(a)) + 1) << 20)
//
// The `*_spec` functions below transcribe them (a shift by `n` as a
// product with `2^n`), and `Flags::encode` ORs them together. They are
// the specification `Flags::decode` is proved against, written from the
// macros rather than from the decoder.

/// `MALLOCX_LG_ALIGN(la)`: `la`.
pub open spec fn mallocx_lg_align_spec(la: u32) -> u32 {
    la
}

/// `MALLOCX_TCACHE(tc)`: `(tc + 2) << 8`; `MALLOCX_TCACHE_NONE` is
/// `mallocx_tcache_spec(-1)`.
pub open spec fn mallocx_tcache_spec(tc: int) -> u32 {
    ((tc + 2) * 0x100) as u32
}

/// `MALLOCX_ARENA(a)`: `(a + 1) << 20`.
pub open spec fn mallocx_arena_spec(a: int) -> u32 {
    ((a + 1) * 0x10_0000) as u32
}

/// `MALLOCX_LG_ALIGN(la)`.
#[cfg(any(test, verus_keep_ghost))]
pub const fn mallocx_lg_align(la: u32) -> (r: u32)
    ensures
        la < 64 ==> r == mallocx_lg_align_spec(la),
{
    proof {
        assert(la < 64 ==> la & 0x3f == la) by (bit_vector);
    }
    la & LG_ALIGN_MASK
}

/// `MALLOCX_TCACHE(t)` for an explicit cache id.
#[cfg(any(test, verus_keep_ghost))]
pub const fn mallocx_tcache(t: u16) -> (r: u32)
    ensures
        t < 0xffe ==> r == mallocx_tcache_spec(t as int),
{
    proof {
        let c = (t + 2) as u32;
        assert(c < 0x1000 ==> (c & 0xfff) << 8u32 == c * 0x100) by (bit_vector);
    }
    ((t as u32 + 2) & TCACHE_MASK) << TCACHE_SHIFT
}

/// `MALLOCX_ARENA(a)`.
#[cfg(any(test, verus_keep_ghost))]
pub const fn mallocx_arena(a: u16) -> (r: u32)
    ensures
        a < 0xfff ==> r == mallocx_arena_spec(a as int),
{
    proof {
        let c = (a + 1) as u32;
        assert(c < 0x1000 ==> (c & 0xfff) << 20u32 == c * 0x10_0000) by (bit_vector);
    }
    ((a as u32 + 1) & ARENA_MASK) << ARENA_SHIFT
}

/// Which cache a `MALLOCX` call goes through.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tcache {
    /// The calling thread's heap.
    Default,
    /// No cache: straight to the slab bitmaps (`MALLOCX_TCACHE_NONE`).
    None,
    /// An explicit cache from `tcache.create`.
    Explicit(u16),
}

/// Decoded `MALLOCX_*` flags.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Flags {
    /// `MALLOCX_LG_ALIGN`; 0 means the natural alignment.
    pub lg_align: u32,
    /// `MALLOCX_ZERO`.
    pub zero: bool,
    pub tcache: Tcache,
    /// `MALLOCX_ARENA`; `None` means the caller's (or the cache's) arena.
    pub arena: Option<u16>,
}

/// The macros' encoding of a cache choice: no flag for the default,
/// `MALLOCX_TCACHE_NONE`, or `MALLOCX_TCACHE(t)`.
pub open spec fn tcache_bits(tc: Tcache) -> u32 {
    match tc {
        Tcache::Default => 0,
        Tcache::None => mallocx_tcache_spec(-1),
        Tcache::Explicit(t) => mallocx_tcache_spec(t as int),
    }
}

/// The macros' encoding of an arena choice: no flag for the default, or
/// `MALLOCX_ARENA(a)`.
pub open spec fn arena_bits(arena: Option<u16>) -> u32 {
    match arena {
        None => 0,
        Some(a) => mallocx_arena_spec(a as int),
    }
}

/// The value `(flags >> 8) & 0xfff` that [`tcache_bits`] puts in bits
/// 8-19.
spec fn tcache_code(tc: Tcache) -> u32 {
    match tc {
        Tcache::Default => 0,
        Tcache::None => 1,
        Tcache::Explicit(t) => (t + 2) as u32,
    }
}

/// The value `(flags >> 20) & 0xfff` that [`arena_bits`] puts in bits
/// 20-31.
spec fn arena_code(arena: Option<u16>) -> u32 {
    match arena {
        None => 0,
        Some(a) => (a + 1) as u32,
    }
}

/// [`Flags::decode`]'s result, field by field from jemalloc's bit layout:
/// what the specs of the `mallocx` entry points name. `decode` ensures it
/// is its result, which [`Flags::lemma_round_trip`] shows is the one
/// well-formed `Flags` the macros encode as `flags`.
pub open spec fn decode_spec(flags: u32) -> Flags {
    let t = (flags >> 8u32) & 0xfff;
    let a = (flags >> 20u32) & 0xfff;
    Flags {
        lg_align: flags & 0x3f,
        zero: flags & 0x40 != 0,
        tcache: if t == 0 {
            Tcache::Default
        } else if t == 1 {
            Tcache::None
        } else {
            Tcache::Explicit((t - 2) as u16)
        },
        arena: if a == 0 { None } else { Some((a - 1) as u16) },
    }
}

impl Flags {
    /// The fields fit the macros' bits: `lg_align` in 6, a cache id or an
    /// arena index in 12 once offset by 2 or 1.
    pub open spec fn wf(self) -> bool {
        &&& self.lg_align < 64
        &&& self.tcache matches Tcache::Explicit(t) ==> t < 0xffe
        &&& self.arena matches Some(a) ==> a < 0xfff
    }

    /// The flags a caller builds for `self` with jemalloc's macros: the OR
    /// of each field's encoding, with an absent field left out.
    pub open spec fn encode(self) -> u32 {
        mallocx_lg_align_spec(self.lg_align) | (if self.zero {
            0x40
        } else {
            0
        }) | tcache_bits(self.tcache) | arena_bits(self.arena)
    }

    /// Each field of a well-formed `f` reads back from its bits of
    /// `f.encode()`, and bit 7 is clear.
    proof fn lemma_fields(f: Flags)
        requires
            f.wf(),
        ensures
            f.encode() & 0x3f == f.lg_align,
            (f.encode() & 0x40 != 0) == f.zero,
            (f.encode() >> 8u32) & 0xfff == tcache_code(f.tcache),
            (f.encode() >> 20u32) & 0xfff == arena_code(f.arena),
            f.encode() & 0x80 == 0,
    {
        let x = f.encode();
        let lg = f.lg_align;
        let z: u32 = if f.zero { 0x40 } else { 0 };
        let c = tcache_code(f.tcache);
        let d = arena_code(f.arena);
        let tb = tcache_bits(f.tcache);
        let ab = arena_bits(f.arena);
        assert(c < 0x1000 && tb == c * 0x100);
        assert(d < 0x1000 && ab == d * 0x10_0000);
        assert(x & 0x3f == lg && (x & 0x40 != 0) == (z == 0x40) && (x >> 8u32) & 0xfff == c
            && (x >> 20u32) & 0xfff == d && x & 0x80 == 0) by (bit_vector)
            requires
                x == lg | z | tb | ab,
                lg < 64,
                z == 0 || z == 0x40,
                c < 0x1000,
                tb == c * 0x100,
                d < 0x1000,
                ab == d * 0x10_0000,
        ;
    }

    /// Decoding inverts the macros: when `r` is what [`Flags::decode`]
    /// returns for the flags built for a well-formed `f` (its ensures with
    /// `flags == f.encode()`), `r` is `f`. So `MALLOCX_TCACHE(t)` decodes
    /// to `Tcache::Explicit(t)`, `MALLOCX_TCACHE_NONE` to `Tcache::None`,
    /// `MALLOCX_ARENA(a)` to `Some(a)` and `MALLOCX_LG_ALIGN(la)` to `la`;
    /// a field with no flag decodes to its default (`Tcache::Default`,
    /// `None`, 0, no zeroing); and an OR of several decodes each field
    /// independently.
    pub proof fn lemma_round_trip(f: Flags, r: Flags)
        requires
            f.wf(),
            r.wf(),
            r.encode() == f.encode() & !0x80u32,
        ensures
            r == f,
    {
        Self::lemma_fields(f);
        Self::lemma_fields(r);
        let x = f.encode();
        assert(x & !0x80u32 == x) by (bit_vector)
            requires
                x & 0x80 == 0,
        ;
    }

    /// Decode jemalloc's layout: bits 0-5 `lg_align`, bit 6 `ZERO`, bits
    /// 8-19 tcache (0 default, 1 none, `t + 2` explicit `t`), bits 20-31
    /// arena (0 default, `a + 1` arena `a`). Bit 7 is unused and ignored.
    /// The result is the well-formed `Flags` that the macros encode as
    /// `flags` ([`Flags::lemma_round_trip`] shows there is only one).
    pub const fn decode(flags: u32) -> (r: Flags)
        ensures
            r.wf(),
            r.encode() == flags & !0x80u32,
            r == decode_spec(flags),
    {
        let t = (flags >> TCACHE_SHIFT) & TCACHE_MASK;
        let a = (flags >> ARENA_SHIFT) & ARENA_MASK;
        proof {
            assert(t <= 0xfff && a <= 0xfff && flags & 0x3f < 64) by (bit_vector)
                requires t == (flags >> 8u32) & 0xfff, a == (flags >> 20u32) & 0xfff;
        }
        let r = Flags {
            lg_align: flags & LG_ALIGN_MASK,
            zero: flags & MALLOCX_ZERO != 0,
            tcache: match t {
                0 => Tcache::Default,
                1 => Tcache::None,
                _ => Tcache::Explicit((t - 2) as u16),
            },
            arena: if a == 0 { None } else { Some((a - 1) as u16) },
        };
        proof {
            let lg = r.lg_align;
            let z: u32 = if r.zero { 0x40 } else { 0 };
            let tb = tcache_bits(r.tcache);
            let ab = arena_bits(r.arena);
            assert(tb == t * 0x100);
            assert(ab == a * 0x10_0000);
            assert(flags & !0x80u32 == lg | z | tb | ab) by (bit_vector)
                requires
                    t == (flags >> 8u32) & 0xfff,
                    a == (flags >> 20u32) & 0xfff,
                    lg == flags & 0x3f,
                    z == (if flags & 0x40 != 0 { 0x40u32 } else { 0u32 }),
                    tb == t * 0x100,
                    ab == a * 0x10_0000,
            ;
        }
        r
    }

    /// The requested alignment in bytes (1 when none).
    pub const fn align(&self) -> (r: usize)
        requires
            self.lg_align < 64,
        ensures
            r == 1usize << self.lg_align,
    {
        1usize << self.lg_align
    }
}

/// `MALLOCX_TCACHE(t)` names an explicit cache id no `tcache.create` hands
/// out (`MAX_TCACHES` or more; the 12-bit field reaches 4093): every path
/// that looks the cache up finds none and aborts by name
/// ([`invalid_tcache`]).
pub open spec fn bad_tcache(t: Tcache) -> bool {
    t matches Tcache::Explicit(i) && i as usize >= super::tcaches::MAX_TCACHES
}

#[cold]
#[inline(never)]
fn invalid_tcache(id: u16) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("MALLOCX_TCACHE({id}): no such explicit cache"))
}

/// Routing of a `MALLOCX` allocation of a non-huge class on the calling
/// thread. `None` from the arena lookup (an index `arenas.create` never
/// handed out) gives null. The flag says the memory is known zero; `blk`
/// gets the block's token. `arena` is never [`FOREIGN`](super::FOREIGN)
/// (`Flags::decode` gives at most 4094): an explicit arena is popped
/// from through the home cache or its own `others` node
/// (`Heap::alloc_in`).
// The nested `if` stands for a let chain, which Verus does not support.
#[allow(clippy::collapsible_if)]
pub fn alloc(
    shared: &Shared,
    class: usize,
    arena: Option<u16>,
    tcache: Tcache,
    Tracked(blk): Tracked<&mut super::Live>,
    errno: Tracked<&mut ThreadErrno>,
) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        class < crate::layout::NCLASSES,
        shared.wf(),
        arena matches Some(a) ==> a != super::FOREIGN,
    ensures
        r.0@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r.0@.addr),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, crate::layout::class_table()[class as int].size as int),
        r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
        !bad_tcache(tcache),
{
    // The two caches take a slot token: its cell goes down to the tick,
    // which may start the background thread, and the default cache
    // borrows the heap from its loan. No cache, no token: that arm
    // reaches the slot through `existing_heap`.
    match tcache {
        Tcache::Default => {
            let tracked tls = crate::sys::tls::TlsSlot::entry();
            match super::thread::heap_for_thread_in(shared, Tracked(&mut tls.cell), Tracked(&mut tls.loan)) {
                Some(h) => {
                    proof {
                        h.lemma_wf_regions();
                    }
                    match arena {
                        Some(a) => {
                            if no_arena(shared, a) {
                                return (ptr::null_mut(), false, Tracked(Zeroed::empty(0)));
                            }
                            let (p, zero) = h.alloc_in(shared, a, class, Tracked(blk), errno, Tracked(&mut tls.cell));
                            let ghost take = p@.addr != 0 && zero;
                            let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
                            (p, zero, z)
                        },
                        None => {
                            let (p, zero) = h.alloc_zeroed(shared, class, Tracked(blk), errno, Tracked(&mut tls.cell));
                            let ghost take = p@.addr != 0 && zero;
                            let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
                            (p, zero, z)
                        },
                    }
                },
                None => alloc_direct(shared, class, arena.unwrap_or(0), arena.is_none(), Tracked(blk)),
            }
        },
        Tcache::Explicit(t) => {
            let tracked tls = crate::sys::tls::TlsSlot::entry();
            // Premise (`axiom_heap_use`): the caller uses cache `t` on no other
            // thread (the `MALLOCX_TCACHE` contract).
            let tracked mut u = super::axiom_heap_use();
            let Some(h) = shared.tcaches.get(t, Tracked(&mut u)) else {
                invalid_tcache(t)
            };
            proof {
                h.lemma_wf_regions();
            }
            // As for the default cache: only an explicit arena pins a
            // shared class to it (`Heap::alloc_in`).
            match arena {
                Some(a) => {
                    if no_arena(shared, a) {
                        return (ptr::null_mut(), false, Tracked(Zeroed::empty(0)));
                    }
                    let (p, zero) = h.alloc_in(shared, a, class, Tracked(blk), errno, Tracked(&mut tls.cell));
                    let ghost take = p@.addr != 0 && zero;
                    let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
                    (p, zero, z)
                },
                None => {
                    let (p, zero) = h.alloc_zeroed(shared, class, Tracked(blk), errno, Tracked(&mut tls.cell));
                    let ghost take = p@.addr != 0 && zero;
                    let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
                    (p, zero, z)
                },
            }
        }
        Tcache::None => {
            let a = arena
                .or_else(|| {
                    crate::heap::existing_heap(crate::heap::shared_ptr(shared)).map(|h| h.arena())
                })
                .unwrap_or(0);
            let (p, zero, z) = alloc_direct(shared, class, a, arena.is_none(), Tracked(blk));
            if !p.is_null() {
                if let Some(h) = crate::heap::existing_heap(crate::heap::shared_ptr(shared)) {
                    h.count_direct_alloc(class);
                }
            }
            (p, zero, z)
        }
    }
}

/// Whether an explicit `MALLOCX_ARENA(a)` names no arena or one that
/// cannot be built: the request is NULL then on every route, even when
/// `a` is the thread's own home, which an implicit request would serve
/// from arena 0 (design 14.1).
fn no_arena(shared: &Shared, a: u16) -> bool
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
{
    // Premise (lock order): called from the `mallocx` entry points, which
    // hold no lock.
    let tracked mut ord = axiom_entry();
    shared.arena(a, Tracked(&mut ord)).is_none()
}

/// One slot of `class` straight from `arena`'s slab bitmaps. When
/// `arena` is the thread's own home (`implicit`) and cannot be built (the
/// metadata region is full, design 14.1), from arena 0 instead, as the
/// `malloc` slow path does; an explicit `MALLOCX_ARENA` that does not
/// exist is NULL.
fn alloc_direct(
    shared: &Shared,
    class: usize,
    arena: u16,
    implicit: bool,
    Tracked(blk): Tracked<&mut super::Live>,
) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        class < crate::layout::NCLASSES,
    ensures
        r.0@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r.0@.addr),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, crate::layout::class_table()[class as int].size as int),
        r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
{
    // Premise (lock order): called from the `mallocx` entry points, which
    // hold no lock.
    let tracked mut ord = axiom_entry();
    match shared.arena(arena, Tracked(&mut ord)) {
        Some((a, w)) => {
            shared.alloc_direct(a, w, class, Tracked(blk), Tracked(&mut ord))
        }
        None if implicit => {
            proof {
                shared.lemma_wf_slow();
            }
            shared.alloc_direct(&shared.arena0, shared.arena0_published(), class, Tracked(blk), Tracked(&mut ord))
        }
        None => (ptr::null_mut(), false, Tracked(Zeroed::empty(0))),
    }
}

/// Routing of a `MALLOCX` huge allocation on the calling thread (caches
/// play no part; the arena is recorded in the run's descriptor). `blk`
/// gets the run's token.
pub fn alloc_huge(
    shared: &Shared,
    size: usize,
    align: usize,
    arena: Option<u16>,
    tcache: Tcache,
    zero: bool,
    Tracked(blk): Tracked<&mut super::Live>,
) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        crate::meta::is_pow2(align),
    ensures
        r.0@.addr != 0 ==> crate::layout::huge_block(r.0@.addr, size, align),
        r.0@.addr != 0 ==> size <= shared.huge_usable(size),
        r.0@.addr != 0 ==> shared.huge_ok(size),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, shared.huge_usable(size)),
        r.0@.addr != 0 ==> final(blk).block(r.0) && final(blk).len() == shared.huge_usable(size),
        r.0@.addr != 0 ==> crate::vspec::zero::huge_placeable(size, align),
        arena is None ==> !bad_tcache(tcache),
{
    // Premise (`axiom_heap_use`): the caller uses the explicit cache
    // `tcache` names on no other thread (the `MALLOCX_TCACHE` contract).
    let tracked mut u = super::axiom_heap_use();
    let a = match (arena, tcache) {
        (Some(a), _) => a,
        (None, Tcache::Explicit(t)) => match shared.tcaches.get(t, Tracked(&mut u)) {
            Some(h) => h.arena(),
            None => invalid_tcache(t),
        },
        (None, _) => {
            crate::heap::existing_heap(crate::heap::shared_ptr(shared)).map_or(0, |h| h.arena())
        }
    };
    // Premise (lock order): called from the `mallocx` entry points, which
    // hold no lock.
    let tracked mut ord = axiom_entry();
    // An explicit `MALLOCX_ARENA` that does not exist is NULL; the
    // thread's own home that cannot be built leaves the run to arena 0
    // (`Shared::alloc_huge`, design 14.1).
    if arena.is_some() && shared.arena(a, Tracked(&mut ord)).is_none() {
        return (ptr::null_mut(), false, Tracked(Zeroed::empty(0)));
    }
    match crate::heap::heap_for_thread(shared) {
        Some(h) => h.alloc_huge_in(shared, size, align, a, zero, Tracked(blk)),
        None => shared.alloc_huge(size, align, a, zero, Tracked(blk), Tracked(&mut ord)),
    }
}

/// Routing of a `MALLOCX` free (`dallocx`, `sdallocx`) on the calling
/// thread. Aborts unless `p` is a live allocation. `blk` is the caller's
/// token for `p` ([`Live::claims`](super::Live::claims)).
pub fn free(shared: &Shared, p: *mut u8, tcache: Tcache, Tracked(blk): Tracked<super::Live>, errno: Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        blk.claims(p),
    ensures
        crate::heap::free_target(p),
        blk.block(p),
        !bad_tcache(tcache),
{
    // Premise (`axiom_heap_use`): the caller uses the explicit cache
    // `tcache` names on no other thread (the `MALLOCX_TCACHE` contract).
    let tracked mut u = super::axiom_heap_use();
    match tcache {
        Tcache::Default => crate::heap::free(shared, p, Tracked(blk), errno),
        Tcache::Explicit(t) => match shared.tcaches.get(t, Tracked(&mut u)) {
            Some(h) => h.free(shared, p, Tracked(blk), errno),
            None => invalid_tcache(t),
        },
        Tcache::None => crate::heap::free_direct(shared, p, Tracked(blk)),
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_decoding_matches_jemalloc() {
        let f = Flags::decode(0);
        assert_eq!(
            f,
            Flags {
                lg_align: 0,
                zero: false,
                tcache: Tcache::Default,
                arena: None
            }
        );
        assert_eq!(f.align(), 1);
        let f = Flags::decode(mallocx_lg_align(12) | MALLOCX_ZERO);
        assert_eq!(f.lg_align, 12);
        assert_eq!(f.align(), 4096);
        assert!(f.zero);
        assert_eq!(Flags::decode(MALLOCX_TCACHE_NONE).tcache, Tcache::None);
        assert_eq!(MALLOCX_TCACHE_NONE, 0x100);
        assert_eq!(Flags::decode(mallocx_tcache(0)).tcache, Tcache::Explicit(0));
        assert_eq!(Flags::decode(mallocx_tcache(7)).tcache, Tcache::Explicit(7));
        assert_eq!(mallocx_tcache(7), 9 << 8);
        assert_eq!(Flags::decode(mallocx_arena(0)).arena, Some(0));
        assert_eq!(Flags::decode(mallocx_arena(3)).arena, Some(3));
        assert_eq!(mallocx_arena(3), 4 << 20);
        assert_eq!(Flags::decode(mallocx_arena(4094)).arena, Some(4094));
        // Every field at once, as Valkey's Lua arena code combines them.
        let f = Flags::decode(
            mallocx_arena(1) | MALLOCX_TCACHE_NONE | mallocx_lg_align(4) | MALLOCX_ZERO,
        );
        assert_eq!(
            f,
            Flags {
                lg_align: 4,
                zero: true,
                tcache: Tcache::None,
                arena: Some(1)
            }
        );
        // Bit 7 is unused and ignored.
        assert_eq!(Flags::decode(0x80), Flags::decode(0));
    }
}
