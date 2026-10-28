//! The permission model for user memory: a [`UserMem`] is the permission
//! to a huge run's usable bytes.

use vstd::prelude::*;
use vstd::raw_ptr::PointsToRaw;

verus! {

/// Ghost permission to user memory: the bytes of `raw`.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct UserMem {
    tracked raw: PointsToRaw,
}

} // verus!
