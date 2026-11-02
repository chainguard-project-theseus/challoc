#!/bin/sh
# Check the inline-assembly seams verus/isla/seams.md lists with Isla
# (verus/ISLA.md): against the Sail Armv8.5-A model (and the PAC ones also
# against the Armv9.4-A model, for FEAT_PAuth2 and FEAT_FPAC) on the
# aarch64 code built from this tree, and against the Sail x86 model
# (verus/isla/x86/) on the x86_64 code.
#
#   verus/isla/run.sh [-a LIBCHALLOC.a] [-x LIBCHALLOC.a] [OUT]
#
# Checks that each seam seams.md lists is the ledger entry whose overrides
# line cites it (`Isla: verus/isla/seams.md`), with the contract seams.md
# quotes, and that the asm templates are the ones templates.txt lists
# (`seams.sh --check`). Then, for each target in ISLA_TARGETS, builds the
# release static library (or takes the one given with -a for aarch64, -x
# for x86_64), finds every instance of each checked template in the
# compiled code, runs isla-footprint on each distinct instruction (the
# PAC ones in parallel, one per CPU) and checks each job with isla-check
# (verus/isla/check.rs): run-aarch64.sh and run-x86_64.sh. Last, checks
# that the jobs each seam's `Jobs:` line in seams.md names for a target
# in ISLA_TARGETS ran and passed. OUT (default: a fresh temporary
# directory) keeps the traces and the jobs. Exit status 0 when every
# check holds.
#
# Tools (verus/ISLA.md, "Setup"; the defaults are where the isla,
# isla-snapshots, sail, isla-sail, z3 and llvm packages put them):
#   ISLA_TARGETS    the targets to check (default "aarch64 x86_64"); a
#                   target without a library from -a or -x is built with
#                   `cargo rustc --target <target>-unknown-linux-gnu`,
#                   which needs that target's standard library
#   ISLA_FOOTPRINT  isla-footprint (default: on PATH)
#   ISLA_PREPROCESS isla-preprocess (default: on PATH)
#   ISLA_CONFIGS    Isla's configurations, armv8p5.toml and armv9p4.toml
#                   (default /usr/share/isla/configs)
#   ISLA_SNAPSHOTS  the Sail models, armv8p5.ir and armv9p4.ir or
#                   armv9p4.ir.gz (default /usr/share/isla-snapshots)
#   ISLA_X86_IR     the x86 model (needed for x86_64 only); without it,
#                   run.sh fetches sail-x86-from-acl2 at a pinned commit
#                   and builds the model with verus/isla/x86/build-ir.sh
#   Z3              the z3 binary isla-check calls (default: z3 on PATH)
#   ISLA_CHECK_JOBS the threads each isla-check judges prefixes on (default
#                   1, as run.sh runs the PAC jobs one isla-check per CPU)
#   CARGO, RUSTC    the build's cargo and rustc (default: on PATH)
#   ISLA_CARGO_FLAGS extra cargo flags for the library, such as
#                   `--no-default-features --features quantum8`
#   LLVM_OBJDUMP, LLVM_AR  llvm-objdump and llvm-ar (default: on PATH)
set -eu
usage() {
	echo "usage: $0 [-a LIBCHALLOC.a] [-x LIBCHALLOC.a] [OUT]" >&2
	exit 2
}
lib=
xlib=
while getopts a:x: o; do
	case $o in
	a) lib=$OPTARG ;;
	x) xlib=$OPTARG ;;
	*) usage ;;
	esac
done
shift $((OPTIND - 1))
[ $# -le 1 ] || usage
abspath() { # FILE: FILE's absolute path (the library is extracted in another directory)
	echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
}
[ -z "$lib" ] || lib=$(abspath "$lib")
[ -z "$xlib" ] || xlib=$(abspath "$xlib")
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:-$(mktemp -d "${TMPDIR:-/tmp}/challoc-isla.XXXXXX")}
mkdir -p "$out"
out=$(cd "$out" && pwd)
targets=${ISLA_TARGETS:-aarch64 x86_64}
for t in $targets; do
	case $t in aarch64 | x86_64) ;; *) echo "isla: unknown target $t in ISLA_TARGETS" >&2; exit 2 ;; esac
done
want() { case " $targets " in *" $1 "*) return 0 ;; esac; return 1; }
tool() { # NAME: NAME's path, from PATH when it has no slash
	case $1 in */*) echo "$1" ;; *) command -v "$1" || echo "$1" ;; esac
}
footprint=$(tool "${ISLA_FOOTPRINT:-isla-footprint}")
preprocess=$(tool "${ISLA_PREPROCESS:-isla-preprocess}")
configs=${ISLA_CONFIGS:-/usr/share/isla/configs}
snapshots=${ISLA_SNAPSHOTS:-/usr/share/isla-snapshots}
cargo=${CARGO:-cargo}
rustc=${RUSTC:-rustc}
llvm_objdump=$(tool "${LLVM_OBJDUMP:-llvm-objdump}")
llvm_ar=$(tool "${LLVM_AR:-llvm-ar}")
ir=$snapshots/armv8p5.ir
need="$footprint $llvm_objdump $llvm_ar"
if want aarch64; then
	ir9=$snapshots/armv9p4.ir
	if [ ! -e "$ir9" ] && [ -e "$ir9.gz" ]; then
		gunzip -c "$ir9.gz" >"$out/armv9p4.ir.part" && mv "$out/armv9p4.ir.part" "$out/armv9p4.ir"
		ir9=$out/armv9p4.ir
	fi
	need="$need $preprocess $ir $ir9 $configs/armv8p5.toml $configs/armv9p4.toml"
fi
if want x86_64; then
	xir=${ISLA_X86_IR:-}
	if [ -z "$xir" ]; then
		# The x86 model from sail-x86-from-acl2 at the commit the x86 jobs
		# were written against, built with the sail and isla-sail packages.
		sx=$out/sail-x86-from-acl2
		sxc=0c6c023dccc7c20d8d9dbfc481f01d9bb49d62bf
		if [ "$(git -C "$sx" rev-parse HEAD 2>/dev/null)" != "$sxc" ]; then
			rm -rf "${sx:?}"
			git init -q "$sx"
			git -C "$sx" fetch -q --depth 1 https://github.com/rems-project/sail-x86-from-acl2 \
				"$sxc"
			git -C "$sx" checkout -q FETCH_HEAD
		fi
		sh "$here/x86/build-ir.sh" "$sx" "$out/x86"
		xir=$out/x86.ir
	fi
	need="$need $xir"
fi
for t in $need; do
	[ -e "$t" ] || { echo "isla: missing $t" >&2; exit 2; }
done
export ISLA_CHECK_JOBS="${ISLA_CHECK_JOBS:-1}"
fails=0
fail() {
	echo "FAIL $*"
	fails=$((fails + 1))
}
tab=$(printf '\t')


# 1. Each seam seams.md lists (a `## \`FILE\` \`NAME\` (ARCH)` heading, ARCH
# aarch64, x86_64 or both) is the one the ledger file of FILE names NAME,
# with the contract seams.md quotes (the first ```rust block of each),
# under the `#[cfg(target_arch = ...)]` of ARCH, or under none for both
# (one item on both architectures, such as `sys::pac`'s). The
# x86_64 and aarch64 `raw::syscallN` share file, name and contract, so
# the last check is what tells `syscallN#1` from `syscallN#2`. The seams
# are exactly those whose overrides line under verus/ledger/ cites
# `Isla: verus/isla/seams.md` (`discharged (Isla: ...)`, or `trusted
# (partly checked, Isla: ...; not checked: ...)` for a check that leaves
# part of the contract to something else), each with tool Islaris/Isla, so
# none escapes the check. The ledger must be current, and `seams.sh
# --check` also holds each seam's asm! calls to templates.txt: every call
# in the item listed under its file and function, every line read by such
# a call. That ties templates to seams, not to jobs: the runners pick
# their jobs by instruction shape, whatever seams.md lists. The last
# section ties each seam to the jobs its `Jobs:` line names.
first_rust() { # FILE HEADING: the first ```rust block after HEADING, lines joined by \001
	awk -v h="$2" '
		$0 == h { on = 1; next }
		on && /^## / { exit }
		on && /^```rust$/ { inb = 1; next }
		on && inb && /^```$/ { print buf; exit }
		on && inb { buf = buf (buf == "" ? "" : "\001") $0 }' "$1"
}
arch_at() { # FILE LINE: the target_arch cfg on the item at LINE, "?" when unclear
	# A column-0 attribute applies to the next column-0 item, which is
	# LINE's own when LINE starts at column 0 and else the one LINE sits in
	# (an fn in `#[cfg(target_arch = ...)] mod imp`); an indented one to
	# the indented item it precedes. A cfg naming target_arch in any other
	# form, or a LINE that is no item, gives "?", which matches no seam.
	awk -v n="$2" '
		function arch(s) {
			if (s ~ /^[ \t]*#\[cfg\(target_arch = "[a-z0-9_]+"\)\]/) { sub(/^[^"]*"/, "", s); sub(/".*/, "", s); return s }
			return s ~ /target_arch/ ? "?" : ""
		}
		NR == n && /^[ \t]*(#|\/\/|}|$)/ { print "?"; exit }
		/^(#\[|\/\/)/ { a = arch($0); if (a != "") b = a; next }
		/^[ \t]+(#\[|\/\/)/ { a = arch($0); if (a != "") ib = a; next }
		/^}/ { b = ""; next }
		/^[^ \t]/ { cur = b; b = ""; ib = ""; if (NR == n) { print cur; exit } next }
		NR == n { print (ib != "" ? ib : cur); exit }
		{ ib = "" }' "$root/$1"
}
cited_tool() { # LEDGER-DIR: FILE NAME TOOL for each overrides line under LEDGER-DIR citing seams.md
	(cd "$1" && find src -name '*.overrides' | sort | while read -r f; do
		awk -F"$tab" -v f="${f%.overrides}" '!/^#/ && index($4, "Isla: verus/isla/seams.md") { print f "\t" $1 "\t" $3 }' "$f"
	done)
}
other_tool() { # CITED-TOOL: one FAIL line naming each seam whose tool is not Islaris/Isla
	awk -F"$tab" '$3 != "Islaris/Isla" { s = s (s == "" ? "" : ", ") $1 " " $2 " (" $3 ")" }
		END { if (s != "") print "FAIL overrides lines citing verus/isla/seams.md with a tool other than Islaris/Isla: " s }' "$1"
}
seam_heads() { # SEAMS-MD: FILE NAME ARCH per seam heading, `bad` and the line for another form
	awk -F'`' '/^## `src\// {
		if (NF == 5 && $1 == "## " && $3 == " " && $5 ~ /^ \((aarch64|x86_64|both)\)$/) { a = $5; gsub(/^ \(|\)$/, "", a); print $2 "\t" $4 "\t" a }
		else print "bad\t" $0 }' "$1"
}
ledger_loc() { # FILE NAME: FILE:LINE of NAME's entry in FILE's ledger, empty when there is none
	awk -v h="## \`$2\`" '$0 == h { on = 1; next } on && /^src\// { sub(/\. Kind:.*/, ""); print; exit }' "$root/verus/ledger/$1.md" 2>/dev/null || true
}
seam_items() { # SEAMS-MD HEADS: per seam in HEADS (seam_heads of SEAMS-MD), a `FAIL seam ...` or a `seam ...` line
	while IFS="$tab" read -r file name arch; do
		loc=$(ledger_loc "$file" "$name")
		if [ -z "$loc" ]; then
			echo "FAIL seam $file $name: no ledger entry in verus/ledger/$file.md"
		elif [ "$(first_rust "$1" "## \`$file\` \`$name\` ($arch)")" != "$(first_rust "$root/verus/ledger/$file.md" "## \`$name\`")" ]; then
			echo "FAIL seam $file $name: verus/isla/seams.md's contract differs from the ledger's ($loc)"
		elif [ "$(arch_at "${loc%:*}" "${loc##*:}")" != "$(echo "$arch" | sed 's/^both$//')" ]; then
			echo "FAIL seam $file $name: $loc is not the $arch item seams.md names"
		else
			echo "seam $file $name: $loc ($arch), contract ok"
		fi
	done <"$2"
}
report() { # LOG: each `FAIL ...` line of LOG fails the run; the others are printed
	while IFS= read -r l; do
		case $l in "FAIL "*) fail "${l#FAIL }" ;; *) echo "$l" ;; esac
	done <"$1"
}
if sh "$root/verus/seams.sh" --check >"$out/seams-check.log" 2>&1; then
	echo "seam ledger and templates: current"
else
	fail "seam ledger or templates: sh verus/seams.sh --check fails: $(head -n 1 "$out/seams-check.log")"
fi
smd=$root/verus/isla/seams.md
cited_tool "$root/verus/ledger" >"$out/cited-tool.txt"
other_tool "$out/cited-tool.txt" >"$out/cited-other.txt"
report "$out/cited-other.txt"
cut -f1,2 "$out/cited-tool.txt" >"$out/cited.txt"
seam_heads "$smd" >"$out/seams-md.txt"
[ -s "$out/seams-md.txt" ] || fail "verus/isla/seams.md: no seam"
! grep -q '^bad' "$out/seams-md.txt" ||
	fail "verus/isla/seams.md: a seam heading not of the form ## \`FILE\` \`NAME\` (ARCH): $(grep '^bad' "$out/seams-md.txt" | head -n 1 | cut -f2)"
sort "$out/cited.txt" >"$out/cited.sorted"
cut -f1,2 "$out/seams-md.txt" | sort >"$out/seams-md.sorted"
cmp -s "$out/seams-md.sorted" "$out/cited.sorted" ||
	fail "verus/isla/seams.md (<) and the overrides lines citing it (>) differ: $(diff "$out/seams-md.sorted" "$out/cited.sorted" | grep '^[<>]' | tr '\n' ' ')"
seam_items "$smd" "$out/seams-md.txt" >"$out/seam-items.txt"
report "$out/seam-items.txt"

# The tools every target shares: isla-check, the negative-control runner,
# the encodings, and a pool of background tasks.
"$rustc" -O --edition 2021 "$here/check.rs" -o "$out/isla-check"
: >"$out/jobs-ran.txt"
ran() { # NAME ok|fail: one isla-check of job NAME (ARCH/JOB), for the last section
	printf '%s\t%s\n' "$1" "$2" >>"$out/jobs-ran.txt"
}
check() { # NAME JOB: isla-check JOB, recorded as one run of NAME
	if "$out/isla-check" "$2"; then
		ran "$1" ok
	else
		ran "$1" fail
		fail "isla-check $2"
	fi
}
refute() { # NAME JOB-TEXT REASON
	printf '%s\n' "$2" >"$out/neg-$1.job"
	# Rejected means the checker judged a combination and failed it with a
	# `FAIL` line naming REASON, the failure the control is built to cause,
	# not that it could not run the job (a missing trace or an empty job
	# from a `sed` on a file that is not there) or failed some other way.
	[ $# -eq 3 ] && [ -n "$3" ] || { fail "negative control $1: no reason given"; return; }
	if "$out/isla-check" "$out/neg-$1.job" >"$out/neg-$1.log" 2>&1; then
		fail "negative control $1 passed"
	elif ! grep '^FAIL' "$out/neg-$1.log" | grep -qF -- "$3"; then
		fail "negative control $1 did not fail for its reason ($3): $(grep -m 1 '^FAIL' "$out/neg-$1.log" || tail -n 1 "$out/neg-$1.log")"
	else
		echo "negative control $1: rejected, as it should be"
	fi
}
# The negative controls of a check that is not a job (sections 1 and 10
# and the last): each runs the check on a mutated copy of its input,
# which must fail for the reason the mutation causes.
refuted() { # NAME LOG REASON: LOG, a check's output on a mutated copy, has a FAIL line naming REASON
	if ! grep -q '^FAIL' "$2"; then
		fail "negative control $1 passed"
	elif ! grep '^FAIL' "$2" | grep -qF -- "$3"; then
		fail "negative control $1 did not fail for its reason ($3): $(grep -m 1 '^FAIL' "$2")"
	else
		echo "negative control $1: rejected, as it should be"
	fi
}
traced=
le() { # WORD: the instruction's bytes in memory order, as -x takes them
	printf '%02x%02x%02x%02x' $(($1 & 255)) $(($1 >> 8 & 255)) $(($1 >> 16 & 255)) $(($1 >> 24 & 255))
}
bin5() {
	n=$1 s=
	for _ in 1 2 3 4 5; do s=$((n & 1))$s n=$((n >> 1)); done
	echo "$s"
}
par=$(nproc 2>/dev/null || echo 4)
# At most `par` background tasks at once: a task takes a token from the
# pipe on fd 3 before it starts and puts it back when it ends. A run
# killed before the pipe was opened can leave one in a reused OUT.
rm -f "${out:?}/tokens.fifo"
mkfifo "$out/tokens.fifo"
exec 3<>"$out/tokens.fifo"
rm -f "${out:?}/tokens.fifo"
for _ in $(seq "$par"); do echo >&3; done

# The libraries: the release build's static library, one object with every
# codegen unit.
build() { # TARGET: the static library for TARGET-unknown-linux-gnu
	echo "building the $1 static library (CARGO_TARGET_DIR=$out/target)" >&2
	# shellcheck disable=SC2086
	(cd "$root" && CARGO_TARGET_DIR=$out/target "$cargo" rustc -q --locked --release --lib \
		--target "$1-unknown-linux-gnu" --crate-type staticlib ${ISLA_CARGO_FLAGS:-}) >&2
	echo "$out/target/$1-unknown-linux-gnu/release/libchalloc.a"
}
if want aarch64; then
	[ -n "$lib" ] || lib=$(build aarch64)
	. "$here/run-aarch64.sh"
fi
if want x86_64; then
	[ -n "$xlib" ] || xlib=$(build x86_64)
	. "$here/run-x86_64.sh"
fi

# Last: each seam's jobs. The runners pick their jobs by instruction
# shape, not from seams.md, so without this a seam could be listed there
# (and its asm! in templates.txt) with no job tracing it. Each seam
# heading in seams.md carries one `Jobs:` line naming the jobs that trace
# the seam, as ARCH/JOB (the name the runner records each isla-check of
# the job under, such as `aarch64/tls-load` for every
# tls-load-xA-xB-xC.job): at least one for each architecture its heading
# names (both for `both`), and none for another. A `both` seam whose arm
# on one architecture has no asm! call names
# ARCH/none for it instead, and nothing else for that architecture. Each
# named job of an architecture in ISLA_TARGETS must have run at least once
# and passed every time. This does not show that a job traces the seam's
# own template, or that an ARCH/none arm has no asm! call: that is what
# the `Jobs:` line claims, and seams.md says why for each seam.
seam_jobs() { # SEAMS-MD JOBS-RAN: per seam heading, `FAIL ...` lines or one `seam ...` line
	awk -v targets=" $targets " '
		function no(msg) { print "FAIL seam " seam ": " msg; nf++ }
		function end_seam(  n, i, t, a, na, nx, nn, ok, nos, skip, m) {
			if (seam == "") return
			split("", none)
			n = split(jobs, j, " ")
			if (n == 0) no("names no job (no Jobs: line under its heading in verus/isla/seams.md)")
			if (nl > 1) no("more than one Jobs: line")
			for (i = 1; i <= n; i++) {
				t = j[i]
				if (t !~ /^(aarch64|x86_64)\/[a-z0-9][a-z0-9_-]*$/) { no("job " t " is not ARCH/JOB"); continue }
				a = substr(t, 1, index(t, "/") - 1)
				if (a == "aarch64") na++; else nx++
				if (arch != "both" && a != arch) no("job " t " is for another architecture")
				else if (t ~ /\/none$/) { if (a in none) no(t " twice"); none[a]++; nn++; nos = nos " " t; if (arch != "both") no(t " is only for a both seam") }
				else if (index(targets, " " a " ") == 0) skip = skip " " t
				else if (t in failed) no("job " t " failed")
				else if (!(t in passed)) no("job " t " did not run")
				else ok = ok " " t
			}
			if (n > 0 && arch != "x86_64" && na == 0) no("no aarch64 job")
			if (n > 0 && arch != "aarch64" && nx == 0) no("no x86_64 job")
			if (("aarch64" in none) && na > none["aarch64"]) no("aarch64/none and an aarch64 job")
			if (("x86_64" in none) && nx > none["x86_64"]) no("x86_64/none and an x86_64 job")
			if (n > 0 && nn == n) no("names no job, only ARCH/none")
			if (nf == 0) {
				m = ok == "" ? "" : " jobs" ok " ran and passed"
				if (nos != "") m = m (m == "" ? "" : ";") nos " (no asm! call there)"
				if (skip != "") m = m (m == "" ? "" : ";") " not run (not in ISLA_TARGETS):" skip
				print "seam " seam ":" m
			}
			seam = ""
		}
		FILENAME == ARGV[1] { split($0, r, "\t"); if (r[2] == "ok") passed[r[1]] = 1; else failed[r[1]] = 1; next }
		/^## / { end_seam() }
		/^## `src\// {
			# Section 1 fails a heading of another form.
			if (split($0, h, "`") == 5 && h[5] ~ /^ \((aarch64|x86_64|both)\)$/) {
				arch = h[5]; gsub(/^ \(|\)$/, "", arch); seam = h[2] " " h[4] " (" arch ")"; nl = 0; nf = 0; jobs = ""
			}
			next
		}
		/^Jobs:/ {
			if (seam == "") { print "FAIL verus/isla/seams.md line " FNR ": a Jobs: line outside a seam section"; next }
			nl++; s = $0; sub(/^Jobs:/, "", s); gsub(/[`,]/, " ", s); jobs = jobs " " s
		}
		END { end_seam() }' "$2" "$1"
}
seam_jobs "$smd" "$out/jobs-ran.txt" >"$out/seam-jobs.txt"
report "$out/seam-jobs.txt"

# The negative controls of section 1 and of this one.
# arch_at itself on the x86_64 raw::syscall0 (syscall0#1, an fn in a mod
# under the cfg): the aarch64-only seams alone would not tell it from an
# arch_at that gives aarch64 everywhere.
x=$(ledger_loc src/sys/raw.rs 'syscall0#1')
xa=$(arch_at "${x%:*}" "${x##*:}" 2>/dev/null || true)
if [ "$xa" = x86_64 ]; then
	echo "control arch-at: $x (syscall0#1) is the x86_64 item, as it should be"
else
	fail "control arch-at: arch_at gives \"$xa\" for ${x:-no ledger entry} (syscall0#1), not x86_64"
fi
# Section 1 on seams.md with syscall0#2, the aarch64 item, headed (x86_64),
# and on seams.md with its contract's first line changed.
a=$(ledger_loc src/sys/raw.rs 'syscall0#2')
awk '$0 == "## `src/sys/raw.rs` `syscall0#2` (aarch64)" { sub(/aarch64/, "x86_64") } { print }' "$smd" >"$out/neg-seam-arch.md"
seam_heads "$out/neg-seam-arch.md" >"$out/neg-seam-arch.txt"
seam_items "$out/neg-seam-arch.md" "$out/neg-seam-arch.txt" >"$out/neg-seam-arch.log"
refuted seam-arch "$out/neg-seam-arch.log" "seam src/sys/raw.rs syscall0#2: $a is not the x86_64 item"
awk '$0 == "## `src/sys/raw.rs` `syscall0#2` (aarch64)" { on = 1 }
	on && /^```rust$/ { print; getline; sub(/-> usize$/, "-> isize"); on = 0 } { print }' "$smd" >"$out/neg-seam-contract.md"
seam_items "$out/neg-seam-contract.md" "$out/seams-md.txt" >"$out/neg-seam-contract.log"
refuted seam-contract "$out/neg-seam-contract.log" "seam src/sys/raw.rs syscall0#2: verus/isla/seams.md's contract differs"
# The overrides lines' tool, on a copy of the ledger whose syscall0#2 line
# names Kani.
rm -rf "${out:?}/neg-ledger"
cp -R "$root/verus/ledger" "$out/neg-ledger"
awk -F"$tab" -v OFS="$tab" '$1 == "syscall0#2" { $3 = "Kani" } { print }' "$root/verus/ledger/src/sys/raw.rs.overrides" \
	>"$out/neg-ledger/src/sys/raw.rs.overrides"
cited_tool "$out/neg-ledger" >"$out/neg-cited-tool.txt"
other_tool "$out/neg-cited-tool.txt" >"$out/neg-cited-tool.log"
refuted cited-tool "$out/neg-cited-tool.log" 'with a tool other than Islaris/Isla: src/sys/raw.rs syscall0#2 (Kani)'
# The last section on seams.md with a seam heading that has no Jobs: line.
{
	cat "$smd"
	echo
	echo "## \`src/sys/fake.rs\` \`fake\` (aarch64)"
} >"$out/neg-seam-no-job.md"
seam_jobs "$out/neg-seam-no-job.md" "$out/jobs-ran.txt" >"$out/neg-seam-no-job.log"
refuted seam-no-job "$out/neg-seam-no-job.log" 'FAIL seam src/sys/fake.rs fake (aarch64): names no job'

if [ "$fails" -ne 0 ]; then
	echo "isla: $fails check(s) failed (traces and jobs in $out)"
	exit 1
fi
echo "isla: all checks passed on $targets (traces and jobs in $out)"
