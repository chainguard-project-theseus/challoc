#!/bin/sh
# verus/notcopy-control.sh: check that the not-Copy checks of
# src/vspec/notcopy.rs have teeth. `cargo check` with
# `--cfg challoc_notcopy_control` adds its controls, a Copy type and a
# Clone type under the same checks; the build must fail with exactly the
# three E0283 errors of those control assertions, and no other error.
#
#   sh verus/notcopy-control.sh
#
# Builds in its own target directory (CARGO_TARGET_DIR/notcopy-control,
# target/ by default), since the cfg rebuilds the crate.
set -u
cd "$(dirname "$0")/.." || exit 2
log=$(mktemp) || exit 2
trap 'rm -f "$log"' EXIT
if CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}/notcopy-control" \
	RUSTFLAGS='--cfg challoc_notcopy_control' \
	cargo check --lib --locked > "$log" 2>&1; then
	echo "notcopy-control: the build with the controls passed: the checks reject nothing"
	exit 1
fi
fail=0
for want in 'CopyControl as NotCopy<_>' 'CopyControl as NotClone<_>' 'CloneControl as NotClone<_>'; do
	n=$(grep -F -c "assert!(<$want>::HOLDS);" "$log")
	if [ "$n" -ge 1 ]; then
		echo "notcopy-control: $want: rejected"
	else
		echo "notcopy-control: $want: ACCEPTED"
		fail=1
	fi
done
e=$(grep -c '^error\[E0283\]' "$log")
a=$(grep -c '^error' "$log")
# The three E0283s, then `could not compile`.
if [ "$e" -ne 3 ] || [ "$a" -ne 4 ]; then
	echo "notcopy-control: want 3 E0283 errors and no other, got $e of $((a - 1)):"
	grep '^error' "$log"
	fail=1
fi
exit "$fail"
