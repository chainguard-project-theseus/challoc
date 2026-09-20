# contrib/wolfi: the Wolfi package definition

`challoc.yaml` is the melange definition for the three Wolfi packages of
docs/plan.md Phase 3 task 1 (design 13.6). Phase 3 runs in the
wolfi-dev/os repository; this directory tracks the draft and the recipe
that builds and tests it here. Results of the first real build (both
architectures, 2026-09-20):
`bench/results/2026-09-20-phase3-task1-wolfi-package.md`.

## Packages

| package | files | notes |
|---|---|---|
| `challoc` | `/usr/lib/libchalloc.so.0` | the runtime: `quantum16`, `DT_SONAME libchalloc.so.0`; `provides so:libchalloc.so.0` (auto). Nothing else: the link name is the -dev package's, as Wolfi's `split/dev` does for every library. |
| `challoc-dev` | `/usr/lib/libchalloc.so -> libchalloc.so.0`, `/usr/include/challoc/jemalloc.h`, `/usr/lib/pkgconfig/challoc.pc` | `runtime: challoc=<version>`. The header is `contrib/valkey/include/challoc/jemalloc.h` (the `je_` API under `<challoc/...>`, never `<jemalloc/...>`). `challoc.pc` is written by the pipeline: `Version` is the crate version, `Libs: -L${libdir} -Wl,--push-state,--no-as-needed -lchalloc -Wl,--pop-state` (see "as-needed" below). |
| `challoc-jemalloc-compat` | `/usr/lib/libjemalloc.so.2`, `/usr/lib/libjemalloc.so -> libjemalloc.so.2`, `/usr/lib/pkgconfig/jemalloc.pc` | `quantum16` built with the `jemalloc-soname` feature and staged by `scripts/jemalloc-compat.sh` (`QUANTUM=16 PREFIX=/usr`), so the file set and the pc file (`Version: 5.3.0`, upstream's `jemalloc.pc.in` fields) are exactly what `make jemalloc-compat` produces. `provides: libjemalloc2=<version>` plus the automatic `so:libjemalloc.so.2=2` and `pc:jemalloc`. No header (a consumer that includes `<jemalloc/jemalloc.h>` still needs `jemalloc-dev`; see "Swapping" below). |

Valkey's private `quantum8` build is Phase 3 task 2 and belongs in
`valkey.yaml` (`/usr/lib/valkey/`, design 13.6: two libraries with one
soname cannot share `/usr/lib`, so the system library is never `quantum8`).

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
