#!/bin/sh
# tests/genmc/run.sh: run the GenMC harnesses (tests/genmc/cases) and
# check each verdict: a `pass` case must pass, a `fail` case (a control)
# must fail with one of its expected reports. Exits 1 if any case does
# not, 2 on a setup failure.
#
#   sh tests/genmc/run.sh [case name, or a grep -E pattern of names]
#   sh tests/genmc/run.sh --templates   # print the templates, one a line
#   sh tests/genmc/run.sh --self-test   # an unnamed template fails setup
#
# The templates are this directory's .rs files but prelude.rs (src/ is
# the crate that builds vstd). Every template must be named by a case,
# or the run stops at setup: a template no case names would never run.
# The genmc check formats the files `--templates` prints.
#
# Each case is one Rust file, `#![no_main]`, prelude.rs, the case's
# constants and its template, which GenMC compiles with rustc (its
# single-file mode, which links genmc-std as `std`) and explores
# exhaustively under each model the case names. The vspec wrappers and
# src/sync.rs compile into it unchanged (prelude.rs, and the loom
# crate's tests/loom/src/vspec.rs); vstd comes from a normal cargo
# build of this directory's crate. rustc runs at -Copt-level=1: at 0
# GenMC 0.20 stops on rustc's IR (an internal check in
# `PromoteMemIntrinsicPass`, or `Constant unimplemented for type`).
# -Zshare-generics=no (RUSTC_BOOTSTRAP=1) instantiates in the case the
# generic functions vstd's rlib would otherwise share, whose IR GenMC
# would not have. GenMC needs a rustc with its LLVM major version, which
# the genmc package pins.
#
# GenMC builds genmc-std inside its own install directory, so cases run
# one at a time, and the first run builds it for the rest.
set -eu
cd "$(dirname "$0")"
here=$(pwd)

trim() {
	printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

templates() {
	for f in *.rs; do
		[ "$f" = prelude.rs ] || printf '%s\n' "$f"
	done
}

check_templates() {
	for f in $(templates); do
		named=
		while IFS= read -r line; do
			case $line in '' | '#'*) continue ;; esac
			rest=${line#*|}
			if [ "$(trim "${rest%%|*}")" = "${f%.rs}" ]; then
				named=1
				break
			fi
		done < cases
		[ -n "$named" ] || { echo "genmc: template $f: no case in cases names it" >&2; exit 2; }
	done
}

# A copy of this directory with one more template, which no case names:
# its run must stop at setup (exit 2, before genmc or cargo).
self_test() {
	tmp=$(mktemp -d)
	trap 'rm -rf "$tmp"' EXIT
	cp run.sh cases ./*.rs "$tmp"/
	printf '%s\n' 'fn harness() {}' > "$tmp/unnamed.rs"
	rc=0
	sh "$tmp/run.sh" > "$tmp/out" 2>&1 || rc=$?
	if [ "$rc" -eq 2 ] && grep -Fq 'genmc: template unnamed.rs: no case in cases names it' "$tmp/out"; then
		echo "ok   self-test: an unnamed template fails setup"
	else
		echo "FAIL self-test: an unnamed template: run.sh exited $rc, not 2 with its report:"
		sed 's/^/    /' "$tmp/out"
		exit 1
	fi
}

case ${1:-} in
	--templates) templates; exit 0 ;;
	--self-test) self_test; exit 0 ;;
esac
filter=${1:-}
check_templates

command -v genmc > /dev/null || { echo "genmc: not on PATH" >&2; exit 2; }
genmc --version

target=${CARGO_TARGET_DIR:-$here/target}
work=$target/genmc-cases
rm -rf "$work"
mkdir -p "$work"

# vstd, and the directory of the dependencies it was built with.
cargo build --locked --message-format=json > "$work/vstd.json" || {
	echo "genmc: building vstd failed" >&2
	exit 2
}
vstd=$(grep -o '"[^"]*/libvstd-[^"/]*\.rlib"' "$work/vstd.json" | tr -d '"' | head -n 1)
[ -n "$vstd" ] && [ -f "$vstd" ] || { echo "genmc: no libvstd rlib in the cargo build" >&2; exit 2; }
deps=$(dirname "$vstd")

skip_build=
ran=0
bad=0
start=$(date +%s)
while IFS= read -r line; do
	case $line in '' | '#'*) continue ;; esac
	name=$(trim "${line%%|*}"); rest=${line#*|}
	tmpl=$(trim "${rest%%|*}"); rest=${rest#*|}
	models=$(trim "${rest%%|*}"); rest=${rest#*|}
	consts=$(trim "${rest%%|*}"); rest=${rest#*|}
	expect=$(trim "${rest%%|*}")
	case $rest in *'|'*) alts=${rest#*|} ;; *) alts= ;; esac
	if [ -n "$filter" ] && ! printf '%s\n' "$name" | grep -Eq -- "$filter"; then
		continue
	fi
	[ -f "$tmpl.rs" ] || { echo "genmc: case $name: no template $tmpl.rs" >&2; exit 2; }
	case $expect in
		pass) [ -z "$alts" ] || { echo "genmc: case $name: pass takes no expected report" >&2; exit 2; } ;;
		fail)
			[ -n "$alts" ] || { echo "genmc: case $name: fail with no expected report" >&2; exit 2; }
			a=$alts
			while [ -n "$a" ]; do
				[ -n "$(trim "${a%%|*}")" ] || { echo "genmc: case $name: an empty expected report" >&2; exit 2; }
				case $a in *'|'*) a=${a#*|} ;; *) a= ;; esac
			done
			;;
		*) echo "genmc: case $name: expected is '$expect', not pass or fail" >&2; exit 2 ;;
	esac
	[ -n "$models" ] || { echo "genmc: case $name: no model" >&2; exit 2; }

	dir=$work/$name
	mkdir -p "$dir"
	{
		echo '#![no_main]'
		echo "include!(\"$here/prelude.rs\");"
		printf '%s\n' "$consts"
		echo "include!(\"$here/$tmpl.rs\");"
	} > "$dir/main.rs"

	for model in $models; do
		case ${model%+live} in
			rc11) opts= ;;
			imm) opts=--imm ;;
			arm8) opts=--arm8 ;;
			*) echo "genmc: case $name: unknown model $model" >&2; exit 2 ;;
		esac
		# `+live`: a thread blocked for good at a spin loop is an error,
		# not an execution GenMC drops. The spin loops are then only the
		# harnesses' own (prelude.rs's `await_until!`), with GenMC's
		# automatic spin-assume pass off. Otherwise the pass may take the
		# background thread's loop, which sleeps in the futex stand-in,
		# for a spin on its loads, and the liveness check assumes a
		# blocked spin would later read newer values, so it would not
		# report a thread asleep on a stale `REQUESTED`. (With the pass
		# on, background, exit_waker and publish explored one execution
		# fewer each, with the same verdicts.)
		case $model in *+live) opts="$opts --check-liveness --disable-spin-assume" ;; esac
		log=$dir/$model.log
		t0=$(date +%s)
		rc=0
		# genmc writes rustc's output in the working directory.
		# --disable-estimation: GenMC otherwise samples random executions first
		# to estimate the state space, and an error found there makes the
		# report (which error, and whether an internal check trips) vary
		# from run to run.
		# shellcheck disable=SC2086 # $opts and $skip_build are words to split
		(cd "$dir" && RUSTC_BOOTSTRAP=1 genmc --disable-estimation $opts $skip_build -- \
			-Copt-level=1 -Zshare-generics=no -Cdebuginfo=2 --edition 2024 -A warnings \
			-L "dependency=$deps" --extern "vstd=$vstd" main.rs) > "$log" 2>&1 || rc=$?
		t1=$(date +%s)
		ran=$((ran + 1))
		execs=$(sed -n 's/^Number of complete executions explored: \([0-9]*\).*/\1/p' "$log" | head -n 1)
		verdict=
		if [ "$expect" = pass ]; then
			if [ "$rc" -eq 0 ] && grep -Fq 'No errors were detected' "$log" && [ "${execs:-0}" -gt 0 ]; then
				verdict="pass, $execs executions"
			fi
		elif [ "$rc" -eq 42 ]; then
			a=$alts
			while [ -n "$a" ]; do
				want=$(trim "${a%%|*}")
				case $a in *'|'*) a=${a#*|} ;; *) a= ;; esac
				if grep -Fq -- "$want" "$log"; then
					verdict="fails as expected: $want"
					break
				fi
			done
		fi
		if [ -n "$verdict" ]; then
			echo "ok   $name $model: $verdict ($((t1 - t0)) s)"
		else
			bad=$((bad + 1))
			echo "FAIL $name $model: expected $expect${alts:+ ($alts)}, genmc exited $rc; the end of $log:"
			tail -n 25 "$log" | sed 's/^/    /'
		fi
		# genmc-std is built once a run gets through compilation.
		if [ -z "$skip_build" ] && grep -Fq "*** Compilation complete." "$log"; then
			skip_build=--skip-genmc-std-build
		fi
	done
done < cases

[ "$ran" -gt 0 ] || { echo "genmc: no case matches '$filter'" >&2; exit 2; }
echo "genmc: $ran runs, $bad not as expected, $(($(date +%s) - start)) s"
[ "$bad" -eq 0 ]
