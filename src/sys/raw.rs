//! Raw syscall instructions. Each function returns the kernel's untranslated
//! result word; callers go through `errno::decode`.
//!
//! These never touch libc, so they cannot disturb `errno`, which matters for
//! `free`. The `asm!` blocks deliberately omit `nomem`/`readonly`: a syscall
//! may read or write any memory the arguments point at.
//!
//! These are the trusted base of the verified wrappers in `sys`: each is
//! `external_body` and specified as returning some word, nothing more.
//! `syscall3` also names the word as the kernel's answer to its call
//! ([`returned3`]), which means nothing until a kernel-fact axiom in `sys`
//! (one per system call) says what that answer can be.

use vstd::prelude::verus;
#[cfg(verus_only)]
use vstd::prelude::*;

verus! {

/// Ghost: `r` is the word the kernel returned to a `syscall3(nr, a0, a1,
/// a2)` call. Uninterpreted, and ensured only by `syscall3`: it says
/// nothing by itself, and gives the `sys` kernel-fact axioms
/// (`axiom_read_count` and its siblings) the call they are about.
pub uninterp spec fn returned3(nr: usize, a0: usize, a1: usize, a2: usize, r: usize) -> bool;

} // verus!

#[cfg(target_arch = "x86_64")]
mod imp {
    use core::arch::asm;
    use vstd::prelude::*;

    verus! {

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall0(nr: usize) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: caller guarantees the syscall number and argument contract.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall1(nr: usize, a0: usize) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> (r: usize)
        requires
            crate::sys::may_syscall(),
        ensures
            super::returned3(nr, a0, a1, a2, r),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 in("rdx") a2, lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 in("rdx") a2, in("r10") a3, lateout("rcx") _, lateout("r11") _,
                 options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall5(
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
    ) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 in("rdx") a2, in("r10") a3, in("r8") a4, lateout("rcx") _,
                 lateout("r11") _, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall6(
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 in("rdx") a2, in("r10") a3, in("r8") a4, in("r9") a5,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    } // verus!
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use core::arch::asm;
    use vstd::prelude::*;

    verus! {

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall0(nr: usize) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: caller guarantees the syscall number and argument contract.
        unsafe {
            asm!("svc 0", in("x8") nr, lateout("x0") ret, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall1(nr: usize, a0: usize) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> (r: usize)
        requires
            crate::sys::may_syscall(),
        ensures
            super::returned3(nr, a0, a1, a2, r),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 in("x2") a2, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 in("x2") a2, in("x3") a3, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall5(
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
    ) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 in("x2") a2, in("x3") a3, in("x4") a4, options(nostack));
        }
        ret
    }

    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn syscall6(
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> usize
        requires
            crate::sys::may_syscall(),
    {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 in("x2") a2, in("x3") a3, in("x4") a4, in("x5") a5, options(nostack));
        }
        ret
    }

    } // verus!
}

pub use imp::*;
