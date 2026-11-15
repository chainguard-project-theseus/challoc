#!/bin/sh
# tests/consumers/preload.sh CASE: an unmodified Wolfi program runs its own
# tests with challoc's libchalloc.so preloaded, against a baseline run of
# the same tests without it. Runs inside the dev container's `consumers`
# stage: `make preload CASE=cpython|git`, or the consumers check
# (ci/extended.sh cpython|git).
#
# This is how most of Wolfi would meet challoc: LD_PRELOAD in front of a
# program built against glibc's malloc. Valkey and Ruby (valkey-build.sh,
# ruby-build.sh) link challoc at build time instead.
#
# What it does:
#   1. builds challoc (QUANTUM, default 16) into target/CASE/challoc;
#   2. proves the preload takes: under LD_DEBUG=bindings the program's
#      `malloc` binds to that libchalloc.so, or the case fails;
#   3. fetches the test sources of the exact version Wolfi installed (the
#      program's own --version), not vendored;
#   4. runs the case's fixed test subset twice, without and with
#      LD_PRELOAD, logging each to target/CASE/logs/, and fails on a test
#      that fails only with challoc. A test that fails in both runs is
#      reported, not failed: it says nothing about the allocator. A
#      module or script in which no test passed, in either run, fails the
#      case: the harness ran nothing there. So does one in which fewer
#      tests passed with challoc than without it: a test that turned into
#      a skip, or a failure whose name the log did not give.
#
# Cases:
#   cpython  Wolfi's python3 (python-3.13 in the image) with its own
#            Lib/test from the python.org source tarball of its version
#            (only Lib/test is extracted, on PYTHONPATH, so the rest of
#            the standard library is Wolfi's). PYTHONMALLOC=malloc in both
#            runs sends every allocation, pymalloc's small objects
#            included, through malloc. Modules: CPYTHON_TESTS.
#   git      Wolfi's /usr/bin/git (GIT_TEST_INSTALLED) with t/ from the
#            kernel.org tarball of its version, SHA-256 checked against
#            that directory's sha256sums.asc; the tree is built only for
#            t/'s helpers (test-tool). Scripts: GIT_TESTS. test-lib.sh
#            swaps LD_PRELOAD for glibc's malloc-check library around each
#            test body and unsets it after, so both runs set
#            TEST_NO_MALLOC_CHECK=1, and the challoc run of GIT_PROBE runs
#            under LD_DEBUG=bindings: every git process in it must bind
#            malloc to libchalloc.so, or the case fails.
#
# A failure is identified by the test's own name: for cpython the
# "FAIL: name (module.Class.name)" and "ERROR: ..." lines of unittest's
# end-of-run report (they start at column 0 even when a test wrote output),
# for git the "not ok N - description" lines (a "# TODO" known breakage
# excluded); a module or script that exits non-zero adds "<module>: exit
# N", so a crash counts too. Passing tests are counted from unittest's
# "Ran N tests" and result lines (N less failures, errors, skips, expected
# failures and unexpected successes) and git's "ok N - " lines.
#
# Environment:
#   QUANTUM        16 (default) or 8
#   JOBS           make -j for git (default nproc)
#   CPYTHON_TESTS  default "test_list test_dict test_set test_bytes
#                  test_str test_gc test_threading test_mmap" (test_str is
#                  3.13's name for test_unicode)
#   GIT_TESTS      default "t0000-basic t1006-cat-file t1050-large
#                  t4000-diff-format t4001-diff-rename t5300-pack-object
#                  t5302-pack-index" (the object store, packs and diff)
#   GIT_PROBE      the GIT_TESTS script whose challoc run proves the
#                  preload inside the harness (default t4001-diff-rename,
#                  a short one: LD_DEBUG writes a file per process)
set -eu

PROG=preload.sh
fail() {
    echo "$PROG: $*" >&2
    exit 1
}
step() { printf '\n== %s\n' "$*"; }

case=${1:-${CASE:-}}
case $case in
    cpython|git) ;;
    *) echo "usage: $PROG cpython|git" >&2; exit 2 ;;
esac
cd "$(dirname "$0")/../.."
ROOT=$(pwd)
QUANTUM=${QUANTUM:-16}
JOBS=${JOBS:-$(nproc)}
WORK=$ROOT/target/$case
LOGS=$WORK/logs
rm -rf "$LOGS"
mkdir -p "$WORK" "$LOGS"

# 1. challoc.
step "challoc (quantum$QUANTUM) -> $WORK/challoc"
cargo build --release --target-dir "$WORK/challoc" --no-default-features --features "quantum$QUANTUM"
LIB=$WORK/challoc/release/libchalloc.so
[ -f "$LIB" ] || fail "$LIB missing"

# proves_preload PROGRAM ARGS...: PROGRAM's malloc binds to $LIB under
# LD_PRELOAD.
proves_preload() {
    if LD_DEBUG=bindings LD_PRELOAD=$LIB "$@" 2>"$LOGS/bindings.txt" >/dev/null \
        && grep -F "to $LIB " "$LOGS/bindings.txt" | grep -qF "symbol \`malloc'"; then
        echo "preload: $1's malloc binds to $LIB"
    else
        grep -F "symbol \`malloc'" "$LOGS/bindings.txt" | head -n 5 >&2 || true
        fail "$1's malloc does not bind to $LIB under LD_PRELOAD"
    fi
}

# passes RUN T N: record that N of T's tests passed in RUN.
passes() {
    echo "$2 ${3:-0}" >>"$LOGS/passes-$1.txt"
}

# unittest_passes LOG: the tests unittest's last "Ran N tests" line counts,
# less every count its result line ("OK (skipped=2)", "FAILED (failures=1,
# errors=1)") names; 0 when no run finished.
unittest_passes() {
    awk '
        /^Ran [0-9]+ tests? in / { ran = $2; res = "" }
        /^(OK|FAILED)( \(.*\))?$/ { res = $0 }
        END {
            n = ran + 0
            if (match(res, /\(.*\)/)) {
                k = split(substr(res, RSTART + 1, RLENGTH - 2), kv, ", ")
                for (i = 1; i <= k; i++) {
                    split(kv[i], p, "=")
                    n -= p[2]
                }
            }
            print n
        }' "$1"
}

# compare: the failures only the challoc run has fail the case. First, a
# module or script that passed no test in either run fails the case: the
# harness ran nothing there (a missing tool, a broken checkout), so
# matching failures in both runs would say nothing about the allocator.
# Last, one with fewer passing tests with challoc than without fails it: a
# test that turned into a skip, or a failure whose name the log lost.
compare() {
    empty=$(awk '$2 == 0 { print FILENAME ": " $1 }' "$LOGS/passes-baseline.txt" "$LOGS/passes-challoc.txt" \
        | sed "s|$LOGS/passes-||; s|\.txt:|:|")
    if [ -n "$empty" ]; then
        echo "FAIL $case: no test passed in (run: module or script):"
        printf '%s\n' "$empty" | sed 's/^/  /'
        return 1
    fi
    counts=$(awk 'NR == FNR { b[$1] = $2; next } { printf "%s%s %s/%s", sep, $1, b[$1], $2; sep = ", " }' \
        "$LOGS/passes-baseline.txt" "$LOGS/passes-challoc.txt")
    fewer=$(awk 'NR == FNR { b[$1] = $2; next } $2 < b[$1] { print $1 ": " b[$1] " without challoc, " $2 " with it" }' \
        "$LOGS/passes-baseline.txt" "$LOGS/passes-challoc.txt")
    sort -u "$LOGS/failures-baseline.txt" >"$LOGS/b.txt"
    sort -u "$LOGS/failures-challoc.txt" >"$LOGS/c.txt"
    both=$(comm -12 "$LOGS/b.txt" "$LOGS/c.txt")
    only=$(comm -13 "$LOGS/b.txt" "$LOGS/c.txt")
    gone=$(comm -23 "$LOGS/b.txt" "$LOGS/c.txt")
    if [ -n "$both" ]; then
        echo "fail in both runs (not the allocator):"
        printf '%s\n' "$both" | sed 's/^/  /'
    fi
    if [ -n "$gone" ]; then
        echo "fail only without challoc:"
        printf '%s\n' "$gone" | sed 's/^/  /'
    fi
    status=0
    if [ -n "$only" ]; then
        echo "FAIL $case: fail only with challoc preloaded (logs in $LOGS):"
        printf '%s\n' "$only" | sed 's/^/  /'
        status=1
    fi
    if [ -n "$fewer" ]; then
        echo "FAIL $case: fewer tests pass with challoc preloaded (logs in $LOGS):"
        printf '%s\n' "$fewer" | sed 's/^/  /'
        status=1
    fi
    [ "$status" -eq 0 ] || return 1
    echo "PASS $case: no test fails only with challoc preloaded; passing tests without/with it: $counts"
}

# fetch URL FILE: download URL to FILE once.
fetch() {
    [ -s "$2" ] || curl -fsSL --retry 3 -o "$2.part" "$1"
    [ -s "$2" ] || mv "$2.part" "$2"
}

cpython() {
    tests=${CPYTHON_TESTS:-test_list test_dict test_set test_bytes test_str test_gc test_threading test_mmap}
    py=$(command -v python3) || fail "no python3"
    proves_preload "$py" -c 'pass'
    version=$("$py" -c 'import platform; print(platform.python_version())')
    step "CPython $version: Lib/test from Python-$version.tar.xz"
    tarball=$WORK/Python-$version.tar.xz
    fetch "https://www.python.org/ftp/python/$version/Python-$version.tar.xz" "$tarball"
    src=$WORK/src-$version
    if [ ! -d "$src/Lib/test" ]; then
        rm -rf "$src"
        mkdir -p "$src"
        tar -xJf "$tarball" -C "$src" --strip-components=1 "Python-$version/Lib/test"
    fi
    for t in $tests; do
        [ -e "$src/Lib/test/$t.py" ] || [ -d "$src/Lib/test/$t" ] \
            || fail "$t is not in Python-$version's Lib/test (CPYTHON_TESTS)"
    done
    for run in baseline challoc; do
        : >"$LOGS/failures-$run.txt"
        : >"$LOGS/passes-$run.txt"
        for t in $tests; do
            step "$run: python3 -m test -v $t (log: $LOGS/$run-$t.log)"
            rc=0
            if [ "$run" = challoc ]; then
                PYTHONPATH=$src/Lib PYTHONMALLOC=malloc LD_PRELOAD=$LIB \
                    "$py" -m test -v "$t" >"$LOGS/$run-$t.log" 2>&1 || rc=$?
            else
                PYTHONPATH=$src/Lib PYTHONMALLOC=malloc \
                    "$py" -m test -v "$t" >"$LOGS/$run-$t.log" 2>&1 || rc=$?
            fi
            sed -nE 's/^(FAIL|ERROR): //p' "$LOGS/$run-$t.log" >>"$LOGS/failures-$run.txt"
            passes "$run" "$t" "$(unittest_passes "$LOGS/$run-$t.log")"
            [ "$rc" -eq 0 ] || echo "$t: exit $rc" >>"$LOGS/failures-$run.txt"
            tail -n 3 "$LOGS/$run-$t.log"
        done
    done
    compare
}

# git_probe: in the LD_DEBUG=bindings run of the probe script (one file
# per process in $LOGS/probe), every git process the test bodies started
# binds malloc to $LIB, and at least one did. A body that ran git without
# the preload (test-lib's malloc check swapping or unsetting LD_PRELOAD)
# binds it elsewhere and fails the case.
git_probe() {
    # ld.so names a program by its argv[0]: the bare `git` a body runs
    # through PATH, or the absolute path test-lib and run_command use.
    grep -hE "binding file ([^ ]*/)?git[^ /]* \[[0-9]+\] to .*symbol \`malloc'" \
        "$LOGS"/probe/b.* >"$LOGS/probe-malloc.txt" 2>/dev/null || true
    rm -rf "$LOGS/probe"
    # Each line starts with the process's pid; a process can log the
    # binding more than once.
    to_lib=$(awk -v lib=" to $LIB " 'index($0, lib) && !($1 in p) { p[$1]; n++ } END { print n + 0 }' \
        "$LOGS/probe-malloc.txt")
    elsewhere=$(awk -v lib=" to $LIB " '!index($0, lib) && !($1 in p) { p[$1]; n++ } END { print n + 0 }' \
        "$LOGS/probe-malloc.txt")
    if [ "$to_lib" -eq 0 ] || [ "$elsewhere" -ne 0 ]; then
        grep -vF " to $LIB " "$LOGS/probe-malloc.txt" | head -n 5 >&2 || true
        fail "in $probe, $to_lib git processes bind malloc to $LIB and $elsewhere elsewhere (want all of them, at least one)"
    fi
    echo "preload: in $probe, all $to_lib git processes bind malloc to $LIB"
}

git_case() {
    tests=${GIT_TESTS:-t0000-basic t1006-cat-file t1050-large t4000-diff-format t4001-diff-rename t5300-pack-object t5302-pack-index}
    proves_preload /usr/bin/git version
    version=$(/usr/bin/git --version | sed -n 's/^git version \([0-9.]*\).*/\1/p')
    [ -n "$version" ] || fail "cannot read git's version"
    step "git $version: t/ from git-$version.tar.xz"
    base=https://mirrors.edge.kernel.org/pub/software/scm/git
    tarball=$WORK/git-$version.tar.xz
    fetch "$base/git-$version.tar.xz" "$tarball"
    fetch "$base/sha256sums.asc" "$WORK/sha256sums-$version.asc"
    want=$(grep " git-$version.tar.xz\$" "$WORK/sha256sums-$version.asc" | cut -d' ' -f1)
    [ -n "$want" ] || fail "no git-$version.tar.xz in $base/sha256sums.asc"
    got=$(sha256sum "$tarball" | cut -d' ' -f1)
    [ "$got" = "$want" ] || { rm -f "$tarball"; fail "git-$version.tar.xz: SHA-256 $got, want $want"; }
    src=$WORK/src-$version
    if [ ! -x "$src/t/helper/test-tool" ]; then
        rm -rf "$src"
        mkdir -p "$src"
        tar -xJf "$tarball" -C "$src" --strip-components=1
        step "make -j$JOBS (t/'s helpers; log: $LOGS/build.log)"
        make -C "$src" -j"$JOBS" NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 \
            NO_CURL=1 NO_EXPAT=1 NO_OPENSSL=1 all >"$LOGS/build.log" 2>&1 \
            || { tail -n 30 "$LOGS/build.log" >&2; fail "git's build failed"; }
    fi
    for t in $tests; do
        [ -f "$src/t/$t.sh" ] || fail "$t.sh is not in git-$version's t/ (GIT_TESTS)"
    done
    # test-lib's chainlint, a lint of the test scripts themselves, needs
    # perl; it says nothing about git or the allocator.
    export GIT_TEST_CHAIN_LINT=0
    # test-lib's malloc check sets LD_PRELOAD to glibc's debug library
    # around each test body and unsets it after, which would take challoc
    # away from every body; this turns that check off in both runs.
    export TEST_NO_MALLOC_CHECK=1
    probe=${GIT_PROBE:-t4001-diff-rename}
    case " $tests " in
        *" $probe "*) ;;
        *) fail "GIT_PROBE $probe is not in GIT_TESTS" ;;
    esac
    for run in baseline challoc; do
        : >"$LOGS/failures-$run.txt"
        : >"$LOGS/passes-$run.txt"
        for t in $tests; do
            step "$run: GIT_TEST_INSTALLED=/usr/bin $t.sh (log: $LOGS/$run-$t.log)"
            rc=0
            if [ "$run" = challoc ] && [ "$t" = "$probe" ]; then
                rm -rf "$LOGS/probe"
                mkdir -p "$LOGS/probe"
                (cd "$src/t" && GIT_TEST_INSTALLED=/usr/bin LD_PRELOAD=$LIB \
                    LD_DEBUG=bindings LD_DEBUG_OUTPUT=$LOGS/probe/b \
                    sh "./$t.sh" -v) >"$LOGS/$run-$t.log" 2>&1 || rc=$?
                git_probe
            elif [ "$run" = challoc ]; then
                (cd "$src/t" && GIT_TEST_INSTALLED=/usr/bin LD_PRELOAD=$LIB \
                    sh "./$t.sh" -v) >"$LOGS/$run-$t.log" 2>&1 || rc=$?
            else
                (cd "$src/t" && GIT_TEST_INSTALLED=/usr/bin \
                    sh "./$t.sh" -v) >"$LOGS/$run-$t.log" 2>&1 || rc=$?
            fi
            grep -E '^not ok [0-9]+ - ' "$LOGS/$run-$t.log" | grep -v '# TODO' \
                | sed "s/^not ok [0-9]* - /$t: /" >>"$LOGS/failures-$run.txt" || true
            passes "$run" "$t" "$(grep -cE '^ok [0-9]+ - ' "$LOGS/$run-$t.log" || true)"
            [ "$rc" -eq 0 ] || echo "$t: exit $rc" >>"$LOGS/failures-$run.txt"
            tail -n 2 "$LOGS/$run-$t.log"
        done
    done
    compare
}

case $case in
    cpython) cpython ;;
    git) git_case ;;
esac
