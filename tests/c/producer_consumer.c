/*
 * Producer/consumer: producers allocate blocks, fill them and push them
 * through a bounded queue; consumers pop, verify and free. Every block is
 * therefore freed by a thread other than the one that allocated it, which
 * is the pure cross-thread free path (the consumer's cache fills with slots
 * from slabs it never allocated from, then flushes them). Two shapes: one
 * shared queue with 4 producers and 4 consumers, and 4 pipelines where each
 * consumer feeds another consumer after realloc.
 */
#include "common.h"

#define QUEUE 256
#define PER_PRODUCER 60000

struct item {
    unsigned char *p;
    size_t n;
    uint32_t seed;
};

struct queue {
    pthread_mutex_t lock;
    pthread_cond_t not_empty, not_full;
    struct item items[QUEUE];
    int head, count;
    int producers_left;
};

static void queue_init(struct queue *q, int producers)
{
    pthread_mutex_init(&q->lock, NULL);
    pthread_cond_init(&q->not_empty, NULL);
    pthread_cond_init(&q->not_full, NULL);
    q->head = q->count = 0;
    q->producers_left = producers;
}

static void queue_push(struct queue *q, struct item it)
{
    pthread_mutex_lock(&q->lock);
    while (q->count == QUEUE)
        pthread_cond_wait(&q->not_full, &q->lock);
    q->items[(q->head + q->count) % QUEUE] = it;
    q->count++;
    pthread_cond_signal(&q->not_empty);
    pthread_mutex_unlock(&q->lock);
}

/* Returns 0 when the queue is drained and every producer has finished. */
static int queue_pop(struct queue *q, struct item *out)
{
    pthread_mutex_lock(&q->lock);
    while (q->count == 0 && q->producers_left > 0)
        pthread_cond_wait(&q->not_empty, &q->lock);
    if (q->count == 0) {
        pthread_mutex_unlock(&q->lock);
        return 0;
    }
    *out = q->items[q->head];
    q->head = (q->head + 1) % QUEUE;
    q->count--;
    pthread_cond_signal(&q->not_full);
    pthread_mutex_unlock(&q->lock);
    return 1;
}

static void queue_producer_done(struct queue *q)
{
    pthread_mutex_lock(&q->lock);
    q->producers_left--;
    pthread_cond_broadcast(&q->not_empty);
    pthread_mutex_unlock(&q->lock);
}

static size_t pick_size(uint64_t *rng)
{
    uint64_t r = rng_next(rng);
    switch (r % 16) {
    case 0: return 256 * 1024 + (size_t)((r >> 8) % 200000); /* huge */
    case 1: case 2: return (size_t)((r >> 8) % 20000) + 1;    /* medium-ish */
    default: return (size_t)((r >> 8) % 512) + 1;             /* small */
    }
}

struct producer_args {
    struct queue *q;
    int id;
    long produced;
};

static void *producer(void *arg)
{
    struct producer_args *a = arg;
    uint64_t rng = 0xABCDEFu + (uint64_t)a->id * 7919;
    for (int i = 0; i < PER_PRODUCER; i++) {
        struct item it;
        it.n = pick_size(&rng);
        it.seed = (uint32_t)rng_next(&rng);
        it.p = malloc(it.n);
        REQUIRE(it.p, "producer %d: malloc(%zu)", a->id, it.n);
        fill(it.p, it.n, it.seed);
        queue_push(a->q, it);
        a->produced++;
    }
    queue_producer_done(a->q);
    return NULL;
}

struct consumer_args {
    struct queue *in;
    struct queue *out; /* NULL: free; otherwise realloc and forward */
    int id;
    long consumed;
};

static void *consumer(void *arg)
{
    struct consumer_args *a = arg;
    struct item it;
    uint64_t rng = 0x1234u + (uint64_t)a->id;
    while (queue_pop(a->in, &it)) {
        CHECK(check_fill(it.p, it.n, it.seed), "consumer %d: block %p (%zu bytes) corrupted", a->id, (void *)it.p,
              it.n);
        CHECK(malloc_usable_size(it.p) >= it.n, "consumer %d: usable", a->id);
        a->consumed++;
        if (a->out) {
            size_t n = pick_size(&rng);
            unsigned char *p = realloc(it.p, n);
            REQUIRE(p, "consumer %d: realloc(%zu)", a->id, n);
            CHECK(check_fill(p, it.n < n ? it.n : n, it.seed), "consumer %d: realloc lost content", a->id);
            it.p = p;
            it.n = n;
            it.seed = (uint32_t)rng_next(&rng);
            fill(p, n, it.seed);
            queue_push(a->out, it);
        } else {
            free(it.p);
        }
    }
    if (a->out)
        queue_producer_done(a->out);
    return NULL;
}

static void shared_queue(void)
{
    enum { P = 4, C = 4 };
    struct queue q;
    queue_init(&q, P);
    struct producer_args pa[P];
    struct consumer_args ca[C];
    pthread_t pt[P], ct[C];
    for (int i = 0; i < C; i++) {
        ca[i] = (struct consumer_args){&q, NULL, i, 0};
        REQUIRE(pthread_create(&ct[i], NULL, consumer, &ca[i]) == 0, "consumer create");
    }
    for (int i = 0; i < P; i++) {
        pa[i] = (struct producer_args){&q, i, 0};
        REQUIRE(pthread_create(&pt[i], NULL, producer, &pa[i]) == 0, "producer create");
    }
    long produced = 0, consumed = 0;
    for (int i = 0; i < P; i++) {
        pthread_join(pt[i], NULL);
        produced += pa[i].produced;
    }
    for (int i = 0; i < C; i++) {
        pthread_join(ct[i], NULL);
        consumed += ca[i].consumed;
    }
    CHECK(produced == consumed && produced == (long)P * PER_PRODUCER, "shared queue: produced %ld consumed %ld",
          produced, consumed);
}

/* producer -> stage 1 (realloc, forward) -> stage 2 (realloc, forward) -> sink (free) */
static void pipelines(void)
{
    enum { N = 4, STAGES = 3 };
    struct queue q[N][STAGES];
    struct producer_args pa[N];
    struct consumer_args ca[N][STAGES];
    pthread_t pt[N], ct[N][STAGES];
    for (int i = 0; i < N; i++) {
        for (int s = 0; s < STAGES; s++)
            queue_init(&q[i][s], 1);
        for (int s = 0; s < STAGES; s++) {
            ca[i][s] = (struct consumer_args){&q[i][s], s + 1 < STAGES ? &q[i][s + 1] : NULL, i * STAGES + s, 0};
            REQUIRE(pthread_create(&ct[i][s], NULL, consumer, &ca[i][s]) == 0, "stage create");
        }
        pa[i] = (struct producer_args){&q[i][0], 100 + i, 0};
        REQUIRE(pthread_create(&pt[i], NULL, producer, &pa[i]) == 0, "producer create");
    }
    for (int i = 0; i < N; i++) {
        pthread_join(pt[i], NULL);
        for (int s = 0; s < STAGES; s++) {
            pthread_join(ct[i][s], NULL);
            CHECK(ca[i][s].consumed == PER_PRODUCER, "pipeline %d stage %d consumed %ld", i, s, ca[i][s].consumed);
        }
    }
}

int main(void)
{
    require_challoc();
    shared_queue();
    pipelines();
    return finish("producer_consumer");
}
