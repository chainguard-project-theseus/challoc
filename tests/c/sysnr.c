/* Prints "<name> <__NR_name>" for every syscall in src/sys/nr.rs so the
 * Rust unit test can cross-check the hand-declared numbers against the
 * kernel headers of the build container. A missing macro prints "-". */
#include <stdio.h>
#include <sys/syscall.h>

#define P(name)                                   \
    do {                                          \
        printf("%s ", #name);                     \
        PV_##name;                                \
        printf("\n");                             \
    } while (0)

#ifdef __NR_read
#define PV_read printf("%d", __NR_read)
#else
#define PV_read printf("-")
#endif
#ifdef __NR_write
#define PV_write printf("%d", __NR_write)
#else
#define PV_write printf("-")
#endif
#ifdef __NR_close
#define PV_close printf("%d", __NR_close)
#else
#define PV_close printf("-")
#endif
#ifdef __NR_mmap
#define PV_mmap printf("%d", __NR_mmap)
#else
#define PV_mmap printf("-")
#endif
#ifdef __NR_mprotect
#define PV_mprotect printf("%d", __NR_mprotect)
#else
#define PV_mprotect printf("-")
#endif
#ifdef __NR_munmap
#define PV_munmap printf("%d", __NR_munmap)
#else
#define PV_munmap printf("-")
#endif
#ifdef __NR_rt_sigaction
#define PV_rt_sigaction printf("%d", __NR_rt_sigaction)
#else
#define PV_rt_sigaction printf("-")
#endif
#ifdef __NR_rt_sigprocmask
#define PV_rt_sigprocmask printf("%d", __NR_rt_sigprocmask)
#else
#define PV_rt_sigprocmask printf("-")
#endif
#ifdef __NR_madvise
#define PV_madvise printf("%d", __NR_madvise)
#else
#define PV_madvise printf("-")
#endif
#ifdef __NR_getpid
#define PV_getpid printf("%d", __NR_getpid)
#else
#define PV_getpid printf("-")
#endif
#ifdef __NR_prctl
#define PV_prctl printf("%d", __NR_prctl)
#else
#define PV_prctl printf("-")
#endif
#ifdef __NR_gettid
#define PV_gettid printf("%d", __NR_gettid)
#else
#define PV_gettid printf("-")
#endif
#ifdef __NR_futex
#define PV_futex printf("%d", __NR_futex)
#else
#define PV_futex printf("-")
#endif
#ifdef __NR_clock_gettime
#define PV_clock_gettime printf("%d", __NR_clock_gettime)
#else
#define PV_clock_gettime printf("-")
#endif
#ifdef __NR_exit_group
#define PV_exit_group printf("%d", __NR_exit_group)
#else
#define PV_exit_group printf("-")
#endif
#ifdef __NR_tgkill
#define PV_tgkill printf("%d", __NR_tgkill)
#else
#define PV_tgkill printf("-")
#endif
#ifdef __NR_openat
#define PV_openat printf("%d", __NR_openat)
#else
#define PV_openat printf("-")
#endif
#ifdef __NR_getrandom
#define PV_getrandom printf("%d", __NR_getrandom)
#else
#define PV_getrandom printf("-")
#endif
#ifdef __NR_mseal
#define PV_mseal printf("%d", __NR_mseal)
#else
#define PV_mseal printf("-")
#endif
#ifdef __NR_sched_getaffinity
#define PV_sched_getaffinity printf("%d", __NR_sched_getaffinity)
#else
#define PV_sched_getaffinity printf("-")
#endif
#ifdef __NR_sched_setaffinity
#define PV_sched_setaffinity printf("%d", __NR_sched_setaffinity)
#else
#define PV_sched_setaffinity printf("-")
#endif
#ifdef __NR_sched_setscheduler
#define PV_sched_setscheduler printf("%d", __NR_sched_setscheduler)
#else
#define PV_sched_setscheduler printf("-")
#endif
#ifdef __NR_getpriority
#define PV_getpriority printf("%d", __NR_getpriority)
#else
#define PV_getpriority printf("-")
#endif
#ifdef __NR_setpriority
#define PV_setpriority printf("%d", __NR_setpriority)
#else
#define PV_setpriority printf("-")
#endif

int main(void) {
    P(read); P(write); P(close); P(mmap); P(mprotect); P(munmap);
    P(rt_sigaction); P(rt_sigprocmask); P(madvise); P(getpid); P(prctl);
    P(gettid); P(futex); P(clock_gettime); P(exit_group); P(tgkill);
    P(openat); P(getrandom); P(mseal);
    P(sched_getaffinity); P(sched_setaffinity); P(sched_setscheduler);
    P(getpriority); P(setpriority);
    return 0;
}
