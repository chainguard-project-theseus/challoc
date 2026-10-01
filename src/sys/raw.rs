//! Raw syscall instructions. Each function returns the kernel's untranslated
//! result word; callers go through `errno::decode`.
//!
//! These never touch libc, so they cannot disturb `errno`, which matters for
//! `free`. The `asm!` blocks deliberately omit `nomem`/`readonly`: a syscall
//! may read or write any memory the arguments point at.

#[cfg(target_arch = "x86_64")]
mod imp {
    use core::arch::asm;

    #[inline(always)]
    pub unsafe fn syscall0(nr: usize) -> usize {
        let ret: usize;
        // SAFETY: caller guarantees the syscall number and argument contract.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall1(nr: usize, a0: usize) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 in("rdx") a2, lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 in("rdx") a2, in("r10") a3, lateout("rcx") _, lateout("r11") _,
                 options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall5(
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
    ) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 in("rdx") a2, in("r10") a3, in("r8") a4, lateout("rcx") _,
                 lateout("r11") _, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall6(
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0, in("rsi") a1,
                 in("rdx") a2, in("r10") a3, in("r8") a4, in("r9") a5,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use core::arch::asm;

    #[inline(always)]
    pub unsafe fn syscall0(nr: usize) -> usize {
        let ret: usize;
        // SAFETY: caller guarantees the syscall number and argument contract.
        unsafe {
            asm!("svc 0", in("x8") nr, lateout("x0") ret, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall1(nr: usize, a0: usize) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 in("x2") a2, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 in("x2") a2, in("x3") a3, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall5(
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
    ) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 in("x2") a2, in("x3") a3, in("x4") a4, options(nostack));
        }
        ret
    }

    #[inline(always)]
    pub unsafe fn syscall6(
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> usize {
        let ret: usize;
        // SAFETY: as above.
        unsafe {
            asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
                 in("x2") a2, in("x3") a3, in("x4") a4, in("x5") a5, options(nostack));
        }
        ret
    }
}

pub use imp::*;
