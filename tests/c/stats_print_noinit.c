/*
 * stats_print_noinit: a program that never allocates, run under the
 * preload with MALLOC_CONF=stats_print:true. challoc is never
 * initialised, so its at-exit report (`print::stats_print_at_exit`) must
 * print nothing and must not initialise the allocator during exit. The
 * driver (src/api/c_programs.rs, c_conf_stats_print_reports_at_exit)
 * checks that stderr has no report.
 *
 * Nothing here may allocate: no stdio (its buffers come from malloc), no
 * dlsym. The preload is detected through a weak reference to
 * `challoc_version`, which the dynamic linker resolves at load; bare, it
 * is null and the program exits 1, as every C test does without the
 * preload.
 */
#include <string.h>
#include <unistd.h>

extern const char *challoc_version(void) __attribute__((weak));

static void say(int fd, const char *s)
{
    size_t n = strlen(s);
    while (n > 0) {
        ssize_t w = write(fd, s, n);
        if (w <= 0)
            return;
        s += w;
        n -= (size_t)w;
    }
}

int main(void)
{
    if (challoc_version == NULL) {
        say(2, "not running under challoc\n");
        return 1;
    }
    say(1, "ok stats_print_noinit\n");
    return 0;
}
