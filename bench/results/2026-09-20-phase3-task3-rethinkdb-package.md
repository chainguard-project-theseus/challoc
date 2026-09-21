# Phase 3 task 3: Wolfi's RethinkDB 2.4 on challoc, first real build

Date 2026-09-20. Lane keydb, worktree `../challoc-wt/keydb`, branch `loop/keydb` from `main` f419b88 (nothing under `src/` changed for this package; the lane's one `src/` change, the `get_defrag_hint` export of commit 9d62dbb, is KeyDB's). Definition: `contrib/wolfi/rethinkdb/rethinkdb.yaml`; layout and recipe: `contrib/wolfi/README.md`, "RethinkDB". No numbers in this file are benchmark results.

## Base

- wolfi-dev/os `rethinkdb.yaml` at `main` `8c473de11ba10d96fb87ddfca734cb650a94ee12` (2026-09-20); the file's last change is `d3dc8df` ("rethinkdb: transition to newest version of glibc (#282712)", 2026-08-30). Wolfi's `pipelines/` (for `test/tw/ldd-check` and `test/daemon-check-output`) checked out at the same commit.
- RethinkDB tag `v2.4.4`, commit `90c6e37472ca945b5138bc0e1d5434face691be0` (Wolfi's `expected-commit`).
- challoc: this tree at 9d62dbb, built into `challoc`, `challoc-dev`, `challoc-jemalloc-compat`, `challoc-jemalloc-compat-dev` 0.1.0-r0 first (`contrib/wolfi/challoc.yaml`, `--build-option local-source`, `melange test` passing on both architectures), so the RethinkDB build environment takes `challoc-jemalloc-compat-dev` from the local repository.
- The diff against Wolfi's file (156 lines, `diff -u`) is at the end of this file.

## Where and with what

- VMs: `challoc-keydb-arm64` (c4a-standard-16, Neoverse-V2) and `challoc-keydb-amd64` (c4-standard-16), Ubuntu 24.04, kernel `7.0.0-1011-gcp`, Docker 29.1.3, zone us-east4-a; created, provisioned, used and deleted within the iteration.
- melange: Wolfi's `melange` apk (`v0.60.0+dirty`) in a `--privileged` `cgr.dev/chainguard/wolfi-base` container, `--runner bubblewrap`, `--source-dir /work/contrib/wolfi/rethinkdb` (no patches). `melange test` with `--test-package-append=wolfi-base`.
- Build environment (from the melange log, both architectures): `gcc-12=12.5.0-r3` (`gcc-12-default`), `binutils=2.47-r1`, `glibc-2.44-dev=2.44-r6`, `boost-dev=1.92.0-r1`, `abseil-cpp-dev=20260817.0-r4`, `curl-dev=8.22.0-r2`, `openssl-dev=3.6.4-r7`, `python-3.13=3.13.15`, `challoc-jemalloc-compat=0.1.0-r0`, `challoc-jemalloc-compat-dev=0.1.0-r0` (from the local repository; no `jemalloc-dev`, no `libjemalloc2`). Configure fetched and built `protobuf_3.19.4` (`--fetch protoc`, Wolfi's choice).
- Clean-container checks: `cgr.dev/chainguard/wolfi-base` with the local repository and the generated key added, Wolfi's repository still configured; the PyPI `rethinkdb` driver installed with pip for the smoke test.

## Timing of the RethinkDB pipeline (aarch64)

```
23:18:15 running step "git-checkout"                 (RethinkDB v2.4.4)
         (./configure --prefix=/usr --sysconfdir=/etc --fetch protoc --with-jemalloc --dynamic jemalloc; config.mk checks)
23:18:22 running step "autoconf/make"                (protoc fetched and built, then 435 objects, -j16)
23:23:42 running step "Check the built server is on challoc's libjemalloc.so.2"
23:23:42 running step "autoconf/make-install"
23:23:43 running step "strip"
```

5 min 20 s of `make` on 16 vCPUs. x86_64: `git-checkout` 23:18:31, `autoconf/make` 23:18:34 -> check step 23:24:54 (6 min 20 s). melange's linter: `no lint findings to persist for package rethinkdb` on both architectures.

Configure's summary (aarch64; the allocator lines are the point):
```
Bash:                           5.3.0(1)-release
Use ccache:                     no
C++ Compiler:                   GCC 12.5.0 (/usr/local/bin/c++)
Host System:                    aarch64-unknown-linux-gnu
Build System:                   no
Operating System:               Linux
OS Version:                     Linux 7.0.0-1011-gcp aarch64
Cross-compiling:                no
Host Operating System:          Linux
Build Architecture:             aarch64
C++11:                          ok
Protobuf compiler:              external/protobuf_3.19.4
python:                         python 3.13.15
wget:                           /usr/bin/wget
curl:                           no
Google Test:                    external/gtest_1.8.1
termcap:                        -ltinfo
boost_system:                   no
Protobuf library:               external/protobuf_3.19.4
RE2:                            external/re2_2015-11-01
z:                              -lz
crypto:                         -lcrypto
ssl:                            -lssl
curl:                           -lcurl
quickjs:                        external/quickjs_95d6dcf6358a74e9cbe04eade7c383f43ba306cb
malloc:                         jemalloc
jemalloc:                       -ljemalloc
Test protobuf:                  external/protobuf_3.19.4
Test boost:                     ok
* Warning: ARM support is still experimental
Installation prefix:            /usr
Configuration prefix:           /etc
Runtime data prefix:            /usr/var
* Wrote configuration to config.mk
ALLOCATOR := jemalloc
JEMALLOC_LIBS += -ljemalloc
HAS_JEMALLOC := 1
HAS_JEMALLOC := 1
STATIC_MALLOC := 0
```

The `config.mk` lines the definition checks, and the check step's output:
```
tep "Check the built server is on challoc's libjemalloc.so.2"
WARN + '[' -n build/release/rethinkdb ]
WARN + readelf -d build/release/rethinkdb
INFO  0x0000000000000001 (NEEDED)             Shared library: [libcurl.so.4]
INFO  0x0000000000000001 (NEEDED)             Shared library: [libssl.so.3]
INFO  0x0000000000000001 (NEEDED)             Shared library: [libcrypto.so.3]
INFO  0x0000000000000001 (NEEDED)             Shared library: [libz.so.1]
INFO  0x0000000000000001 (NEEDED)             Shared library: [libjemalloc.so.2]
INFO  0x0000000000000001 (NEEDED)             Shared library: [libstdc++.so.6]
INFO  0x0000000000000001 (NEEDED)             Shared library: [libm.so.6]
INFO  0x0000000000000001 (NEEDED)             Shared library: [libgcc_s.so.1]
INFO  0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
INFO  0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
WARN + readelf -d build/release/rethinkdb
WARN + nm build/release/rethinkdb
WARN + readelf -d /usr/lib/libjemalloc.so.2
WARN + build/release/rethinkdb --version
INFO rethinkdb 2.4.4-dirty (aarch64-unknown-linux-gnu) (GCC 12.5.0)
INFO Linux 7.0.0-1011-gcp aarch64
WARN + exit 0
INFO
```

melange's SCA on the built package: `found lib libjemalloc.so.2 for usr/bin/rethinkdb` -> `depend = so:libjemalloc.so.2` next to the explicit `depend = challoc-jemalloc-compat`. Wolfi's current package has neither: it links `libjemalloc.a` (see "Base" in the README section for why `--dynamic jemalloc` is the one configure change).

## Built packages

`aarch64` (`/out/packages/aarch64/`, next to the challoc and KeyDB packages of this iteration):
```
-rw-r--r-- 1 root root 9885025 Sep 20 23:23 rethinkdb-2.4.4-r15.apk
df1a9b6367af9e193c5a374e0494eba744b59321defbc561a6a64db4366e3f4d  rethinkdb-2.4.4-r15.apk
```
`x86_64`:
```
-rw-r--r-- 1 root root 10363292 Sep 20 23:24 rethinkdb-2.4.4-r15.apk
a2de9f9949d449f69a9c59b673554ac70c98aed317df243a3d8cf3330aefae86  rethinkdb-2.4.4-r15.apk
```
Metadata melange derived (aarch64; x86_64 identical with `so:ld-linux-x86-64.so.2`):
```
-- rethinkdb .PKGINFO
pkgname = rethinkdb
pkgver = 2.4.4-r15
size = 26390857
commit = 9d62dbbdc95bed84c06de3f925af11c1b1813d95
depend = challoc-jemalloc-compat
depend = so:ld-linux-aarch64.so.1
depend = so:libc.so.6
depend = so:libcrypto.so.3
depend = so:libcurl.so.4
depend = so:libgcc_s.so.1
depend = so:libjemalloc.so.2
depend = so:libm.so.6
depend = so:libssl.so.3
depend = so:libstdc++.so.6
depend = so:libz.so.1
provides = cmd:rethinkdb=2.4.4-r15
```
`depend = challoc-jemalloc-compat` is the explicit runtime dependency, `so:libjemalloc.so.2` the derived one (new: Wolfi's package has no jemalloc dependency at all, the library being linked in). No subpackages (Wolfi's file has none).

## `melange test` (both architectures pass)

Verbatim from the aarch64 log (`WARN +` lines are the test's `set -x` trace with the plumbing elided; `INFO` lines are output; the `--help` text elided):
```
running step "test/tw/ldd-check"
INFO running step "check for missing library dependencies using ldd"
INFO [ldd-check] Testing binaries in package rethinkdb
INFO PASS[ldd-check]: /usr/bin/rethinkdb
INFO INFO[ldd-check]: tested 1 files with ldd. 1 passes. 0 fails.
INFO rethinkdb 2.4.4-dirty (aarch64-unknown-linux-gnu) (GCC 12.5.0)
INFO (rethinkdb --help output elided)
INFO running step "Test rethinkdb daemon starts and accepts connections"
INFO running step "start daemon on localhost"
INFO running setup from /tmp/tmp.vh0AHoVe3H/setup
INFO daemon started as pid 14 with: /usr/bin/rethinkdb --directory /tmp/rethinkdb-data --no-http-admin --bind all
INFO looking for 3 lines in output within 30 seconds
INFO found within 2 seconds: Loading data from directory
INFO found within 2 seconds: Listening for intracluster connections
INFO found within 2 seconds: Listening for client driver connections
INFO > Recursively removing directory /tmp/rethinkdb-data/tmp
INFO > Initializing directory /tmp/rethinkdb-data
INFO > Running rethinkdb 2.4.4-dirty (aarch64-unknown-linux-gnu) (GCC 12.5.0)...
INFO > Running on Linux 7.0.0-1011-gcp aarch64
INFO > Loading data from directory /tmp/rethinkdb-data
INFO > Listening for intracluster connections on port 29015
INFO > Listening for client driver connections on port 28015
INFO > Administrative HTTP connections are disabled.
INFO > Listening on cluster addresses: 127.0.0.1, 172.17.0.2, ::1
INFO > Listening on driver addresses: 127.0.0.1, 172.17.0.2, ::1
INFO > Listening on http addresses: 127.0.0.1, 172.17.0.2, ::1
INFO > Server ready, "5c2e7d9cc8a1_d8f" d10c0827-8b50-48b1-b95a-84a43f66d011
INFO found 3 of expected 3 lines in output.
INFO found 0 / 9 error strings in output.
INFO twk: SIGTERM sent to pid 14. kill returned 0.
INFO twk: pid 14 exited within 2 seconds after SIGTERM
INFO running step "RethinkDB runs on challoc (the compat libjemalloc.so.2)"
WARN + readelf -d /usr/bin/rethinkdb
WARN + apk info --who-owns /usr/lib/libjemalloc.so.2
INFO /usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
WARN + apk info --who-owns /usr/lib/libjemalloc.so.2
WARN WARNING: opening /out/packages: No such file or directory
WARN WARNING: opening from cache https://packages.wolfi.dev/os: No such file or directory
WARN + rethinkdb --directory /tmp/rdb/data --no-http-admin --bind all --daemon --pid-file /tmp/rdb/pid --log-file /tmp/rdb/log
INFO Recursively removing directory /tmp/rdb/data/tmp
WARN + break
WARN + '[' -d /proc/27 ]
INFO /usr/lib/libjemalloc.so.2
WARN + '[' /usr/lib/libjemalloc.so.2 '=' /usr/lib/libjemalloc.so.2 ]
INFO 2026-09-20T23:24:13.762531074 0.019405s notice: Running rethinkdb 2.4.4-dirty (aarch64-unknown-linux-gnu) (GCC 12.5.0)...
INFO 2026-09-20T23:24:13.782316462 0.039191s notice: Listening for client driver connections on port 28015
WARN + '[' -d /proc/27 ]
WARN + '[' -d /proc/27 ]
WARN + break
WARN + '[' -d /proc/27 ]
INFO 2026-09-20T23:24:13.746129122 0.003004s notice: Recursively removing directory /tmp/rdb/data/tmp
INFO 2026-09-20T23:24:13.762328878 0.019205s notice: Initializing directory /tmp/rdb/data
INFO 2026-09-20T23:24:13.762476098 0.019350s info: Creating a default database for your convenience. (This is because you ran 'rethinkdb' without 'create', 'serve', or '--join', and the directory '/tmp/rdb/data' did not already exist or is empty.)
INFO 2026-09-20T23:24:13.762531074 0.019405s notice: Running rethinkdb 2.4.4-dirty (aarch64-unknown-linux-gnu) (GCC 12.5.0)...
INFO 2026-09-20T23:24:13.764377789 0.021252s notice: Running on Linux 7.0.0-1011-gcp aarch64
INFO 2026-09-20T23:24:13.764389106 0.021263s notice: Loading data from directory /tmp/rdb/data
INFO 2026-09-20T23:24:13.781064819 0.037939s info: Automatically using cache size of 30677 MB
INFO 2026-09-20T23:24:13.781781627 0.038656s notice: Listening for intracluster connections on port 29015
INFO 2026-09-20T23:24:13.782316462 0.039191s notice: Listening for client driver connections on port 28015
INFO 2026-09-20T23:24:13.782342878 0.039217s notice: Administrative HTTP connections are disabled.
INFO 2026-09-20T23:24:13.782344339 0.039218s notice: Listening on cluster addresses: 127.0.0.1, 172.17.0.2, ::1
INFO 2026-09-20T23:24:13.782346286 0.039220s notice: Listening on driver addresses: 127.0.0.1, 172.17.0.2, ::1
INFO 2026-09-20T23:24:13.782347893 0.039222s notice: Listening on http addresses: 127.0.0.1, 172.17.0.2, ::1
INFO 2026-09-20T23:24:13.782349434 0.039223s notice: Server ready, "5c2e7d9cc8a1_9ew" 4158f20e-ba4f-41c5-94d7-ae0c7d1aa5c3
INFO 2026-09-20T23:24:13.961464877 0.218339s notice: Server got SIGTERM from pid 2, uid 0; shutting down...
INFO 2026-09-20T23:24:13.961477207 0.218351s notice: Shutting down client connections...
INFO 2026-09-20T23:24:13.961527242 0.218401s notice: All client connections closed.
INFO 2026-09-20T23:24:13.961528624 0.218403s notice: Shutting down storage engine... (This may take a while if you had a lot of unflushed data in the writeback cache.)
INFO 2026-09-20T23:24:13.961762722 0.218637s notice: Storage engine shut down.
```
x86_64: the same steps and outputs (pass; `twk: pid ... exited within 2 seconds after SIGTERM`, `found 0 / 9 error strings in output`, the same maps line and shutdown lines).

## Clean-container installation, the checks by hand and the smoke test (aarch64, verbatim)

`apk add rethinkdb binutils apk-tools python3 py3-pip procps` from the local repository with Wolfi's still configured, then the server, the PyPI driver (`rethinkdb` 2.4.10.post1) and the smoke test: `server_status`, `db_create`, `table_create`, 20 000 inserts of 100-1000 byte documents, `count`, `get`, `sum`, a filtered `delete` of half, `count` again; RSS before and after; `SIGTERM` and the log's shutdown lines; the last `0` is the count of `challoc:`/crash/Segmentation lines in the log:
```
== 23:24:03 container aarch64
== packages
challoc-jemalloc-compat-0.1.0-r0 - challoc under jemalloc's soname (libjemalloc.so.2), a drop-in for libjemalloc2
rethinkdb-2.4.4-r15 - The open-source database for the realtime web.
/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
challoc-jemalloc-compat
libjemalloc2 INSTALLED
== binary
rethinkdb 2.4.4-dirty (aarch64-unknown-linux-gnu) (GCC 12.5.0)
Linux 7.0.0-1011-gcp aarch64
 0x0000000000000001 (NEEDED) Shared library: [libcurl.so.4]
 0x0000000000000001 (NEEDED) Shared library: [libssl.so.3]
 0x0000000000000001 (NEEDED) Shared library: [libcrypto.so.3]
 0x0000000000000001 (NEEDED) Shared library: [libz.so.1]
 0x0000000000000001 (NEEDED) Shared library: [libjemalloc.so.2]
 0x0000000000000001 (NEEDED) Shared library: [libstdc++.so.6]
 0x0000000000000001 (NEEDED) Shared library: [libm.so.6]
 0x0000000000000001 (NEEDED) Shared library: [libgcc_s.so.1]
 0x0000000000000001 (NEEDED) Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED) Shared library: [ld-linux-aarch64.so.1]
/usr/bin/rethinkdb: NEEDED libcurl.so.4 libssl.so.3 libcrypto.so.3 libz.so.1 libjemalloc.so.2 libstdc++.so.6 libm.so.6 libgcc_s.so.1 libc.so.6 ld-linux-aarch64.so.1 
  ok      libjemalloc.so.2
                 U free
                 U malloc
                 U malloc_usable_size
    70: 000000000003e0b0    12 FUNC    GLOBAL DEFAULT   10 challoc_version
== server
Recursively removing directory /tmp/rdb/data/tmp
pid 48
/usr/lib/libjemalloc.so.2
[anon:challoc:classes]
[anon:challoc:huge]
[anon:challoc:meta]
VmRSS:	   33504 kB
== driver smoke test (pypi rethinkdb driver)
WARNING: Running pip as the 'root' user can result in broken permissions and conflicting behaviour with the system package manager, possibly rendering your system unusable. It is recommended to use a virtual environment instead: https://pip.pypa.io/warnings/venv. Use the --root-user-action option if you know what you are doing and want to suppress this warning.
driver 2.4.10.post1+source python 3.13.15
server rethinkdb.net.DefaultCursor (done streaming): 
[{'name': 'ca2f173c4661_sqr',
  'process': {'pid': 48,
              'version': 'rethinkdb 2.4.4-dirty (aarch64-unknown-linux-gnu) '
                         '(GCC 12.5.0)'}}]
insert {'inserted': 20000, 'errors': 0}
count 20000
get 12345 745 12345
sum 199990000 expected 199990000
delete 10000 count 10000
VmRSS:	  178216 kB
== clean shutdown
exited
2026-09-20T23:24:13.036060830 0.879222s info: Table 4058886c-009e-453a-9c35-de2bb0da0f12: Starting a new Raft election for term 1.
2026-09-20T23:24:13.036237513 0.879399s info: Table 4058886c-009e-453a-9c35-de2bb0da0f12: Added replica on this server.
2026-09-20T23:24:13.038274548 0.881436s info: Table 4058886c-009e-453a-9c35-de2bb0da0f12: This server is Raft leader for term 1. Latest log index is 0.
2026-09-20T23:24:16.957246503 4.800408s notice: Server got SIGTERM from pid 1, uid 0; shutting down...
2026-09-20T23:24:16.957252740 4.800414s notice: Shutting down client connections...
2026-09-20T23:24:16.957289614 4.800451s notice: All client connections closed.
2026-09-20T23:24:16.957291088 4.800452s notice: Shutting down storage engine... (This may take a while if you had a lot of unflushed data in the writeback cache.)
2026-09-20T23:24:16.962349153 4.805511s notice: Storage engine shut down.
0
== 23:24:17 done
EXIT 0
```
x86_64: the same output (`rethinkdb 2.4.4-dirty (x86_64-unknown-linux-gnu) (GCC 12.5.0)`, `insert {'inserted': 20000, 'errors': 0}`, `count 20000`, `get 12345 745 12345`, `sum 199990000`, `delete 10000 count 10000`, exited on SIGTERM, `Storage engine shut down.`, 0 crash lines).

## Headline results

- Wolfi's RethinkDB 2.4.4 builds against `challoc-jemalloc-compat-dev` with two configure flags spelled out (`--with-jemalloc`, the default, and `--dynamic jemalloc`, because the compat package has no `libjemalloc.a`), on aarch64 and x86_64; the build is 5-6 minutes on 16 vCPUs, lints clean.
- `melange test` passes on both architectures with Wolfi's own checks plus the allocator checks: `DT_NEEDED libjemalloc.so.2`, resolved to `/usr/lib/libjemalloc.so.2` owned by `challoc-jemalloc-compat`, no `libjemalloc2`, the running server maps exactly that file, clean shutdown on SIGTERM.
- The smoke test with the Python driver (create, insert 20 k, read, aggregate, delete) runs on challoc on both architectures, RSS about 180 MB for the 20 k-document table, clean shutdown.
- What changes for Wolfi: the package gains a runtime dependency on `challoc-jemalloc-compat` (it had none for the allocator: jemalloc was static) and `--dynamic jemalloc`; nothing in RethinkDB calls jemalloc's API, so no source change and no header question. The `provides: jemalloc-dev` of the -dev package was not exercised here (the environment names `challoc-jemalloc-compat-dev` directly), as for Ruby.

## Not done, and why

- RethinkDB has no allocator statistics command (nothing like `MEMORY MALLOC-STATS`; the `rethinkdb.stats` system table carries no allocator figures), so the allocator is proven by the loader's maps, the package database and the binary's `DT_NEEDED`, as Ruby's package does.
- The table create / insert / read smoke test is by hand (the PyPI driver), not in the package test: Wolfi does not package a RethinkDB client driver, and a package test that pip-installs from the network is not Wolfi's style. The package test keeps Wolfi's `daemon-check-output` plus the allocator checks and a clean-shutdown check.
- RethinkDB's own test suite (`test/run`) was not run: it needs the Python driver and hours; out of scope for a packaging lane.
- No performance numbers.

## Diff against Wolfi's `rethinkdb.yaml` (8c473de)

```diff
--- wolfi-dev/os rethinkdb.yaml (8c473de)	2026-09-20 18:54:45
+++ contrib/wolfi/rethinkdb/rethinkdb.yaml	2026-09-20 19:15:54
@@ -1,10 +1,36 @@
+# rethinkdb on challoc: the Wolfi package definition with the allocator
+# switched from Wolfi's jemalloc to challoc through challoc-jemalloc-compat
+# (docs/plan.md Phase 3 task 3, docs/design.md section 13.6). Based on
+# wolfi-dev/os rethinkdb.yaml at 8c473de (2026-09-20; the file itself last
+# changed in d3dc8df, the glibc 2.44 transition); contrib/wolfi/README.md
+# ("RethinkDB") lists every change from it, and
+# bench/results/2026-09-20-phase3-task3-rethinkdb-package.md the build.
+#
+# Ruby's shape: RethinkDB's configure defaults to jemalloc on Linux
+# (--with-jemalloc), probes <jemalloc/jemalloc.h> for JEMALLOC_VERSION_MAJOR
+# >= 4 and links -ljemalloc, so the build environment takes
+# challoc-jemalloc-compat-dev in place of jemalloc-dev (the same file set)
+# and the package names challoc-jemalloc-compat at run time. One
+# difference from Ruby: configure links jemalloc *statically* by default
+# (default_static="tcmalloc jemalloc", libjemalloc.a from jemalloc-dev's
+# split/dev), which is how Wolfi's rethinkdb carries jemalloc 5.3.1 today
+# with no DT_NEEDED for it. The compat package ships no static library
+# (the Phase 1 decision holds for the system packages until a static link
+# is tested), so --dynamic jemalloc makes the link -ljemalloc and the
+# binary record libjemalloc.so.2, which is what the swap needs anyway.
 package:
   name: rethinkdb
   version: 2.4.4
-  epoch: 14 # glibc: 2.43->2.44
+  epoch: 15 # allocator: jemalloc (static) -> challoc (challoc-jemalloc-compat, dynamic); 14 was glibc 2.43->2.44
   description: The open-source database for the realtime web.
   copyright:
     - license: Apache-2.0
+  dependencies:
+    runtime:
+      # melange derives so:libjemalloc.so.2 from the binary; apk resolves a
+      # bare so: name to the package that carries it (libjemalloc2) over a
+      # provider, so the package names what it was built and tested against.
+      - challoc-jemalloc-compat
   resources:
     cpu: "2"
     memory: 2Gi
@@ -18,15 +44,16 @@
       - abseil-cpp
       - abseil-cpp-dev
       - bash
+      - binutils # readelf for the DT_NEEDED check of the built binary
       - boost-dev
       - build-base
       - busybox
       - ca-certificates-bundle
+      - challoc-jemalloc-compat-dev # in place of jemalloc-dev: the same file set, challoc underneath
       - curl-dev
       - gcc-12
       - gcc-12-default
       - glibc-2.44-dev
-      - jemalloc-dev
       - m4
       - openssl
       - openssl-dev
@@ -44,13 +71,36 @@
   - runs: |
       # This build requires a specific version of protoc that doesn't work with ours, so use --allow-fetch
       # We can't use our autoconf/configure pipeline because this build barfs on the --host and --libdir options.
+      # --with-jemalloc is the Linux default, spelled out; --dynamic jemalloc
+      # because the compat package has no libjemalloc.a (see the header).
       ./configure \
         --prefix=/usr \
         --sysconfdir=/etc \
-        --fetch protoc
+        --fetch protoc \
+        --with-jemalloc \
+        --dynamic jemalloc
+      grep -E '^(ALLOCATOR|STATIC_MALLOC|JEMALLOC_LIBS|HAS_JEMALLOC)\b' config.mk
+      grep -E '^ALLOCATOR\b' config.mk | grep jemalloc > /dev/null
+      grep -E '^STATIC_MALLOC\b' config.mk | grep -vE '1|true' > /dev/null
+      grep -q '^#define JEMALLOC_CHALLOC 1' /usr/include/jemalloc/jemalloc.h
 
   - uses: autoconf/make
 
+  - name: Check the built server is on challoc's libjemalloc.so.2
+    runs: |
+      set -eux
+      bin=$(find build -maxdepth 2 -type f -name rethinkdb | head -1)
+      [ -n "$bin" ]
+      readelf -d "$bin" | grep NEEDED
+      readelf -d "$bin" | grep 'NEEDED.*\[libjemalloc.so.2\]' > /dev/null
+      # No jemalloc statically linked in: its internal symbols are absent.
+      ! nm "$bin" 2>/dev/null | grep -E ' (je_)?arena_init$' > /dev/null
+      # The environment's libjemalloc.so.2 is challoc's (a build environment
+      # has no apk, so the file is asked).
+      readelf -d /usr/lib/libjemalloc.so.2 | grep 'Library soname: \[libjemalloc.so.2\]' > /dev/null
+      readelf --dyn-syms /usr/lib/libjemalloc.so.2 | grep ' challoc_version$' > /dev/null
+      "$bin" --version
+
   - uses: autoconf/make-install
 
   - uses: strip
@@ -62,6 +112,11 @@
     strip-prefix: v
 
 test:
+  environment:
+    contents:
+      packages:
+        - apk-tools
+        - binutils
   pipeline:
     - uses: test/tw/ldd-check
     # AUTOGENERATED
@@ -78,18 +133,32 @@
           Loading data from directory
           Listening for intracluster connections
           Listening for client driver connections
-          Server ready
-        timeout: "60"
-        post: |
-          wait-for-it localhost:28015 -t 10
-          echo "SUCCESS: RethinkDB is listening for client connections on port 28015"
-        error_strings: ""
-
-subpackages:
-  - name: rethinkdb-doc
-    description: rethinkdb documentation
-    pipeline:
-      - uses: split/manpages
-    test:
-      pipeline:
-        - uses: test/docs
+    - name: RethinkDB runs on challoc (the compat libjemalloc.so.2)
+      runs: |
+        set -eux
+        readelf -d /usr/bin/rethinkdb | grep 'NEEDED.*\[libjemalloc.so.2\]' > /dev/null
+        ldd /usr/bin/rethinkdb | grep libjemalloc.so.2
+        ldd /usr/bin/rethinkdb | grep -E 'libjemalloc.so.2 => /(usr/)?lib/libjemalloc.so.2' > /dev/null
+        apk info --who-owns /usr/lib/libjemalloc.so.2
+        apk info --who-owns /usr/lib/libjemalloc.so.2 | grep challoc-jemalloc-compat > /dev/null
+        ! apk info -e libjemalloc2
+        readelf --dyn-syms /usr/lib/libjemalloc.so.2 | grep ' challoc_version$' > /dev/null
+        rm -rf /tmp/rdb && mkdir -p /tmp/rdb
+        rethinkdb --directory /tmp/rdb/data --no-http-admin --bind all --daemon --pid-file /tmp/rdb/pid --log-file /tmp/rdb/log
+        for i in $(seq 1 100); do
+          grep -q 'Listening for client driver connections' /tmp/rdb/log 2>/dev/null && break
+          sleep 0.2
+        done
+        pid=$(cat /tmp/rdb/pid)
+        [ -d /proc/$pid ]
+        # The one libjemalloc mapped in the server is the compat package's.
+        awk '$6 ~ /libjemalloc/ {print $6}' /proc/$pid/maps | sort -u
+        [ "$(awk '$6 ~ /libjemalloc/ {print $6}' /proc/$pid/maps | sort -u)" = /usr/lib/libjemalloc.so.2 ]
+        grep -E 'Running rethinkdb|Listening for client driver' /tmp/rdb/log
+        # Clean shutdown on SIGTERM.
+        kill -TERM $pid
+        for i in $(seq 1 150); do [ -d /proc/$pid ] || break; sleep 0.2; done
+        ! [ -d /proc/$pid ]
+        cat /tmp/rdb/log
+        grep -qi 'shutting down\|Storage engine shut down' /tmp/rdb/log
+        ! grep -iE 'challoc:|crash|Segmentation' /tmp/rdb/log
```
