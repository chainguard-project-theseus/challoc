/* pin: run a command with its CPU affinity set to a list ("0-9", "1", or
 * "0,2,4-6"), then exec it. Wolfi's dev image has no taskset, so this is the
 * `PIN=1` experiment of bench/lib.sh's run_row (bench/README.md, "Noise").
 * Invoke as
 *   rt env LD_PRELOAD=... pin 0-9 prog args
 * The exec keeps the pid, so rt's rusage and a perf attach still see prog. */
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: pin CPULIST cmd [args...]\n");
        return 2;
    }
    cpu_set_t set;
    CPU_ZERO(&set);
    int any = 0;
    char *list = argv[1];
    for (char *tok = strtok(list, ","); tok; tok = strtok(NULL, ",")) {
        char *dash = strchr(tok, '-');
        long lo = strtol(tok, NULL, 10), hi = dash ? strtol(dash + 1, NULL, 10) : lo;
        if (lo < 0 || hi < lo || hi >= CPU_SETSIZE) {
            fprintf(stderr, "pin: bad cpu range '%s'\n", tok);
            return 2;
        }
        for (long c = lo; c <= hi; c++) { CPU_SET((int)c, &set); any = 1; }
    }
    if (!any || sched_setaffinity(0, sizeof set, &set) != 0) {
        perror("pin: sched_setaffinity");
        return 2;
    }
    execvp(argv[2], argv + 2);
    perror("pin: execvp");
    return 127;
}
