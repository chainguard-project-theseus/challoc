# Consumers

Real programs under challoc. The `consumers` check
(`.chainguard/ci/consumers.yaml`) runs each case on x86_64 and aarch64,
on request: the label `ci/run:consumers` on a pull request, or
`chainctl checks run . --check consumers` for a pushed commit
(`ci/README.md`); `consumers-weekly` runs the same instances on main
weekly. Each case also runs locally in the dev container through its make target.

| Case | How it meets challoc | Run it | Script |
|---|---|---|---|
| Valkey | built with `MALLOC=challoc` at Wolfi's tag with `contrib/valkey/` patches; its full suite and `unit/memefficiency` | `make valkey STAGE=all` | `valkey-build.sh` |
| Ruby 3.4 | built `--with-jemalloc` against the jemalloc-compat library; `make test-all` in three shards | `make ruby STAGE=all` | `ruby-build.sh` |
| CPython | Wolfi's `python3`, unmodified, with `libchalloc.so` preloaded and `PYTHONMALLOC=malloc`; a fixed subset of its own `Lib/test` | `make preload CASE=cpython` | `preload.sh` |
| git | Wolfi's `/usr/bin/git`, unmodified, with `libchalloc.so` preloaded; a fixed subset of its own `t/` (object store, packs, diff) | `make preload CASE=git` | `preload.sh` |

Redis (`make redis`, `redis-build.sh`) builds the same way as Valkey and is
not in the check.

## The preload cases

`preload.sh` is how most of Wolfi would meet challoc: `LD_PRELOAD` in front
of a program built against glibc's malloc. For each case it:

1. builds challoc (`QUANTUM`, default 16);
2. proves the preload takes: under `LD_DEBUG=bindings` the program's
   `malloc` binds to that `libchalloc.so`, or the case fails;
3. fetches the test sources of the exact version Wolfi installed, never
   vendored: CPython's `Lib/test` from python.org's tarball of
   `python3`'s version, git's `t/` from kernel.org's tarball of
   `git --version`, SHA-256 checked against that directory's
   `sha256sums.asc` (git's tree is built only for `t/`'s helpers). That
   checksum comes from the same host, so it guards against a truncated
   or corrupted download, not a substituted one; CPython's tarball has
   no check. Both trust the TLS connection to their host, as the Checks
   guest's egress proxy relays it, the same trust the Valkey and Ruby
   clones from github.com get;
4. runs the subset twice, without and with the preload, and fails on a
   test that fails only with challoc. A test that fails in both runs is
   listed, not failed: it says nothing about the allocator. A module or
   script in which no test passed, in either run, fails the case, since
   the harness ran nothing there; so does one with fewer passing tests
   under challoc than without it (a test that turned into a skip, or a
   failure the names missed). The PASS line gives each one's counts.

git's `t/test-lib.sh` swaps `LD_PRELOAD` for glibc's malloc-check
library around every test body and unsets it after (`setup_malloc_check`
and `teardown_malloc_check`). Both runs set `TEST_NO_MALLOC_CHECK=1`,
which turns those hooks off, and the challoc run proves the preload
inside the harness: under `LD_DEBUG=bindings`, the git processes of one
script (`GIT_PROBE`, default `t4001-diff-rename`), whether a body started
them as a bare `git` through `PATH` or by absolute path, must all bind
`malloc` to `libchalloc.so`, none elsewhere.

The subsets are `CPYTHON_TESTS` (`test_list test_dict test_set test_bytes
test_str test_gc test_threading test_mmap`; `test_str` is 3.13's name for
`test_unicode`) and `GIT_TESTS` (`t0000-basic t1006-cat-file t1050-large
t4000-diff-format t4001-diff-rename t5300-pack-object t5302-pack-index`).
Set either to run others (with `GIT_PROBE` naming one of `GIT_TESTS`;
`make preload` passes all three into the container). Logs go to `target/<case>/logs/`: each run's log
per module or script, and `failures-baseline.txt` and
`failures-challoc.txt`.

## Expected failures

None. A test that fails only with challoc fails its case. Fix it; if that
must wait, open an issue and list the test here with it in the same change
that takes the test out of the default subset.
