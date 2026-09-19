//! rseq time slice extension for lock holders (design.md section 17,
//! "Time slice extension"; plan Phase 2.5 task 15; kernel 7.0,
//! `CONFIG_RSEQ_SLICE_EXTENSION`).
//!
//! A thread that holds one of the allocator's locks (class, table, huge,
//! metadata, statistics: every `lock::RawLock`) and is preempted mid-hold
//! leaves every other thread that wants that lock spinning and then
//! sleeping until it runs again. The extension lets the holder ask the
//! kernel, through two bytes of its own `struct rseq`, to defer a
//! preemption that would land inside the hold by up to a few microseconds
//! (5 us by default, 50 at most, `debugfs rseq/slice_ext_nsec`): the
//! holder stores `slice_ctrl.request = 1` after acquiring and `0` before
//! releasing; a scheduler tick that finds the request set grants it
//! (clears `request`, sets `granted`, arms a timer) and lets the thread
//! return to user space; after the release, a holder that reads
//! `granted == 1` calls the `rseq_slice_yield` syscall, which gives the CPU
//! back at once (any syscall ends a grant; the dedicated one is the cheap
//! way). A grant that is not yielded expires with the timer. The kernel's
//! documentation (`Documentation/userspace-api/rseq.rst`) prescribes
//! exactly this sequence and says user space may write nothing but
//! `request`; this module writes that byte and reads `granted` and the
//! area's `flags`, and never another field.
//!
//! The ABI (`include/uapi/linux/rseq.h`, 7.0): `struct rseq` gains
//! `slice_ctrl` at byte 28 (`request` u8 at 28, `granted` u8 at 29, two
//! reserved bytes), so the registered `__rseq_size` is at least 32 (33 on
//! the 7.0 kernels, `end` after one reserved byte). The kernel advertises
//! the feature per registered area in the area's `flags` field (byte 16):
//! `RSEQ_CS_FLAG_SLICE_EXT_AVAILABLE` (bit 4) once the area is registered
//! with the v2 size and the kernel has the option; `prctl(79,
//! PR_RSEQ_SLICE_EXTENSION_SET = 2, PR_RSEQ_SLICE_EXT_ENABLE = 1)` turns it
//! on for the *calling thread* and sets `RSEQ_CS_FLAG_SLICE_EXT_ENABLED`
//! (bit 5) in that thread's area. The enable is per task, not per
//! process: `rseq_fork` resets the rseq state of a `CLONE_VM` child (glibc
//! registers the new thread's area afresh, without the enable), and
//! copies it to a plain `fork` child (so the child of a fork inherits the
//! forking thread's enable). This module therefore enables lazily, per
//! thread, on the thread's first lock hold: one `flags` load per hold
//! (the same cache line as `slice_ctrl`) says whether this thread is
//! enabled, and a thread that is not yet is enabled with one `prctl`.
//!
//! Everything is probed at run time ([`probe`]): the symbols and the size
//! from `rseq::probe`, bit 4 of this thread's `flags`, the `prctl`, bit 5
//! after it, and the syscall (an `ENOSYS` from `rseq_slice_yield` on a
//! kernel that has the option compiled out, or that predates it, is the
//! fallback's signature; the number, 471 on both architectures, is from
//! the unified table since 7.0). Where any step fails the module is off
//! and a lock hold costs one relaxed load of [`STATE`]. Ubuntu's
//! `7.0.0-1011-gcp` kernel (the GCE reference VMs) does not set the
//! option; arm64 selects neither `GENERIC_ENTRY` nor
//! `HAVE_GENERIC_TIF_BITS` through 7.2 and master, so on aarch64 the probe
//! fails on every kernel to date and this file is the fallback there.

use core::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicU32, AtomicU64, Ordering};

/// Byte offset of `flags` in `struct rseq` (kernel-owned since 6.19's
/// rework; user space reads it).
pub const FLAGS_OFFSET: usize = 16;
/// Byte offset of `slice_ctrl.request` (the one byte user space writes).
pub const REQUEST_OFFSET: usize = 28;
/// Byte offset of `slice_ctrl.granted` (kernel-written, user space reads).
pub const GRANTED_OFFSET: usize = 29;
/// The registered size that covers `slice_ctrl`: the field ends at byte 32.
pub const SLICE_CTRL_END: u32 = 32;
/// `RSEQ_CS_FLAG_SLICE_EXT_AVAILABLE`: the kernel has the option and the
/// area is registered with the v2 size.
pub const FLAG_AVAILABLE: u32 = 1 << 4;
/// `RSEQ_CS_FLAG_SLICE_EXT_ENABLED`: the `prctl` ran for this thread.
pub const FLAG_ENABLED: u32 = 1 << 5;
/// `prctl(2)` option `PR_RSEQ_SLICE_EXTENSION` and its arguments.
pub const PR_RSEQ_SLICE_EXTENSION: i32 = 79;
pub const PR_RSEQ_SLICE_EXTENSION_GET: usize = 1;
pub const PR_RSEQ_SLICE_EXTENSION_SET: usize = 2;
pub const PR_RSEQ_SLICE_EXT_ENABLE: usize = 1;
/// `__NR_rseq_slice_yield` (7.0, the unified table, so the same on x86_64
/// and aarch64; `arch/x86/entry/syscalls/syscall_64.tbl` and
/// `include/uapi/asm-generic/unistd.h` at v7.0 both say 471). Probed with
/// `ENOSYS`, never assumed.
pub const NR_RSEQ_SLICE_YIELD: usize = 471;
/// `__NR_sched_yield`: the grant-ending syscall where a seccomp filter
/// returns `ENOSYS` for the dedicated one (Docker's default profile does
/// for every syscall newer than its table). Any syscall entry inside a
/// granted extension revokes the grant and reschedules; `sched_yield`
/// is the cheapest one that also means "yield".
#[cfg(target_arch = "x86_64")]
pub const NR_SCHED_YIELD: usize = 24;
#[cfg(target_arch = "aarch64")]
pub const NR_SCHED_YIELD: usize = 124;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub const NR_SCHED_YIELD: usize = 0;

/// Process-wide state, one byte read on every lock hold.
const OFF: u8 = 0;
const ON: u8 = 1;

static STATE: AtomicU8 = AtomicU8::new(OFF);
/// `__rseq_offset` (thread pointer to area), valid once `PROBED` is set.
static OFFSET: AtomicIsize = AtomicIsize::new(0);
/// The probe passed at least once, so every thread's area covers
/// `slice_ctrl` and [`OFFSET`] is right. Never cleared: a release after a
/// later switch-off (a failed `prctl` in some thread) still withdraws a
/// request its acquire may have stored.
static PROBED: AtomicBool = AtomicBool::new(false);
/// Why the module is off ([`Verdict`] as `u8`); `Verdict::Enabled` when on.
static VERDICT: AtomicU8 = AtomicU8::new(Verdict::Unprobed as u8);
/// The key was `on` (the report notes a failed probe only then).
static FORCED: AtomicBool = AtomicBool::new(false);
/// Threads enabled through the `prctl` (init's and the lazy ones).
static ENABLED_THREADS: AtomicU32 = AtomicU32::new(0);
/// Grants observed after a release, each followed by one yield syscall:
/// the count of holder preemptions the kernel deferred.
static YIELDS: AtomicU64 = AtomicU64::new(0);
/// The yield syscall in use: `rseq_slice_yield` (false) or, where a
/// filter hides it, `sched_yield` (true).
static YIELD_SCHED: AtomicBool = AtomicBool::new(false);
/// Requests stored (test builds only: a counter per hold is not free).
#[cfg(test)]
static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// The probe's verdict, one word for the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Verdict {
    /// `configure` has not run.
    Unprobed = 0,
    /// `slice_ext:off`.
    Key = 1,
    /// No rseq registration (`__rseq_size == 0`, or no symbols).
    NoRseq = 2,
    /// `__rseq_size < 32`: the kernel does not write `slice_ctrl` (6.18
    /// and earlier).
    NoSliceCtrl = 3,
    /// Bit 4 clear: the kernel has the option compiled out or disabled
    /// (`rseq_slice_ext=0`), or the area was not registered as v2.
    KernelDisabled = 4,
    /// The `prctl` failed or did not set bit 5.
    PrctlFailed = 5,
    /// Unused since the `sched_yield` fallback; kept so the numbering of
    /// the report words is stable.
    NoSyscall = 6,
    /// On.
    Enabled = 7,
}

impl Verdict {
    fn from_u8(v: u8) -> Verdict {
        match v {
            1 => Verdict::Key,
            2 => Verdict::NoRseq,
            3 => Verdict::NoSliceCtrl,
            4 => Verdict::KernelDisabled,
            5 => Verdict::PrctlFailed,
            6 => Verdict::NoSyscall,
            7 => Verdict::Enabled,
            _ => Verdict::Unprobed,
        }
    }

    /// The word in the report and the results files.
    pub const fn name(self) -> &'static str {
        match self {
            Verdict::Unprobed => "unprobed",
            Verdict::Key => "slice_ext:off",
            Verdict::NoRseq => "no-rseq",
            Verdict::NoSliceCtrl => "no-slice_ctrl",
            Verdict::KernelDisabled => "kernel-disabled",
            Verdict::PrctlFailed => "prctl-failed",
            Verdict::NoSyscall => "no-syscall",
            Verdict::Enabled => "enabled",
        }
    }
}

/// What the calling thread's area and the kernel say, before any enable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    /// `__rseq_size` (0 without a registration).
    pub size: u32,
    /// `__rseq_offset`.
    pub offset: isize,
    /// Bit 4 of this thread's `flags` (only read when `size >= 32`).
    pub available: bool,
    /// Bit 5 of this thread's `flags`.
    pub enabled: bool,
    /// `rseq_slice_yield` reaches the kernel (returned 0, not `ENOSYS`:
    /// a kernel without the option, or a seccomp filter in front of it,
    /// says `ENOSYS`); else grants are ended with `sched_yield`.
    pub yield_syscall: bool,
}

impl Probe {
    /// Nothing available (off Linux, or no registration).
    pub const NONE: Probe = Probe { size: 0, offset: 0, available: false, enabled: false, yield_syscall: false };

    /// The verdict [`configure`] would reach without the `prctl`.
    pub fn verdict(&self) -> Verdict {
        if self.size == 0 {
            Verdict::NoRseq
        } else if self.size < SLICE_CTRL_END {
            Verdict::NoSliceCtrl
        } else if !self.available {
            Verdict::KernelDisabled
        } else {
            Verdict::Enabled
        }
    }
}

/// Read the registration, this thread's `flags` bits and the syscall.
/// Writes nothing. Off Linux everything is absent.
pub fn probe() -> Probe {
    #[cfg(target_os = "linux")]
    {
        let r = crate::rseq::probe();
        if !r.present || r.size == 0 {
            return Probe::NONE;
        }
        let (mut available, mut enabled) = (false, false);
        if r.size >= SLICE_CTRL_END {
            // SAFETY: the area is `size >= 32` bytes at the thread pointer
            // plus the offset for every thread, and `flags` is inside it.
            let f = unsafe { flags(crate::rseq::area(r.offset)) };
            available = f & FLAG_AVAILABLE != 0;
            enabled = f & FLAG_ENABLED != 0;
        }
        // The syscall with no grant outstanding returns 0 and does nothing
        // else; `ENOSYS` is the kernel without it.
        let yield_syscall = available && matches!(slice_yield(), Ok(_));
        Probe { size: r.size, offset: r.offset, available, enabled, yield_syscall }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Probe::NONE
    }
}

/// This thread's `flags` field.
///
/// # Safety
/// `area` is this thread's registered area of at least 20 bytes.
#[cfg(target_os = "linux")]
#[inline(always)]
unsafe fn flags(area: *mut u8) -> u32 {
    // SAFETY: guaranteed by the caller.
    unsafe { area.add(FLAGS_OFFSET).cast::<u32>().read_volatile() }
}

/// Apply the configuration (`slice_ext:auto|on|off`, design 12) on the
/// init thread: probe, enable this thread, publish the state. Returns the
/// verdict; `Ok` when on. `forced` is the key's `on`: it changes nothing
/// about what runs (a failed probe cannot be forced) and makes the report
/// say the request was refused.
pub fn configure(enabled_by_key: bool, forced: bool) -> Result<(), Verdict> {
    FORCED.store(forced, Ordering::Relaxed);
    let verdict = if !enabled_by_key { Verdict::Key } else { try_enable() };
    VERDICT.store(verdict as u8, Ordering::Relaxed);
    if verdict == Verdict::Enabled {
        STATE.store(ON, Ordering::Release);
        Ok(())
    } else {
        STATE.store(OFF, Ordering::Release);
        Err(verdict)
    }
}

/// The probe and this thread's enable.
fn try_enable() -> Verdict {
    let p = probe();
    let v = p.verdict();
    if v != Verdict::Enabled {
        return v;
    }
    OFFSET.store(p.offset, Ordering::Relaxed);
    YIELD_SCHED.store(!p.yield_syscall, Ordering::Relaxed);
    PROBED.store(true, Ordering::Release);
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `size >= 32`, so the area covers `flags` and `slice_ctrl`.
        if p.enabled || unsafe { enable_thread(crate::rseq::area(p.offset)) } {
            Verdict::Enabled
        } else {
            Verdict::PrctlFailed
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Verdict::PrctlFailed
    }
}

/// `prctl(PR_RSEQ_SLICE_EXTENSION, SET, ENABLE)` for the calling thread,
/// confirmed by bit 5 of its area. Counts the thread on success.
///
/// # Safety
/// `area` is this thread's registered area of at least 32 bytes.
#[cfg(target_os = "linux")]
unsafe fn enable_thread(area: *mut u8) -> bool {
    // SAFETY: a `prctl` with integer arguments; option-specific contract
    // is "the calling thread has an rseq area registered with the v2
    // size", which the probe established.
    let ok = unsafe {
        crate::sys::prctl(PR_RSEQ_SLICE_EXTENSION, PR_RSEQ_SLICE_EXTENSION_SET, PR_RSEQ_SLICE_EXT_ENABLE, 0, 0)
    }
    .is_ok();
    // SAFETY: guaranteed by the caller.
    let set = ok && unsafe { flags(area) } & FLAG_ENABLED != 0;
    if set {
        ENABLED_THREADS.fetch_add(1, Ordering::Relaxed);
    }
    set
}

/// `prctl(PR_RSEQ_SLICE_EXTENSION, GET)`: whether the calling thread is
/// enabled (`Ok(1)`), not (`Ok(0)`), or the kernel has no such option
/// (`Err(ENOTSUPP)`, `Err(EINVAL)`). Diagnostics and tests.
#[cfg(target_os = "linux")]
pub fn thread_enabled() -> Result<usize, crate::sys::Errno> {
    // SAFETY: a query with integer arguments and no pointer.
    unsafe { crate::sys::prctl(PR_RSEQ_SLICE_EXTENSION, PR_RSEQ_SLICE_EXTENSION_GET, 0, 0, 0) }
}

/// `rseq_slice_yield(2)`: end a granted extension now. `Ok(1)` when the
/// kernel had marked the thread as yielded, `Ok(0)` otherwise,
/// `Err(ENOSYS)` on a kernel without it.
#[cfg(target_os = "linux")]
#[inline]
pub fn slice_yield() -> Result<usize, crate::sys::Errno> {
    let ret: usize;
    // SAFETY: a syscall with no arguments; it reads and writes nothing of
    // ours. The same register contract as `sys::raw::syscall0`, which is
    // private to `sys` (this module keeps its one syscall to itself).
    unsafe {
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!("syscall", inlateout("rax") NR_RSEQ_SLICE_YIELD => ret,
             lateout("rcx") _, lateout("r11") _, options(nostack));
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("svc 0", in("x8") NR_RSEQ_SLICE_YIELD, lateout("x0") ret, options(nostack));
    }
    crate::sys::decode(ret)
}

/// Whether the extension is on for this process.
#[inline]
pub fn on() -> bool {
    STATE.load(Ordering::Relaxed) == ON
}

/// The verdict behind [`on`].
pub fn verdict() -> Verdict {
    Verdict::from_u8(VERDICT.load(Ordering::Relaxed))
}

/// The key was `on` and the probe failed: worth a note in the report.
pub fn forced_off() -> bool {
    FORCED.load(Ordering::Relaxed) && !on()
}

/// Threads enabled so far.
pub fn enabled_threads() -> u32 {
    ENABLED_THREADS.load(Ordering::Relaxed)
}

/// Grants seen after a release (each yielded).
pub fn yields() -> u64 {
    YIELDS.load(Ordering::Relaxed)
}

/// The syscall a grant is ended with, for the report.
pub fn yield_name() -> &'static str {
    if YIELD_SCHED.load(Ordering::Relaxed) { "sched_yield" } else { "rseq_slice_yield" }
}

/// Requests stored (test builds).
#[cfg(test)]
pub fn requests() -> u64 {
    REQUESTS.load(Ordering::Relaxed)
}

/// The lock was acquired by the calling thread: ask the kernel to hold
/// off a preemption until the release. One load of [`STATE`]; when on,
/// one `flags` load and one byte store into this thread's area, plus the
/// thread's one-time `prctl`. Returns the area for [`released`] (null when
/// nothing was requested).
#[inline]
pub fn acquired() -> *mut u8 {
    if !on() {
        return core::ptr::null_mut();
    }
    #[cfg(target_os = "linux")]
    {
        let area = crate::rseq::area(OFFSET.load(Ordering::Relaxed));
        // SAFETY: `STATE == ON` means the probe found this process's areas
        // registered with `size >= 32`; every thread's area has the same
        // layout at the same offset from its thread pointer.
        let f = unsafe { flags(area) };
        if f & FLAG_ENABLED == 0 {
            // A thread created after init starts without the enable
            // (`rseq_fork` resets a `CLONE_VM` child's rseq state).
            // SAFETY: as above.
            if f & FLAG_AVAILABLE == 0 || !unsafe { enable_thread(area) } {
                // Should not happen once the probe passed (bit 4 is per
                // registration, the `prctl` fails only on a kernel that
                // lied); switch the process off rather than retry a
                // syscall per hold.
                VERDICT.store(Verdict::PrctlFailed as u8, Ordering::Relaxed);
                STATE.store(OFF, Ordering::Release);
                return core::ptr::null_mut();
            }
        }
        // SAFETY: `request` is the one byte user space may write, and it is
        // this thread's own.
        unsafe { area.add(REQUEST_OFFSET).write_volatile(1) };
        #[cfg(test)]
        REQUESTS.fetch_add(1, Ordering::Relaxed);
        area
    }
    #[cfg(not(target_os = "linux"))]
    {
        core::ptr::null_mut()
    }
}

/// The area [`acquired`] requested through, for a release that did not
/// keep the pointer (`RawLock` has no per-holder field): null until the
/// probe has passed. Keyed on [`PROBED`] rather than [`on`] so a hold
/// that requested before another thread switched the process off still
/// withdraws its request. A thread that could not be enabled has no
/// request outstanding; clearing its own `request` byte and reading
/// `granted` (0) is harmless.
#[inline]
pub fn acquired_area() -> *mut u8 {
    if !PROBED.load(Ordering::Acquire) {
        return core::ptr::null_mut();
    }
    #[cfg(target_os = "linux")]
    {
        crate::rseq::area(OFFSET.load(Ordering::Relaxed))
    }
    #[cfg(not(target_os = "linux"))]
    {
        core::ptr::null_mut()
    }
}

/// The holder is about to release: withdraw the request. `area` is what
/// [`acquired`] returned.
#[inline]
pub fn releasing(area: *mut u8) {
    if area.is_null() {
        return;
    }
    // SAFETY: `area` came from `acquired` on this thread.
    unsafe { area.add(REQUEST_OFFSET).write_volatile(0) };
    // Keep the store before the lock word's release store, whatever the
    // compiler thinks of two unrelated stores.
    core::sync::atomic::compiler_fence(Ordering::Release);
}

/// The lock is released: if the kernel granted an extension during the
/// hold, give the CPU back now. A syscall the release itself made
/// (`FUTEX_WAKE`) already ended the grant and zeroed `granted`, so this
/// reads 0 then and nothing more happens.
#[inline]
pub fn released(area: *mut u8) {
    if area.is_null() {
        return;
    }
    core::sync::atomic::compiler_fence(Ordering::Acquire);
    // SAFETY: `area` came from `acquired` on this thread; `granted` is a
    // byte the kernel writes and we only read.
    if unsafe { area.add(GRANTED_OFFSET).read_volatile() } != 0 {
        yield_now();
    }
}

#[cold]
#[inline(never)]
fn yield_now() {
    YIELDS.fetch_add(1, Ordering::Relaxed);
    #[cfg(target_os = "linux")]
    if YIELD_SCHED.load(Ordering::Relaxed) {
        let _ = sched_yield();
    } else {
        let _ = slice_yield();
    }
}

/// `sched_yield(2)`, the grant-ending syscall where `rseq_slice_yield` is
/// filtered.
#[cfg(target_os = "linux")]
#[inline]
pub fn sched_yield() -> Result<usize, crate::sys::Errno> {
    let ret: usize;
    // SAFETY: a syscall with no arguments and no memory effect of ours.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!("syscall", inlateout("rax") NR_SCHED_YIELD => ret,
             lateout("rcx") _, lateout("r11") _, options(nostack));
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("svc 0", in("x8") NR_SCHED_YIELD, lateout("x0") ret, options(nostack));
    }
    crate::sys::decode(ret)
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) mod tests {
    use super::*;
    use crate::lock::RawLock;

    /// Serialises the tests that reconfigure the process-wide state.
    pub(crate) static SLICE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn hold_once(l: &RawLock) {
        l.lock();
        l.unlock();
    }

    /// Both ways: on a kernel with the option (bit 4 set, `prctl` and the
    /// syscall present) `configure(auto)` turns the module on, the init
    /// thread is enabled, a hold stores and clears the request and the
    /// counter moves; on a kernel without it (Ubuntu's 7.0 GCE kernels,
    /// the Mac's 6.10, every arm64 kernel to date) the verdict names the
    /// missing piece, nothing is stored, and a hold leaves the two
    /// `slice_ctrl` bytes untouched. `off` is off whatever the kernel.
    #[test]
    fn probe_both_ways_and_off_never_requests() {
        let _serial = SLICE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = (verdict(), on());
        let p = probe();
        std::eprintln!(
            "slice_ext: rseq size {} offset {} available={} enabled={} yield_syscall={} verdict={}",
            p.size,
            p.offset,
            p.available,
            p.enabled,
            p.yield_syscall,
            p.verdict().name()
        );
        assert_eq!(p.available && p.size < SLICE_CTRL_END, false, "bit 4 needs the v2 size");

        // `off`: never a request, whatever the kernel.
        assert_eq!(configure(false, false), Err(Verdict::Key));
        assert!(!on());
        assert_eq!(verdict(), Verdict::Key);
        let l = RawLock::new();
        let byte = |off: usize| -> u8 {
            if p.size >= SLICE_CTRL_END {
                // SAFETY: this thread's area covers `slice_ctrl`.
                unsafe { crate::rseq::area(p.offset).add(off).read_volatile() }
            } else {
                0
            }
        };
        for _ in 0..100 {
            l.lock();
            assert_eq!(byte(REQUEST_OFFSET), 0, "slice_ext:off stored a request");
            l.unlock();
        }
        assert!(!forced_off(), "`off` is not a refused request");

        // `auto` / `on`: follows the probe.
        let res = configure(true, true);
        match p.verdict() {
            Verdict::Enabled => {
                assert_eq!(res, Ok(()), "the probe passed but configure refused");
                assert!(on());
                assert_eq!(verdict(), Verdict::Enabled);
                assert_eq!(thread_enabled(), Ok(1), "the init thread is enabled");
                assert!(!forced_off());
                std::eprintln!("slice_ext: grants end with {}", yield_name());
                assert_eq!(sched_yield(), Ok(0));
                assert_eq!(yield_name() == "rseq_slice_yield", p.yield_syscall);
                // The request is set exactly while the lock is held (the
                // process-wide counter also moves, but other tests' holds
                // share it, so it is only a lower bound).
                let r0 = requests();
                for _ in 0..100 {
                    l.lock();
                    assert_eq!(byte(REQUEST_OFFSET), 1, "no request during the hold");
                    l.unlock();
                    assert_eq!(byte(REQUEST_OFFSET), 0, "request left set");
                }
                assert!(requests() >= r0 + 100);
                // A thread created now starts unenabled and is enabled by
                // its first hold (the per-task enable, confirmed).
                std::thread::scope(|s| {
                    s.spawn(|| {
                        assert_eq!(thread_enabled(), Ok(0), "a new thread inherits no enable");
                        let t0 = enabled_threads();
                        hold_once(&l);
                        assert_eq!(thread_enabled(), Ok(1), "the first hold enabled the thread");
                        assert!(enabled_threads() > t0);
                        hold_once(&l);
                        assert_eq!(thread_enabled(), Ok(1));
                    });
                });
            }
            v => {
                assert_eq!(res, Err(v));
                assert!(!on());
                assert_eq!(verdict(), v);
                assert!(forced_off(), "`on` refused is reported");
                assert_eq!(acquired(), core::ptr::null_mut());
                for _ in 0..100 {
                    l.lock();
                    assert_eq!(byte(REQUEST_OFFSET), 0, "a failed probe stored a request");
                    l.unlock();
                }
                assert_eq!((byte(REQUEST_OFFSET), byte(GRANTED_OFFSET)), (0, 0), "slice_ctrl touched on the fallback");
                if v == Verdict::KernelDisabled || v == Verdict::NoSliceCtrl {
                    assert_eq!(slice_yield(), Err(crate::sys::Errno::ENOSYS), "the syscall exists but bit 4 is clear");
                    assert!(thread_enabled().is_err(), "prctl GET on a kernel without the option");
                }
            }
        }
        // Back to what init chose, for the rest of the suite.
        let _ = configure(before.0 != Verdict::Key, false);
        assert_eq!(on(), before.1);
    }

    /// The granted path: with a CPU hog per CPU keeping every run queue
    /// busy (waiters asleep in the futex are not competition: a holder
    /// alone on its CPU is never preempted) and holds long enough for a
    /// tick to land inside one, the scheduler wants to preempt holders,
    /// grants the extension instead, and every grant is followed by one
    /// yield syscall (the counter). On a kernel without the option the
    /// counter stays where it was.
    #[test]
    fn grants_are_yielded_under_oversubscription() {
        let _serial = SLICE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = (verdict(), on());
        let res = configure(true, false);
        let y0 = yields();
        let cpus = crate::sys::ncpus().max(1) as usize;
        let threads = 4 * cpus;
        let l = RawLock::new();
        let stop = AtomicBool::new(false);
        let holds = AtomicU64::new(0);
        std::thread::scope(|s| {
            for _ in 0..cpus {
                s.spawn(|| {
                    let mut x = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                        core::hint::black_box(x);
                    }
                });
            }
            for _ in 0..threads {
                s.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        l.lock();
                        // About 20 us of work under the lock.
                        let mut x = 0u64;
                        for i in 0..20_000u64 {
                            x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
                        }
                        core::hint::black_box(x);
                        l.unlock();
                        holds.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
            stop.store(true, Ordering::Relaxed);
        });
        let y = yields() - y0;
        std::eprintln!(
            "slice_ext: {} lock threads and {cpus} hogs on {} CPUs, {} holds, {} grants yielded via {}, threads enabled {}, state {}",
            threads,
            crate::sys::ncpus(),
            holds.load(Ordering::Relaxed),
            y,
            yield_name(),
            enabled_threads(),
            verdict().name()
        );
        if res.is_ok() {
            assert!(y > 0, "no grant in 3 s of oversubscribed holds");
        } else {
            assert_eq!(y, 0, "a grant without the extension");
        }
        let _ = configure(before.0 != Verdict::Key, false);
        assert_eq!(on(), before.1);
    }

    /// The constants against the kernel's layout: `slice_ctrl` follows
    /// `mm_cid`, the size gate is its end, the flag bits are 4 and 5.
    #[test]
    fn layout_constants() {
        assert_eq!(REQUEST_OFFSET, crate::rseq::MM_CID_OFFSET + 4);
        assert_eq!(GRANTED_OFFSET, REQUEST_OFFSET + 1);
        assert_eq!(SLICE_CTRL_END, REQUEST_OFFSET as u32 + 4);
        assert_eq!(FLAG_AVAILABLE, 16);
        assert_eq!(FLAG_ENABLED, 32);
        assert_eq!(FLAGS_OFFSET, 16);
        for v in 0..=8u8 {
            let d = Verdict::from_u8(v);
            if v <= 7 {
                assert_eq!(d as u8, v);
            } else {
                assert_eq!(d, Verdict::Unprobed);
            }
            assert!(!d.name().is_empty());
        }
    }
}
