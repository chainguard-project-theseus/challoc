#!/bin/sh
# Run the unit tests under Miri, one test per process, and sort the
# outcomes (verus/MIRI.md). A Miri error ends its process, so one run of
# the whole harness would stop at the first unsupported operation.
#
#   verus/miri/run.sh [-e] [-j JOBS] [-t SECONDS] OUT [FILTER]
#
# -e applies verus/miri/emulator.patch (the syscall emulator) to the copy
# and adds -Zmiri-ignore-leaks (the fixtures leak on purpose). MIRIFLAGS is
# passed through; -Zmiri-disable-isolation is always added. Needs a nightly
# toolchain with the miri component.
set -eu
usage() {
	echo "usage: $0 [-e] [-j JOBS] [-t SECONDS] OUT [FILTER]" >&2
	exit 2
}
emulate=0 jobs=$(nproc) secs=1200
while getopts ej:t: o; do
	case $o in
	e) emulate=1 ;;
	j) jobs=$OPTARG ;;
	t) secs=$OPTARG ;;
	*) usage ;;
	esac
done
shift $((OPTIND - 1))
[ $# -ge 1 ] || usage
mkdir -p "$1"
out=$(cd "$1" && pwd)
filter=${2:-}
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
mkdir -p "$out/logs"
# Miri re-reads the sources for every test: work on a copy on local disk.
src=$out/src
rm -rf "$src"
mkdir -p "$src"
git -C "$root" ls-files >"$out/files.txt"
tar -C "$root" -cf "$out/src.tar" -T "$out/files.txt"
tar -C "$src" -xf "$out/src.tar"
rm "$out/src.tar"
flags=-Zmiri-disable-isolation
if [ $emulate = 1 ]; then
	(cd "$src" && git apply "$root/verus/miri/emulator.patch")
	flags="$flags -Zmiri-ignore-leaks"
fi
export CARGO_TARGET_DIR=$out/target CARGO_INCREMENTAL=0
export MIRIFLAGS="$flags ${MIRIFLAGS:-}"
cd "$src"
cargo +nightly miri test --no-run -q
cargo +nightly miri test -q -- --list ${filter:+"$filter"} >"$out/list.txt"
sed -n 's/: test$//p' "$out/list.txt" >"$out/tests.txt"
if [ ! -s "$out/tests.txt" ]; then
	echo "no tests listed (see $out/list.txt)" >&2
	exit 1
fi
: >"$out/results.txt"
export out secs
# `killed` is a Miri process ended by a signal, most often the host running
# out of memory.
xargs -P "$jobs" -I{} sh -c '
	log="$out/logs/$1.log"
	timeout -k 10 "$secs" cargo +nightly miri test -q -- --exact "$1" >"$log" 2>&1 && r=pass || {
		rc=$?
		if [ $rc = 124 ] || [ $rc = 137 ]; then r=timeout
		elif grep -q "^error: Undefined Behavior" "$log"; then r=ub
		elif grep -q "^error: unsupported operation" "$log"; then r=unsupported
		elif grep -q "^error: abnormal termination" "$log"; then r=abort
		elif grep -q "(signal: " "$log"; then r=killed
		else r=fail
		fi
	}
	echo "$r $1" >>"$out/results.txt"
' _ {} <"$out/tests.txt"
sort -k2 "$out/results.txt" -o "$out/results.txt"
awk '{print $1}' "$out/results.txt" | sort | uniq -c
