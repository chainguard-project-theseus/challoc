# contrib/wolfi: the Wolfi package definitions

`challoc.yaml` is the melange definition for the three Wolfi packages of
docs/plan.md Phase 3 task 1 (design 13.6); `valkey/valkey-9.1.yaml` is
Wolfi's Valkey 9.1 package switched to challoc (task 2, the "Valkey"
section below). Phase 3 runs in the wolfi-dev/os repository; this
directory tracks the drafts and the recipes that build and test them
here. Results of the first real builds (both architectures, 2026-09-20):
`bench/results/2026-09-20-phase3-task1-wolfi-package.md` and
`bench/results/2026-09-20-phase3-task2-valkey-package.md`.

## Packages

| package | files | notes |
|---|---|---|
| `challoc` | `/usr/lib/libchalloc.so.0` | the runtime: `quantum16`, `DT_SONAME libchalloc.so.0`; `provides so:libchalloc.so.0` (auto). Nothing else: the link name is the -dev package's, as Wolfi's `split/dev` does for every library. |
| `challoc-dev` | `/usr/lib/libchalloc.so -> libchalloc.so.0`, `/usr/include/challoc/jemalloc.h`, `/usr/lib/pkgconfig/challoc.pc` | `runtime: challoc=<version>`. The header is `contrib/valkey/include/challoc/jemalloc.h` (the `je_` API under `<challoc/...>`, never `<jemalloc/...>`). `challoc.pc` is written by the pipeline: `Version` is the crate version, `Libs: -L${libdir} -Wl,--push-state,--no-as-needed -lchalloc -Wl,--pop-state` (see "as-needed" below). |
| `challoc-jemalloc-compat` | `/usr/lib/libjemalloc.so.2`, `/usr/lib/libjemalloc.so -> libjemalloc.so.2`, `/usr/lib/pkgconfig/jemalloc.pc` | `quantum16` built with the `jemalloc-soname` feature and staged by `scripts/jemalloc-compat.sh` (`QUANTUM=16 PREFIX=/usr`), so the file set and the pc file (`Version: 5.3.0`, upstream's `jemalloc.pc.in` fields) are exactly what `make jemalloc-compat` produces. `provides: libjemalloc2=<version>` plus the automatic `so:libjemalloc.so.2=2` and `pc:jemalloc`. No header (a consumer that includes `<jemalloc/jemalloc.h>` still needs `jemalloc-dev`; see "Swapping" below). |

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
- Open point for task 3: a consumer that includes `<jemalloc/jemalloc.h>`
  (Ruby's `--with-jemalloc`) builds against `jemalloc-dev`, which
  conflicts with the compat package's `libjemalloc.so` and `jemalloc.pc`.
  Either the compat package grows a `/usr/include/jemalloc/jemalloc.h`
  (a header declaring the unprefixed API; `contrib/valkey/include/challoc/jemalloc.h`
  declares the `je_` one) or the consumer's build environment lists
  `jemalloc-dev` for the header and links against the compat library from
  a `--repository-append` of its own; the PROGRESS.d note records it.

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
