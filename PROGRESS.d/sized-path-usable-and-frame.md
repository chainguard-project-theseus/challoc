## Done

- Sized path, size-returning API for Valkey (lane sized-path, 2026-09-21): in progress. `challoc_malloc_usable`, `challoc_calloc_usable`, `challoc_realloc_usable`, `challoc_free_usable` (design 13.7, `<challoc/challoc.h>`), the Valkey patch's `zmalloc.c` on them, the `valkey1-write-zmalloc` / `valkey1-write-pair` rows, the soak's `challoc-old` server.

- Sized path, the frame of the sized path (lane sized-path, 2026-09-21): not started.

## Design decisions

## Notes for the human
