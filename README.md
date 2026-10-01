# challoc

A memory-safe, jemalloc-compatible `malloc` for Wolfi and Chainguard OS.

challoc is a general-purpose allocator written in `no_std` Rust that
exports the C allocation functions, the glibc extras and jemalloc's
extended API (`mallocx`, `sdallocx`, `mallctl`, `malloc_stats_print`,
`MALLOCX_*`), so a program built for jemalloc runs on it unchanged. It is
meant to be the allocator Wolfi packages such as Valkey and Ruby link
instead of jemalloc. Three things set it apart:

- **Hardening.** No allocator metadata lives in user memory, every free is
  validated (an invalid or double free aborts), size classes live in
  separate randomised address ranges, and guard slabs and sealed gaps are
  used where the kernel has them (`docs/hardening.md`).
- **cgroup awareness.** challoc reads its memory cgroup's effective limit
  and pressure, and returns freed memory faster and with the right advice
  as usage nears the limit (`docs/tuning.md` section 4).
- **Runtime feature probing.** One binary runs on the floor below and uses
  whatever newer kernels and CPUs offer, found at init, never at build time.

`docs/overview.md` has the goals, the case against staying on jemalloc and
a few pages on how it works; `docs/design.md` is the full design.

## Status

challoc 0.1.0 is not released. On the reference machines (Google Axion
and Intel Xeon VMs) it is at parity with jemalloc 5.3 on throughput and
returns memory better under cgroup limits: in a `docker run
--memory=512m` container, 400 MiB allocated and freed at 90% of the limit
is back to 13 MiB of RSS within 50 ms (jemalloc 5.3.1 still holds 190 MiB
2 s later), and a live
set swinging between 200 and 480 MiB runs for 5 minutes where jemalloc and
mimalloc are OOM-killed within 12 s (`docs/tuning.md` section 4).

Known regression: on the 16-thread Ruby soak on x86_64, challoc's peak RSS
is 12.9% above jemalloc's and its final RSS is 163 MiB against 132 MiB
(`bench/results/2026-09-21-x86_64-ruby-rss-v3-soak.md`; aarch64 is 5.7%
below jemalloc at the peak). `narenas:1` brings the x86_64 peak to within
1% of jemalloc's, at a higher final RSS.

The test suite (unit tests, C programs under `LD_PRELOAD`, the vendored
glibc and mimalloc suites, cgroup container tests, a fuzz driver) runs in
the dev container (`docs/testing.md`), and Wolfi's Valkey 9.1 and Ruby 3.4
are built and tested on challoc on both architectures (`make valkey`,
`make ruby`, `contrib/wolfi/`).

Tagged pointers (`tag_pointers`, MTE or LAM) are designed (design section
18) and wait for hardware that exposes them.

## Support

challoc supports Chainguard OS and Wolfi containers with glibc 2.44 or
newer, on host kernels 5.14 and newer, on x86_64 and on aarch64 with 4,
16 or 64 KiB pages. Newer kernels and CPUs turn features on at run time (the
table below); they are never required, and a missing feature changes
cost, memory or detection, never allocation results. Other distributions,
libcs and operating systems are not supported.

## Building

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
pointer's class, and `malloc_usable_size`, `realloc` and `sallocx` abort on
a freed or foreign pointer, where jemalloc's behaviour is undefined
(`docs/hardening.md`).

## Kernel and CPU features

Every feature is probed at init and reported as one line by
`challoc_features()` and by `malloc_stats_print`'s `Kernel and CPU
features:` (`docs/design.md` section 17).

| Feature                      | Provided by | Used for                                   | Fallback                          |
|------------------------------|--------|--------------------------------------------|-----------------------------------|
| `MADV_GUARD_INSTALL`         | kernel 6.13   | Guard slabs and guards after huge runs     | `PROT_NONE` page after huge runs; no guard slabs |
| `process_madvise` on self    | kernel 6.13 (`MADV_DONTNEED` through it, with a pidfd of self; 5.10..6.12 refuse the advice), 6.15 (`PIDFD_SELF`); hidden by Docker's default seccomp | Batched purge                              | Loop over `madvise`               |
| `process_madvise(MADV_FREE)` on self | the same, probed separately (`process_madvise_free`) | Batched lazy purge under 70% of a cgroup limit | Lazy batches loop over `madvise(MADV_FREE)` |
| `PR_SET_VMA_ANON_NAME`       | kernel 5.17   | Named heap mappings in `/proc/pid/maps`    | Skip                              |
| `mseal`                      | kernel 6.10   | Seals the never-committed parts of the reservations (the gaps between class regions, the slack around the huge and metadata regions) at init; not the regions or the region table | Skip (also when seccomp filters it, as Docker's default profile does) |
| PSI triggers                 | kernel 6.4 (unprivileged) | Background thread arms `some 150000 1000000` on the cgroup's `memory.pressure` (or the unprivileged `some 300000 2000000`); a fire takes a sample, re-evaluates the policy and runs a decay pass at once | Sample `memory.current` on the tick; also when the write is refused (Docker's default profile mounts the cgroup read-only: `EROFS`; with `nsdelegate` on the host a container's root cgroup refuses the write: `EPERM`) |
| Transparent huge pages       | any kernel with THP; per-size knobs since 6.8 | Opt-in: `thp:auto` advises 64 KiB folios on the slab groups and metadata where the host enables `hugepages-64kB` without `madvise`-gating 2 MiB pages (never a 2 MiB page, so fork stays cheap); `thp:always` commits and advises 2 MiB chunks (at 4 KiB pages; the folio path at 16 and 64 KiB, whose PMD is 32 and 512 MiB), huge runs and metadata; `thp:never` issues `MADV_NOHUGEPAGE` on the class superblock and the metadata region, so the slab regions get no huge page on a host whose global mode is `always`, and leaves the huge region (allocations of a page or more) to the host's policy, as jemalloc's `never` leaves its large allocations (the `thp:` report line says `nohuge 2 (classes, metadata; huge follows host)`); the report's `thp:` line names the host's global mode | Off (`thp:never`, the default) |
| rseq time slice extension  | kernel 7.0 with `CONFIG_RSEQ_SLICE_EXTENSION` (x86_64; arm64 lacks its dependencies through master), glibc 2.44 | A lock holder asks the kernel to defer a preemption until the release: `slice_ctrl.request` set after the acquire and cleared before the release, `rseq_slice_yield` after a granted release (`slice_ext`: `auto`, `on`, `off`; design 17). The feature line's `slice_ext=` key gives the kernel's answer and what is in force: `on`, `available` (the kernel has it, the key turned it off), `kernel-disabled` (the option compiled out: Ubuntu's 7.0 GCE kernels), `no-slice_ctrl` (before 7.0), `no-rseq`, `prctl-failed` | The lock without the request (one relaxed load per hold); `Slice extension: off (<why>)` in the report and the same word after `slice_ext=` in the feature line |
| PAC `pac`                    | arm64 CPU (Armv8.3) and kernel, `AT_HWCAP` | Opt-in signed cache pointers (`pac_pointers`) | Plain pointers |

Chainguard OS ships Linux 6.18, so every kernel row above except the rseq
time slice extension is available there. Containers run on whatever
kernel the host has; note that a kernel before 6.13 (Docker Desktop's
6.10, for one) has no guard slabs, and that Docker's default seccomp
profile hides `mseal` and `process_madvise`.

## Documentation

- `docs/overview.md`: goals, why not jemalloc, how it works
- `docs/design.md`: the design, section by section
- `docs/tuning.md`: the operator's guide (keys, consumers, cgroup policy, THP, the report)
- `docs/hardening.md`: security properties against jemalloc, and the windows the defaults leave
- `docs/testing.md`: the test suite and how to add a test
- `docs/benchmarking.md`: how numbers are produced; `bench/README.md` for the harness
- `docs/research.md`: the survey behind the design decisions
- `CONTRIBUTING.md`, `SECURITY.md`

## License

challoc is licensed under the Apache License, Version 2.0 (`LICENSE`).
Vendored test suites and the jemalloc API layout of the compatibility
headers carry their own licences, listed in `NOTICE`.
