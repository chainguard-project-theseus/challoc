//! Off Linux there is no background thread: the stub with the names of
//! `background.rs`, so the host build of the crate type-checks.

use crate::heap::Shared;

pub fn present() -> bool {
    false
}

pub fn requested() -> bool {
    false
}

pub fn request(_on: bool) {}

pub fn wake() {}

pub fn start(_shared: &Shared) -> bool {
    false
}
