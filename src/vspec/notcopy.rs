//! Compile-time checks that the ghost tokens Verus takes on trust, the
//! `tracked` `external_body` types of the seam ledger (`verus/SEAMS.md`,
//! "Status"), are neither `Copy` nor `Clone`. Verified code can copy a
//! value of a `Copy` type; ruling out `Clone` too, which `Copy`
//! requires, also rules out an explicit duplicate. Without either, code
//! that holds one token cannot make a second, the premise of facts such
//! as "`api::axiom_statics` makes the only [`LinePerm`]".
//!
//! Each check is a top-level `const _: () = assert!(..)` under a `//`
//! comment that names its type, which a seam's status cites (`const
//! assert: <label> in src/vspec/notcopy.rs`). `<T as NotCopy<_>>::HOLDS`
//! names one item when `T` is not `Copy` and two when it is: then the
//! `_` has two solutions, which rustc rejects as ambiguous (E0283). So the
//! build fails, at each quantum and on each target, and under Verus too,
//! when a token gains either impl; the asserted value is always true.
//! `NotClone` is the same for `Clone`. Building with
//! `RUSTFLAGS='--cfg challoc_notcopy_control'` adds the controls at the
//! end, a `Copy` type and a `Clone` one that the checks must reject
//! (`verus/notcopy-control.sh`).

use super::word::WordPerm;
use crate::api::{LinePerm, StatePerm};
use crate::heap::HeapPerm;
use crate::layout::Reserved;

/// `<T as NotCopy<_>>::HOLDS` compiles when `T` is not `Copy`.
pub(crate) trait NotCopy<A> {
    const HOLDS: bool = true;
}

impl<T: ?Sized> NotCopy<()> for T {}

/// The second impl, for a `Copy` type only.
pub(crate) enum IfCopy {}

impl<T: Copy> NotCopy<IfCopy> for T {}

/// `<T as NotClone<_>>::HOLDS` compiles when `T` is not `Clone`.
pub(crate) trait NotClone<A> {
    const HOLDS: bool = true;
}

impl<T: ?Sized> NotClone<()> for T {}

/// The second impl, for a `Clone` type only.
pub(crate) enum IfClone {}

impl<T: Clone> NotClone<IfClone> for T {}

// `LinePerm` is neither Copy nor Clone: `LINE_STATE`'s one permission.
const _: () = assert!(<LinePerm as NotCopy<_>>::HOLDS && <LinePerm as NotClone<_>>::HOLDS);

// `StatePerm` is neither Copy nor Clone: `STATE`'s one permission.
const _: () = assert!(<StatePerm as NotCopy<_>>::HOLDS && <StatePerm as NotClone<_>>::HOLDS);

// `HeapPerm` is neither Copy nor Clone: a thread heap's permission.
const _: () = assert!(<HeapPerm as NotCopy<_>>::HOLDS && <HeapPerm as NotClone<_>>::HOLDS);

// `Reserved` is neither Copy nor Clone: one live reservation.
const _: () = assert!(<Reserved as NotCopy<_>>::HOLDS && <Reserved as NotClone<_>>::HOLDS);

// `WordPerm` is neither Copy nor Clone: a bitmap word's permission.
const _: () = assert!(<WordPerm as NotCopy<_>>::HOLDS && <WordPerm as NotClone<_>>::HOLDS);

// The controls: a `Copy` type, which both checks must reject, and a
// `Clone` one, which `NotClone` must reject. Each `assert!` here is an
// error under the cfg.
#[cfg(challoc_notcopy_control)]
mod control {
    use super::{NotClone, NotCopy};

    #[derive(Clone, Copy)]
    struct CopyControl;

    #[derive(Clone)]
    struct CloneControl;

    const _: () = assert!(<CopyControl as NotCopy<_>>::HOLDS);
    const _: () = assert!(<CopyControl as NotClone<_>>::HOLDS);
    const _: () = assert!(<CloneControl as NotClone<_>>::HOLDS);
}
