/* challoc stand-in for mimalloc's <mimalloc.h>. test-stress.c includes it
 * unconditionally; with USE_STD_MALLOC the only thing it still needs from
 * it is the mi_subproc_id_t type, which the test threads through
 * run_os_threads() and compares against NULL in thread_entry(). Nothing
 * else from mimalloc's API is declared, so any accidental use of an mi_*
 * function fails to compile instead of silently linking against nothing. */
#ifndef CHALLOC_MIMALLOC_H
#define CHALLOC_MIMALLOC_H

#ifndef USE_STD_MALLOC
#error "challoc only builds mimalloc's tests with USE_STD_MALLOC"
#endif

typedef struct mi_subproc_id_s {
    void *_mi_subproc_id;
} mi_subproc_id_t;

#endif
