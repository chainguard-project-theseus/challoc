//! The `core::panic` items the panic handler (`lib.rs`) reads: the
//! `PanicInfo` it is handed, the panic's `Location` and its message.
//! Each specification claims only what the signature says: the calls
//! have no precondition and return. One axiom says more, of the crate's
//! panic sites: that the message formats ([`axiom_fmt_req_all_panic_message`]).

use vstd::prelude::*;

verus! {

#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExPanicInfo<'a>(core::panic::PanicInfo<'a>);

#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExLocation<'a>(core::panic::Location<'a>);

#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExPanicMessage<'a>(core::panic::PanicMessage<'a>);

/// `PanicInfo::location`: where the panic was raised, if known; nothing
/// is claimed about it. (Verus compares the signature with the
/// returned reference's lifetimes erased, which it spells `'static`.)
pub assume_specification<'a>[ core::panic::PanicInfo::<'a>::location ](info: &core::panic::PanicInfo<'a>) -> Option<&'static core::panic::Location<'static>>;

/// `PanicInfo::message`: the panic's message; nothing is claimed about
/// it.
pub assume_specification<'a, 'b>[ core::panic::PanicInfo::<'a>::message ](info: &'b core::panic::PanicInfo<'a>) -> core::panic::PanicMessage<'b>;

/// `Location::file`: the source file's name.
pub assume_specification<'a>[ core::panic::Location::<'a>::file ](loc: &core::panic::Location<'a>) -> &'a str;

/// `Location::line`: the line number.
pub assume_specification<'a>[ core::panic::Location::<'a>::line ](loc: &core::panic::Location<'a>) -> u32;

/// A panic's message formats under any formatter (`Display`'s
/// precondition, for `format_args!`). Trusted, of every panic site in
/// the crate: the message's `Display` writes the `fmt::Arguments` the
/// panic was raised with, so the arguments' own `fmt`s run here, in the
/// handler; the panics challoc can raise (the `panic!`, `assert!`,
/// `unwrap` and `expect` of its trusted bodies, and `core`'s own) format
/// `&str`s, integers and types whose `fmt_req` is true, never a crate
/// type with a formatting precondition (such as `sys::features`'
/// `LgSize`, formatted only by the verified `SizeSet::fmt`).
pub broadcast axiom fn axiom_fmt_req_all_panic_message<'a>()
    ensures
        #[trigger] vstd::std_specs::fmt::fmt_req_all::<core::panic::PanicMessage<'a>>(),
;

} // verus!
