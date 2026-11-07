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
#[cfg(verus_keep_ghost)]
use super::libc::{keeps, lemma_class_aligned, spec_class_for};
use super::print::{cancel_disable, cancel_restore};
#[cfg(verus_keep_ghost)]
use crate::heap::mallocx::MALLOCX_ZERO;
use crate::heap::mallocx::{self, Flags, Tcache};
use crate::heap::{Live, Shared};
#[cfg(verus_keep_ghost)]
use crate::layout::class_table;
use crate::layout::{CLASSES, Kind};
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::axiom_entry;
use crate::vspec::cbuf::CCall;
use crate::vspec::errno::ThreadErrno;
#[cfg(verus_keep_ghost)]
use crate::vspec::usable::{usable_is, usable_of};
use crate::vspec::zero::Zeroed;
#[cfg(verus_keep_ghost)]
use crate::vspec::zero::{
    covers_usable, huge_placeable, huge_usable, lemma_covers_huge, lemma_covers_slot,
};
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, int, verus};

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

/// An installed `malloc_message` hook. A newtype, opaque to the verifier,
/// which rejects function pointer types: [`malloc_write`] carries it and
/// calls it through [`call_hook_nocancel`] and [`call_hook`].
#[derive(Copy, Clone)]
pub struct Hook(unsafe extern "C" fn(*mut c_void, *const c_char));

/// Test stand-in for a program assigning `malloc_message`.
#[cfg(test)]
pub fn set_message_hook_for_tests(hook: MallocMessage) {
    // SAFETY: tests that set the hook hold `testutil::global_state()`.
    unsafe { ptr::write_volatile(&raw mut malloc_message, hook) };
}

verus! {

#[verifier::external_type_specification]
#[verifier::external_body]
// Never constructed: it only names `Hook` to the verifier.
#[allow(dead_code)]
pub struct ExHook(Hook);

/// The installed hook, if any: `malloc_message`, else `je_malloc_message`.
/// Read volatile: C code stores them behind the compiler's back. Trusted:
/// the two volatile reads; nothing is claimed about the hook.
#[verifier::external_body]
pub fn message_hook() -> Option<Hook> {
    // SAFETY: both statics are plain 8-byte slots that only ever hold null
    // or a function pointer stored by the program.
    unsafe {
        ptr::read_volatile(&raw const malloc_message)
            .or_else(|| ptr::read_volatile(&raw const je_malloc_message))
            .map(Hook)
    }
}

/// `cb(NULL, buf)`: the hook called with a null `cbopaque`, as jemalloc
/// calls it, on the C string in `buf`, which holds a NUL. Trusted: a call
/// through the program's function pointer; claims nothing. The calling
/// thread holds no allocator lock (`ord`, the report's lock-order token,
/// has top 0), as for the report's other calls out (`print::call_back`,
/// `print::write_file`), so a hook that allocates cannot deadlock on one.
#[verifier::external_body]
#[inline(always)]
fn call_hook(cb: Hook, buf: &[u8; 512], Tracked(ord): Tracked<&LockOrder>)
    requires
        crate::sys::may_syscall(),
        ord.top() == 0,
        exists|i: int| 0 <= i < 512 && #[trigger] buf@[i] == 0,
{
    // SAFETY: the program installed `cb` as a
    // `void (*)(void *, const char *)`; `buf` is NUL-terminated.
    unsafe { (cb.0)(ptr::null_mut(), buf.as_ptr() as *const c_char) };
}

/// [`call_hook`] with cancellation disabled, as `print::write_file`'s
/// `fwrite` runs: a deferred `pthread_cancel` is not acted on at a
/// cancellation point inside the hook, unless the hook turns cancellation
/// back on (design 8.1). `ord` has top 0, as for `call_hook`.
#[inline(always)]
fn call_hook_nocancel(cb: Hook, buf: &[u8; 512], Tracked(ord): Tracked<&LockOrder>)
    requires
        crate::sys::may_syscall(),
        ord.top() == 0,
        exists|i: int| 0 <= i < 512 && #[trigger] buf@[i] == 0,
{
    let old = cancel_disable();
    call_hook(cb, buf, Tracked(ord));
    cancel_restore(old);
}

/// jemalloc's `malloc_write`: hand `s` to the `malloc_message` hook in
/// NUL-terminated chunks (with a null `cbopaque`, as jemalloc does) or, with
/// no hook, write it to stderr with `write(2)`. Under the report's lock-order
/// token `ord`, with no allocator lock held (top 0): `print::emit` is the
/// only caller.
pub(super) fn malloc_write(s: &[u8], Tracked(ord): Tracked<&LockOrder>)
    requires
        crate::sys::may_syscall(),
        ord.top() == 0,
{
    match message_hook() {
        None => crate::sys::write_all(crate::sys::STDERR_FILENO, s),
        Some(cb) => {
            let mut buf = [0u8; 512];
            // `for chunk in s.chunks(buf.len() - 1)`, with an index:
            // Verus does not specify `chunks`. Each chunk is the next
            // `buf.len() - 1` bytes, or the rest when fewer remain.
            let mut at = 0;
            while at < s.len()
                invariant
                    crate::sys::may_syscall(),
                    ord.top() == 0,
                    at <= s@.len(),
                decreases s@.len() - at,
            {
                let n = if s.len() - at < buf.len() - 1 {
                    s.len() - at
                } else {
                    buf.len() - 1
                };
                let chunk = &s[at..at + n];
                buf[..chunk.len()].copy_from_slice(chunk);
                buf[chunk.len()] = 0;
                assert(buf@[n as int] == 0);
                call_hook_nocancel(cb, &buf, Tracked(ord));
                at += n;
            }
        }
    }
}

/// The abort for a pointer that is not a live allocation, naming the
/// entry point.
#[cold]
#[inline(never)]
fn not_an_allocation(what: &str, p: *mut u8) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("{what} of {:#x}: not an allocation", p as usize))
}

/// The usable size `mallocx(size, flags)` gives and `nallocx(size, flags)`
/// returns, for `lg_align` the flags' `MALLOCX_LG_ALIGN`: the size of the
/// smallest class that serves `size` bytes at `1 << lg_align`
/// ([`spec_class_for`]), or for a huge request `size` rounded up to whole
/// pages ([`huge_usable`]), 0 when no run for it fits the huge region at
/// that alignment ([`huge_placeable`]: the rounding overflows, or the run
/// and its guard page fit from no aligned page of the empty region).
pub open spec fn nalloc_usable(size: usize, lg_align: u32) -> int {
    match spec_class_for(size, 1usize << lg_align) {
        Some(c) => class_table()[c as int].size as int,
        None => if huge_placeable(size, 1usize << lg_align) {
            huge_usable(size as int)
        } else {
            0
        },
    }
}

/// The usable size `mallocx(size, flags)` gives: the class size at the
/// flags' alignment, or the huge run's page-rounded size. `None` when no
/// allocation can satisfy the request, so `mallocx` always rejects it
/// (`nallocx` returns 0): the size cannot be represented, or the run and
/// its guard page do not fit the huge region at that alignment
/// (`Huge::placeable`).
pub(super) fn usable_for(shared: &Shared, size: usize, f: &Flags) -> (r: Option<usize>)
    requires
        f.lg_align < 64,
        shared.wf(),
    ensures
        r matches Some(u) ==> size <= u,
        r matches Some(u) ==> u == nalloc_usable(size, f.lg_align),
        r is None ==> nalloc_usable(size, f.lg_align) == 0,
{
    proof {
        crate::meta::lemma_is_pow2_shl(f.lg_align as usize);
        shared.lemma_wf();
        shared.lemma_wf_slow();
        shared.lemma_wf_ready();
        shared.lemma_huge_usable(size);
        shared.lemma_huge_placeable(size, 1usize << f.lg_align);
        shared.lemma_huge_aligned();
    }
    match class_for(size, f.align()) {
        Some(c) => Some(CLASSES.table()[c].size()),
        None if shared.huge.placeable(size, f.align()) => shared.huge.usable_for(size),
        None => None,
    }
}

/// A `mallocx(size, flags)` that returns with an explicit cache id no
/// `tcache.create` hands out ([`mallocx::bad_tcache`]) was a huge request
/// with an explicit arena, the one path that never looks the cache up;
/// every other aborts (`invalid_tcache`).
pub open spec fn tcache_checked(size: usize, flags: c_int) -> bool {
    let f = mallocx::decode_spec(flags as u32);
    mallocx::bad_tcache(f.tcache) ==> f.arena is Some && spec_class_for(size, 1usize << f.lg_align) is None
}

/// `mallocx`'s core: allocate `size` bytes per `f`, zeroing the whole
/// usable size when `MALLOCX_ZERO` is set, unless it is a huge run known
/// to be zero (a slab slot always is cleared). Null on exhaustion or for an arena that does not
/// exist (`errno` untouched); an explicit cache that does not exist
/// aborts (`invalid_tcache`), except on the huge path with an explicit
/// arena, which never looks the cache up. With `MALLOCX_ZERO`, a
/// non-null result comes with a [`Zeroed`] token for its whole usable
/// size ([`covers_usable`]), which is at least `size`. `blk` gets the
/// block's token.
fn alloc_with(
    shared: &Shared,
    size: usize,
    f: &Flags,
    Tracked(blk): Tracked<&mut Live>,
    errno: Tracked<&mut ThreadErrno>,
) -> (r: (*mut u8, Tracked<Zeroed>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        f.lg_align < 64,
        shared.wf(),
        f.arena matches Some(a) ==> a != crate::heap::FOREIGN,
    ensures
        crate::heap::fits(r.0, size as int, (1usize << f.lg_align) as int),
        r.0@.addr != 0 && f.zero ==> r.1@.covers(r.0, size as int),
        r.0@.addr != 0 && f.zero ==> covers_usable(r.1@, r.0, 0, size as int),
        r.0@.addr != 0 ==> usable_is(r.0, size as int, nalloc_usable(size, f.lg_align)),
        final(blk).gives(r.0, size as int),
        r.0@.addr != 0 && f.zero ==> final(blk).reads_zero(0, final(blk).len()),
        mallocx::bad_tcache(f.tcache) ==> f.arena is Some && spec_class_for(size, 1usize << f.lg_align) is None,
{
    let align = f.align();
    proof {
        crate::meta::lemma_is_pow2_shl(f.lg_align as usize);
        shared.lemma_regions_ok();
        shared.lemma_wf();
        shared.lemma_wf_slow();
        shared.lemma_wf_ready();
    }
    // The class `p` is a slot of, or -1 for a huge run.
    let ghost mut cls: int = -1;
    let (p, usable, zeroed, z) = match class_for(size, align) {
        Some(c) => {
            let (p, _zero, z) = mallocx::alloc(shared, c, f.arena, f.tcache, Tracked(blk), errno);
            // The heap's zero flag is not trusted for a slab slot: it records that
            // challoc wrote nothing to the slot since its slab was `Fresh` or
            // `Purged`, but the slot is read-write memory that a linear
            // overflow from a live slot below, or a write through a stale
            // pointer into a purged slab, reaches without challoc seeing it
            // (SECURITY.md, "Zeroing allocations are zero"). The slot is
            // always cleared below.
            let zero = false;
            proof {
                cls = c as int;
                if p@.addr != 0 {
                    lemma_class_aligned(c, align);
                    crate::heap::lemma_slot_fits(p, c as int, size as int, align);
                    crate::vspec::usable::lemma_usable_slot(p, c as int, size as int);
                    // `p`'s token covers the class size, the zero fill's length.
                    blk.lemma_slot_len(p, c as int);
                    if zero {
                        lemma_covers_slot(z@, p, c as int, 0, size as int);
                    }
                }
                super::libc::lemma_class_gives(*blk, p, c as int, size);
            }
            (p, CLASSES.table()[c].size(), zero, z)
        }
        None => {
            let (p, zero, z) = mallocx::alloc_huge(
                shared,
                size,
                align,
                f.arena,
                f.tcache,
                f.zero,
                Tracked(blk),
            );
            proof {
                if p@.addr != 0 {
                    crate::heap::lemma_huge_fits(p, size, align, align);
                    crate::vspec::usable::lemma_usable_huge(p, size as int);
                }
            }
            let u = shared.huge.usable_for(size);
            proof {
                shared.lemma_huge_usable(size);
                if p@.addr != 0 && zero {
                    lemma_covers_huge(z@, p, 0, size as int);
                }
            }
            (p, u.unwrap_or(0), zero, z)
        }
    };
    if !p.is_null() && f.zero && !zeroed {
        let ghost orig = *blk;
        proof {
            blk.lemma_mem();
        }
        let tracked mut m = blk.take_mem();
        // SAFETY: `m` is the permission to the fresh block's `usable`
        // bytes.
        let z = unsafe { crate::vspec::user::zero_user(p, usable, Tracked(&mut m)) };
        proof {
            blk.put_mem(m, orig);
            // `usable` is the block's usable size.
            if cls >= 0 {
                lemma_covers_slot(z@, p, cls, 0, size as int);
            } else {
                lemma_covers_huge(z@, p, 0, size as int);
            }
        }
        return (p, z);
    }
    // Known zero (or no zeroing asked for): with the flag, the allocator's
    // token for the block's usable bytes, which its permission then reads
    // as zero.
    let Tracked(za) = z;
    let tracked z0;
    proof {
        if p@.addr != 0 && f.zero {
            let tracked (zs, _) = za.split(usable as int);
            blk.lemma_mem();
            let ghost orig = *blk;
            let tracked mut m = blk.take_mem();
            crate::vspec::user::axiom_zeroed_bytes(&zs, &mut m);
            blk.put_mem(m, orig);
            if cls >= 0 {
                lemma_covers_slot(zs, p, cls, 0, size as int);
            } else {
                lemma_covers_huge(zs, p, 0, size as int);
            }
            z0 = zs;
        } else {
            z0 = za;
        }
    }
    (p, Tracked(z0))
}

/// `mallocx(size, flags)`. `size == 0` gets the smallest class, as
/// `malloc(0)` does. The block's token goes to C with it.
pub fn mallocx(size: usize, flags: c_int, errno: Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        crate::heap::fits(r, size as int, (1usize << ((flags as u32) & 0x3f)) as int),
        r@.addr != 0 ==> usable_is(r, size as int, nalloc_usable(size, (flags as u32) & 0x3f)),
        tcache_checked(size, flags),
{
    let tracked mut blk = Live::empty();
    mallocx_zeroed(size, flags, Tracked(&mut blk), errno).0
}

/// [`mallocx`] with the token that `MALLOCX_ZERO` memory reads as zero:
/// with the flag, a non-null result comes with a [`Zeroed`] token for its
/// whole usable size ([`covers_usable`]: the class's size for a slot,
/// `size` rounded up to whole pages for a huge run), which is at least
/// `size`; and `blk` gets the block's token.
pub fn mallocx_zeroed(
    size: usize,
    flags: c_int,
    Tracked(blk): Tracked<&mut Live>,
    errno: Tracked<&mut ThreadErrno>,
) -> (r: (*mut u8, Tracked<Zeroed>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        crate::heap::fits(r.0, size as int, (1usize << ((flags as u32) & 0x3f)) as int),
        r.0@.addr != 0 && (flags as u32) & MALLOCX_ZERO != 0 ==> r.1@.covers(r.0, size as int),
        r.0@.addr != 0 && (flags as u32) & MALLOCX_ZERO != 0 ==> covers_usable(r.1@, r.0, 0, size as int),
        r.0@.addr != 0 ==> usable_is(r.0, size as int, nalloc_usable(size, (flags as u32) & 0x3f)),
        final(blk).gives(r.0, size as int),
        tcache_checked(size, flags),
{
    let f = Flags::decode(flags as u32);
    alloc_with(super::shared(), size, &f, Tracked(blk), errno)
}

/// `rallocx(p, size, flags)`: in place when the new size maps to the same
/// class (whose slots satisfy the requested alignment by construction)
/// or, for a size no class serves at the alignment, to the same number of
/// huge pages at a start that satisfies it (`libc::old_size_or_keep`;
/// exactly then: `libc::keeps` in [`rallocx_zeroed`]); otherwise
/// allocate per `flags`, copy `min(old usable, size)`, free `p` through
/// the flags' cache. `MALLOCX_ZERO` zeroes the bytes beyond the copied
/// prefix of a moved block. On failure `p` is intact and null comes back.
/// C holds the token of the block it passes
/// ([`crate::heap::live::axiom_c_claim`]), and gets the result's.
pub fn rallocx(p: *mut u8, size: usize, flags: c_int, Tracked(c): Tracked<CCall>, errno: Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        crate::heap::fits(r, size as int, (1usize << ((flags as u32) & 0x3f)) as int),
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 && r@.addr != 0 && r != p ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),
{
    let tracked blk = crate::heap::live::axiom_c_claim(c);
    let tracked mut out = Live::empty();
    rallocx_zeroed(p, size, flags, Tracked(blk), Tracked(&mut out), errno).0
}

/// [`rallocx`] with the token that `MALLOCX_ZERO` memory past the copy
/// reads as zero: with the flag, a new block (null `p`, or a moved block)
/// comes with a [`Zeroed`] token for its bytes from `k` to the end of its
/// usable size ([`covers_usable`]), where `k`, the ghost second result,
/// is the number of bytes copied from `p`: `min(old usable, size)` for
/// `old` the usable size a query of `p` reads ([`usable_of`]), which is
/// the class size for a slot `p`, and 0 for a null `p`. For a huge `p`,
/// `old` is what `Huge::usable_size` reads from the run's descriptor, of which
/// `usable_of` states only the shape (whole pages, at least one, inside
/// the region), not the value; the value is the usable size of `p`'s
/// token (`k == min(blk.len(), size)`). `blk` is the caller's token for
/// `p` ([`Live::claims`]), and `out` gets the result's: a new block's, or
/// `blk` itself when `p` is kept or the move fails (`p` is then still the
/// caller's).
pub fn rallocx_zeroed(
    p: *mut u8,
    size: usize,
    flags: c_int,
    Tracked(blk): Tracked<Live>,
    Tracked(out): Tracked<&mut Live>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
) -> (r: (*mut u8, Ghost<int>, Tracked<Zeroed>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        crate::heap::fits(r.0, size as int, (1usize << ((flags as u32) & 0x3f)) as int),
        p@.addr != 0 ==> crate::heap::free_target(p),
        r.0@.addr != 0 && r.0 != p && (flags as u32) & MALLOCX_ZERO != 0 ==> {
            &&& 0 <= r.1@ <= size
            &&& r.2@.addr() == r.0@.addr + r.1@
            &&& r.2@.len() >= size - r.1@
            &&& covers_usable(r.2@, r.0, r.1@, size as int)
            &&& p@.addr == 0 ==> r.1@ == 0
            &&& p@.addr != 0 ==> exists|o: int| #[trigger] usable_of(p, o) && r.1@ == (if o < size { o } else { size as int })
            // The same `o` is the usable size of `p`'s token: exact for a
            // huge `p` too.
            &&& p@.addr != 0 ==> r.1@ == (if blk.len() < size { blk.len() } else { size as int })
            // The slot case of the `usable_of` line, spelled out for callers.
            &&& p@.addr != 0 ==> forall|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr)
                ==> r.1@ == (if class_table()[c].size < size { class_table()[c].size as int } else { size as int })
        },
        final(out).gives(r.0, size as int),
        r.0@.addr != 0 && r.0 != p && (flags as u32) & MALLOCX_ZERO != 0 ==> final(out).reads_zero(r.1@, final(out).len() - r.1@),
        p@.addr != 0 && r.0@.addr != 0 && r.0 != p ==> final(out).copied_from(blk, if blk.len() < size { blk.len() } else { size as int }),
        // A move frees `p` through the flags' cache, which exists.
        p@.addr != 0 && r.0@.addr != 0 && r.0 != p ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),
        p@.addr != 0 ==> blk.block(p),
        p@.addr != 0 && r.0@.addr == 0 ==> *final(out) == blk,
        p@.addr != 0 ==> (r.0@.addr == p@.addr <==> keeps(p, size, (1usize << ((flags as u32) & 0x3f)), blk.len())),
{
    let f = Flags::decode(flags as u32);
    if p.is_null() {
        let (q, z) = alloc_with(super::shared(), size, &f, Tracked(out), Tracked(errno));
        return (q, Ghost(0), z);
    }
    let Some(shared) = super::shared_if_ready() else {
        not_an_allocation("rallocx", p)
    };
    let align = f.align();
    proof {
        let lg = f.lg_align;
        assert((1usize << lg) > 0) by (bit_vector)
            requires lg < 64;
        crate::meta::lemma_is_pow2_shl(lg as usize);
    }
    let tracked mut blk = blk;
    let Some(old) = old_size_or_keep(shared, "rallocx", p, size, align, Tracked(&blk))
    else {
        proof {
            out.set(blk);
        }
        return (p, Ghost(0), Tracked(Zeroed::empty(0)));
    };
    let (q, Tracked(z)) = alloc_with(shared, size, &f, Tracked(out), Tracked(&mut *errno));
    if q.is_null() {
        // The block at `p` stays the caller's, with its token.
        proof {
            out.set(blk);
        }
        return (q, Ghost(0), Tracked(z));
    }
    proof {
        broadcast use vstd::layout::align_of_u8;
        shared.lemma_regions_ok();
        blk.theorem_apart(out);
        // The new block is apart from `p`'s, so not at `p`: a move.
        blk.lemma_len_pos();
        out.lemma_len_pos();
        assert(q@.addr != p@.addr);
        // The copy reads `p`'s bytes: rallocx's one read of user memory.
        crate::vspec::user::grant_user_reads();
    }
    let ghost k = if old < size { old as int } else { size as int };
    proof {
        // `old` is the usable size a query of `p` reads.
        assert(usable_of(p, old as int));
    }
    // The copy writes the first `k` bytes of `q`; the rest of the token
    // is what stays zero.
    let tracked tail = if f.zero { z.split(k).1 } else { Zeroed::empty(0) };
    // SAFETY: `blk` holds the permission to `p`'s `old` usable bytes and
    // `out` to `q`'s `size` or more; the two share no byte. `alloc_with`
    // already zeroed `q` when `MALLOCX_ZERO` asked for it, so the tail
    // past the copy is zero.
    unsafe {
        super::libc::copy_moved(
            p,
            q,
            old.min(size),
            Tracked(&mut blk),
            Tracked(out),
        )
    };
    mallocx::free(shared, p, f.tcache, Tracked(blk), Tracked(errno));
    (q, Ghost(k), Tracked(tail))
}

/// `xallocx(p, size, extra, flags)`: resize in place only. challoc never
/// changes a block's class or run in place, so the result is always the
/// current usable size; a caller that needs more must `rallocx`.
pub fn xallocx(p: *mut u8, _size: usize, _extra: usize, _flags: c_int) -> (r: usize)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        crate::heap::free_target(p),
        usable_of(p, r as int),
        // NULL aborts (no block is at address 0), and the size is positive.
        p@.addr != 0,
        0 < r,
{
    sallocx(p, 0)
}

/// `sallocx(p, flags)`: usable size of the live allocation `p`.
pub fn sallocx(p: *mut u8, _flags: c_int) -> (r: usize)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        crate::heap::free_target(p),
        usable_of(p, r as int),
        p@.addr != 0,
        0 < r,
{
    if p.is_null() {
        not_an_allocation("sallocx", p);
    }
    let Some(shared) = super::shared_if_ready() else {
        not_an_allocation("sallocx", p)
    };
    let tracked none = Live::empty();
    usable_size(shared, p, Tracked(&none))
}

/// `dallocx(p, flags)`: free through the flags' cache (`MALLOCX_TCACHE_NONE`
/// releases the slot straight to its slab bitmap). Null is a no-op. C
/// holds the token of the block it frees
/// ([`crate::heap::live::axiom_c_claim`]).
pub fn dallocx(p: *mut u8, flags: c_int, Tracked(c): Tracked<CCall>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),
{
    let tracked blk = crate::heap::live::axiom_c_claim(c);
    dallocx_block(p, flags, Tracked(blk), Tracked(errno))
}

/// [`dallocx`] with the caller's token for `p` ([`Live::claims`]), which
/// the free takes back: the token of the live block `p`.
pub fn dallocx_block(p: *mut u8, flags: c_int, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),
        p@.addr != 0 ==> blk.block(p),
{
    if p.is_null() {
        return;
    }
    let f = Flags::decode(flags as u32);
    let Some(shared) = super::shared_if_ready() else {
        not_an_allocation("dallocx", p)
    };
    mallocx::free(shared, p, f.tcache, Tracked(blk), Tracked(errno));
}

/// `sdallocx(p, size, flags)`: `size` must satisfy `nallocx(size, flags)
/// == sallocx(p)` (jemalloc's contract); the class it names is checked
/// against the pointer and stands in for the slot bit test, so the fast
/// path touches no per-slab metadata (design 6.3; the check is left to a
/// flush, which catches a bad free only if the slot is still unclaimed
/// then). A mismatch aborts. With `MALLOCX_TCACHE_NONE` the release itself
/// checks the bit at once (a clear bit, not a copy still held in a cache). C holds the token of the block it frees
/// ([`crate::heap::live::axiom_c_claim`]).
///
/// `flags == 0` (every Valkey free) goes straight to `free_sized`'s path
/// before any decoding: no alignment shift, no tcache-bits test, and the
/// hint is the plain `class_of(size)` (design 6.3). Other flags take
/// [`sdallocx_flags`], a call, so that the compiler cannot merge the two
/// sized fast paths behind a test of the hint.
#[inline(always)]
pub fn sdallocx(p: *mut u8, size: usize, flags: c_int, Tracked(c): Tracked<CCall>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),
        p@.addr != 0 ==> crate::heap::hint_ok(p, spec_class_for(size, 1usize << ((flags as u32) & 0x3f))),
{
    let tracked blk = crate::heap::live::axiom_c_claim(c);
    sdallocx_block(p, size, flags, Tracked(blk), Tracked(errno))
}

/// [`sdallocx`] with the caller's token for `p` ([`Live::claims`]), which
/// the free takes back: the token of the live block `p`.
#[inline(always)]
pub fn sdallocx_block(p: *mut u8, size: usize, flags: c_int, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),
        p@.addr != 0 ==> crate::heap::hint_ok(p, spec_class_for(size, 1usize << ((flags as u32) & 0x3f))),
        p@.addr != 0 ==> crate::heap::huge_hint_ok(blk, p, size),
        p@.addr != 0 ==> blk.block(p),
{
    proof {
        // `flags == 0` names no alignment, so the hint is `class_index`'s,
        // and the default cache.
        if flags == 0 {
            let fl = flags as u32;
            assert(fl == 0);
            assert(1usize << (fl & 0x3f) == 1 && (fl >> 8u32) & 0xfff == 0) by (bit_vector)
                requires
                    fl == 0,
            ;
        }
    }
    if flags == 0 {
        return super::libc::free_sized_with(p, size, Tracked(blk), Tracked(errno));
    }
    sdallocx_flags(p, size, flags, Tracked(blk), Tracked(errno))
}

/// `sdallocx` with non-zero flags: the decoded alignment's class as the
/// hint through the default cache, or an explicit cache or none.
#[inline(never)]
fn sdallocx_flags(p: *mut u8, size: usize, flags: c_int, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),
        p@.addr != 0 ==> crate::heap::hint_ok(p, spec_class_for(size, 1usize << ((flags as u32) & 0x3f))),
        p@.addr != 0 ==> crate::heap::huge_hint_ok(blk, p, size),
        p@.addr != 0 ==> blk.block(p),
{
    let f = Flags::decode(flags as u32);
    proof {
        crate::meta::lemma_is_pow2_shl(f.lg_align as usize);
    }
    match f.tcache {
        Tcache::Default => super::libc::free_sized_block(
            p,
            size,
            class_for(size, f.align()),
            Tracked(blk),
            Tracked(errno),
        ),
        // The raw flags, not `f`: a `Flags` argument goes by pointer to a
        // stack copy, and that copy is a frame every default `sdallocx`
        // sets up and tears down (the sized path's instruction budget,
        // design 6.3).
        _ => sdallocx_routed(p, size, flags, Tracked(blk), Tracked(errno)),
    }
}

/// `sdallocx` through an explicit cache or none.
#[cold]
#[inline(never)]
// sdallocx's path: left as written so its codegen does not move.
#[allow(clippy::collapsible_if)]
fn sdallocx_routed(p: *mut u8, size: usize, flags: c_int, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> !mallocx::bad_tcache(mallocx::decode_spec(flags as u32).tcache),
        p@.addr != 0 ==> crate::heap::hint_ok(p, spec_class_for(size, 1usize << ((flags as u32) & 0x3f))),
        p@.addr != 0 ==> crate::heap::huge_hint_ok(blk, p, size),
        p@.addr != 0 ==> blk.block(p),
{
    let tracked mut blk = blk;
    if p.is_null() {
        return;
    }
    let f = Flags::decode(flags as u32);
    proof {
        crate::meta::lemma_is_pow2_shl(f.lg_align as usize);
    }
    let Some(shared) = super::shared_if_ready() else {
        not_an_allocation("sdallocx", p)
    };
    let hint = class_for(size, f.align());
    if let Tcache::Explicit(t) = f.tcache {
        // Premise (`axiom_heap_use`): the caller uses cache `t` on no other
        // thread (the `MALLOCX_TCACHE` contract); the reference ends before
        // `mallocx::free` takes its own.
        let tracked mut u = crate::heap::axiom_heap_use();
        if let Some(h) = shared.tcaches.get(t, Tracked(&mut u)) {
            if crate::heap::free_sized_fast(
                shared,
                h,
                p,
                hint,
                shared.push_flags(),
                Tracked(&mut blk),
            ) {
                return;
            }
        }
    }
    shared.check_size_hint(p, size, hint, Tracked(&blk));
    mallocx::free(shared, p, f.tcache, Tracked(blk), Tracked(errno));
}

/// `nallocx(size, flags)`: the usable size `mallocx` would give, or 0 when
/// no allocation can satisfy the request (see [`usable_for`]).
/// `theorem_read_only_never_aborts` restates its calls after `shared()`;
/// `api::restated::read_only_restatements_match_their_bodies` checks
/// that the two make the same calls.
pub fn nallocx(size: usize, flags: c_int) -> (r: usize)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r == 0 || size <= r,
        r == nalloc_usable(size, (flags as u32) & 0x3f),
{
    let f = Flags::decode(flags as u32);
    usable_for(super::shared(), size, &f).unwrap_or(0)
}

/// `nallocx`'s zero: `nallocx(size, flags)` is 0 exactly when no class
/// serves `size` at the flags' alignment and no run for it has a
/// placement in the empty huge region at that alignment
/// ([`huge_placeable`]): a class's size and a run's usable size are
/// positive.
pub proof fn theorem_nallocx_zero(size: usize, lg_align: u32)
    requires
        crate::heap::regions_ok(),
    ensures
        nalloc_usable(size, lg_align) == 0 <==> spec_class_for(size, 1usize << lg_align) is None && !huge_placeable(size, 1usize << lg_align),
{
    let align = 1usize << lg_align;
    if spec_class_for(size, align) is Some {
        let c = choose|c: int| super::libc::least_class(c, size as int, align);
        crate::vspec::class::lemma_class_table_wf(c);
    } else if huge_placeable(size, align) {
        lemma_huge_usable_pos(size as int);
    }
}

/// A run's usable size is at least a page, so positive.
proof fn lemma_huge_usable_pos(size: int)
    requires
        crate::heap::regions_ok(),
        size >= 0,
    ensures
        huge_usable(size) > 0,
{
    let lg = crate::layout::process_regions().lg_page;
    let pg = (1usize << lg) as int;
    assert((1usize << lg) > 0) by (bit_vector)
        requires lg < 64;
    let n = (size + pg - 1) / pg;
    vstd::arithmetic::div_mod::lemma_div_pos_is_pos(size + pg - 1, pg);
    let m = if n == 0 { 1 } else { n };
    assert(m * pg > 0) by (nonlinear_arith)
        requires m >= 1, pg > 0;
}

/// A request whose `nallocx` is 0 is one `mallocx` rejects: its result is
/// NULL in every state of the allocator. A non-NULL block is a slot or a
/// run ([`crate::heap::fits`]), and `mallocx` ensures its usable size is
/// `nalloc_usable`, which is then a class's size or a run's, positive.
#[cfg(verus_keep_ghost)]
fn theorem_nallocx_zero_rejects(size: usize, flags: c_int, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let n = nallocx(size, flags);
    let p = mallocx(size, flags, Tracked(errno));
    proof {
        if p@.addr != 0 {
            let r = crate::layout::process_regions();
            if exists|c: int| #![trigger r.is_slot(c, p@.addr)] #![trigger class_table()[c]] r.is_slot(c, p@.addr) && size <= class_table()[c].size {
                let c = choose|c: int| #![trigger r.is_slot(c, p@.addr)] #![trigger class_table()[c]] r.is_slot(c, p@.addr) && size <= class_table()[c].size;
                crate::vspec::class::lemma_class_table_wf(c);
            } else {
                lemma_huge_usable_pos(size as int);
            }
        }
    }
    assert(n == 0 ==> p@.addr == 0);
}

/// A sized free of a slot that returns normally
/// ([`crate::heap::hint_ok`], which the sized frees ensure) was given a
/// size and alignment whose `nallocx` is the slot's class size, its
/// `sallocx` (jemalloc's contract, `nallocx(size, flags) == sallocx(p)`).
pub proof fn theorem_sized_slot_nallocx(p: *mut u8, size: usize, lg_align: u32, c: int)
    requires
        crate::heap::hint_ok(p, spec_class_for(size, 1usize << lg_align)),
        crate::layout::process_regions().is_slot(c, p@.addr),
    ensures
        nalloc_usable(size, lg_align) == class_table()[c].size,
{
    assert(spec_class_for(size, 1usize << lg_align) == Some(c as usize));
}

/// A sized free of a huge run that returns normally
/// ([`crate::heap::huge_hint_ok`] of the caller's token for the run,
/// which the sized frees' cores ensure) was given a size whose `nallocx`
/// is the run's usable size, its `sallocx`, at every alignment that
/// leaves the size huge and placeable (`nalloc_usable`'s huge branch).
/// At another alignment the size still rounds up to the run's pages:
/// that is the small size of a run an alignment made huge, or an
/// alignment no run could be placed at, which the check, by page count,
/// lets through.
pub proof fn theorem_sized_run_nallocx(blk: Live, p: *mut u8, size: usize, lg_align: u32)
    requires
        crate::heap::huge_hint_ok(blk, p, size),
        blk.claims(p),
        !crate::heap::slot_start(p),
        crate::layout::process_regions().is_huge_block(p@.addr, 1),
        spec_class_for(size, 1usize << lg_align) is None,
        huge_placeable(size, 1usize << lg_align),
    ensures
        nalloc_usable(size, lg_align) == blk.len(),
{
}

/// What `get_defrag_hint` read (ghost): whether the allocator was ready,
/// `p`'s classification, the slot's bitmap bit (`allocated`), that its
/// slab is below the watermark (`committed`), the slab's live count, that
/// the arena its descriptor names is built (`found`), that arena's lowest
/// partial slab of the class (`lowest`), its counters from one lock hold
/// (`st`, `curregs`: `bin_stats`), and what the current slab took out of
/// `curregs` (`cur_live`: the live count of the lowest partial slab,
/// clamped at `curregs`; `None` when there is no such slab or its
/// descriptor read as uncommitted).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct HintRead {
    pub ready: bool,
    pub kind: Kind,
    pub allocated: bool,
    pub committed: bool,
    pub live: int,
    pub found: bool,
    pub lowest: Option<usize>,
    pub st: crate::class::ClassStats,
    pub curregs: int,
    pub cur_live: Option<int>,
}

/// The design 13.5 rule over the reads `rd`: `p` is a live slot (bit set,
/// slab committed) of a slab class, its slab is not full (`live <
/// nslots`), the owning arena is built, the slab is not the class's lowest
/// partial slab there (jemalloc's `slabcur`), and `live * (curslabs - 1)
/// <= curregs - live(slabcur)`, the current slab taken out of the average
/// (both differences clamped at zero) when its descriptor was read, and
/// `live * curslabs <= curregs` when it was not.
#[verifier::opaque]
pub open spec fn defrag_rule(rd: HintRead) -> bool {
    &&& rd.ready
    &&& match rd.kind {
        Kind::Slab { class, slab, .. } => {
            let n = class_table()[class as int].nslots as int;
            let curslabs = rd.st.committed - rd.st.empty - rd.st.purged;
            &&& rd.allocated
            &&& rd.committed
            &&& rd.live < n
            &&& rd.found
            &&& rd.lowest != Some(slab as usize)
            &&& match rd.cur_live {
                Some(l) => rd.live * crate::api::mallctl::sat_sub(curslabs, 1) <= crate::api::mallctl::sat_sub(rd.curregs, l),
                None => rd.live * curslabs <= rd.curregs,
            }
        },
        _ => false,
    }
}

/// `rd` is a possible read of a `get_defrag_hint(p)` that returned `r`:
/// once the allocator is ready, `rd.kind` is `p`'s classification in the
/// process's regions; the counters are within what `bin_stats` returns,
/// the slab's live count fits a `u32`, and `r` is 1 exactly when
/// [`defrag_rule`] holds. `cur_live` is the amount `curregs` lost to the
/// current slab (its live count, clamped at `curregs`).
pub open spec fn hint_read(p: usize, rd: HintRead, r: c_int) -> bool {
    &&& rd.ready ==> crate::api::mallctl::classified(crate::layout::process_regions(), p, rd.kind)
    &&& rd.st.empty + rd.st.purged <= rd.st.committed
    &&& 0 <= rd.live <= u32::MAX
    &&& 0 <= rd.curregs
    &&& (rd.cur_live matches Some(l) ==> 0 <= l)
    &&& r == if defrag_rule(rd) { 1int } else { 0int }
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
/// initialised. Never aborts over the pointer: a defragmenter asks about
/// pointers it owns, and a wrong answer only costs it a move. Before it
/// answers, the call runs [`crate::heap::Heap::gc_for_defrag`] on the
/// calling thread's heap: the cache GC, at most once per
/// `DEFRAG_GC_INTERVAL_NS` (100 ms), with the clock read once every
/// `PERIODIC_EVERY` calls. Cached slots are invisible to the defragmenter
/// and keep their slabs occupied, so without it a large cache (Redis 8 sets
/// 1000 slots a class) holds the class's figures above the defragmenter's
/// threshold until the periodic GC while the defragmenter rescans. The
/// GC's release is the free path's and can abort where a valid program's
/// free can (a failed system call, or one of the pending free-path checks:
/// `valid_aborts`), and the thread-heap lookup at an integrity check
/// (`may_abort_corrupt`), so the hint requires those two and nothing
/// wider; `theorem_defrag_hint_aborts_only_in_its_gc` (`api/mod.rs`)
/// calls it with only them. It is not among the read-only entry points of
/// `theorem_read_only_never_aborts`. Cold: two class-lock acquisitions per
/// call, as the verbose utilization query, a counter, and now and then a
/// clock read and a GC pass.
#[allow(non_shorthand_field_patterns)] // `verus!`'s expansion of the `let`-`else`
#[cold]
#[inline(never)]
pub fn get_defrag_hint(p: *const u8) -> (r: c_int)
    requires
        crate::sys::may_abort() || crate::sys::valid_aborts(),
        crate::sys::may_abort_corrupt(),
        crate::sys::may_syscall(),
    ensures
        r == 0 || r == 1,
        // Some reading of the state satisfies `hint_read`. Only its
        // readiness and `p`'s classification are tied to the allocator
        // here, so a 1 states that the allocator was ready and `p` is a
        // slab slot; the rest of the design 13.5 rule holds over some
        // reading (a 0 claims less).
        exists|rd: HintRead| #[trigger] hint_read(p as usize, rd, r),
{
    proof {
        reveal(defrag_rule);
    }
    let ghost mut rd = HintRead {
        ready: false,
        kind: Kind::Invalid,
        allocated: false,
        committed: false,
        live: 0,
        found: false,
        lowest: None,
        st: crate::class::ClassStats { committed: 0, guards: 0, empty: 0, purged: 0, nonfull: 0, closed_slots: 0, closed_pages: 0, closable_slots: 0 },
        curregs: 0,
        cur_live: None,
    };
    let Some(shared) = super::shared_if_ready() else {
        proof {
            assert(hint_read(p as usize, rd, 0));
        }
        return 0;
    };
    proof {
        shared.lemma_wf();
        shared.lemma_wf_ready();
        rd.ready = true;
    }
    // Premise (lock order): called from the entry point, which holds no
    // lock.
    let tracked mut ord = axiom_entry();
    if let Some(h) = crate::heap::existing_heap(crate::heap::shared_ptr(shared)) {
        h.gc_for_defrag(shared, Tracked(&mut ord));
    }
    let Kind::Slab { class, slab, slot } = shared.regions.classify(p as usize) else {
        proof {
            // `rd.kind` stays `Invalid`: `classified` allows it for a huge
            // page start, and `defrag_rule` treats the two alike.
            assert(hint_read(p as usize, rd, 0));
        }
        return 0;
    };
    proof {
        rd.kind = Kind::Slab { class, slab, slot };
    }
    let class = class as usize;
    let t = shared.table(class);
    let (slab, slot) = (slab as usize, slot as usize);
    if !t.is_allocated(slab, slot) {
        proof {
            assert(hint_read(p as usize, rd, 0));
        }
        return 0;
    }
    proof {
        rd.allocated = true;
    }
    let Some(d) = t.desc_if_committed(slab) else {
        proof {
            assert(hint_read(p as usize, rd, 0));
        }
        return 0;
    };
    let cls = t.class();
    let live = d.live();
    proof {
        rd.committed = true;
        rd.live = live as int;
    }
    if live >= cls.nslots() {
        proof {
            assert(hint_read(p as usize, rd, 0));
        }
        return 0;
    }
    let Some(a) = shared.arenas.get_existing(d.arena()) else {
        proof {
            assert(hint_read(p as usize, rd, 0));
        }
        return 0;
    };
    proof {
        rd.found = true;
    }
    let cs = a.class(class);
    // A defragmenter is at work on this class: its slab-pages walk runs at
    // once and closes what the moves left closable (lock-free, at most
    // once per 100 ms per class; docs/proposals/slab-pages.md, 7.1).
    cs.note_defrag_hint();
    let slabcur = cs.lowest_partial(Tracked(&mut ord));
    proof {
        rd.lowest = slabcur;
    }
    if slabcur == Some(slab) {
        proof {
            assert(hint_read(p as usize, rd, 0));
        }
        return 0;
    }
    let (st, mut curregs) = cs.bin_stats(Tracked(&mut ord));
    let mut curslabs = st.curslabs();
    proof {
        rd.st = st;
        rd.curregs = curregs as int;
    }
    if let Some(dc) = slabcur.and_then(|s| t.desc_if_committed(s)) {
        // Take the current slab out of the average, as jemalloc does.
        curregs = curregs.saturating_sub(dc.live());
        curslabs = curslabs.saturating_sub(1);
        proof {
            rd.cur_live = Some(rd.curregs - curregs);
        }
    }
    proof {
        // `live` is below the class's `nslots` and `curslabs` at most the
        // committed slabs, whose product with `nslots` fits.
        let n = class_table()[class as int].nslots as int;
        let c = st.committed as int;
        assert((live as int) * (curslabs as int) <= c * n) by (nonlinear_arith)
            requires
                0 <= live < n,
                0 <= curslabs <= c;
    }
    proof {
        assert(hint_read(p as usize, rd, (live * curslabs <= curregs) as c_int));
    }
    (live * curslabs <= curregs) as c_int
}

} // verus!

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see `libc.rs`).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    #[cfg(verus_keep_ghost)]
    use crate::vspec::cbuf::{CCall, CEntry};
    use core::ffi::{c_int, c_void};
    #[cfg(verus_keep_ghost)]
    use vstd::prelude::*;
    use vstd::prelude::{Tracked, verus};

    // `verified_export_pair!` expands inside its own `verus!`; this outer
    // block holds it (Verus passes the invocation through).
    verus! {

    /// The token of a C call that hands over the block at `p` to
    /// `rallocx`, `dallocx` or `sdallocx` ([`CCall`], [`CEntry::Frees`]).
    /// Trusted: as `libc`'s `axiom_c_call`, which this module cannot name.
    /// Called only in the export bodies, once each, for the call's own
    /// `p`: checked by `api::tests::c_call_tokens_only_in_export_bodies`.
    axiom fn axiom_c_call(e: CEntry) -> (tracked c: CCall)
        requires
            e is Frees,
        ensures
            c.fresh(e),
    ;

    verified_export_pair! {
        fn mallocx / je_mallocx(size: usize, flags: c_int) -> (r: *mut c_void) ensures [
            crate::heap::fits(r as *mut u8, size as int, (1usize << ((flags as u32) & 0x3f)) as int),
            r@.addr != 0 ==> crate::vspec::usable::usable_is(r as *mut u8, size as int, super::nalloc_usable(size, (flags as u32) & 0x3f)),
            super::tcache_checked(size, flags),
        ] {
            let Tracked(mut errno) = crate::vspec::errno::lend();
            super::mallocx(size, flags, Tracked(&mut errno)) as *mut c_void
        }
        fn rallocx / je_rallocx(p: *mut c_void, size: usize, flags: c_int) -> (r: *mut c_void) ensures [
            crate::heap::fits(r as *mut u8, size as int, (1usize << ((flags as u32) & 0x3f)) as int),
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
            p@.addr != 0 && r@.addr != 0 && r as *mut u8 != p as *mut u8 ==> !crate::heap::mallocx::bad_tcache(crate::heap::mallocx::decode_spec(flags as u32).tcache),
        ] {
            let Tracked(mut errno) = crate::vspec::errno::lend();
            super::rallocx(p as *mut u8, size, flags, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno)) as *mut c_void
        }
        fn xallocx / je_xallocx(p: *mut c_void, size: usize, extra: usize, flags: c_int) -> (r: usize) ensures [
            crate::heap::free_target(p as *mut u8),
            crate::vspec::usable::usable_of(p as *mut u8, r as int),
            p@.addr != 0,
            0 < r,
        ] {
            super::xallocx(p as *mut u8, size, extra, flags)
        }
        fn sallocx / je_sallocx(p: *mut c_void, flags: c_int) -> (r: usize) ensures [
            crate::heap::free_target(p as *mut u8),
            crate::vspec::usable::usable_of(p as *mut u8, r as int),
            p@.addr != 0,
            0 < r,
        ] {
            super::sallocx(p as *mut u8, flags)
        }
        fn dallocx / je_dallocx(p: *mut c_void, flags: c_int) ensures [
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
            p@.addr != 0 ==> !crate::heap::mallocx::bad_tcache(crate::heap::mallocx::decode_spec(flags as u32).tcache),
        ] {
            let Tracked(mut errno) = crate::vspec::errno::lend();
            super::dallocx(p as *mut u8, flags, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
        }
        fn sdallocx / je_sdallocx(p: *mut c_void, size: usize, flags: c_int) ensures [
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
            p@.addr != 0 ==> !crate::heap::mallocx::bad_tcache(crate::heap::mallocx::decode_spec(flags as u32).tcache),
            p@.addr != 0 ==> crate::heap::hint_ok(p as *mut u8, super::spec_class_for(size, 1usize << ((flags as u32) & 0x3f))),
        ] {
            let Tracked(mut errno) = crate::vspec::errno::lend();
            super::sdallocx(p as *mut u8, size, flags, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
        }
        fn nallocx / je_nallocx(size: usize, flags: c_int) -> (r: usize) ensures [
            r == 0 || size <= r,
            r == super::nalloc_usable(size, (flags as u32) & 0x3f),
        ] {
            super::nallocx(size, flags)
        }
        fn get_defrag_hint / je_get_defrag_hint(p: *mut c_void) -> (r: c_int) ensures [
            r == 0 || r == 1,
            exists|rd: super::HintRead| #[trigger] super::hint_read((p as *const u8) as usize, rd, r),
        ] {
            super::get_defrag_hint(p as *const u8)
        }
    }

    } // verus!
}

#[cfg(test)]
mod tests;

/// Kani checks of `message_hook` and `call_hook`
/// (`verus/ledger/src/api/jemalloc.rs.md`; `cfg(kani)` only, see
/// `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_hook {
    use super::{Hook, MallocMessage, call_hook, je_malloc_message, malloc_message, message_hook};
    use core::ffi::{c_char, c_void};
    use core::ptr;
    use vstd::prelude::Tracked;

    /// How many times `record` was called, and the last call's
    /// `cbopaque` and `s` as addresses.
    static mut CALLS: usize = 0;
    static mut GOT: (usize, usize) = (0, 0);

    /// A hook that records its arguments.
    unsafe extern "C" fn record(opaque: *mut c_void, s: *const c_char) {
        // SAFETY: the harness's own statics; no other code runs in a
        // harness.
        unsafe {
            CALLS += 1;
            GOT = (opaque as usize, s as usize);
        }
    }

    /// `call_hook`: no `ensures`; it returns for any buffer holding a NUL
    /// (its `requires`; the token is ghost), and (beyond the contract)
    /// calls the hook once, with a null `cbopaque` and the buffer's
    /// address.
    #[kani::proof]
    fn call_hook_calls_once() {
        let buf: [u8; 512] = kani::any();
        let nul: usize = kani::any();
        kani::assume(nul < 512 && buf[nul] == 0);
        call_hook(Hook(record), &buf, Tracked::assume_new());
        // SAFETY: as in `record`.
        let (calls, got) = unsafe { (CALLS, GOT) };
        assert_eq!(calls, 1);
        assert_eq!(got, (0, buf.as_ptr() as usize));
    }

    unsafe extern "C" fn first(_: *mut c_void, _: *const c_char) {}

    unsafe extern "C" fn second(_: *mut c_void, _: *const c_char) {}

    /// `None`, `first` or `second`.
    fn any_hook() -> MallocMessage {
        if kani::any() {
            None
        } else if kani::any() {
            Some(first)
        } else {
            Some(second)
        }
    }

    /// The address of a hook, 0 for none.
    fn addr(h: MallocMessage) -> usize {
        h.map_or(0, |f| f as usize)
    }

    /// `message_hook`: no `ensures`; it returns, for every pair of values
    /// of `malloc_message` and `je_malloc_message` (each none or one of
    /// two functions), and (beyond the contract) gives `malloc_message`
    /// when it is set, else `je_malloc_message`.
    #[kani::proof]
    fn message_hook_picks_first_set() {
        let (m, j) = (any_hook(), any_hook());
        // SAFETY: the two statics, written only here (no other code runs
        // in a harness).
        unsafe {
            ptr::write_volatile(&raw mut malloc_message, m);
            ptr::write_volatile(&raw mut je_malloc_message, j);
        }
        let want = if m.is_some() { m } else { j };
        assert_eq!(addr(message_hook().map(|h| h.0)), addr(want));
    }
}
