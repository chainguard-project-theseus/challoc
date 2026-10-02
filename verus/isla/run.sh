#!/bin/sh
# Check the inline-assembly seams discharged with Isla (verus/ISLA.md)
# against the Sail Armv8.5-A model, on the aarch64 code built from this
# tree.
#
#   verus/isla/run.sh [-a LIBCHALLOC.a] [OUT]
#
# Builds the aarch64 static library (or takes the one given with -a),
# checks that the asm templates are the ones the records describe, finds
# every instance of each discharged template in the compiled code, runs
# isla-footprint on each distinct instruction and checks each job with
# isla-check (verus/isla/check.rs). OUT (default: a fresh temporary
# directory) keeps the traces and the jobs. Exit status 0 when every check
# holds.
#
# Tools, from the environment (ISLA.md "Setup"):
#   ISLA            an Isla checkout built with `cargo build --release`
#                   (isla-footprint, configs/armv8p5.toml)
#   ISLA_SNAPSHOTS  an isla-snapshots checkout (armv8p5.ir)
#   ISLA_FOOTPRINT  overrides $ISLA/target/release/isla-footprint
#   Z3              the z3 binary isla-check calls (default: z3 on PATH);
#                   isla-footprint itself needs libz3.so on LD_LIBRARY_PATH
#   RUSTUP_TOOLCHAIN the toolchain for the build (default 1.95.0); its
#                   llvm-tools component provides llvm-objdump and llvm-ar
set -eu
usage() {
	echo "usage: $0 [-a LIBCHALLOC.a] [OUT]" >&2
	exit 2
}
lib=
while getopts a: o; do
	case $o in
	a) lib=$OPTARG ;;
	*) usage ;;
	esac
done
shift $((OPTIND - 1))
[ $# -le 1 ] || usage
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:-$(mktemp -d "${TMPDIR:-/tmp}/challoc-isla.XXXXXX")}
mkdir -p "$out"
out=$(cd "$out" && pwd)
: "${ISLA:?set ISLA to an Isla checkout (verus/ISLA.md)}"
: "${ISLA_SNAPSHOTS:?set ISLA_SNAPSHOTS to an isla-snapshots checkout}"
footprint=${ISLA_FOOTPRINT:-$ISLA/target/release/isla-footprint}
ir=$ISLA_SNAPSHOTS/armv8p5.ir
toolchain=${RUSTUP_TOOLCHAIN:-1.95.0}
sysroot=$(rustc +"$toolchain" --print sysroot)
host=$(rustc +"$toolchain" -vV | sed -n 's/^host: //p')
llvm=$sysroot/lib/rustlib/$host/bin
for t in "$footprint" "$ir" "$llvm/llvm-objdump" "$llvm/llvm-ar"; do
	[ -e "$t" ] || { echo "isla: missing $t" >&2; exit 2; }
done
fails=0
fail() {
	echo "FAIL $*"
	fails=$((fails + 1))
}

# 1. The asm templates the records describe, verbatim up to whitespace.
# Each line of templates.txt is `FILE<TAB>FN<TAB>ASM`: some `asm!(...)`
# call inside `fn FN` in FILE must read ASM.
asm_blocks() { # FILE FN: every asm!(...) call in fn FN, one per line
	awk -v fn="$2" '
		$0 ~ "fn " fn "\\(" { infn = 1; depth = 0 }
		infn && /^(    )?}/ && !inasm { infn = 0 }
		infn && /asm!\(/ { inasm = 1; buf = "" }
		inasm {
			line = $0; sub(/^[ \t]+/, "", line)
			buf = buf (buf == "" ? "" : " ") line
			if (line ~ /\);$/) { inasm = 0; sub(/^.*asm!\(/, "asm!(", buf); print buf }
		}' "$root/$1"
}
tab=$(printf '\t')
while IFS="$tab" read -r file fn want; do
	case $file in '#'* | '') continue ;; esac
	if asm_blocks "$file" "$fn" | grep -Fqx -- "$want"; then
		echo "template $file $fn: ok"
	else
		fail "template $file $fn: no asm! block reads: $want"
	fi
done <"$here/templates.txt"

# Each Isla record's ledger entry (the one whose status names the record):
# its contract is the record's (the first ```rust block of each), and the
# item sits under the `#[cfg(target_arch = ...)]` of the architecture in
# the record's title. The x86_64 and aarch64 `raw::syscallN` share file,
# name and contract, so the second check is what tells them apart.
rust_blocks() { # FILE: every ```rust block, its lines joined by \001
	awk '/^```rust$/ { inb = 1; buf = ""; next }
		inb && /^```$/ { inb = 0; print buf; next }
		inb { buf = buf (buf == "" ? "" : "\001") $0 }' "$1"
}
entry_of() { # RECORD-PATH: "FILE:LINE<TAB>CONTRACT" of the entry naming it
	awk -v want="Status: discharged ($1)." '
		function flush() { if (hit) print loc "\t" blk; hit = 0; loc = ""; blk = ""; nb = 0 }
		/^### S[0-9]+\. / { flush(); next }
		/^src\/[^ ]*:[0-9]+\. Kind: / { loc = $1; sub(/\.$/, "", loc) }
		index($0, want) { hit = 1 }
		/^```rust$/ { inb = 1; nb++; next }
		inb && /^```$/ { inb = 0; next }
		inb && nb == 1 { blk = blk (blk == "" ? "" : "\001") $0 }
		END { flush() }' "$root/verus/SEAMS.md"
}
arch_at() { # FILE LINE: the target_arch of the last column-0 cfg above LINE
	awk -v n="$2" 'NR >= n { exit }
		/^#\[cfg\(target_arch = "[a-z0-9_]+"\)\]/ { a = $0; sub(/^[^"]*"/, "", a); sub(/".*/, "", a) }
		END { print a }' "$root/$1"
}
for rec in "$root"/verus/seams/*.md; do
	grep -q 'Tool: Isla\. Status: discharged' "$rec" || continue
	name=verus/seams/$(basename "$rec")
	arch=$(sed -n '1s/.*(\(aarch64\|x86_64\))$/\1/p' "$rec")
	entries=$(entry_of "$name")
	if [ "$(printf '%s\n' "$entries" | grep -c .)" -ne 1 ]; then
		fail "record $name: not exactly one ledger entry has its status"
		continue
	fi
	loc=${entries%%"$tab"*}
	if [ "$(rust_blocks "$rec" | head -n 1)" != "${entries#*"$tab"}" ]; then
		fail "record $name: its contract differs from the ledger's ($loc)"
	elif [ -z "$arch" ] || [ "$(arch_at "${loc%:*}" "${loc##*:}")" != "$arch" ]; then
		fail "record $name: $loc is not the ${arch:-?} item its title names"
	else
		echo "record $name: $loc ($arch), contract ok"
	fi
done

# 2. The aarch64 code: one object with every codegen unit.
if [ -z "$lib" ]; then
	echo "building the aarch64 static library (CARGO_TARGET_DIR=$out/target)"
	(cd "$root" && CARGO_TARGET_DIR=$out/target cargo +"$toolchain" rustc -q --release --lib \
		--target aarch64-unknown-linux-gnu --crate-type staticlib)
	lib=$out/target/aarch64-unknown-linux-gnu/release/libchalloc.a
fi
rm -rf "$out/obj" && mkdir -p "$out/obj"
members=$("$llvm/llvm-ar" t "$lib" | grep '^challoc-.*\.o$')
[ -n "$members" ] || { echo "isla: no challoc objects in $lib" >&2; exit 2; }
(cd "$out/obj" && "$llvm/llvm-ar" x "$lib" $members)
for m in $members; do
	"$llvm/llvm-objdump" -dr --no-print-imm-hex "$out/obj/$m"
done >"$out/dis.txt"

# 3. Isla's view of one instruction. The model runs at Non-secure EL0 in
# AArch64 state (SCR_EL3.{NS,RW}), as Linux user space does, with a
# symbolic PC and vector bases; `-s` simplifies the trace.
config=$out/armv8p5.toml
# The configuration's default toolchain names a cross assembler Isla only
# uses for assembly input; this harness passes opcodes.
sed 's/^\(assembler\|objdump\|nm\|linker\) = .*/\1 = "true"/' "$ISLA/configs/armv8p5.toml" >"$config"
traced=
trace() { # NAME ISLA-INSTRUCTION-ARGS...
	name=$1
	shift
	case " $traced " in *" $name "*) return 0 ;; esac
	traced="$traced $name"
	"$footprint" -A "$ir" -C "$config" -R PSTATE.EL=0b00 -R PSTATE.nRW=0b0 \
		-R SCR_EL3=0x0000000000000401 -R _PC=undefined:%bv64 \
		-R VBAR_EL1=undefined:%bv64 -R VBAR_EL2=undefined:%bv64 -s "$@" \
		>"$out/$name.trace" 2>"$out/$name.err" || {
		fail "isla-footprint $name: $(grep -v 'No primop' "$out/$name.err" | head -3)"
		: >"$out/$name.trace"
	}
}
le() { # WORD: the instruction's bytes in memory order, as -x takes them
	printf '%02x%02x%02x%02x' $(($1 & 255)) $(($1 >> 8 & 255)) $(($1 >> 16 & 255)) $(($1 >> 24 & 255))
}
bin5() {
	n=$1 s=
	for _ in 1 2 3 4 5; do s=$((n & 1))$s n=$((n >> 1)); done
	echo "$s"
}
rustc +"$toolchain" -O --edition 2021 "$here/check.rs" -o "$out/isla-check"
check() { # JOB
	"$out/isla-check" "$1" || fail "isla-check $1"
}

# 4. `svc #0` (sys::raw's aarch64 syscall0..syscall6): every svc, hvc or
# smc in the code is `svc #0` (brk, from sys::trap, is not checked), and
# Isla's trace of it.
nsvc=$(grep -cE "^ +[0-9a-f]+: d4000001 +${tab}svc${tab}#0$" "$out/dis.txt" || true)
nother=$(grep -E "${tab}(svc|hvc|smc)${tab}" "$out/dis.txt" | grep -vcE ": d4000001 +${tab}svc${tab}#0$" || true)
echo "svc #0 instances: $nsvc; other svc, hvc or smc: $nother"
[ "$nsvc" -gt 0 ] || fail "no svc #0 in the aarch64 code"
[ "$nother" -eq 0 ] || fail "$nother svc/hvc/smc other than svc #0"
# Linux runs its kernel at EL1, or at EL2 with VHE (HCR_EL2 as Linux sets
# it for a VHE host: E2H, TGE, RW), which takes EL0's exceptions to EL2:
# one job for each.
trace svc0 -x -i "$(le 0xd4000001)"
trace svc0-vhe -x -i "$(le 0xd4000001)" -R HCR_EL2=0x0000000488000000
cp "$here/svc0.job" "$here/svc0-vhe.job" "$out/"
check "$out/svc0.job"
check "$out/svc0-vhe.job"

# 5. The TLS slot load (tls::get_tp on aarch64, and get's aarch64 arm):
# every `adrp A, :gottprel:challoc_tls; ldr A, [A, :gottprel_lo12:...];
# mrs B, tpidr_el0; ldr C, [B, A]` in the code, one job per register
# assignment, with the relocated immediates symbolic.
awk '
	/^ +[0-9a-f]+: [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f] / { n++; word[n] = $2; text[n] = $0; next }
	/R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21\tchalloc_tls$/ { page[n] = 1; next }
	/R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC\tchalloc_tls$/ { lo[n] = 1; next }
	/challoc_tls/ { other++ }
	END {
		for (i = 1; i <= n; i++) if (page[i]) {
			kind = (text[i + 3] ~ /\tldr\t/) ? "load" : (text[i + 3] ~ /\tstr\t/) ? "store" : "other"
			if (!lo[i + 1]) kind = "other"
			print kind, word[i], word[i + 1], word[i + 2], word[i + 3]
		}
		for (i = 1; i <= n; i++) if (lo[i] && !page[i - 1]) other++
		if (other) print "other-reloc", other
	}' "$out/dis.txt" | sort | uniq -c >"$out/tls-sites.txt"
cat "$out/tls-sites.txt"
grep -q ' load ' "$out/tls-sites.txt" || fail "no TLS slot load in the aarch64 code"
if grep -Eq ' (other|other-reloc) ' "$out/tls-sites.txt"; then
	fail "a challoc_tls access matches neither the load nor the store template"
fi
grep ' load ' "$out/tls-sites.txt" >"$out/tls-loads.txt"
while read -r _count _kind w1 w2 w3 w4; do
	a=$((0x$w1 & 31)) b=$((0x$w3 & 31)) c=$((0x$w4 & 31))
	ok=1
	[ $((0x$w1 & 0x9f00001f)) -eq $((0x90000000 | a)) ] || ok=0              # adrp xA, 0
	[ $((0x$w2)) -eq $((0xf9400000 | a << 5 | a)) ] || ok=0                    # ldr xA, [xA]
	[ $((0x$w3 & 0xffffffe0)) -eq $((0xd53bd040)) ] || ok=0                    # mrs xB, tpidr_el0
	[ $((0x$w4)) -eq $((0xf8606800 | a << 16 | b << 5 | c)) ] || ok=0          # ldr xC, [xB, xA]
	[ $a -ne $b ] && [ $a -ne $c ] && [ $b -ne $c ] || ok=0
	if [ $ok -eq 0 ]; then
		fail "TLS load $w1 $w2 $w3 $w4: not the template's shape"
		continue
	fi
	trace "adrp-x$a" --partial -i "1 immlo:2 10000 immhi:19 $(bin5 $a)"
	trace "ldr-x$a" --partial -i "1111100101 imm12:12 $(bin5 $a) $(bin5 $a)"
	trace "mrs-x$b" -x -i "$(le $((0x$w3)))"
	trace "ldr-x$c-x$b-x$a" -x -i "$(le $((0x$w4)))"
	job=$out/tls-load-x$a-x$b-x$c.job
	{
		echo "insn adrp-x$a.trace fallthrough"
		echo "insn ldr-x$a.trace fallthrough"
		echo "insn mrs-x$b.trace fallthrough"
		echo "insn ldr-x$c-x$b-x$a.trace fallthrough"
		echo "writes R$a R$b R$c"
		sed -e "s/@A@/R$a/g" -e "s/@B@/R$b/g" -e "s/@C@/R$c/g" "$here/tls-load.job.in"
	} >"$job"
	check "$job"
done <"$out/tls-loads.txt"

# 6. Negative controls: jobs that must fail, so a checker that accepts
# everything fails here. On the real traces: a wrong syndrome, a wrong
# exception level, a memory read where there is none, `svc` expected to
# fall through, the GOT load one entry off, the TLS load without the LDR
# relocation's constraint or without the slot's alignment, and `add x0,
# x1, #3` claimed to write nothing. On synthetic traces (controls/): a
# read of a register an earlier write left inexpressible, a write through
# a field accessor, and a write of PSTATE.N under `flags-preserved`.
refute() { # NAME JOB-TEXT
	printf '%s\n' "$2" >"$out/neg-$1.job"
	if "$out/isla-check" "$out/neg-$1.job" >"$out/neg-$1.log" 2>&1; then
		fail "negative control $1 passed"
	else
		echo "negative control $1: rejected, as it should be"
	fi
}
refute svc-esr "$(sed 's/#x56000000/#x56000001/' "$out/svc0.job")"
refute svc-el "$(sed 's/^assert (= final.PSTATE.EL #b01)$/assert (= final.PSTATE.EL #b10)/' "$out/svc0.job")"
refute svc-mem "$(sed 's/^mem-reads 0$/mem-reads 1/' "$out/svc0.job")"
refute svc-falls "insn svc0.trace fallthrough"
first=$(head -n 1 "$out/tls-loads.txt")
set -- $first
a=$((0x$3 & 31)) b=$((0x$5 & 31)) c=$((0x$6 & 31))
tls=$out/tls-load-x$a-x$b-x$c.job
refute got-off "$(sed 's/^assert (= mem.1.addr got)$/assert (= mem.1.addr (bvadd got #x0000000000000008))/' "$tls")"
refute no-lo12 "$(grep -v '^assume (= seg.2.imm12 ' "$tls")"
refute no-align "$(grep -v '^assume (= ((_ extract 2 0) (bvadd init.TPIDR_EL0 mem.1.val)) #b000)$' "$tls")"
trace add-x0-x1 -x -i "$(le 0x91000c20)"
refute add-writes "insn add-x0-x1.trace fallthrough"
ctl=$here/controls
refute poison "$(printf 'insn %s\ninsn %s\nwrites R0 R1\nassert (= final.R1 final.R1)' \
	"$ctl/poison-1.trace" "$ctl/poison-2.trace")"
refute field-write "insn $ctl/field-write.trace fallthrough"
refute flags "$(printf 'insn %s\nflags-preserved' "$ctl/flags.trace")"

if [ "$fails" -ne 0 ]; then
	echo "isla: $fails check(s) failed (traces and jobs in $out)"
	exit 1
fi
echo "isla: all checks passed (traces and jobs in $out)"
