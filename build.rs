//! Build script: the shared library's soname (design 13.6) and the C++
//! catch for the `nothrow_t` operators (design 13.1).
//!
//! The plain build is `libchalloc.so.0`, so a program linked with
//! `-lchalloc` records a versioned name in `DT_NEEDED` and a distro package
//! can install the file under it; the `jemalloc-soname` feature gives it
//! jemalloc's soname, `libjemalloc.so.2` (design 13.2), which is what
//! link-only jemalloc consumers (Ruby, KeyDB, ...) and a
//! `challoc-jemalloc-compat` package need. Cargo cannot rename the output
//! file itself, so the file is always `libchalloc.so`;
//! `scripts/jemalloc-compat.sh` (`make jemalloc-compat`) and
//! `tests/consumers/valkey-build.sh` put the soname-named link beside it.
//! The soname flag only applies to the cdylib, never the staticlib.
//!
//! `src/api/cxx_handler.cpp` is compiled with the C++ compiler and archived
//! into a static native library that rustc links into the cdylib and
//! bundles into the staticlib. The compiler is `CXX_<target>` (the target
//! triple with `-` as `_`), else `CXX`, else `c++` for a native build and
//! `<arch>-linux-gnu-g++` for a cross build; the archiver is `AR_<target>`,
//! else `AR`, else `ar` (GNU ar archives foreign ELF objects, so a cross
//! build needs no prefixed one). Each is a command line split at
//! whitespace, so
//! `CXX_aarch64_unknown_linux_gnu="clang++ --target=aarch64-linux-gnu"`
//! works. `CXXFLAGS` is not read: the flags are fixed here. The file
//! includes no headers and links against no C++ library. Nothing else
//! happens here: no code generation, no probing.

use std::env;
use std::path::PathBuf;
use std::process::Command;

const SHIM: &str = "src/api/cxx_handler.cpp";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let soname = if env::var_os("CARGO_FEATURE_JEMALLOC_SONAME").is_some() {
        "libjemalloc.so.2"
    } else {
        "libchalloc.so.0"
    };
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,{soname}");
    compile_shim();
}

/// `<name>_<target>`, then `<name>`, then `default`, as a command whose
/// program is the first word and whose leading arguments are the rest.
fn tool(name: &str, target: &str, default: String) -> Command {
    let per_target = format!("{name}_{}", target.replace('-', "_"));
    println!("cargo:rerun-if-env-changed={per_target}");
    println!("cargo:rerun-if-env-changed={name}");
    let line = env::var(&per_target)
        .or_else(|_| env::var(name))
        .unwrap_or(default);
    let mut words = line.split_whitespace();
    let mut cmd = Command::new(words.next().unwrap_or_else(|| panic!("{name} is empty")));
    cmd.args(words);
    cmd
}

fn run(cmd: &mut Command) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("cannot run {cmd:?}: {e}"));
    assert!(status.success(), "{cmd:?} failed: {status}");
}

fn compile_shim() {
    println!("cargo:rerun-if-changed={SHIM}");
    let target = env::var("TARGET").unwrap();
    let host = env::var("HOST").unwrap();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let default_cxx = if target == host {
        "c++".to_string()
    } else {
        format!("{arch}-linux-gnu-g++")
    };
    let mut cxx = tool("CXX", &target, default_cxx);
    let mut ar = tool("AR", &target, "ar".to_string());
    let obj = out.join("cxx_handler.o");
    let lib = out.join("libchalloc_cxx.a");
    run(cxx
        .args(["-std=c++17", "-O2", "-fPIC", "-fexceptions"])
        .args(["-Wall", "-Wextra", "-c", SHIM, "-o"])
        .arg(&obj));
    // `ar r` adds to an existing archive; start from nothing.
    let _ = std::fs::remove_file(&lib);
    run(ar.arg("crs").arg(&lib).arg(&obj));
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=challoc_cxx");
}
