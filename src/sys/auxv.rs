//! The auxiliary vector: `getauxval(3)` and the `AT_*` keys the crate
//! reads (`include/uapi/linux/auxvec.h`, the same values on x86_64 and
//! aarch64).
//!
//! The one libc call the allocator makes on its own behalf. glibc answers
//! from the vector the kernel put on the initial stack, saved at startup:
//! no syscall, no allocation, safe from within `malloc`, which is how
//! glibc's own malloc learns the page size (`GLRO(dl_pagesize)`). The
//! readers are `sys::features` (`AT_PAGESZ`), `sys::pac` (`AT_HWCAP`) and
//! `config::load` (`AT_SECURE`).

use vstd::prelude::*;

verus! {

/// `AT_NULL`: end of the vector.
#[cfg(test)]
pub const AT_NULL: usize = 0;
/// `AT_PAGESZ`: the system page size.
pub const AT_PAGESZ: usize = 6;
/// `AT_HWCAP`: the first arm64 hardware-capability word (`HWCAP_*`).
#[cfg(any(test, target_arch = "aarch64"))]
pub const AT_HWCAP: usize = 16;
/// `AT_RSEQ_FEATURE_SIZE`: the bytes of `struct rseq` the kernel writes
/// (6.3+); read by the tests, glibc uses it to size its registration.
#[cfg(test)]
pub const AT_RSEQ_FEATURE_SIZE: usize = 27;
/// `AT_SECURE`: set for setuid/setgid/capability-elevated processes, whose
/// environment is untrusted (glibc's `secure_getenv` rule; `config::load`
/// skips the environment sources when it is set).
pub const AT_SECURE: usize = 23;

/// The process's `AT_SECURE` word: what `getauxval(AT_SECURE)` returns.
/// One word for the life of the process: the kernel writes the vector at
/// `execve` and never rewrites it. Uninterpreted: nothing is known
/// of its value; [`secure_exec`] names the case it is not 0.
pub uninterp spec fn at_secure() -> usize;

/// A secure-execution process (`AT_SECURE` set: setuid, setgid, file
/// capabilities or an LSM transition), whose environment is untrusted.
/// `config::env_var`, the one reader of `environ`, requires that the
/// process is not one.
pub open spec fn secure_exec() -> bool {
    at_secure() != 0
}

/// glibc's `getauxval(3)`: some word, 0 when the key is absent. The
/// kernel's page (`AT_PAGESZ`) is a power of two of at most 64 KiB:
/// Linux uses 4 KiB pages on x86_64 and 4, 16 or 64 KiB on aarch64, and
/// always passes `AT_PAGESZ`. It is the process regions' page: init
/// reserves them with it (`sys::features::page_size`), and a process's
/// `AT_PAGESZ` never changes. For `AT_SECURE` it returns [`at_secure`],
/// the same word at every call. Trusted: the kernel writes the vector and
/// glibc returns the word it wrote.
#[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
pub assume_specification[ getauxval ](kind: usize) -> (r: usize)
    ensures
        kind == AT_PAGESZ ==> r != 0 && crate::meta::is_pow2(r) && r <= 0x1_0000,
        kind == AT_PAGESZ ==> r == (1usize << crate::layout::process_regions().lg_page),
        kind == AT_SECURE ==> r == at_secure(),
;

/// As above, and on arm64: an `AT_HWCAP` word with `HWCAP_PACA` and
/// `HWCAP_PACG` set means the process may execute the data-key A
/// instructions ([`crate::sys::pac::pac_available`]). Trusted (Linux
/// `arch/arm64`): the kernel sets `HWCAP_PACA` only when every CPU
/// implements FEAT_PAuth with an address-key algorithm (a CPU without it
/// is not brought online), and glibc returns the word the kernel wrote.
/// `AT_SECURE` as above.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub assume_specification[ getauxval ](kind: usize) -> (r: usize)
    ensures
        kind == AT_PAGESZ ==> r != 0 && crate::meta::is_pow2(r) && r <= 0x1_0000,
        kind == AT_PAGESZ ==> r == (1usize << crate::layout::process_regions().lg_page),
        kind == AT_SECURE ==> r == at_secure(),
        kind == AT_HWCAP ==> (r as u64 & crate::sys::pac::HWCAP_PACA != 0 && r as u64 & crate::sys::pac::HWCAP_PACG != 0 ==> crate::sys::pac::pac_available()),
;

} // verus!

unsafe extern "C" {
    /// glibc's `getauxval(3)`: the value for `kind`, 0 when absent.
    pub safe fn getauxval(kind: usize) -> usize;
}

#[cfg(test)]
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
        // SAFETY: `getauxval` has no preconditions.
        let (pagesz, hwcap, rseq) = unsafe {
            (
                libc::getauxval(libc::AT_PAGESZ),
                libc::getauxval(libc::AT_HWCAP),
                libc::getauxval(27),
            )
        };
        assert_eq!(getauxval(AT_PAGESZ), pagesz as usize);
        assert_eq!(getauxval(AT_HWCAP), hwcap as usize);
        assert_eq!(getauxval(AT_RSEQ_FEATURE_SIZE), rseq as usize);
        let raw = std::fs::read("/proc/self/auxv").unwrap();
        const W: usize = core::mem::size_of::<usize>();
        let mut found = std::collections::BTreeMap::new();
        for pair in raw.as_chunks::<{ 2 * W }>().0 {
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
        // `AT_HWCAP` through `getauxval` on aarch64 alone (pac.rs).
        assert!(
            found.contains_key(&AT_HWCAP),
            "AT_HWCAP is in every Linux vector"
        );
        #[cfg(target_arch = "aarch64")]
        assert_eq!(found.get(&AT_HWCAP).copied(), Some(getauxval(AT_HWCAP)));
    }
}
