//! Build script: the shared library's soname (design 13.6). The plain build
//! is `libchalloc.so.0`, so a program linked with `-lchalloc` records a
//! versioned name in `DT_NEEDED` and a distro package can install the file
//! under it; the `jemalloc-soname` feature gives it jemalloc's soname,
//! `libjemalloc.so.2` (design 13.2), which is what link-only jemalloc
//! consumers (Ruby, KeyDB, ...) and a `challoc-jemalloc-compat` package
//! need. Cargo cannot rename the output file itself, so the file is always
//! `libchalloc.so`; `scripts/jemalloc-compat.sh` (`make jemalloc-compat`)
//! and `contrib/valkey/build.sh` put the soname-named link beside it.
//! Nothing else happens here: no code generation, no probing, and the flag
//! only applies to the cdylib, never the staticlib.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let soname = if std::env::var_os("CARGO_FEATURE_JEMALLOC_SONAME").is_some() {
        "libjemalloc.so.2"
    } else {
        "libchalloc.so.0"
    };
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,{soname}");
}
