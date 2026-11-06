/* A library that allocates in its constructor, destructor and a thread it
 * starts, for `lifecycle dlopen`. Its `__thread` block is dynamic TLS
 * (general-dynamic model), so each dlopen grows the DTV of the threads
 * that touch it. */
#include <pthread.h>
#include <stdlib.h>
#include <string.h>

static void *kept;
static __thread char tls_buf[4096];
static __thread void *tls_block;

__attribute__((constructor)) static void tlslib_init(void)
{
    kept = malloc(4096);
    memset(kept, 1, 4096);
}

__attribute__((destructor)) static void tlslib_fini(void)
{
    free(kept);
    free(malloc(77));
}

static void *worker(void *arg)
{
    size_t n = (size_t)(unsigned long)arg % 20000 + 1;
    tls_buf[n % sizeof tls_buf] = 1;
    tls_block = malloc(n);
    memset(tls_block, 2, n);
    void *q = realloc(tls_block, n * 2);
    free(q);
    return NULL;
}

void tlslib_run(int i)
{
    pthread_t t;
    pthread_create(&t, NULL, worker, (void *)(unsigned long)i);
    pthread_join(t, NULL);
    tls_buf[i % sizeof tls_buf] = 3; /* the calling thread's dynamic TLS too */
    free(malloc((size_t)i + 1));
}
