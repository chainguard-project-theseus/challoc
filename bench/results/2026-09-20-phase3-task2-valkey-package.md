# Phase 3 task 2: Wolfi's Valkey 9.1 on challoc, first real build

Date 2026-09-20. Lane P (packaging; nothing under `src/` changed), worktree `../challoc-wt/lane-p`, branch `loop/lane-p` from `main` 5a1e715. Definition: `contrib/wolfi/valkey/valkey-9.1.yaml` (commit 6a14d5b) with `contrib/wolfi/valkey/valkey-9.1/{0000-Disable-protected-mode,0001-Add-MALLOC-challoc}.patch` and `contrib/wolfi/valkey/stage.sh`; layout and recipe: `contrib/wolfi/README.md`, "Valkey". No numbers in this file are benchmark results: the `valkey-benchmark` lines only show the package serving traffic.

## Base

- wolfi-dev/os `valkey-9.1.yaml` at `main` `a944f08e036412ab4ed83803c890347799af2f69` (2026-09-19T13:06Z); the file's last change is `195f812fd9c941876ea597b46ee43b1b3231339d` ("valkey-9.1/9.1.2 package update (#289261)", 2026-09-01). Wolfi's `valkey-9.1/0000-Disable-protected-mode.patch` copied from the same commit. Wolfi's `pipelines/` (for `test/tw/ldd-check`) from the same commit.
- Valkey tag `9.1.2`, commit `7f1dffedff6de73058b2c2a389422b6ecd56c8fb` (Wolfi's `expected-commit`, the one `contrib/valkey/build.sh` pins).
- challoc: this tree at 5a1e715 (`--build-option local-challoc`; the `git-checkout` of `github.com/chainguard-dev/challoc` in the definition carries that commit as `expected-commit` for the Wolfi PR to replace with the release tag).
- The diff against Wolfi's file (266 lines, `diff -u`) is at the end of this file.

## Where and with what

- VMs: `challoc-lane-p-arm64` (c4a-standard-16, Neoverse-V2) and `challoc-lane-p-amd64` (c4-standard-16, x86_64), Ubuntu 24.04, kernel `7.0.0-1011-gcp`, Docker 29.1.3, zone us-east4-a; created, provisioned, used and deleted within the iteration.
- melange: Wolfi's `melange` apk (`v0.60.0+dirty`) in a `--privileged` `cgr.dev/chainguard/wolfi-base` container, `--runner bubblewrap`, `--source-dir /tmp/stage --build-option local-challoc` (the staged directory from `stage.sh`: the two patches plus the challoc tree, 714 files, 25 MB). `melange test` with `--test-package-append=wolfi-base`, as wolfi-dev/os's Makefile passes it (line 105 at `a944f08`); without it the `-cli` and `-benchmark` test environments hold nothing but the package and bubblewrap fails with `execvp /bin/sh: No such file or directory`.
- Build environment (from the melange log, both architectures): `rust-1.98=1.98.1-r0` (`rustc 1.98.1 (48a229cea 2026-09-01)`, `cargo 1.98.1`), `gcc=16.2.0-r1`, `binutils=2.47-r1`, `glibc-2.44-dev=2.44-r6`, `openssl-dev=3.6.4-r7`, `linux-headers=7.2.6-r0`, `build-base=1-r9`, `busybox=1.38.0-r2`, `libLLVM-22=22.1.8-r4`.
- Clean-container checks: `cgr.dev/chainguard/wolfi-base` with the local repository and the generated key added, Wolfi's repository still configured (so `apk` can resolve the `so:` dependencies of the client tools).
- The challoc packages of task 1 were rebuilt first from the same tree (same YAML, `--build-option local-source`), so the local repository holds `challoc`, `challoc-dev`, `challoc-jemalloc-compat` 0.1.0-r0 next to the Valkey packages; the co-installation and both-ways checks below use the system `challoc` from it.

## Timing of the Valkey pipeline (aarch64; x86_64 within seconds of it)

```
12:53:20 running step "git-checkout"                 (Valkey 9.1.2)
12:53:22 running step "patch"                        (both patches)
12:53:22 running step "Build the private challoc (quantum8, soname libchalloc.so.0)"
12:53:22    Compiling challoc v0.1.0 (/home/build/challoc)
12:53:30     Finished `release` profile [optimized + debuginfo] target(s) in 8.20s
12:53:30 running step "Build Valkey against it"      (make MALLOC=challoc BUILD_TLS=yes all -j16; make install)
12:53:49 running step "Check the built server names the allocator and the private library"
12:53:49 running step "strip"
```

The check step's output, from the build log (aarch64):
```
Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=96a5eb4a877df797
/home/build/melange-out/valkey-9.1/usr/bin/valkey-server: NEEDED libchalloc.so.0 libm.so.6 libssl.so.3 libcrypto.so.3 libc.so.6 ld-linux-aarch64.so.1  (RPATH=/usr/lib/valkey )
  ok      libchalloc.so.0
```

Note `RPATH`: Wolfi's binutils 2.47 emits `DT_RPATH` for `-Wl,-rpath` (no `--enable-new-dtags` default). Design 13.6 says "RUNPATH"; the definition and its test accept either, and the both-ways check below shows what the difference buys.

## Built packages

melange's linters: `no lint findings to persist` for `valkey-9.1`, `valkey-9.1-cli`, `valkey-9.1-benchmark` on both architectures.

`aarch64` (`/out/packages/aarch64/`):
```
-rw-r--r--    1 root     root          1635 Sep 20 12:53 APKINDEX.tar.gz
-rw-r--r--    1 root     root        198914 Sep 20 12:53 challoc-0.1.0-r0.apk
-rw-r--r--    1 root     root         12744 Sep 20 12:53 challoc-dev-0.1.0-r0.apk
-rw-r--r--    1 root     root        199402 Sep 20 12:53 challoc-jemalloc-compat-0.1.0-r0.apk
-rw-r--r--    1 root     root       2103078 Sep 20 12:53 valkey-9.1-9.1.2-r1.apk
-rw-r--r--    1 root     root        285839 Sep 20 12:53 valkey-9.1-benchmark-9.1.2-r1.apk
-rw-r--r--    1 root     root        352144 Sep 20 12:53 valkey-9.1-cli-9.1.2-r1.apk
cccfbd3f3fe5e2f11f39b69a4ad4c1c2a9981ccf449a2579708c2719c1ba8c21  /out/packages/aarch64/challoc-0.1.0-r0.apk
c91ade739f2f531df2c503330412a767607cd09ec91092d2501099477e73c8a1  /out/packages/aarch64/challoc-dev-0.1.0-r0.apk
c5875e0886353adaa25ee9e088a16e9d3eede0a215483815144e3547da1bca93  /out/packages/aarch64/challoc-jemalloc-compat-0.1.0-r0.apk
893188669c4b455d0b940f12e0ece686d4c556e9b5e3e5516fb26f1d3d9c909d  /out/packages/aarch64/valkey-9.1-9.1.2-r1.apk
8cba78de1865d0ccdbd90df1a64a2293f79eb7faa240e64e8e775db6db9e975f  /out/packages/aarch64/valkey-9.1-benchmark-9.1.2-r1.apk
b7a7099116265038a9554d02cd1e19581600dfc367897977a49c32ef19539832  /out/packages/aarch64/valkey-9.1-cli-9.1.2-r1.apk
```
Installed sizes (melange): `valkey-9.1` 4,789,805 B (4677 KiB by `apk info -s`; Wolfi's r0 is 4923 KiB), `-cli` 956,400 B, `-benchmark` 606,024 B. The private library: `/usr/lib/valkey/libchalloc.so.0` 405,928 B (the system one 405,864 B).

`x86_64` (`/out/packages/x86_64/`):
```
-rw-r--r--    1 root     root          1635 Sep 20 12:54 APKINDEX.tar.gz
-rw-r--r--    1 root     root        194463 Sep 20 12:53 challoc-0.1.0-r0.apk
-rw-r--r--    1 root     root         12747 Sep 20 12:53 challoc-dev-0.1.0-r0.apk
-rw-r--r--    1 root     root        194992 Sep 20 12:53 challoc-jemalloc-compat-0.1.0-r0.apk
-rw-r--r--    1 root     root       2174113 Sep 20 12:54 valkey-9.1-9.1.2-r1.apk
-rw-r--r--    1 root     root        307578 Sep 20 12:54 valkey-9.1-benchmark-9.1.2-r1.apk
-rw-r--r--    1 root     root        380263 Sep 20 12:54 valkey-9.1-cli-9.1.2-r1.apk
0ebeafdfc03c1697c02b1fe2205c0f6d1c9ff90ae7fa01c57cd7d78812e3741d  /out/packages/x86_64/valkey-9.1-9.1.2-r1.apk
3b1972948345e749aac4ff2e16b626313db03bf76fa4561b5864aad8e091e528  /out/packages/x86_64/valkey-9.1-benchmark-9.1.2-r1.apk
8fb461cb5ffa7889cab3745fe7090776cf485c727e430e2a34b9dd30a78a16a5  /out/packages/x86_64/valkey-9.1-cli-9.1.2-r1.apk
```
Installed sizes: `valkey-9.1` 4,858,084 B, `-cli` 969,839 B, `-benchmark` 604,847 B; private library 392,416 B (system 388,256 B).

### Package metadata melange derived (aarch64; x86_64 identical with `ld-linux-x86-64.so.2` and no separate loader entry)

```
generating package valkey-9.1-9.1.2-r1
  runtime:
    posix-libc-utils
    so:ld-linux-aarch64.so.1
    so:libc.so.6
    so:libcrypto.so.3
    so:libm.so.6
    so:libssl.so.3
  provides:
    cmd:valkey-server=9.1.2-r1
    valkey=9.1.2-r1
  installed-size: 4789805
generating package valkey-9.1-cli-9.1.2-r1
  runtime:
    so:ld-linux-aarch64.so.1
    so:libc.so.6
    so:libchalloc.so.0
    so:libcrypto.so.3
    so:libm.so.6
    so:libssl.so.3
  provides:
    cmd:valkey-cli=9.1.2-r1
    valkey-cli=9.1.2-r1
  installed-size: 956400
generating package valkey-9.1-benchmark-9.1.2-r1
  runtime:
    so:ld-linux-aarch64.so.1
    so:libc.so.6
    so:libchalloc.so.0
    so:libcrypto.so.3
    so:libm.so.6
    so:libssl.so.3
  provides:
    cmd:valkey-benchmark=9.1.2-r1
    valkey-benchmark=9.1.2-r1
  installed-size: 606024
```

and `.PKGINFO` of `valkey-9.1` (aarch64):
```
size = 4789805
license = BSD-3-Clause
license = Apache-2.0
depend = posix-libc-utils
depend = so:ld-linux-aarch64.so.1
depend = so:libc.so.6
depend = so:libcrypto.so.3
depend = so:libm.so.6
depend = so:libssl.so.3
provides = cmd:valkey-server=9.1.2-r1
provides = valkey=9.1.2-r1
# vendored = so-ver:libchalloc.so.0=9.1.2-r1
# vendored = so:libchalloc.so.0=0
```

What this shows (the layout decision, `contrib/wolfi/README.md` "Valkey"): the private library is `vendored:` (melange's SCA lists a soname under `provides:` only for a file directly in `lib/`, `usr/lib/`, `lib64/`, `usr/lib64/`), so nothing else can resolve `so:libchalloc.so.0` to the `quantum8` build; `valkey-server`'s own `DT_NEEDED libchalloc.so.0` was pruned (`removeSelfProvidedDeps` against the vendored list), so `valkey-9.1` depends on no challoc package. The `-cli` and `-benchmark` subpackages do depend on `so:libchalloc.so.0`: Valkey links every binary with `FINAL_LIBS`, so `valkey-cli` and `valkey-benchmark` carry the same `DT_NEEDED` and rpath, and their subpackages do not vendor the library. apk satisfies that with the system `challoc` (see below); with `valkey-9.1` installed the rpath makes them load the private copy.

## `melange test` (both architectures pass; final YAML, MD5 `05bdacc1`, 13:06Z)

The main package's test, aarch64, trace lines (`+ ...`) and timestamps removed; the `melange test exit 0` line is the driver's. The first run (12:51Z) had failed only in the `-cli`/`-benchmark` environments for the missing `/bin/sh` described above; the main package's steps passed in every run.
```
melange v0.60.0+dirty with runner bubblewrap is testing:
    packages:     [binutils ldd-check valkey-9.1 valkey-9.1-benchmark valkey-9.1-cli]
running step "test/tw/ldd-check"
running step "check for missing library dependencies using ldd"
PASS[ldd-check]: /usr/bin/redis-benchmark
PASS[ldd-check]: /usr/bin/redis-check-aof
PASS[ldd-check]: /usr/bin/redis-check-rdb
PASS[ldd-check]: /usr/bin/redis-cli
PASS[ldd-check]: /usr/bin/redis-sentinel
PASS[ldd-check]: /usr/bin/redis-server
PASS[ldd-check]: /usr/bin/valkey-check-aof
PASS[ldd-check]: /usr/bin/valkey-check-rdb
PASS[ldd-check]: /usr/bin/valkey-sentinel
PASS[ldd-check]: /usr/bin/valkey-server
PASS[ldd-check]: /usr/lib/valkey/libchalloc.so.0
running step "The server is linked against the private challoc and says so"
Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=96a5eb4a877df797
 0x0000000000000001 (NEEDED)             Shared library: [libchalloc.so.0]
 0x0000000000000001 (NEEDED)             Shared library: [libm.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [libssl.so.3]
 0x0000000000000001 (NEEDED)             Shared library: [libcrypto.so.3]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
 0x000000000000000f (RPATH)              Library rpath: [/usr/lib/valkey]
valkey-cli 9.1.2 (git:7f1dffed-dirty)
valkey-benchmark 9.1.2 (git:7f1dffed-dirty)
running step "A running server uses the private library, defragments and purges"
/usr/lib/valkey/libchalloc.so.0
allocator_allocated:2728256
allocator_frag_ratio:1.20
mem_allocator:jemalloc-5.3.0-challoc
___ Begin jemalloc statistics ___
Version: "5.3.0-0-gchalloc"
Build-time option settings
SET: 192307.69 requests per second, p50=0.135 msec
GET: 190476.20 requests per second, p50=0.135 msec
2                                                  <- DBSIZE after the benchmark's key:__rand_int__ and bike:1
used_memory:1197984
allocator_active:12132352
allocator_resident:14340096
running step "The Wolfi checks kept from the jemalloc build"
Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=96a5eb4a877df797  (x4: redis-server, valkey-server, and the sentinel links)
running test pipeline for subpackage valkey-9.1-cli
    packages:     [valkey-9.1-cli]
valkey-cli 9.1.2 (git:7f1dffed-dirty)
running test pipeline for subpackage valkey-9.1-benchmark
    packages:     [valkey-9.1-benchmark]
valkey-benchmark 9.1.2 (git:7f1dffed-dirty)
== melange test exit 0
```
Every `grep -q` assertion of the step (`CONFIG SET activedefrag yes` -> `OK`, `CONFIG GET` -> `yes`, `MEMORY PURGE` -> `OK`, `SET`/`GET`/`EXISTS`, the `maps` equality, the clean log) is silent when it passes; the step runs under `set -eux` and the pipeline reached the next step. x86_64: the same lines, `SET: 235294.11` / `GET: 232558.12 requests per second`, `melange test exit 0`.

## Clean-container installation and the checks by hand (aarch64, verbatim)

`apk add valkey-9.1 valkey-9.1-cli valkey-9.1-benchmark binutils posix-libc-utils` from the local repository (the `apk info -s`/`-R` listings of Wolfi's older revisions are cut):
```
valkey-9.1-9.1.2-r1 installed size:
4677 KiB
valkey-9.1-cli-9.1.2-r1 installed size:
933 KiB
valkey-9.1-benchmark-9.1.2-r1 installed size:
591 KiB
challoc                                  <- apk info -e challoc: installed, pulled in by the -cli/-benchmark so: dependency
== files of valkey-9.1
usr/bin/redis-benchmark
usr/bin/redis-check-aof
usr/bin/redis-check-rdb
usr/bin/redis-cli
usr/bin/redis-sentinel
usr/bin/redis-server
usr/bin/valkey-check-aof
usr/bin/valkey-check-rdb
usr/bin/valkey-sentinel
usr/bin/valkey-server
usr/lib/valkey/libchalloc.so.0
-rwxr-xr-x    1 root     root       4342040 Jan  1  1970 /usr/bin/valkey-server
-rwxr-xr-x    1 root     root        405928 Jan  1  1970 /usr/lib/valkey/libchalloc.so.0
== valkey-server --version
Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=96a5eb4a877df797
== readelf -d valkey-server (NEEDED, RUNPATH)
 0x0000000000000001 (NEEDED)             Shared library: [libchalloc.so.0]
 0x0000000000000001 (NEEDED)             Shared library: [libm.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [libssl.so.3]
 0x0000000000000001 (NEEDED)             Shared library: [libcrypto.so.3]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
 0x000000000000000f (RPATH)              Library rpath: [/usr/lib/valkey]
== readelf -d /usr/lib/valkey/libchalloc.so.0
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
 0x000000000000000e (SONAME)             Library soname: [libchalloc.so.0]
== scripts/check-needed.sh
/usr/bin/valkey-server: NEEDED libchalloc.so.0 libm.so.6 libssl.so.3 libcrypto.so.3 libc.so.6 ld-linux-aarch64.so.1  (RPATH=/usr/lib/valkey )
  ok      libchalloc.so.0
exit 0
== ldd valkey-server
	linux-vdso.so.1 (0x0000f83582abc000)
	libchalloc.so.0 => /usr/lib/valkey/libchalloc.so.0 (0x0000f835825e0000)
	libm.so.6 => /usr/lib/libm.so.6 (0x0000f83582500000)
	libssl.so.3 => /usr/lib/libssl.so.3 (0x0000f835823e0000)
	libcrypto.so.3 => /usr/lib/libcrypto.so.3 (0x0000f83581ea0000)
	libc.so.6 => /usr/lib/libc.so.6 (0x0000f83581cd0000)
	/lib/ld-linux-aarch64.so.1 (0x0000f83582a80000)
== valkey-cli / valkey-benchmark (both: the same NEEDED list as the server, libchalloc.so.0 first)
 0x0000000000000001 (NEEDED)             Shared library: [libchalloc.so.0]
 0x000000000000000f (RPATH)              Library rpath: [/usr/lib/valkey]
== start valkey-server on port 6399 (nothing listens there: 0 sockets)
PING: PONG
== mapped libchalloc in the server (pid 60)
/usr/lib/valkey/libchalloc.so.0
== INFO memory (allocator lines)
used_memory:1000008
allocator_allocated:2728256
allocator_active:10928128
allocator_resident:13049856
allocator_frag_ratio:1.20
allocator_frag_bytes:541168
mem_allocator:jemalloc-5.3.0-challoc
== CONFIG SET activedefrag yes
OK
activedefrag yes 
== MEMORY PURGE
OK
== MEMORY MALLOC-STATS | head -12
___ Begin jemalloc statistics ___
Version: "5.3.0-0-gchalloc"
Build-time option settings
  config.debug: false
  config.stats: true
  config.quantum: 8
Run-time option settings
  opt.background_thread: false
  opt.prefault: false
  opt.dirty_decay_ms: 10000
  opt.muzzy_decay_ms: 0
  opt.narenas: 64
== valkey-benchmark -p 6399 -t set -n 200000 -q
SET: 137174.22 requests per second, p50=0.191 msec
== after: DBSIZE, INFO memory
1
used_memory:1148336
used_memory_human:1.10M
allocator_allocated:3202528
allocator_active:12197888
allocator_resident:14405632
allocator_frag_ratio:1.25
active_defrag_running:0
total_commands_processed:200013
active_defrag_hits:0
active_defrag_misses:0
== MEMORY PURGE again, then SHUTDOWN NOSAVE
OK
server exit 0
== server log tail
60:M 20 Sep 2026 12:54:04.210 * Server initialized
60:M 20 Sep 2026 12:54:04.210 * Ready to accept connections tcp
60:M 20 Sep 2026 12:54:05.896 * User requested shutdown... (user request from 'id=65 addr=127.0.0.1:51184 laddr=127.0.0.1:6399 fd=10 name=*redacted* user=*redacted* lib-name= lib-ver=')
60:M 20 Sep 2026 12:54:05.896 * Module lua unloaded
60:M 20 Sep 2026 12:54:05.896 # Valkey is now ready to exit, bye bye...

== co-installation: apk add challoc (the system quantum16 library) next to it
exit 0
-rwxr-xr-x    1 root     root        405864 Jan  1  1970 /usr/lib/libchalloc.so.0
-rwxr-xr-x    1 root     root        405928 Jan  1  1970 /usr/lib/valkey/libchalloc.so.0
/usr/lib/libchalloc.so.0 is owned by challoc-0.1.0-r0
/usr/lib/valkey/libchalloc.so.0 is owned by valkey-9.1-9.1.2-r1
PING: PONG; mapped: /usr/lib/valkey/libchalloc.so.0 
OK                                                 <- CONFIG SET activedefrag yes with both libraries installed
server exit 0

== both ways (a): LD_LIBRARY_PATH=/usr/lib; DT_RPATH is searched before it, so the private library still wins
PING: PONG; mapped: /usr/lib/valkey/libchalloc.so.0 
server exit 0
== both ways (b): LD_PRELOAD=/usr/lib/libchalloc.so.0 (the system quantum16 build) satisfies the soname first
exit 139 (134 = abort; 124 = still running after 15 s)
```
`MEMORY MALLOC-STATS` shows `config.quantum: 8`, the private build; `config.quantum` is challoc's own line in the report.

The client tools' dependency and where they load from (a second clean container, aarch64):
```
== apk info -R valkey-9.1-cli
valkey-9.1-cli-9.1.2-r1 depends on:
so:ld-linux-aarch64.so.1
so:libc.so.6
so:libchalloc.so.0
so:libcrypto.so.3
so:libm.so.6
so:libssl.so.3
== who satisfied it
/usr/lib/libchalloc.so.0 is owned by challoc-0.1.0-r0
== readelf valkey-cli
 0x0000000000000001 (NEEDED)             Shared library: [libchalloc.so.0]
 0x000000000000000f (RPATH)              Library rpath: [/usr/lib/valkey]
== LD_DEBUG=libs valkey-cli --version (which file the client tool loads)
        22:	find library=libchalloc.so.0 [0]; searching
        22:	 search path=/usr/lib/valkey		(RPATH from file valkey-cli)
        22:	  trying file=/usr/lib/valkey/libchalloc.so.0
```

x86_64 (`task2-2-apk-x86_64.log`): the same sequence with the same results; the lines that differ: `valkey-server` 4,423,832 B, private library 392,416 B, system library 388,256 B, `allocator_frag_ratio:1.20` -> `1.24` after the benchmark, `SET: 172413.80 requests per second, p50=0.151 msec`, `total_commands_processed:200013`, both-ways (a) `mapped: /usr/lib/valkey/libchalloc.so.0`, (b) `exit 139`.

### The both-ways crash, explained (gdb, both architectures)

`exit 139` is a segfault with no output at all, so it was run under gdb (`apk add gdb` in the clean container). First the controls:
```
== LD_PRELOAD system lib, --version only (no server init)
Segmentation fault (core dumped)
exit 139
== LD_PRELOAD private lib, --version
Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=96a5eb4a877df797
exit 0
== LD_PRELOAD system lib, valkey-cli --version
valkey-cli 9.1.2 (git:7f1dffed-dirty)
exit 0
== LD_PRELOAD private lib, full server start (control: should run), 3 s
18:M 20 Sep 2026 13:04:31.009 * Ready to accept connections tcp
```
then the backtrace (aarch64; x86_64 identical apart from addresses):
```
Program received signal SIGSEGV, Segmentation fault.
0x0000c827c0ba0a00 in serverLogRaw ()
#0  0x0000c827c0ba0a00 in serverLogRaw ()
#1  0x0000c827c0ba106c [PAC] in _serverLog ()
#2  0x0000c827c0ab6a3c [PAC] in _serverAssert ()
#3  0x0000c827c0a56e64 [PAC] in allocatorDefragInit ()
#4  0x0000c827c0a442e4 [PAC] in main ()
From                To                  Syms Read   Shared Object Library
0x0000f4c283490000  0x0000f4c2834e0000  Yes (*)     /lib/ld-linux-aarch64.so.1
0x0000f4c283410000  0x0000f4c283490000  Yes (*)     /usr/lib/libchalloc.so.0
0x0000f4c283330000  0x0000f4c283410000  Yes (*)     /usr/lib/libm.so.6
0x0000f4c283210000  0x0000f4c283330000  Yes (*)     /usr/lib/libssl.so.3
0x0000f4c282cd0000  0x0000f4c283210000  Yes (*)     /usr/lib/libcrypto.so.3
0x0000f4c282b00000  0x0000f4c282cd0000  Yes (*)     /usr/lib/libc.so.6
```
So: with the system library preloaded the loader satisfies `DT_NEEDED libchalloc.so.0` from it (one `libchalloc` mapped, the private one never opened), and `main` calls `allocatorDefragInit` (Valkey 9.1 does it before `initServerConfig`), whose `assert(jemalloc_quantum == 8)` (`allocator_defrag.c:276`, `serverassert.h`'s `assert`) fires; `_serverAssert` then tries to log before the log is configured and segfaults in `serverLogRaw`. The `--version` control crashes the same way (that call precedes the version print). The outcome is the design's: the `quantum16` system library cannot run this binary, loudly and at start; the trigger is the quantum assertion; `valkey-cli`, which has no such assertion, runs on either. Under `DT_RPATH`, `LD_LIBRARY_PATH` cannot bring this about at all (check (a)).

## Valkey's `unit/memefficiency` against the packaged binary (both architectures pass)

Clean `wolfi-base` container: `apk add valkey-9.1 valkey-9.1-cli valkey-9.1-benchmark tcl git procps bash`; Valkey 9.1.2 cloned (`7f1dffedff6de73058b2c2a389422b6ecd56c8fb`), `contrib/valkey/0001-Add-MALLOC-challoc.patch` applied for its `tests/unit/memefficiency.tcl` hunk (the `ignore_bytes 5000kb` of the eval-scripts test under `*challoc*`, Phase 1 acceptance step 4's approved constant), `src/valkey-server`, `src/valkey-cli`, `src/valkey-benchmark` symlinked to the installed binaries, `tclsh9.0` 9.0.4.

aarch64, `./runtest --single unit/memefficiency --dump-logs`:
```
[ok]: Memory efficiency with values in range 32 (129 ms)
[ok]: Memory efficiency with values in range 64 (129 ms)
[ok]: Memory efficiency with values in range 128 (129 ms)
[ok]: Memory efficiency with values in range 1024 (144 ms)
[ok]: Memory efficiency with values in range 16384 (337 ms)
[ok]: Active defrag - AOF loading (1585 ms)
[ok]: Active Defrag main dictionary: standalone (20560 ms)
[ok]: Active Defrag eval scripts: standalone (6054 ms)
[ok]: Active Defrag big hash: standalone (20083 ms)
[ok]: Active Defrag big list: standalone (19490 ms)
[ok]: Active Defrag big set: standalone (20729 ms)
[ok]: Active Defrag big zset: standalone random-score (21065 ms)
[ok]: Active Defrag big zset: standalone fixed-score (21181 ms)
[ok]: Active Defrag stream: standalone (20431 ms)
[ok]: Active Defrag pubsub: standalone (48971 ms)
[ok]: Active Defrag main dictionary: cluster (18972 ms)
[ok]: Active Defrag eval scripts: cluster (6147 ms)
[ok]: Active Defrag big hash: cluster (20108 ms)
[ok]: Active Defrag big list: cluster (19491 ms)
[ok]: Active Defrag big set: cluster (20734 ms)
[ok]: Active Defrag big zset: cluster random-score (21081 ms)
[ok]: Active Defrag big zset: cluster fixed-score (21072 ms)
[ok]: Active Defrag stream: cluster (20523 ms)
[ok]: Active Defrag pubsub: cluster (48295 ms)
[1/1 done]: unit/memefficiency (418 seconds)
\o/ All tests passed without errors!
== runtest exit 0, 418 s
```
(the nine `[ok]: Setup start_cluster (1910 ms)` lines between the cluster cases are cut.) 33 ok, 0 failed, the same count as Phase 1 acceptance step 4's run 2 (`bench/results/2026-09-17-valkey-suite.md`).

x86_64: the same 33 `[ok]` lines, `[1/1 done]: unit/memefficiency (433 seconds)`, `\o/ All tests passed without errors!`, `runtest exit 0, 434 s`.

## Headline results

- `valkey-9.1`, `valkey-9.1-cli`, `valkey-9.1-benchmark` 9.1.2-r1 build from Wolfi's definition plus the changes in `contrib/wolfi/valkey/valkey-9.1.yaml` on aarch64 and x86_64 with Wolfi's toolchain (gcc 16.2, Rust 1.98); the private `quantum8` build adds 8.2 s of `cargo` to a 30 s pipeline; melange's linters are clean; `melange test` passes on both (final YAML).
- `valkey-server --version`: `malloc=jemalloc-5.3.0-challoc`. `readelf -d`: `NEEDED [libchalloc.so.0]`, `RPATH [/usr/lib/valkey]` (`DT_RPATH`, not `RUNPATH`, from Wolfi's binutils). `ldd` and `/proc/<pid>/maps` of the running server name `/usr/lib/valkey/libchalloc.so.0` and nothing else; `test/tw/ldd-check` passes on every binary and on the private library.
- `INFO memory`: `mem_allocator:jemalloc-5.3.0-challoc`; `CONFIG SET activedefrag yes` -> `OK` (also with the system `challoc` co-installed); `MEMORY PURGE` -> `OK`; `MEMORY MALLOC-STATS` is challoc's jemalloc-format report with `config.quantum: 8`; `valkey-benchmark -t set -n 200000 -q` served 200,013 commands on each architecture and the server shut down cleanly.
- Package metadata: `valkey-9.1` vendors `so:libchalloc.so.0` and depends on no challoc package; the `-cli` and `-benchmark` subpackages depend on `so:libchalloc.so.0` (the system `challoc`), which is correct for them and is the one place the two-library layout costs a second copy (a server image with the cli).
- Both ways: `LD_LIBRARY_PATH=/usr/lib` does not displace the private library (`DT_RPATH` precedence); `LD_PRELOAD` of the system `quantum16` library kills the server at start in `allocatorDefragInit`'s quantum assertion (before the log exists, so as a segfault in `serverLogRaw`).
- `unit/memefficiency` against the packaged binary: 33 ok, 0 failed on both architectures (418 s / 433 s).

## Not done, and why

- Not run in Wolfi CI (Phase 3's acceptance); the challoc `git-checkout` step of the definition is unexercised for the same reason as in task 1 (no release tag, repository not public): the local build used `--build-option local-challoc` with `stage.sh`'s copy of the tree. The Valkey `git-checkout` and `patch` steps did run.
- The full Valkey suite was not run against the package (out of scope here; `make valkey` in this repository is where it runs, last full pass in Phase 1 acceptance step 4). The melange test does not run the suite either: the package build environment has `tcl` only if asked, and a 20-minute test is not a package test.
- No `challoc-valkey` subpackage or `challoc-valkey.yaml` was written: the SCA finding above decided for the in-package build; the alternative is described in `contrib/wolfi/README.md` and in the PROGRESS.d note for the coordinator.
- No static-link variant (`libchalloc.a` into the three binaries, which would remove the client tools' `so:` dependency and the second copy) was built: untested territory (`contrib/wolfi/README.md`, "Nothing is shipped from libchalloc.a") and not what the task asked for.
- `RPATH` vs `RUNPATH`: the linker's default was kept and documented rather than forced either way; a one-flag change (`-Wl,--enable-new-dtags`) if Wolfi prefers `RUNPATH`.

## Diff against Wolfi's `valkey-9.1.yaml` (a944f08)

```diff
--- .tmp-wolfi/valkey-9.1.yaml	2026-09-20 08:27:39
+++ contrib/wolfi/valkey/valkey-9.1.yaml	2026-09-20 09:05:38
@@ -1,10 +1,50 @@
+# valkey-9.1 on challoc: the Wolfi package definition with the allocator
+# switched from the bundled jemalloc to challoc (docs/plan.md Phase 3 task
+# 2, docs/design.md section 13.6). Based on wolfi-dev/os valkey-9.1.yaml at
+# a944f08 (2026-09-19; the file itself last changed in 195f812, the 9.1.2
+# update); contrib/wolfi/README.md ("Valkey") lists every change from it,
+# and bench/results/2026-09-20-phase3-task2-valkey-package.md the build.
+#
+# Two libraries, one soname (design 13.6): the system libchalloc.so.0 in
+# /usr/lib is a quantum16 build (the class table Wolfi's jemalloc
+# consumers run on); Valkey's defragmenter asserts arenas.quantum == 8
+# (allocator_defrag.c, from the --with-lg-quantum=3 of its bundled
+# jemalloc), so Valkey gets a private quantum8 build of the same source,
+# built here from the challoc tree the way the bundled jemalloc was built
+# from deps/jemalloc, installed as /usr/lib/valkey/libchalloc.so.0 and
+# found through the binaries' rpath. melange's SCA files a shared object
+# outside the library directories under `vendored:` rather than
+# `provides:` and drops the package's own DT_NEEDED dependency on it, so
+# nothing else can pick this build and valkey-9.1 pulls in no challoc
+# package. Valkey links every binary with the allocator, so valkey-cli
+# and valkey-benchmark carry the same DT_NEEDED and rpath: their
+# subpackages get an automatic `so:libchalloc.so.0` dependency, which apk
+# satisfies with the system challoc package (they have no quantum
+# requirement); next to valkey-9.1 they load the private copy through the
+# rpath. The system challoc and this package coexist (the results file
+# has the co-installation check and the both-ways check).
+#
+# Local build: contrib/wolfi/valkey/stage.sh assembles the source
+# directory (the two patches and a copy of the challoc tree), then
+#
+#   melange build contrib/wolfi/valkey/valkey-9.1.yaml --arch aarch64 \
+#     --source-dir <staged> --build-option local-challoc \
+#     --pipeline-dir /path/to/wolfi-dev/os/pipelines \
+#     --signing-key melange.rsa --out-dir packages
+#
+# The Wolfi copy drops the option and the challoc checkout runs.
 package:
   name: valkey-9.1
   version: "9.1.2"
-  epoch: 0 # glibc: 2.43->2.44
+  epoch: 1 # allocator: bundled jemalloc -> challoc (private quantum8 build)
   description: Advanced key-value store
   copyright:
     - license: BSD-3-Clause
+    # The private allocator build (challoc, Apache-2.0) is part of this
+    # package; melange records its source under `vendored:`.
+    - license: Apache-2.0
+      paths:
+        - /usr/lib/valkey/libchalloc.so.0
   dependencies:
     provides:
       - valkey=${{package.full-version}}
@@ -17,19 +57,37 @@
     cpu: "1"
     memory: 1Gi
 
+vars:
+  # The challoc release built into /usr/lib/valkey (contrib/wolfi/challoc.yaml
+  # carries the same version for the system packages; keep them together).
+  challoc-version: "0.1.0"
+  rust-version: "1.98"
+  # What valkey-server --version and INFO memory's mem_allocator report:
+  # "jemalloc-" JEMALLOC_VERSION_{MAJOR,MINOR,BUGFIX} "-challoc" from
+  # challoc's <challoc/jemalloc.h> (contrib/valkey patch, src/zmalloc.h).
+  allocator-name: jemalloc-5.3.0-challoc
+
+options:
+  # `--build-option local-challoc`: the source directory melange was given
+  # already holds the challoc tree under challoc/ (contrib/wolfi/valkey/
+  # stage.sh), skip its git-checkout. For a local or CI build from the
+  # challoc repository; the Wolfi copy never sets it.
+  local-challoc: {}
+
 environment:
   contents:
     packages:
       - autoconf
       - automake
       - bash
+      - binutils # readelf for the DT_NEEDED and RUNPATH check of the built binary
       - build-base
       - busybox
       - ca-certificates-bundle
       - glibc-2.44-dev
-      - jemalloc
       - linux-headers
       - openssl-dev # FTBFS with openssl-hardened-dev due to missing ERR_load_crypto_strings, SSL_load_error_strings, SSL_library_init
+      - rust-${{vars.rust-version}} # the private challoc build
 
 pipeline:
   - uses: git-checkout
@@ -38,19 +96,77 @@
       tag: ${{package.version}}
       expected-commit: 7f1dffedff6de73058b2c2a389422b6ecd56c8fb
 
+  # 0001-Add-MALLOC-challoc.patch is challoc's contrib/valkey patch for this
+  # tag: `make MALLOC=challoc` keeps every USE_JEMALLOC code path (stats,
+  # purge, background thread, active defrag) and links -lchalloc instead
+  # of building deps/jemalloc; zmalloc.h includes <challoc/jemalloc.h>
+  # and reports the allocator as jemalloc-5.3.0-challoc; the test suite's
+  # eval-scripts defrag test gets a challoc stop threshold (its Lua heap is
+  # in the allocator under challoc, in glibc under the je_-prefixed
+  # bundled jemalloc). The package build does not run the suite; the hunk
+  # is carried so the patch is the one the suite was passed with.
   - uses: patch
     with:
-      patches: 0000-Disable-protected-mode.patch
+      patches: 0000-Disable-protected-mode.patch 0001-Add-MALLOC-challoc.patch
 
-  - runs: |
-      export CFLAGS="$CFLAGS -DUSE_MALLOC_USABLE_SIZE"
-        make BUILD_TLS=yes \
-        all -j$(nproc)
-      make install PREFIX=/usr INSTALL_BIN="${{targets.destdir}}/usr/bin"
+  - uses: git-checkout
+    if: ${{options.local-challoc.enabled}} == 'false'
+    with:
+      repository: https://github.com/chainguard-dev/challoc
+      tag: v${{vars.challoc-version}}
+      # No release tag yet (2026-09-20): the main commit this definition
+      # was written against. The Wolfi PR sets the tag and its commit.
+      expected-commit: 5a1e715bca2f11402b998a18664016a47fa5a938
+      destination: challoc
 
+  - name: Build the private challoc (quantum8, soname libchalloc.so.0)
+    working-directory: challoc
+    runs: |
+      crate=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
+      [ "$crate" = "${{vars.challoc-version}}" ] || {
+        echo "challoc Cargo.toml version $crate != vars.challoc-version ${{vars.challoc-version}}"; exit 1; }
+      rustc --version; cargo --version
+      cargo build --release --locked --no-default-features --features quantum8
+      readelf -d target/release/libchalloc.so | grep -q 'Library soname: \[libchalloc.so.0\]'
+
+  - name: Build Valkey against it
+    runs: |
+      # The header is challoc's own (<challoc/jemalloc.h>, never
+      # <jemalloc/...>); -L finds libchalloc.so in the cargo output (the
+      # link records the soname, libchalloc.so.0); the RUNPATH is where
+      # this package installs it. -Wl,--no-as-needed is not needed:
+      # valkey-server calls je_mallctl and friends itself, so --as-needed
+      # keeps the entry (design 13.6); the next step checks it anyway.
+      lib="$PWD/challoc/target/release"
+      export CFLAGS="$CFLAGS -DUSE_MALLOC_USABLE_SIZE -I$PWD/challoc/contrib/valkey/include"
+      export LDFLAGS="$LDFLAGS -L$lib -Wl,-rpath,/usr/lib/valkey"
+      # MALLOC on every invocation: Valkey's Makefile persists its settings
+      # and a `make install` without it would rebuild with the bundled
+      # jemalloc.
+      make MALLOC=challoc BUILD_TLS=yes all -j$(nproc)
+      make MALLOC=challoc BUILD_TLS=yes install PREFIX=/usr INSTALL_BIN="${{targets.destdir}}/usr/bin"
+      install -Dm755 "$lib/libchalloc.so" "${{targets.destdir}}/usr/lib/valkey/libchalloc.so.0"
+      [ ! -d deps/jemalloc/lib ] || { echo "deps/jemalloc was built"; exit 1; }
+
+  - name: Check the built server names the allocator and the private library
+    runs: |
+      bin="${{targets.destdir}}/usr/bin/valkey-server"
+      # The RUNPATH (/usr/lib/valkey) exists only once the package is
+      # installed; here the loader is pointed at the staged copy (the
+      # cargo output is named libchalloc.so, the loader wants the soname).
+      export LD_LIBRARY_PATH="${{targets.destdir}}/usr/lib/valkey"
+      "$bin" --version
+      "$bin" --version | grep -q 'malloc=${{vars.allocator-name}}'
+      sh challoc/scripts/check-needed.sh "$bin" libchalloc.so.0
+      readelf -d "$bin" | grep -E 'RUNPATH|RPATH' | grep -q '\[/usr/lib/valkey\]'
+
   - uses: strip
 
 subpackages:
+  # Both client tools are linked against libchalloc.so.0 with the rpath
+  # (see the header comment); melange derives their so:libchalloc.so.0
+  # dependency, satisfied by the system challoc package when valkey-9.1
+  # is not installed alongside.
   - name: ${{package.name}}-cli
     dependencies:
       provides:
@@ -91,23 +207,76 @@
   environment:
     contents:
       packages:
+        - ${{package.name}}-benchmark
         - ${{package.name}}-cli
+        - binutils
   pipeline:
     - uses: test/tw/ldd-check
-    - runs: |
-        cat <<EOF >> /tmp/valkey.conf
-        dbfilename dump.rdb
-        pidfile /tmp/valkey_6379.pid
+    - name: The server is linked against the private challoc and says so
+      runs: |
+        set -eux
+        valkey-server --version
+        valkey-server --version | grep -q 'malloc=${{vars.allocator-name}}'
+        readelf -d /usr/bin/valkey-server | grep -E 'NEEDED|RUNPATH|RPATH'
+        readelf -d /usr/bin/valkey-server | grep -q 'NEEDED.*\[libchalloc.so.0\]'
+        readelf -d /usr/bin/valkey-server | grep -E 'RUNPATH|RPATH' | grep -q '\[/usr/lib/valkey\]'
+        readelf -d /usr/lib/valkey/libchalloc.so.0 | grep -q 'Library soname: \[libchalloc.so.0\]'
+        # The client tools are linked the same way (Valkey links every
+        # binary with FINAL_LIBS); their subpackages depend on
+        # so:libchalloc.so.0, the system challoc, and prefer this copy
+        # through the rpath when it is installed.
+        for b in valkey-cli valkey-benchmark; do
+          readelf -d /usr/bin/$b | grep -q 'NEEDED.*\[libchalloc.so.0\]'
+          readelf -d /usr/bin/$b | grep -E 'RUNPATH|RPATH' | grep -q '\[/usr/lib/valkey\]'
+        done
+        valkey-cli --version
+        valkey-benchmark --version
+    - name: A running server uses the private library, defragments and purges
+      runs: |
+        set -eux
+        # A unix socket, so no port has to be free in the test environment.
+        cat <<EOF > /tmp/valkey.conf
+        port 0
+        unixsocket /tmp/valkey.sock
+        unixsocketperm 700
+        pidfile /tmp/valkey.pid
         dir /tmp/
+        dbfilename dump.rdb
         EOF
-
-        valkey-server /tmp/valkey.conf > /dev/null 2>&1 &
-        sleep 2 # wait for valkey to start
-        valkey-server --version | grep jemalloc || exit 1
-        valkey-cli SET bike:1 "Process 134" || exit 1
-        valkey-cli GET bike:1 | grep 'Process 134' || exit 1
-        valkey-cli exists bike:1 | grep 1 || exit 1
-        valkey-cli exists bike:2 | grep 0 || exit 1
+        valkey-server /tmp/valkey.conf > /tmp/valkey.log 2>&1 &
+        pid=$!
+        for i in $(seq 1 100); do
+          valkey-cli -s /tmp/valkey.sock PING 2>/dev/null | grep -q PONG && break
+          sleep 0.2
+        done
+        valkey-cli -s /tmp/valkey.sock PING | grep -q PONG
+        # The one libchalloc mapped in the server is the private build.
+        awk '$6 ~ /libchalloc/ {print $6}' /proc/$pid/maps | sort -u
+        [ "$(awk '$6 ~ /libchalloc/ {print $6}' /proc/$pid/maps | sort -u)" = /usr/lib/valkey/libchalloc.so.0 ]
+        valkey-cli -s /tmp/valkey.sock INFO memory | tr -d '\r' | grep -E '^(mem_allocator|allocator_frag_ratio|allocator_allocated):'
+        valkey-cli -s /tmp/valkey.sock INFO memory | tr -d '\r' | grep -qx 'mem_allocator:${{vars.allocator-name}}'
+        # Active defrag needs HAVE_DEFRAG and an allocator whose
+        # arenas.quantum is 8; the bundled-jemalloc error text is
+        # "Active defragmentation cannot be enabled: ...".
+        valkey-cli -s /tmp/valkey.sock CONFIG SET activedefrag yes | grep -qx OK
+        valkey-cli -s /tmp/valkey.sock CONFIG GET activedefrag | grep -qx yes
+        valkey-cli -s /tmp/valkey.sock MEMORY PURGE | grep -qx OK
+        valkey-cli -s /tmp/valkey.sock MEMORY MALLOC-STATS | head -3
+        valkey-cli -s /tmp/valkey.sock MEMORY MALLOC-STATS | grep -q 'jemalloc statistics'
+        valkey-cli -s /tmp/valkey.sock SET bike:1 "Process 134" | grep -qx OK
+        valkey-cli -s /tmp/valkey.sock GET bike:1 | grep -q 'Process 134'
+        valkey-cli -s /tmp/valkey.sock EXISTS bike:1 | grep -qx 1
+        valkey-cli -s /tmp/valkey.sock EXISTS bike:2 | grep -qx 0
+        # Some traffic through the allocator, then the server is still up.
+        valkey-benchmark -s /tmp/valkey.sock -t set,get -n 20000 -q
+        valkey-cli -s /tmp/valkey.sock DBSIZE
+        valkey-cli -s /tmp/valkey.sock INFO memory | tr -d '\r' | grep -E '^(used_memory|allocator_active|allocator_resident):'
+        valkey-cli -s /tmp/valkey.sock CONFIG SET activedefrag no | grep -qx OK
+        valkey-cli -s /tmp/valkey.sock SHUTDOWN NOSAVE || true
+        wait $pid || true
+        ! grep -iE 'challoc:|assert|crash' /tmp/valkey.log
+    - name: The Wolfi checks kept from the jemalloc build
+      runs: |
         redis-check-aof --version
         set +e; output=$(redis-check-rdb 2>&1); set -e
         echo "$output" | grep -F "Usage:"
```
