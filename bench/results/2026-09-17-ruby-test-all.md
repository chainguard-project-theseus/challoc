# Ruby 3.4.10 `--with-jemalloc` against challoc's `libjemalloc.so.2`: `make test-all`

Phase 1 acceptance, last line (docs/plan.md): Ruby 3.4 built `--with-jemalloc`
against challoc's `libjemalloc.so.2` passes `make test-all` in the container,
or the failures are shown to be unrelated to allocation.

## Setup

- Ruby: the version Wolfi ships for 3.4 (`wolfi-dev/os` `ruby-3.4.yaml`):
  tag `v3_4_10`, commit 2b0b7728dc7f0561c35c3d8c4489945c94b783ad, plus Wolfi's
  cherry-pick b92c8c225351f6d61a8aabd54e6f57d5a7503a53 (fiber_switch UAF fix).
  Configured with Wolfi's flags `--enable-shared --disable-rpath
  --sysconfdir=/etc --enable-mkmf-verbose --enable-yjit --with-jemalloc`,
  `CFLAGS=-DOPENSSL_NO_ENGINE`, plus `--disable-install-doc` and a prefix under
  `target/`; `LDFLAGS=-L<stage>/lib -Wl,-rpath,<stage>/lib` points at the
  stage. Bundled gems via `make update-gems extract-gems` as Wolfi does.
- challoc: `make jemalloc-compat` stage (quantum16, `jemalloc-soname` feature),
  built from the tree at the commit named below. Ruby's configure probe links a
  program that *reads* `malloc_conf` against `-ljemalloc`; the library now
  defines that symbol (null, exported, read through the GOT so a program's own
  definition still interposes), otherwise the probe reports "jemalloc requested
  but not found". With the change: `checking for jemalloc library... -ljemalloc`.
- Checks before the suite (`contrib/ruby/build.sh`): `ruby` and
  `libruby.so.3.4` record `libjemalloc.so.2` in `DT_NEEDED` with a runpath to
  the stage, `config.h` has `HAVE_MALLOC_CONF 1` and
  `RUBY_ALTERNATIVE_MALLOC_HEADER <jemalloc/jemalloc.h>` (Wolfi's
  `jemalloc-dev` header; Ruby then uses `malloc_usable_size` and does not call
  `malloc_trim`), and a running `ruby` maps
  `target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2` and nothing else
  called libjemalloc. The last check matters: Wolfi's real jemalloc 5.3 lives
  in `/usr/lib` under the same soname.
- Suite: `make test-all TESTOPTS="-j16 -q --tty=no"` (Ruby's runner retries
  failed files serially), run as root inside the dev container
  (`Dockerfile.dev`, Wolfi) on the two GCE reference VMs, zone `us-east4-a`:
  `challoc-bench-arm64` (c4a-standard-16, Neoverse V2) and `challoc-bench-amd64`
  (c4-standard-16, Xeon 8581C), Ubuntu 24.04 kernel 7.0.0-1011-gcp. Full logs
  in each VM's target volume under `target/ruby/logs/`.
- Baseline for failures: the same source configured `--without-jemalloc`
  (glibc malloc, `make ruby ALLOC=glibc`), same container, same VM, run on
  the failing test files (`TESTS=...`) back to back with the challoc build.

## Run 1: container without tzdata (both VMs, image before the tzdata line)

| arch    | tests | assertions | failures | errors | skips | wall  |
|---------|-------|------------|----------|--------|-------|-------|
| aarch64 | 32826 | 6587062    | 102      | 2      | 409   | 382 s |
| x86_64  | 32812 | 6585851    | 102      | 2      | 407   | 60 s  |

Identical failing set on both architectures (104 tests):

| count | test                                                   | what it needs                                                |
|-------|--------------------------------------------------------|--------------------------------------------------------------|
| 95    | `TestTimeTZ#*`                                         | zone files (`/usr/share/zoneinfo`); every zone came back as +0000 |
| 4     | `OpenSSL::TestSSL#test_tmp_dh{,_callback}`, `#test_connect_works_when_setting_dh_callback_to_nil`, `#test_tlsext_hostname` | "no shared cipher" / "tlsv1 unrecognized name" from Wolfi's OpenSSL policy, not memory |
| 1     | `OpenSSL::TestProvider#test_openssl_legacy_provider`   | OpenSSL's legacy provider (RC4) is not installed             |
| 1     | `TestTmpdir#test_world_writable`                       | a non-root user (root writes to any TMPDIR)                  |
| 1     | `TestFileUtils#test_rm_r_no_permissions`               | a non-root user (root removes 000-mode directories)          |
| 1     | `RDocParserTest#test_class_for_forbidden`              | a non-root user (root reads a 000-mode file)                 |
| 1     | `TestFiddle#test_dlopen_linker_script_group_linux`     | Wolfi's `libc.so` linker script names `libc.so-2`; the test expects `libc.so` |

None of these is an allocation failure: no crash, no abort, no `challoc:`
line, no memory-related assertion. The 95 zone failures are the container
lacking `tzdata`, so the image gained it and the suite was rerun (run 2).


## Run 2: container with tzdata (the image at commit 38f1a16)

Library, stage, build script and image from the tree of commit 38f1a16
(the challoc `malloc_conf` change included). Same VMs, same command.

| arch    | tests | assertions | failures | errors | skips | wall  |
|---------|-------|------------|----------|--------|-------|-------|
| aarch64 | 32827 | 6587582    | 7        | 2      | 409   | 413 s |
| x86_64  | 32834 | 6587217    | 7        | 2      | 407   | 51 s  |

The same 9 tests on both architectures, the run-1 list minus `TestTimeTZ`:
`OpenSSL::TestSSL#test_tmp_dh`, `#test_tmp_dh_callback`,
`#test_connect_works_when_setting_dh_callback_to_nil`,
`#test_tlsext_hostname`, `OpenSSL::TestProvider#test_openssl_legacy_provider`,
`TestTmpdir#test_world_writable`, `TestFileUtils#test_rm_r_no_permissions`
(error), `RDocParserTest#test_class_for_forbidden`,
`TestFiddle#test_dlopen_linker_script_group_linux`. The wall-time gap is
Ruby's serial retry pass after the parallel run plus slower per-file setup on
the arm64 VM; all files ran on both.

## Proof the 9 failures are not allocation

The seven test files that hold them (`ruby/test_time_tz.rb` included, to
show the tzdata fix) were rerun back to back with `TESTS=...` in the same
container on each VM, once under the challoc build and once under the same
source configured `--without-jemalloc` (glibc malloc; `ruby` and
`libruby.so.3.4` verified to have no `libjemalloc` in `DT_NEEDED`):

| arch    | allocator | tests | assertions | failures | errors | skips |
|---------|-----------|-------|------------|----------|--------|-------|
| aarch64 | challoc   | 408   | 3004       | 7        | 2      | 1     |
| aarch64 | glibc     | 408   | 3004       | 7        | 2      | 1     |
| x86_64  | challoc   | 408   | 3004       | 7        | 2      | 1     |
| x86_64  | glibc     | 408   | 3004       | 7        | 2      | 1     |

The sorted list of failing test names is byte-identical between the challoc
full run and the glibc subset run (checked with `md5sum` on aarch64; the
x86_64 lists were compared by name, the same 9), and the failure messages
are the same: OpenSSL "no shared cipher" / "tlsv1 unrecognized name" / RC4
"unsupported", the three root-permission cases, and `libc.so-2`. All 95
`TestTimeTZ` cases pass under both allocators once tzdata is installed.

Reading: Ruby 3.4.10 built `--with-jemalloc` against challoc's
`libjemalloc.so.2` passes `make test-all` on both architectures except for
9 tests that fail identically under glibc malloc, each for a container or
Wolfi-environment reason (root, OpenSSL policy and providers, linker-script
name). No crash, abort, hang or memory assertion under challoc in 32.8k tests
per architecture. The acceptance line is met in its second form.

## Also on this tree

`make test` (219 host-style + 76 `api::` tests, including the new
`malloc_conf` cases) passes natively on both VMs at commit 38f1a16.
Timings: Ruby configure + build + gems about 4 min on x86_64 and 6 min on
aarch64 at `-j16`; the glibc baseline build the same again.
