//! What a `mallctl` call changed of the allocator state (ghost),
//! [`Change`]: recorded by the function that makes the change, so the
//! record moves with the call that makes it (`api::mallctl`'s `ctl`).

use vstd::prelude::*;

verus! {

/// What a `mallctl` call changed of the allocator state the `mallctl`
/// keys set (ghost): the settings (`background_thread`'s request,
/// `arena.<i>.dirty_decay_ms`, `thread.tcache.enabled`, `thread.arena`)
/// and the explicit arenas and tcaches it creates (`tcache.destroy`
/// reports none). Each setter and creator, and `stats::epoch`, records
/// its change into the caller's record as it makes it.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost enum Change {
    /// Nothing of it.
    Unchanged,
    /// A setting took the key's new value; `read_first`: the call's read
    /// through `oldp`, if it asked for one, had been made before
    /// (`CCall::written` of the call's token when the setter ran), so it
    /// got the old value.
    Set { read_first: bool },
    /// `arenas.create` or `tcache.create` created an arena or a tcache.
    Created,
    /// `epoch`'s write refreshed the stats (before its read, as jemalloc's
    /// `epoch_ctl`).
    Refreshed,
}

} // verus!
