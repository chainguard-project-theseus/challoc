# challoc

[![Verus: 82.2% of functions, 92.7% of lines](https://img.shields.io/badge/Verus-82.2%25%20of%20functions%2C%2092.7%25%20of%20lines-blue)](verus/README.md#coverage)

A memory-safe, jemalloc-compatible `malloc` for Wolfi and Chainguard OS.

challoc is a general-purpose allocator written in `no_std` Rust that
exports the C allocation functions, the glibc extras and jemalloc's
extended API (`mallocx`, `sdallocx`, `mallctl`, `malloc_stats_print`,
`MALLOCX_*`), so a program built for jemalloc runs on it unchanged. It is
meant to be the allocator Wolfi packages such as Valkey and Ruby link
instead of jemalloc. Three things set it apart:

- **Hardening.** No allocator metadata lives in user memory, every plain
  free is validated against the slot bitmap (frees of slots still held in
  a thread cache are checked exactly at the call only with
  `double_free_scan:true`; sized frees, which defer their check to a flush,
  are checked at the call only with `double_free_scan:true` or
  `exact_sized_free:true`),
  size classes live in
  separate randomised address ranges, and guard slabs and sealed gaps are
  used where the kernel has them (`docs/hardening.md`).
- **cgroup awareness.** challoc reads its memory cgroup's effective limit
  and usage, and returns freed memory faster and with the right advice
  as usage nears the limit (`docs/tuning.md` section 4).
- **Runtime feature probing.** One binary runs on the floor below and uses
  whatever newer kernels and CPUs offer, found at init, never at build time.

`docs/overview.md` has the goals, the case against staying on jemalloc and
a few pages on how it works; `docs/design.md` is the full design. Where
the project stands is under "Status" below.

## Formal verification

Verus verifies 85.2% of challoc's executable functions and 94.0% of its
executable lines. Counting also the trusted functions whose seams Kani,
a constant assertion, Isla or `make emptyasm` discharges gives 89.3% of
functions and 95.8% of lines. Counting instead the trusted functions
checked by loom models of the atomic protocol gives 87.5% and 95.0%;
those functions stay partly checked, not verified (main `8b2b5353`;
[verus/RESULTS.md](verus/RESULTS.md#headline-figures) has the figures
and the commands that measure them).

`theorem_metadata_integrity` proves that an allocated block shares no
byte with the allocator metadata it covers or the `Shared` static.

`theorem_no_syscall_on_fast_paths` proves that the covered `malloc` and
`free` fast paths make no system call except an integrity abort. The
`theorem_valid_*_free_never_aborts` family proves that freeing a live
block never reaches the invalid-free or double-free checks on its
covered paths.

The contracts of `malloc`, `calloc`, `mallocx` and the aligned
allocators rule out, on those entry points, the size-overflow bug
classes behind glibc, jemalloc and TCMalloc CVEs such as CVE-2013-4332
and CVE-2017-17426 ([list](verus/RESULTS.md#vulnerability-classes-ruled-out)).

These results rest on stated trust assumptions: the C caller obeys the
C allocation contract, and the seams below hold. [Verus
results](verus/RESULTS.md) groups the guarantees and states their
boundaries; [verus/THEOREMS.md](verus/THEOREMS.md) gives each theorem
with its premises.

Each tool covers less than the whole allocator, and CI runs each of
them ([verus/RESULTS.md](verus/RESULTS.md#check-it-yourself)):

- **Verus proofs.** Functions inside `verus! { ... }` blocks in `src/`
  are checked by [Verus](https://github.com/verus-lang/verus) against
  their stated contracts and for overflow, out-of-bounds indexing,
  reachable panics and termination, on every pull request. The badge
  above is the share of functions and lines Verus verifies, and
  `verus/FLOOR` keeps it from falling. Code outside `verus!` is not
  verified, and what a verified function relies on without proof
  (contracts for `core` functions in `src/vspec/`, `external_body`
  functions such as the syscall wrappers) is a *seam*, listed in a
  generated ledger. See [verus/README.md](verus/README.md) for what
  counts as verified and how to run it (`make verus`, which runs on the
  host with Verus installed rather than in the dev container), and
  [verus/SEAMS.md](verus/SEAMS.md) for the seams and their status.
- **Kani harnesses.** `src/kani/` (and `kani_*` modules next to the code
  they check) holds bounded model-checking harnesses that check some
  seams' bodies against the contract Verus assumes for them; each states
  its bounds. The `kani` check runs them on pull requests that change the
  files it lists, and `kani-ignoring` runs every harness after each
  landing.
- **Isla and `make emptyasm`.** `make isla` traces the compiled inline
  assembly (system calls, the TLS slot, pointer authentication, traps,
  the GOT functions and the rseq symbols) with
  [Isla](https://github.com/rems-project/isla) on Sail's Arm and x86
  models, and checks each run against its seam's contract
  ([verus/ISLA.md](verus/ISLA.md)). `make emptyasm` checks that the
  empty-template `asm!` register hints add no instruction. Every
  inline-assembly seam has one of these checks.
- **loom models.** `tests/loom/` checks copies of the atomics protocols
  (the background thread's wake-up and exit, `RawLock`, the slab bitmap,
  the free path's reads and others) under every interleaving and C11
  weak-memory outcome loom explores. They model the protocols, not the
  shipped code itself, so a seam they check stays partly checked;
  `tests/loom/tests/pins.rs` fails when the source lines a model copies
  change.

## Support

challoc supports Chainguard OS and Wolfi containers with glibc 2.44 or
newer, on host kernels 5.14 and newer, on x86_64 and on aarch64 with 4,
16 or 64 KiB pages. Newer kernels and CPUs turn features on at run time (the
table below); they are never required, and a missing feature changes
cost, memory or detection, never allocation results. A process needs
about 435 GiB of address space (`RLIMIT_AS`) at quantum 16, the minimum
layout of `docs/design.md` section 2; under a data limit
(`RLIMIT_DATA`) in force from the start, challoc makes no metadata
writable before it uses it. Other distributions, libcs and
operating systems are not supported.

## Building

Prerequisites: Docker is the only host prerequisite for building and
testing; the toolchain and every build and test dependency live in the
dev container. (The proof tools are separate; see "Formal verification".)

Everything builds and runs inside a Wolfi dev container (`Dockerfile.dev`),
whatever the machine running Docker:

```sh
make dev                       # shell in the Wolfi dev container, repo at /src
make build                     # cargo build --release
make run CMD='cargo test'      # any one command inside the container
make test                      # release build, then the full test suite
make jemalloc-compat           # libjemalloc.so.2 + jemalloc.pc in target/jemalloc-compat/stage
```

Add `QUANTUM=8` for the 8-byte size-class quantum Valkey needs, and
`PLATFORM=linux/amd64` for an x86_64 build on Apple silicon (emulated:
correctness only). `CONTRIBUTING.md` lists every variable.

The library is Rust plus one C++ file (`src/api/cxx_handler.cpp`), so a
build outside the container needs a C++ compiler as well as cargo.

## Installing and using

`make install` builds the library and installs it under `DESTDIR` +
`PREFIX` (default `dist/` + `/usr/local`; `LIBDIR` and `INCLUDEDIR` move
the parts). The paths are the container's, and `/src` is the repository,
so the default lands in `./dist`. The layout is the Wolfi package's:

```
$PREFIX/lib/libchalloc.so.0            the library (soname libchalloc.so.0)
$PREFIX/lib/libchalloc.so              link name
$PREFIX/lib/pkgconfig/challoc.pc
$PREFIX/include/challoc/challoc.h      challoc's own entry points
$PREFIX/include/challoc/jemalloc.h     the je_-prefixed jemalloc API
```

`make install-jemalloc-compat` installs the same allocator under
jemalloc's name for programs that only know how to link jemalloc:
`libjemalloc.so.2` (soname `libjemalloc.so.2`), the `libjemalloc.so` link
name, `jemalloc.pc` (version 5.3.0) and `<jemalloc/jemalloc.h>`.
`scripts/install.sh` does the work for both, and the Wolfi package
definitions are in `contrib/wolfi/`.

Use it one of three ways:

- **Preload** it into an unmodified program:
  `LD_PRELOAD=/usr/lib/libchalloc.so.0 program`.
- **Link** it: `cc ... $(pkg-config --cflags --libs challoc)`. The pc file
  wraps `-lchalloc` in `--no-as-needed`, because Wolfi's linker default
  `--as-needed` would otherwise drop it from a program that calls no
  allocator symbol by name, and the program would silently run on glibc's
  `malloc`.
- **Replace jemalloc** in a program that links `-ljemalloc` (Ruby's
  `--with-jemalloc`, KeyDB) by building or running it against the
  jemalloc-compat files.

`challoc_version()` is the `dlsym` probe for whether challoc is the loaded
allocator, and `challoc_features()` returns the feature line below.

**Valkey.** `contrib/valkey/` holds two patches against Valkey 9.1:
`make MALLOC=challoc` links the system challoc instead of the bundled
jemalloc and keeps every jemalloc code path (stats, `MEMORY PURGE`, the
background thread, active defrag), and the second patch moves Valkey's
memory accounting onto challoc's size-returning entry points. Valkey
needs a `QUANTUM=8` build (`allocator_defrag.c` asserts it);
`make valkey` builds and tests one, and `docs/tuning.md` section 3 covers
the deployment.

## Configuration

challoc reads jemalloc's `key:value,...` syntax from the `malloc_conf`
and `je_malloc_conf` globals, `/etc/challoc.conf`, `MALLOC_CONF` and
`CHALLOC_CONF` (which wins, so it can override a `MALLOC_CONF` written for
jemalloc). Unknown or invalid keys are reported on stderr, and
`abort_conf:true` makes them fatal. The defaults are the measured
configuration; `docs/tuning.md` is the operator's guide: every key, the
cgroup policy, transparent huge pages and how to read
`malloc_stats_print`.

## Compatibility

challoc exports the standard C functions including C23's `free_sized` and
`free_aligned_sized`; the glibc extras (`malloc_usable_size`,
`malloc_trim`, `mallopt`, `mallinfo2`, ...); the jemalloc surface
(`mallocx`, `rallocx`, `xallocx`, `sallocx`, `dallocx`, `sdallocx`,
`nallocx`, `mallctl`, `mallctlnametomib`, `mallctlbymib`,
`malloc_stats_print`), also under the `je_` prefix; and the C++
`operator new`/`operator delete` forms. Its own extension is
`<challoc/challoc.h>`: `challoc_malloc_usable`, `challoc_calloc_usable`,
`challoc_realloc_usable` and `challoc_free_usable` return the block's
usable size from the work the call already did (design 13.7).

It is stricter than jemalloc in two ways: a sized free must name the
pointer's class (for a huge run, its page count), and `malloc_usable_size`, `realloc` and `sallocx` abort on
a freed or foreign pointer, where jemalloc's behaviour is undefined
(`docs/hardening.md`).

## Kernel and CPU features

Every feature is probed at init and reported as one line by
`challoc_features()` and by `malloc_stats_print`'s `Kernel and CPU
features:` (`docs/design.md` section 17).

| Feature (report key)         | Provided by | Used for                                   | Fallback                          |
|------------------------------|-------------|--------------------------------------------|-----------------------------------|
| `AT_PAGESZ` (`page_size`)    | kernel      | Slab, mapping and purge granularity; 16 and 64 KiB pages give the 512..1024-byte classes page-sized slabs | None needed |
| `MADV_GUARD_INSTALL` (`guard_install`) | kernel 6.13 | Guard slabs and guards after huge runs | `PROT_NONE` page after huge runs; no guard slabs |
| `mseal` (`mseal`)            | kernel 6.10 | Seals the never-committed parts of the reservations (the gaps around class regions, the slack around the huge and metadata regions) at init | Skip (also when seccomp filters it, as Docker's default profile does) |
| `PR_SET_VMA_ANON_NAME` (`anon_name`) | kernel 5.17 | Named heap mappings in `/proc/pid/maps` | Skip |
| Transparent huge pages (`thp`, `mthp`) | any kernel with THP; per-size knobs since 6.8 | Opt-in through the `thp` key: `auto` advises 64 KiB folios (never a 2 MiB page), `always` 2 MiB chunks; the default `never` keeps the slab and metadata regions off huge pages even on a THP-`always` host and leaves the huge region to the host | Small pages |
| rseq registration (`rseq`, `rseq_size`) | glibc 2.44 | Reported only | `rseq=no` |
| Pointer authentication (`pac`) | arm64 CPU (Armv8.3) and kernel, `AT_HWCAP` | Opt-in signed cache pointers (`pac_pointers`) | Plain pointers |

Chainguard OS ships Linux 6.18, so every kernel row above is available
there. Containers run on whatever kernel the host has: a kernel before
6.13 (RHEL 9's 5.14, for one) has no guard slabs, and Docker's default
seccomp profile hides `mseal`.

## Documentation

- `docs/overview.md`: goals, why not jemalloc, how it works
- `docs/design.md`: the design, section by section
- `docs/tuning.md`: the operator's guide (keys, consumers, cgroup policy, THP, the report)
- `docs/hardening.md`: security properties against jemalloc, and the windows the defaults leave
- `docs/testing.md`: the test suite and how to add a test
- `docs/benchmarking.md`: how numbers are produced; `bench/README.md` for the harness
- `docs/research.md`: the survey behind the design decisions
- `CONTRIBUTING.md`, `SECURITY.md`

## Status

We're building challoc in the open at Chainguard. We don't publish it to
any package registry; it ships only from Chainguard's own repositories.

challoc 0.1.0 is not released. On the reference machines (Google Axion
and Intel Xeon VMs) it is at parity with jemalloc 5.3 on throughput and
returns memory better under cgroup limits: in a `docker run
--memory=512m` container, 400 MiB allocated and freed at about 80% of the
limit is back to 13 MiB of RSS within 50 ms (jemalloc 5.3.1 still holds 190 MiB
2 s later), and a live
set swinging between 200 and 480 MiB runs for 5 minutes where jemalloc and
mimalloc are OOM-killed within 12 s (`docs/tuning.md` section 4).

Known limitations are listed in `docs/tuning.md` section 8. The largest:
on a 16-thread Ruby soak on x86_64, challoc's peak RSS is about 13% above
jemalloc's (on aarch64 it is 6% below), and `narenas:1` brings the x86_64
peak close to jemalloc's. The extra RSS comes from partly used 64 KiB
slabs and empty slabs spread across many arenas; a fix is still open.

The test suite (unit tests, C programs under `LD_PRELOAD`, the vendored
glibc and mimalloc suites, cgroup container tests, a fuzz driver) runs in
the dev container (`docs/testing.md`), and Wolfi's Valkey 9.1 and Ruby 3.4
are built and tested on challoc on both architectures (`make valkey`,
`make ruby`, `contrib/wolfi/`).

Tagged pointers (a software tag in the ignored top bits, Arm MTE or
Intel LAM) are designed but not implemented (design section 18).

## Contributing

We don't accept pull requests from external contributors. If you and your
organization would seriously like to get involved, email
dlorenc@chainguard.dev.

Use of the challoc and Chainguard names is governed by Chainguard's
trademark policy.

## License

challoc is licensed under the Apache License, Version 2.0 (`LICENSE`).
Vendored test suites and the jemalloc API layout of the compatibility
headers carry their own licences, listed in `NOTICE`.
