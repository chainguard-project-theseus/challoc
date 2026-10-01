# glibc malloc tests

`malloc/` holds five test programs copied unmodified from the GNU C Library,
tag `glibc-2.44` (commit `c3a3a9808ad3ab4a3336836833f83288b672ccbf`), files
`malloc/tst-malloc.c`, `malloc/tst-calloc.c`, `malloc/tst-mallocfork.c`,
`malloc/tst-posix_memalign.c`, `malloc/tst-realloc.c` and the helper
`malloc/tst-malloc-aux.h`. They are licensed under the GNU Lesser General
Public License version 2.1 or later; the licence text is in `COPYING.LIB`.
`tst-mallocfork.c` carries no header of its own and is covered by glibc's
top-level licence like the rest of the malloc directory.

Everything else here is challoc's and replaces the parts of glibc's build
tree the tests reach for:

- `test-skeleton.c`: what `#include "../test-skeleton.c"` at the end of each
  test resolves to. Supplies `main()`, refuses to run unless challoc is
  preloaded, runs `do_test`, prints `ok <name>`.
- `include/libc-diag.h`, `include/support/check.h`,
  `include/support/test-pointer.h`: the few macros and one inline function
  the tests use from glibc's internal headers.

`src/api/c_programs.rs` (the `glibc!` rows) compiles each program through
`scripts/ctest-cc.sh` (the shared flag line, `-DQUANTUM=<n>` read from the
built library) plus `-D_GNU_SOURCE -Wno-unused-parameter
-DTEST_NAME="<name>" -Itests/glibc/include`, and runs it bare (must fail)
and under `LD_PRELOAD=libchalloc.so` (must pass), as part of `make test`.
To update the vendored copy, replace the five files
and `tst-malloc-aux.h` from the new tag and change the tag and commit above;
nothing else here depends on the glibc version.
