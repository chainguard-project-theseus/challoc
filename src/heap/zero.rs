//! The zero tokens a heap hands out with a slot it reports zero: the
//! pop that took the slot from inside a zero prefix leaves the slot's
//! token in the heap's cache permissions, and its caller takes it.

use crate::vspec::zero::Zeroed;
use vstd::prelude::{Ghost, Tracked, int};

impl super::Heap {
    /// Hand out token `t` of slot `p` of class `c` when `put`, as a pop
    /// does. No code: ghost only.
    #[inline(always)]
    #[allow(dead_code)]
    pub(super) fn put_handed(
        &mut self,
        _t: Tracked<Zeroed>,
        _put: Ghost<bool>,
        _p: Ghost<*mut u8>,
        _c: Ghost<int>,
    ) {
    }

    /// Take the token the last pop handed out when `take` (an empty one
    /// otherwise). No code: ghost only.
    #[inline(always)]
    #[allow(dead_code)]
    pub fn take_handed(
        &mut self,
        _take: Ghost<bool>,
        _p: Ghost<*mut u8>,
        _c: Ghost<int>,
    ) -> Tracked<Zeroed> {
        Tracked::assume_new()
    }
}
