# Phase 3 task 1: the Wolfi package definition, first real build

Date 2026-09-20. Lane P (packaging; nothing under `src/` changed), worktree `../challoc-wt/lane-p`, branch `loop/lane-p` from `main` 4a7d30c. Definition: `contrib/wolfi/challoc.yaml`; recipe and layout: `contrib/wolfi/README.md`.

## Where and with what

- VMs: `challoc-lane-p-arm64` (c4a-standard-16, Neoverse-V2) and `challoc-lane-p-amd64` (c4-standard-16, Xeon Platinum 8581C), Ubuntu 24.04, kernel `7.0.0-1011-gcp`, Docker 29.1.3, zone us-east4-a; created, provisioned, used and deleted within the iteration.
- melange: Wolfi's `melange` apk in a `--privileged` `cgr.dev/chainguard/wolfi-base` container, `--runner bubblewrap`; the binary reports `melange v0.60.0+dirty`. Wolfi `pipelines/` (for `test/tw/ldd-check`, `test/pkgconf`) from wolfi-dev/os at `a944f08`.
- Build environment (from the melange log, identical on both architectures except the arch): `rust-1.98=1.98.1-r0` (`rustc 1.98.1 (48a229cea 2026-09-01)`, `cargo 1.98.1`), `gcc=16.2.0-r1`, `binutils=2.47-r1`, `glibc-2.44=2.44-r6`, `glibc-2.44-dev=2.44-r6`, `build-base=1-r9`, `busybox=1.38.0-r2`, `wolfi-base=1-r7`, `libLLVM-22=22.1.8-r4`, `pkgconf=3.0.7-r0`, `apk-tools=2.14.10-r14`.
- Source: the synced working tree at the YAML's commit, built with `--source-dir /work --build-option local-source` (the `git-checkout` step is skipped: the repository has no tag and is not public, see "Not done").
- Clean-container tests: `cgr.dev/chainguard/wolfi-base` with the local repository and the generated key added to `/etc/apk/keys`, Wolfi's repository still configured (so the `libjemalloc2` alternative is visible to apk).

## Built packages

`aarch64` (`/out/packages/aarch64/`):

```
-rw-r--r-- 1 root root   1263 Sep 20 12:14 APKINDEX.tar.gz
-rw-r--r-- 1 root root 198685 Sep 20 12:14 challoc-0.1.0-r0.apk
-rw-r--r-- 1 root root  12673 Sep 20 12:14 challoc-dev-0.1.0-r0.apk
-rw-r--r-- 1 root root 199189 Sep 20 12:14 challoc-jemalloc-compat-0.1.0-r0.apk
a20b3f4aa26d2c343ebc6e80e4e3f648989a0a66d09f42850d5e827f85e0246e  /packages/aarch64/challoc-0.1.0-r0.apk
003f398dc81b5ab86c392dd86ababb633e5ced27cc5f4e211666d21327b6627d  /packages/aarch64/challoc-dev-0.1.0-r0.apk
6429bfcf65f6bed4b2663995011e38ed2c942feb45a417701b9a37695c0b5dc4  /packages/aarch64/challoc-jemalloc-compat-0.1.0-r0.apk
```
Each cargo build (release, fat LTO) took about 8 s (aarch64): 8.04s, 8.02s.

`x86_64` (`/out/packages/x86_64/`):

```
-rw-r--r-- 1 root root   1251 Sep 20 12:14 APKINDEX.tar.gz
-rw-r--r-- 1 root root 194415 Sep 20 12:14 challoc-0.1.0-r0.apk
-rw-r--r-- 1 root root  12678 Sep 20 12:14 challoc-dev-0.1.0-r0.apk
-rw-r--r-- 1 root root 194941 Sep 20 12:14 challoc-jemalloc-compat-0.1.0-r0.apk
9a6377d1572664c893cd628def3d86210e7af64a3af7497822cac18941847a12  /packages/x86_64/challoc-0.1.0-r0.apk
a70e44d3fa41c801a8414761562f2cb3ea5e9c48bf01cc78c361daf3a32673b3  /packages/x86_64/challoc-dev-0.1.0-r0.apk
77aee2aac99719462c1e1e1914c0920b45c5e1c6ac48f08c213ed2b001d59ca6  /packages/x86_64/challoc-jemalloc-compat-0.1.0-r0.apk
```
Each cargo build (release, fat LTO) took about 8 s (x86_64): 8.13s, 7.90s.

melange's own linters: `no lint findings to persist` for all three packages on both architectures. Installed sizes (`apk info`): challoc 427 KiB / 410 KiB (aarch64 / x86_64), challoc-dev 48 KiB / 48 KiB, challoc-jemalloc-compat 432 KiB / 415 KiB.

Package metadata melange derived (aarch64; on x86_64 the same except that the `runtime:` lists of `challoc` and `challoc-jemalloc-compat` are `so:libc.so.6` alone, with no separate loader dependency):
```
generating package challoc-0.1.0-r0
  runtime:
    so:ld-linux-aarch64.so.1
    so:libc.so.6
  provides:
    so-ver:libchalloc.so.0=0.1.0-r0
    so:libchalloc.so.0=0
  installed-size: 438242
generating package challoc-dev-0.1.0-r0
  runtime:
    challoc=0.1.0-r0
    so:libchalloc.so.0
  provides:
    pc:challoc=0.1.0-r0
  installed-size: 49189
generating package challoc-jemalloc-compat-0.1.0-r0
  runtime:
    so:ld-linux-aarch64.so.1
    so:libc.so.6
  provides:
    libjemalloc2=0.1.0-r0
    pc:jemalloc=0.1.0-r0
    so-ver:libjemalloc.so.2=0.1.0-r0
    so:libjemalloc.so.2=2
  installed-size: 442452
```

## `melange test` (both architectures pass)

The three test environments (`challoc`, `challoc-dev`, `challoc-jemalloc-compat`) and their steps, from the aarch64 log; the x86_64 log has the same lines with `ld-linux-x86-64.so.2` in place of `ld-linux-aarch64.so.1`. Trace lines (`+ ...`) removed.
```
== wolfi-dev/os pipelines at a944f08
-- melange v0.60.0+dirty with runner bubblewrap is testing:
packages:     [binutils challoc ldd-check posix-libc-utils]
running the main test pipeline
running step "test/tw/ldd-check"
running step "check for missing library dependencies using ldd"
[ldd-check] Testing binaries in package challoc
PASS[ldd-check]: /usr/lib/libchalloc.so.0
INFO[ldd-check]: tested 1 files with ldd. 1 passes. 0 fails.
running step "soname, loadability and an LD_PRELOAD run"
0x000000000000000e (SONAME)             Library soname: [libchalloc.so.0]
linux-vdso.so.1 (0x0000eaf9874ec000)
libc.so.6 => /lib/libc.so.6 (0x0000eaf987260000)
/lib/ld-linux-aarch64.so.1 (0x0000eaf9874b0000)
running test pipeline for subpackage challoc-dev
-- melange v0.60.0+dirty with runner bubblewrap is testing:
packages:     [binutils build-base challoc-dev ldd-check pkgconf]
running step "test/tw/ldd-check"
running step "check for missing library dependencies using ldd"
[ldd-check] Testing binaries in package challoc
PASS[ldd-check]: /usr/lib/libchalloc.so.0
INFO[ldd-check]: tested 1 files with ldd. 1 passes. 0 fails.
running step "test/pkgconf"
running step "pkgconf build dependency check"
0.1.0
-Wl,--push-state,--no-as-needed -lchalloc -Wl,--pop-state
running step "Link a program with pkg-config's flags and check DT_NEEDED"
0x0000000000000001 (NEEDED)             Shared library: [libchalloc.so.0]
0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
challoc_version=0.1.0 usable=112 mallctl.version=5.3.0-0-gchalloc
running test pipeline for subpackage challoc-jemalloc-compat
-- melange v0.60.0+dirty with runner bubblewrap is testing:
packages:     [binutils build-base challoc-jemalloc-compat ldd-check pkgconf]
running step "test/tw/ldd-check"
running step "check for missing library dependencies using ldd"
[ldd-check] Testing binaries in package challoc-jemalloc-compat
PASS[ldd-check]: /usr/lib/libjemalloc.so
PASS[ldd-check]: /usr/lib/libjemalloc.so.2
INFO[ldd-check]: tested 2 files with ldd. 2 passes. 0 fails.
running step "test/pkgconf"
running step "pkgconf build dependency check"
5.3.0
-ljemalloc
-I/usr/include/jemalloc
running step "The library carries jemalloc's soname and is challoc"
running step "A link-only jemalloc consumer resolves mallocx into the compat library"
0x0000000000000001 (NEEDED)             Shared library: [libjemalloc.so.2]
0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
mallocx from /lib/libjemalloc.so.2
malloc from /lib/libjemalloc.so.2
version 5.3.0-0-gchalloc
== melange test exit 0
== docker exit 0
```
x86_64: `melange test exit 0`; every step above appears with the same result (`.tmp-logs/amd64/melange-test-x86_64.log` on the host at the time of writing; the log is not committed).

## Clean-container installation and the checks by hand

aarch64 (`apk-test-aarch64.log`), verbatim except the busybox `ls` of `/`:
```
== apk add challoc challoc-dev challoc-jemalloc-compat (+ test tools)
== installed
challoc-dev: 
challoc-dev: 48 KiB
challoc-dev: challoc headers, link name and pkg-config file
challoc-jemalloc-compat: 
challoc-jemalloc-compat: 432 KiB
challoc-jemalloc-compat: challoc under jemalloc's soname (libjemalloc.so.2) and jemalloc.pc, for link-only jemalloc consumers
challoc: 
challoc: 427 KiB
challoc: A memory-safe, jemalloc-compatible malloc replacement for Wolfi and Chainguard OS
== files
challoc-0.1.0-r0 contains:
usr/lib/libchalloc.so.0
var/lib/db/sbom/challoc-0.1.0-r0.spdx.json
challoc-dev-0.1.0-r0 contains:
usr/include/challoc/jemalloc.h
usr/lib/libchalloc.so
usr/lib/pkgconfig/challoc.pc
var/lib/db/sbom/challoc-dev-0.1.0-r0.spdx.json
challoc-jemalloc-compat-0.1.0-r0 contains:
usr/lib/libjemalloc.so
usr/lib/libjemalloc.so.2
usr/lib/pkgconfig/jemalloc.pc
var/lib/db/sbom/challoc-jemalloc-compat-0.1.0-r0.spdx.json
-rw-r--r--    1 root     root          4123 Jan  1  1970 /usr/include/challoc/jemalloc.h
lrwxrwxrwx    1 root     root            15 Sep 20 12:16 /usr/lib/libchalloc.so -> libchalloc.so.0
-rwxr-xr-x    1 root     root        405840 Jan  1  1970 /usr/lib/libchalloc.so.0
lrwxrwxrwx    1 root     root            16 Sep 20 12:16 /usr/lib/libjemalloc.so -> libjemalloc.so.2
-rwxr-xr-x    1 root     root        405440 Jan  1  1970 /usr/lib/libjemalloc.so.2
-rw-r--r--    1 root     root           333 Jan  1  1970 /usr/lib/pkgconfig/challoc.pc
-rw-r--r--    1 root     root           386 Jan  1  1970 /usr/lib/pkgconfig/jemalloc.pc
== provides/depends
challoc-0.1.0-r0 depends on:
so:ld-linux-aarch64.so.1
so:libc.so.6
challoc-0.1.0-r0 provides:
so-ver:libchalloc.so.0=0.1.0-r0
so:libchalloc.so.0=0
challoc-dev-0.1.0-r0 depends on:
challoc=0.1.0-r0
so:libchalloc.so.0
challoc-dev-0.1.0-r0 provides:
pc:challoc=0.1.0-r0
challoc-jemalloc-compat-0.1.0-r0 depends on:
so:ld-linux-aarch64.so.1
so:libc.so.6
challoc-jemalloc-compat-0.1.0-r0 provides:
libjemalloc2=0.1.0-r0
pc:jemalloc=0.1.0-r0
so-ver:libjemalloc.so.2=0.1.0-r0
so:libjemalloc.so.2=2
== readelf -d soname
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
 0x000000000000000e (SONAME)             Library soname: [libchalloc.so.0]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
 0x000000000000000e (SONAME)             Library soname: [libjemalloc.so.2]
== ldd
	linux-vdso.so.1 (0x0000ee2596477000)
	libc.so.6 => /usr/lib/libc.so.6 (0x0000ee25961e0000)
	/lib/ld-linux-aarch64.so.1 (0x0000ee2596430000)
	linux-vdso.so.1 (0x0000eb510f898000)
	libc.so.6 => /usr/lib/libc.so.6 (0x0000eb510f600000)
	/lib/ld-linux-aarch64.so.1 (0x0000eb510f850000)
== LD_PRELOAD runs
  Type:                              DYN (Shared object file)
challoc.pc
jemalloc.pc
libcrypt.pc
libxcrypt.pc
== pkg-config
5.3.0
-I/usr/include/jemalloc -ljemalloc
atleast 5.3 ok
0.1.0
-Wl,--push-state,--no-as-needed -lchalloc -Wl,--pop-state
== tiny C program linked with $(pkg-config --libs jemalloc)
mallocx from /usr/lib/libjemalloc.so.2
malloc  from /usr/lib/libjemalloc.so.2
mallctl version 5.3.0-0-gchalloc
== scripts/check-needed.sh on it (design 13.6)
/tmp/compat: NEEDED libjemalloc.so.2 libc.so.6 ld-linux-aarch64.so.1 
  ok      libjemalloc.so.2
exit 0
== tests/c/jemalloc_link.c linked with pkg-config flags (the Phase 1 task 6 consumer), libdir /usr/lib
ok jemalloc_link
exit 0
== challoc-dev: link with $(pkg-config --libs challoc), a program referencing nothing
/tmp/none: NEEDED libchalloc.so.0 libc.so.6 
  ok      libchalloc.so.0
exit 0
== and the same with a plain -lchalloc (Wolfi gcc defaults to --as-needed: expected MISSING)
/tmp/none2: NEEDED libc.so.6 
  MISSING libchalloc.so.0 (linked with --as-needed and no symbol of it referenced? see design 13.6)
exit 1
== challoc_version through the header
challoc_version=0.1.0 mallctl=5.3.0-0-gchalloc
== co-installation with Wolfi jemalloc: apk add libjemalloc2 while challoc-jemalloc-compat is installed
OK: 581 MiB in 44 packages
exit 0
== apk add jemalloc-dev while challoc-jemalloc-compat is installed
               world[libjemalloc2]
               jemalloc-dev-5.3.1-r3[so:libjemalloc.so.2]
  jemalloc-dev-5.3.1-r3:
    conflicts: challoc-jemalloc-compat-0.1.0-r0[pc:jemalloc=5.3.1-r3]
    satisfies: world[jemalloc-dev]
exit 0
== done
== docker exit 0
== fresh container 2: apk add ruby-3.4 with both providers of so:libjemalloc.so.2 in the repositories
-- provider apk picked:
libjemalloc2-5.3.1-r3
/usr/lib/libjemalloc.so.2 is owned by libjemalloc2-5.3.1-r3
3.4.10
-- swap: apk add challoc-jemalloc-compat on top
(1/2) Purging libjemalloc2 (5.3.1-r3)
(2/2) Installing challoc-jemalloc-compat (0.1.0-r0)
OK: 59 MiB in 22 packages
exit 0
challoc-jemalloc-compat-0.1.0-r0
/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
3.4.10
/usr/lib/libjemalloc.so.2
-- readelf DT_NEEDED of libruby (Ruby links -ljemalloc with --no-as-needed)
sh: line 10: readelf: not found   (my script forgot binutils in that container; the DT_NEEDED of libruby was checked in the melange test of the consumer shape instead, see below)
== fresh container 3: apk add challoc-jemalloc-compat ruby-3.4 together
exit 0
challoc-jemalloc-compat-0.1.0-r0
/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
3.4.10
-- and apk add jemalloc-dev on top (expected: conflict, they are alternatives)
               ruby-3.4-3.4.10-r7[so:libjemalloc.so.2]
               jemalloc-dev-5.3.1-r3[so:libjemalloc.so.2]
  jemalloc-dev-5.3.1-r3:
    conflicts: challoc-jemalloc-compat-0.1.0-r0[pc:jemalloc=5.3.1-r3]
    satisfies: world[jemalloc-dev]
```

x86_64 (`apk-test-x86_64.log`): identical apart from sizes and the loader name; the lines that carry a result:
```
 0x000000000000000e (SONAME)             Library soname: [libchalloc.so.0]
 0x000000000000000e (SONAME)             Library soname: [libjemalloc.so.2]
5.3.0
-I/usr/include/jemalloc -ljemalloc
atleast 5.3 ok
0.1.0
-Wl,--push-state,--no-as-needed -lchalloc -Wl,--pop-state
mallocx from /usr/lib/libjemalloc.so.2
malloc  from /usr/lib/libjemalloc.so.2
mallctl version 5.3.0-0-gchalloc
  ok      libjemalloc.so.2
ok jemalloc_link
  ok      libchalloc.so.0
== and the same with a plain -lchalloc (Wolfi gcc defaults to --as-needed: expected MISSING)
  MISSING libchalloc.so.0 (linked with --as-needed and no symbol of it referenced? see design 13.6)
challoc_version=0.1.0 mallctl=5.3.0-0-gchalloc
    conflicts: challoc-jemalloc-compat-0.1.0-r0[pc:jemalloc=5.3.1-r3]
/usr/lib/libjemalloc.so.2 is owned by libjemalloc2-5.3.1-r3
3.4.10
(1/2) Purging libjemalloc2 (5.3.1-r3)
(2/2) Installing challoc-jemalloc-compat (0.1.0-r0)
/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
3.4.10
/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
3.4.10
    conflicts: challoc-jemalloc-compat-0.1.0-r0[pc:jemalloc=5.3.1-r3]
```

## Headline results

- All three `.apk`s build from this tree with Wolfi's Rust 1.98 on aarch64 and x86_64; melange's linters are clean.
- `readelf -d`: `Library soname: [libchalloc.so.0]` on `/usr/lib/libchalloc.so.0`; `Library soname: [libjemalloc.so.2]` on `/usr/lib/libjemalloc.so.2`; each has only `libc.so.6` and the loader as `NEEDED`.
- `LD_PRELOAD=/usr/lib/libchalloc.so.0 ls` and `readelf` run; `LD_PRELOAD=/usr/lib/libjemalloc.so.2 ls` runs.
- `pkg-config --modversion jemalloc` = `5.3.0`, `--libs` = `-ljemalloc`, `--atleast-version=5.3` ok; `pkg-config --modversion challoc` = `0.1.0`.
- A C program with strong `mallocx`/`dallocx`/`mallctl` references linked with `$(pkg-config --libs jemalloc)` has `DT_NEEDED libjemalloc.so.2`, resolves `mallocx` and `malloc` into `/usr/lib/libjemalloc.so.2` (`dladdr`), and `mallctl("version")` reports `5.3.0-0-gchalloc`; `scripts/check-needed.sh <binary> libjemalloc.so.2` says `ok`. `tests/c/jemalloc_link.c` (the Phase 1 task 6 consumer) passes against the installed package: `ok jemalloc_link`.
- The as-needed rule (design 13.6): with `challoc.pc`'s `Libs` a program that references nothing from the allocator keeps `DT_NEEDED libchalloc.so.0` (`check-needed.sh`: `ok`); with a plain `-lchalloc` Wolfi's gcc drops it (`MISSING`), the failure the pc file's `--push-state,--no-as-needed` exists for.
- Swapping: with both repositories visible, `apk add ruby-3.4` installs Wolfi's `libjemalloc2`; `apk add challoc-jemalloc-compat` purges it and Ruby 3.4.10 runs with `/usr/lib/libjemalloc.so.2` owned by `challoc-jemalloc-compat`; `apk add challoc-jemalloc-compat ruby-3.4` together picks the compat library directly; `apk add jemalloc-dev` on top is refused (`conflicts: challoc-jemalloc-compat[pc:jemalloc]`).

## Not done, and why

- The `git-checkout` step of the pipeline was not exercised: `github.com/chainguard-dev/challoc` is not reachable (`gh repo view`: could not resolve) and the repository has no release tag, so the YAML's `expected-commit` is the `main` commit it was written against (4a7d30c) with `tag: v${{package.version}}` for the Wolfi PR to set. The local-source build (`--build-option local-source`) is what ran.
- No numbers: this is packaging; the library built here is the same `quantum16` release build the scorecard measures, plus `strip`.
- Not run in Wolfi CI (Phase 3's acceptance is "packages build and pass tests in Wolfi CI"; that happens in the wolfi-dev/os repository with this file).
- `melange test` for the `challoc` package ran `test/tw/ldd-check` and the `runs:` block; the `ldd-check` pipeline's own package list defaulted to `challoc` in the `challoc-dev` test too (as written, `packages: challoc`), which is intended: the -dev package's link name is checked by the `readlink` line.

