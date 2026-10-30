#!/bin/sh
# tests/loom/run.sh: run the loom models (tests/loom/src/) and the pins
# (tests/loom/tests/pins.rs) under loom 0.7.2 with loom-0.7.2-dpor.patch.
#
#   sh tests/loom/run.sh [cargo test arguments, such as a test name filter]
#
# Stock loom's DPOR misses executions these models need (the patch says
# which; src/dpor.rs fails without it). The script checks Cargo.lock
# (`cargo fetch --locked`), copies loom 0.7.2 from the cargo registry into
# the target directory, applies the patch, and runs the tests with cargo
# pointed at the copy (`--config patch...`, offline: every other crate is
# the locked one). The patched source rewrites Cargo.lock's loom entry, so
# the script restores Cargo.lock when it exits. The LOOM_* variables that
# bound or cut short a run are unset: every model is exhaustive except the
# two that set their own preemption bound (src/lock.rs).
set -eu
cd "$(dirname "$0")"
here=$(pwd)

unset LOOM_MAX_PREEMPTIONS LOOM_MAX_BRANCHES LOOM_MAX_PERMUTATIONS LOOM_MAX_DURATION \
	LOOM_CHECKPOINT_FILE LOOM_CHECKPOINT_INTERVAL

cargo fetch --locked || {
	echo "loom: Cargo.lock is not the locked one; a killed run leaves the patched entry (git checkout tests/loom/Cargo.lock)" >&2
	exit 1
}
src=
for d in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/loom-0.7.2; do
	if [ -d "$d" ]; then
		src=$d
		break
	fi
done
[ -n "$src" ] || { echo "loom: loom-0.7.2 is not in the cargo registry" >&2; exit 1; }

target=${CARGO_TARGET_DIR:-$here/target}
work=$target/loom-0.7.2-dpor
rm -rf "$work"
mkdir -p "$target"
cp -Rp "$src" "$work"
(cd "$work" && patch -s -p1 < "$here/loom-0.7.2-dpor.patch")

cp Cargo.lock "$target/Cargo.lock.locked"
trap 'cp "$target/Cargo.lock.locked" "$here/Cargo.lock"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cargo --config "patch.crates-io.loom.path=\"$work\"" test --release --offline "$@"
