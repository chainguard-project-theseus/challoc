/*
 * The fork child's reset of the background thread's state (design 8.1,
 * `purge::background::fork_child`): the child has no background thread,
 * and turning `background_thread` on there starts exactly one. A reset
 * that left the state saying a thread is being created would have every
 * later start refuse, and the child would never get one.
 */
#include "common.h"
#include <dirent.h>

static int bg_threads(void)
{
    DIR *d = opendir("/proc/self/task");
    REQUIRE(d, "opendir /proc/self/task");
    struct dirent *e;
    int n = 0;
    char path[300], name[64];
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.')
            continue;
        snprintf(path, sizeof path, "/proc/self/task/%s/comm", e->d_name);
        FILE *f = fopen(path, "r");
        if (f && fgets(name, sizeof name, f) && strncmp(name, "challoc-bg", 10) == 0)
            n++;
        if (f)
            fclose(f);
    }
    closedir(d);
    return n;
}

/* bg_threads() once the count is exactly one, or after 2 s: a new thread
 * names itself `challoc-bg` when it starts running, a moment after the
 * start returned. */
static int bg_threads_one(void)
{
    int n = bg_threads();
    for (int i = 0; n != 1 && i < 2000; i++) {
        usleep(1000);
        n = bg_threads();
    }
    return n;
}

static int in_child(void)
{
    int n = bg_threads();
    if (n != 0) {
        fprintf(stderr, "child: %d challoc-bg threads before asking, want 0\n", n);
        return 4;
    }
    bool on = true;
    if (mallctl("background_thread", NULL, NULL, &on, sizeof on) != 0) {
        fprintf(stderr, "child: background_thread on failed\n");
        return 5;
    }
    bool st = false;
    size_t len = sizeof st;
    if (mallctl("background_thread", &st, &len, NULL, 0) != 0 || !st) {
        fprintf(stderr, "child: background_thread reads off after turning it on\n");
        return 6;
    }
    n = bg_threads_one();
    if (n != 1) {
        fprintf(stderr, "child: %d challoc-bg threads after turning it on, want 1\n", n);
        return 7;
    }
    bool off = false;
    if (mallctl("background_thread", NULL, NULL, &off, sizeof off) != 0)
        return 8;
    return 0;
}

int main(void)
{
    require_challoc();
    bool on = true;
    REQUIRE(mallctl("background_thread", NULL, NULL, &on, sizeof on) == 0, "background_thread on");
    CHECK(bg_threads_one() == 1, "parent: no challoc-bg thread after turning it on");
    for (int round = 0; round < 3; round++) {
        fflush(stdout);
        pid_t pid = fork();
        REQUIRE(pid >= 0, "fork: %s", strerror(errno));
        if (pid == 0)
            _exit(in_child());
        int status = 0;
        while (waitpid(pid, &status, 0) != pid)
            REQUIRE(errno == EINTR, "waitpid: %s", strerror(errno));
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "round %d: child status %#x", round, status);
    }
    bool off = false;
    REQUIRE(mallctl("background_thread", NULL, NULL, &off, sizeof off) == 0, "background_thread off");
    return finish("fork_bg_start");
}
