# tests/consumers/lib.sh: the parts of valkey-build.sh and redis-build.sh
# that are the same for both servers. Each script sets PROG (its name, for
# messages) and sources this file before anything else. The functions read
# the caller's STAGE, LOGS, ARCH and TEST_FLAGS.
# shellcheck shell=sh

# fail MESSAGE: print "PROG: MESSAGE" on stderr and exit 1.
fail() {
    echo "$PROG: $*" >&2
    exit 1
}

step() { printf '\n== %s\n' "$*"; }

# Check STAGE (build | memefficiency | full | all), default all.
check_stage() {
    STAGE=${STAGE:-all}
    case "$STAGE" in
        build|memefficiency|full|all) ;;
        *) echo "$PROG: STAGE must be build, memefficiency, full or all, got '$STAGE'" >&2; exit 2 ;;
    esac
}

# checkout_patched SRC URL TAG COMMIT PATCH...: clone URL at TAG into SRC
# (once), require the tag to resolve to COMMIT, and apply the PATCH series
# in order in one `git apply`. The stamp beside the tree (SRC.patches)
# records which version of the series the tree carries; a tree with no
# stamp, an older series or local edits is reset first. Leaves the shell
# in SRC.
checkout_patched() {
    src=$1 url=$2 tag=$3 commit=$4
    shift 4
    if [ ! -d "$src/.git" ]; then
        rm -rf "$src"
        git clone -q --depth 1 --branch "$tag" "$url" "$src"
    fi
    cd "$src" || exit 1
    head=$(git rev-parse HEAD)
    [ "$head" = "$commit" ] || fail "tag $tag is $head, expected $commit"
    stamp=$(cat "$@" | sha256sum | cut -d' ' -f1)
    if [ "$(cat "$src.patches" 2>/dev/null)" = "$stamp" ]; then
        echo "patches already applied"
    else
        if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
            echo "stale patches in the tree, resetting it"
            git checkout -q -- .
        fi
        git apply "$@"
        echo "$stamp" > "$src.patches"
        echo "patches applied"
    fi
    git diff --stat | tail -1
}

# check_server BIN LIB: BIN reports malloc=jemalloc-5.3.0-challoc, names
# libchalloc.so.0 in DT_NEEDED, has LIB in its rpath, and the tree built no
# deps/jemalloc. Leaves BIN's dynamic section in `dyn`.
check_server() {
    version=$("$1" --version)
    echo "$version"
    case "$version" in
        *"malloc=jemalloc-5.3.0-challoc"*) ;;
        *) fail "$1 does not report malloc=jemalloc-5.3.0-challoc" ;;
    esac
    # No ldd in the image; read the dynamic section instead. The DT_NEEDED
    # check is scripts/check-needed.sh, the same one a package test runs
    # (--as-needed drops the entry from a binary that references no symbol
    # of the library).
    sh "$ROOT/scripts/check-needed.sh" "$1" libchalloc.so.0 \
        || fail "$1 has no DT_NEEDED libchalloc.so.0"
    dyn=$(readelf -d "$1")
    echo "$dyn" | grep -E 'RUNPATH|RPATH' | grep -q "$2" \
        || fail "$1 has no rpath to $2"
    if [ -d deps/jemalloc/lib ]; then
        fail "deps/jemalloc was built"
    fi
}

# Set TCLSH to the first tclsh of 8.5 or newer. The suites' ./runtest only
# looks for tclsh8.5, 8.6, 8.7 and 9.0, and Wolfi's tcl is 9.1 now, so the
# suite is started the way runtest starts it, with tests/test_helper.tcl.
# Wolfi's tcl installs its script library in /usr/library, where tclsh does
# not look, and `clock` (unit/dump, integration/corrupt-dump-fuzzer) then
# fails with 'invalid command name "::tcl::clock::GetSystemTimeZone"'; point
# TCL_LIBRARY at it when the probe fails, as Wolfi's sqlite build does. The
# probe is a script file: tclsh reading stdin exits 0 after an error.
find_tclsh() {
    TCLSH=
    for t in tclsh9.1 tclsh9.0 tclsh8.7 tclsh8.6 tclsh8.5 tclsh; do
        c=$(command -v "$t" 2>/dev/null) || continue
        echo 'exit [expr {[package vsatisfies [info patchlevel] 8.5-]} ? 0 : 1]' | "$c" 2>/dev/null && { TCLSH=$c; break; }
    done
    [ -n "$TCLSH" ] || fail "no tclsh 8.5 or newer for the test suite"
    probe=$LOGS/clock-probe.tcl
    echo 'clock format 0' > "$probe"
    if ! "$TCLSH" "$probe" >/dev/null 2>&1; then
        for d in /usr/library /usr/share/tcl$(echo 'puts [info tclversion]' | "$TCLSH" 2>/dev/null); do
            [ -f "$d/init.tcl" ] || continue
            if TCL_LIBRARY=$d "$TCLSH" "$probe" >/dev/null 2>&1; then
                TCL_LIBRARY=$d; export TCL_LIBRARY; echo "TCL_LIBRARY=$d (tclsh's clock needs its script library)"; break
            fi
        done
        "$TCLSH" "$probe" >/dev/null 2>&1 \
            || fail "$TCLSH has no working clock command (set TCL_LIBRARY)"
    fi
    echo "tclsh: $TCLSH ($(echo 'puts [info patchlevel]' | "$TCLSH"))"
}

# run_tests NAME FLAGS...: the suite with FLAGS, --dump-logs and TEST_FLAGS,
# logged to LOGS/NAME-ARCH.log; fails unless it reports every test passed.
run_tests() {
    name=$1
    shift
    step "test_helper.tcl $* (log: $LOGS/$name-$ARCH.log)"
    start=$(date +%s)
    # shellcheck disable=SC2086 # TEST_FLAGS is a list of runtest flags
    if "$TCLSH" tests/test_helper.tcl --dump-logs ${TEST_FLAGS:-} "$@" 2>&1 | tee "$LOGS/$name-$ARCH.log" \
        | grep -E '^\[(ok|err|exception|skip|ignore)\]|All tests passed|tests failed|\[[0-9]+/[0-9]+ done\]|Killed by signal|Timed out' ; then :; fi
    end=$(date +%s)
    if grep -q 'All tests passed without errors' "$LOGS/$name-$ARCH.log"; then
        echo "== $name: PASS ($((end - start)) s)"
    else
        echo "== $name: FAIL ($((end - start)) s); see $LOGS/$name-$ARCH.log" >&2
        grep -E '^\[err\]|^\[exception\]|Killed by signal' "$LOGS/$name-$ARCH.log" | head -40 >&2
        return 1
    fi
}

# run_stages FLAGS...: the STAGE's suites, each with FLAGS: `unit/memefficiency`
# alone (the active defrag tests), then the full suite. Fails if either does.
run_stages() {
    rc=0
    if [ "$STAGE" = memefficiency ] || [ "$STAGE" = all ]; then
        run_tests memefficiency --single unit/memefficiency "$@" || rc=1
    fi
    if [ "$STAGE" = full ] || [ "$STAGE" = all ]; then
        run_tests full "$@" || rc=1
    fi
    return $rc
}
