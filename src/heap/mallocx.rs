//! `MALLOCX_*` flags (design.md section 13.2): the jemalloc flag layout
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

} // verus!

#[cold]
#[inline(never)]
fn invalid_tcache(id: u16) -> ! {
    crate::sys::abort_fmt(format_args!("MALLOCX_TCACHE({id}): no such explicit cache"))
}

/// Routing of a `MALLOCX` allocation of a non-huge class on the calling
/// thread. `None` from the arena lookup (an index `arenas.create` never
/// handed out) gives null. The flag says the memory is known zero.
// The nested `if` stands for a let chain, which Verus does not support.
#[allow(clippy::collapsible_if)]
pub fn alloc(
    shared: &Shared,
    class: usize,
    arena: Option<u16>,
    tcache: Tcache,
    _blk: Tracked<&mut super::Live>,
    _errno: Tracked<&mut ThreadErrno>,
) -> (*mut u8, bool, Tracked<Zeroed>) {
    match tcache {
        Tcache::Default => match crate::heap::heap_for_thread(shared) {
            Some(h) => match arena {
                Some(a) => {
                    let (p, zero) = h.alloc_in(
                        shared,
                        a,
                        class,
                        Tracked::assume_new(),
                        Tracked::assume_new(),
                    );
                    let z = h.take_handed(
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                    );
                    (p, zero, z)
                }
                None => {
                    let (p, zero) =
                        h.alloc_zeroed(shared, class, Tracked::assume_new(), Tracked::assume_new());
                    let z = h.take_handed(
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                    );
                    (p, zero, z)
                }
            },
            None => alloc_direct(shared, class, arena.unwrap_or(0), Tracked::assume_new()),
        },
        Tcache::Explicit(t) => {
            let Some(h) = shared.tcaches.get(t) else {
                invalid_tcache(t)
            };
            // As for the default cache: only an explicit arena pins a
            // shared class to it (`Heap::alloc_in`).
            match arena {
                Some(a) => {
                    let (p, zero) = h.alloc_in(
                        shared,
                        a,
                        class,
                        Tracked::assume_new(),
                        Tracked::assume_new(),
                    );
                    let z = h.take_handed(
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                    );
                    (p, zero, z)
                }
                None => {
                    let (p, zero) =
                        h.alloc_zeroed(shared, class, Tracked::assume_new(), Tracked::assume_new());
                    let z = h.take_handed(
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                    );
                    (p, zero, z)
                }
            }
        }
        Tcache::None => {
            let a = arena
                .or_else(|| {
                    crate::heap::existing_heap(crate::heap::shared_ptr(shared)).map(|h| h.arena())
                })
                .unwrap_or(0);
            let (p, zero, z) = alloc_direct(shared, class, a, Tracked::assume_new());
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
fn alloc_direct(
    shared: &Shared,
    class: usize,
    arena: u16,
    _blk: Tracked<&mut super::Live>,
) -> (*mut u8, bool, Tracked<Zeroed>) {
    match shared.arena(arena, Tracked::assume_new()) {
        Some((a, w)) => {
            shared.alloc_direct(a, w, class, Tracked::assume_new(), Tracked::assume_new())
        }
        None => (ptr::null_mut(), false, Tracked::assume_new()),
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
    _blk: Tracked<&mut super::Live>,
) -> (*mut u8, bool, Tracked<Zeroed>) {
    let a = match (arena, tcache) {
        (Some(a), _) => a,
        (None, Tcache::Explicit(t)) => match shared.tcaches.get(t) {
            Some(h) => h.arena(),
            None => invalid_tcache(t),
        },
        (None, _) => {
            crate::heap::existing_heap(crate::heap::shared_ptr(shared)).map_or(0, |h| h.arena())
        }
    };
    if shared.arena(a, Tracked::assume_new()).is_none() {
        return (ptr::null_mut(), false, Tracked::assume_new());
    }
    match crate::heap::heap_for_thread(shared) {
        Some(h) => h.alloc_huge_in(shared, size, align, a, Tracked::assume_new()),
        None => shared.alloc_huge(size, align, a, Tracked::assume_new(), Tracked::assume_new()),
    }
}

/// Routing of a `MALLOCX` free (`dallocx`, `sdallocx`) on the calling
/// thread. Aborts unless `p` is a live allocation.
pub fn free(
    shared: &Shared,
    p: *mut u8,
    tcache: Tcache,
    _blk: Tracked<super::Live>,
    _errno: Tracked<&mut ThreadErrno>,
) {
    match tcache {
        Tcache::Default => {
            crate::heap::free(shared, p, Tracked::assume_new(), Tracked::assume_new())
        }
        Tcache::Explicit(t) => match shared.tcaches.get(t) {
            Some(h) => h.free(shared, p, Tracked::assume_new(), Tracked::assume_new()),
            None => invalid_tcache(t),
        },
        Tcache::None => crate::heap::free_direct(shared, p, Tracked::assume_new()),
    }
}

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
