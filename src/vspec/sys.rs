//! Specifications for `sys`: the `core` byte and integer methods its
//! parsers and its error mapping call.

use vstd::prelude::*;

verus! {

/// `u8::is_ascii_digit`: `0`..`9`.
pub assume_specification[ u8::is_ascii_digit ](b: &u8) -> (r: bool)
    ensures r <==> 48 <= *b <= 57,
;

/// `isize::wrapping_neg`: `-x`, except that `isize::MIN` is its own
/// negation.
pub assume_specification[ isize::wrapping_neg ](x: isize) -> (r: isize)
    ensures r == if x == isize::MIN { isize::MIN as int } else { -x },
;

} // verus!
