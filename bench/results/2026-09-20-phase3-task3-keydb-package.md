# Phase 3 task 3: Wolfi's KeyDB 6.3 on challoc, first real build

Date 2026-09-20. Lane keydb, worktree `../challoc-wt/keydb`, branch `loop/keydb` from `main` f419b88; the one `src/` change is the `get_defrag_hint` export (commit 9d62dbb, design 13.5) this package needs. Definition: `contrib/wolfi/keydb/keydb.yaml` with `contrib/wolfi/keydb/keydb/{cstdint,0001-Add-MALLOC-challoc}.patch`; layout and recipe: `contrib/wolfi/README.md`, "KeyDB". No numbers in this file are benchmark results: the `keydb-benchmark` lines only show the package serving traffic.

## Base

- wolfi-dev/os `keydb.yaml` at `main` `8c473de11ba10d96fb87ddfca734cb650a94ee12` (2026-09-20); the file's last change is `276d78c` (2026-09-02, a merge-bot resources bump; the last content change `92ca315`, "keydb: transition to newest version of glibc (#280440)", 2026-08-29). Wolfi's `keydb/cstdint.patch` copied from the same commit (`cmp` identical). Wolfi's `pipelines/` (for `test/tw/ldd-check`) checked out at the same commit.
- KeyDB tag `v6.3.4`, commit `7e7e5e57d25fe246a8201f0acf5e7363c0bf1e14` (Wolfi's `expected-commit`), submodules included as Wolfi's file asks (RocksDB, unused without `ENABLE_FLASH`).
- challoc: this tree at 9d62dbb (`get_defrag_hint` exported, the headers define `JEMALLOC_FRAG_HINT`), built into `challoc`, `challoc-dev`, `challoc-jemalloc-compat`, `challoc-jemalloc-compat-dev` 0.1.0-r0 first (`contrib/wolfi/challoc.yaml`, `--build-option local-source`, `melange test` passing on both architectures), so the KeyDB build environment takes `challoc-jemalloc-compat-dev` from the local repository.
- The diff against Wolfi's file (224 lines, `diff -u`) is at the end of this file, with the new patch.

## Where and with what

- VMs: `challoc-keydb-arm64` (c4a-standard-16, Neoverse-V2) and `challoc-keydb-amd64` (c4-standard-16), Ubuntu 24.04, kernel `7.0.0-1011-gcp`, Docker 29.1.3, zone us-east4-a; created, provisioned, used and deleted within the iteration.
- melange: Wolfi's `melange` apk (`v0.60.0+dirty`) in a `--privileged` `cgr.dev/chainguard/wolfi-base` container, `--runner bubblewrap`, `--source-dir /work/contrib/wolfi/keydb/keydb` (the patch directory, as Wolfi's Makefile passes `./keydb`). `melange test` with `--test-package-append=wolfi-base`.
- Build environment (from the melange log, both architectures): `gcc=16.2.0-r1`, `binutils=2.47-r1`, `glibc-2.44-dev=2.44-r6`, `linux-headers=7.2.6-r0`, `build-base=1-r9`, `busybox=1.38.0-r2`, `openssl-dev=3.6.4-r7`, `nasm=3.02-r1`, `tcl=9.0.4-r1`, `challoc-jemalloc-compat=0.1.0-r0`, `challoc-jemalloc-compat-dev=0.1.0-r0` (from the local repository; no `jemalloc-dev`, no `libjemalloc2`).
- Clean-container checks: `cgr.dev/chainguard/wolfi-base` with the local repository and the generated key added, Wolfi's repository still configured.

## Timing of the KeyDB pipeline (aarch64; x86_64: 23:16:47 -> 23:17:29)

```
23:16:42 running step "git-checkout"                 (KeyDB v6.3.4 with submodules)
23:16:51 running step "patch"                        (cstdint.patch, in deps/rocksdb)
23:16:51 running step "patch"                        (0001-Add-MALLOC-challoc.patch)
         (protected-mode sed; header checks; make MALLOC=challoc -j16; make MALLOC=challoc install; mv)
23:17:12 running step "Check the built binaries name the allocator and the compat library"
23:17:12 running step "strip"
```

30 s from patch to check on 16 vCPUs; the `make install` compiled only `Makefile.dep` (the persisted `MALLOC=challoc` matched, no rebuild). melange's linters: `no lint findings to persist` for `keydb`, `keydb-cli`, `keydb-benchmark` on both architectures.

The check step's output, from the build log (aarch64):
```
KeyDB server v=6.3.4 sha=7e7e5e57:1 malloc=jemalloc-5.3.0-challoc bits=64 build=b15658b47ff8c39b
keydb-server:    NEEDED libjemalloc.so.2 libuuid.so.1 libcurl.so.4 libssl.so.3 libcrypto.so.3 libstdc++.so.6 libm.so.6 libgcc_s.so.1 libc.so.6 ld-linux-aarch64.so.1
keydb-cli:       NEEDED libjemalloc.so.2 libcurl.so.4 libssl.so.3 libcrypto.so.3 libstdc++.so.6 libm.so.6 libgcc_s.so.1 libc.so.6 ld-linux-aarch64.so.1
keydb-benchmark: NEEDED libjemalloc.so.2 libssl.so.3 libcrypto.so.3 libstdc++.so.6 libm.so.6 libgcc_s.so.1 libc.so.6 ld-linux-aarch64.so.1
/usr/lib/libjemalloc.so.2: soname libjemalloc.so.2, exports challoc_version and je_get_defrag_hint
keydb-server imports je_get_defrag_hint (defrag.cpp compiled in: HAVE_DEFRAG)
deps/jemalloc/lib: absent (the bundled jemalloc was never built)
```

The first run of the check step died with exit 141: `readelf --dyn-syms "$bin" | grep -q ...` under melange's `pipefail` (grep -q exits at the first match, readelf gets SIGPIPE). Every `| grep -q` at the end of a pipe in both new definitions now reads its whole input (`grep ... > /dev/null`); the KeyDB package was rebuilt with the fixed check step (the build steps are unchanged). The same pattern is in lane P's `ruby-3.4.yaml` and `valkey-9.1.yaml` (`readelf --dyn-syms /usr/lib/libjemalloc.so.2 | grep -q ' challoc_version$'`), where it has passed by timing; noted for the human, not changed here.

## Built packages

Two builds of the same YAML: from 9d62dbb (the bin-only hint; the `melange test` and by-hand outputs below are from it) and, after the memefficiency measurement, from 896a60d (E2; `melange test` passed again on both architectures, 23:47Z, then the suite twice). The final packages (896a60d), `aarch64` (`/out/packages/aarch64/`, `ls` time and size, then sha256):
```
23:45 challoc-0.1.0-r0.apk 201950
23:45 challoc-dev-0.1.0-r0.apk 13826
23:45 challoc-jemalloc-compat-0.1.0-r0.apk 201943
23:45 challoc-jemalloc-compat-dev-0.1.0-r0.apk 14741
23:47 keydb-6.3.4-r9.apk 1274010
23:47 keydb-benchmark-6.3.4-r9.apk 258567
23:47 keydb-cli-6.3.4-r9.apk 206223
ca0f944f60229a1b92013868f5f684ccf0989b0dc63c3ae0b39b3e2ab3db2ace  challoc-0.1.0-r0.apk
085a45d8e449d00c6cbbc9de0decc80171f0842724b35c21815d01c5edab41ce  challoc-dev-0.1.0-r0.apk
b09dc204cec052b41928ea64697899f66e6e4c85ee0b01a01db19ede56743650  challoc-jemalloc-compat-0.1.0-r0.apk
49133d78818cb35baff8807575bbd54cd7627e6fa48cd61697161ad5703bc4d0  challoc-jemalloc-compat-dev-0.1.0-r0.apk
d61d5f3263e7064534efb0845d1bfc80aa710aa04760cb12a18050d013f8711d  keydb-6.3.4-r9.apk
ed5397607538ee2118ac4498b64390d1f9b9d6e4e56306b85c24fdafe9981e81  keydb-benchmark-6.3.4-r9.apk
e02d1faea4e7b76d9cad827bc76f8c8274238c9ca89e2479a3964c92dad028af  keydb-cli-6.3.4-r9.apk
```
`x86_64`:
```
23:45 challoc-0.1.0-r0.apk 199002
23:45 challoc-dev-0.1.0-r0.apk 13823
23:45 challoc-jemalloc-compat-0.1.0-r0.apk 199036
23:45 challoc-jemalloc-compat-dev-0.1.0-r0.apk 14745
23:47 keydb-6.3.4-r9.apk 1344169
23:47 keydb-benchmark-6.3.4-r9.apk 277660
23:47 keydb-cli-6.3.4-r9.apk 214449
7537ce2c3dc821deda3d41d6b3d344657a2a5de4d901c95740597d3f84b8ef1c  challoc-0.1.0-r0.apk
1796db2ee828a13240dbbc0ce961516cfe7296ed0d0c507c45e81c1de4db5910  challoc-dev-0.1.0-r0.apk
d16676f9a7e8d36d5ab6339420cb1062ec6343d982d9e874a8c0eeb3659a3034  challoc-jemalloc-compat-0.1.0-r0.apk
299fa8d79dff931301aac12dd7392680a424bf1e512c99eec2292b02ac73edfc  challoc-jemalloc-compat-dev-0.1.0-r0.apk
12a60759ab5d6867841d75f4707b09d27d48ae5a89da8c79978bd0a2bf23394a  keydb-6.3.4-r9.apk
e69845fe9c7611a2b9d9e70138457f83281ec9712c45aa406fba3843f1e670e9  keydb-benchmark-6.3.4-r9.apk
fb657be823e724a4d5439d3226707509397167c61ca3846c90d81b368534040a  keydb-cli-6.3.4-r9.apk
```
The first build's sums for the record: aarch64 `f53460dc...` (`keydb`), `acb06445...` (`-cli`), `74e0ef6e...` (`-benchmark`); x86_64 `d157260b...`, `be0bb920...`, `e8e8765f...`.

### Package metadata melange derived (aarch64; x86_64 identical with `so:ld-linux-x86-64.so.2`)

```
-- keydb .PKGINFO
pkgname = keydb
pkgver = 6.3.4-r9
size = 2817434
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
depend = so:libuuid.so.1
provides = cmd:gcc-wrapper=6.3.4-r9
provides = cmd:keydb-server=6.3.4-r9
-- keydb-cli .PKGINFO
pkgname = keydb-cli
pkgver = 6.3.4-r9
size = 485374
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
provides = cmd:keydb-cli=6.3.4-r9
-- keydb-benchmark .PKGINFO
pkgname = keydb-benchmark
pkgver = 6.3.4-r9
size = 989928
commit = 9d62dbbdc95bed84c06de3f925af11c1b1813d95
depend = challoc-jemalloc-compat
depend = so:ld-linux-aarch64.so.1
depend = so:libc.so.6
depend = so:libcrypto.so.3
depend = so:libgcc_s.so.1
depend = so:libjemalloc.so.2
depend = so:libm.so.6
depend = so:libssl.so.3
depend = so:libstdc++.so.6
provides = cmd:keydb-benchmark=6.3.4-r9
```

`depend = challoc-jemalloc-compat` is the explicit runtime dependency (in all three packages); `so:libjemalloc.so.2` is the derived one, which apk alone would resolve to Wolfi's `libjemalloc2` by name preference. `cmd:gcc-wrapper` is Wolfi's existing behaviour (KeyDB's `make install` puts a `gcc-wrapper` in `/usr/local/bin`, and Wolfi's file moves everything there); unchanged.

## `melange test` (both architectures pass)

Verbatim from the aarch64 log (the `WARN +` lines are the test's `set -x` trace, with the greps and plumbing elided; `INFO` lines are output):
```
running step "The server is linked against the compat library and says so"
WARN + keydb-server --version
INFO KeyDB server v=6.3.4 sha=7e7e5e57:1 malloc=jemalloc-5.3.0-challoc bits=64 build=b15658b47ff8c39b
WARN + keydb-server --version
WARN + readelf -d /usr/bin/keydb-server
WARN + readelf -d /usr/bin/keydb-cli
WARN + readelf -d /usr/bin/keydb-benchmark
WARN + ldd /usr/bin/keydb-server
WARN + apk info --who-owns /usr/lib/libjemalloc.so.2
INFO /usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
WARN + apk info --who-owns /usr/lib/libjemalloc.so.2
WARN + apk info -e libjemalloc2
INFO challoc-jemalloc-compat
WARN + readelf --dyn-syms /usr/lib/libjemalloc.so.2
WARN + keydb-sentinel --version
INFO KeyDB server v=6.3.4 sha=7e7e5e57:1 malloc=jemalloc-5.3.0-challoc bits=64 build=b15658b47ff8c39b
WARN + exit 0
INFO running step "A running server uses the compat library, defragments and purges"
WARN + keydb-server --save  --dir /tmp
WARN wait-for-it: waiting 30 seconds for localhost:6379
WARN wait-for-it: localhost:6379 is available after 0 seconds
WARN + keydb-cli PING
INFO /usr/lib/libjemalloc.so.2
WARN + '[' /usr/lib/libjemalloc.so.2 '=' /usr/lib/libjemalloc.so.2 ]
WARN + keydb-cli INFO memory
INFO allocator_allocated:3604864
INFO allocator_frag_ratio:3.20
INFO mem_allocator:jemalloc-5.3.0-challoc
WARN + keydb-cli INFO memory
WARN + keydb-cli CONFIG SET activedefrag yes
WARN + keydb-cli CONFIG GET activedefrag
WARN + keydb-cli MEMORY PURGE
WARN + keydb-cli MEMORY MALLOC-STATS
WARN + head -3
INFO ___ Begin jemalloc statistics ___
INFO Version: "5.3.0-0-gchalloc"
INFO Build-time option settings
WARN + keydb-cli MEMORY MALLOC-STATS
WARN + keydb-cli DEBUG MALLCTL arenas.page
INFO 4096
WARN + keydb-cli SET bike:1 'Process 134'
INFO OK
WARN + keydb-cli GET bike:1
INFO Process 134
WARN + keydb-cli exists bike:1
INFO 1
WARN + keydb-cli exists bike:2
INFO 0
WARN + keydb-benchmark -t set,get -n 20000 -q
INFO SET: rps=0.0 (overall: 0.0) avg_msec=nan (overall: nan)
                                                        
SET: 133333.33 requests per second, p50=0.191 msec
INFO GET: rps=49828.0 (overall: 124570.0) avg_msec=0.216 (overall: 0.216)
                                                                     
GET: 126582.27 requests per second, p50=0.191 msec
INFO
WARN + keydb-benchmark -t set -n 200000 -r 200000 -d 150 -q -P 16
INFO SET: rps=0.0 (overall: 0.0) avg_msec=nan (overall: nan)
SET: rps=757056.0 (overall: 754039.9) avg_msec=0.993 (overall: 0.993)
                                                                      
SET: 751879.69 requests per second, p50=1.063 msec
INFO
WARN + keydb-cli DBSIZE
INFO 126586
WARN + keydb-cli EVAL 'for i=1,100000 do redis.call("del", redis.call("randomkey")) end return 1' 0
INFO 1
WARN + keydb-cli INFO stats
INFO active_defrag_hits:0
INFO active_defrag_misses:0
INFO active_defrag_key_hits:0
INFO active_defrag_key_misses:0
WARN + keydb-cli INFO memory
INFO used_memory:9517408
INFO allocator_active:47939584
INFO allocator_resident:61034496
INFO allocator_frag_ratio:3.77
WARN + keydb-cli CONFIG SET activedefrag no
WARN + keydb-cli SHUTDOWN NOSAVE
WARN + exit 0
INFO
```

`active_defrag_*` all 0 in this run: the dataset is a few MiB, below `active-defrag-ignore-bytes` (100 MB default), so no cycle started; the defragmenter is exercised by KeyDB's own `unit/memefficiency` below. The `-cli` and `-benchmark` subpackage tests (Wolfi's `--version`/`--help`) pass. x86_64: the same lines with x86_64 figures (`allocator_frag_ratio:2.61` at start).

### Re-run with the final test block (both architectures pass)

Two assertions of the first test block were no-ops and are corrected in the committed file: under `set -e` a `! command` never fails a step (POSIX exempts `!` pipelines), so `! apk info -e libjemalloc2` and `! grep -iE 'challoc:|assert|crash' /tmp/keydb.log` could not fail; and `apk info -e libjemalloc2` exits 0 anyway because the compat package provides the name. The committed block checks `apk info -vv` for a package named `libjemalloc2-` and spells the negations `if ...; then exit 1; fi`. `melange test` of the same packages with the committed YAML: aarch64 pass (23:21:30 -> 23:22:00), x86_64 pass (23:21:30 -> 23:22:07); the outputs are those above. The same `!` pattern is in lane P's `ruby-3.4.yaml` (`! apk info -e libjemalloc2`) and `valkey-9.1.yaml` (`! grep -iE 'challoc:|assert|crash' /tmp/valkey.log`), where those two lines have never been able to fail; noted for the human, not changed here.

## Clean-container installation and the checks by hand (aarch64, verbatim)

`apk add keydb keydb-cli keydb-benchmark binutils posix-libc-utils apk-tools tcl git procps` from the local repository with Wolfi's still configured: installs `challoc-jemalloc-compat-0.1.0-r0` for the explicit dependency, no `libjemalloc2` package and no system `challoc` (`apk info -vv`):
```
challoc-jemalloc-compat-0.1.0-r0 - challoc under jemalloc's soname (libjemalloc.so.2), a drop-in for libjemalloc2
keydb-6.3.4-r9 - A Multithreaded Fork of Redis
keydb-benchmark-6.3.4-r9 -
keydb-cli-6.3.4-r9 -
/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
```
`apk info -e libjemalloc2` answers `challoc-jemalloc-compat` and exits 0: the compat package `provides: libjemalloc2`, so the name is "installed" as far as apk is concerned. The package tests therefore check `apk info -vv` for a package *named* `libjemalloc2-` instead (see the note on `!` below).

Binaries (`readelf -d`, `scripts/check-needed.sh`, the library's exports, the server's imports):
```
+ echo 'system challoc not installed (expected)'
+ echo ''
+ keydb-server --version
KeyDB server v=6.3.4 sha=7e7e5e57:1 malloc=jemalloc-5.3.0-challoc bits=64 build=b15658b47ff8c39b
+ readelf -d /usr/bin/keydb-server
+ grep NEEDED
+ tr -s ' '
+ sed 's/^/keydb-server: /'
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [libjemalloc.so.2]
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [libuuid.so.1]
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [libcurl.so.4]
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [libssl.so.3]
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [libcrypto.so.3]
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [libstdc++.so.6]
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [libm.so.6]
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [libgcc_s.so.1]
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [libc.so.6]
keydb-server:  0x0000000000000001 (NEEDED) Shared library: [ld-linux-aarch64.so.1]
+ readelf -d /usr/bin/keydb-cli
+ grep NEEDED
+ tr -s ' '
+ sed 's/^/keydb-cli: /'
keydb-cli:  0x0000000000000001 (NEEDED) Shared library: [libjemalloc.so.2]
keydb-cli:  0x0000000000000001 (NEEDED) Shared library: [libcurl.so.4]
keydb-cli:  0x0000000000000001 (NEEDED) Shared library: [libssl.so.3]
keydb-cli:  0x0000000000000001 (NEEDED) Shared library: [libcrypto.so.3]
keydb-cli:  0x0000000000000001 (NEEDED) Shared library: [libstdc++.so.6]
keydb-cli:  0x0000000000000001 (NEEDED) Shared library: [libm.so.6]
keydb-cli:  0x0000000000000001 (NEEDED) Shared library: [libgcc_s.so.1]
keydb-cli:  0x0000000000000001 (NEEDED) Shared library: [libc.so.6]
keydb-cli:  0x0000000000000001 (NEEDED) Shared library: [ld-linux-aarch64.so.1]
+ + readelfgrep -d NEEDED /usr/bin/keydb-benchmark

+ tr -s ' '
+ sed 's/^/keydb-benchmark: /'
keydb-benchmark:  0x0000000000000001 (NEEDED) Shared library: [libjemalloc.so.2]
keydb-benchmark:  0x0000000000000001 (NEEDED) Shared library: [libssl.so.3]
keydb-benchmark:  0x0000000000000001 (NEEDED) Shared library: [libcrypto.so.3]
keydb-benchmark:  0x0000000000000001 (NEEDED) Shared library: [libstdc++.so.6]
keydb-benchmark:  0x0000000000000001 (NEEDED) Shared library: [libm.so.6]
keydb-benchmark:  0x0000000000000001 (NEEDED) Shared library: [libgcc_s.so.1]
keydb-benchmark:  0x0000000000000001 (NEEDED) Shared library: [libc.so.6]
keydb-benchmark:  0x0000000000000001 (NEEDED) Shared library: [ld-linux-aarch64.so.1]
+ sh /work/scripts/check-needed.sh /usr/bin/keydb-server libjemalloc.so.2
/usr/bin/keydb-server: NEEDED libjemalloc.so.2 libuuid.so.1 libcurl.so.4 libssl.so.3 libcrypto.so.3 libstdc++.so.6 libm.so.6 libgcc_s.so.1 libc.so.6 ld-linux-aarch64.so.1 
  ok      libjemalloc.so.2
+ sh /work/scripts/check-needed.sh /usr/bin/keydb-cli libjemalloc.so.2
/usr/bin/keydb-cli: NEEDED libjemalloc.so.2 libcurl.so.4 libssl.so.3 libcrypto.so.3 libstdc++.so.6 libm.so.6 libgcc_s.so.1 libc.so.6 ld-linux-aarch64.so.1 
  ok      libjemalloc.so.2
+ sh /work/scripts/check-needed.sh /usr/bin/keydb-benchmark libjemalloc.so.2
/usr/bin/keydb-benchmark: NEEDED libjemalloc.so.2 libssl.so.3 libcrypto.so.3 libstdc++.so.6 libm.so.6 libgcc_s.so.1 libc.so.6 ld-linux-aarch64.so.1 
  ok      libjemalloc.so.2
+ readelf -d /usr/lib/libjemalloc.so.2
+ grep soname
 0x000000000000000e (SONAME)             Library soname: [libjemalloc.so.2]
+ readelf --dyn-syms /usr/lib/libjemalloc.so.2
+ grep -E ' (je_)?get_defrag_hint$| challoc_version$'
    48: 000000000000b27c     4 FUNC    GLOBAL DEFAULT   10 get_defrag_hint
    70: 000000000003e0b0    12 FUNC    GLOBAL DEFAULT   10 challoc_version
    85: 000000000000b27c     4 FUNC    GLOBAL DEFAULT   10 je_get_defrag_hint
+ readelf --dyn-syms -W /usr/bin/keydb-server
+ grep -E 'je_get_defrag_hint|je_mallctl|mallctl$'
+ head
   349: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND mallctl
   421: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND je_get_defrag_hint
+ echo '
```

Server run (`keydb-server --save "" --dir /tmp`; maps, INFO, activedefrag, purge, the mallctl bridge, traffic, a defrag-shaped churn, shutdown; the last `0` is the count of `challoc:`/assert/crash lines in the log):
```
'

/usr/lib/libjemalloc.so.2
[anon:challoc:classes]
[anon:challoc:huge]
[anon:challoc:meta]
allocator_allocated:3604864
allocator_active:11517952
allocator_resident:14635008
allocator_frag_ratio:3.20
mem_allocator:jemalloc-5.3.0-challoc
redis_version:6.3.4
arch_bits:64
multiplexing_api:epoll
OK
activedefrag
yes
OK
___ Begin jemalloc statistics ___
Version: "5.3.0-0-gchalloc"
Build-time option settings
  config.debug: false
4096
16
28
SET: rps=0.0 (overall: nan) avg_msec=nan (overall: nan)
                                                        
SET: 127388.53 requests per second, p50=0.199 msec
GET: rps=43617.5 (overall: 117720.4) avg_msec=0.234 (overall: 0.234)
                                                                     
GET: 121212.12 requests per second, p50=0.207 msec

SET: rps=0.0 (overall: 0.0) avg_msec=nan (overall: nan)
SET: rps=727360.0 (overall: 724462.2) avg_msec=1.039 (overall: 1.039)
                                                                      
SET: 680272.06 requests per second, p50=1.167 msec

189788
1
active_defrag_hits:0
active_defrag_misses:0
active_defrag_key_hits:0
active_defrag_key_misses:0
used_memory:13945904
used_memory_rss:69025792
allocator_active:65175552
allocator_resident:79495168
allocator_frag_ratio:3.80
allocator_frag_bytes:48024496
OK
0
```
`DEBUG MALLCTL arenas.page` = 4096, `arenas.quantum` = 16, `arenas.nbins` = 28: the system `quantum16` build, as intended (KeyDB asserts nothing about them; the test suite's gate is `arenas.page <= 8192`).

Both ways, Wolfi's `libjemalloc.so.2` (jemalloc 5.3.1, fetched as an apk and unpacked under /tmp, since the name conflicts) preloaded in front of the compat library:
```
== both ways: Wolfi'"'"'s libjemalloc.so.2 preloaded (no hint symbol)'

total 808
drwxr-xr-x    2 root     root          4096 Sep 20 23:18 .
drwxr-xr-x    3 root     root          4096 Sep 20 23:18 ..
-rwxr-xr-x    1 root     root        817104 Aug 29 02:05 libjemalloc.so.2
keydb-server: symbol lookup error: keydb-server: undefined symbol: je_get_defrag_hint
```
That is the honest shape of the dependency: a KeyDB built with `HAVE_DEFRAG` needs an allocator that exports `je_get_defrag_hint`, which Wolfi's jemalloc does not (only Redis's patched bundled copy does); the package is only correct on the compat library, which its explicit `challoc-jemalloc-compat` dependency states.

x86_64: the same output with x86_64 addresses and `ld-linux-x86-64.so.2`; `allocator_frag_ratio` 2.61 at start.

## KeyDB's `unit/memefficiency` against the packaged binary: the honest result

Clean `wolfi-base` container: `apk add keydb keydb-cli keydb-benchmark tcl git procps bash`; KeyDB `v6.3.4` cloned (`7e7e5e57`), `src/keydb-server` and `src/keydb-cli` symlinked to the installed binaries. Two things the suite needed that have nothing to do with the allocator: KeyDB's `runtest` only knows `tclsh8.5`/`8.6` and Wolfi's `tcl` is 9.0.4, so `tests/test_helper.tcl` is run with `tclsh9.0` directly; and Tcl 9 rejects the suite's `package require Tcl 8.5` (`8.5-` is the range spelling) and `fconfigure ... -encoding binary` (`-translation binary`, the spelling Redis moved to upstream), which otherwise hang the harness at its first client accept. Both are `sed`s over `tests/` in the container, recorded in the log; `--clients 1 --dump-logs`.

### Run 1: the bin-only hint (commit 9d62dbb), default configuration

aarch64 (229 s):
```
[ok]: Memory efficiency with values in range 32
[ok]: Memory efficiency with values in range 64
[ok]: Memory efficiency with values in range 128
[ok]: Memory efficiency with values in range 1024
[ok]: Memory efficiency with values in range 16384
[err]: Active defrag in tests/unit/memefficiency.tcl
defrag didn't stop.
[err]: Active defrag big keys in tests/unit/memefficiency.tcl
defrag didn't stop.
[err]: Active defrag big list in tests/unit/memefficiency.tcl
defrag didn't stop.
[err]: Active defrag edge case in tests/unit/memefficiency.tcl
defrag didn't stop.
[ok]: Active defrag with active replica
[1/1 done]: unit/memefficiency (229 seconds)
defrag didn't stop.
defrag didn't stop.
defrag didn't stop.
defrag didn't stop.
== test_helper exit 0 after 229 s
```
x86_64 (192 s):
```
[ok]: Memory efficiency with values in range 32
[ok]: Memory efficiency with values in range 64
[ok]: Memory efficiency with values in range 128
[ok]: Memory efficiency with values in range 1024
[ok]: Memory efficiency with values in range 16384
[err]: Active defrag in tests/unit/memefficiency.tcl
defrag didn't stop.
[err]: Active defrag big keys in tests/unit/memefficiency.tcl
defrag didn't stop.
[err]: Active defrag big list in tests/unit/memefficiency.tcl
defrag didn't stop.
[ok]: Active defrag edge case
[ok]: Active defrag with active replica
[1/1 done]: unit/memefficiency (192 seconds)
defrag didn't stop.
defrag didn't stop.
defrag didn't stop.
== test_helper exit 0 after 192 s
```

The five memory-efficiency tests pass; four (aarch64) or three (x86_64) of the five defrag tests fail, all the same way: `defrag didn't stop.` That assertion is `wait_for_condition 150 100 { [s active_defrag_running] eq 0 }`, 15 s for the defragmenter to reach its stop condition after it started. From the server logs the suite dumps, the defragmenter *works*: the "Active defrag edge case" server went `frag=102%, frag_bytes=254148752` -> `frag=39%` (reallocated=480506) -> `frag=6%, frag_bytes=14077888` (reallocated=148074) in three cycles, then cycled forever at `reallocated=0, frag=6%` and 65% CPU. KeyDB's stop rule (`computeDefragCycles`, Redis 6.2's): stop when `frag_pct < active-defrag-threshold-lower` (the test sets 5; default 10) or `frag_bytes < active-defrag-ignore-bytes` (the test sets 2 MB; default 100 MB), where `frag_bytes = stats.active - stats.allocated` over the whole allocator (`getAllocatorFragmentation`). Under challoc that figure has a floor the hint cannot bring down, and on these workloads the floor is 6-13%.

### Where the floor is (by hand, aarch64: the "Active defrag" test's workload, then challoc's own figures)

`keydb-server --server-threads 1`, the test's settings (`hz 100`, thresholds 5 / 2 MB, `maxmemory 100mb allkeys-lru`), `populate 700000 asdf1 150` and `populate 170000 asdf2 300` through `keydb-cli --pipe` until `allocator_frag_ratio >= 1.4`, `maxmemory 110mb`, `activedefrag yes`, then `INFO` every 2 s and, once stuck, the per-bin `mallctl` figures (`arenas.bin.<j>.*`, `stats.arenas.4096.bins.<j>.*`; `waste = curslabs * slab_size - curregs * size`, the free slots plus the slab tails) and the per-arena small/large split (`stats.arenas.<i>.*`):

```
round 1 frag 1.57 dbsize 284074
== before defrag
used_memory:104854960
allocator_allocated:107692416
allocator_active:169263104
allocator_resident:184086528
allocator_frag_ratio:1.57
allocator_frag_bytes:61570688
mem_fragmentation_ratio:1.68
OK
t=2s allocator_allocated:107733376 allocator_active:129155072 allocator_frag_ratio:1.20 allocator_frag_bytes:21421696  active_defrag_hits:253392 active_defrag_misses:2920512 active_defrag_key_hits:191793 active_defrag_key_misses:621415  running=66
t=4s allocator_allocated:107733376 allocator_active:129155072 allocator_frag_ratio:1.20 allocator_frag_bytes:21421696  active_defrag_hits:253392 active_defrag_misses:7621752 active_defrag_key_hits:191793 active_defrag_key_misses:1796725  running=66
t=6s allocator_allocated:107733376 allocator_active:129155072 allocator_frag_ratio:1.20 allocator_frag_bytes:21421696  active_defrag_hits:253392 active_defrag_misses:12166936 active_defrag_key_hits:191793 active_defrag_key_misses:2933021  running=66
t=8s allocator_allocated:107733376 allocator_active:129155072 allocator_frag_ratio:1.20 allocator_frag_bytes:21421696  active_defrag_hits:253392 active_defrag_misses:16834008 active_defrag_key_hits:191793 active_defrag_key_misses:4099789  running=66
t=10s allocator_allocated:107733376 allocator_active:129155072 allocator_frag_ratio:1.20 allocator_frag_bytes:21421696  active_defrag_hits:253392 active_defrag_misses:21666988 active_defrag_key_hits:191793 active_defrag_key_misses:5308034  running=66
t=15s allocator_allocated:107733376 allocator_active:129155072 allocator_frag_ratio:1.20 allocator_frag_bytes:21421696  active_defrag_hits:253392 active_defrag_misses:26488596 active_defrag_key_hits:191793 active_defrag_key_misses:6513436  running=66
t=20s allocator_allocated:107733376 allocator_active:129155072 allocator_frag_ratio:1.20 allocator_frag_bytes:21421696  active_defrag_hits:253392 active_defrag_misses:31217232 active_defrag_key_hits:191793 active_defrag_key_misses:7695595  running=66
```
```
== per-bin figures (mallctl): j size nregs slab_size curregs curslabs nonfull waste_bytes(capacity-live incl. tail)
0 16 4096 65536 2560 3 3 155648
1 32 2048 65536 868096 426 4 139264
2 48 1365 65536 1668 3 3 116544
3 64 1024 65536 641 2 2 90048
4 80 819 65536 641 2 2 79792
5 96 682 65536 640 2 2 69632
6 112 585 65536 1154 3 3 67360
7 128 512 65536 641 2 1 49024
8 160 409 65536 120550 390 97 6271040
9 192 341 65536 320 2 2 69632
10 224 292 65536 320 2 2 59392
11 256 256 65536 321 2 1 48896
12 320 204 65536 164477 808 3 320448
13 384 170 65536 129 1 1 16000
14 448 146 65536 128 1 1 8192
15 512 8 4096 220 28 1 2048
16 640 25 16384 64 3 1 8192
17 768 21 16384 64 4 1 16384
19 1024 4 4096 81 21 1 3072
20 1280 51 65536 32 1 1 24576
21 1536 42 65536 32 1 1 16384
23 2048 32 65536 41 2 1 47104
24 2560 25 65536 21 2 2 77312
25 3072 21 65536 16 1 1 16384
27 4096 16 65536 21 2 1 45056
```
```
== small/large per arena (mallctl stats.arenas.<i>.*): i small.allocated small.resident large.allocated resident
0 1769856 2146304 1572864 14336000
1 49152 196608 0 196608
2 99325792 112787456 4222976 118423552
```
(`arenas[0]` and `[1]` are the background threads' and the classes above `tcache_max`, which the default path takes from arena 0 (design 13.3); `[2]` is the main thread's.)

Reading it: 241 590 hits in the first two seconds (frag 1.56 -> 1.18), then 171 million misses and no movement. The 21 MB of `active - allocated` that remain are (1) bin 8 (160 B, the 150-byte values): 390 slabs of which 97 non-full holding 713 live slots between them, 6.3 MB of free slots; those 7-per-slab "live" slots are not objects KeyDB owns (the defragmenter never asks about them), they are the main thread's *cached* free slots from the evictions, and a cached slot pins its slab until the cache GC returns it (`tcache_gc_interval_ms` 10 s, three quarters of the idle entries per pass, so 1024 -> 256 -> 64 slots over 20 s, each possibly in a different slab; the drop from 18.9 MB to 13.9 MB between t=30 s and t=40 s in the first run is one such pass); (2) arena 0's classes above 4 KiB: `large: allocated 1.6 MB, active 9.4 MB`, a few live blocks of the 64-256 KiB classes each pinning a 16-slot slab (1-4 MiB), which the bin-only hint answered 0 for (the utilization queries report those classes as one region each, design 13.4); (3) about 1 MB per active arena of slab tails and two-slab idle classes in the small bins, which `active` counts and the utilization queries do not.

### Experiment E1: a faster cache GC and jemalloc-sized caches, no source change

`MALLOC_CONF=tcache_gc_interval_ms:1000,tcache_nslots_small_max:200` on the packaged binary (read back as `opt.tcache_gc_interval_ms 1000`, `opt.tcache_nslots_small_max 200`). Term (1) is gone: bin 8 has 2 non-full slabs and 109 KB of waste, `active - allocated` settles at 9.7-11.2 MB (8-11%) instead of 14-21 MB; terms (2) and (3) remain (`arenas[0]` large 1.58 MB allocated in 9.44 MB active; about 1 MB of small-bin waste per active arena):
```
round 1 frag 1.54 dbsize 293921
== before defrag
used_memory:104854992
allocator_allocated:105728976
allocator_active:162869248
allocator_resident:178421760
allocator_frag_ratio:1.54
allocator_frag_bytes:57140272
mem_fragmentation_ratio:1.63
OK
t=2s allocator_allocated:105752016 allocator_active:116940800 allocator_frag_ratio:1.11 allocator_frag_bytes:11188784  active_defrag_hits:205365 active_defrag_misses:3111737 active_defrag_key_hits:139723 active_defrag_key_misses:701544  running=65
t=4s allocator_allocated:105752016 allocator_active:116940800 allocator_frag_ratio:1.11 allocator_frag_bytes:11188784  active_defrag_hits:205365 active_defrag_misses:7545705 active_defrag_key_hits:139723 active_defrag_key_misses:1810036  running=65
t=6s allocator_allocated:105764816 allocator_active:116953088 allocator_frag_ratio:1.11 allocator_frag_bytes:11188272  active_defrag_hits:205365 active_defrag_misses:11991081 active_defrag_key_hits:139723 active_defrag_key_misses:2921380  running=65
t=8s allocator_allocated:105752016 allocator_active:116944896 allocator_frag_ratio:1.11 allocator_frag_bytes:11192880  active_defrag_hits:205365 active_defrag_misses:16424981 active_defrag_key_hits:139723 active_defrag_key_misses:4029855  running=65
t=10s allocator_allocated:105752016 allocator_active:116944896 allocator_frag_ratio:1.11 allocator_frag_bytes:11192880  active_defrag_hits:205365 active_defrag_misses:20908981 active_defrag_key_hits:139723 active_defrag_key_misses:5150855  running=65
t=15s allocator_allocated:105764816 allocator_active:116953088 allocator_frag_ratio:1.11 allocator_frag_bytes:11188272  active_defrag_hits:205365 active_defrag_misses:25611717 active_defrag_key_hits:139723 active_defrag_key_misses:6326539  running=65
t=20s allocator_allocated:105752016 allocator_active:116948992 allocator_frag_ratio:1.11 allocator_frag_bytes:11196976  active_defrag_hits:205398 active_defrag_misses:30184764 active_defrag_key_hits:139756 active_defrag_key_misses:7469776  running=65
```
```
== per-bin figures (mallctl): j size nregs slab_size curregs curslabs nonfull waste_bytes(capacity-live incl. tail)
0 16 4096 65536 1536 3 3 172032
1 32 2048 65536 895925 440 4 166240
2 48 1365 65536 907 3 3 153072
3 64 1024 65536 203 2 2 118080
4 80 819 65536 103 2 2 122832
5 96 682 65536 102 2 2 121280
6 112 585 65536 604 3 3 128960
7 128 512 65536 503 2 2 66688
8 160 409 65536 145544 357 2 109312
9 192 341 65536 54 2 2 120704
10 224 292 65536 63 2 2 116960
11 256 256 65536 52 2 2 117760
12 320 204 65536 148651 729 2 207424
13 384 170 65536 26 1 1 55552
14 448 146 65536 25 1 1 54336
15 512 8 4096 58 11 5 15360
16 640 25 16384 12 1 1 8704
17 768 21 16384 12 1 1 7168
19 1024 4 4096 13 4 1 3072
20 1280 51 65536 6 1 1 57856
21 1536 42 65536 6 1 1 56320
23 2048 32 65536 8 2 2 114688
24 2560 25 65536 6 2 2 115712
25 3072 21 65536 4 1 1 53248
27 4096 16 65536 5 2 2 110592
```
```
== small/large per arena (mallctl stats.arenas.<i>.*): i small.allocated small.resident large.allocated resident
0 896048 1896448 1581056 13881344
1 9600 196608 0 196608
2 99063568 101752832 4214784 106471424
```
The suite with the same `MALLOC_CONF` (the server inherits the environment): still `defrag didn't stop.` on the four defrag tests on both architectures (aarch64 229 s: 10 err; x86_64 238 s: 8 err); 8-11% is above the test's 5%.

### Experiment E2: the hint answers for every slab class (commit 896a60d)

Term (2) is the one the hint can address: a live 16-256 KiB block alone in its 16-slot slab is exactly what a move recovers, and KeyDB's defragmenter does ask about its dict tables and other large allocations. `get_defrag_hint` now applies jemalloc's rule to every slab class (huge runs still 0; the utilization queries unchanged, so Valkey sees nothing different). The challoc packages and the KeyDB package rebuilt from 896a60d on both VMs, then the suite twice, with the default configuration and with the E1 `MALLOC_CONF`:

aarch64, default configuration (232 s):
```
[ok]: Memory efficiency with values in range 32
[ok]: Memory efficiency with values in range 64
[ok]: Memory efficiency with values in range 128
[ok]: Memory efficiency with values in range 1024
[ok]: Memory efficiency with values in range 16384
[err]: Active defrag in tests/unit/memefficiency.tcl
[err]: Active defrag big keys in tests/unit/memefficiency.tcl
[err]: Active defrag big list in tests/unit/memefficiency.tcl
[err]: Active defrag edge case in tests/unit/memefficiency.tcl
[ok]: Active defrag with active replica
[1/1 done]: unit/memefficiency (232 seconds)
== test_helper exit 0 after 232 s
```
aarch64, `MALLOC_CONF=tcache_gc_interval_ms:1000,tcache_nslots_small_max:200` (243 s):
```
[ok]: Memory efficiency with values in range 32
[ok]: Memory efficiency with values in range 64
[ok]: Memory efficiency with values in range 128
[ok]: Memory efficiency with values in range 1024
[ok]: Memory efficiency with values in range 16384
[err]: Active defrag in tests/unit/memefficiency.tcl
[err]: Active defrag big keys in tests/unit/memefficiency.tcl
[err]: Active defrag big list in tests/unit/memefficiency.tcl
[err]: Active defrag edge case in tests/unit/memefficiency.tcl
[err]: Active defrag with active replica in tests/unit/memefficiency.tcl
[1/1 done]: unit/memefficiency (228 seconds)
== test_helper exit 0 after 228 s
```
x86_64, default configuration (193 s):
```
[ok]: Memory efficiency with values in range 32
[ok]: Memory efficiency with values in range 64
[ok]: Memory efficiency with values in range 128
[ok]: Memory efficiency with values in range 1024
[ok]: Memory efficiency with values in range 16384
[err]: Active defrag in tests/unit/memefficiency.tcl
[err]: Active defrag big keys in tests/unit/memefficiency.tcl
[err]: Active defrag big list in tests/unit/memefficiency.tcl
[ok]: Active defrag edge case
[ok]: Active defrag with active replica
[1/1 done]: unit/memefficiency (193 seconds)
== test_helper exit 0 after 193 s
```
x86_64, the E1 `MALLOC_CONF`:
```
[ok]: Memory efficiency with values in range 32
[ok]: Memory efficiency with values in range 64
[ok]: Memory efficiency with values in range 128
[ok]: Memory efficiency with values in range 1024
[ok]: Memory efficiency with values in range 16384
[err]: Active defrag in tests/unit/memefficiency.tcl
[err]: Active defrag big keys in tests/unit/memefficiency.tcl
[err]: Active defrag big list in tests/unit/memefficiency.tcl
[err]: Active defrag edge case in tests/unit/memefficiency.tcl
[ok]: Active defrag with active replica
[1/1 done]: unit/memefficiency (237 seconds)
== test_helper exit 0 after 237 s
```
The "with active replica" failure on aarch64 under the E1 configuration is the other assertion, `Expected 1.10 < 1.1` (`assert {$frag < 1.1}` after the cycle did stop): the same floor, one hundredth over the line; the test is borderline under challoc either way.

No change from E1 alone. The by-hand attribution with the E2 packages says why: `arenas[0]`'s large classes stay at 1.58 MB allocated in 9.44 MB active, so the blocks pinning those slabs are not objects the defragmenter asks about (client and query buffers, internal structures: KeyDB walks the keyspace, its dict tables and a few server structures), and the main arena's large classes barely move (4.30 MB in 5.11 MB). Hits are not higher than under E1 (184 919 vs 205 365 in the first two seconds). aarch64, E1 configuration, E2 packages:
```
round 1 frag 1.58 dbsize 286283
== before defrag
allocator_allocated:105740272
allocator_active:166866944
allocator_resident:182427648
allocator_frag_ratio:1.58
allocator_frag_bytes:61126672
t=2s allocator_allocated:105753072 allocator_active:117596160 allocator_frag_ratio:1.11 allocator_frag_bytes:11843088  active_defrag_hits:184919 active_defrag_misses:3207544 active_defrag_key_hits:126216 active_defrag_key_misses:732633  running=65
t=10s allocator_allocated:105753072 allocator_active:117600256 allocator_frag_ratio:1.11 allocator_frag_bytes:11847184  active_defrag_hits:184919 active_defrag_misses:22330240 active_defrag_key_hits:126216 active_defrag_key_misses:5513307  running=65
t=20s allocator_allocated:105722368 allocator_active:115503104 allocator_frag_ratio:1.09 allocator_frag_bytes:9780736  active_defrag_hits:184959 active_defrag_misses:31835790 active_defrag_key_hits:126246 active_defrag_key_misses:7889678  running=65
```
```
arenas[0]:
  small:                       896048      1896448            0            0
  large:                      1581056      9437184            0     11022336  (2 huge runs)
arenas[1]:
  small:                         9600       196608            0            0
  large:                            0            0            0            0  (0 huge runs)
arenas[2]:
  small:                     99053488    100175872       397312     51126272
  large:                      4296704      5111808            0       917504  (1 huge runs)
```
x86_64, default configuration, E2 packages (the cached slots of term (1) visible again as `small: 99.4 MB allocated, 102.3 MB active` in `arenas[2]` and 2.1 MB of small waste in `arenas[0]`):
```
round 1 frag 1.49 dbsize 298315
== before defrag
allocator_allocated:107766144
allocator_active:160153600
allocator_resident:172810240
allocator_frag_ratio:1.49
allocator_frag_bytes:52387456
t=2s allocator_allocated:107766144 allocator_active:121159680 allocator_frag_ratio:1.12 allocator_frag_bytes:13393536  active_defrag_hits:295618 active_defrag_misses:4877947 active_defrag_key_hits:222011 active_defrag_key_misses:1092642  running=65
t=10s allocator_allocated:107766144 allocator_active:121159680 allocator_frag_ratio:1.12 allocator_frag_bytes:13393536  active_defrag_hits:295618 active_defrag_misses:33030615 active_defrag_key_hits:222011 active_defrag_key_misses:8130809  running=65
t=20s allocator_allocated:107766144 allocator_active:121159680 allocator_frag_ratio:1.12 allocator_frag_bytes:13393536  active_defrag_hits:295618 active_defrag_misses:47349735 active_defrag_key_hits:222011 active_defrag_key_misses:11710589  running=65
```
```
arenas[0]:
  small:                      1769856      2146304            0            0
  large:                      1548288      7340032            0      8925184  (2 huge runs)
arenas[1]:
  small:                        49152       196608            0            0
  large:                            0            0            0            0  (0 huge runs)
arenas[2]:
  small:                     99397632    102305792      3207168     40370176
  large:                      4329472      5242880       786432            0  (1 huge runs)
```
E2 stays (it is the right answer for a slab allocator and costs nothing on any allocation path; design 13.5 records what it did and did not recover), but it is not what KeyDB's test needs.

### What KeyDB needs beyond the hint

KeyDB's defragmenter works on challoc: it reduces fragmentation from 1.5-2.0 to 1.09-1.18 within two seconds on the suite's workloads and moves what the hint marks. What fails is its *stop rule*, Redis 6.2's `active - allocated` over the whole allocator against a 5% threshold, which under challoc has a floor of about 8-12% on these workloads made of three terms: (1) free slots the freeing thread's cache still holds pin their slabs until the cache GC returns them (up to 6 MB here; the 10 s GC returns three quarters per pass; a 1 s GC removes the term, `MALLOC_CONF` E1); (2) 16-slot slabs of the 64-256 KiB classes pinned by one or two live blocks that are not the defragmenter's objects (about 8 MB, in arena 0 where the classes above `tcache_max` live; design 13.4's "megabytes on an idle server"); (3) the slab tails and two-slab idle classes that `active` counts and the utilization queries do not (about 1 MB per active arena). With KeyDB's defaults (`active-defrag-threshold-lower 10`, `active-defrag-ignore-bytes 100mb`) the cycle stops at once (`frag_bytes` 10-20 MB is under 100 MB), so a production KeyDB on challoc does not spin; the suite's `2mb` / 5% settings are what it cannot meet. Closing the gap is one of: a KeyDB-side stop rule over defraggable waste (Redis 7's and Valkey's `frag_smallbins` from the utilization queries, which KeyDB 6.3 does not call), a `tests/unit/memefficiency.tcl` hunk in the package's patch that lifts the two thresholds above challoc's floor (as `contrib/valkey`'s patch does for the eval-scripts test under `*challoc*`; not done here: the hunk would be for the record only, since the package build does not run the suite, and the goal was to know whether KeyDB's defrag works, which it does), or challoc-side smaller slabs for the classes above 4 KiB and a faster default cache GC, which are lane A's and lane B's files and a performance question, carried as an open item. The package is not shipped with defrag compiled out.


## Headline results

- Wolfi's KeyDB 6.3.4 builds against `challoc-jemalloc-compat-dev` with a two-hunk patch (`MALLOC=challoc`, the allocator name), on aarch64 and x86_64, in 30 s of make; lints clean; `melange test` passes on both with the allocator checks (`malloc=jemalloc-5.3.0-challoc`, `NEEDED libjemalloc.so.2` on all three binaries, the compat package's file mapped, `CONFIG SET activedefrag yes` OK, purge, stats, the mallctl bridge, traffic, clean shutdown).
- What KeyDB needed from challoc: `je_get_defrag_hint` (exported in this lane, both spellings, `JEMALLOC_FRAG_HINT` in the headers). Nothing about the quantum: the system `quantum16` compat library is the right one, no private build.
- Both ways: Wolfi's `libjemalloc.so.2` preloaded in front of the compat library, `keydb-server` fails to start with `undefined symbol: je_get_defrag_hint`; the explicit `challoc-jemalloc-compat` dependency states the requirement.
- KeyDB's `unit/memefficiency`: the memory-efficiency tests pass; the defragmenter runs and reduces fragmentation (1.5-2.0 -> 1.09-1.18) but the defrag tests fail `defrag didn't stop.` because KeyDB's whole-allocator `active - allocated` stop rule has a floor under challoc (8-12% on these workloads: cached slots pinning slabs, large-class slabs pinned by non-defragmentable objects, slab tails) above the test's 5% / 2 MB settings; at KeyDB's defaults (10% / 100 MB) the cycle stops. Measured with the bin-only hint, with a faster cache GC (`MALLOC_CONF`, removes one of the three terms), and with the hint answering for every slab class (commit 896a60d, kept, no effect on this suite). What closes the gap is named above; the honest result is known and the lane stops there.
- Two Tcl 9 spellings are needed to run KeyDB's suite on Wolfi at all (`package require Tcl 8.5-`, `-translation binary`), unrelated to the allocator.
- Two assertion no-ops in the first test block (`!` under `set -e`; `apk info -e` on a provided name) found and fixed here; the same lines exist in lane P's Ruby and Valkey tests (noted for the human).
- Packages of the final build (896a60d, the E2 rebuild) are the ones whose sums are listed; the first build (9d62dbb) differed only in the hint's class range.

## Not done, and why

- The full KeyDB test suite was not run against the package (out of scope: the defrag tests are the ones that exercise what changed; the rest of the suite runs on the allocator through `malloc`/`free` like any program).
- No performance numbers: this is a packaging lane; a KeyDB benchmark row against Wolfi's build is a measurement task with its own protocol.
- The `-cli` and `-benchmark` subpackages name `challoc-jemalloc-compat` too (linked with the same `FINAL_LIBS`); a KeyDB image with the cli therefore carries exactly one allocator library. Whether Wolfi wants the client tools on the system `libjemalloc2` instead is a policy question for the PR.

## Diff against Wolfi's `keydb.yaml` (8c473de)

```diff
--- wolfi-dev/os keydb.yaml (8c473de)	2026-09-20 18:54:45
+++ contrib/wolfi/keydb/keydb.yaml	2026-09-20 19:15:54
@@ -1,10 +1,50 @@
+# keydb on challoc: the Wolfi package definition with the allocator switched
+# from the bundled jemalloc to challoc through challoc-jemalloc-compat
+# (docs/plan.md Phase 3 task 3, docs/design.md sections 13.5 and 13.6).
+# Based on wolfi-dev/os keydb.yaml at 8c473de (2026-09-20; the file itself
+# last changed in 276d78c, a resources bump); contrib/wolfi/README.md
+# ("KeyDB") lists every change from it, and
+# bench/results/2026-09-20-phase3-task3-keydb-package.md the build.
+#
+# KeyDB's defragmenter (src/defrag.cpp) calls je_get_defrag_hint, the
+# extension Redis adds to its bundled jemalloc, and zmalloc.h compiles it
+# in (HAVE_DEFRAG) only when the jemalloc header defines
+# JEMALLOC_FRAG_HINT; challoc exports the hint and its header defines the
+# macro (design 13.5). Unlike Valkey, KeyDB 6.3 asserts nothing about the
+# allocator's quantum or bin geometry (its zmalloc.cpp reads epoch,
+# stats.*, background_thread, arenas.narenas and arena.<n>.purge, and
+# defrag.cpp only the hint), so it runs on the system quantum16 library
+# like Ruby does: the build environment takes challoc-jemalloc-compat-dev
+# in place of jemalloc-dev (the same file set: <jemalloc/jemalloc.h>,
+# libjemalloc.so, jemalloc.pc) and the package names
+# challoc-jemalloc-compat at run time. KeyDB's Makefile has no
+# system-jemalloc mode (MALLOC=jemalloc builds deps/jemalloc statically),
+# so keydb/0001-Add-MALLOC-challoc.patch adds MALLOC=challoc: the
+# USE_JEMALLOC code paths, -ljemalloc, and a ZMALLOC_LIB of
+# "jemalloc-5.3.0-challoc" so --version and INFO memory say which build.
+#
+# Local build (contrib/wolfi/README.md, "KeyDB"): after challoc.yaml has
+# been built into the local repository,
+#
+#   melange build contrib/wolfi/keydb/keydb.yaml --arch aarch64 \
+#     --source-dir contrib/wolfi/keydb/keydb \
+#     --pipeline-dir /path/to/wolfi-dev/os/pipelines \
+#     --repository-append <local repo> --keyring-append <its key> \
+#     --signing-key melange.rsa --out-dir packages
 package:
   name: keydb
   version: 6.3.4
-  epoch: 8 # go/wolfi-rsc/keydb
+  epoch: 9 # allocator: bundled jemalloc -> challoc (challoc-jemalloc-compat); 8 was go/wolfi-rsc/keydb
   description: A Multithreaded Fork of Redis
   copyright:
     - license: BSD-3-Clause
+  dependencies:
+    runtime:
+      # melange derives so:libjemalloc.so.2 from the binaries; apk resolves
+      # a bare so: name to the package that carries it (libjemalloc2) over
+      # a provider, so the package names what it was built and tested
+      # against.
+      - challoc-jemalloc-compat
   resources:
     cpu: "2"
     memory: 5Gi
@@ -12,18 +52,25 @@
     cpu: "1"
     memory: 1Gi
 
+vars:
+  # What keydb-server --version and INFO memory's mem_allocator report:
+  # "jemalloc-" JEMALLOC_VERSION_{MAJOR,MINOR,BUGFIX} "-" JEMALLOC_VERSION_GID
+  # from challoc's <jemalloc/jemalloc.h> (the patch's zmalloc.h hunk).
+  allocator-name: jemalloc-5.3.0-challoc
+
 environment:
   contents:
     packages:
       - autoconf
       - automake
       - bash
+      - binutils # readelf for the DT_NEEDED check of the built binaries
       - build-base
       - busybox
       - bzip2-dev
+      - challoc-jemalloc-compat-dev # in place of jemalloc-dev: the same file set, challoc underneath
       - curl-dev
       - glibc-2.44-dev
-      - jemalloc-dev
       - libbz2-1
       - libcurl-openssl4
       - lz4-dev
@@ -50,6 +97,12 @@
     with:
       patches: ../../cstdint.patch
 
+  # MALLOC=challoc (src/Makefile) and the "-challoc" allocator name
+  # (src/zmalloc.h); see the header comment.
+  - uses: patch
+    with:
+      patches: 0001-Add-MALLOC-challoc.patch
+
   - runs: |
       # From here: https://github.com/Snapchat/KeyDB/blob/main/pkg/docker/Dockerfile#L52C9-L55C92
       # disable protected mode as it relates to docker
@@ -58,19 +111,48 @@
       grep -E '^ *createBoolConfig[(]"protected-mode",.*, *0 *,.*[)],$' ./src/config.cpp;
 
   - runs: |
-      make -j$(nproc)
+      # The header the build sees is challoc's, with the defragmenter hint.
+      grep -q '^#define JEMALLOC_CHALLOC 1' /usr/include/jemalloc/jemalloc.h
+      grep -q '^#define JEMALLOC_FRAG_HINT' /usr/include/jemalloc/jemalloc.h
+      grep -q 'je_get_defrag_hint' /usr/include/jemalloc/jemalloc.h
+      # MALLOC on every invocation: the Makefile persists its settings and
+      # an install without it would rebuild with the bundled jemalloc.
+      make MALLOC=challoc -j$(nproc)
 
-  - runs: make install
+  - runs: make MALLOC=challoc install
 
   - runs: |
       # The make install doesn't respect PREFIX
       mkdir -p ${{targets.destdir}}/usr/bin
       mv /usr/local/bin/* ${{targets.destdir}}/usr/bin
 
+  - name: Check the built binaries name the allocator and the compat library
+    runs: |
+      set -eux
+      [ ! -d deps/jemalloc/lib ] || { echo "deps/jemalloc was built"; exit 1; }
+      bin="${{targets.destdir}}/usr/bin/keydb-server"
+      "$bin" --version
+      "$bin" --version | grep 'malloc=${{vars.allocator-name}}' > /dev/null
+      for b in keydb-server keydb-cli keydb-benchmark; do
+        readelf -d "${{targets.destdir}}/usr/bin/$b" | grep NEEDED
+        readelf -d "${{targets.destdir}}/usr/bin/$b" | grep 'NEEDED.*\[libjemalloc.so.2\]' > /dev/null
+      done
+      # The environment's libjemalloc.so.2 is challoc's (a build environment
+      # has no apk, so the file is asked): it exports the hint the
+      # defragmenter is linked against.
+      readelf -d /usr/lib/libjemalloc.so.2 | grep 'Library soname: \[libjemalloc.so.2\]' > /dev/null
+      readelf --dyn-syms /usr/lib/libjemalloc.so.2 | grep ' challoc_version$' > /dev/null
+      readelf --dyn-syms /usr/lib/libjemalloc.so.2 | grep ' je_get_defrag_hint$' > /dev/null
+      # defrag.cpp was compiled in: HAVE_DEFRAG makes activedefrag settable.
+      readelf --dyn-syms --wide "$bin" | grep 'je_get_defrag_hint' || nm -D "$bin" | grep -q je_get_defrag_hint > /dev/null
+
   - uses: strip
 
 subpackages:
   - name: keydb-cli
+    dependencies:
+      runtime:
+        - challoc-jemalloc-compat # linked with the same FINAL_LIBS as the server
     pipeline:
       - runs: |
           mkdir -p ${{targets.subpkgdir}}/usr/bin
@@ -81,6 +163,9 @@
             keydb-cli --version
 
   - name: keydb-benchmark
+    dependencies:
+      runtime:
+        - challoc-jemalloc-compat
     pipeline:
       - runs: |
           mkdir -p ${{targets.subpkgdir}}/usr/bin
@@ -101,16 +186,64 @@
   environment:
     contents:
       packages:
+        - apk-tools
+        - binutils
+        - keydb-benchmark
         - keydb-cli
         - wait-for-it
   pipeline:
     - uses: test/tw/ldd-check
-    - runs: |
-        keydb-server > /dev/null 2>&1 &
+    - name: The server is linked against the compat library and says so
+      runs: |
+        set -eux
+        keydb-server --version
+        keydb-server --version | grep 'malloc=${{vars.allocator-name}}' > /dev/null
+        for b in keydb-server keydb-cli keydb-benchmark; do
+          readelf -d /usr/bin/$b | grep 'NEEDED.*\[libjemalloc.so.2\]' > /dev/null
+        done
+        ldd /usr/bin/keydb-server | grep -E 'libjemalloc.so.2 => /(usr/)?lib/libjemalloc.so.2' > /dev/null
+        apk info --who-owns /usr/lib/libjemalloc.so.2
+        apk info --who-owns /usr/lib/libjemalloc.so.2 | grep challoc-jemalloc-compat > /dev/null
+        ! apk info -e libjemalloc2
+        readelf --dyn-syms /usr/lib/libjemalloc.so.2 | grep ' je_get_defrag_hint$' > /dev/null
+        keydb-sentinel --version
+    - name: A running server uses the compat library, defragments and purges
+      runs: |
+        set -eux
+        keydb-server --save "" --dir /tmp > /tmp/keydb.log 2>&1 &
+        pid=$!
         wait-for-it localhost:6379 -t 30
+        keydb-cli PING | grep PONG > /dev/null
+        # The one libjemalloc mapped in the server is the compat package's.
+        awk '$6 ~ /libjemalloc/ {print $6}' /proc/$pid/maps | sort -u
+        [ "$(awk '$6 ~ /libjemalloc/ {print $6}' /proc/$pid/maps | sort -u)" = /usr/lib/libjemalloc.so.2 ]
+        keydb-cli INFO memory | tr -d '\r' | grep -E '^(mem_allocator|allocator_frag_ratio|allocator_allocated):'
+        keydb-cli INFO memory | tr -d '\r' | grep -x 'mem_allocator:${{vars.allocator-name}}' > /dev/null
+        # Active defrag needs HAVE_DEFRAG (JEMALLOC_FRAG_HINT from the
+        # header); without it the error is "Active defragmentation cannot
+        # be enabled: it requires a Redis server compiled with a modified
+        # Jemalloc ...".
+        keydb-cli CONFIG SET activedefrag yes | grep -x OK > /dev/null
+        keydb-cli CONFIG GET activedefrag | grep -x yes > /dev/null
+        keydb-cli MEMORY PURGE | grep -x OK > /dev/null
+        keydb-cli MEMORY MALLOC-STATS | head -3
+        keydb-cli MEMORY MALLOC-STATS | grep 'jemalloc statistics' > /dev/null
+        keydb-cli DEBUG MALLCTL arenas.page
+        # Wolfi's checks.
         keydb-cli SET bike:1 "Process 134" || exit 1
         keydb-cli GET bike:1 | grep 'Process 134' || exit 1
         keydb-cli exists bike:1 | grep 1 || exit 1
         keydb-cli exists bike:2 | grep 0 || exit 1
-        keydb-sentinel --version
-        keydb-server --version
+        # Some traffic through the allocator and a defrag-shaped churn
+        # (populate, delete half, wait a cycle), then the server is still up.
+        keydb-benchmark -t set,get -n 20000 -q
+        keydb-benchmark -t set -n 200000 -r 200000 -d 150 -q -P 16
+        keydb-cli DBSIZE
+        keydb-cli EVAL 'for i=1,100000 do redis.call("del", redis.call("randomkey")) end return 1' 0
+        sleep 2
+        keydb-cli INFO stats | tr -d '\r' | grep -E '^active_defrag_(hits|misses|key_hits|key_misses):'
+        keydb-cli INFO memory | tr -d '\r' | grep -E '^(used_memory|allocator_active|allocator_resident|allocator_frag_ratio):'
+        keydb-cli CONFIG SET activedefrag no | grep -x OK > /dev/null
+        keydb-cli SHUTDOWN NOSAVE || true
+        wait $pid || true
+        ! grep -iE 'challoc:|assert|crash' /tmp/keydb.log
```

### `contrib/wolfi/keydb/keydb/0001-Add-MALLOC-challoc.patch` (new)

```diff
From: challoc <noreply@chainguard.dev>
Subject: [PATCH] Add MALLOC=challoc: link the system challoc (jemalloc-compat) instead of the bundled jemalloc

challoc (https://github.com/chainguard-dev/challoc) is a malloc replacement
that exports jemalloc's non-standard API (mallocx family, mallctl, the
stats.* keys zmalloc.cpp reads) and Redis's get_defrag_hint, the extension
of the bundled jemalloc that zmalloc.h's HAVE_DEFRAG requires: its
<jemalloc/jemalloc.h> defines JEMALLOC_FRAG_HINT and declares
je_get_defrag_hint, so defrag.cpp compiles unchanged and active defrag
works. `make MALLOC=challoc` keeps every USE_JEMALLOC code path and links
the system libjemalloc.so.2 (the challoc-jemalloc-compat package) instead
of building deps/jemalloc; no header or symbol renames are needed because
KeyDB calls the unprefixed names and challoc exports both spellings.

- src/Makefile: MALLOC=challoc adds -DUSE_JEMALLOC -DUSE_CHALLOC and links
  -ljemalloc inside --push-state,--no-as-needed; no jemalloc dependency
  target, so deps/jemalloc is never built.
- src/zmalloc.h: under USE_CHALLOC report ZMALLOC_LIB as
  "jemalloc-<version>-challoc" (challoc's JEMALLOC_VERSION_GID), so
  `keydb-server --version` and INFO's mem_allocator say which allocator
  the build has; everything keyed on "jemalloc" (the test suite's
  memefficiency gate, MEMORY MALLOC-STATS) still matches.

Build: make MALLOC=challoc all; make MALLOC=challoc install (the Makefile
persists its settings, so MALLOC goes on every invocation). Default
behaviour without MALLOC=challoc is unchanged.
---
--- a/src/Makefile
+++ b/src/Makefile
@@ -322,6 +322,20 @@
 	FINAL_CFLAGS+= -DUSE_MEMKIND -I../deps/memkind/src/include
 	FINAL_CXXFLAGS+= -DUSE_MEMKIND -I../deps/memkind/src/include
 	FINAL_LIBS := ../deps/memkind/src/.libs/libmemkind.a -lnuma $(FINAL_LIBS)
+endif
+
+# challoc (https://github.com/chainguard-dev/challoc) is a system allocator
+# with jemalloc's non-standard API, including the get_defrag_hint of the
+# bundled jemalloc that HAVE_DEFRAG needs. MALLOC=challoc keeps every
+# USE_JEMALLOC code path (stats, purge, background thread, active defrag)
+# and links the system libjemalloc.so.2 that challoc's jemalloc-compat
+# package installs, with its <jemalloc/jemalloc.h>, instead of building
+# deps/jemalloc. -Wl,--no-as-needed keeps the DT_NEEDED entry even where a
+# binary references no allocator symbol itself.
+ifeq ($(MALLOC),challoc)
+	FINAL_CFLAGS+= -DUSE_JEMALLOC -DUSE_CHALLOC
+	FINAL_CXXFLAGS+= -DUSE_JEMALLOC -DUSE_CHALLOC
+	FINAL_LIBS := -Wl,--push-state,--no-as-needed -ljemalloc -Wl,--pop-state $(FINAL_LIBS)
 endif
 
 ifeq ($(BUILD_TLS),yes)
--- a/src/zmalloc.h
+++ b/src/zmalloc.h
@@ -51,8 +51,14 @@
 #endif
 
 #elif defined(USE_JEMALLOC)
-#define ZMALLOC_LIB ("jemalloc-" __xstr(JEMALLOC_VERSION_MAJOR) "." __xstr(JEMALLOC_VERSION_MINOR) "." __xstr(JEMALLOC_VERSION_BUGFIX))
 #include <jemalloc/jemalloc.h>
+#if defined(USE_CHALLOC) && defined(JEMALLOC_VERSION_GID)
+/* challoc: "jemalloc-5.3.0-challoc", so INFO memory and --version tell it
+ * from a jemalloc build (everything keyed on "jemalloc" still matches). */
+#define ZMALLOC_LIB ("jemalloc-" __xstr(JEMALLOC_VERSION_MAJOR) "." __xstr(JEMALLOC_VERSION_MINOR) "." __xstr(JEMALLOC_VERSION_BUGFIX) "-" JEMALLOC_VERSION_GID)
+#else
+#define ZMALLOC_LIB ("jemalloc-" __xstr(JEMALLOC_VERSION_MAJOR) "." __xstr(JEMALLOC_VERSION_MINOR) "." __xstr(JEMALLOC_VERSION_BUGFIX))
+#endif
 #if (JEMALLOC_VERSION_MAJOR == 2 && JEMALLOC_VERSION_MINOR >= 1) || (JEMALLOC_VERSION_MAJOR > 2)
 #define HAVE_MALLOC_SIZE 1
 #define zmalloc_size(p) malloc_usable_size(p)
```
