//! Specifications of the derived `PartialEq` of three enums verified code
//! compares: `config::Thp` and `sys::thp_host::ThpSetting` (`thp.rs`), and
//! `huge::RunState`.

use vstd::prelude::*;

verus! {

// The derived `PartialEq` of these enums compares variants.

impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::config::Thp {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::sys::thp_host::ThpSetting {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::huge::RunState {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

} // verus!
