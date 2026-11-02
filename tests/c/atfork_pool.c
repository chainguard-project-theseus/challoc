/*
 * A prepare handler that waits on other threads which allocate: a thread
 * pool's shutdown before fork (OpenBLAS's blas_thread_shutdown, gRPC).
 * The pool registers its handlers from the program's first constructor;
 * its prepare handler stops the workers and joins them, and each worker's
 * exit returns its thread cache to the allocator. challoc registers its
 * own handlers from its constructor, which runs before the program's
 * (preloaded, or linked), so glibc runs the pool's prepare first, before
 * challoc's takes the allocator's locks, and the joins return. The parent
 * and child handlers start the pool again. A watcher fails the run if a
 * fork does not return (fork blocks every signal, so a deadlock cannot
 * time itself out).
 */
#include "common.h"

#define WORKERS 4
#define FORKS 20

static pthread_t workers[WORKERS];
static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static int stopping, running;

static void *worker(void *arg)
{
    (void)arg;
    for (;;) {
        void *p[16];
        for (int i = 0; i < 16; i++)
            p[i] = malloc(16 + (size_t)i * 200);
        for (int i = 0; i < 16; i++)
            free(p[i]);
        pthread_mutex_lock(&mu);
        if (stopping) {
            pthread_mutex_unlock(&mu);
            return NULL;
        }
        struct timespec t;
        clock_gettime(CLOCK_REALTIME, &t);
        t.tv_nsec += 100 * 1000;
        if (t.tv_nsec >= 1000000000) {
            t.tv_sec++;
            t.tv_nsec -= 1000000000;
        }
        pthread_cond_timedwait(&cv, &mu, &t);
        pthread_mutex_unlock(&mu);
    }
}

static void pool_start(void)
{
    stopping = 0;
    for (int i = 0; i < WORKERS; i++)
        if (pthread_create(&workers[i], NULL, worker, NULL) != 0)
            _exit(4);
    running = 1;
}

static void pool_stop(void)
{
    if (!running)
        return;
    pthread_mutex_lock(&mu);
    stopping = 1;
    pthread_cond_broadcast(&cv);
    pthread_mutex_unlock(&mu);
    for (int i = 0; i < WORKERS; i++)
        pthread_join(workers[i], NULL);
    running = 0;
}

static void prepare(void)
{
    pool_stop();
}

static void parent(void)
{
    pool_start();
}

static void child(void)
{
    /* The child's own pool. */
    pool_start();
}

__attribute__((constructor(101))) static void register_first(void)
{
    if (pthread_atfork(prepare, parent, child) != 0)
        _exit(3);
}

static volatile int forks_done;

static void *forker(void *arg)
{
    (void)arg;
    for (int i = 0; i < FORKS; i++) {
        pid_t pid = fork();
        if (pid < 0)
            _exit(5);
        if (pid == 0) {
            free(malloc(100));
            pool_stop();
            _exit(0);
        }
        int st = 0;
        while (waitpid(pid, &st, 0) != pid)
            if (errno != EINTR)
                _exit(6);
        if (!WIFEXITED(st) || WEXITSTATUS(st) != 0)
            _exit(7);
        forks_done = i + 1;
    }
    return NULL;
}

int main(void)
{
    require_challoc();
    pool_start();
    pthread_t f;
    REQUIRE(pthread_create(&f, NULL, forker, NULL) == 0, "pthread_create");
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 30;
    int rc = pthread_timedjoin_np(f, NULL, &deadline);
    if (rc != 0) {
        fprintf(stderr, "fork did not return (%d of %d forks done): the pool's prepare handler deadlocked\n",
                forks_done, FORKS);
        _exit(1);
    }
    pool_stop();
    printf("ok atfork_pool\n");
    return 0;
}
