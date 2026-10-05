//! jemalloc's non-standard allocation entry points (design.md section
//! 13.2): `mallocx`, `rallocx`, `xallocx`, `sallocx`, `dallocx`,
//! `sdallocx`, `nallocx`, the `malloc_message` hook and the read-only
//! `malloc_conf` symbols, each also exported under its `je_` name, and
//! Redis's `get_defrag_hint` (design 13.5). The `mallctl` family is in
//! `mallctl.rs`, the reports in `print.rs`.
//!
//! `MALLOCX_*` flags are decoded by `heap::mallocx::Flags` and routed by
//! `heap::mallocx::alloc` / `alloc_huge` / `free`: the arena and
//! cache selection lives there, this module adds the size and alignment
//! arithmetic (`libc::class_for`) and the zeroing.
//!
//! Semantics follow jemalloc where it defines them: a `rallocx` that maps
//! to the same class or the same number of huge pages keeps the pointer
//! whatever the flags say about arenas (jemalloc's `arena_ralloc` does the
//! same); `xallocx` never moves and challoc never grows a block in place,
//! so it reports the current usable size; `MALLOCX_ZERO` zeroes the whole
//! usable size of newly obtained memory unless the allocator already knows
//! it reads as zero. A null pointer is not an allocation: `rallocx(NULL)`
//! allocates like `mallocx`, `dallocx(NULL)` is a no-op like `free`, and
//! `sallocx`/`xallocx` of null abort, as does any pointer that is not a
//! live allocation. The `*allocx` functions leave `errno` alone on failure,
//! as jemalloc's do.
//!
//! `malloc_message` and `je_malloc_message` are exported data symbols
//! holding a `void (*)(void *, const char *)` that `malloc_stats_print`
//! uses instead of `write(2)` to stderr when a program sets either (the
//! plain name is consulted first). They are two objects rather than one
//! aliased symbol because rustc's cdylib export list only carries Rust
//! items: symbols defined in `global_asm!` stay local, and stable Rust has
//! no symbol aliases.
//!
//! `malloc_conf` is an exported data symbol too, a `const char *` that is
//! null unless a program defines its own: Ruby's `--with-jemalloc` configure
//! probe links a program that *reads* `malloc_conf` against `-ljemalloc`, so
//! the staged `libjemalloc.so.2` must define it, as jemalloc does. jemalloc's
//! definition is weak and stable Rust has no weak statics, so this one is a
//! plain global; that changes nothing for a dynamically linked program that
//! defines the symbol itself (Redis and Valkey define `je_malloc_conf`,
//! `tests/c/conf.c` both): the executable's definition comes first in the
//! loader's lookup and `config.rs` reads the symbol through the GOT, never
//! directly, so it sees the program's string. A weak `global_asm!`
//! definition was tried first and rejected by test: rustc's version script
//! makes it local even with `--export-dynamic-symbol`, and a local
//! definition binds the library's own read to itself, hiding the program's.
//! `je_malloc_conf` stays a weak undefined reference: Valkey defines it, and
//! a strong definition here would clash with such a program under a static
//! link, where no consumer needs the library to define either symbol.

use core::ffi::{c_char, c_int, c_void};
use core::ptr;

use super::libc::{class_for, old_size_or_keep, usable_size};
use crate::heap::Shared;
use crate::heap::mallocx::{self, Flags, Tcache};
use crate::layout::{CLASSES, Kind};

/// `void (*malloc_message)(void *cbopaque, const char *s)`; null when the
/// program has not installed one.
pub type MallocMessage = Option<unsafe extern "C" fn(*mut c_void, *const c_char)>;

/// `malloc_message`: null until a program installs a hook.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mut malloc_message: MallocMessage = None;

/// `je_malloc_message`: the prefixed name, a separate object (see the
/// module docs); consulted when `malloc_message` is null.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mut je_malloc_message: MallocMessage = None;

/// `const char *malloc_conf`: jemalloc's configuration string, defined so a
/// program that only references it links (Ruby's configure probe). Null
/// here; a program that defines its own interposes it, and `config::load`
/// reads whichever definition the loader picked (see the module docs).
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mut malloc_conf: *const c_char = ptr::null();

/// The installed hook, if any: `malloc_message`, else `je_malloc_message`.
/// Read volatile: C code stores them behind the compiler's back.
pub fn message_hook() -> MallocMessage {
    // SAFETY: both statics are plain 8-byte slots that only ever hold null
    // or a function pointer stored by the program.
    unsafe {
        ptr::read_volatile(&raw const malloc_message)
            .or_else(|| ptr::read_volatile(&raw const je_malloc_message))
    }
}

/// Test stand-in for a program assigning `malloc_message`.
#[cfg(test)]
pub fn set_message_hook_for_tests(hook: MallocMessage) {
    // SAFETY: tests that set the hook serialise on their own mutex.
    unsafe { ptr::write_volatile(&raw mut malloc_message, hook) };
}

/// jemalloc's `malloc_write`: hand `s` to the `malloc_message` hook in
/// NUL-terminated chunks (with a null `cbopaque`, as jemalloc does) or, with
/// no hook, write it to stderr with `write(2)`.
pub fn malloc_write(s: &[u8]) {
    match message_hook() {
        None => crate::sys::write_all(crate::sys::STDERR_FILENO, s),
        Some(cb) => {
            let mut buf = [0u8; 512];
            for chunk in s.chunks(buf.len() - 1) {
                buf[..chunk.len()].copy_from_slice(chunk);
                buf[chunk.len()] = 0;
                // SAFETY: the program installed `cb` as a
                // `void (*)(void *, const char *)`; `buf` is NUL-terminated.
                unsafe { cb(ptr::null_mut(), buf.as_ptr() as *const c_char) };
            }
        }
    }
}

#[cold]
#[inline(never)]
fn not_an_allocation(what: &str, p: *mut u8) -> ! {
    crate::sys::abort_fmt(format_args!(
        "{what} of {:#x}: not an allocation",
        p as usize
    ))
}

/// The usable size `mallocx(size, flags)` gives: the class size at the
/// flags' alignment, or the huge run's page-rounded size. `None` when no
/// allocation can satisfy the request, so `mallocx` always rejects it
/// (`nallocx` returns 0): the size cannot be represented, or the run and
/// its guard page do not fit the huge region at that alignment
/// (`Huge::placeable`).
fn usable_for(shared: &Shared, size: usize, f: &Flags) -> Option<usize> {
    match class_for(size, f.align()) {
        Some(c) => Some(CLASSES[c].size()),
        None if shared.huge.placeable(size, f.align()) => shared.huge.usable_for(size),
        None => None,
    }
}

/// `mallocx`'s core: allocate `size` bytes per `f`, zeroing the whole
/// usable size when `MALLOCX_ZERO` is set and the memory is not already
/// known to be zero. Null on exhaustion or for an arena or explicit cache
/// that does not exist (`errno` untouched).
fn alloc_with(shared: &Shared, size: usize, f: &Flags) -> *mut u8 {
    let align = f.align();
    let (p, usable, zeroed) = match class_for(size, align) {
        Some(c) => {
            let (p, z) = mallocx::alloc(shared, c, f.arena, f.tcache);
            (p, CLASSES[c].size(), z)
        }
        None => {
            let (p, z) = mallocx::alloc_huge(shared, size, align, f.arena, f.tcache);
            (p, shared.huge.usable_for(size).unwrap_or(0), z)
        }
    };
    if !p.is_null() && f.zero && !zeroed {
        // SAFETY: `p` is a fresh allocation of `usable` bytes.
        unsafe { ptr::write_bytes(p, 0, usable) };
    }
    p
}

/// `mallocx(size, flags)`. `size == 0` gets the smallest class, as
/// `malloc(0)` does.
pub fn mallocx(size: usize, flags: c_int) -> *mut u8 {
    let f = Flags::decode(flags as u32);
    alloc_with(super::shared(), size, &f)
}

/// `rallocx(p, size, flags)`: in place when the new size maps to the same
/// class (whose slots satisfy the requested alignment by construction) or
/// the same number of huge pages at a start that satisfies it
/// (`libc::old_size_or_keep`); otherwise allocate per `flags`, copy
/// `min(old usable, size)`, free `p` through the flags' cache.
/// `MALLOCX_ZERO` zeroes the bytes beyond the copied prefix of a moved
/// block. On failure `p` is intact and null comes back.
pub fn rallocx(p: *mut u8, size: usize, flags: c_int) -> *mut u8 {
    let f = Flags::decode(flags as u32);
    if p.is_null() {
        return alloc_with(super::shared(), size, &f);
    }
    let Some(shared) = super::shared_if_ready() else {
        not_an_allocation("rallocx", p)
    };
    let Some(old) = old_size_or_keep(shared, "rallocx", p, size, f.align()) else {
        return p;
    };
    let q = alloc_with(shared, size, &f);
    if q.is_null() {
        return q;
    }
    // SAFETY: `p` has `old` readable bytes, `q` at least `size` writable
    // ones, and they are distinct allocations. `alloc_with` already zeroed
    // `q` when `MALLOCX_ZERO` asked for it, so the tail past the copy is
    // zero.
    unsafe { ptr::copy_nonoverlapping(p, q, old.min(size)) };
    mallocx::free(shared, p, f.tcache);
    q
}

/// `xallocx(p, size, extra, flags)`: resize in place only. challoc never
/// changes a block's class or run in place, so the result is always the
/// current usable size; a caller that needs more must `rallocx`.
pub fn xallocx(p: *mut u8, _size: usize, _extra: usize, _flags: c_int) -> usize {
    sallocx(p, 0)
}

/// `sallocx(p, flags)`: usable size of the live allocation `p`.
pub fn sallocx(p: *mut u8, _flags: c_int) -> usize {
    if p.is_null() {
        not_an_allocation("sallocx", p);
    }
    let Some(shared) = super::shared_if_ready() else {
        not_an_allocation("sallocx", p)
    };
    usable_size(shared, p)
}

/// `dallocx(p, flags)`: free through the flags' cache (`MALLOCX_TCACHE_NONE`
/// releases the slot straight to its slab bitmap). Null is a no-op.
pub fn dallocx(p: *mut u8, flags: c_int) {
    if p.is_null() {
        return;
    }
    let f = Flags::decode(flags as u32);
    let Some(shared) = super::shared_if_ready() else {
        not_an_allocation("dallocx", p)
    };
    mallocx::free(shared, p, f.tcache);
}

/// `sdallocx(p, size, flags)`: `size` must satisfy `nallocx(size, flags)
/// == sallocx(p)` (jemalloc's contract); the class it names is checked
/// against the pointer and stands in for the slot bit test, so the fast
/// path touches no per-slab metadata (design 6.3; the check lands at the
/// cache flush). A mismatch aborts. With `MALLOCX_TCACHE_NONE` the release
/// itself checks the bit at once.
///
/// `flags == 0` (every Valkey free) goes straight to `free_sized`'s path
/// before any decoding: no alignment shift, no tcache-bits test, and the
/// hint is the plain `class_of(size)` (design 6.3). Other flags take
/// [`sdallocx_flags`], a call, so that the compiler cannot merge the two
/// sized fast paths behind a test of the hint.
#[inline(always)]
pub fn sdallocx(p: *mut u8, size: usize, flags: c_int) {
    if flags == 0 {
        return super::libc::free_sized(p, size);
    }
    sdallocx_flags(p, size, flags)
}

/// `sdallocx` with non-zero flags: the decoded alignment's class as the
/// hint through the default cache, or an explicit cache or none.
#[inline(never)]
fn sdallocx_flags(p: *mut u8, size: usize, flags: c_int) {
    let f = Flags::decode(flags as u32);
    match f.tcache {
        Tcache::Default => super::libc::free_sized_hint(p, size, class_for(size, f.align())),
        // The raw flags, not `f`: a `Flags` argument goes by pointer to a
        // stack copy, and that copy is a frame every default `sdallocx`
        // sets up and tears down (the sized path's instruction budget,
        // design 6.3).
        _ => sdallocx_routed(p, size, flags),
    }
}

/// `sdallocx` through an explicit cache or none.
#[cold]
#[inline(never)]
fn sdallocx_routed(p: *mut u8, size: usize, flags: c_int) {
    if p.is_null() {
        return;
    }
    let f = Flags::decode(flags as u32);
    let Some(shared) = super::shared_if_ready() else {
        not_an_allocation("sdallocx", p)
    };
    let hint = class_for(size, f.align());
    if let Tcache::Explicit(t) = f.tcache {
        if let Some(h) = shared.tcaches.get(t) {
            if crate::heap::free_sized_fast(shared, h, p, hint, shared.push_flags()) {
                return;
            }
        }
    }
    shared.check_size_hint(p, size, hint);
    mallocx::free(shared, p, f.tcache);
}

/// `nallocx(size, flags)`: the usable size `mallocx` would give, or 0 when
/// no allocation can satisfy the request (see [`usable_for`]).
pub fn nallocx(size: usize, flags: c_int) -> usize {
    let f = Flags::decode(flags as u32);
    usable_for(super::shared(), size, &f).unwrap_or(0)
}

/// `get_defrag_hint(ptr)`: the extension Redis adds to its bundled jemalloc
/// (`iget_defrag_hint`), which KeyDB's defragmenter calls as
/// `je_get_defrag_hint` on every allocation it considers moving
/// (`activeDefragAlloc`; design 13.5). Returns 1 when moving `p` is worth
/// it, by jemalloc's rule and from the data the utilization queries report
/// (`mallctl::utilization`): `p` is a live slot of a slab class, its slab
/// has a free slot, the slab is not the one a `MALLOCX_TCACHE_NONE` claim
/// takes from next (jemalloc's `slabcur`, challoc's lowest partial slab:
/// the move would land the object back in it), and its live count is at
/// or below the average live count of the class's other slabs in the
/// owning arena (`live * (curslabs - 1) <= curregs - live(slabcur)`,
/// jemalloc's multiplication in place of a division). Every slab class
/// answers, the ones above `BIN_MAX` included: the utilization queries
/// report those as one region each because Valkey's bin arithmetic is
/// calibrated to jemalloc's bins (13.4), but the hint has no geometry
/// contract, and one live 16 KiB block pinning a 16-slot slab is exactly
/// the waste a move recovers (measured on KeyDB's memefficiency suite). 0 for
/// everything else: a huge run (no slab to consolidate), a full slab, a
/// freed or foreign pointer, null, and before the allocator is
/// initialised. Never aborts: a defragmenter asks about pointers it owns,
/// and a wrong answer only costs it a move. Cold: two class-lock
/// acquisitions per call, as the verbose utilization query.
#[cold]
#[inline(never)]
pub fn get_defrag_hint(p: *const u8) -> c_int {
    let Some(shared) = super::shared_if_ready() else {
        return 0;
    };
    let Kind::Slab { class, slab, slot } = shared.regions.classify(p as usize) else {
        return 0;
    };
    let class = class as usize;
    let t = shared.table(class);
    let (slab, slot) = (slab as usize, slot as usize);
    if !t.is_allocated(slab, slot) {
        return 0;
    }
    let Some(d) = t.desc_if_committed(slab) else {
        return 0;
    };
    let cls = t.class();
    let live = d.live();
    if live >= cls.nslots() {
        return 0;
    }
    let Some(a) = shared.arenas.get_existing(d.arena()) else {
        return 0;
    };
    let cs = a.class(class);
    let slabcur = cs.lowest_partial();
    if slabcur == Some(slab) {
        return 0;
    }
    let (st, mut curregs) = cs.bin_stats();
    let mut curslabs = st.curslabs();
    if let Some(dc) = slabcur.and_then(|s| t.desc_if_committed(s)) {
        // Take the current slab out of the average, as jemalloc does.
        curregs = curregs.saturating_sub(dc.live());
        curslabs = curslabs.saturating_sub(1);
    }
    (live * curslabs <= curregs) as c_int
}

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see `libc.rs`).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use core::ffi::{c_int, c_void};

    export_pair! {
        fn mallocx / je_mallocx(size: usize, flags: c_int) -> *mut c_void {
            super::mallocx(size, flags) as *mut c_void
        }
        fn rallocx / je_rallocx(p: *mut c_void, size: usize, flags: c_int) -> *mut c_void {
            super::rallocx(p as *mut u8, size, flags) as *mut c_void
        }
        fn xallocx / je_xallocx(p: *mut c_void, size: usize, extra: usize, flags: c_int) -> usize {
            super::xallocx(p as *mut u8, size, extra, flags)
        }
        fn sallocx / je_sallocx(p: *mut c_void, flags: c_int) -> usize {
            super::sallocx(p as *mut u8, flags)
        }
        fn dallocx / je_dallocx(p: *mut c_void, flags: c_int) {
            super::dallocx(p as *mut u8, flags)
        }
        fn sdallocx / je_sdallocx(p: *mut c_void, size: usize, flags: c_int) {
            super::sdallocx(p as *mut u8, size, flags)
        }
        fn nallocx / je_nallocx(size: usize, flags: c_int) -> usize {
            super::nallocx(size, flags)
        }
        fn get_defrag_hint / je_get_defrag_hint(p: *mut c_void) -> c_int {
            super::get_defrag_hint(p as *const u8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::mallocx::{
        MALLOCX_TCACHE_NONE, MALLOCX_ZERO, Tcache, mallocx_arena, mallocx_lg_align, mallocx_tcache,
    };
    use crate::layout::{Kind, MEDIUM_MAX, class_size_of};
    use std::thread;
    use std::vec::Vec;

    const SIZES: &[usize] = &[
        0,
        1,
        16,
        17,
        100,
        1000,
        4096,
        65_536,
        MEDIUM_MAX,
        MEDIUM_MAX + 1,
        1 << 20,
        5 << 20,
    ];

    fn shared() -> &'static Shared {
        super::super::shared()
    }

    #[test]
    fn mallocx_honours_size_alignment_and_zero_and_matches_nallocx() {
        for &size in SIZES {
            for la in [0u32, 4, 5, 6, 8, 12, 16, 20] {
                let flags = mallocx_lg_align(la) as c_int;
                let p = mallocx(size, flags);
                assert!(!p.is_null(), "mallocx({size}, lg_align {la})");
                assert_eq!(
                    p as usize % (1usize << la),
                    0,
                    "mallocx({size}, lg_align {la}) = {p:p}"
                );
                let usable = sallocx(p, flags);
                assert!(usable >= size);
                assert_eq!(
                    usable,
                    nallocx(size, flags),
                    "nallocx disagrees for size {size} lg_align {la}"
                );
                assert_eq!(usable, crate::api::libc::malloc_usable_size(p));
                // Every class is a multiple of the quantum, so up to it the
                // alignment leaves the class alone.
                if (1usize << la) <= crate::QUANTUM {
                    match class_size_of(size) {
                        Some(s) => assert_eq!(usable, s),
                        None => assert_eq!(usable, shared().huge.usable_for(size).unwrap()),
                    }
                }
                // Dirty it, free it, and take the same shape zeroed.
                unsafe { ptr::write_bytes(p, 0xAB, usable) };
                dallocx(p, flags);
                let z = mallocx(size, flags | MALLOCX_ZERO as c_int);
                assert!(!z.is_null());
                let n = sallocx(z, 0);
                for i in 0..n {
                    assert_eq!(
                        unsafe { *z.add(i) },
                        0,
                        "mallocx ZERO size {size} lg_align {la} byte {i}"
                    );
                }
                sdallocx(z, size, flags);
            }
        }
        // Alignment above the medium tier is a huge run even for one byte.
        let p = mallocx(1, mallocx_lg_align(19) as c_int);
        assert!(shared().regions.classify(p as usize) != Kind::Invalid);
        assert!(matches!(
            shared().regions.classify(p as usize),
            Kind::Huge { .. }
        ));
        assert_eq!(p as usize % (1 << 19), 0);
        dallocx(p, 0);
        assert_eq!(nallocx(usize::MAX, 0), 0);
        assert_eq!(nallocx(0, 0), class_size_of(0).unwrap());
        assert!(mallocx(usize::MAX / 2, 0).is_null());
    }

    /// `nallocx` is 0 exactly when `mallocx` rejects the size or alignment:
    /// a request larger than the huge region, or an alignment no page of it
    /// satisfies, fails every time, so `nallocx` must say so; a large
    /// aligned run that fits still gets its size.
    #[test]
    fn nallocx_is_zero_when_mallocx_rejects() {
        for (size, flags) in [
            (usize::MAX / 2, 0),
            (1usize << 60, 0),
            (1, mallocx_lg_align(62) as c_int),
            (1, mallocx_lg_align(63) as c_int),
        ] {
            assert!(
                mallocx(size, flags).is_null(),
                "mallocx({size:#x}, {flags:#x})"
            );
            assert_eq!(nallocx(size, flags), 0, "nallocx({size:#x}, {flags:#x})");
        }
        let flags = mallocx_lg_align(24) as c_int;
        let n = nallocx(1 << 20, flags);
        assert_eq!(n, 1 << 20);
        let p = mallocx(1 << 20, flags);
        assert!(!p.is_null());
        assert_eq!(p as usize % (1 << 24), 0);
        dallocx(p, flags);
    }

    #[test]
    fn rallocx_keeps_moves_zeroes_and_preserves_content() {
        // Same class keeps the pointer; a class change moves and copies.
        let p = rallocx(ptr::null_mut(), 10, 0);
        assert!(!p.is_null());
        for i in 0..10 {
            unsafe { *p.add(i) = i as u8 };
        }
        assert_eq!(rallocx(p, 16, 0), p);
        let q = rallocx(p, 100, MALLOCX_ZERO as c_int);
        assert_ne!(q, p);
        let n = sallocx(q, 0);
        for i in 0..n {
            let want = if i < 10 { i as u8 } else { 0 };
            assert_eq!(unsafe { *q.add(i) }, want, "byte {i} after rallocx ZERO");
        }
        // Alignment forces a move even within the same size.
        let r = rallocx(q, 100, mallocx_lg_align(12) as c_int);
        assert_eq!(r as usize % 4096, 0);
        assert_eq!(unsafe { *r.add(9) }, 9);
        // Into and out of the huge tier.
        let h = rallocx(r, MEDIUM_MAX + 1, 0);
        assert!(matches!(
            shared().regions.classify(h as usize),
            Kind::Huge { .. }
        ));
        assert_eq!(unsafe { *h.add(9) }, 9);
        let page = shared().regions.page_size();
        assert_eq!(
            rallocx(h, MEDIUM_MAX + page, 0),
            h,
            "same page count keeps the run"
        );
        let h2 = rallocx(h, MEDIUM_MAX + page + 1, MALLOCX_ZERO as c_int);
        assert_ne!(h2, h);
        assert_eq!(unsafe { *h2.add(9) }, 9);
        let n = sallocx(h2, 0);
        for i in (MEDIUM_MAX + page)..n {
            assert_eq!(unsafe { *h2.add(i) }, 0, "huge rallocx ZERO tail byte {i}");
        }
        let s = rallocx(h2, 24, 0);
        assert_eq!(sallocx(s, 0), class_size_of(24).unwrap());
        assert_eq!(unsafe { *s.add(9) }, 9);
        // xallocx never moves and reports the current usable size, with
        // or without `extra` (jemalloc may grow into `size + extra`;
        // challoc never grows in place, so `extra` changes nothing).
        assert_eq!(xallocx(s, 1 << 20, 0, 0), class_size_of(24).unwrap());
        assert_eq!(xallocx(s, 1, 0, 0), class_size_of(24).unwrap());
        assert_eq!(xallocx(s, 1, 100, 0), class_size_of(24).unwrap());
        assert_eq!(xallocx(s, 24, 1 << 20, 0), class_size_of(24).unwrap());
        assert_eq!(
            xallocx(s, 1 << 20, 1 << 20, MALLOCX_ZERO as c_int),
            class_size_of(24).unwrap()
        );
        assert_eq!(unsafe { *s.add(9) }, 9, "xallocx touched nothing");
        // A failed rallocx leaves the block intact.
        assert!(rallocx(s, usize::MAX / 2, 0).is_null());
        assert_eq!(unsafe { *s.add(9) }, 9);
        dallocx(s, 0);
        dallocx(ptr::null_mut(), 0);
    }

    #[test]
    fn mallocx_routes_arenas_and_caches() {
        let s = shared();
        let lua = s.arenas.create(s).expect("arenas.create");
        let c = crate::layout::class_of(300).unwrap();
        // Default cache, explicit arena.
        let p = mallocx(300, mallocx_arena(lua) as c_int);
        assert_eq!(s.classify_free(p), Ok((c, lua)));
        // rallocx within the class keeps it in the Lua arena; across
        // classes with the arena flag lands there too.
        assert_eq!(rallocx(p, 320, mallocx_arena(lua) as c_int), p);
        let q = rallocx(p, 3000, mallocx_arena(lua) as c_int);
        assert_eq!(s.classify_free(q).map(|x| x.1), Ok(lua));
        dallocx(q, mallocx_arena(lua) as c_int);
        // No cache: the live count moves at once.
        let flags = (mallocx_arena(lua) | MALLOCX_TCACHE_NONE) as c_int;
        let d = mallocx(300, flags);
        let (slab, _) = s.table(c).locate(d);
        let desc = s.table(c).desc(slab);
        assert_eq!(desc.arena(), lua);
        let live = desc.live();
        dallocx(d, flags);
        assert_eq!(desc.live(), live - 1);
        // Explicit cache.
        let t = s.tcaches.create(s, 0).expect("tcache.create");
        let flags = mallocx_tcache(t) as c_int;
        let e = mallocx(300, flags);
        let th = s.tcaches.get(t).unwrap();
        let before = th.cache_in(0, c).map_or(0, |cc| cc.len());
        dallocx(e, flags);
        assert_eq!(th.cache_in(0, c).unwrap().len(), before + 1);
        assert!(s.tcaches.destroy(s, t));
        // Huge runs record the arena.
        let h = mallocx(2 << 20, mallocx_arena(lua) as c_int);
        let Err(page) = s.classify_free(h) else {
            panic!("not huge")
        };
        assert_eq!(s.huge.lookup(page).unwrap().arena, lua);
        dallocx(h, 0);
        // Nonexistent arena or explicit cache: null, not an abort.
        assert!(mallocx(300, mallocx_arena(4000) as c_int).is_null());
        assert!(mallocx(2 << 20, mallocx_arena(4000) as c_int).is_null());
        let _ = Tcache::Default;
    }

    /// Arenas and explicit caches against the real allocator (design 13.3):
    /// `MALLOCX_ARENA` allocations come from the named arena and plain
    /// `free` puts them on the freeing thread's one foreign cache for the
    /// class, whichever thread that is (the flush routes them home; the
    /// per-arena cache on the `others` list sees explicit-arena
    /// allocations only); `MALLOCX_TCACHE_NONE` claims and
    /// releases against the slab bitmaps at once; an explicit cache holds
    /// its own slots until flushed.
    #[test]
    fn arenas_route_mallocx_allocations_and_plain_frees() {
        use crate::api::libc as api_libc;
        use crate::heap;
        use crate::layout::class_of;
        let s = shared();
        let lua = s.arenas.create(s).expect("arenas.create");
        assert!(s.multi_arena());
        let c = class_of(200).unwrap();
        let size = crate::layout::CLASSES[c].size();
        let f = Flags::decode(mallocx_arena(lua));
        assert_eq!((f.arena, f.tcache), (Some(lua), Tcache::Default));

        // Default cache, explicit arena: through this thread's heap.
        let (p, _) = mallocx::alloc(s, c, f.arena, f.tcache);
        assert!(!p.is_null());
        unsafe { core::ptr::write_bytes(p, 0xa5, size) };
        assert_eq!(s.classify_free(p), Ok((c, lua)));
        let h = heap::existing_heap(s).unwrap();
        // This thread's automatic arena: whichever `Arenas::assign` gave
        // the test thread (the default is `min(4 * ncpus, 64)` arenas).
        let home = h.arena();
        assert_ne!(home, lua);
        // A plain `free` of another arena's slot pushes onto the FOREIGN
        // cache; the thread's per-arena caches see only explicit-arena
        // traffic.
        let cached_in = |h: &heap::Heap| h.cache_in(lua, c).map_or(0, |cc| cc.len());
        let cached_foreign = |h: &heap::Heap| h.cache_in(heap::FOREIGN, c).map_or(0, |cc| cc.len());
        let before = cached_in(h);
        let before_foreign = cached_foreign(h);
        api_libc::free(p);
        assert_eq!(cached_in(h), before);
        assert_eq!(cached_foreign(h), before_foreign + 1);
        // realloc within the class keeps the pointer and the arena.
        let (p, _) = mallocx::alloc(s, c, Some(lua), Tcache::Default);
        // (Written again: this is a fresh slot of the Lua arena's cache,
        // the one freed above went to the FOREIGN cache.)
        unsafe { core::ptr::write_bytes(p, 0xa5, size) };
        assert_eq!(api_libc::realloc(p, size), p);
        let q = api_libc::realloc(p, size * 4);
        assert_ne!(q, p);
        assert_eq!(unsafe { *q }, 0xa5);
        // Across classes, plain realloc lands in the caller's home arena.
        assert_eq!(s.classify_free(q).map(|x| x.1), Ok(home));
        api_libc::free(q);

        // Direct path: live count moves at once, no cache involved.
        let f = Flags::decode(mallocx_arena(lua) | MALLOCX_TCACHE_NONE);
        assert_eq!(f.tcache, Tcache::None);
        let (d, _) = mallocx::alloc(s, c, f.arena, f.tcache);
        let (slab, _) = s.table(c).locate(d);
        let desc = s.table(c).desc(slab);
        assert_eq!(desc.arena(), lua);
        let live = desc.live();
        let cached = cached_in(h);
        mallocx::free(s, d, Tcache::None);
        assert_eq!(desc.live(), live - 1);
        assert_eq!(cached_in(h), cached);
        // A non-existent arena is null, not an abort.
        assert!(
            mallocx::alloc(s, c, Some(4000), Tcache::Default)
                .0
                .is_null()
        );
        assert!(mallocx::alloc(s, c, Some(4000), Tcache::None).0.is_null());
        assert!(
            mallocx::alloc_huge(s, 1 << 20, 1, Some(4000), Tcache::Default)
                .0
                .is_null()
        );

        // Huge runs record the arena too.
        let (hp, _) = mallocx::alloc_huge(s, 3 << 20, 1, Some(lua), Tcache::Default);
        assert!(!hp.is_null());
        let Err(page) = s.classify_free(hp) else {
            panic!("not huge")
        };
        assert_eq!(s.huge.lookup(page).unwrap().arena, lua);
        api_libc::free(hp);

        // Explicit cache.
        let t = s.tcaches.create(s, 0).expect("tcache.create");
        let f = Flags::decode(mallocx_tcache(t) | mallocx_arena(lua));
        assert_eq!(f.tcache, Tcache::Explicit(t));
        let (e, _) = mallocx::alloc(s, c, f.arena, f.tcache);
        assert_eq!(s.classify_free(e), Ok((c, lua)));
        let th = s.tcaches.get(t).unwrap();
        let before = th.cache_in(lua, c).unwrap().len();
        let before_foreign = th.cache_in(heap::FOREIGN, c).map_or(0, |cc| cc.len());
        // The free through the explicit cache (homed on arena 0) lands on
        // its FOREIGN cache, as any plain free of another arena's slot.
        mallocx::free(s, e, Tcache::Explicit(t));
        assert_eq!(th.cache_in(lua, c).unwrap().len(), before);
        assert_eq!(
            th.cache_in(heap::FOREIGN, c).unwrap().len(),
            before_foreign + 1
        );
        assert!(s.tcaches.flush(s, t));
        assert_eq!(th.cache_in(lua, c).unwrap().len(), 0);
        assert_eq!(th.cache_in(heap::FOREIGN, c).unwrap().len(), 0);
        assert!(s.tcaches.destroy(s, t));

        // Cross-thread: another thread allocates from the Lua arena and its
        // own home arena; this thread frees both with plain `free`, both
        // land on this thread's FOREIGN cache, and the flush routes each
        // slot back to its own arena.
        let (other, ptrs) = thread::spawn(move || {
            let s = shared();
            let mut v = Vec::new();
            for i in 0..64 {
                let a = if i % 2 == 0 { Some(lua) } else { None };
                let (p, _) = mallocx::alloc(s, c, a, Tcache::Default);
                assert!(!p.is_null());
                unsafe { *p = i as u8 };
                v.push(p as usize);
            }
            (heap::existing_heap(s).unwrap().arena(), v)
        })
        .join()
        .unwrap();
        // The other thread was bound to an idle automatic arena, not ours.
        assert_ne!(other, home);
        assert_ne!(other, lua);
        let h = heap::existing_heap(s).unwrap();
        // Pin the home (a `thread.arena` write): this thread's heap is
        // young and the other thread's arena is idle now, so its first
        // free of a slot there would otherwise move it onto that arena
        // (`Heap::try_rehome`, covered in heap/tests.rs).
        h.set_arena(s, home);
        h.flush_all(s);
        for (i, &p) in ptrs.iter().enumerate() {
            let p = p as *mut u8;
            assert_eq!(unsafe { *p }, i as u8);
            let (class, arena) = s.classify_free(p).unwrap();
            assert_eq!(class, c);
            if i % 2 == 0 {
                assert_eq!(arena, lua);
            } else {
                assert_eq!(arena, other);
            }
            api_libc::free(p);
        }
        assert_eq!(
            cached_in(h),
            0,
            "plain frees never reach the thread's per-arena caches"
        );
        assert!(
            h.cache_in(other, c).is_none(),
            "a plain free creates no arena node"
        );
        assert!(
            h.cache(c).is_none_or(|cache| cache.len() == 0),
            "nothing landed in the home cache"
        );
        assert_eq!(
            cached_foreign(h),
            64,
            "both arenas' slots on the one FOREIGN cache"
        );
        h.flush_all(s);
        // Per-arena purge covers the Lua arena's slabs only.
        let lua_arena = s.arena(lua).unwrap();
        let empty_before = lua_arena.classes[c].stats().empty;
        assert!(
            empty_before >= 1,
            "the Lua arena's slab should be Empty after the flush"
        );
        let purged = crate::purge::purge_arena(s, lua).unwrap();
        assert!(purged >= empty_before);
        assert_eq!(lua_arena.classes[c].stats().empty, 0);
        assert!(crate::purge::purge_arena(s, 4000).is_none());
    }

    #[test]
    fn malloc_write_goes_to_the_hook_when_set() {
        use std::sync::Mutex;
        static GOT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        unsafe extern "C" fn hook(opaque: *mut c_void, s: *const c_char) {
            assert!(opaque.is_null());
            let s = unsafe { core::ffi::CStr::from_ptr(s) };
            GOT.lock().unwrap().extend_from_slice(s.to_bytes());
        }
        set_message_hook_for_tests(Some(hook));
        let text: Vec<u8> = (0..2000u32).map(|i| b'a' + (i % 26) as u8).collect();
        malloc_write(&text);
        set_message_hook_for_tests(None);
        assert_eq!(
            *GOT.lock().unwrap(),
            text,
            "chunks must reassemble to the original"
        );
        assert!(message_hook().is_none());
        // Without a hook it goes to stderr and must not crash.
        malloc_write(b"challoc: malloc_write test line\n");
    }
    /// Design 13.5: `get_defrag_hint` answers from the utilization data
    /// with jemalloc's rule. A private arena keeps the bin figures this
    /// test's own.
    #[test]
    fn get_defrag_hint_follows_the_utilization_data() {
        use crate::api::mallctl::utilization;
        let s = shared();
        let a = s.arenas.create(s).expect("arenas.create");
        let flags = (mallocx_arena(a) | MALLOCX_TCACHE_NONE) as c_int;
        let c = crate::layout::class_of(100).unwrap();
        let cls = &CLASSES[c];
        let n = 20 * cls.nslots();
        let ptrs: Vec<*mut u8> = (0..n).map(|_| mallocx(100, flags)).collect();
        assert!(ptrs.iter().all(|p| !p.is_null()));
        // Every other slot of the first half goes back: those slabs are
        // half full, the second half's stay full.
        for p in ptrs[..n / 2].iter().step_by(2) {
            dallocx(*p, flags);
        }
        let live: Vec<*mut u8> = ptrs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i >= n / 2 || i % 2 == 1)
            .map(|(_, p)| *p)
            .collect();
        let t = s.table(c);
        let cs = s.arena(a).unwrap().class(c);
        let mut moved = 0;
        for &p in &live {
            let hint = get_defrag_hint(p);
            let u = utilization(s, p as *const c_void, true);
            assert_eq!(u.nregs, cls.nslots());
            // The reference: jemalloc's rule over the same figures.
            let (slab, _) = t.locate(p);
            let slabcur = cs.lowest_partial();
            let mut curslabs = u.bin_nregs / u.nregs;
            let mut curregs = u.bin_nregs - u.bin_nfree;
            if let Some(sc) = slabcur {
                curregs -= t.desc(sc).live();
                curslabs -= 1;
            }
            let want =
                u.nfree > 0 && slabcur != Some(slab) && (u.nregs - u.nfree) * curslabs <= curregs;
            assert_eq!(
                hint, want as c_int,
                "ptr {p:p}: nfree {} of {}, bin {}/{}, slabcur {slabcur:?} slab {slab}",
                u.nfree, u.nregs, curregs, curslabs
            );
            if hint == 1 {
                assert!(u.nfree > 0, "a full slab is never worth moving out of");
                moved += 1;
            } else if u.nfree == 0 {
                // Full: 0 whatever the average.
            }
        }
        // The half-full slabs (all but the one a claim takes from next)
        // are below the average; the full ones are never hinted.
        assert!(
            moved >= cls.nslots() / 2 * 8,
            "{moved} hints for {} half-full slots",
            n / 4
        );
        assert!(
            live[n / 4..].iter().all(|p| get_defrag_hint(*p) == 0
                || utilization(s, *p as *const c_void, false).nfree > 0)
        );
        // Not a hint: a freed slot, a huge run, null, a stack address.
        assert_eq!(get_defrag_hint(ptrs[0]), 0, "freed");
        let h = mallocx(3 << 20, flags);
        assert_eq!(get_defrag_hint(h), 0, "huge");
        assert_eq!(get_defrag_hint(ptr::null()), 0, "null");
        let stack = 0u64;
        assert_eq!(
            get_defrag_hint(&stack as *const u64 as *const u8),
            0,
            "stack"
        );
        dallocx(h, flags);
        // A class above BIN_MAX answers like a bin (the utilization queries
        // report it as one region; the hint has no geometry contract): two
        // full slabs of 16 KiB slots plus one slab holding a single slot,
        // whose block is below the average and worth moving unless it sits
        // in the slab a claim takes from next.
        let cm = crate::layout::class_of(16 << 10).unwrap();
        assert!(cm >= crate::layout::NBINS);
        let clm = &CLASSES[cm];
        let big: Vec<*mut u8> = (0..2 * clm.nslots() + 1)
            .map(|_| mallocx(16 << 10, flags))
            .collect();
        assert!(big.iter().all(|p| !p.is_null()));
        let um = utilization(s, big[0] as *const c_void, true);
        assert_eq!(
            (um.nregs, um.nfree),
            (1, 0),
            "the utilization query keeps jemalloc's large-extent shape"
        );
        let tm = s.table(cm);
        let csm = s.arena(a).unwrap().class(cm);
        let (lone_slab, _) = tm.locate(*big.last().unwrap());
        assert_eq!(tm.desc(lone_slab).live(), 1);
        let lone_hint = get_defrag_hint(*big.last().unwrap());
        assert_eq!(
            lone_hint,
            (csm.lowest_partial() != Some(lone_slab)) as c_int,
            "the lone block moves unless its slab is the current one"
        );
        for p in &big[..2 * clm.nslots()] {
            assert_eq!(
                get_defrag_hint(*p),
                0,
                "a full slab is never worth moving out of"
            );
        }
        for p in big {
            dallocx(p, flags);
        }
        for p in live {
            dallocx(p, flags);
        }
    }
}
