//! Build script. The `jemalloc-soname` feature gives the shared library
//! jemalloc's soname, `libjemalloc.so.2` (design 13.2), so a program linked
//! against a copy of it under that name records `libjemalloc.so.2` in its
//! `DT_NEEDED`, which is what link-only jemalloc consumers (Ruby, KeyDB, ...)
//! and a `challoc-jemalloc-compat` package need. Cargo cannot rename the
//! output file itself; `scripts/jemalloc-compat.sh` (`make jemalloc-compat`)
//! copies it into place. Nothing else happens here: no code generation, no
//! probing, and the flag only applies to the cdylib, never the staticlib.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_JEMALLOC_SONAME").is_some() {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libjemalloc.so.2");
    }
}
