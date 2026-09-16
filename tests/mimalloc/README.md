# mimalloc stress test

`test-stress.c` is copied unmodified from mimalloc, tag `v3.5.2` (commit
`4e3a61669be12515da74a0208d4f58bd920931ab`), file `test/test-stress.c`. It
is MIT licensed; the licence text is in `LICENSE`. The test allocates with a
power-of-two size distribution plus 1% large objects, fills every block and
verifies it on free, transfers pointers between threads through a shared
buffer, and destroys and recreates all threads every iteration with half the
transferred objects surviving.

Everything else here is challoc's:

- `stress.c`: the program that is actually built. It defines
  `USE_STD_MALLOC` so the test allocates through `calloc`/`realloc`/`free`,
  `#include`s `test-stress.c` with its `main` renamed, and wraps it with
  `require_challoc()` and the `ok stress` line the Rust driver expects.
- `include/mimalloc.h`, `include/mimalloc-stats.h`: `test-stress.c` includes
  mimalloc's headers unconditionally; under `USE_STD_MALLOC` the only thing
  it needs from them is the `mi_subproc_id_t` type.

`src/api/ctests.rs` compiles `stress.c` with the common flags plus
`-Wno-unused-parameter -Itests/mimalloc/include` and runs it bare (must
fail) and under `LD_PRELOAD=libchalloc.so` with `8 50 10`
(`THREADS=8 SCALE=50 ITER=10`, plan Phase 0 task 16) as part of `make test`.
