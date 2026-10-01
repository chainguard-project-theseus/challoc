//! `MALLOCX_*` flags (design.md section 13.2): the jemalloc flag layout
//! ([`Flags::decode`], [`Tcache`], the `mallocx_*` builders) and the
//! routing of a `mallocx` / `dallocx` / `sdallocx` call onto the calling
//! thread's heap, an explicit cache ([`Tcache::Explicit`], a `Heap` owned
//! by no thread, `heap/tcaches.rs`) or the direct paths
//! ([`Tcache::None`], straight against the slab bitmaps).

use core::ptr;

use super::Shared;
use crate::vspec::zero::Zeroed;
use vstd::prelude::verus;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked};

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

/// `MALLOCX_LG_ALIGN(la)`.
#[cfg(test)]
pub const fn mallocx_lg_align(la: u32) -> u32 {
    la & LG_ALIGN_MASK
}

/// `MALLOCX_TCACHE(t)` for an explicit cache id.
#[cfg(test)]
pub const fn mallocx_tcache(t: u16) -> u32 {
    ((t as u32 + 2) & TCACHE_MASK) << TCACHE_SHIFT
}

/// `MALLOCX_ARENA(a)`.
#[cfg(test)]
pub const fn mallocx_arena(a: u16) -> u32 {
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


/// [`Flags::decode`]'s result: jemalloc's bit layout.
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
    /// Decode jemalloc's layout: bits 0-5 `lg_align`, bit 6 `ZERO`, bits
    /// 8-19 tcache (0 default, 1 none, `t + 2` explicit `t`), bits 20-31
    /// arena (0 default, `a + 1` arena `a`).
    pub const fn decode(flags: u32) -> (r: Flags)
        ensures
            r == decode_spec(flags),
            r.lg_align < 64,
    {
        let t = (flags >> TCACHE_SHIFT) & TCACHE_MASK;
        let a = (flags >> ARENA_SHIFT) & ARENA_MASK;
        proof {
            assert(t <= 0xfff && a <= 0xfff && flags & 0x3f < 64) by (bit_vector)
                requires t == (flags >> 8u32) & 0xfff, a == (flags >> 20u32) & 0xfff;
        }
        Flags {
            lg_align: flags & LG_ALIGN_MASK,
            zero: flags & MALLOCX_ZERO != 0,
            tcache: match t {
                0 => Tcache::Default,
                1 => Tcache::None,
                _ => Tcache::Explicit((t - 2) as u16),
            },
            arena: if a == 0 { None } else { Some((a - 1) as u16) },
        }
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

} // verus!

verus! {

/// Trusted: aborts.
#[cold]
#[inline(never)]
#[verifier::external_body]
fn invalid_tcache(id: u16) -> ! {
    panic!("MALLOCX_TCACHE({id}): no such explicit cache")
}

/// Routing of a `MALLOCX` allocation of a non-huge class on the calling
/// thread. `None` from the arena lookup (an index `arenas.create` never
/// handed out) gives null. The flag says the memory is known zero.
pub fn alloc(shared: &Shared, class: usize, arena: Option<u16>, tcache: Tcache) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        class < crate::layout::NCLASSES,
        shared.wf(),
    ensures
        r.0@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r.0@.addr),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, crate::layout::class_table()[class as int].size as int),
{
    match tcache {
        Tcache::Default => match crate::heap::heap_for_thread(shared) {
            Some(h) => {
                proof {
                    h.lemma_wf_regions();
                }
                match arena {
                    Some(a) => {
                        let (p, zero) = h.alloc_in(shared, a, class);
                        let ghost take = p@.addr != 0 && zero;
                        let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
                        (p, zero, z)
                    },
                    None => {
                        let (p, zero) = h.alloc_zeroed(shared, class);
                        let ghost take = p@.addr != 0 && zero;
                        let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
                        (p, zero, z)
                    },
                }
            },
            None => alloc_direct(shared, class, arena.unwrap_or(0)),
        },
        Tcache::Explicit(t) => {
            let Some(h) = shared.tcaches.get(t) else {
                invalid_tcache(t)
            };
            proof {
                h.lemma_wf_regions();
            }
            // As for the default cache: only an explicit arena pins a
            // shared class to it (`Heap::alloc_in`).
            match arena {
                Some(a) => {
                    let (p, zero) = h.alloc_in(shared, a, class);
                    let ghost take = p@.addr != 0 && zero;
                    let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
                    (p, zero, z)
                },
                None => {
                    let (p, zero) = h.alloc_zeroed(shared, class);
                    let ghost take = p@.addr != 0 && zero;
                    let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
                    (p, zero, z)
                },
            }
        }
        Tcache::None => {
            let a = arena
                .or_else(|| crate::heap::existing_heap(crate::heap::shared_ptr(shared)).map(|h| h.arena()))
                .unwrap_or(0);
            let (p, zero, z) = alloc_direct(shared, class, a);
            if !p.is_null() {
                if let Some(h) = crate::heap::existing_heap(crate::heap::shared_ptr(shared)) {
                    h.count_direct_alloc(class);
                }
            }
            (p, zero, z)
        }
    }
}

/// One slot of `class` straight from `arena`'s slab bitmaps.
fn alloc_direct(shared: &Shared, class: usize, arena: u16) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        shared.wf(),
        class < crate::layout::NCLASSES,
    ensures
        r.0@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r.0@.addr),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, crate::layout::class_table()[class as int].size as int),
{
    match shared.arena(arena) {
        Some(a) => shared.alloc_direct(a, class),
        None => (ptr::null_mut(), false, Tracked(Zeroed::empty(0))),
    }
}

/// Routing of a `MALLOCX` huge allocation on the calling thread (caches
/// play no part; the arena is recorded in the run's descriptor).
pub fn alloc_huge(
    shared: &Shared,
    size: usize,
    align: usize,
    arena: Option<u16>,
    tcache: Tcache,
) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        shared.wf(),
        crate::meta::is_pow2(align),
    ensures
        r.0@.addr != 0 ==> crate::layout::huge_block(r.0@.addr, size, align),
        r.0@.addr != 0 ==> size <= shared.huge_usable(size),
        r.0@.addr != 0 ==> shared.huge_ok(size),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, shared.huge_usable(size)),
{
    let a = match (arena, tcache) {
        (Some(a), _) => a,
        (None, Tcache::Explicit(t)) => match shared.tcaches.get(t) {
            Some(h) => h.arena(),
            None => invalid_tcache(t),
        },
        (None, _) => crate::heap::existing_heap(crate::heap::shared_ptr(shared)).map_or(0, |h| h.arena()),
    };
    if shared.arena(a).is_none() {
        return (ptr::null_mut(), false, Tracked(Zeroed::empty(0)));
    }
    match crate::heap::heap_for_thread(shared) {
        Some(h) => h.alloc_huge_in(shared, size, align, a),
        None => shared.alloc_huge(size, align, a),
    }
}

/// Routing of a `MALLOCX` free (`dallocx`, `sdallocx`) on the calling
/// thread. Aborts unless `p` is a live allocation.
pub fn free(shared: &Shared, p: *mut u8, tcache: Tcache)
    requires
        shared.wf(),
    ensures
        crate::heap::free_target(p),
{
    match tcache {
        Tcache::Default => crate::heap::free(shared, p),
        Tcache::Explicit(t) => match shared.tcaches.get(t) {
            Some(h) => h.free(shared, p),
            None => invalid_tcache(t),
        },
        Tcache::None => crate::heap::free_direct(shared, p),
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
