//! Cold stacks: the allocator's own stack for its heavy, rare work
//! (design 13.8).
//!
//! The statistics and report entry points (`mallctl`, `malloc_stats_print`,
//! `malloc_info`, `malloc_stats`, `mallinfo`), the deferred cgroup
//! discovery (which the periodic tick of any `malloc`, `fork`'s prepare
//! handler or a statistics read may run), the build of an automatic arena
//! (a thread's first allocation) and initialisation need 8 to 36 KiB of
//! stack: the per-arena figures with their bins, the report's buffers, the
//! cgroup scanner's path and line buffers, the class states. A thread
//! created with `PTHREAD_STACK_MIN` (16 KiB on x86_64 glibc, of which
//! glibc's TCB and static TLS take a few) has far less, and the same calls
//! work there under glibc's malloc and jemalloc. [`run`] runs such work on
//! a stack of the allocator's instead, so what any entry point needs of
//! the caller's stack stays small (the `small_stack` C test and the
//! frame-size test in `api::c_programs` hold it there).
//!
//! The stacks are a fixed pool of [`SLOTS`] slots of [`STACK_BYTES`]
//! each, mapped on a slot's first use with a `PROT_NONE` guard page below
//! (an overflow faults rather than running into other memory) and kept for
//! the life of the process. A call claims a free slot with one
//! compare-exchange of its owner word from 0 to the thread pointer and
//! frees it with a store, so nothing ever waits: concurrent calls, a
//! signal handler's call while its thread is on a cold stack, and a call
//! from inside a `malloc_stats_print` callback each take another slot.
//! With every slot taken, or when the mapping fails, the work runs on the
//! caller's stack as it did before cold stacks existed.
//!
//! Cancellation: a `pthread_cancel` acted on (or a `pthread_exit` called)
//! inside a `malloc_stats_print` callback, which runs on the cold stack,
//! unwinds through it to the thread's start (the trampoline's CFI lets
//! the unwinder through) without freeing the slot: the library has no
//! unwind cleanups. Nothing waits on a slot, so nothing deadlocks; each
//! such cancellation keeps one slot taken, and with all [`SLOTS`] taken
//! the work runs on the caller's stack, as before cold stacks.
//!
//! `fork`: the child has only the forking thread, so a slot another
//! thread owned at the fork will never be freed; the child's handler
//! ([`fork_child`]) frees every slot the forking thread does not own (it
//! may own one when it forked from a callback or a signal handler that
//! runs on a cold stack, and it returns to that stack).

use super::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE};
use core::sync::atomic::{AtomicUsize, Ordering};
use vstd::prelude::*;

/// Cold stacks the process may hold at once.
pub const SLOTS: usize = 16;

/// Bytes of one cold stack, the guard page not included: about twice what
/// the deepest cold path needs (`malloc_stats_print` with every arena's
/// bins, a `stats.arenas.<i>.bins.<j>.*` read, the cgroup discovery).
pub const STACK_BYTES: usize = 64 * 1024;

/// Slot `i`'s owner: 0 when free, else the owning thread's thread pointer.
static OWNER: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];

/// Slot `i`'s stack top, 0 until it is mapped. Written only by the slot's
/// owner; the owner word's release store and acquire exchange order it
/// for the next owner.
static TOP: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];

verus! {

/// `f()`, on a cold stack when one is free (module doc). Trusted: the
/// result is `f`'s, whichever stack it ran on.
#[verifier::external_body]
#[inline(always)]
pub fn run<R, F: FnOnce() -> R>(f: F) -> (r: R)
    requires
        call_requires(f, ()),
    ensures
        call_ensures(f, (), r),
{
    run_impl(f)
}

/// The child's side of `fork` (module doc): every slot the forking thread
/// does not own is free again. Called only from `api::fork_child`, where
/// only the forking thread runs.
#[verifier::external_body]
pub fn fork_child() {
    let me = thread_pointer();
    for o in OWNER.iter() {
        if o.load(Ordering::Relaxed) != me {
            o.store(0, Ordering::Relaxed);
        }
    }
}

} // verus!

#[inline(never)]
fn run_impl<R, F: FnOnce() -> R>(f: F) -> R {
    let me = thread_pointer();
    for (owner, top) in OWNER.iter().zip(TOP.iter()) {
        if owner.load(Ordering::Relaxed) != 0
            || owner
                .compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
        {
            continue;
        }
        let t = stack_top(top);
        if t == 0 {
            owner.store(0, Ordering::Release);
            break;
        }
        let mut state: (Option<F>, Option<R>) = (Some(f), None);
        // SAFETY: `t` is the top of a mapped, writable stack of
        // `STACK_BYTES` with a guard page below, owned by this thread until
        // the store below; `call::<R, F>` reads `state` as the type it is.
        unsafe {
            switch(
                t,
                call::<R, F>,
                &mut state as *mut (Option<F>, Option<R>) as *mut u8,
            )
        };
        owner.store(0, Ordering::Release);
        match state.1 {
            Some(r) => return r,
            // `call` always stores the result.
            None => super::trap(),
        }
    }
    in_place(f)
}

/// `f()` on the caller's stack, in a frame of its own: inlined into
/// [`run_impl`], `f`'s locals would be in its frame on every call.
#[inline(never)]
#[cold]
fn in_place<R, F: FnOnce() -> R>(f: F) -> R {
    f()
}

/// The function [`switch`] calls on the cold stack: `state.1 = (state.0)()`.
///
/// # Safety
/// `p` points to a live `(Option<F>, Option<R>)`.
unsafe extern "C" fn call<R, F: FnOnce() -> R>(p: *mut u8) {
    // SAFETY: guaranteed by the caller.
    let state = unsafe { &mut *(p as *mut (Option<F>, Option<R>)) };
    if let Some(f) = state.0.take() {
        state.1 = Some(f());
    }
}

/// The top of the stack in `top`, mapping it first if it is not yet: 0 if
/// the mapping fails.
#[cold]
fn map_stack(top: &AtomicUsize) -> usize {
    let page = super::features::page_size();
    let len = STACK_BYTES + page;
    // SAFETY: a fresh private anonymous mapping at no fixed address.
    let Ok(base) = (unsafe {
        super::mmap(
            0,
            len,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
            -1,
            0,
        )
    }) else {
        return 0;
    };
    // SAFETY: the lowest page of the mapping just made, which nothing uses.
    if unsafe { super::mprotect(base, page, PROT_NONE) }.is_err() {
        // SAFETY: the mapping just made, which nothing references.
        let _ = unsafe { super::munmap(base, len) };
        return 0;
    }
    // SAFETY: the name is a static NUL-terminated string. A kernel without
    // anonymous VMA names leaves the mapping unnamed.
    let _ = unsafe { super::set_vma_anon_name(base, len, b"challoc:cold-stack\0") };
    let t = base + len;
    top.store(t, Ordering::Relaxed);
    t
}

#[inline(always)]
fn stack_top(top: &AtomicUsize) -> usize {
    match top.load(Ordering::Relaxed) {
        0 => map_stack(top),
        t => t,
    }
}

/// The calling thread's thread pointer: nonzero and distinct for every
/// live thread, and the forking thread's in the child too.
#[inline(always)]
fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reads the thread pointer, which every thread has.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("mov {}, fs:0", out(reg) tp, options(nostack, readonly, preserves_flags));
    }
    // SAFETY: as above.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("mrs {}, tpidr_el0", out(reg) tp, options(nostack, nomem, preserves_flags));
    }
    tp
}

/// Call `f(arg)` with the stack pointer at `top`, then return on the
/// caller's stack. The frame pointer holds the caller's stack pointer
/// across the call, and the CFI says so, so a debugger's backtrace from
/// the cold stack continues into the caller's frames.
///
/// # Safety
/// `top` is the 16-byte aligned top of a writable stack no other code
/// uses, deep enough for `f`.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn switch(top: usize, f: unsafe extern "C" fn(*mut u8), arg: *mut u8) {
    core::arch::naked_asm!(
        ".cfi_startproc",
        "push rbp",
        ".cfi_def_cfa_offset 16",
        ".cfi_offset rbp, -16",
        "mov rbp, rsp",
        ".cfi_def_cfa_register rbp",
        "mov rsp, rdi",
        "mov rdi, rdx",
        "call rsi",
        "mov rsp, rbp",
        "pop rbp",
        ".cfi_def_cfa rsp, 8",
        "ret",
        ".cfi_endproc",
    )
}

/// See the x86_64 [`switch`]. `bti c` lets it be reached by an indirect
/// call on a BTI-enforcing system; elsewhere it is a no-op hint.
///
/// # Safety
/// As the x86_64 one.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn switch(top: usize, f: unsafe extern "C" fn(*mut u8), arg: *mut u8) {
    core::arch::naked_asm!(
        ".cfi_startproc",
        "hint #34",
        "stp x29, x30, [sp, #-16]!",
        ".cfi_def_cfa_offset 16",
        ".cfi_offset x30, -8",
        ".cfi_offset x29, -16",
        "mov x29, sp",
        ".cfi_def_cfa w29, 16",
        "mov sp, x0",
        "mov x0, x2",
        "blr x1",
        "mov sp, x29",
        ".cfi_def_cfa wsp, 16",
        "ldp x29, x30, [sp], #16",
        ".cfi_def_cfa_offset 0",
        ".cfi_restore x30",
        ".cfi_restore x29",
        "ret",
        ".cfi_endproc",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> usize {
        let x = 0u8;
        core::hint::black_box(&x) as *const u8 as usize
    }

    /// The closure runs on a cold stack (far from this thread's), gets its
    /// captures and returns its value; the slot is free again after.
    #[test]
    fn runs_on_a_cold_stack_and_frees_the_slot() {
        let here = sp();
        let mut seen = 0usize;
        let r = run(|| {
            seen = sp();
            let big = core::hint::black_box([7u8; 40 * 1024]);
            big[1234] as u32 + 1
        });
        assert_eq!(r, 8);
        let on_cold = |a: usize| {
            TOP.iter().any(|t| {
                let t = t.load(Ordering::Relaxed);
                t != 0 && t - STACK_BYTES <= a && a < t
            })
        };
        assert!(on_cold(seen) && !on_cold(here), "{here:#x} {seen:#x}");
        let me = thread_pointer();
        assert!(OWNER.iter().all(|o| o.load(Ordering::Relaxed) != me));
    }

    /// A call from inside a cold run takes another slot; with every slot
    /// taken the work runs in place.
    #[test]
    fn nested_calls_take_other_slots_then_run_in_place() {
        fn depth(n: usize) -> usize {
            if n == 0 {
                return 0;
            }
            run(|| depth(n - 1) + 1)
        }
        assert_eq!(depth(SLOTS + 3), SLOTS + 3);
    }
}
