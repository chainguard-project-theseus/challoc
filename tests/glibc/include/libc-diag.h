/* challoc stand-in for glibc's internal <libc-diag.h>: just the three
 * diagnostic-control macros the malloc tests use. Same spelling and
 * semantics as glibc's, so the sources in malloc/ compile unmodified. */
#ifndef CHALLOC_LIBC_DIAG_H
#define CHALLOC_LIBC_DIAG_H

#define _DIAG_STR1(s) #s
#define _DIAG_STR(s) _DIAG_STR1(s)

#define DIAG_PUSH_NEEDS_COMMENT _Pragma("GCC diagnostic push")
#define DIAG_POP_NEEDS_COMMENT _Pragma("GCC diagnostic pop")
/* `version` documents the compiler the warning was seen with; unused. */
#define DIAG_IGNORE_NEEDS_COMMENT(version, option) \
    _Pragma(_DIAG_STR(GCC diagnostic ignored option))

#endif
