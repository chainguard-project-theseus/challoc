# Phase 3 task 3 (Ruby): Wolfi's Ruby 3.4 on challoc through `challoc-jemalloc-compat`, first real build

Date 2026-09-20. Lane P (packaging; nothing under `src/`, `Cargo.toml` or `build.rs` changed), worktree `../challoc-wt/lane-p`, branch `loop/lane-p` from `main` 47b96d4. Definitions: `contrib/wolfi/challoc.yaml` (the new `challoc-jemalloc-compat-dev` subpackage), `contrib/wolfi/ruby/ruby-3.4.yaml` with `contrib/wolfi/ruby/ruby-3.4/remove-already-provided-gems-from-gems_bundled_gems.patch` (Wolfi's, unchanged), the header `contrib/jemalloc-compat/include/jemalloc/jemalloc.h` (staged by `scripts/jemalloc-compat.sh` under `include/jemalloc/`). Layout and reasoning: `contrib/wolfi/README.md`, "Ruby" and the packages table. No number in this file is a benchmark result; the RSS lines only show the package allocating. KeyDB and RethinkDB were not reached (the end of this file says what each needs).

## Base

- wolfi-dev/os `ruby-3.4.yaml` at `main` `a944f08e036412ab4ed83803c890347799af2f69` (2026-09-19T13:06Z); the file's last change is `1c5cd09e8f739388c15ad9656a2c78e061fe9b9e` ("ruby-3.4: cherry-pick resolv 0.7.2 to remediate CVE-2026-80212 and CVE-2026-80213 (#337303)", 2026-09-18), `version: 3.4.10`, `epoch: 7`. Wolfi's `ruby-3.4/remove-already-provided-gems-from-gems_bundled_gems.patch` copied from the same commit. Wolfi's `pipelines/` (for `test/tw/*`, `test/pkgconf`, `autoconf/*`, `split/*`, `git-checkout`) from the same commit.
- Ruby tag `v3_4_10`, commit `2b0b7728dc7f0561c35c3d8c4489945c94b783ad` (Wolfi's `expected-commit`, the one Phase 1 built), plus Wolfi's two cherry-picks (`b92c8c2` fiber_switch, `6deacfb` resolv 0.7.2).
- challoc: this tree at 47b96d4 plus this task's files (`--build-option local-source` for `challoc.yaml`).
- The diff against Wolfi's `ruby-3.4.yaml` (`diff -u`) is at the end of this file.

## Where and with what

- VMs: `challoc-lane-p-arm64` (c4a-standard-16, Neoverse-V2) and `challoc-lane-p-amd64` (c4-standard-16, Xeon Platinum 8581C), Ubuntu 24.04, kernel `7.0.0-1011-gcp`, Docker 29.1.3, zone us-east4-a; created, provisioned, used and deleted within the iteration.
- melange: Wolfi's `melange` apk (`v0.60.0+dirty`) in a `--privileged` `cgr.dev/chainguard/wolfi-base` container, `--runner bubblewrap`; `challoc.yaml` with `--source-dir /work --build-option local-source`; `ruby-3.4.yaml` with `--source-dir /work/contrib/wolfi/ruby/ruby-3.4` (the patch directory, as Wolfi's Makefile passes `./ruby-3.4`) and the local repository appended (`--repository-append /out/packages`), so the Ruby build environment takes `challoc-jemalloc-compat-dev` from the packages built minutes before. `melange test` with `--test-package-append=wolfi-base` for both.
- Ruby build environment (from the melange log, both architectures): `gcc-14=14.4.0-r1` (Wolfi's file pins `gcc-14-default` for Ruby), `binutils=2.47-r1`, `glibc-2.44-dev=2.44-r6`, `rust-1.98=1.98.1-r0` (YJIT), `libffi-dev=3.8.0-r1` (Fiddle), `openssl-dev=3.6.4-r7`, `autoconf=2.73-r0`, `challoc-jemalloc-compat=0.1.0-r0`, `challoc-jemalloc-compat-dev=0.1.0-r0`, and the bootstrap `ruby-3.4=3.4.10-r7` (Wolfi's current, which itself needs `so:libjemalloc.so.2`: apko resolved it to the compat library, no `libjemalloc2` in the environment, so the bootstrap interpreter ran on challoc during the build). challoc build environment as in tasks 1 and 2 (`rust-1.98=1.98.1-r0`, `gcc=16.2.0-r1`, `binutils=2.47-r1`, `glibc-2.44-dev=2.44-r6`).
- Clean-container checks: `cgr.dev/chainguard/wolfi-base` with the local repository and key added; the image's own repository (`https://apk.cgr.dev/chainguard`) stays, so `apk` sees Wolfi's `ruby-3.4` 3.4.10-r7 and `libjemalloc2`/`jemalloc-dev` next to the local packages, and every `so:` dependency resolves.

## The compat header and the -dev package

`contrib/jemalloc-compat/include/jemalloc/jemalloc.h` (152 lines) declares jemalloc 5.3's unprefixed API as challoc exports it: `malloc_conf`, `malloc_message`, `mallocx`/`rallocx`/`xallocx`/`sallocx`/`dallocx`/`sdallocx`/`nallocx`, `mallctl`/`mallctlnametomib`/`mallctlbymib`, `malloc_stats_print`; the `MALLOCX_*` and `MALLCTL_ARENAS_*` macros bit for bit; `JEMALLOC_VERSION "5.3.0-0-gchalloc"` and the `_MAJOR/_MINOR/_BUGFIX/_NREV/_GID` macros; the `JEMALLOC_ATTR`/`JEMALLOC_EXPORT`/... attribute macros and the `jemalloc_defs.h` flags a consumer may test (`JEMALLOC_OVERRIDE_MEMALIGN`, `JEMALLOC_OVERRIDE_VALLOC`, `JEMALLOC_USABLE_SIZE_CONST` empty as on glibc); the `je_` spellings as prototypes (real symbols of the library, not rename macros, so it coexists with `<challoc/jemalloc.h>`); and `JEMALLOC_CHALLOC 1` / `CHALLOC 1` to tell. The standard entry points are left to `<stdlib.h>` and `<malloc.h>` (glibc's declarations), so a C++ translation unit sees one exception specification for `malloc` and `malloc_usable_size` takes `void *`.

Checked in the dev container before the package build (aarch64): `gcc -std=c11 -Wall -Wextra -Wundef -pedantic -fsyntax-only`, `-std=gnu99`, `g++ -std=c++17 ... -fsyntax-only` all clean; Ruby's configure probe program (a read of `malloc_conf` through the header) plus `mallctl("version")`, `mallocx(100, MALLOCX_ALIGN(64))`, `je_malloc`/`malloc_usable_size` built and ran in C and C++ against the stage: `version=5.3.0-0-gchalloc sallocx=128 usable=16 5.3.0-0-gchalloc`, `check-needed.sh`: `ok libjemalloc.so.2`.

Package layout change: task 1 had `/usr/lib/libjemalloc.so` and `jemalloc.pc` in `challoc-jemalloc-compat`; they moved to `challoc-jemalloc-compat-dev` with the header, so the runtime package is exactly `libjemalloc2`'s file set and the -dev package exactly `jemalloc-dev`'s (`split/dev` convention). Package metadata melange derived (aarch64; x86_64 identical but for the loader entry):

```
generating package challoc-jemalloc-compat-0.1.0-r0
  runtime:  so:ld-linux-aarch64.so.1, so:libc.so.6
  provides: libjemalloc2=0.1.0-r0, so:libjemalloc.so.2=2
  installed-size: 437978            (x86_64: 420769)
generating package challoc-jemalloc-compat-dev-0.1.0-r0
  runtime:  challoc-jemalloc-compat=0.1.0-r0, so:libjemalloc.so.2
  provides: jemalloc-dev=0.1.0-r0, pc:jemalloc=0.1.0-r0
  installed-size: 52808             (x86_64: 52807)
```
`apk info -L` in the clean container: `challoc-jemalloc-compat` = `usr/lib/libjemalloc.so.2` (+ its SBOM); `challoc-jemalloc-compat-dev` = `usr/include/jemalloc/jemalloc.h`, `usr/lib/libjemalloc.so`, `usr/lib/pkgconfig/jemalloc.pc` (+ SBOM). Header 7576 B, pc file 386 B, library 405,464 B (aarch64).

## Timing (aarch64; x86_64 in parentheses)

```
13:36:00 melange build challoc.yaml            (13:56:22)   4 packages, 24 s
13:36:24 melange test  challoc.yaml            (13:56:46)   4 test environments, 4 s
13:36:28 melange build ruby-3.4.yaml           (13:56:50)
13:36:34   git-checkout (ruby + 2 cherry-picks)
13:37:25   patch
13:37:25   Generate and Configure               (13:57:46)   checking for jemalloc/jemalloc.h... yes
                                                             checking for jemalloc library... -ljemalloc
13:37:49   autoconf/make                        (13:58:09)
13:38:33   Check the build is on challoc's libjemalloc.so.2   (13:58:54)  see below
13:38:33   autoconf/make update-gems extract-gems
13:38:34   autoconf/make rdoc capi              (13:58:56)
13:39:19   autoconf/make-install                (13:59:37)
13:40:03   split/manpages, split/dev; linters: no lint findings for ruby-3.4, ruby-3.4-doc, ruby-3.4-dev
13:40:11 melange test  ruby-3.4.yaml            (14:00:23)   3 environments; exit 0 (14:00:34)
13:50:04 (re-run of the Ruby test after the final YAML edits: exit 0; x86_64 re-run 14:04:19 exit 0)
```
The whole Ruby pipeline is 3 min 43 s on either VM. The build's check step, from the log (aarch64; x86_64 the same with `ld-linux-x86-64.so.2`):
```
#define RUBY_ALTERNATIVE_MALLOC_HEADER <jemalloc/jemalloc.h>
#define HAVE_MALLOC_CONF 1
ruby:              NEEDED libruby.so.3.4 libz.so.1 libjemalloc.so.2 libgmp.so.10 libcrypt.so.1 libm.so.6 libc.so.6 ld-linux-aarch64.so.1
libruby.so.3.4.10: NEEDED libz.so.1 libjemalloc.so.2 libgmp.so.10 libcrypt.so.1 libm.so.6 libgcc_s.so.1 libc.so.6 ld-linux-aarch64.so.1
lrwxrwxrwx 1 root root     16 Sep 20 13:28 /usr/lib/libjemalloc.so -> libjemalloc.so.2
-rwxr-xr-x 1 root root 405464 Sep 20 13:28 /usr/lib/libjemalloc.so.2
readelf --dyn-syms /usr/lib/libjemalloc.so.2 | grep ' challoc_version$'   -> found
```
(The first attempt of this step used `apk info --who-owns`, which does not exist in a build environment: `/bin/sh: apk: not found`. A build environment is asked through the file, a test environment through the database.)

## Built packages

`aarch64` (`/out/packages/aarch64/`):
```
   199605  challoc-0.1.0-r0.apk                     489d5e23c31300dcd851ea3ccbb83a12e92f1b7c0da6aa8d61d0e279ded5ac39
    13426  challoc-dev-0.1.0-r0.apk                 744f284825953869c52959521914c81d1f5fca79d4d24d7a011dcd963c2aa857
   199650  challoc-jemalloc-compat-0.1.0-r0.apk     1c98d0dbc1d2eaf4e6f75b387f587423458feea892739ef5c04c5be4d2384fa9
    14324  challoc-jemalloc-compat-dev-0.1.0-r0.apk acf5aadbbf39c3dc672e51e66103daab514eeabb529ebf5b406889c53355d1a9
 14193587  ruby-3.4-3.4.10-r8.apk                   fab91d48ae0758c551d895afab4597f5515571dc034a59f453d82f89fa657656
   934201  ruby-3.4-dev-3.4.10-r8.apk               89682de571ab11cb6bab7c81b10c269fc140c7bc7eba56ad792397245886ba7d
  3744643  ruby-3.4-doc-3.4.10-r8.apk               bc2cc6ca3dab1af186d47b6dc570b6a12e780be81074690cc7a7492eec05df8d
```
Installed sizes (melange): `ruby-3.4` 41,483,628 B (`apk info -s`: 39 MiB, as Wolfi's r5..r7), `ruby-3.4-dev` 4,181,200 B, `ruby-3.4-doc` 17,854,543 B.

`x86_64` (`/out/packages/x86_64/`):
```
   195156  challoc-0.1.0-r0.apk                     e456b8cc630e7932e929e8aaeddc1f4265357b2ade58d6e06016dfc065fcd047
    13430  challoc-dev-0.1.0-r0.apk                 0306d13665342506e0705aafe71b69597a7bcc0afd78e5325c1a21cc15afaca0
   195195  challoc-jemalloc-compat-0.1.0-r0.apk     b2e852d48a1ca58674598cf5b358657f0a53864b33a86ab689b9dfb734cb809e
    14328  challoc-jemalloc-compat-dev-0.1.0-r0.apk 73d37d2ad59517c105ba205c69df97565d30e1cec4b7e51ca839834f5409ce0b
 13931013  ruby-3.4-3.4.10-r8.apk                   cef1d6a648f0391b3ccce20fbb91579c471131f34ecc1882dc9e990d98a9a14a
   922612  ruby-3.4-dev-3.4.10-r8.apk               e551e1e5f9b7b0b759cd24d9e8c021931027286072d31f21c140eec6ebe96135
  3744655  ruby-3.4-doc-3.4.10-r8.apk               902f74df6f8fdc07fb93e7363b704231b4ae7aa678efb1c615a3cf5303dc417d
```
Installed sizes: `ruby-3.4` 36,890,409 B, `ruby-3.4-dev` 4,180,075 B, `ruby-3.4-doc` 17,854,542 B.

### Package metadata melange derived for `ruby-3.4` (aarch64; x86_64 identical but for the loader)

```
generating package ruby-3.4-3.4.10-r8
  runtime:
    challoc-jemalloc-compat        <- the explicit dependency
    glibc  gmp  libgcc  ruby-3.4
    so:ld-linux-aarch64.so.1  so:libc.so.6  so:libcrypt.so.1  so:libcrypto.so.3  so:libffi.so.8
    so:libgcc_s.so.1  so:libgmp.so.10  so:libjemalloc.so.2  so:libm.so.6  so:libssl.so.3
    so:libyaml-0.so.2  so:libz.so.1
    yaml  zlib
  provides: cmd:erb ... cmd:ruby=3.4.10-r8, ruby3.4-base64, ruby3.4-csv, ruby3.4-drb, ruby=3.4.10-r8, so:libruby.so.3.4=3.4
generating package ruby-3.4-dev-3.4.10-r8
  runtime: challoc-jemalloc-compat-dev, ruby-3.4, so:libruby.so.3.4
  provides: pc:ruby-3.4=3.4.10-r8
```
Why the explicit dependency matters: the derived `so:libjemalloc.so.2` alone is satisfied by Wolfi's `libjemalloc2` (apk prefers the package that carries a name over a provider; task 1 measured it), so without `challoc-jemalloc-compat` in `runtime:` an image built from this package would run on Wolfi's jemalloc while the package test ran on challoc.

## `melange test` (both architectures pass with the committed YAMLs)

`challoc.yaml`, the new `challoc-jemalloc-compat-dev` environment (`binutils build-base pkgconf` + the package and its runtime dependency), trace lines removed; aarch64, x86_64 identical:
```
running step "test/tw/ldd-check"                       (on challoc-jemalloc-compat's library)
running step "test/pkgconf"
running step "The link name and jemalloc.pc are jemalloc-dev's"
-I/usr/include/jemalloc                                 (pkg-config --cflags jemalloc)
running step "A consumer that includes <jemalloc/jemalloc.h> builds, and resolves into the compat library"
 0x0000000000000001 (NEEDED)             Shared library: [libjemalloc.so.2]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
mallocx from /lib/libjemalloc.so.2                      (gcc -Wall -Wextra -Werror, C)
malloc from /lib/libjemalloc.so.2
version 5.3.0-0-gchalloc (header 5.3.0-0-gchalloc)
stats.allocated 32768
mallocx from /lib/libjemalloc.so.2                      (g++ -Wall -Wextra -Werror, the same file as C++)
malloc from /lib/libjemalloc.so.2
version 5.3.0-0-gchalloc (header 5.3.0-0-gchalloc)
stats.allocated 32768
___ Begin jemalloc statistics ___                       (malloc_stats_print(NULL, NULL, NULL))
Version: "5.3.0-0-gchalloc"
Build-time option settings
```
The probe program (Ruby's configure shape, `if (malloc_conf) printf(...)`) built and ran silently. The `challoc-jemalloc-compat` environment now asserts the runtime ships no `libjemalloc.so` and no `jemalloc.pc`. (Two earlier runs failed on test-program bugs of mine: a missing `printf` argument, then `#define _GNU_SOURCE` redefined under `g++ -Werror`; both fixed in the committed YAML, no package change.)

`ruby-3.4.yaml`, the main package's environment (`apk-tools binutils ruby-3.4 ruby3.4-bundler` + Wolfi's `help-check ldd-check ver-check`), the added step; aarch64 (x86_64: `rss_kb=177352`, otherwise identical):
```
running step "test/tw/ver-check"      PASS 'ruby'
running step "test/tw/help-check"     tested 7 binaries. 7 passes. 0 fails.
running step "Test hello world script"
Hello Wolfi!
running step "Ruby runs on challoc (the compat libjemalloc.so.2), and says so"
    libjemalloc.so.2 => /lib/libjemalloc.so.2 (0x0000e6a4bdba0000)
/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
challoc-jemalloc-compat
mapped: /usr/lib/libjemalloc.so.2
mallctl.version=5.3.0-0-gchalloc challoc_version=0.1.0 malloc_is_libjemalloc=true
___ Begin jemalloc statistics ___
Version: "5.3.0-0-gchalloc"
Build-time option settings
gc_count=24 live=40000 rss_kb=176172
running step "Validate HTTPS support"
Bundler: success / RubyGems: success / Ruby net/http: success ... No issues found with the installed bundle
running step "test/tw/ldd-check"      PASS on /usr/bin/ruby, libruby.so.3.4, libruby.so.3.4.10 and every extension .so
running test pipeline for subpackage ruby-3.4-doc    test/tw/docs
running test pipeline for subpackage ruby-3.4-dev    test/pkgconf, test/tw/ldd-check: tested 1 files with ldd. 1 passes. 0 fails.
== exit 0
```
The silent assertions of the step (`ruby -e 1`, `JSON.generate` = `{"a":1}`, `MAINLIBS` has `-ljemalloc`, `readelf -d` on `ruby` and `libruby.so.3.4.10` has `NEEDED [libjemalloc.so.2]`, `ldd` resolves to `/(usr/)?lib/libjemalloc.so.2`, `libjemalloc2` not installed, `grep -q` on the Fiddle output and on `Version: "5.3.0-0-gchalloc"`, `MALLOC_CONF=stats_print:true ruby -e 1` prints `challoc` or a jemalloc report) passed under `set -eux`. The first test run failed on my `ldd` assertion (`/lib/` vs `/usr/lib/`: Wolfi's `/lib` is a link to `usr/lib`, and the apko-built test root's loader reports `/lib/`, the wolfi-base container's `/usr/lib/`); the YAML accepts either, and `/proc/self/maps` (the resolved path) is asserted as before.

## Clean-container installation and the checks by hand (aarch64 verbatim; x86_64 identical unless noted)

`apk add ruby-3.4 ruby-3.4-dev apk-tools binutils posix-libc-utils build-base git pkgconf` with both repositories visible (Wolfi's has 3.4.10-r7 with jemalloc; the local one 3.4.10-r8):
```
(49/54) Installing challoc-jemalloc-compat (0.1.0-r0)
(52/54) Installing ruby-3.4 (3.4.10-r8)
(53/54) Installing challoc-jemalloc-compat-dev (0.1.0-r0)
(54/54) Installing ruby-3.4-dev (3.4.10-r8)
OK: 661 MiB in 70 packages                       (x86_64: 601 MiB)
installed: challoc-jemalloc-compat, challoc-jemalloc-compat-dev; `apk info -e libjemalloc2` and `jemalloc-dev` answer with the compat packages (provided names); absent: challoc (the system library is not pulled in)
== ruby -v
ruby 3.4.10 (1980-01-01 revision 78c064747c) +PRISM [aarch64-linux-gnu]
== ruby -e 1
rc=0
== ruby -rjson
{"a":1}
== MAINLIBS / LIBS / DLDLIBS
"-lz -lrt -lrt -ljemalloc -lgmp -ldl -lcrypt -lm -lpthread "
"-lm -lpthread "
"-ljemalloc -lc"
== config.h
#define RUBY_ALTERNATIVE_MALLOC_HEADER <jemalloc/jemalloc.h>
#define HAVE_MALLOC_CONF 1
== readelf -d ruby (NEEDED; no RPATH/RUNPATH, --disable-rpath as Wolfi's)
 libruby.so.3.4  libz.so.1  libjemalloc.so.2  libgmp.so.10  libcrypt.so.1  libm.so.6  libc.so.6  ld-linux-aarch64.so.1
== readelf -d /usr/lib/libruby.so.3.4.10
 libz.so.1  libjemalloc.so.2  libgmp.so.10  libcrypt.so.1  libm.so.6  libgcc_s.so.1  libc.so.6  ld-linux-aarch64.so.1
== scripts/check-needed.sh
/usr/bin/ruby: NEEDED libruby.so.3.4 libz.so.1 libjemalloc.so.2 libgmp.so.10 libcrypt.so.1 libm.so.6 libc.so.6 ld-linux-aarch64.so.1
  ok      libjemalloc.so.2
/usr/lib/libruby.so.3.4.10: NEEDED libz.so.1 libjemalloc.so.2 libgmp.so.10 libcrypt.so.1 libm.so.6 libgcc_s.so.1 libc.so.6 ld-linux-aarch64.so.1
  ok      libjemalloc.so.2
== ldd ruby
	libruby.so.3.4 => /usr/lib/libruby.so.3.4 (0x0000edc4437c0000)
	libz.so.1 => /usr/lib/libz.so.1 (0x0000edc443780000)
	libjemalloc.so.2 => /usr/lib/libjemalloc.so.2 (0x0000edc443700000)
	...
== who owns /usr/lib/libjemalloc.so.2
/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
lrwxrwxrwx    1 root     root            16 Sep 20 13:52 /usr/lib/libjemalloc.so -> libjemalloc.so.2
-rwxr-xr-x    1 root     root        405464 Jan  1  1970 /usr/lib/libjemalloc.so.2
-rw-r--r--    1 root     root           386 Jan  1  1970 /usr/lib/pkgconfig/jemalloc.pc
-rw-r--r--    1 root     root          7576 Jan  1  1970 /usr/include/jemalloc/jemalloc.h
== readelf -d /usr/lib/libjemalloc.so.2
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [ld-linux-aarch64.so.1]
 0x000000000000000e (SONAME)             Library soname: [libjemalloc.so.2]
== a running ruby maps
/usr/lib/libjemalloc.so.2
== Fiddle: mallctl(version), challoc_version, malloc identity, arenas.quantum, malloc_stats_print
mallctl.version=5.3.0-0-gchalloc challoc_version=0.1.0 malloc_is_libjemalloc=true arenas.quantum=16
rc=0
___ Begin jemalloc statistics ___
Version: "5.3.0-0-gchalloc"
Build-time option settings
  config.debug: false
147                                              (lines of the report)
== MALLOC_CONF is read (abort_conf on an unknown key aborts; stats_print is inert)
stats_print rc=0 output lines=1
challoc: MALLOC_CONF: conf key stats_print is accepted but has no effect (not implemented)
abort_conf rc=134
challoc: MALLOC_CONF: unknown conf key "no_such_key"
challoc: abort_conf: 1 configuration problem(s), see above
Aborted (core dumped)
== gem list
84 lines: abbrev (0.1.2), base64 (0.2.0), benchmark (default: 0.4.0), bigdecimal (3.1.8), cgi (default: 0.4.2), csv (3.3.2), date (default: 3.4.1), debug (1.11.0), ...
== bench/ruby/gc_churn.rb 3 3
gc_count=27 major=11 minor=16 forced=3 allocated_objects=846277 freed_objects=788787 heap_live_slots=57490 heap_pages=274 malloc_increase_bytes=2336 oldmalloc_increase_bytes=2336 rss_before_kb=296608 rss_after_kb=292960 hwm_kb=297068
   (x86_64: allocated_objects=846327 freed_objects=788834 heap_live_slots=57493 rss_before_kb=298028 rss_after_kb=294380 hwm_kb=298540)
== swap semantics: apk add jemalloc-dev
OK: 661 MiB in 70 packages                       (a no-op: the name is provided by challoc-jemalloc-compat-dev)
== swap semantics: apk add libjemalloc2
OK: 661 MiB in 70 packages                       (a no-op: provided by challoc-jemalloc-compat)
/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat-0.1.0-r0
== pkg-config jemalloc (what an extension's mkmf would get)
5.3.0
-I/usr/include/jemalloc -ljemalloc
== ruby test subset from ruby v3_4_10 (2b0b7728) against the packaged interpreter
ruby test/runner.rb -q --tty=no test/ruby/test_gc.rb test/ruby/test_gc_compact.rb test/ruby/test_string.rb
    test/ruby/test_hash.rb test/ruby/test_array.rb test/ruby/test_objectspace.rb test/ruby/test_weakmap.rb
Finished tests in 41.293718s, 31.5787 tests/s, 881.0783 assertions/s.
1304 tests, 36383 assertions, 0 failures, 0 errors, 19 skips
   (x86_64: 1304 tests, 36383 assertions, 0 failures, 0 errors, 19 skips, 38 s)
```
Swap semantics, what changed from task 1: task 1's compat package conflicted with `jemalloc-dev` on files and `apk add jemalloc-dev` was refused; with `provides: jemalloc-dev` on the -dev package the same command is a no-op (the dependency is already satisfied), and `apk info -e jemalloc-dev` answers yes. Wolfi's real `jemalloc-dev` can still be installed by removing `challoc-jemalloc-compat-dev` first. `apk add ruby-3.4` from Wolfi's repository alone is unaffected (no `provider-priority`).

## What could not be done, and why

- KeyDB and RethinkDB: not reached; Ruby took the iteration (three melange test iterations on assertion bugs of mine, both architectures, the by-hand checks). What each needs, from Wolfi's files at `a944f08`:
  - `keydb.yaml` (6.3.4, epoch 8) lists `jemalloc-dev` but runs a plain `make -j$(nproc)`: KeyDB's Makefile defaults to `MALLOC=jemalloc` and builds its bundled `deps/jemalloc` (`--with-jemalloc-prefix=je_ --with-lg-quantum=3`), exactly Valkey's shape, so `jemalloc-dev` is unused there as it was in Valkey's file. The switch is Valkey's, not Ruby's: a `MALLOC=challoc` Makefile/`zmalloc.h` patch for KeyDB's tree (a Redis 6.2 lineage; `contrib/valkey/0001-Add-MALLOC-challoc.patch` is written against Valkey 9.1 and will not apply as is) and a private `quantum8` build under `/usr/lib/keydb/`, plus one thing Valkey does not need: KeyDB's active defragmentation calls `je_get_defrag_hint()`, the Redis-patched bundled-jemalloc extension design 13.5 leaves out of scope "unless a Wolfi Redis package appears"; KeyDB is such a package, so either `src/` grows the export (lane A/B territory) or KeyDB is built with active defrag compiled out. Not a `challoc-jemalloc-compat` consumer.
  - `rethinkdb.yaml` (2.4.4, epoch 14) runs `./configure --prefix=/usr --sysconfdir=/etc --fetch protoc` with `jemalloc-dev` in the environment and `gcc-12`; RethinkDB's configure takes the system jemalloc when it finds `<jemalloc/jemalloc.h>` and `-ljemalloc` (otherwise `--fetch jemalloc`), so it is a compat consumer of Ruby's shape: `challoc-jemalloc-compat-dev` in place of `jemalloc-dev`, `challoc-jemalloc-compat` in `runtime:`, a check that `rethinkdb` has `NEEDED libjemalloc.so.2` (RethinkDB's binary calls `mallctl`/`malloc_usable_size` itself, so `--as-needed` keeps the entry), and `rethinkdb --version` in the test. It is a large C++ build (tens of minutes per architecture) and its use of `mallctl` keys (`thread.tcache.flush`, `epoch`, `stats.*`) should be read first against design 13.4's list. Not started.
- `make test-all` on the packaged interpreter was not run (Phase 1 ran it against the same tag and the same library under this soname: `bench/results/2026-09-17-ruby-test-all.md`, nine environmental failures identical under glibc); the 1304-test subset above (GC, GC compaction, String, Hash, Array, ObjectSpace, WeakMap) ran clean on both architectures.
- The prompt's `MALLOC_CONF=stats_print:true ruby -e 1 2>&1 | grep -q challoc`: challoc does not implement `stats_print` (accepted, inert, `src/config.rs` `INERT_KEYS`), so the report is asked for through Fiddle (`malloc_stats_print`, `mallctl("version")`, `challoc_version()`); the `MALLOC_CONF` line is kept in the test in a form that also passes once the key is implemented (`grep -q 'challoc\|jemalloc statistics'`), because challoc's diagnostic for the inert key happens to name it.
- The package test relies on `apk info --who-owns` in the test environment (`apk-tools` added to it); it worked under melange 0.60.0's apko-built root, which carries the installed database. If Wolfi CI's test roots ever lack it, the `readelf --dyn-syms ... challoc_version` check the build step uses is the fallback.

## Diff against Wolfi's `ruby-3.4.yaml` (`a944f08`)

```diff
--- wolfi-dev/os/ruby-3.4.yaml (a944f08)	2026-09-20 09:16:34
+++ contrib/wolfi/ruby/ruby-3.4.yaml	2026-09-20 10:01:58
@@ -1,7 +1,32 @@
+# ruby-3.4 on challoc: Wolfi's ruby-3.4.yaml (wolfi-dev/os main a944f08,
+# 2026-09-19; the file last changed in 1c5cd09, the resolv cherry-pick) with
+# the allocator switched from jemalloc to challoc through
+# challoc-jemalloc-compat (docs/plan.md Phase 3 task 3, design 13.2, 13.6).
+#
+# What changes (contrib/wolfi/README.md, "Ruby", has the reasoning and the
+# results of the first real build):
+#   - the build environment lists challoc-jemalloc-compat-dev in place of
+#     jemalloc-dev: the same file set (/usr/lib/libjemalloc.so, jemalloc.pc,
+#     <jemalloc/jemalloc.h>), so `--with-jemalloc` and Ruby's configure
+#     probe (a read of malloc_conf through the header, -ljemalloc) are
+#     unchanged, and the built ruby/libruby record DT_NEEDED libjemalloc.so.2
+#     as before (Ruby appends --no-as-needed itself, ruby/ruby#4627);
+#   - the package names challoc-jemalloc-compat in its runtime dependencies:
+#     melange derives `so:libjemalloc.so.2` from the binaries, and apk would
+#     satisfy that with Wolfi's libjemalloc2 by name preference (the compat
+#     package only provides it); the explicit dependency picks the compat
+#     library, which is what the package was built and tested against;
+#   - ruby-3.4-dev's runtime dependency jemalloc-dev becomes
+#     challoc-jemalloc-compat-dev (extensions built with mkmf link
+#     -ljemalloc through DLDLIBS);
+#   - a check step after the build and a test step that proves which
+#     allocator a running ruby is on (the mapped file, and mallctl("version")
+#     called from Ruby through Fiddle: "5.3.0-0-gchalloc").
+# Everything else is Wolfi's file, unchanged.
 package:
   name: ruby-3.4
   version: "3.4.10"
-  epoch: 7 # CVE-2026-80212, CVE-2026-80213
+  epoch: 8 # allocator: jemalloc -> challoc (challoc-jemalloc-compat); 7 was CVE-2026-80212, CVE-2026-80213
   description: "the Ruby programming language"
   copyright:
     - license: Ruby
@@ -14,6 +39,10 @@
       - ruby3.4-base64
       - ruby3.4-drb
     runtime:
+      # Not derived: melange records so:libjemalloc.so.2, which apk would
+      # resolve to libjemalloc2 by name preference (challoc-jemalloc-compat
+      # provides it); name the library the package was built against.
+      - challoc-jemalloc-compat
       - glibc
       - gmp
       - libgcc
@@ -41,8 +70,8 @@
       - gdbm-dev
       - glibc-2.44-dev
       - gmp-dev
+      - challoc-jemalloc-compat-dev # in place of jemalloc-dev: the same file set, challoc underneath
       - graphviz
-      - jemalloc-dev
       - libffi-dev
       - libxcrypt-dev
       - linux-headers
@@ -101,6 +130,31 @@
 
   - uses: autoconf/make
 
+  - name: Check the build is on challoc's libjemalloc.so.2
+    runs: |
+      set -eux
+      # configure found the header and linked -ljemalloc (config.h, as
+      # contrib/ruby/build.sh checks): every translation unit includes
+      # <jemalloc/jemalloc.h> through ruby/missing.h.
+      grep -h 'define HAVE_MALLOC_CONF\|define RUBY_ALTERNATIVE_MALLOC_HEADER' .ext/include/*/ruby/config.h
+      grep -q 'define HAVE_MALLOC_CONF 1' .ext/include/*/ruby/config.h
+      grep -q 'define RUBY_ALTERNATIVE_MALLOC_HEADER <jemalloc/jemalloc.h>' .ext/include/*/ruby/config.h
+      # The header is challoc's (challoc-jemalloc-compat-dev), not jemalloc-dev's.
+      grep -q '^#define JEMALLOC_CHALLOC 1' /usr/include/jemalloc/jemalloc.h
+      # DT_NEEDED names the library on the binary and on libruby
+      # (scripts/check-needed.sh's rule; design 13.6: Ruby's --no-as-needed
+      # keeps it on the binary, which references no allocator symbol itself).
+      for f in ruby libruby.so.3.4*; do
+        readelf -d "$f" | grep NEEDED
+        readelf -d "$f" | grep -q 'NEEDED.*\[libjemalloc.so.2\]'
+      done
+      # The library the build environment resolves the soname to is
+      # challoc's (it exports challoc_version; jemalloc's does not). No apk
+      # in a build environment, so the file is asked, not the database.
+      readelf -d /usr/lib/libjemalloc.so.2 | grep -q 'Library soname: \[libjemalloc.so.2\]'
+      ls -l /usr/lib/libjemalloc.so /usr/lib/libjemalloc.so.2
+      readelf --dyn-syms /usr/lib/libjemalloc.so.2 | grep -q ' challoc_version$'
+
   # Update and Extract bundled gems
   - uses: autoconf/make
     with:
@@ -149,7 +203,7 @@
     description: "ruby development headers"
     dependencies:
       runtime:
-        - jemalloc-dev
+        - challoc-jemalloc-compat-dev # was jemalloc-dev: extensions link -ljemalloc (DLDLIBS)
     pipeline:
       - uses: split/dev
     test:
@@ -172,6 +226,8 @@
   environment:
     contents:
       packages:
+        - apk-tools
+        - binutils
         - ruby3.4-bundler
   pipeline:
     - uses: test/tw/ver-check
@@ -194,6 +250,79 @@
         puts("Hello Wolfi!")
         EOF
         ruby /tmp/hello.rb
+    - name: Ruby runs on challoc (the compat libjemalloc.so.2), and says so
+      runs: |
+        set -eux
+        ruby -e 1
+        ruby -rjson -e 'puts JSON.generate(a: 1)' | grep -qx '{"a":1}'
+        ruby -e 'p RbConfig::CONFIG["MAINLIBS"]' | grep -q -- '-ljemalloc'
+        # Linked against the soname (ruby and libruby), the file is the
+        # compat package's.
+        libruby=$(ruby -e 'puts File.join(RbConfig::CONFIG["libdir"], RbConfig::CONFIG["LIBRUBY_SO"])')
+        for f in "$(command -v ruby)" "$libruby"; do
+          readelf -d "$f" | grep -q 'NEEDED.*\[libjemalloc.so.2\]'
+        done
+        # (/lib is a link to usr/lib in Wolfi; ldd prints whichever the
+        # loader searched, /proc/self/maps the resolved path.)
+        ldd "$(command -v ruby)" | grep libjemalloc.so.2
+        ldd "$(command -v ruby)" | grep -qE 'libjemalloc.so.2 => /(usr/)?lib/libjemalloc.so.2'
+        apk info --who-owns /usr/lib/libjemalloc.so.2
+        apk info --who-owns /usr/lib/libjemalloc.so.2 | grep -q challoc-jemalloc-compat
+        ! apk info -e libjemalloc2
+        # A running ruby maps exactly that file.
+        mapped=$(ruby -e 'puts File.readlines("/proc/self/maps").grep(/libjemalloc/).map { |l| l.split[5] }.uniq')
+        echo "mapped: $mapped"
+        case "$mapped" in /usr/lib/libjemalloc.so.2|/lib/libjemalloc.so.2) ;; *) exit 1 ;; esac
+        # And the allocator the process is on reports itself as challoc:
+        # mallctl("version") and challoc_version() called from Ruby through
+        # Fiddle, and the process's malloc is the compat library's malloc.
+        # (MALLOC_CONF=stats_print:true prints nothing under challoc: the key
+        # is accepted and inert, src/config.rs INERT_KEYS; so the report is
+        # asked for directly.)
+        cat > /tmp/alloc.rb <<'EOF'
+        require "fiddle"
+        h = Fiddle::Handle::DEFAULT
+        mallctl = Fiddle::Function.new(h["mallctl"],
+          [Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_SIZE_T],
+          Fiddle::TYPE_INT)
+        out = Fiddle::Pointer.malloc(Fiddle::SIZEOF_VOIDP, Fiddle::RUBY_FREE)
+        len = Fiddle::Pointer.malloc(Fiddle::SIZEOF_SIZE_T, Fiddle::RUBY_FREE)
+        len[0, Fiddle::SIZEOF_SIZE_T] = [Fiddle::SIZEOF_VOIDP].pack("J")
+        rc = mallctl.call("version", out, len, nil, 0)
+        raise "mallctl(version) rc=#{rc}" unless rc == 0
+        version = Fiddle::Pointer.new(out[0, Fiddle::SIZEOF_VOIDP].unpack1("J")).to_s
+        cv = Fiddle::Function.new(h["challoc_version"], [], Fiddle::TYPE_VOIDP).call.to_s
+        lib = Fiddle::Handle.new("libjemalloc.so.2")
+        same = h["malloc"] == lib["malloc"] && h["free"] == lib["free"]
+        puts "mallctl.version=#{version} challoc_version=#{cv} malloc_is_libjemalloc=#{same}"
+        Fiddle::Function.new(h["malloc_stats_print"],
+          [Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP], Fiddle::TYPE_VOID).call(nil, nil, nil)
+        exit(version.include?("challoc") && same ? 0 : 1)
+        EOF
+        ruby /tmp/alloc.rb > /tmp/alloc.out 2> /tmp/alloc.err
+        cat /tmp/alloc.out
+        head -3 /tmp/alloc.err
+        grep -q 'mallctl.version=5.3.0-0-gchalloc' /tmp/alloc.out
+        grep -q 'malloc_is_libjemalloc=true' /tmp/alloc.out
+        grep -q '^Version: "5.3.0-0-gchalloc"' /tmp/alloc.err
+        # MALLOC_CONF reaches the allocator: challoc answers stats_print with
+        # a "challoc: ... accepted but has no effect" diagnostic today and
+        # would print the statistics if it implemented the key; jemalloc
+        # prints its report; glibc prints nothing.
+        MALLOC_CONF=stats_print:true ruby -e 1 2>&1 | grep -q 'challoc\|jemalloc statistics'
+        # The package is seen allocating: medium objects that reach malloc
+        # (strings over ~600 B, hashes over 8 entries), a burst of garbage,
+        # forced full GCs (bench/ruby/gc_churn.rb's shape, cut down).
+        cat > /tmp/churn.rb <<'EOF'
+        live = Array.new(20_000) { |i| "x" * (1024 + (i * 613) % 3072) }
+        hashes = Array.new(20_000) { |i| h = {}; 12.times { |k| h[k] = i + k }; h }
+        3.times { Array.new(50_000) { |i| "y" * (700 + i % 2000) }; GC.start }
+        3.times { GC.start(full_mark: true, immediate_sweep: true) }
+        rss = File.read("/proc/self/status")[/^VmRSS:\s+(\d+)/, 1].to_i
+        puts "gc_count=#{GC.stat[:count]} live=#{live.size + hashes.size} rss_kb=#{rss}"
+        exit(GC.stat[:count] >= 6 && live.all? { |s| s.start_with?("x") } ? 0 : 1)
+        EOF
+        ruby /tmp/churn.rb
     - name: Validate HTTPS support
       runs: |
         bundle init
```
