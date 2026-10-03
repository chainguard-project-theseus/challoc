//! The `Display` and `Debug` impls that the `DisplaySpecImpl` and
//! `DebugSpecImpl` seams describe. Their `fmt_req` says when `fmt` may
//! be called: always, or (for `LgSize`) on a stated condition. Verus
//! assumes that such a call returns without a panic or undefined
//! behaviour; nothing is claimed about what it writes. [`formats`] checks
//! exactly that, through a sink that may fail any write (so every error
//! path runs too), under the plain formatter and a padded one.

use core::fmt::{self, Debug, Display, Write};

use crate::huge::RunState;
use crate::slab;
use crate::sys::Errno;
use crate::sys::features;

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
pub(crate) fn formats_debug<T: Debug>(v: &T) {
    let _ = write!(Sink, "{:?}", v);
    let _ = write!(Sink, "{:#?}", v);
}

/// `v` formats with `{:?}` and `{:>24?}`, whatever the writer returns:
/// [`formats_debug`] without the alternate form, for the impls whose
/// `{:#?}` Kani does not finish.
pub(crate) fn formats_debug_flat<T: Debug>(v: &T) {
    let _ = write!(Sink, "{:?}", v);
    let _ = write!(Sink, "{:>24?}", v);
}

/// S297 `DisplaySpecImpl for sys::Errno` (`fmt_req` true): every `i32`.
#[kani::proof]
#[kani::unwind(12)]
fn display_errno() {
    formats(&Errno(kani::any()));
}

/// S298 `DebugSpecImpl for slab::State` (`fmt_req` true): every variant.
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

/// S299 `DebugSpecImpl for huge::RunState` (`fmt_req` true): every
/// variant.
#[kani::proof]
fn debug_run_state() {
    let v = [RunState::Active, RunState::Empty, RunState::Purged];
    let i: usize = kani::any();
    kani::assume(i < v.len());
    formats_debug(&v[i]);
}

/// An arbitrary `features::Value`; `Word` holds one of the words the
/// feature line uses (any `&'static str` would do: `write_str` takes it
/// as is).
fn any_feature_value() -> features::Value {
    match kani::any::<u8>() % 4 {
        0 => features::Value::YesNo(kani::any()),
        1 => features::Value::Num(kani::any()),
        2 => features::Value::Word(if kani::any() {
            "madvise"
        } else {
            "kernel-disabled"
        }),
        _ => features::Value::Sizes(kani::any()),
    }
}

/// S217 `DisplaySpecImpl for Value` (`sys::features::Value`, `fmt_req`
/// true): every variant, every number and bit set.
#[kani::proof]
#[kani::unwind(66)]
fn display_feature_value() {
    formats(&any_feature_value());
}
