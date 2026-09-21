# contrib/wolfi: the Wolfi package definitions

`challoc.yaml` is the melange definition for the Wolfi packages of
docs/plan.md Phase 3 task 1 (design 13.6; three packages, plus the
`challoc-jemalloc-compat-dev` header package task 3 added);
`valkey/valkey-9.1.yaml` is Wolfi's Valkey 9.1 package switched to
challoc (task 2, the "Valkey" section below); `ruby/ruby-3.4.yaml`,
`keydb/keydb.yaml` and `rethinkdb/rethinkdb.yaml` are Wolfi's Ruby 3.4,
KeyDB 6.3 and RethinkDB 2.4 switched to `challoc-jemalloc-compat` (task
3, the "Ruby", "KeyDB" and "RethinkDB" sections). Phase 3 runs in the
wolfi-dev/os repository; this directory tracks the drafts and the recipes
that build and test them here. Results of the first real builds
(2026-09-20): `bench/results/2026-09-20-phase3-task1-wolfi-package.md`,
`bench/results/2026-09-20-phase3-task2-valkey-package.md`,
`bench/results/2026-09-20-phase3-task3-ruby-package.md`,
`bench/results/2026-09-20-phase3-task3-keydb-package.md` and
`bench/results/2026-09-20-phase3-task3-rethinkdb-package.md`.

## Packages

| package | files | notes |
|---|---|---|
| `challoc` | `/usr/lib/libchalloc.so.0` | the runtime: `quantum16`, `DT_SONAME libchalloc.so.0`; `provides so:libchalloc.so.0` (auto). Nothing else: the link name is the -dev package's, as Wolfi's `split/dev` does for every library. |
| `challoc-dev` | `/usr/lib/libchalloc.so -> libchalloc.so.0`, `/usr/include/challoc/jemalloc.h`, `/usr/lib/pkgconfig/challoc.pc` | `runtime: challoc=<version>`. The header is `contrib/valkey/include/challoc/jemalloc.h` (the `je_` API under `<challoc/...>`, never `<jemalloc/...>`). `challoc.pc` is written by the pipeline: `Version` is the crate version, `Libs: -L${libdir} -Wl,--push-state,--no-as-needed -lchalloc -Wl,--pop-state` (see "as-needed" below). |
| `challoc-jemalloc-compat` | `/usr/lib/libjemalloc.so.2` | `quantum16` built with the `jemalloc-soname` feature and staged by `scripts/jemalloc-compat.sh` (`QUANTUM=16 PREFIX=/usr`). The file set of Wolfi's `libjemalloc2`, which it `provides` (plus the automatic `so:libjemalloc.so.2=2`). Task 1 had the link name and the pc file here too; task 3 moved them to the -dev package, where `jemalloc-dev` keeps its own. |
| `challoc-jemalloc-compat-dev` | `/usr/lib/libjemalloc.so -> libjemalloc.so.2`, `/usr/lib/pkgconfig/jemalloc.pc`, `/usr/include/jemalloc/jemalloc.h` | `runtime: challoc-jemalloc-compat=<version>`; `provides: jemalloc-dev=<version>` (the file set of Wolfi's `jemalloc-dev`, so the two conflict and apk refuses to co-install them). The pc file is the stage's (`Version: 5.3.0`, upstream's `jemalloc.pc.in` fields, `Cflags: -I${includedir}/jemalloc`, `Libs: -L${libdir} -ljemalloc`); the header is `contrib/jemalloc-compat/include/jemalloc/jemalloc.h`: jemalloc 5.3's unprefixed API as challoc exports it (`malloc_conf`, `malloc_message`, the `mallocx` family, `mallctl*`, `malloc_stats_print`, the `MALLOCX_*`/`MALLCTL_ARENAS_*` and `JEMALLOC_VERSION*` macros, the `je_` spellings as prototypes) with the standard entry points left to `<stdlib.h>`/`<malloc.h>` so C++ sees one declaration of `malloc`. `scripts/jemalloc-compat.sh` stages the same header under `include/jemalloc/`, so the stage and the packages have one file set. See "Ruby" below. |

Valkey's private `quantum8` build is not one of these packages: it is
built inside `valkey/valkey-9.1.yaml` (`/usr/lib/valkey/`, design 13.6:
two libraries with one soname cannot share `/usr/lib`, so the system
library is never `quantum8`); see "Valkey" below.

Nothing is shipped from `libchalloc.a`: the Phase 1 task 6 decision (no
`libjemalloc.a`) holds for the system package too until a static link is
tested; a `challoc-static` subpackage (`split/static`) is a one-line
addition when it is.

## Source

The pipeline's first step is `git-checkout` of
`https://github.com/chainguard-dev/challoc` at `v${{package.version}}`,
guarded by a build option: `--build-option local-source` skips it and
builds the tree melange was given with `--source-dir` instead. The
repository has no release tag and is not public (2026-09-20), so the
`git-checkout` path has not been exercised and its `expected-commit`
is the `main` commit the file was written against (`4a7d30c`); the Wolfi
PR that carries the file sets the tag and its commit and drops the
option if Wolfi's conventions prefer. The pipeline's second step checks
that `Cargo.toml`'s version equals `package.version` and that
`src/api/mallctl.rs`'s `VERSION_STR` starts with `vars.jemalloc-version`
(5.3.0), so a bump in either place fails the build until the YAML follows.

Toolchain, Wolfi style: `rust-${{vars.rust-version}}` (1.98 today;
`edition = "2024"` needs 1.85+), `build-base`, `glibc-2.44-dev`,
`wolfi-base`. melange assembles the build root from these, so the
`glibc-2.44` stream swap Dockerfile.dev performs on top of `wolfi-base` is
not needed here: `glibc-2.44-dev` selects the stream. Two `cargo build
--release --locked` runs (`--features quantum16` and, through
`scripts/jemalloc-compat.sh`, `quantum16,jemalloc-soname` into its own
target dir), about 8 s each on a 16-vCPU VM.

## Building locally

Nothing here runs on macOS. melange needs bubblewrap and user namespaces,
which the Mac Docker VM does not give a container; the recipe below is
what ran on the lane VMs (Ubuntu 24.04 on GCE, kernel 7.0, Docker 29). It
uses Wolfi's own `melange` and `bubblewrap` apks in a privileged
`wolfi-base` container and Wolfi's `pipelines/` directory for the
`test/tw/*` and `test/pkgconf` steps, which live in wolfi-dev/os, not in
melange.

```sh
# on the VM: ~/challoc is the synced tree (challoc-bench sync), ~/melange-out the output
sudo docker run --rm --privileged -v ~/challoc:/work -v ~/melange-out:/out -w /work \
  cgr.dev/chainguard/wolfi-base sh -c '
set -eu
apk add -q melange bubblewrap git binutils
git clone -q --depth 1 --filter=blob:none --sparse https://github.com/wolfi-dev/os /tmp/os \
  && git -C /tmp/os sparse-checkout set pipelines
[ -f /out/melange.rsa ] || melange keygen /out/melange.rsa
WOLFI="--repository-append https://packages.wolfi.dev/os \
       --keyring-append https://packages.wolfi.dev/os/wolfi-signing.rsa.pub"
melange build /work/contrib/wolfi/challoc.yaml --arch $(uname -m) \
  --source-dir /work --build-option local-source \
  --pipeline-dirs /tmp/os/pipelines $WOLFI \
  --signing-key /out/melange.rsa --out-dir /out/packages --runner bubblewrap
melange test /work/contrib/wolfi/challoc.yaml --arch $(uname -m) \
  --pipeline-dirs /tmp/os/pipelines \
  $WOLFI --repository-append /out/packages --keyring-append /out/melange.rsa.pub \
  --runner bubblewrap
'
```

Notes learned the hard way:

- Pass the config by absolute path. With `--source-dir` melange opens the
  config relative to the workspace and `contrib/wolfi/challoc.yaml`
  becomes "open challoc.yaml: no such file or directory".
- The Wolfi repository and key go on the command line (as wolfi-dev/os's
  Makefile passes them), not in the YAML; without them melange stops at
  "must provide at least one repository".
- `melange build` takes `--pipeline-dir`, `melange test` takes
  `--pipeline-dirs` (0.60.0).
- `--source-dir /work` copies the whole tree into the workspace, `.git`
  included; keep `target/` out of the synced tree (the lane VMs keep it
  in a Docker volume, so it never is).
- The `WARN invalid license: NOASSERTION` lines at the end of the build
  are about the config file's own license (`melange build --license`),
  not the package's.

Output: `/out/packages/<arch>/{challoc,challoc-dev,challoc-jemalloc-compat}-0.1.0-r0.apk`
plus a signed `APKINDEX.tar.gz`. Installing them into a clean
`cgr.dev/chainguard/wolfi-base` container:

```sh
sudo docker run --rm -v ~/challoc:/work:ro -v ~/melange-out:/out:ro cgr.dev/chainguard/wolfi-base sh -c '
cp /out/melange.rsa.pub /etc/apk/keys/ && echo /out/packages >> /etc/apk/repositories && apk update
apk add challoc challoc-dev challoc-jemalloc-compat binutils build-base pkgconf posix-libc-utils
...'
```

## Test matrix

`melange test` runs one clean environment per package (the package, its
runtime dependencies and `test.environment.contents.packages`); the same
checks were then repeated by hand in a clean `wolfi-base` container from
the local repository. Both passed on aarch64 and x86_64 on 2026-09-20; the
outputs are in the results file.

| package | check | how |
|---|---|---|
| challoc | no missing dependency | `test/tw/ldd-check` on `/usr/lib/libchalloc.so.0` |
| challoc | soname | `readelf -d /usr/lib/libchalloc.so.0` has `Library soname: [libchalloc.so.0]` |
| challoc | loads and runs | `ldd` clean; `LD_PRELOAD=/usr/lib/libchalloc.so.0 ls`, `... readelf -h` |
| challoc | no link name in the runtime | `! [ -e /usr/lib/libchalloc.so ]` |
| challoc-dev | pc file | `test/pkgconf`; `pkg-config --modversion challoc` = 0.1.0 |
| challoc-dev | link name | `readlink /usr/lib/libchalloc.so` = `libchalloc.so.0` |
| challoc-dev | a consumer links and runs | `gcc $(pkg-config --cflags challoc) version.c $(pkg-config --libs challoc)` using `<challoc/jemalloc.h>` (`je_malloc`, `je_mallctl("version")`) and `challoc_version()`; `DT_NEEDED libchalloc.so.0`; prints `challoc_version=0.1.0 ... mallctl.version=5.3.0-0-gchalloc` |
| challoc-dev | as-needed rule (design 13.6) | a program referencing nothing, linked with `$(pkg-config --libs challoc)`, still has `DT_NEEDED libchalloc.so.0`; by hand also the negative: with a plain `-lchalloc` Wolfi's gcc drops it (`scripts/check-needed.sh` says `MISSING`) |
| challoc-jemalloc-compat | no missing dependency | `test/tw/ldd-check` on `libjemalloc.so`, `libjemalloc.so.2` |
| challoc-jemalloc-compat | soname, link name | `Library soname: [libjemalloc.so.2]`; `readlink /usr/lib/libjemalloc.so` = `libjemalloc.so.2` |
| challoc-jemalloc-compat | pc file | `test/pkgconf`; `pkg-config --modversion jemalloc` = 5.3.0; `--atleast-version=5.3`; `--libs` = `-ljemalloc`; `--cflags` = `-I/usr/include/jemalloc` |
| challoc-jemalloc-compat | link-only consumer | `gcc compat.c $(pkg-config --libs jemalloc) -ldl` (strong `mallocx`, `dallocx`, `mallctl`): `DT_NEEDED libjemalloc.so.2`, `dladdr` puts `mallocx` and `malloc` in `.../libjemalloc.so.2`, `mallctl("version")` = `5.3.0-0-gchalloc` |
| challoc-jemalloc-compat | LD_PRELOAD run | `LD_PRELOAD=/usr/lib/libjemalloc.so.2 ls /` |
| by hand | `scripts/check-needed.sh` | on the compat consumer (`ok libjemalloc.so.2`) and on the challoc-dev consumers (`ok`/`MISSING` as above) |
| by hand | `tests/c/jemalloc_link.c` | the Phase 1 task 6 consumer, `-DQUANTUM=16`, linked with the installed pc file, `./jl /usr/lib`: `ok jemalloc_link` |

## Swapping a jemalloc consumer (what `provides` does)

Measured in clean containers with both Wolfi's repository and the local
one configured (results file, "co-installation"):

- `apk add ruby-3.4` alone installs Wolfi's `libjemalloc2` for
  `so:libjemalloc.so.2`: apk prefers the package that carries the name
  over a provider, so nothing changes for existing consumers merely because
  `challoc-jemalloc-compat` exists in the repository. No
  `provider-priority` is set for the same reason; setting one would flip
  every jemalloc consumer at once.
- `apk add challoc-jemalloc-compat` on top purges `libjemalloc2` and
  installs the compat library (`provides: libjemalloc2` plus the same
  `so:` name); Ruby 3.4.10 then runs on challoc
  (`/usr/lib/libjemalloc.so.2 is owned by challoc-jemalloc-compat`).
  `apk add challoc-jemalloc-compat ruby-3.4` in one go does the same, so a
  consumer package swaps by naming `challoc-jemalloc-compat` in its
  runtime dependencies (and in `environment.contents.packages` in place
  of `jemalloc-dev`), which is Phase 3 task 3.
- `apk add libjemalloc2` while the compat package is installed is a no-op
  (the name is provided); `apk add jemalloc-dev` is refused
  (`conflicts: challoc-jemalloc-compat[pc:jemalloc]`): the two are
  alternatives, by design.
- A consumer that includes `<jemalloc/jemalloc.h>` (Ruby's
  `--with-jemalloc`) lists `challoc-jemalloc-compat-dev` in place of
  `jemalloc-dev` in its build environment (task 3; the task 1 open point).
  The -dev package carries the header, the link name and the pc file, the
  file set of `jemalloc-dev`, so the consumer's configure sees nothing
  different; `apk add jemalloc-dev` is refused while it is installed and
  a dependency on `jemalloc-dev` is satisfied by it (`provides`), with no
  `provider-priority`, so nothing flips merely because it exists. The
  consumer's runtime dependency must still name `challoc-jemalloc-compat`
  explicitly: melange derives `so:libjemalloc.so.2` from the binaries and
  apk would resolve that to `libjemalloc2` by name preference (the results
  file of task 3, "Package metadata").

## Valkey

`valkey/valkey-9.1.yaml` is wolfi-dev/os's `valkey-9.1.yaml` (main at
`a944f08`, 2026-09-19; the file last changed in `195f812`, the 9.1.2
update) with the allocator switched from the bundled jemalloc to challoc.
`valkey/valkey-9.1/` holds the patches the pipeline applies (Wolfi keeps
them in a directory named after the package): Wolfi's own
`0000-Disable-protected-mode.patch` and a copy of
`contrib/valkey/0001-Add-MALLOC-challoc.patch` (`valkey/stage.sh` refuses
to build if the two copies differ). `bench/results/2026-09-20-phase3-task2-valkey-package.md`
has the diff against Wolfi's file and the build.

### Two libraries, one soname

| library | build | who loads it |
|---|---|---|
| `/usr/lib/libchalloc.so.0` (package `challoc`) | `quantum16`, the class table Wolfi's jemalloc (5.3.1, default `lg_quantum` 4) consumers already run on | anything linked with `-lchalloc`, `LD_PRELOAD`, Ruby through `challoc-jemalloc-compat` |
| `/usr/lib/valkey/libchalloc.so.0` (package `valkey-9.1`) | `quantum8`, the same source with `--features quantum8` | `valkey-server` through its `DT_RPATH /usr/lib/valkey`; `valkey-cli` and `valkey-benchmark` (linked the same way) when the server package is installed, the system library otherwise |

Why Valkey needs its own: `allocator_defrag.c` asserts
`arenas.quantum == 8` at start (`allocatorDefragInit`, from the
`--with-lg-quantum=3` of Valkey's bundled jemalloc) and its bin index
arithmetic (`jeSize2BinIndexLgQ3`) assumes that class table; challoc's
class table is compile time, one quantum per build (design section 3), and
a second file with the soname `libchalloc.so.0` cannot live in `/usr/lib`.
So Valkey gets the allocator the way it got jemalloc: built from source
in its own package, private to it. The results file shows the both-ways
check: with the system library forced in front (`LD_PRELOAD`) the server
dies at start in `main -> allocatorDefragInit -> _serverAssert` (the
quantum assertion fires before the log is configured, so it is a
segfault in `serverLogRaw` with no message rather than the assertion
text); with `LD_LIBRARY_PATH=/usr/lib` the private library still wins,
because `DT_RPATH` is searched before `LD_LIBRARY_PATH`.

Why it is built inside `valkey-9.1.yaml` and not shipped as a
`challoc-valkey` subpackage of `challoc.yaml` (the other layout considered):

- melange's SCA (`pkg/sca/sca.go`, `generateSharedObjectNameDeps`) turns
  every `DT_NEEDED` of an installed ELF file into a `so:` runtime
  dependency, and lists a shared object's soname under `provides:` only
  when the file sits directly in `lib/`, `usr/lib/`, `lib64/` or
  `usr/lib64/` (`isInDir`, exact match, plus `/etc/ld.so.conf.d` paths);
  anywhere else it goes under `vendored:`. `removeSelfProvidedDeps`
  (`pkg/build/package.go`) then drops a package's own `so:` dependency
  when the same package vendors that soname. Built in the Valkey package,
  `/usr/lib/valkey/libchalloc.so.0` is therefore `vendored:
  so:libchalloc.so.0=0`, `valkey-server`'s `so:libchalloc.so.0` is
  pruned, and the package neither provides the soname (nothing else can
  pick the `quantum8` build) nor depends on any challoc package. As a
  separate package the library would still be `vendored:` (fine), but
  `valkey-9.1` would keep an automatic `so:libchalloc.so.0` dependency
  that apk satisfies with the system `challoc` package: a second, unused
  copy in every Valkey image, removable only with `no-depends` and a
  hand-written dependency list.
- The client tools are the exception either way. Valkey links every
  binary with `FINAL_LIBS`, so `valkey-cli` and `valkey-benchmark` carry
  the same `DT_NEEDED libchalloc.so.0` and rpath, and the `-cli` and
  `-benchmark` subpackages (which do not vendor the library) get the
  automatic `so:libchalloc.so.0` dependency, satisfied by the system
  `challoc`. That is correct for them: neither has a quantum requirement
  (`valkey-cli --version` runs with the system library preloaded, the
  results file shows it), and next to `valkey-9.1` the rpath makes them
  load the private copy. A server image with the cli therefore carries
  both builds (about 400 KiB for the system one); a static link from
  `libchalloc.a` into the three binaries would remove the second library
  at the price of three copies of the allocator code, and is the
  alternative design 13.6 leaves open, untested here.
- It is what design 13.6 says ("a private `quantum8` build of the same
  source in its own package ... the way it gets jemalloc today") and how
  Wolfi handles a library a consumer must carry privately: built in the
  consumer's pipeline, recorded in the SBOM through its own `git-checkout`
  step and the `vendored:` line, and rebuilt by an epoch bump of the
  consumer when the library changes. The cost is one `cargo build` per
  Valkey stream (about 20 s with Wolfi's `rust-1.98`) and `rust` in the
  build environment.

Changes from Wolfi's file, in pipeline order: `epoch` 0 -> 1 with the
reason; a second `copyright` entry (Apache-2.0 for
`/usr/lib/valkey/libchalloc.so.0`); `vars` for the challoc version, the
Rust version and the allocator name; the `local-challoc` build option;
`jemalloc` dropped from the environment (it was unused: the default
`MALLOC=jemalloc` builds `deps/jemalloc`), `binutils` and
`rust-1.98` added; the challoc patch in the `patch` step; a `git-checkout`
of challoc into `challoc/` (skipped under `local-challoc`, when
`stage.sh` has already put the tree there); a `cargo build --release
--locked --no-default-features --features quantum8` step; the build step
gains `-I$PWD/challoc/contrib/valkey/include` in `CFLAGS`,
`-L<cargo output> -Wl,-rpath,/usr/lib/valkey` in `LDFLAGS` and
`MALLOC=challoc` on both `make` invocations (Valkey's Makefile persists
its settings; an install without it rebuilds with the bundled jemalloc),
then installs the library as `/usr/lib/valkey/libchalloc.so.0`; a check
step (`--version` says `malloc=jemalloc-5.3.0-challoc`,
`scripts/check-needed.sh` finds `libchalloc.so.0` in `DT_NEEDED`, the
rpath is `/usr/lib/valkey`); and the `test:` block below. The
subpackages, `update:` and the `redis-*` checks are Wolfi's unchanged.

`-Wl,-rpath` gives `DT_RPATH` with Wolfi's binutils (2.47, no
`--enable-new-dtags` default). Design 13.6 says "RUNPATH"; the definition
and its test accept either (`RUNPATH|RPATH`), and the difference is in
Valkey's favour: `DT_RPATH` is searched before `LD_LIBRARY_PATH`, so an
environment pointing at `/usr/lib` cannot swap the private library out
(only `LD_PRELOAD` can, and then the server aborts at start rather than
running on the wrong class table). No `--no-as-needed` is needed:
`valkey-server` references `je_mallctl` and friends itself, so
`--as-needed` keeps the entry; the check step and the test verify it
rather than assume it.

### Package test

`melange test` of `valkey-9.1` (environment: the `-cli` and `-benchmark`
subpackages, `binutils`), in this order:

| check | how |
|---|---|
| no missing dependency, private library included | `test/tw/ldd-check` (it walks `/usr/lib/valkey/libchalloc.so.0` too) |
| the build says challoc | `valkey-server --version` contains `malloc=jemalloc-5.3.0-challoc` |
| linked against the soname, found through the private path | `readelf -d /usr/bin/valkey-server` has `NEEDED [libchalloc.so.0]` and `RPATH`/`RUNPATH [/usr/lib/valkey]`; the library's soname is `libchalloc.so.0`; `valkey-cli` and `valkey-benchmark` have the same two entries and run |
| a running server loads the private build | server on a unix socket (`port 0`, no free port needed); `/proc/<pid>/maps` names exactly `/usr/lib/valkey/libchalloc.so.0` |
| the allocator reports itself | `INFO memory` has `mem_allocator:jemalloc-5.3.0-challoc` |
| active defrag is available | `CONFIG SET activedefrag yes` -> `OK`, `CONFIG GET` -> `yes` (the bundled-jemalloc-less error would be "Active defragmentation cannot be enabled") |
| purge and stats | `MEMORY PURGE` -> `OK`; `MEMORY MALLOC-STATS` begins with `___ Begin jemalloc statistics ___` |
| traffic | Wolfi's `SET`/`GET`/`EXISTS`, then `valkey-benchmark -t set,get -n 20000 -q`, `DBSIZE`, `INFO memory` |
| clean shutdown | `SHUTDOWN NOSAVE`; the log has no `challoc:` message, assertion or crash report |
| Wolfi's own | the `redis-*`/`valkey-*` `--version` and `Usage:` checks |

The `-cli` and `-benchmark` tests are Wolfi's. Their environments hold
nothing but the package, so a local `melange test` needs
`--test-package-append=wolfi-base` as wolfi-dev/os's Makefile passes it.

### Building and testing locally

Same VM and container as "Building locally" above (melange with
bubblewrap in a privileged `wolfi-base` container, Wolfi's `pipelines/`
at `a944f08`). The Valkey definition needs the challoc tree in its source
directory, which `valkey/stage.sh` assembles:

```sh
sh /work/contrib/wolfi/valkey/stage.sh /tmp/stage     # the two patches + challoc/ (no target/, .git)
melange build /work/contrib/wolfi/valkey/valkey-9.1.yaml --arch $(uname -m) \
  --source-dir /tmp/stage --build-option local-challoc \
  --pipeline-dir /tmp/os/pipelines $WOLFI \
  --signing-key /out/melange.rsa --out-dir /out/packages --runner bubblewrap
melange test /work/contrib/wolfi/valkey/valkey-9.1.yaml --arch $(uname -m) \
  --pipeline-dirs /tmp/os/pipelines $WOLFI \
  --repository-append /out/packages --keyring-append /out/melange.rsa.pub \
  --test-package-append=wolfi-base --runner bubblewrap
```

The Valkey checkout comes from GitHub (the build environment has the
network); the challoc checkout is skipped by the option. In a clean
`wolfi-base` container with the local repository configured, `apk add
valkey-9.1` alone installs no challoc package (`apk add valkey-9.1-cli`
brings the system `challoc` for its `so:` dependency); with both
libraries installed `valkey-server` maps only
`/usr/lib/valkey/libchalloc.so.0`. Valkey's
own `unit/memefficiency` (the Phase 1 gate) runs against the packaged
binary from a 9.1.2 checkout with the contrib patch applied (for its
`memefficiency.tcl` hunk) and `src/valkey-server` symlinked to
`/usr/bin/valkey-server`; the results file has the summary. The full
suite is `make valkey` in this repository, not the package test.

Two more things learned:

- The check step in the pipeline runs the freshly built `valkey-server`
  before the package is installed, where the rpath does not exist yet;
  it sets `LD_LIBRARY_PATH` to the staged `usr/lib/valkey` (the cargo
  output is named `libchalloc.so`, and the loader wants the soname file).
- `MALLOC=challoc` must go on the `make install` too: Valkey's
  `.make-settings` machinery rebuilds everything when the allocator
  setting changes between invocations, and the rebuild would be with the
  bundled jemalloc.

## Ruby

`ruby/ruby-3.4.yaml` is wolfi-dev/os's `ruby-3.4.yaml` (main at
`a944f08`, 2026-09-19; the file last changed in `1c5cd09`, the resolv
0.7.2 cherry-pick, `epoch: 7`) with the allocator switched from Wolfi's
jemalloc to challoc through `challoc-jemalloc-compat`.
`ruby/ruby-3.4/remove-already-provided-gems-from-gems_bundled_gems.patch`
is Wolfi's own patch, unchanged (Wolfi keeps it in a directory named after
the package, and melange resolves `patches:` against `--source-dir`).
`bench/results/2026-09-20-phase3-task3-ruby-package.md` has the diff
against Wolfi's file, the build and every assertion's output.

The changes, in file order:

| where | Wolfi | here | why |
|---|---|---|---|
| `epoch` | 7 | 8 | the allocator switch is a rebuild of the same version |
| `dependencies.runtime` | (derived) | `challoc-jemalloc-compat` added | melange derives `so:libjemalloc.so.2` from `ruby` and `libruby.so.3.4`; apk resolves a bare `so:` name to the package that carries it (`libjemalloc2`) over a provider, so without the explicit name an image would run the package on Wolfi's jemalloc, not on what it was built and tested against |
| `environment.contents.packages` | `jemalloc-dev` | `challoc-jemalloc-compat-dev` | the same file set (`libjemalloc.so`, `jemalloc.pc`, `<jemalloc/jemalloc.h>`); configure's `--with-jemalloc` probe (a read of `malloc_conf` through the header, then `-ljemalloc`) is unchanged, `HAVE_MALLOC_CONF` and `RUBY_ALTERNATIVE_MALLOC_HEADER <jemalloc/jemalloc.h>` are defined as before, and `--no-as-needed` (Ruby's own, ruby/ruby#4627) keeps `DT_NEEDED libjemalloc.so.2` on the binary that references no allocator symbol itself (design 13.6) |
| after `autoconf/make` | - | a check step | `config.h` has `HAVE_MALLOC_CONF 1` and the header path; the header is challoc's (`JEMALLOC_CHALLOC`); `ruby` and `libruby.so.3.4*` have `NEEDED libjemalloc.so.2`; the environment's `libjemalloc.so.2` exports `challoc_version` (a build environment has no `apk`, so the file is asked, not the database) |
| `ruby-3.4-dev` `runtime` | `jemalloc-dev` | `challoc-jemalloc-compat-dev` | extensions built with mkmf link `-ljemalloc` through `DLDLIBS`; the -dev package satisfies a `jemalloc-dev` dependency too (`provides`), but the package names what it means |
| `test.environment` | `ruby3.4-bundler` | plus `apk-tools`, `binutils` | for `apk info --who-owns` and `readelf` |
| `test.pipeline` | Wolfi's five steps | plus "Ruby runs on challoc" | below; Wolfi's steps (`ver-check`, `help-check`, hello world, `bundle doctor --ssl`, `ldd-check`) stay |

The bootstrap `ruby-3.4` in the build environment (Wolfi builds Ruby with
Ruby) also needs `so:libjemalloc.so.2`; with `challoc-jemalloc-compat-dev`
in the environment apko resolves it to the compat library, so the
bootstrap interpreter runs on challoc during the build as well.

### What the package test proves

The prompt's `MALLOC_CONF=stats_print:true ruby -e 1 | grep challoc` does
not work under challoc: `stats_print` is parsed and inert (`src/config.rs`
`INERT_KEYS`; nothing is printed at exit), so the report is asked for
directly, from inside the running interpreter, through Fiddle (a default
gem in 3.4, built here because `libffi-dev` is in the environment):

| check | how |
|---|---|
| runs | `ruby -e 1`; `ruby -rjson -e 'puts JSON.generate(a: 1)'` = `{"a":1}` |
| linked the jemalloc way | `RbConfig::CONFIG["MAINLIBS"]` contains `-ljemalloc`; `readelf -d` on `ruby` and on `libruby.so.3.4.10` has `NEEDED [libjemalloc.so.2]` |
| the file is the compat package's | `ldd ruby` resolves `libjemalloc.so.2 => /usr/lib/libjemalloc.so.2`; `apk info --who-owns /usr/lib/libjemalloc.so.2` names `challoc-jemalloc-compat`; `libjemalloc2` is not installed |
| a running ruby maps exactly that file | `/proc/self/maps` filtered for `libjemalloc` = `/usr/lib/libjemalloc.so.2` |
| the process's allocator says challoc | Fiddle: `mallctl("version")` = `5.3.0-0-gchalloc`, `challoc_version()` = `0.1.0`, `Fiddle::Handle::DEFAULT["malloc"] == Fiddle::Handle.new("libjemalloc.so.2")["malloc"]` (and `free`), `malloc_stats_print(NULL, NULL, NULL)` writes `Version: "5.3.0-0-gchalloc"` |
| it allocates | 40 k live medium objects (strings of 1..4 KiB, 12-entry hashes: what reaches `malloc` from Ruby's heap), three garbage bursts with `GC.start`, three forced full GCs (bench/ruby/gc_churn.rb's shape, cut down); `GC.stat[:count] >= 6`, the live set intact, RSS printed |

### Building and testing locally

Same VM and container as "Building locally" above (melange with
bubblewrap in a privileged `wolfi-base` container, Wolfi's `pipelines/`
at `a944f08`), after `challoc.yaml` has been built into the local
repository (the Ruby build environment takes
`challoc-jemalloc-compat-dev` from it):

```sh
melange build /work/contrib/wolfi/ruby/ruby-3.4.yaml --arch $(uname -m) \
  --source-dir /work/contrib/wolfi/ruby/ruby-3.4 \
  --pipeline-dir /tmp/os/pipelines $WOLFI \
  --repository-append /out/packages --keyring-append /out/melange.rsa.pub \
  --signing-key /out/melange.rsa --out-dir /out/packages --runner bubblewrap
melange test /work/contrib/wolfi/ruby/ruby-3.4.yaml --arch $(uname -m) \
  --pipeline-dirs /tmp/os/pipelines $WOLFI \
  --repository-append /out/packages --keyring-append /out/melange.rsa.pub \
  --test-package-append=wolfi-base --runner bubblewrap
```

`--source-dir` is the patch directory (Wolfi's Makefile passes
`./ruby-3.4`); the Ruby checkout comes from GitHub. The full `make
test-all` is not part of the package test (Phase 1 ran it against the
same tag and the same library under this soname,
`bench/results/2026-09-17-ruby-test-all.md`); the results file has the
short subset run by hand against the packaged interpreter.

## KeyDB

`keydb/keydb.yaml` is wolfi-dev/os's `keydb.yaml` (main at `8c473de`,
2026-09-20; the file last changed in `276d78c`, a resources bump, `epoch:
8`) with the allocator switched from the bundled jemalloc to challoc
through `challoc-jemalloc-compat`. `keydb/keydb/cstdint.patch` is Wolfi's
own RocksDB patch, unchanged; `keydb/keydb/0001-Add-MALLOC-challoc.patch`
is new (below). `bench/results/2026-09-20-phase3-task3-keydb-package.md`
has the diff against Wolfi's file, the build and every assertion's output.

Why the compat library and not a private `quantum8` build like Valkey's:
KeyDB 6.3.4 is Redis 6.2's shape. Its `zmalloc.cpp` reads `epoch`,
`stats.{resident,active,allocated}`, writes `background_thread` and
`arena.<narenas>.purge`, and allocates with `mallocx`/`dallocx` under
`MALLOCX_TCACHE_NONE` for the defragmenter; its `defrag.cpp` has one
allocator input, `je_get_defrag_hint(ptr)`, the extension Redis adds to
its bundled jemalloc (and `zmalloc.h` compiles the defragmenter in,
`HAVE_DEFRAG`, only when the jemalloc header defines
`JEMALLOC_FRAG_HINT`). Nothing asserts the quantum or does bin-index
arithmetic (`grep quantum src/` is empty; Valkey 9's
`allocatorDefragInit` and `jeSize2BinIndexLgQ3` do not exist here), so
KeyDB runs on the system `quantum16` library like every other jemalloc
consumer. What it needed from challoc is the hint: `get_defrag_hint` /
`je_get_defrag_hint` are exported since this task (design 13.5, from the
data the utilization queries report, with the rule of Redis's
`iget_defrag_hint`), and both shipped headers define `JEMALLOC_FRAG_HINT`
and declare it.

KeyDB's Makefile has no system-jemalloc mode: `MALLOC=jemalloc` (the
Linux default) builds `deps/jemalloc` with `--with-lg-quantum=3` and links
the static library; `MALLOC=libc` drops every `USE_JEMALLOC` path. The
patch adds `MALLOC=challoc` the way `contrib/valkey`'s patch does for
Valkey:

| hunk | what |
|---|---|
| `src/Makefile` | `ifeq ($(MALLOC),challoc)`: `-DUSE_JEMALLOC -DUSE_CHALLOC`, `FINAL_LIBS := -Wl,--push-state,--no-as-needed -ljemalloc -Wl,--pop-state $(FINAL_LIBS)`; no `jemalloc` dependency target, so `deps/jemalloc` is never built. `<jemalloc/jemalloc.h>` comes from `/usr/include` (the -dev package), no `-I`. |
| `src/zmalloc.h` | under `USE_CHALLOC`, `ZMALLOC_LIB` is `"jemalloc-5.3.0-challoc"` (`JEMALLOC_VERSION_GID` appended), so `keydb-server --version` and `INFO memory`'s `mem_allocator` say which build; everything keyed on `jemalloc` (the test suite's memefficiency gate, `MEMORY MALLOC-STATS`) still matches. |

`--no-as-needed` is belt and braces: `keydb-server`, `keydb-cli` and
`keydb-benchmark` all link `zmalloc.o`, which calls `mallctl`, so
`--as-needed` would keep the entry anyway; the check step verifies rather
than assumes.

The changes to Wolfi's file, in file order:

| where | Wolfi | here | why |
|---|---|---|---|
| `epoch` | 8 | 9 | the allocator switch is a rebuild of the same version |
| `dependencies.runtime` | (derived) | `challoc-jemalloc-compat` | as Ruby's: melange derives `so:libjemalloc.so.2`, apk would resolve it to `libjemalloc2` by name preference |
| `vars` | - | `allocator-name: jemalloc-5.3.0-challoc` | what the build reports |
| `environment.contents.packages` | `jemalloc-dev` | `challoc-jemalloc-compat-dev`, plus `binutils` | the same file set, challoc underneath (`jemalloc-dev` was unused before: the bundled jemalloc was built); `readelf` for the check step |
| `pipeline` | `patch` (cstdint), sed, `make -j`, `make install` | plus `patch` (the challoc patch); the header checked for `JEMALLOC_CHALLOC`, `JEMALLOC_FRAG_HINT` and the hint prototype; `make MALLOC=challoc` on both invocations; a check step | `MALLOC` on the install too: the Makefile persists its settings and an install without it rebuilds with the bundled jemalloc (the Valkey lesson) |
| check step | - | `deps/jemalloc/lib` absent; `--version` says `malloc=jemalloc-5.3.0-challoc`; the three binaries have `NEEDED libjemalloc.so.2`; the environment's `libjemalloc.so.2` exports `challoc_version` and `je_get_defrag_hint`; the server imports `je_get_defrag_hint` (so `defrag.cpp` was compiled in) | |
| `keydb-cli`, `keydb-benchmark` `runtime` | (derived) | `challoc-jemalloc-compat` | linked with the same `FINAL_LIBS` |
| `test.environment` | `keydb-cli`, `wait-for-it` | plus `keydb-benchmark`, `apk-tools`, `binutils` | |
| `test.pipeline` | `ldd-check`, the SET/GET/EXISTS run | plus the two steps below; Wolfi's commands kept inside the second | |

### Package test

| check | how |
|---|---|
| the build says challoc | `keydb-server --version` has `malloc=jemalloc-5.3.0-challoc`; `keydb-sentinel --version` runs |
| linked against the compat library | `readelf -d` on the three binaries has `NEEDED [libjemalloc.so.2]`; `ldd` resolves it to `/usr/lib/libjemalloc.so.2`; `apk info --who-owns` names `challoc-jemalloc-compat`; `libjemalloc2` is not installed; the library exports `je_get_defrag_hint` |
| a running server maps exactly that file | `/proc/<pid>/maps` filtered for `libjemalloc` = `/usr/lib/libjemalloc.so.2` |
| the allocator reports itself | `INFO memory` has `mem_allocator:jemalloc-5.3.0-challoc` |
| active defrag is available | `CONFIG SET activedefrag yes` -> `OK`, `CONFIG GET` -> `yes` (without `HAVE_DEFRAG` the error is "Active defragmentation cannot be enabled: it requires a Redis server compiled with a modified Jemalloc ...") |
| purge, stats, the mallctl bridge | `MEMORY PURGE` -> `OK`; `MEMORY MALLOC-STATS` has `jemalloc statistics`; `DEBUG MALLCTL arenas.page` answers |
| Wolfi's own | `SET`/`GET`/`EXISTS` |
| traffic and a defrag-shaped churn | `keydb-benchmark -t set,get -n 20000`; 200 k random 150-byte `SET`s, 100 k random deletes through `EVAL`, a defrag cycle later `INFO stats`' `active_defrag_*` counters and `INFO memory`'s fragmentation figures are printed |
| clean shutdown | `SHUTDOWN NOSAVE`; the log has no `challoc:` message, assertion or crash report |

KeyDB's own `unit/memefficiency` (the defrag tests, gated on
`mem_allocator` matching `*jemalloc*` and `arenas.page <= 8192`) is not
part of the package test; the results file has its run against the
packaged binary from a `v6.3.4` checkout with `src/keydb-server` symlinked
to `/usr/bin/keydb-server`, the way lane P ran Valkey's (with `tclsh9.0
tests/test_helper.tcl --single unit/memefficiency --clients 1`: KeyDB's
`runtest` only knows Tcl 8.5/8.6, Wolfi's `tcl` is 9.0, and two Tcl 9
spellings, `package require Tcl 8.5-` and `-translation binary`, are
`sed`ed into the suite first). The honest result (2026-09-20, both
architectures): the five memory-efficiency tests pass; the defragmenter
runs and brings `allocator_frag_ratio` from 1.5-2.0 to 1.09-1.18 within
two seconds; the defrag tests then fail `defrag didn't stop.`, because
KeyDB's stop rule is Redis 6.2's, `stats.active - stats.allocated` over
the whole allocator against the test's 5% / 2 MB, and under challoc that
figure has a floor of 8-12% on these workloads (cached free slots pinning
their slabs until the 10 s cache GC returns them; 16-slot slabs of the
64-256 KiB classes pinned by one live client buffer each; slab tails)
that the hint cannot move. At KeyDB's defaults (10% / 100 MB) the cycle
stops at once. The results file attributes the floor per term and names
what would close the gap (a KeyDB-side `frag_smallbins` stop rule as
Redis 7's and Valkey's, a test-threshold hunk, or smaller large-class
slabs on challoc's side); the package is not shipped with defrag compiled
out.

### Building and testing locally

Same VM and container as "Building locally" above (melange with
bubblewrap in a privileged `wolfi-base` container, Wolfi's `pipelines/`
at `8c473de`), after `challoc.yaml` has been built into the local
repository (the build environment takes `challoc-jemalloc-compat-dev`
from it):

```sh
melange build /work/contrib/wolfi/keydb/keydb.yaml --arch $(uname -m) \
  --source-dir /work/contrib/wolfi/keydb/keydb \
  --pipeline-dir /tmp/os/pipelines $WOLFI \
  --repository-append /out/packages --keyring-append /out/melange.rsa.pub \
  --signing-key /out/melange.rsa --out-dir /out/packages --runner bubblewrap
melange test /work/contrib/wolfi/keydb/keydb.yaml --arch $(uname -m) \
  --pipeline-dirs /tmp/os/pipelines $WOLFI \
  --repository-append /out/packages --keyring-append /out/melange.rsa.pub \
  --test-package-append=wolfi-base --runner bubblewrap
```

`--source-dir` is the patch directory (Wolfi's Makefile passes
`./keydb`); the KeyDB checkout, submodules included (RocksDB, unused
without `ENABLE_FLASH`), comes from GitHub.

## RethinkDB

`rethinkdb/rethinkdb.yaml` is wolfi-dev/os's `rethinkdb.yaml` (main at
`8c473de`, 2026-09-20; the file last changed in `d3dc8df`, the glibc 2.44
transition, `epoch: 14`) with the allocator switched from Wolfi's jemalloc
to challoc through `challoc-jemalloc-compat`.
`bench/results/2026-09-20-phase3-task3-rethinkdb-package.md` has the diff
against Wolfi's file, the build and every assertion's output.

Ruby's shape, with one twist. RethinkDB's `configure` defaults to
jemalloc on Linux (`--with-jemalloc`), probes `<jemalloc/jemalloc.h>` for
`JEMALLOC_VERSION_MAJOR >= 4` and links the library; nothing in the
server calls jemalloc's API, it is the process's `malloc`. The twist:
`default_static="tcmalloc jemalloc"`, so configure looks for
`libjemalloc.a` (`check_static_lib`) and links it in, which is how Wolfi's
`rethinkdb` carries jemalloc 5.3.1 today, with no `DT_NEEDED` for it.
The compat package ships no static library (the Phase 1 task 6 decision
holds for the system packages), so `--dynamic jemalloc` turns the probe
into `check_dyn_lib` (`-ljemalloc` against the compat header) and the
binary records `libjemalloc.so.2`, which is what the swap needs anyway:
a static link would have frozen one allocator build into the package.

| where | Wolfi | here | why |
|---|---|---|---|
| `epoch` | 14 | 15 | the allocator switch is a rebuild of the same version |
| `dependencies.runtime` | (none; jemalloc was static) | `challoc-jemalloc-compat` | the binary now depends on `so:libjemalloc.so.2`; named explicitly for the same reason as Ruby's |
| `environment.contents.packages` | `jemalloc-dev` | `challoc-jemalloc-compat-dev`, plus `binutils` | the same file set, challoc underneath; `readelf` for the check step |
| configure | `--prefix --sysconfdir --fetch protoc` | plus `--with-jemalloc --dynamic jemalloc`; `config.mk` checked for `ALLOCATOR` = jemalloc and `STATIC_MALLOC` off; the header for `JEMALLOC_CHALLOC` | see above |
| after `autoconf/make` | - | a check step | the built `rethinkdb` has `NEEDED libjemalloc.so.2` and no statically linked jemalloc (`nm` has no `arena_init`); the environment's library exports `challoc_version`; `--version` runs |
| `test.environment` | (none) | `apk-tools`, `binutils` | |
| `test.pipeline` | `ldd-check`, `--version`/`--help`, `test/daemon-check-output` | plus "RethinkDB runs on challoc" | below; Wolfi's steps stay |

### Package test

| check | how |
|---|---|
| Wolfi's | `ldd-check`; `--version`, `--help`; `daemon-check-output` (`Loading data from directory`, `Listening for intracluster connections`, `Listening for client driver connections`) |
| linked against the compat library | `readelf -d /usr/bin/rethinkdb` has `NEEDED [libjemalloc.so.2]`; `ldd` resolves it to `/usr/lib/libjemalloc.so.2`; `apk info --who-owns` names `challoc-jemalloc-compat`; `libjemalloc2` is not installed; the library exports `challoc_version` |
| a running server maps exactly that file | started with `--daemon --pid-file --log-file`; `/proc/<pid>/maps` filtered for `libjemalloc` = `/usr/lib/libjemalloc.so.2` |
| clean shutdown | `SIGTERM`; the process is gone within 30 s; the log says it shut down and has no `challoc:` message or crash |

RethinkDB has no allocator statistics command (no `MEMORY`-style
introspection; the `rethinkdb.stats` system table has no allocator
figures), so the package test proves the library by the loader's maps
and the package database, as Ruby's does. The table create / insert /
read smoke test needs a client driver, which Wolfi does not package; the
results file has it by hand (the PyPI `rethinkdb` driver in the clean
container).

### Building and testing locally

Same as KeyDB's recipe with `contrib/wolfi/rethinkdb/rethinkdb.yaml` and
`--source-dir /work/contrib/wolfi/rethinkdb` (no patches; any directory).
The RethinkDB checkout comes from GitHub and configure fetches and builds
its own `protoc` (`--fetch protoc`, Wolfi's choice); the build is the long
one of the three (the results file has the timing).
