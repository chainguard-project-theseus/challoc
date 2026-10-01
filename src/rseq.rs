//! glibc's restartable-sequences registration, read for the feature
//! line's `rseq` and `rseq_size` keys.
//!
//! glibc (2.44, the supported floor) registers one `struct rseq` per
//! thread with the kernel and exports three symbols from the dynamic
//! loader: `__rseq_offset`, the signed distance from the thread pointer to
//! that thread's area (it moves with the static TLS layout, so it is read
//! from the symbol, never assumed: 192 on the aarch64 reference VM, -192
//! bare and -256 under a preload on the x86_64 one), `__rseq_size`, the
//! number of bytes of the area the kernel writes (the auxiliary vector's
//! `AT_RSEQ_FEATURE_SIZE` rounded up; 20 on a kernel without the extended
//! fields), and `__rseq_flags`, the registration flags (0). The symbols
//! are weak references resolved through the GOT; `__rseq_size` is 0 when
//! the registration is off (`GLIBC_TUNABLES=glibc.pthread.rseq=0`, a
//! kernel without `CONFIG_RSEQ`), and then nothing here reads the area.
//! Nothing in this module is assumed from a header constant; [`probe`]
//! reads the symbols and this thread's `cpu_id`.
//!
//! The area (`struct rseq`, kernel `include/uapi/linux/rseq.h`), offsets in
//! bytes: `cpu_id_start` 0, `cpu_id` 4, `rseq_cs` 8 (u64), `flags` 16,
//! `node_id` 20, `mm_cid` 24 (u32), `slice_ctrl` 28. This module stores
//! to none of them: writing `cpu_id`, `mm_cid` or `flags` is what
//! broke tcmalloc on Linux 6.19.

use core::arch::asm;

use crate::sys::features::Value;

/// Byte offset of `cpu_id` (kernel-owned; `-1` before registration).
pub const CPU_ID_OFFSET: usize = 4;
/// The size of the original `struct rseq` (through `flags`), the least a
/// successful registration reports; `__rseq_size` is 0 or at least this.
pub const BASE_SIZE: u32 = 20;
/// `RSEQ_CPU_ID_UNINITIALIZED`.
pub const CPU_ID_UNINITIALIZED: u32 = u32::MAX;

/// What glibc registered, read from the three weak symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The symbols resolved.
    pub present: bool,
    /// `__rseq_size`: 0 when registration is off (`glibc.pthread.rseq=0`,
    /// or a kernel without `CONFIG_RSEQ`), 20 on kernels before the
    /// extended fields, more with them.
    pub size: u32,
    /// `__rseq_flags` (0).
    pub flags: u32,
    /// `__rseq_offset`: thread pointer to area, signed.
    pub offset: isize,
    /// The calling thread's `cpu_id` field is not `CPU_ID_UNINITIALIZED`:
    /// the kernel has written this thread's area at least once.
    pub registered: bool,
}

impl Probe {
    /// Nothing registered (the fallback when the symbols do not resolve).
    pub const NONE: Probe = Probe { present: false, size: 0, flags: 0, offset: 0, registered: false };

    /// The feature line's `rseq=` and `rseq_size=` words. `rseq=yes` when
    /// glibc registered an area (`__rseq_size` covers the original struct,
    /// [`BASE_SIZE`]) and the kernel has written this thread's `cpu_id`;
    /// `rseq_size` is the symbol's value even when the registration did not
    /// take (`0` without one: `GLIBC_TUNABLES=glibc.pthread.rseq=0` or a
    /// kernel without `CONFIG_RSEQ`; 20 on a kernel that reports no feature
    /// size).
    pub fn words(&self) -> impl Iterator<Item = (&'static str, Value)> {
        let registered = self.present && self.registered && self.size >= BASE_SIZE;
        [("rseq", Value::YesNo(registered)), ("rseq_size", Value::Num(u64::from(self.size)))].into_iter()
    }
}

/// Read the three weak symbols and the calling thread's `cpu_id` field.
pub fn probe() -> Probe {
    let (so, ss, sf) = symbols();
    if so.is_null() || ss.is_null() {
        return Probe::NONE;
    }
    // SAFETY: non-null addresses of the loader's `__rseq_offset`
    // (`ptrdiff_t`) and `__rseq_size` (`unsigned int`) globals, read
    // once; `__rseq_flags` likewise when present.
    let (offset, size) = unsafe { (so.read(), ss.read()) };
    let flags = if sf.is_null() { 0 } else { unsafe { sf.read() } };
    let registered = size >= (CPU_ID_OFFSET as u32 + 4) && {
        // SAFETY: the area is `size` bytes at the thread pointer plus
        // the offset for every thread, and `cpu_id` is inside it.
        unsafe { area(offset).add(CPU_ID_OFFSET).cast::<u32>().read_volatile() != CPU_ID_UNINITIALIZED }
    };
    Probe { present: true, size, flags, offset, registered }
}

/// Addresses of the weak `__rseq_offset`, `__rseq_size` and `__rseq_flags`
/// (null when they do not resolve). The same `.weak` + GOT form as
/// `config.rs` uses for `malloc_conf`: stable Rust has no weak statics.
#[inline(never)]
fn symbols() -> (*const isize, *const u32, *const u32) {
    let (so, ss, sf): (*const isize, *const u32, *const u32);
    // SAFETY: GOT loads of three weak symbols; no memory is dereferenced.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            ".weak __rseq_offset",
            ".weak __rseq_size",
            ".weak __rseq_flags",
            "mov {so}, qword ptr [rip + __rseq_offset@GOTPCREL]",
            "mov {ss}, qword ptr [rip + __rseq_size@GOTPCREL]",
            "mov {sf}, qword ptr [rip + __rseq_flags@GOTPCREL]",
            so = out(reg) so,
            ss = out(reg) ss,
            sf = out(reg) sf,
            options(nostack, readonly, preserves_flags),
        );
        #[cfg(target_arch = "aarch64")]
        asm!(
            ".weak __rseq_offset",
            ".weak __rseq_size",
            ".weak __rseq_flags",
            "adrp {so}, :got:__rseq_offset",
            "ldr {so}, [{so}, :got_lo12:__rseq_offset]",
            "adrp {ss}, :got:__rseq_size",
            "ldr {ss}, [{ss}, :got_lo12:__rseq_size]",
            "adrp {sf}, :got:__rseq_flags",
            "ldr {sf}, [{sf}, :got_lo12:__rseq_flags]",
            so = out(reg) so,
            ss = out(reg) ss,
            sf = out(reg) sf,
            options(nostack, readonly, preserves_flags),
        );
    }
    (so, ss, sf)
}

/// The calling thread's thread pointer: `%fs:0` (the TCB's self pointer)
/// on x86_64, `tpidr_el0` on aarch64. The rseq area is at
/// `thread_pointer() + __rseq_offset`.
#[inline(always)]
pub fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reads the thread pointer; no other memory or flag effect.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!("mov {tp}, qword ptr fs:[0]", tp = out(reg) tp, options(nostack, readonly, preserves_flags));
        #[cfg(target_arch = "aarch64")]
        asm!("mrs {tp}, tpidr_el0", tp = out(reg) tp, options(nomem, nostack, preserves_flags));
    }
    tp
}

/// The calling thread's rseq area for the probed `offset`
/// (`Probe::offset`).
#[inline(always)]
pub fn area(offset: isize) -> *mut u8 {
    thread_pointer().wrapping_add_signed(offset) as *mut u8
}

/// The calling thread's `cpu_id` field (diagnostics and tests).
///
/// # Safety
/// `area` is this thread's registered area.
#[inline]
pub unsafe fn cpu_id(area: *mut u8) -> u32 {
    // SAFETY: guaranteed by the caller.
    unsafe { area.add(CPU_ID_OFFSET).cast::<u32>().read_volatile() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `probe` reads on a host whose glibc registered `size` bytes at
    /// `offset` (registered when the size is not 0).
    fn registered(size: u32, offset: isize) -> Probe {
        Probe { present: true, size, flags: 0, offset, registered: size != 0 }
    }

    /// The rseq words: a kernel with extended fields (`__rseq_size=33`,
    /// positive and negative offsets), one without (`__rseq_size=20`), no
    /// symbols, a registration the kernel never wrote, and a size below the
    /// original struct.
    #[test]
    fn rseq_words_follow_size_and_registration() {
        let line = |p: &Probe| -> std::string::String {
            let v: std::vec::Vec<std::string::String> = p.words().map(|(k, v)| std::format!("{k}={v}")).collect();
            v.join(" ")
        };
        for off in [192isize, -192, -256] {
            assert_eq!(line(&registered(33, off)), "rseq=yes rseq_size=33");
        }
        assert_eq!(line(&registered(20, 192)), "rseq=yes rseq_size=20");
        assert_eq!(line(&Probe::NONE), "rseq=no rseq_size=0");
        assert_eq!(line(&registered(0, 0)), "rseq=no rseq_size=0", "symbols present, nothing registered");
        // The kernel did not write the area: no registration, whatever the size says.
        let p = Probe { registered: false, ..registered(33, -192) };
        assert_eq!(line(&p), "rseq=no rseq_size=33");
        // Unknown flags do not change what the line says exists.
        let p = Probe { flags: 1, ..registered(33, 192) };
        assert_eq!(line(&p), "rseq=yes rseq_size=33");
        // Below the original struct nothing is registered.
        assert_eq!(line(&registered(19, 192)), "rseq=no rseq_size=19");
        assert_eq!(BASE_SIZE, 20);
    }

    /// The both-ways probe test: what glibc registered on this host is
    /// printed, and where it registered an area, `cpu_id` reads through
    /// the probed offset (and is left unread where it did not).
    #[test]
    fn probe_reads_the_registered_area() {
        let p = probe();
        std::eprintln!(
            "rseq: present={} size={} flags={} offset={} registered={}",
            p.present,
            p.size,
            p.flags,
            p.offset,
            p.registered
        );
        assert!(p.present, "glibc 2.44 exports the symbols");
        if p.size == 0 {
            assert!(!p.registered);
            return;
        }
        assert!(p.registered, "cpu_id is written at registration");
        // SAFETY: this thread's registered area (`registered`), which
        // holds `cpu_id` from `BASE_SIZE` on.
        let cpu = unsafe { cpu_id(area(p.offset)) };
        assert_ne!(cpu, CPU_ID_UNINITIALIZED);
        pinned_threads_read_their_cpu(p);
    }

    /// The calling thread's allowed CPUs (`sched_getaffinity`: the cpuset
    /// or container mask, not the machine's), lowest first.
    fn allowed_cpus() -> Option<std::vec::Vec<usize>> {
        // SAFETY: a zeroed `cpu_set_t` is a valid empty set; the call
        // writes at most `size_of::<cpu_set_t>()` bytes into it.
        let mut set: ::libc::cpu_set_t = unsafe { core::mem::zeroed() };
        let rc = unsafe { ::libc::sched_getaffinity(0, core::mem::size_of::<::libc::cpu_set_t>(), &mut set) };
        if rc != 0 {
            return None;
        }
        // SAFETY: `CPU_ISSET` reads one bit of the set for `c` below
        // `CPU_SETSIZE`.
        Some((0..::libc::CPU_SETSIZE as usize).filter(|&c| unsafe { ::libc::CPU_ISSET(c, &set) }).collect())
    }

    /// Pin the calling thread to `cpu` alone; `false` when the kernel
    /// refuses (the CPU left the allowed set meanwhile).
    fn pin_to(cpu: usize) -> bool {
        // SAFETY: as above; `CPU_SET` writes one bit of the set (`cpu` came
        // from `allowed_cpus`, so it is below `CPU_SETSIZE`), and
        // `sched_setaffinity` only reads it.
        let mut set: ::libc::cpu_set_t = unsafe { core::mem::zeroed() };
        unsafe { ::libc::CPU_SET(cpu, &mut set) };
        unsafe { ::libc::sched_setaffinity(0, core::mem::size_of::<::libc::cpu_set_t>(), &set) == 0 }
    }

    /// Threads pinned to distinct allowed CPUs, at most this many.
    const MAX_PINNED: usize = 64;
    /// Loads of `cpu_id` per pinned thread.
    const PIN_READS: u32 = 10_000;

    /// The live half of the probe test: one thread pinned per allowed CPU
    /// with `sched_setaffinity` reads its CPU in `cpu_id` on every load
    /// through the probed offset, so the area [`area`] computes is the
    /// registered one. Where the check cannot be made (one CPU allowed, a
    /// pin refused) the test says so and skips it, never passes it.
    fn pinned_threads_read_their_cpu(p: Probe) {
        let Some(allowed) = allowed_cpus() else {
            std::eprintln!("rseq: skip the pinned check: sched_getaffinity failed");
            return;
        };
        if allowed.len() < 2 {
            std::eprintln!("rseq: skip the pinned check: {} CPU allowed ({:?})", allowed.len(), allowed);
            return;
        }
        let cpus: std::vec::Vec<usize> = allowed.iter().copied().take(MAX_PINNED).collect();
        let hs: std::vec::Vec<_> = cpus
            .iter()
            .map(|&cpu| {
                std::thread::spawn(move || {
                    if !pin_to(cpu) {
                        return (cpu, false, 0u32, 0u32);
                    }
                    let a = area(p.offset);
                    let (mut strays, mut first) = (0u32, 0u32);
                    for _ in 0..PIN_READS {
                        // SAFETY: this thread's registered area.
                        let c = unsafe { cpu_id(a) };
                        if c as usize != cpu {
                            if strays == 0 {
                                first = c;
                            }
                            strays += 1;
                        }
                    }
                    (cpu, true, strays, first)
                })
            })
            .collect();
        let rs: std::vec::Vec<(usize, bool, u32, u32)> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        std::eprintln!("rseq: {} threads pinned to CPUs {:?}", rs.len(), cpus);
        if let Some(r) = rs.iter().find(|r| !r.1) {
            std::eprintln!("rseq: skip the pinned check: sched_setaffinity to CPU {} refused", r.0);
            return;
        }
        for &(cpu, _, strays, first) in &rs {
            assert_eq!(
                strays, 0,
                "thread pinned to CPU {cpu} read cpu_id {first} on {strays} of {PIN_READS} loads: the area at offset {} is not the registered one",
                p.offset
            );
        }
    }
}
