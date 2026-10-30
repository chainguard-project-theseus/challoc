//! The `Display` and `Debug` impls that the `DisplaySpecImpl` and
//! `DebugSpecImpl` seams of `vspec/fmt.rs` describe
//! (`verus/ledger/src/vspec/fmt.rs.md`). Their `fmt_req` says when `fmt`
//! may be called: always, for both. Verus assumes that such a call
//! returns without a panic or undefined behaviour; nothing is claimed
//! about what it writes. [`formats`] (for `Display`) and `formats_debug`
//! (for `Debug`) check exactly that, through a sink that may fail any
//! write (so every error path runs too), under the plain formatter and a
//! padded or alternate one.

use core::fmt::{self, Debug, Display, Write};

use crate::slab;
use crate::sys::Errno;

/// A writer that keeps nothing and fails any write, at Kani's choice.
pub(crate) struct Sink;

impl Write for Sink {
    fn write_str(&mut self, _: &str) -> fmt::Result {
        if kani::any() { Err(fmt::Error) } else { Ok(()) }
    }
}

/// `v` formats with `{}` and `{:>24}` (a width, which integers and
/// `Formatter::pad` honour and `write_str` ignores), whatever the writer
/// returns. Kani fails the harness on any panic or undefined behaviour.
pub(crate) fn formats<T: Display>(v: &T) {
    let _ = write!(Sink, "{}", v);
    let _ = write!(Sink, "{:>24}", v);
}

/// `v` formats with `{:?}` and `{:#?}` (the alternate form indents
/// through a `PadAdapter`), whatever the writer returns.
fn formats_debug<T: Debug>(v: &T) {
    let _ = write!(Sink, "{:?}", v);
    let _ = write!(Sink, "{:#?}", v);
}

/// `DisplaySpecImpl for sys::Errno` (`fmt_req` true): every `i32`.
#[kani::proof]
#[kani::unwind(12)]
fn display_errno() {
    formats(&Errno(kani::any()));
}

/// `DebugSpecImpl for slab::State` (`fmt_req` true): every variant.
#[kani::proof]
fn debug_slab_state() {
    let v = [
        slab::State::Fresh,
        slab::State::Active,
        slab::State::Empty,
        slab::State::Purged,
    ];
    let i: usize = kani::any();
    kani::assume(i < v.len());
    formats_debug(&v[i]);
}
