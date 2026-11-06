//! Build script: sets `cfg(loom)` for this crate, so the `src/sync.rs` it
//! compiles (`src/vspec.rs`) re-exports loom's atomics, not `core`'s.

fn main() {
    println!("cargo::rustc-cfg=loom");
}
