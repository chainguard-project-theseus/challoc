/*
 * A shared library whose constructor registers an atfork prepare handler
 * that allocates in every tier, for `tests/c/atfork_early.c -DLINKED_LIB`
 * (the `api::` test `atfork_handlers_of_linked_libraries_may_allocate`).
 * A library the program links runs its constructors before challoc's is
 * initialised whether challoc is preloaded or linked after it, so its
 * handler is registered first and runs on the forking thread while
 * challoc's hold every allocator lock.
 */
#include <pthread.h>
#include <stdlib.h>
#include <string.h>

int atfork_lib_prepared, atfork_lib_parent, atfork_lib_child;

static void prepare(void)
{
    static const size_t sizes[] = {24, 3000, 40000, 300000, 3 << 20};
    void *p[sizeof sizes / sizeof sizes[0]];
    for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++) {
        p[i] = malloc(sizes[i]);
        if (p[i])
            memset(p[i], 0x5a, 16);
    }
    for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++)
        free(p[i]);
    atfork_lib_prepared++;
}

static void parent(void)
{
    free(malloc(5000));
    atfork_lib_parent++;
}

static void child(void)
{
    free(malloc(5000));
    atfork_lib_child++;
}

__attribute__((constructor)) static void register_handlers(void)
{
    if (pthread_atfork(prepare, parent, child) != 0)
        abort();
}
