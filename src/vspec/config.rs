//! Specifications for `config.rs`: `char::to_digit`.

use vstd::prelude::*;

use crate::config::to_digit_spec;

verus! {

/// `char::to_digit` (`core` documentation): the digits are `0`..`9`, then
/// `a`..`z` and `A`..`Z` for 10..35, and only those below `radix` count;
/// panics for a radix outside `2..=36`.
pub assume_specification[ char::to_digit ](c: char, radix: u32) -> (r: Option<u32>)
    requires
        2 <= radix <= 36,
    ensures
        r == to_digit_spec(c as u32, radix),
;

} // verus!
