//! jemalloc's non-standard allocation entry points (design.md section
//! 13.2): `mallocx`, `rallocx`, `xallocx`, `sallocx`, `dallocx`,
//! `sdallocx`, `nallocx`, the `malloc_message` hook and the read-only
//! `malloc_conf` symbols, each also exported under its `je_` name. The
//! `mallctl` family is in `mallctl.rs`, the reports in `print.rs`.
//!
//! `MALLOCX_*` flags are decoded by `arena::Flags` and routed by
//! `arena::alloc` / `alloc_huge` / `free`: the arena and
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

use super::libc::{class_for, usable_size};
use crate::arena::{self, Flags, Tcache};
use crate::heap::Shared;
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
    unsafe { ptr::read_volatile(&raw const malloc_message).or_else(|| ptr::read_volatile(&raw const je_malloc_message)) }
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
    crate::sys::abort_fmt(format_args!("{what} of {:#x}: not an allocation", p as usize))
}

/// The usable size `mallocx(size, flags)` gives: the class size at the
/// flags' alignment, or the huge run's page-rounded size. `None` when the
/// size cannot be represented (`nallocx` returns 0).
fn usable_for(shared: &Shared, size: usize, f: &Flags) -> Option<usize> {
    match class_for(size, f.align()) {
        Some(c) => Some(CLASSES[c].size()),
        None => shared.huge.usable_for(size),
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
            let (p, z) = arena::alloc(shared, c, f.arena, f.tcache);
            (p, CLASSES[c].size(), z)
        }
        None => {
            let (p, z) = arena::alloc_huge(shared, size, align, f.arena, f.tcache);
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
/// the same number of huge pages at a start that satisfies it; otherwise
/// allocate per `flags`, copy `min(old usable, size)`, free `p` through
/// the flags' cache. `MALLOCX_ZERO` zeroes the bytes beyond the copied
/// prefix of a moved block. On failure `p` is intact and null comes back.
pub fn rallocx(p: *mut u8, size: usize, flags: c_int) -> *mut u8 {
    let f = Flags::decode(flags as u32);
    if p.is_null() {
        return alloc_with(super::shared(), size, &f);
    }
    let Some(shared) = super::shared_if_ready() else { not_an_allocation("rallocx", p) };
    let align = f.align();
    let old = match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            shared.table(class).check_live(slab as usize, slot as usize, p as usize);
            if class_for(size, align) == Some(class) {
                return p;
            }
            CLASSES[class].size()
        }
        Kind::Huge { page } => {
            let old = shared.huge.usable_size(page);
            if class_for(size, align).is_none() && shared.huge.usable_for(size) == Some(old) && (p as usize) % align == 0 {
                return p;
            }
            old
        }
        Kind::Invalid => not_an_allocation("rallocx", p),
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
    arena::free(shared, p, f.tcache);
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
    let Some(shared) = super::shared_if_ready() else { not_an_allocation("sallocx", p) };
    usable_size(shared, p)
}

/// `dallocx(p, flags)`: free through the flags' cache (`MALLOCX_TCACHE_NONE`
/// releases the slot straight to its slab bitmap). Null is a no-op.
pub fn dallocx(p: *mut u8, flags: c_int) {
    if p.is_null() {
        return;
    }
    let f = Flags::decode(flags as u32);
    let Some(shared) = super::shared_if_ready() else { not_an_allocation("dallocx", p) };
    arena::free(shared, p, f.tcache);
}

/// `sdallocx(p, size, flags)`: `size` must satisfy `nallocx(size, flags)
/// == sallocx(p)` (jemalloc's contract); the class it names is checked
/// against the pointer and stands in for the slot bit test, so the fast
/// path touches no per-slab metadata (design 6.3; the check lands at the
/// cache flush). A mismatch aborts. With `MALLOCX_TCACHE_NONE` the release
/// itself checks the bit at once.
#[inline(always)]
pub fn sdallocx(p: *mut u8, size: usize, flags: c_int) {
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
    let Some(shared) = super::shared_if_ready() else { not_an_allocation("sdallocx", p) };
    let hint = class_for(size, f.align());
    if let Tcache::Explicit(t) = f.tcache {
        if let Some(h) = shared.tcaches.get(t) {
            if crate::heap::free_sized_fast(shared, h, p, hint, shared.push_flags()) {
                return;
            }
        }
    }
    shared.check_size_hint(p, size, hint);
    arena::free(shared, p, f.tcache);
}

/// `nallocx(size, flags)`: the usable size `mallocx` would give, or 0 when
/// the request cannot be represented.
pub fn nallocx(size: usize, flags: c_int) -> usize {
    let f = Flags::decode(flags as u32);
    usable_for(super::shared(), size, &f).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see `libc.rs`).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use core::ffi::{c_int, c_void};

    #[unsafe(no_mangle)]
    pub extern "C" fn mallocx(size: usize, flags: c_int) -> *mut c_void {
        super::mallocx(size, flags) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rallocx(p: *mut c_void, size: usize, flags: c_int) -> *mut c_void {
        super::rallocx(p as *mut u8, size, flags) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn xallocx(p: *mut c_void, size: usize, extra: usize, flags: c_int) -> usize {
        super::xallocx(p as *mut u8, size, extra, flags)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn sallocx(p: *mut c_void, flags: c_int) -> usize {
        super::sallocx(p as *mut u8, flags)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn dallocx(p: *mut c_void, flags: c_int) {
        super::dallocx(p as *mut u8, flags)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn sdallocx(p: *mut c_void, size: usize, flags: c_int) {
        super::sdallocx(p as *mut u8, size, flags)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn nallocx(size: usize, flags: c_int) -> usize {
        super::nallocx(size, flags)
    }

    #[unsafe(export_name = "je_mallocx")]
    pub extern "C" fn je_mallocx(size: usize, flags: c_int) -> *mut c_void {
        super::mallocx(size, flags) as *mut c_void
    }

    #[unsafe(export_name = "je_rallocx")]
    pub extern "C" fn je_rallocx(p: *mut c_void, size: usize, flags: c_int) -> *mut c_void {
        super::rallocx(p as *mut u8, size, flags) as *mut c_void
    }

    #[unsafe(export_name = "je_xallocx")]
    pub extern "C" fn je_xallocx(p: *mut c_void, size: usize, extra: usize, flags: c_int) -> usize {
        super::xallocx(p as *mut u8, size, extra, flags)
    }

    #[unsafe(export_name = "je_sallocx")]
    pub extern "C" fn je_sallocx(p: *mut c_void, flags: c_int) -> usize {
        super::sallocx(p as *mut u8, flags)
    }

    #[unsafe(export_name = "je_dallocx")]
    pub extern "C" fn je_dallocx(p: *mut c_void, flags: c_int) {
        super::dallocx(p as *mut u8, flags)
    }

    #[unsafe(export_name = "je_sdallocx")]
    pub extern "C" fn je_sdallocx(p: *mut c_void, size: usize, flags: c_int) {
        super::sdallocx(p as *mut u8, size, flags)
    }

    #[unsafe(export_name = "je_nallocx")]
    pub extern "C" fn je_nallocx(size: usize, flags: c_int) -> usize {
        super::nallocx(size, flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::{MALLOCX_TCACHE_NONE, MALLOCX_ZERO, Tcache, mallocx_arena, mallocx_lg_align, mallocx_tcache};
    use crate::layout::{MEDIUM_MAX, class_size_of};
    use std::vec::Vec;

    const SIZES: &[usize] = &[0, 1, 16, 17, 100, 1000, 4096, 65_536, MEDIUM_MAX, MEDIUM_MAX + 1, 1 << 20, 5 << 20];

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
                assert_eq!(p as usize % (1usize << la), 0, "mallocx({size}, lg_align {la}) = {p:p}");
                let usable = sallocx(p, flags);
                assert!(usable >= size);
                assert_eq!(usable, nallocx(size, flags), "nallocx disagrees for size {size} lg_align {la}");
                assert_eq!(usable, crate::api::libc::malloc_usable_size(p));
                if la <= 4 {
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
                    assert_eq!(unsafe { *z.add(i) }, 0, "mallocx ZERO size {size} lg_align {la} byte {i}");
                }
                sdallocx(z, size, flags);
            }
        }
        // Alignment above the medium tier is a huge run even for one byte.
        let p = mallocx(1, mallocx_lg_align(19) as c_int);
        assert!(shared().regions.classify(p as usize) != Kind::Invalid);
        assert!(matches!(shared().regions.classify(p as usize), Kind::Huge { .. }));
        assert_eq!(p as usize % (1 << 19), 0);
        dallocx(p, 0);
        assert_eq!(nallocx(usize::MAX, 0), 0);
        assert_eq!(nallocx(0, 0), class_size_of(0).unwrap());
        assert!(mallocx(usize::MAX / 2, 0).is_null());
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
        assert!(matches!(shared().regions.classify(h as usize), Kind::Huge { .. }));
        assert_eq!(unsafe { *h.add(9) }, 9);
        let page = shared().regions.page_size();
        assert_eq!(rallocx(h, MEDIUM_MAX + page, 0), h, "same page count keeps the run");
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
        assert_eq!(xallocx(s, 1 << 20, 1 << 20, MALLOCX_ZERO as c_int), class_size_of(24).unwrap());
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
        let Err(page) = s.classify_free(h) else { panic!("not huge") };
        assert_eq!(s.huge.lookup(page).unwrap().arena, lua);
        dallocx(h, 0);
        // Nonexistent arena or explicit cache: null, not an abort.
        assert!(mallocx(300, mallocx_arena(4000) as c_int).is_null());
        assert!(mallocx(2 << 20, mallocx_arena(4000) as c_int).is_null());
        let _ = Tcache::Default;
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
        assert_eq!(*GOT.lock().unwrap(), text, "chunks must reassemble to the original");
        assert!(message_hook().is_none());
        // Without a hook it goes to stderr and must not crash.
        malloc_write(b"challoc: malloc_write test line\n");
    }
}
