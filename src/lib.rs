//! challoc: a C-ABI malloc replacement for Linux, written in `no_std` Rust.
//!
//! See `docs/design.md` for the design.
//! Consumers are C programs; every entry point is a top-level
//! `pub extern "C"` function. The crate never allocates through anything but
//! its own machinery and never unwinds: `panic = "abort"` in every profile.
//! The unit tests (`cfg(test)`) link `std` for the test harness; the
//! library itself never does.

#![no_std]
// Warnings fail the Kani and Verus runs, which compile code no clippy run
// sees; ordinary builds (and downstream packagers) only see them as
// warnings, and the clippy check fails CI on them.
#![cfg_attr(any(kani, verus_keep_ghost), deny(warnings))]
#![deny(unsafe_op_in_unsafe_fn)]
// Kani checks the exported `malloc` family against the libc signatures
// `std` links to; challoc defines them, as it must.
#![cfg_attr(kani, allow(suspicious_runtime_symbol_definitions))]

#[cfg(test)]
extern crate std;

// Verus needs its prelude imported at the crate root (`verus/README.md`).
#[allow(unused_imports)]
use vstd::prelude::*;

#[cfg(all(feature = "quantum8", feature = "quantum16"))]
compile_error!("features `quantum8` and `quantum16` are mutually exclusive");

#[cfg(not(any(feature = "quantum8", feature = "quantum16")))]
compile_error!("exactly one of the `quantum8` or `quantum16` features must be enabled");

// The proofs take `usize` as 8 bytes (`global size_of usize == 8` in
// `vspec`): x86_64 and aarch64 are 64-bit, and the only targets with the
// asm the crate needs (TLS, rseq, syscalls).
#[cfg(not(target_pointer_width = "64"))]
compile_error!("challoc needs a 64-bit target");

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("challoc supports only x86_64 and aarch64");

verus! {

/// Quantum (minimum size-class granularity and alignment) in bytes, selected
/// at build time by the `quantum8` / `quantum16` features.
#[cfg(feature = "quantum16")]
pub const QUANTUM: usize = 16;
#[cfg(feature = "quantum8")]
pub const QUANTUM: usize = 8;

} // verus!

pub(crate) mod api;
pub(crate) mod arena;
pub(crate) mod bitmap;
pub(crate) mod class;
pub(crate) mod config;
pub(crate) mod heap;
pub(crate) mod huge;
pub(crate) mod layout;
pub(crate) mod lock;
pub(crate) mod meta;
#[cfg(test)]
mod model;
pub(crate) mod policy;
pub(crate) mod purge;
pub(crate) mod rseq;
pub(crate) mod slab;
pub(crate) mod stats;
// `core`'s atomics, or loom's when the loom crate compiles the `vspec`
// wrappers (`tests/loom`).
pub(crate) mod sync;
pub(crate) mod sys;
#[cfg(test)]
mod testutil;
pub(crate) mod thp;
// Specifications of `core` functions vstd lacks, and the trusted wrappers
// the executable code calls for operations Verus does not model
// (`verus/README.md`). Ordinary builds compile the wrappers; the
// specifications are ghost and vanish there, and the expansion of
// `global` is a braced block.
#[allow(unused_braces)]
mod vspec;
// Kani harnesses (`src/kani/`, `.chainguard/ci/kani.yaml`); never part of
// an ordinary build.
#[cfg(kani)]
#[path = "kani/mod.rs"]
mod kani_proofs;

verus! {

/// Panic handler for the `no_std` library. The crate is built with
/// `panic = "abort"`, so this is only reached by an explicit `panic!` or a
/// failed debug assertion. It reports through `sys::abort_fmt`, which writes
/// to stderr with `write(2)` and raises `SIGABRT`, never unwinding. The
/// line reads `challoc: <message> [<file>:<line>]`: the message first, since
/// it is what a user sees when their program frees a pointer twice. Not
/// under Kani, whose harnesses link `std` and its handler.
#[cfg(not(any(test, kani)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    proof {
        // A root of the call graph: the panic runtime calls it, nothing
        // in the crate does.
        crate::sys::grant_aborts();
        broadcast use crate::vspec::panic::axiom_fmt_req_all_panic_message;
    }
    match info.location() {
        Some(loc) => sys::abort_fmt(format_args!(
            "{} [{}:{}]",
            info.message(),
            loc.file(),
            loc.line()
        )),
        None => sys::abort_fmt(format_args!("{}", info.message())),
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::QUANTUM;

    #[test]
    // The constant checks are the test.
    #[allow(clippy::assertions_on_constants)]
    fn quantum_is_a_power_of_two() {
        assert!(QUANTUM.is_power_of_two());
        assert!(QUANTUM == 8 || QUANTUM == 16);
    }

    /// The source files `testutil::shipped_sources` returns (no test or
    /// Kani module or file; `#[cfg(test)]` items inside them are kept, so
    /// this is stricter than the build) have no `assume(..)` or `admit()`,
    /// which would let Verus prove anything after them, and no
    /// `#[verifier(..)]` attribute: the `#[verifier::name]` spelling is the
    /// one `verus/seams.sh` reads, and `name` is one of the attributes the
    /// crate uses: seam kinds `seams.sh` lists (`external_body`),
    /// proof settings (`rlimit`), and `external`, which keeps an item out of
    /// verification. An attribute that trusts code goes in `seams.sh`
    /// before this list.
    #[test]
    fn no_assume_or_admit_in_verified_code() {
        const ATTRS: &[&str] = &[
            "accept_recursive_types",
            "atomic",
            "exec_allows_no_decreases_clause",
            "external",
            "external_body",
            "external_trait_specification",
            "external_type_specification",
            "loop_isolation",
            "opaque",
            "reject_recursive_types",
            "rlimit",
            "spinoff_prover",
            "truncate",
            "type_invariant",
        ];
        // Spelled in parts, so that this test's own lines match nothing.
        let calls = [concat!("assu", "me("), concat!("adm", "it(")];
        let attr = concat!("veri", "fier");
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let mut bad = std::vec::Vec::new();
        let sources = crate::testutil::shipped_sources();
        assert!(sources.len() > 50, "the source tree was not found");
        for (rel, text) in &sources {
            for (i, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                let code = t.split_once(" // ").map_or(t, |(c, _)| c);
                let at = || std::format!("{rel}:{}: `{t}`", i + 1);
                for call in calls {
                    for (k, _) in code.match_indices(call) {
                        let before = &code[..k];
                        if !before.ends_with("kani::") && !before.ends_with(ident) {
                            bad.push(std::format!("{}: a call of `{call}..`", at()));
                        }
                    }
                }
                for (k, _) in code.match_indices(attr) {
                    let rest = &code[k + attr.len()..];
                    if code[..k].ends_with(ident) {
                        continue;
                    }
                    if let Some(rest) = rest.strip_prefix("::") {
                        let name = rest.split([']', '(', ')', ',']).next().unwrap();
                        if !ATTRS.contains(&name) {
                            bad.push(std::format!(
                                "{}: unlisted attribute `{attr}::{name}`",
                                at()
                            ));
                        }
                    } else if rest.starts_with('(') {
                        bad.push(std::format!(
                            "{}: write `{attr}::name`, not `{attr}(name)`",
                            at()
                        ));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "Verus would trust these lines without the seam ledger knowing:\n{}",
            bad.join("\n")
        );
    }

    /// The trusted functions whose premise holds only at their call sites
    /// (`verus/README.md`, "Guard tests"): each is pinned to the callers
    /// its doc names, so a new caller fails here until that doc argues the
    /// premise for it.
    #[test]
    fn trusted_mint_sites_are_pinned() {
        type Sites = &'static [(&'static str, &'static str, usize)];
        const PINS: &[(&str, Sites)] = &[
            // A destroyed heap's block (`heap::axiom_heap_block`).
            (
                concat!("axiom_", "heap_block("),
                &[("heap/mod.rs:", "pub unsafe fn destroy(", 1)],
            ),
            // Zero bytes for a fresh permission (`vspec::zero::ZeroRaw::axiom_new`).
            (
                concat!("ZeroRaw::", "axiom_new("),
                &[
                    ("meta.rs:", "pub fn reserve_table(", 1),
                    ("meta.rs:", "proof fn reserved_table(", 1),
                    ("meta.rs:", "fn reserve_committed(", 1),
                    ("huge/table.rs:", "fn storage(", 1),
                ],
            ),
            // The zero and fresh-memory facts of a refill's claim.
            (
                concat!("axiom_", "discarded_slots_zero("),
                &[("class/mod.rs:", "fn take(", 1)],
            ),
            (
                concat!("axiom_", "fresh_slab_mem("),
                &[("class/mod.rs:", "fn take(", 1)],
            ),
            // Verified itself, but `axiom_discarded_slots_zero` takes its
            // witness on the premise that the read was made in the lock
            // hold that claims the slots.
            (
                concat!("purged_", "zero("),
                &[("class/mod.rs:", "fn take(", 1)],
            ),
            // The frozen `Shared` fields (`heap::shared`).
            (
                concat!("axiom_", "shared_regions("),
                &[("heap/shared.rs:", "pub unsafe fn init(", 1)],
            ),
            (
                concat!("axiom_", "shared_meta("),
                &[("heap/shared.rs:", "unsafe fn put_head<", 1)],
            ),
            // The thread slot's `Heap::create` count (`sys::tls::TlsCell`).
            (
                concat!("note_", "create("),
                &[("heap/thread.rs:", "fn create_heap_for_thread<", 1)],
            ),
        ];
        for (call, sites) in PINS {
            crate::testutil::assert_only_at(call, sites);
        }
    }
}
