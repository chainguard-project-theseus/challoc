//! Specifications for `heap/`: the types of the modules [`crate::heap::Shared`]
//! holds that are still outside `verus!`, opaque to the verified code.
//! Delete each one when its type enters `verus!` (the "duplicate
//! specification" error says which).

use vstd::prelude::*;

verus! {

#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExCVoid(core::ffi::c_void);

// `Tcaches::create` and `Tcaches::destroy` build and free an explicit
// cache's heap (`heap/mod.rs`, outside `verus!`: raw writes of a fresh
// metadata block, and the walk of its node list). Each claims only that
// the call returns.

/// A heap homed on `arena`, null when the metadata region is exhausted.
pub assume_specification[ crate::heap::Heap::create_detached ](shared: &crate::heap::Shared, arena: u16) -> *mut crate::heap::Heap;

/// Flush and free heap `h`. The caller guarantees `h` came from
/// `Heap::create_detached` on `shared` and is no longer in use.
pub assume_specification[ crate::heap::Heap::destroy ](h: *mut crate::heap::Heap, shared: &crate::heap::Shared);

// `huge/mod.rs`, `heap/` and `meta.rs` functions the routing calls.
// Each claims only what its body shows.

/// The sized free's abort for a slab pointer whose class is not the
/// hint's (`sdallocx`, `free_sized`); claims nothing (it may abort; the
/// check formats a panic message, outside `verus!`).
pub assume_specification[ crate::heap::Shared::check_size_hint ](shared: &crate::heap::Shared, p: *mut u8, size: usize, hint: Option<usize>);

} // verus!

verus! {

// `class/` functions outside `verus!` that the heap's claims and direct
// releases call. Delete each when its function enters `verus!`.

/// Release a flushed batch of `class` slots through their owning arenas
/// (`heap/shared.rs`, outside `verus!`: a formatted abort and a loop whose
/// progress rests on `ClassState::release_owned`'s count); aborts on an
/// arena that does not exist, a pointer that is not an allocated slot, or
/// a slab that changes arena under the flush. `class` indexes the arena's
/// class states.
pub assume_specification[ crate::heap::Shared::release_routed ](shared: &crate::heap::Shared, arena: u16, class: usize, ptrs: &[*mut u8])
    requires
        class < crate::layout::NCLASSES,
;

} // verus!

verus! {

/// `Option::or_else`: `option` itself when it is `Some`, else `f()`.
pub assume_specification<T, F: FnOnce() -> Option<T>>[ Option::<T>::or_else ](option: Option<T>, f: F) -> (r: Option<T>)
    requires
        option is None ==> f.requires(()),
    ensures
        option is Some ==> r == option,
        option is None ==> f.ensures((), r),
;

} // verus!

verus! {

// Phase A of the port (verus/TODO.md, "To restore"): `Shared`'s
// accessors are outside `verus!`.
// Each specification claims only that the call returns.

/// The decay period of the huge allocator's empty runs, nanoseconds.
pub assume_specification[ crate::heap::Shared::huge_decay_ns ](shared: &crate::heap::Shared) -> u64;

} // verus!
