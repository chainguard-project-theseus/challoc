//! Restartable sequences: the per-CPU mode's kernel interface (design.md
//! section 15).
//!
//! glibc (2.35+) registers one `struct rseq` per thread with the kernel
//! and exports three symbols from the dynamic loader: `__rseq_offset`, the
//! signed distance from the thread pointer to that thread's area (it moves
//! with the static TLS layout, so it is read from the symbol, never
//! assumed: 192 on the aarch64 reference VM, -192 bare and -256 under a
//! preload on the x86_64 one), `__rseq_size`, the number of bytes of the
//! area the kernel writes (the auxiliary vector's `AT_RSEQ_FEATURE_SIZE`
//! rounded up; 20 on a kernel without the extended fields, 28 or more once
//! `mm_cid` exists), and `__rseq_flags`, the registration flags (0). All
//! three are weak references here: on a libc without them they resolve to
//! null and the mode stays off. Nothing in this module is ever assumed from
//! a header constant; [`probe`] reads the symbols and [`Probe::mm_cid_usable`]
//! is the one gate the allocator acts on.
//!
//! The area (`struct rseq`, kernel `include/uapi/linux/rseq.h`), offsets in
//! bytes: `cpu_id_start` 0, `cpu_id` 4, `rseq_cs` 8 (u64), `flags` 16,
//! `node_id` 20, `mm_cid` 24 (u32). `rseq_cs` is the one field user space
//! writes: the address of a critical-section descriptor (`struct rseq_cs`:
//! `version`, `flags`, `start_ip`, `post_commit_offset`, `abort_ip`). Every
//! other field is the kernel's, and this module never stores to any of
//! them: writing `cpu_id`, `mm_cid` or `flags` is what broke tcmalloc on
//! Linux 6.19 (design 15), and the kernel now kills a process whose
//! descriptor sets the deprecated `RSEQ_CS_FLAG_NO_RESTART_*` flags.
//!
//! `mm_cid` is the *concurrency id* (kernel 6.3, `CONFIG_SCHED_MM_CID`): a
//! small integer, unique among the threads of the process that are running
//! at any instant and bounded by the number of possible CPUs
//! (`nr_cpu_ids`), so a 200-thread process on 4 CPUs uses ids 0..3. That
//! bound is what makes a cache per concurrency id cost memory by cores
//! rather than by threads. Two kernel implementations exist and the code
//! here relies only on what they share (docs/research.md, the 6.19 row):
//! before 6.19 (Chainguard OS 6.18) ids are `< min(threads, cpus)`, a
//! thread gives its id up when it is switched out and may get another on
//! migration, and the kernel hands ids over through fully ordered atomics
//! on the process's id mask (so what the previous holder of an id stored
//! is visible to the next); since 6.19 (the reference VMs' 7.0) a process
//! with no more threads than possible CPUs runs in *per-task* mode, where
//! every thread keeps its own id across migrations (the caches then behave
//! like thread caches and no header is ever shared), and switches to the
//! shared per-CPU ids only above that count, at fork or exit. In both, an
//! id has one running holder at any instant and stays below `nr_cpu_ids`,
//! which is all the sections need. [`aborts`] counts the sections the
//! kernel restarted, so a test can see the abort path taken (it is, once
//! threads outnumber CPUs).
//!
//! A *critical section* is a straight run of instructions `[start_ip,
//! start_ip + post_commit_offset)` ending in one store, the *commit*. If
//! the thread is preempted, migrated or takes a signal while its program
//! counter is inside that range, the kernel moves it to `abort_ip` when it
//! resumes (after checking that the four bytes before `abort_ip` are the
//! architecture's signature, `0x53053053` on x86_64 spelled as a `ud1`
//! instruction, `0xd428bc00` on aarch64), and the section runs again from
//! the top, re-reading `mm_cid`. Loads and register work inside the section
//! are free; a store before the commit is allowed only if being aborted
//! after it leaves the data structure consistent (writing a slot into the
//! dead part of a stack above its length, or shrinking a lower bound). The
//! kernel clears `rseq_cs` itself once it sees the thread outside the
//! section, so a completed section costs one store, of the descriptor's
//! address, and no clearing.
//!
//! The sections here operate on `heap::CacheHeader` stacks indexed by the
//! running thread's `mm_cid`: [`pop`], [`push`], [`push_batch`] (a refill
//! or a flush's kept half), [`take_all`] (a flush or a drain) and
//! [`install`] (a header's first use). Each reads `mm_cid` inside the
//! section, so the header it works on is the running CPU's by
//! construction, and each is written entirely in `asm!` on both
//! architectures, the descriptor emitted into the `__rseq_cs` section next
//! to it; the Rust wrappers only decode the status word. The batch
//! sections copy their pointers in a loop inside the section: a long
//! section is simply more likely to be aborted and retried, never
//! incorrect. With pointer authentication on (`sys::pac`) the sections
//! sign a stored entry with `pacda` and authenticate a loaded one with
//! `autda`, the stack's base as the modifier, exactly as the thread caches
//! do; the branch on the `pac` flag is inside the section and the
//! instruction is never executed on a CPU without the extension.

#[cfg(target_os = "linux")]
use core::arch::asm;

/// Byte offset of `rseq_cs` in `struct rseq`: the descriptor pointer, the
/// one field this module writes.
pub const RSEQ_CS_OFFSET: usize = 8;
/// Byte offset of `cpu_id` (kernel-owned; `-1` before registration).
pub const CPU_ID_OFFSET: usize = 4;
/// Byte offset of `mm_cid` (kernel-owned, u32).
pub const MM_CID_OFFSET: usize = 24;
/// The registered size that covers `mm_cid`: the field ends at byte 28.
pub const MM_CID_END: u32 = 28;
/// `RSEQ_CPU_ID_UNINITIALIZED`.
pub const CPU_ID_UNINITIALIZED: u32 = u32::MAX;
/// The abort signature the kernel checks before `abort_ip` (x86_64: the
/// operand of a `ud1 <sig>(%rip), %edi`; aarch64: a `brk` encoding).
#[cfg(target_arch = "x86_64")]
pub const SIG: u32 = 0x5305_3053;
#[cfg(target_arch = "aarch64")]
pub const SIG: u32 = 0xd428_bc00;

/// Sections the kernel moved to their abort handler (preempted, migrated
/// or signalled inside one), summed over the process: a diagnostic
/// (`stats.percpu.aborts`, the report) and the tests' witness that the
/// abort path runs. Incremented in the abort handlers themselves, so a
/// completed section costs nothing for it; the aarch64 increment is a
/// plain load-add-store (a lost count under a race is acceptable for a
/// counter).
#[cfg(target_os = "linux")]
static ABORTS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Sections aborted and retried so far (0 off Linux).
pub fn aborts() -> u64 {
    #[cfg(target_os = "linux")]
    {
        ABORTS.load(core::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// What glibc registered, read from the three weak symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The symbols exist (glibc 2.35+, or any libc exporting them).
    pub present: bool,
    /// `__rseq_size`: 0 when registration is off (`glibc.pthread.rseq=0`,
    /// or a kernel without `CONFIG_RSEQ`), 20 on kernels before the
    /// extended fields, 28 or more with `mm_cid`.
    pub size: u32,
    /// `__rseq_flags` (0; anything else is unknown and disables the mode).
    pub flags: u32,
    /// `__rseq_offset`: thread pointer to area, signed.
    pub offset: isize,
    /// The calling thread's `cpu_id` field is not `CPU_ID_UNINITIALIZED`:
    /// the kernel has written this thread's area at least once.
    pub registered: bool,
}

impl Probe {
    /// Nothing registered (the fallback everywhere the symbols are absent).
    pub const NONE: Probe = Probe { present: false, size: 0, flags: 0, offset: 0, registered: false };

    /// The one gate the allocator acts on (`config::PercpuArena::enabled`):
    /// the area is registered with the kernel, covers `mm_cid`, and the
    /// registration carries no flags this module does not know.
    #[inline]
    pub fn mm_cid_usable(&self) -> bool {
        self.present && self.registered && self.size >= MM_CID_END && self.flags == 0
    }

    /// One word for the report: why the mode is off, or `usable`.
    pub fn verdict(&self) -> &'static str {
        if !self.present {
            "no-symbols"
        } else if self.size == 0 {
            "unregistered"
        } else if !self.registered {
            "thread-unregistered"
        } else if self.size < MM_CID_END {
            "no-mm_cid"
        } else if self.flags != 0 {
            "flags"
        } else {
            "usable"
        }
    }
}

/// Read the three weak symbols and the calling thread's `cpu_id` field.
/// Off Linux everything is absent.
pub fn probe() -> Probe {
    #[cfg(target_os = "linux")]
    {
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
    #[cfg(not(target_os = "linux"))]
    {
        Probe::NONE
    }
}

/// Shorthand for [`Probe::mm_cid_usable`] on a fresh probe.
pub fn mm_cid_usable() -> bool {
    probe().mm_cid_usable()
}

/// Addresses of the weak `__rseq_offset`, `__rseq_size` and `__rseq_flags`
/// (null when the libc does not export them). The same `.weak` + GOT form
/// as `config.rs` uses for `malloc_conf`: stable Rust has no weak statics.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
#[inline(always)]
pub fn area(offset: isize) -> *mut u8 {
    thread_pointer().wrapping_add_signed(offset) as *mut u8
}

/// The calling thread's `mm_cid` as the kernel last wrote it. Only
/// meaningful under [`Probe::mm_cid_usable`]; a hint outside a critical
/// section (the thread may migrate right after the load), which is how the
/// slow paths use it.
///
/// # Safety
/// `area` is this thread's registered area of at least [`MM_CID_END`] bytes.
#[inline(always)]
pub unsafe fn mm_cid(area: *mut u8) -> u32 {
    // SAFETY: guaranteed by the caller.
    unsafe { area.add(MM_CID_OFFSET).cast::<u32>().read_volatile() }
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

/// Status words of [`pop_raw`] and [`push_raw`], the sections' raw
/// results the entry points decode with one test each: a pop hit has
/// [`POP_HIT`] set (and [`POP_ZERO`] when the slot is known zero); a push
/// hit is [`PUSH_PUSHED`], zero. The other values are the misses the cold paths
/// decode; [`POP_OFF_MODE`] / [`PUSH_OFF_MODE`] are the heap's own
/// (`heap.rs`: a heap off the mode), never the sections'.
pub const POP_HIT: u32 = 1;
pub const POP_ZERO: u32 = 2;
pub const POP_EMPTY: u32 = 0;
pub const POP_OUT_OF_RANGE: u32 = 2;
pub const POP_SCHEDULED: u32 = 4;
pub const POP_OFF_MODE: u32 = 8;
pub const PUSH_PUSHED: u32 = 0;
pub const PUSH_FULL: u32 = 1;
pub const PUSH_OUT_OF_RANGE: u32 = 2;
/// The heap declined the push before the section (a pointer the checks
/// did not verify, `zero_on_free`).
pub const PUSH_DECLINED: u32 = 3;
pub const PUSH_OFF_MODE: u32 = 8;

/// Outcome of [`pop`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pop {
    /// The top entry, plain, and whether it is known to read as zero
    /// (`len < zeroed` after the pop).
    Popped(*mut u8, bool),
    /// The header's stack is empty (or the header was never installed):
    /// the slow path refills.
    Empty,
    /// This pop's count hit the compaction schedule (`allocs & compact_mask
    /// == 0`, `heap.rs`): nothing was popped, `allocs` was counted, the
    /// slow path serves the scheduled allocation.
    Scheduled,
    /// `mm_cid >= n`: the table is smaller than the kernel's id space,
    /// which the sizing rules out; the caller aborts.
    OutOfRange,
}

impl Pop {
    /// [`pop_raw`]'s `(p, status)` as the enum.
    #[inline(always)]
    pub fn decode(p: *mut u8, st: u32) -> Pop {
        if st & POP_HIT != 0 {
            return Pop::Popped(p, st & POP_ZERO != 0);
        }
        match st {
            POP_EMPTY => Pop::Empty,
            POP_OUT_OF_RANGE => Pop::OutOfRange,
            _ => Pop::Scheduled,
        }
    }
}

/// Outcome of [`push`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
    Pushed,
    /// `len == cap` (also a never-installed header, `cap == 0`).
    Full,
    OutOfRange,
}

impl Push {
    /// [`push_raw`]'s status as the enum.
    #[inline(always)]
    pub fn decode(st: u32) -> Push {
        match st {
            PUSH_PUSHED => Push::Pushed,
            PUSH_FULL => Push::Full,
            _ => Push::OutOfRange,
        }
    }
}

/// The table geometry every section takes: `base` is the address of the
/// running CPU's class header when `mm_cid` is 0, i.e. the table base plus
/// the class offset; the header for id `c` is `base + c * STRIDE`; ids at
/// or above `n` are out of range.
#[derive(Clone, Copy, Debug)]
pub struct Table {
    pub base: *mut u8,
    pub n: u32,
}

/// Distance between one id's headers and the next's: one
/// `heap::CpuCache`.
pub const STRIDE: usize = crate::heap::CPU_CACHE_SIZE;

// The header field offsets the sections use (`heap::CacheHeader`); only
// the Linux sections read them, hence the cfg.
#[cfg(target_os = "linux")]
mod hdr {
    pub const O_LEN: usize = crate::heap::OFF_LEN;
    pub const O_CAP: usize = crate::heap::OFF_CAP;
    pub const O_LOW_WATER: usize = crate::heap::OFF_LOW_WATER;
    pub const O_ZEROED: usize = crate::heap::OFF_ZEROED;
    pub const O_SLOTS: usize = crate::heap::OFF_SLOTS;
    pub const O_ALLOCS: usize = crate::heap::OFF_ALLOCS;
    pub const O_MASK: usize = crate::heap::OFF_COMPACT_MASK;
}
#[cfg(target_os = "linux")]
use hdr::*;
const _: () = assert!(STRIDE < i32::MAX as usize && STRIDE % 64 == 0);
const _: () = assert!(
    crate::heap::OFF_LEN < 4096
        && crate::heap::OFF_CAP < 4096
        && crate::heap::OFF_SLOTS < 4096
        && crate::heap::OFF_ALLOCS < 4096
);

// Section label plan, shared by every section below (numeric labels made
// of the digits 0 and 1 alone are avoided, as the `asm!` documentation
// asks): 9 the descriptor, 8 the `rseq_cs` store the abort handler jumps
// back to, 2 `start_ip`, 3 `post_commit_ip`, 4 `abort_ip` (preceded by the
// signature), 5..7 and 12.. the status exits.

/// Pop the top entry of the running CPU's header for the class `t.base`
/// names, counting the pop in `allocs` and clipping `low_water`.
///
/// # Safety
/// `area` is this thread's registered rseq area with `mm_cid` usable;
/// `t` describes a live table whose headers, once installed, have stacks
/// of `cap` slots; `pac` is the process's `PUSH_PAC` (entries are stored
/// signed with the stack base when it is set).
#[cfg(target_os = "linux")]
#[inline(always)]
pub unsafe fn pop(area: *mut u8, t: Table, pac: bool) -> Pop {
    // SAFETY: guaranteed by the caller.
    let (p, st) = unsafe { pop_raw(area, t, pac) };
    Pop::decode(p, st)
}

/// [`pop`] returning the section's raw status word (`POP_*`) beside the
/// pointer: what the entry points inline, so a hit is one bit test.
///
/// # Safety
/// As for [`pop`].
#[cfg(target_os = "linux")]
#[inline(always)]
pub unsafe fn pop_raw(area: *mut u8, t: Table, pac: bool) -> (*mut u8, u32) {
    let p: *mut u8;
    // x86_64 has no pointer authentication; the flag is for aarch64.
    #[cfg(target_arch = "x86_64")]
    let _ = pac;
    // The status word, decoded below: bit 0 set is a pop (bit 1 then says
    // the slot is known zero, `len < zeroed` after the pop, compared inside
    // the asm so the entry point needs no register for `zeroed`); 0 is
    // empty, 2 out of range, 4 scheduled. One test of bit 0 is all
    // `malloc` decodes; the entry point inlines this section.
    let st: u32;
    // SAFETY: guaranteed by the caller; the section reads the header the
    // kernel's `mm_cid` names and commits with one store of `len`.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "lea {t}, [rip + 9b]",
            "mov qword ptr [{rs} + {off_cs}], {t}",
            "2:",
            "mov {h:e}, dword ptr [{rs} + {off_cid}]",
            "cmp {h:e}, {n:e}",
            "jae 6f",
            "imul {h}, {h}, {stride}",
            "add {h}, {base}",
            "mov {l:e}, dword ptr [{h} + {o_len}]",
            "test {l:e}, {l:e}",
            "jz 5f",
            "mov {t}, qword ptr [{h} + {o_allocs}]",
            "inc {t}",
            "mov qword ptr [{h} + {o_allocs}], {t}",
            "test {t:e}, dword ptr [{h} + {o_mask}]",
            "jz 7f",
            "dec {l:e}",
            "mov {t}, qword ptr [{h} + {o_slots}]",
            "mov {p}, qword ptr [{t} + {l:r}*8]",
            "cmp {l:e}, dword ptr [{h} + {o_lw}]",
            "jae 12f",
            "mov dword ptr [{h} + {o_lw}], {l:e}",
            "12:",
            "mov dword ptr [{h} + {o_len}], {l:e}",
            "3:",
            "mov {st:e}, 1",
            "cmp {l:e}, dword ptr [{h} + {o_zeroed}]",
            "jae 13f",
            "mov {st:e}, 3",
            "jmp 13f",
            ".byte 0x0f, 0xb9, 0x3d",
            ".long {sig}",
            "4:",
            "lock inc qword ptr [rip + {aborts}]",
            "jmp 8b",
            "5:",
            "xor {st:e}, {st:e}",
            "jmp 13f",
            "6:",
            "mov {st:e}, 2",
            "jmp 13f",
            "7:",
            "mov {st:e}, 4",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            h = out(reg) _,
            l = out(reg) _,
            t = out(reg) _,
            p = out(reg) p,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_len = const O_LEN,
            o_allocs = const O_ALLOCS,
            o_mask = const O_MASK,
            o_slots = const O_SLOTS,
            o_lw = const O_LOW_WATER,
            o_zeroed = const O_ZEROED,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
        #[cfg(target_arch = "aarch64")]
        asm!(
            ".arch_extension pauth",
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "adrp {t}, 9b",
            "add {t}, {t}, :lo12:9b",
            "str {t}, [{rs}, #{off_cs}]",
            "2:",
            "ldr {h:w}, [{rs}, #{off_cid}]",
            "cmp {h:w}, {n:w}",
            "b.hs 6f",
            "mov {t}, #{stride}",
            "madd {h}, {h}, {t}, {base}",
            "ldr {l:w}, [{h}, #{o_len}]",
            "cbz {l:w}, 5f",
            "ldr {t}, [{h}, #{o_allocs}]",
            "add {t}, {t}, #1",
            "str {t}, [{h}, #{o_allocs}]",
            "ldr {z:w}, [{h}, #{o_mask}]",
            "tst {t:w}, {z:w}",
            "b.eq 7f",
            "sub {l:w}, {l:w}, #1",
            "ldr {t}, [{h}, #{o_slots}]",
            "ldr {p}, [{t}, {l:x}, lsl #3]",
            "cbz {pac}, 12f",
            "autda {p}, {t}",
            "12:",
            "ldr {z:w}, [{h}, #{o_lw}]",
            "cmp {l:w}, {z:w}",
            "b.hs 14f",
            "str {l:w}, [{h}, #{o_lw}]",
            "14:",
            "ldr {z:w}, [{h}, #{o_zeroed}]",
            "str {l:w}, [{h}, #{o_len}]",
            "3:",
            "cmp {l:w}, {z:w}",
            "mov {st:w}, #1",
            "b.hs 13f",
            "mov {st:w}, #3",
            "b 13f",
            ".inst {sig}",
            "4:",
            "adrp {t}, {aborts}",
            "add {t}, {t}, :lo12:{aborts}",
            "ldr {h}, [{t}]",
            "add {h}, {h}, #1",
            "str {h}, [{t}]",
            "b 8b",
            "5:",
            "mov {st:w}, #0",
            "b 13f",
            "6:",
            "mov {st:w}, #2",
            "b 13f",
            "7:",
            "mov {st:w}, #4",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            pac = in(reg) pac as usize,
            h = out(reg) _,
            l = out(reg) _,
            t = out(reg) _,
            p = out(reg) p,
            z = out(reg) _,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_len = const O_LEN,
            o_allocs = const O_ALLOCS,
            o_mask = const O_MASK,
            o_slots = const O_SLOTS,
            o_lw = const O_LOW_WATER,
            o_zeroed = const O_ZEROED,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
    }
    (p, st)
}

/// Push `p` onto the running CPU's header for the class `t.base` names,
/// clipping the zero prefix. The entry is written into the dead slot above
/// `len` before the commit, which is harmless if the section is aborted.
///
/// # Safety
/// As for [`pop`]; `p` is a plain slot pointer of the class.
#[cfg(target_os = "linux")]
#[inline(always)]
pub unsafe fn push(area: *mut u8, t: Table, p: *mut u8, pac: bool) -> Push {
    // SAFETY: guaranteed by the caller.
    Push::decode(unsafe { push_raw(area, t, p, pac) })
}

/// [`push`] returning the section's raw status word (`PUSH_*`): what the
/// entry points inline, so a hit is one compare with zero.
///
/// # Safety
/// As for [`push`].
#[cfg(target_os = "linux")]
#[inline(always)]
pub unsafe fn push_raw(area: *mut u8, t: Table, p: *mut u8, pac: bool) -> u32 {
    let st: u32;
    // x86_64 has no pointer authentication; the flag is for aarch64.
    #[cfg(target_arch = "x86_64")]
    let _ = pac;
    // SAFETY: guaranteed by the caller; one commit store of `len`.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "lea {t}, [rip + 9b]",
            "mov qword ptr [{rs} + {off_cs}], {t}",
            "2:",
            "mov {h:e}, dword ptr [{rs} + {off_cid}]",
            "cmp {h:e}, {n:e}",
            "jae 6f",
            "imul {h}, {h}, {stride}",
            "add {h}, {base}",
            "mov {l:e}, dword ptr [{h} + {o_len}]",
            "cmp {l:e}, dword ptr [{h} + {o_cap}]",
            "jae 5f",
            "mov {t}, qword ptr [{h} + {o_slots}]",
            "mov qword ptr [{t} + {l:r}*8], {p}",
            "cmp {l:e}, dword ptr [{h} + {o_zeroed}]",
            "jae 12f",
            "mov dword ptr [{h} + {o_zeroed}], {l:e}",
            "12:",
            "inc {l:e}",
            "mov dword ptr [{h} + {o_len}], {l:e}",
            "3:",
            "xor {st:e}, {st:e}",
            "jmp 13f",
            ".byte 0x0f, 0xb9, 0x3d",
            ".long {sig}",
            "4:",
            "lock inc qword ptr [rip + {aborts}]",
            "jmp 8b",
            "5:",
            "mov {st:e}, 1",
            "jmp 13f",
            "6:",
            "mov {st:e}, 2",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            p = in(reg) p,
            h = out(reg) _,
            l = out(reg) _,
            t = out(reg) _,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_len = const O_LEN,
            o_cap = const O_CAP,
            o_slots = const O_SLOTS,
            o_zeroed = const O_ZEROED,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
        #[cfg(target_arch = "aarch64")]
        asm!(
            ".arch_extension pauth",
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "adrp {t}, 9b",
            "add {t}, {t}, :lo12:9b",
            "str {t}, [{rs}, #{off_cs}]",
            "2:",
            "ldr {h:w}, [{rs}, #{off_cid}]",
            "cmp {h:w}, {n:w}",
            "b.hs 6f",
            "mov {t}, #{stride}",
            "madd {h}, {h}, {t}, {base}",
            "ldr {l:w}, [{h}, #{o_len}]",
            "ldr {e:w}, [{h}, #{o_cap}]",
            "cmp {l:w}, {e:w}",
            "b.hs 5f",
            "ldr {t}, [{h}, #{o_slots}]",
            "mov {e}, {p}",
            "cbz {pac}, 12f",
            "pacda {e}, {t}",
            "12:",
            "str {e}, [{t}, {l:x}, lsl #3]",
            "ldr {e:w}, [{h}, #{o_zeroed}]",
            "cmp {l:w}, {e:w}",
            "b.hs 14f",
            "str {l:w}, [{h}, #{o_zeroed}]",
            "14:",
            "add {l:w}, {l:w}, #1",
            "str {l:w}, [{h}, #{o_len}]",
            "3:",
            "mov {st:w}, #0",
            "b 13f",
            ".inst {sig}",
            "4:",
            "adrp {t}, {aborts}",
            "add {t}, {t}, :lo12:{aborts}",
            "ldr {h}, [{t}]",
            "add {h}, {h}, #1",
            "str {h}, [{t}]",
            "b 8b",
            "5:",
            "mov {st:w}, #1",
            "b 13f",
            "6:",
            "mov {st:w}, #2",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            p = in(reg) p,
            pac = in(reg) pac as usize,
            h = out(reg) _,
            l = out(reg) _,
            t = out(reg) _,
            e = out(reg) _,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_len = const O_LEN,
            o_cap = const O_CAP,
            o_slots = const O_SLOTS,
            o_zeroed = const O_ZEROED,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
    }
    st
}

/// Push as many of `ptrs` (in order, so `ptrs[k-1]` ends on top) as the
/// running CPU's header has room for, and return how many. When the stack
/// was empty the zero prefix becomes the batch if `zeroed` (else 0) and
/// `low_water` restarts at the batch length; a batch on top of existing
/// entries leaves both alone. `Err(())` is out of range. A header never
/// installed has `cap == 0` and takes nothing; the caller checks `cap` and
/// installs.
///
/// With `pac` the entries are signed under the stack's base as the
/// modifier, the one every pop and `take_all` authenticate with. An
/// earlier form of the aarch64 section advanced the base register by
/// `len` entries before the copy loop and signed with that, so a batch
/// pushed onto a non-empty stack (a free on this CPU between the empty
/// pop and the refill's push) was signed under another modifier and the
/// next pop's `autda` faulted (`SIGILL` under FEAT_FPAC). The base is
/// now kept in its own register for the whole section.
///
/// # Safety
/// As for [`pop`]; `ptrs` are plain slot pointers of the class.
#[cfg(target_os = "linux")]
#[inline(never)]
pub unsafe fn push_batch(area: *mut u8, t: Table, ptrs: &[*mut u8], zeroed: bool, pac: bool) -> Result<usize, ()> {
    let pushed: usize;
    // x86_64 has no pointer authentication; the flag is for aarch64.
    #[cfg(target_arch = "x86_64")]
    let _ = pac;
    let st: u32;
    // SAFETY: guaranteed by the caller; the copies land above `len` and
    // the one commit store is `len`.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "lea {t}, [rip + 9b]",
            "mov qword ptr [{rs} + {off_cs}], {t}",
            "2:",
            "mov {h:e}, dword ptr [{rs} + {off_cid}]",
            "cmp {h:e}, {n:e}",
            "jae 6f",
            "imul {h}, {h}, {stride}",
            "add {h}, {base}",
            "mov {l:e}, dword ptr [{h} + {o_len}]",
            "mov {r:e}, dword ptr [{h} + {o_cap}]",
            "sub {r:e}, {l:e}",
            "cmp {r}, {k}",
            "jbe 12f",
            "mov {r}, {k}",
            "12:",
            "mov {t}, qword ptr [{h} + {o_slots}]",
            "lea {t}, [{t} + {l:r}*8]",
            "xor {i:e}, {i:e}",
            "14:",
            "cmp {i}, {r}",
            "jae 15f",
            "mov {e}, qword ptr [{buf} + {i}*8]",
            "mov qword ptr [{t} + {i}*8], {e}",
            "inc {i}",
            "jmp 14b",
            "15:",
            "test {l:e}, {l:e}",
            "jnz 16f",
            "mov {e:e}, {r:e}",
            "test {zf:e}, {zf:e}",
            "jnz 17f",
            "xor {e:e}, {e:e}",
            "17:",
            "mov dword ptr [{h} + {o_zeroed}], {e:e}",
            "mov dword ptr [{h} + {o_lw}], {r:e}",
            "16:",
            "add {l:e}, {r:e}",
            "mov dword ptr [{h} + {o_len}], {l:e}",
            "3:",
            "xor {st:e}, {st:e}",
            "jmp 13f",
            ".byte 0x0f, 0xb9, 0x3d",
            ".long {sig}",
            "4:",
            "lock inc qword ptr [rip + {aborts}]",
            "jmp 8b",
            "6:",
            "mov {st:e}, 2",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            buf = in(reg) ptrs.as_ptr(),
            k = in(reg) ptrs.len(),
            zf = in(reg) zeroed as u32,
            h = out(reg) _,
            l = out(reg) _,
            r = out(reg) pushed,
            t = out(reg) _,
            i = out(reg) _,
            e = out(reg) _,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_len = const O_LEN,
            o_cap = const O_CAP,
            o_slots = const O_SLOTS,
            o_zeroed = const O_ZEROED,
            o_lw = const O_LOW_WATER,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
        #[cfg(target_arch = "aarch64")]
        asm!(
            ".arch_extension pauth",
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "adrp {t}, 9b",
            "add {t}, {t}, :lo12:9b",
            "str {t}, [{rs}, #{off_cs}]",
            "2:",
            "ldr {h:w}, [{rs}, #{off_cid}]",
            "cmp {h:w}, {n:w}",
            "b.hs 6f",
            "mov {t}, #{stride}",
            "madd {h}, {h}, {t}, {base}",
            "ldr {l:w}, [{h}, #{o_len}]",
            "ldr {r:w}, [{h}, #{o_cap}]",
            "sub {r:w}, {r:w}, {l:w}",
            "cmp {r}, {k}",
            "csel {r}, {r}, {k}, ls",
            // `t` stays the stack base, the PAC modifier (function doc);
            // `a` is the write position.
            "ldr {t}, [{h}, #{o_slots}]",
            "add {a}, {t}, {l:x}, lsl #3",
            "mov {i}, #0",
            "14:",
            "cmp {i}, {r}",
            "b.hs 15f",
            "ldr {e}, [{buf}, {i}, lsl #3]",
            "cbz {pac}, 12f",
            "pacda {e}, {t}",
            "12:",
            "str {e}, [{a}, {i}, lsl #3]",
            "add {i}, {i}, #1",
            "b 14b",
            "15:",
            "cbnz {l:w}, 16f",
            "cmp {zf:w}, #0",
            "csel {e:w}, {r:w}, wzr, ne",
            "str {e:w}, [{h}, #{o_zeroed}]",
            "str {r:w}, [{h}, #{o_lw}]",
            "16:",
            "add {l:w}, {l:w}, {r:w}",
            "str {l:w}, [{h}, #{o_len}]",
            "3:",
            "mov {st:w}, #0",
            "b 13f",
            ".inst {sig}",
            "4:",
            "adrp {t}, {aborts}",
            "add {t}, {t}, :lo12:{aborts}",
            "ldr {h}, [{t}]",
            "add {h}, {h}, #1",
            "str {h}, [{t}]",
            "b 8b",
            "6:",
            "mov {st:w}, #2",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            buf = in(reg) ptrs.as_ptr(),
            k = in(reg) ptrs.len(),
            zf = in(reg) zeroed as u32,
            pac = in(reg) pac as usize,
            h = out(reg) _,
            l = out(reg) _,
            r = out(reg) pushed,
            t = out(reg) _,
            a = out(reg) _,
            i = out(reg) _,
            e = out(reg) _,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_len = const O_LEN,
            o_cap = const O_CAP,
            o_slots = const O_SLOTS,
            o_zeroed = const O_ZEROED,
            o_lw = const O_LOW_WATER,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
    }
    if st == 0 { Ok(pushed) } else { Err(()) }
}

/// Move every entry of the running CPU's header for the class into `out`
/// (bottom first, plain pointers) and leave it empty (`low_water` 0).
/// Returns how many. `out` must hold the header's capacity.
///
/// # Safety
/// As for [`pop`]; `out.len()` is at least the class's cache capacity.
#[cfg(target_os = "linux")]
#[inline(never)]
pub unsafe fn take_all(area: *mut u8, t: Table, out: &mut [*mut u8], pac: bool) -> Result<usize, ()> {
    let n_out: u32;
    // x86_64 has no pointer authentication; the flag is for aarch64.
    #[cfg(target_arch = "x86_64")]
    let _ = pac;
    let st: u32;
    // SAFETY: guaranteed by the caller; the copies go to the caller's
    // buffer and the one commit store is `len = 0`.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "lea {t}, [rip + 9b]",
            "mov qword ptr [{rs} + {off_cs}], {t}",
            "2:",
            "mov {h:e}, dword ptr [{rs} + {off_cid}]",
            "cmp {h:e}, {n:e}",
            "jae 6f",
            "imul {h}, {h}, {stride}",
            "add {h}, {base}",
            "mov {l:e}, dword ptr [{h} + {o_len}]",
            "mov {t}, qword ptr [{h} + {o_slots}]",
            "xor {i:e}, {i:e}",
            "14:",
            "cmp {i}, {l:r}",
            "jae 15f",
            "mov {e}, qword ptr [{t} + {i}*8]",
            "mov qword ptr [{out} + {i}*8], {e}",
            "inc {i}",
            "jmp 14b",
            "15:",
            "mov dword ptr [{h} + {o_lw}], 0",
            "mov dword ptr [{h} + {o_len}], 0",
            "3:",
            "xor {st:e}, {st:e}",
            "jmp 13f",
            ".byte 0x0f, 0xb9, 0x3d",
            ".long {sig}",
            "4:",
            "lock inc qword ptr [rip + {aborts}]",
            "jmp 8b",
            "6:",
            "mov {st:e}, 2",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            out = in(reg) out.as_mut_ptr(),
            h = out(reg) _,
            l = out(reg) n_out,
            t = out(reg) _,
            i = out(reg) _,
            e = out(reg) _,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_len = const O_LEN,
            o_slots = const O_SLOTS,
            o_lw = const O_LOW_WATER,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
        #[cfg(target_arch = "aarch64")]
        asm!(
            ".arch_extension pauth",
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "adrp {t}, 9b",
            "add {t}, {t}, :lo12:9b",
            "str {t}, [{rs}, #{off_cs}]",
            "2:",
            "ldr {h:w}, [{rs}, #{off_cid}]",
            "cmp {h:w}, {n:w}",
            "b.hs 6f",
            "mov {t}, #{stride}",
            "madd {h}, {h}, {t}, {base}",
            "ldr {l:w}, [{h}, #{o_len}]",
            "ldr {t}, [{h}, #{o_slots}]",
            "mov {i}, #0",
            "14:",
            "cmp {i}, {l:x}",
            "b.hs 15f",
            "ldr {e}, [{t}, {i}, lsl #3]",
            "cbz {pac}, 12f",
            "autda {e}, {t}",
            "12:",
            "str {e}, [{out}, {i}, lsl #3]",
            "add {i}, {i}, #1",
            "b 14b",
            "15:",
            "str wzr, [{h}, #{o_lw}]",
            "str wzr, [{h}, #{o_len}]",
            "3:",
            "mov {st:w}, #0",
            "b 13f",
            ".inst {sig}",
            "4:",
            "adrp {t}, {aborts}",
            "add {t}, {t}, :lo12:{aborts}",
            "ldr {h}, [{t}]",
            "add {h}, {h}, #1",
            "str {h}, [{t}]",
            "b 8b",
            "6:",
            "mov {st:w}, #2",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            out = in(reg) out.as_mut_ptr(),
            pac = in(reg) pac as usize,
            h = out(reg) _,
            l = out(reg) n_out,
            t = out(reg) _,
            i = out(reg) _,
            e = out(reg) _,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_len = const O_LEN,
            o_slots = const O_SLOTS,
            o_lw = const O_LOW_WATER,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
    }
    if st == 0 { Ok(n_out as usize) } else { Err(()) }
}

/// Outcome of [`install`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    Installed,
    /// Another thread installed the header first; the caller frees its
    /// stack.
    Exists,
    /// The running CPU's header is not `expected`: the thread migrated
    /// after the caller prepared that header, and the section must not
    /// publish one the caller did not prepare. The caller re-reads
    /// `mm_cid` and starts over.
    Moved,
    OutOfRange,
}

/// Give the running CPU's header for the class its stack: store `slots`
/// and commit `cap`, unless another thread already did (`Exists`). The
/// header's other fields are written by the caller beforehand with plain
/// stores, which every installer writes identically; only `slots` differs
/// between installers, so it is stored inside the section, and `cap` is
/// the commit that publishes it. The section checks that the header it
/// resolved is `expected`, the one the caller prepared (`Moved` otherwise).
///
/// # Safety
/// As for [`pop`]; `slots` is a metadata object of `cap` pointers;
/// `expected` is a header of the table for this class.
#[cfg(target_os = "linux")]
#[inline(never)]
pub unsafe fn install(area: *mut u8, t: Table, expected: *const u8, slots: *mut *mut u8, cap: u32) -> Install {
    let st: u32;
    // SAFETY: guaranteed by the caller; the commit store is `cap`.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "lea {t}, [rip + 9b]",
            "mov qword ptr [{rs} + {off_cs}], {t}",
            "2:",
            "mov {h:e}, dword ptr [{rs} + {off_cid}]",
            "cmp {h:e}, {n:e}",
            "jae 6f",
            "imul {h}, {h}, {stride}",
            "add {h}, {base}",
            "cmp {h}, {exp}",
            "jne 7f",
            "cmp dword ptr [{h} + {o_cap}], 0",
            "jne 5f",
            "mov qword ptr [{h} + {o_slots}], {slots}",
            "mov dword ptr [{h} + {o_cap}], {cap:e}",
            "3:",
            "xor {st:e}, {st:e}",
            "jmp 13f",
            ".byte 0x0f, 0xb9, 0x3d",
            ".long {sig}",
            "4:",
            "lock inc qword ptr [rip + {aborts}]",
            "jmp 8b",
            "5:",
            "mov {st:e}, 1",
            "jmp 13f",
            "6:",
            "mov {st:e}, 2",
            "jmp 13f",
            "7:",
            "mov {st:e}, 3",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            exp = in(reg) expected,
            slots = in(reg) slots,
            cap = in(reg) cap,
            h = out(reg) _,
            t = out(reg) _,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_cap = const O_CAP,
            o_slots = const O_SLOTS,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
        // No `.arch_extension pauth` here, unlike the four sections above:
        // this one stores `slots` and `cap`, never a signed entry, so it
        // has no `pacda`/`autda` to assemble.
        #[cfg(target_arch = "aarch64")]
        asm!(
            ".pushsection __rseq_cs, \"aw\"",
            ".balign 32",
            "9:",
            ".long 0, 0",
            ".quad 2f, 3f - 2f, 4f",
            ".popsection",
            "8:",
            "adrp {t}, 9b",
            "add {t}, {t}, :lo12:9b",
            "str {t}, [{rs}, #{off_cs}]",
            "2:",
            "ldr {h:w}, [{rs}, #{off_cid}]",
            "cmp {h:w}, {n:w}",
            "b.hs 6f",
            "mov {t}, #{stride}",
            "madd {h}, {h}, {t}, {base}",
            "cmp {h}, {exp}",
            "b.ne 7f",
            "ldr {t:w}, [{h}, #{o_cap}]",
            "cbnz {t:w}, 5f",
            "str {slots}, [{h}, #{o_slots}]",
            "str {cap:w}, [{h}, #{o_cap}]",
            "3:",
            "mov {st:w}, #0",
            "b 13f",
            ".inst {sig}",
            "4:",
            "adrp {t}, {aborts}",
            "add {t}, {t}, :lo12:{aborts}",
            "ldr {h}, [{t}]",
            "add {h}, {h}, #1",
            "str {h}, [{t}]",
            "b 8b",
            "5:",
            "mov {st:w}, #1",
            "b 13f",
            "6:",
            "mov {st:w}, #2",
            "b 13f",
            "7:",
            "mov {st:w}, #3",
            "13:",
            rs = in(reg) area,
            base = in(reg) t.base,
            n = in(reg) t.n,
            exp = in(reg) expected,
            slots = in(reg) slots,
            cap = in(reg) cap,
            h = out(reg) _,
            t = out(reg) _,
            st = lateout(reg) st,
            off_cs = const RSEQ_CS_OFFSET,
            off_cid = const MM_CID_OFFSET,
            stride = const STRIDE,
            o_cap = const O_CAP,
            o_slots = const O_SLOTS,
            sig = const SIG,
            aborts = sym ABORTS,
            options(nostack),
        );
    }
    match st {
        0 => Install::Installed,
        1 => Install::Exists,
        2 => Install::OutOfRange,
        _ => Install::Moved,
    }
}

/// Number of possible CPUs, `nr_cpu_ids`, from
/// `/sys/devices/system/cpu/possible` (`0-63`, or a list such as `0-3,8`):
/// the bound on `mm_cid` and the size of the per-CPU table. `None` when
/// the file cannot be read or parsed (the mode then stays off, the report
/// says why); `possible_cpus_from` is the parser, host-testable.
#[cfg(target_os = "linux")]
pub fn possible_cpus() -> Option<u32> {
    let mut buf = [0u8; 256];
    let n = crate::sys::cgroup::read_file(b"/sys/devices/system/cpu/possible\0", &mut buf)?;
    possible_cpus_from(&buf[..n])
}

/// Parse a CPU list (`0-63\n`, `0,2-5`) to the highest id plus one.
pub fn possible_cpus_from(text: &[u8]) -> Option<u32> {
    let mut max: Option<u32> = None;
    let mut cur: Option<u32> = None;
    for &b in text {
        match b {
            b'0'..=b'9' => cur = Some(cur.unwrap_or(0).checked_mul(10)?.checked_add((b - b'0') as u32)?),
            b'-' | b',' | b'\n' | b' ' => {
                if let Some(v) = cur.take() {
                    max = Some(max.map_or(v, |m| m.max(v)));
                }
            }
            _ => return None,
        }
    }
    if let Some(v) = cur {
        max = Some(max.map_or(v, |m| m.max(v)));
    }
    max?.checked_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn possible_cpu_lists_parse() {
        assert_eq!(possible_cpus_from(b"0-63\n"), Some(64));
        assert_eq!(possible_cpus_from(b"0\n"), Some(1));
        assert_eq!(possible_cpus_from(b"0-3,8-11\n"), Some(12));
        assert_eq!(possible_cpus_from(b"0,2-5"), Some(6));
        assert_eq!(possible_cpus_from(b""), None);
        assert_eq!(possible_cpus_from(b"x"), None);
        assert_eq!(possible_cpus_from(b"99999999999"), None);
    }

    #[test]
    fn verdicts_follow_the_gate() {
        assert_eq!(Probe::NONE.verdict(), "no-symbols");
        assert!(!Probe::NONE.mm_cid_usable());
        let p = Probe { present: true, size: 0, flags: 0, offset: 0, registered: false };
        assert_eq!(p.verdict(), "unregistered");
        let p = Probe { present: true, size: 20, flags: 0, offset: 0, registered: true };
        assert_eq!(p.verdict(), "no-mm_cid");
        assert!(!p.mm_cid_usable());
        let p = Probe { present: true, size: 28, flags: 1, offset: 0, registered: true };
        assert_eq!(p.verdict(), "flags");
        let p = Probe { present: true, size: 33, flags: 0, offset: 192, registered: true };
        assert_eq!(p.verdict(), "usable");
        assert!(p.mm_cid_usable());
    }

    /// The both-ways probe test: what glibc registered on this host is
    /// printed, and whatever it is, the gate agrees with the raw fields
    /// and `mm_cid` reads as a valid id where it is usable (and is left
    /// unread where it is not).
    #[cfg(target_os = "linux")]
    #[test]
    fn probe_reads_the_registered_area() {
        let p = probe();
        std::eprintln!(
            "rseq: present={} size={} flags={} offset={} registered={} verdict={}",
            p.present,
            p.size,
            p.flags,
            p.offset,
            p.registered,
            p.verdict()
        );
        assert!(p.present, "glibc 2.35+ exports the symbols in every supported image");
        if p.size == 0 {
            assert!(!p.mm_cid_usable());
            return;
        }
        assert!(p.registered, "cpu_id is written at registration");
        let a = area(p.offset);
        // SAFETY: this thread's registered area (`registered`), which
        // holds `cpu_id` from `BASE_SIZE` on.
        let cpu = unsafe { cpu_id(a) };
        let ncpus = possible_cpus().expect("/sys/devices/system/cpu/possible");
        assert!(cpu < ncpus, "cpu_id {cpu} below the possible count {ncpus}");
        if !p.mm_cid_usable() {
            assert!(p.size < MM_CID_END || p.flags != 0, "unusable for a reason the verdict names: {}", p.verdict());
            return;
        }
        assert!(p.size >= MM_CID_END);
        // SAFETY: as above, and `size >= MM_CID_END` covers `mm_cid`.
        let cid = unsafe { mm_cid(a) };
        assert!(cid < ncpus, "mm_cid {cid} below the possible count {ncpus}");
        pinned_threads_read_live_ids(p, ncpus);
    }

    /// The calling thread's allowed CPUs (`sched_getaffinity`: the cpuset
    /// or container mask, not the machine's), lowest first.
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    fn pin_to(cpu: usize) -> bool {
        // SAFETY: as above; `CPU_SET` writes one bit of the set (`cpu` came
        // from `allowed_cpus`, so it is below `CPU_SETSIZE`), and
        // `sched_setaffinity` only reads it.
        let mut set: ::libc::cpu_set_t = unsafe { core::mem::zeroed() };
        unsafe { ::libc::CPU_SET(cpu, &mut set) };
        unsafe { ::libc::sched_setaffinity(0, core::mem::size_of::<::libc::cpu_set_t>(), &set) == 0 }
    }

    /// What one pinned thread saw.
    #[cfg(target_os = "linux")]
    struct Pinned {
        cpu: usize,
        pinned: bool,
        /// `cpu_id` reads that were not the pinned CPU: (count, first value).
        strays: (u64, u32),
        reads: u64,
        cids: std::collections::BTreeSet<u32>,
    }

    /// Threads pinned to distinct allowed CPUs, at most this many.
    #[cfg(target_os = "linux")]
    const MAX_PINNED: usize = 64;
    /// The spin window when the ids are slow to spread (a loaded host);
    /// the common case ends as soon as every pinned thread has shown its
    /// own id.
    #[cfg(target_os = "linux")]
    const PIN_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

    /// The live half of the probe test. An earlier form spawned as many
    /// spinners as CPUs and asserted more than one `mm_cid` after 50 ms,
    /// which depended on the scheduler spreading them: inside the whole
    /// unit-test binary on an 8-vCPU Chainguard OS cell it read one id on
    /// the first run of every cell (`bench/results/2026-09-20-
    /// chainguard-os-cells.md`, section 3). This form pins one thread per
    /// allowed CPU with `sched_setaffinity`, which makes two facts
    /// deterministic on both `mm_cid` implementations (6.18's, where an id
    /// follows the running task and is recycled when it sleeps; 6.19+'s
    /// rewrite, per-task ids while threads <= CPUs, per-CPU ids beyond):
    /// a pinned thread's `cpu_id` field reads its CPU on every load (the
    /// kernel writes the area, and the probe reads the registered one),
    /// and two pinned threads running at the same time on two CPUs hold
    /// two ids, so the spinners' ids cover more than one value as soon as
    /// any two overlap, which the window is long enough for on any host
    /// that runs two threads in two seconds. Where the check cannot be
    /// made (one CPU allowed, a pin refused, or a host that never ran two
    /// of them together) the test says so and skips it, never passes it.
    #[cfg(target_os = "linux")]
    fn pinned_threads_read_live_ids(p: Probe, ncpus: u32) {
        let Some(allowed) = allowed_cpus() else {
            std::eprintln!("rseq: skip the pinned check: sched_getaffinity failed");
            return;
        };
        if allowed.len() < 2 {
            std::eprintln!("rseq: skip the pinned check: {} CPU allowed ({:?})", allowed.len(), allowed);
            return;
        }
        let cpus: std::vec::Vec<usize> = allowed.iter().copied().take(MAX_PINNED).collect();
        let n = cpus.len();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(n));
        let distinct = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hs: std::vec::Vec<_> = cpus
            .iter()
            .map(|&cpu| {
                let barrier = barrier.clone();
                let distinct = distinct.clone();
                let done = done.clone();
                std::thread::spawn(move || {
                    let mut r = Pinned { cpu, pinned: pin_to(cpu), strays: (0, 0), reads: 0, cids: std::collections::BTreeSet::new() };
                    barrier.wait();
                    if !r.pinned {
                        return r;
                    }
                    let a = area(p.offset);
                    let t0 = std::time::Instant::now();
                    loop {
                        // SAFETY: this thread's registered area, `size >= MM_CID_END`.
                        let (c, id) = unsafe { (cpu_id(a), mm_cid(a)) };
                        r.reads += 1;
                        if c as usize != cpu {
                            if r.strays.0 == 0 {
                                r.strays.1 = c;
                            }
                            r.strays.0 += 1;
                        }
                        assert!(id < ncpus, "mm_cid {id} below the possible count {ncpus}");
                        if r.cids.insert(id) {
                            let mut d = distinct.lock().unwrap();
                            d.insert(id);
                            if d.len() >= n {
                                done.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        if done.load(std::sync::atomic::Ordering::Relaxed) || t0.elapsed() >= PIN_WINDOW {
                            return r;
                        }
                    }
                })
            })
            .collect();
        let rs: std::vec::Vec<Pinned> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        let d = distinct.lock().unwrap().len();
        let reads: u64 = rs.iter().map(|r| r.reads).sum();
        std::eprintln!(
            "rseq: {d} distinct mm_cid values over {n} threads pinned to CPUs {:?} of {ncpus} possible ({reads} reads)",
            cpus
        );
        if let Some(r) = rs.iter().find(|r| !r.pinned) {
            std::eprintln!("rseq: skip the pinned check: sched_setaffinity to CPU {} refused", r.cpu);
            return;
        }
        // Deterministic on both implementations: the kernel keeps a pinned
        // thread's `cpu_id` at its CPU, so every load through the probe's
        // offset read the registered area.
        for r in &rs {
            assert_eq!(
                r.strays.0, 0,
                "thread pinned to CPU {} read cpu_id {} on {} of {} loads: the area at offset {} is not the registered one",
                r.cpu, r.strays.1, r.strays.0, r.reads, p.offset
            );
        }
        if d == 1 {
            // Two pinned spinners never overlapped in two seconds: nothing
            // to assert about, and said so rather than passed.
            std::eprintln!("rseq: skip the distinctness check: every pinned thread read one mm_cid over {:?} (a host too loaded to run two of them at once)", PIN_WINDOW);
        }
    }
}
