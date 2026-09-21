//! The auxiliary vector: `getauxval(3)` and the `AT_*` keys the crate
//! reads (`include/uapi/linux/auxvec.h`, the same values on x86_64 and
//! aarch64).
//!
//! The one libc call the allocator makes on its own behalf. glibc and musl
//! answer from the vector the kernel put on the initial stack, saved at
//! startup: no syscall, no allocation, safe from within `malloc`, which is
//! how glibc's own malloc learns the page size (`GLRO(dl_pagesize)`). The
//! readers are `sys::features` (`AT_PAGESZ`), `sys::cpu` and `sys::pac`
//! (`AT_HWCAP`, `AT_HWCAP2`); `config.rs` declares its own `getauxval` for
//! `AT_SECURE` and keeps that constant beside it.

/// `AT_NULL`: end of the vector.
pub const AT_NULL: usize = 0;
/// `AT_PAGESZ`: the system page size.
pub const AT_PAGESZ: usize = 6;
/// `AT_HWCAP`: the first arm64 hardware-capability word (`HWCAP_*`).
pub const AT_HWCAP: usize = 16;
/// `AT_HWCAP2`: the second arm64 hardware-capability word (`HWCAP2_*`).
pub const AT_HWCAP2: usize = 26;
/// `AT_RSEQ_FEATURE_SIZE`: the bytes of `struct rseq` the kernel writes
/// (6.3+); read by the tests, glibc uses it to size its registration.
#[cfg(all(test, target_os = "linux"))]
pub const AT_RSEQ_FEATURE_SIZE: usize = 27;
/// `AT_SECURE`: set for setuid/setgid/capability-elevated processes, whose
/// environment is untrusted (glibc's `secure_getenv` rule; `config::load`
/// skips the environment sources when it is set).
pub const AT_SECURE: usize = 23;

#[cfg(target_os = "linux")]
unsafe extern "C" {
    /// glibc/musl `getauxval(3)`: the value for `kind`, 0 when absent.
    pub safe fn getauxval(kind: usize) -> usize;
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    /// The keys against the libc crate's and the vector the kernel wrote
    /// (`/proc/self/auxv`), so a wrong constant reads the wrong entry
    /// here rather than in a probe.
    #[test]
    fn keys_match_libc_and_the_vector() {
        assert_eq!(AT_NULL as u64, libc::AT_NULL);
        assert_eq!(AT_PAGESZ as u64, libc::AT_PAGESZ);
        assert_eq!(AT_HWCAP as u64, libc::AT_HWCAP);
        assert_eq!(AT_HWCAP2 as u64, libc::AT_HWCAP2);
        // SAFETY: `getauxval` has no preconditions.
        assert_eq!(getauxval(AT_PAGESZ), unsafe { libc::getauxval(libc::AT_PAGESZ) } as usize);
        assert_eq!(getauxval(AT_HWCAP), unsafe { libc::getauxval(libc::AT_HWCAP) } as usize);
        assert_eq!(getauxval(AT_RSEQ_FEATURE_SIZE), unsafe { libc::getauxval(27) } as usize);
        let raw = std::fs::read("/proc/self/auxv").unwrap();
        const W: usize = core::mem::size_of::<usize>();
        let mut found = std::collections::BTreeMap::new();
        for pair in raw.chunks_exact(2 * W) {
            let key = usize::from_ne_bytes(pair[..W].try_into().unwrap());
            let val = usize::from_ne_bytes(pair[W..].try_into().unwrap());
            if key == AT_NULL {
                break;
            }
            found.insert(key, val);
        }
        assert_eq!(found.get(&AT_PAGESZ).copied(), Some(getauxval(AT_PAGESZ)));
        // glibc's x86 startup overwrites its `AT_HWCAP` with a word of its
        // own (`HWCAP_X86_64`, the AVX-512 usability bits), so `getauxval`
        // and the kernel's vector agree only on aarch64; the crate reads
        // `AT_HWCAP` through `getauxval` on aarch64 alone (cpu.rs, pac.rs)
        // and `cpuid` on x86_64.
        assert!(found.contains_key(&AT_HWCAP), "AT_HWCAP is in every Linux vector");
        #[cfg(target_arch = "aarch64")]
        assert_eq!(found.get(&AT_HWCAP).copied(), Some(getauxval(AT_HWCAP)));
        assert_eq!(found.get(&AT_HWCAP2).copied().unwrap_or(0), getauxval(AT_HWCAP2));
    }
}
