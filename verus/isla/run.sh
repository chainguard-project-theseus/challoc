#!/bin/sh
# Check the inline-assembly seams discharged with Isla (verus/ISLA.md)
# against the Sail Armv8.5-A model (and the PAC ones also against the
# Armv9.4-A model, for FEAT_PAuth2 and FEAT_FPAC), on the aarch64 code
# built from this tree, and against the Sail x86 model (verus/isla/x86/)
# on the x86_64 code.
#
#   verus/isla/run.sh [-a LIBCHALLOC.a] [-x LIBCHALLOC.a] [OUT]
#
# Builds the aarch64 and x86_64 static libraries (or takes the ones given
# with -a and -x), checks that the asm templates are the ones the records
# describe, finds every instance of each checked template in the
# compiled code, runs isla-footprint on each distinct instruction (the
# PAC ones in parallel, one per CPU) and checks each job with isla-check
# (verus/isla/check.rs); the blocks with an empty template have no
# instruction to trace, and verus/emptyasm/run.sh checks they have none
# in the shipped code. OUT (default: a fresh temporary directory) keeps
# the traces and the jobs. Exit status 0 when every check holds. About
# fifteen minutes after the builds on 16 CPUs.
#
# Tools, from the environment (ISLA.md "Setup"):
#   ISLA            an Isla checkout built with `cargo build --release`
#                   (isla-footprint, configs/armv8p5.toml, armv9p4.toml)
#   ISLA_SNAPSHOTS  an isla-snapshots checkout (armv8p5.ir, and armv9p4.ir
#                   or the armv9p4.ir.gz the checkout carries)
#   ISLA_X86_IR     the x86 model, built by verus/isla/x86/build-ir.sh
#   ISLA_FOOTPRINT  overrides $ISLA/target/release/isla-footprint
#   ISLA_PREPROCESS overrides $ISLA/target/release/isla-preprocess
#   Z3              the z3 binary isla-check calls (default: z3 on PATH);
#                   isla-footprint itself needs libz3.so on LD_LIBRARY_PATH
#   ISLA_CHECK_JOBS the threads each isla-check judges prefixes on (default
#                   1, as run.sh runs one isla-check per CPU; 4 for the x86
#                   jobs of step 12, the longest)
#   RUSTUP_TOOLCHAIN the toolchain for the build (default 1.95.0); its
#                   llvm-tools component provides llvm-objdump and llvm-ar
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
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:-$(mktemp -d "${TMPDIR:-/tmp}/challoc-isla.XXXXXX")}
mkdir -p "$out"
out=$(cd "$out" && pwd)
: "${ISLA:?set ISLA to an Isla checkout (verus/ISLA.md)}"
: "${ISLA_SNAPSHOTS:?set ISLA_SNAPSHOTS to an isla-snapshots checkout}"
: "${ISLA_X86_IR:?set ISLA_X86_IR to the x86 model (verus/isla/x86/build-ir.sh)}"
footprint=${ISLA_FOOTPRINT:-$ISLA/target/release/isla-footprint}
preprocess=${ISLA_PREPROCESS:-$ISLA/target/release/isla-preprocess}
ir=$ISLA_SNAPSHOTS/armv8p5.ir
xir=$ISLA_X86_IR
toolchain=${RUSTUP_TOOLCHAIN:-1.95.0}
sysroot=$(rustc +"$toolchain" --print sysroot)
host=$(rustc +"$toolchain" -vV | sed -n 's/^host: //p')
llvm=$sysroot/lib/rustlib/$host/bin
ir9=$ISLA_SNAPSHOTS/armv9p4.ir
if [ ! -e "$ir9" ] && [ -e "$ir9.gz" ]; then
	gunzip -c "$ir9.gz" >"$out/armv9p4.ir.part" && mv "$out/armv9p4.ir.part" "$out/armv9p4.ir"
	ir9=$out/armv9p4.ir
fi
for t in "$footprint" "$preprocess" "$ir" "$ir9" "$xir" "$ISLA/configs/armv9p4.toml" "$llvm/llvm-objdump" "$llvm/llvm-ar"; do
	[ -e "$t" ] || { echo "isla: missing $t" >&2; exit 2; }
done
export ISLA_CHECK_JOBS=${ISLA_CHECK_JOBS:-1}
fails=0
fail() {
	echo "FAIL $*"
	fails=$((fails + 1))
}

# 1. The asm templates the records describe, verbatim up to whitespace.
# Each line of templates.txt is `FILE<TAB>FN<TAB>ASM`: some `asm!(...)`
# call inside `fn FN` in FILE must read ASM.
asm_blocks() { # FILE FN: every asm!(...) call in fn FN, one per line
	# A call runs from `asm!(` to its matching `)` (parentheses inside
	# string literals not counted), with the `;` right after it if any.
	awk -v fn="$2" '
		$0 ~ "fn " fn "\\(" { infn = 1; depth = 0 }
		infn && /^(    )?}/ && !inasm { infn = 0 }
		infn && !inasm && /asm!\(/ { inasm = 1; buf = ""; d = 0; instr = 0 }
		inasm {
			line = $0; sub(/^[ \t]+/, "", line)
			if (buf == "") line = substr(line, index(line, "asm!("))
			out = ""; done = 0
			for (k = 1; k <= length(line) && !done; k++) {
				c = substr(line, k, 1); out = out c
				if (c == "\"") instr = !instr
				else if (!instr && c == "(") d++
				else if (!instr && c == ")" && --d == 0) {
					done = 1
					if (substr(line, k + 1, 1) == ";") out = out ";"
				}
			}
			buf = buf (buf == "" ? "" : " ") out
			if (done) { inasm = 0; print buf }
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
# the record's title, or under none when the title names no architecture
# (`sys::pac`'s items). The x86_64 and aarch64 `raw::syscallN` share file,
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
arch_at() { # FILE LINE: the target_arch cfg on the item at LINE, "?" when unclear
	# A column-0 attribute applies to the next column-0 item, which is
	# LINE's own when LINE starts at column 0 and else the one LINE sits in
	# (an fn in `#[cfg(target_arch = ...)] mod imp`); an indented one to
	# the indented item it precedes. A cfg naming target_arch in any other
	# form, or a LINE that is no item, gives "?", which matches no record.
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
# The records are the ones verus/seams.overrides marks discharged with
# tool Islaris/Isla, so none escapes the check.
irecs=$(awk -F"$tab" '$3 == "Islaris/Isla" && $4 ~ /^discharged \(verus\/seams\/.*\.md\)$/ { r = $4; sub(/^discharged \(/, "", r); sub(/\)$/, "", r); print r }' "$root/verus/seams.overrides")
[ -n "$irecs" ] || fail "no Isla record in verus/seams.overrides"
for name in $irecs; do
	rec=$root/$name
	if [ ! -f "$rec" ] || ! grep -q 'Tool: Isla\. Status: discharged' "$rec"; then
		fail "record $name: missing, or not stating \"Tool: Isla. Status: discharged\" on one line"
		continue
	fi
	arch=$(sed -n '1s/.*(\(aarch64\|x86_64\))$/\1/p' "$rec")
	entries=$(entry_of "$name")
	if [ "$(printf '%s\n' "$entries" | grep -c .)" -ne 1 ]; then
		fail "record $name: not exactly one ledger entry has its status"
		continue
	fi
	loc=${entries%%"$tab"*}
	if grep -q '@[A-Z0-9_-]*@' "$rec"; then
		fail "record $name: an unfilled @...@ placeholder"
	elif [ "$(rust_blocks "$rec" | head -n 1)" != "${entries#*"$tab"}" ]; then
		fail "record $name: its contract differs from the ledger's ($loc)"
	elif [ "$(arch_at "${loc%:*}" "${loc##*:}")" != "$arch" ]; then
		# A title without an architecture names an item both share.
		fail "record $name: $loc is not the ${arch:-architecture-independent} item its title names"
	else
		echo "record $name: $loc (${arch:-both architectures}), contract ok"
	fi
done

# 1b. The blocks with an empty template (`/* {0} */`, a register hint;
# verus/emptyasm/blocks.txt): Isla has no instruction of theirs to
# trace, so verus/emptyasm/run.sh checks that the shipped code holds
# none, on both targets (the records of shared_base, word_index,
# entry_barrier and ClassLine::guard_word rest on it).
if RUSTUP_TOOLCHAIN=$toolchain EMPTYASM_TARGET_DIR=$out/target sh "$root/verus/emptyasm/run.sh" "$out/emptyasm"; then
	echo "empty asm blocks: ok"
else
	fail "verus/emptyasm/run.sh: an empty-template block is not empty in the code"
fi

# 2. The aarch64 code: one object with every codegen unit (the x86_64
# library is built here too, and read from step 9).
if [ -z "$lib" ]; then
	echo "building the aarch64 static library (CARGO_TARGET_DIR=$out/target)"
	(cd "$root" && CARGO_TARGET_DIR=$out/target cargo +"$toolchain" rustc -q --release --lib \
		--target aarch64-unknown-linux-gnu --crate-type staticlib)
	lib=$out/target/aarch64-unknown-linux-gnu/release/libchalloc.a
fi
if [ -z "$xlib" ]; then
	echo "building the x86_64 static library (CARGO_TARGET_DIR=$out/target)"
	(cd "$root" && CARGO_TARGET_DIR=$out/target cargo +"$toolchain" rustc -q --release --lib \
		--target x86_64-unknown-linux-gnu --crate-type staticlib)
	xlib=$out/target/x86_64-unknown-linux-gnu/release/libchalloc.a
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
# smc in the code is `svc #0` (brk, from sys::trap, is section 7), and
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

# 6. Pointer authentication (pac::sign, auth, strip and the round trip
# axiom_auth_sign; verus/seams/sys-pac-*.md). Every PAC-family instruction
# in the code is `pacda Xd, Xn` or `autda Xd, Xn` (d and n distinct, neither
# 31) or `xpacd Xd` (d not 31). Two models, each at Non-secure EL0 with the
# PAC traps off: SCR_EL3.{API,APK} set, as firmware sets them, and either
# a kernel at EL1 with HCR_EL2 = HCR_HOST_NVHE_FLAGS (RW, API, APK, ATA) or a
# VHE kernel (E2H, TGE, RW). TCR is Linux's for each user VA size it supports:
# 36 (16K pages), 39 (4K), 42 (64K), 47 (16K), 48 (4K) and 52 bits (64K),
# each with TBI0 on, SCTLR (EnDA) symbolic, and ComputePAC
# abstract: Isla leaves the PAC algorithm an uninterpreted function.
# - Armv8.5-A (armv8p5.ir; configurations el1-36 to vhe-52): FEAT_PAuth
#   and the v8.2-v8.5 features on. `pacda` puts the PAC in the pointer's
#   PAC field; a failed `autda` returns the pointer with an error code.
# - Armv9.4-A (armv9p4.ir; p2-el1-36 to p2-vhe-52, and p2c-el1-36 to
#   p2c-vhe-52 with FEAT_CONSTPACFIELD), run as an Armv9.0-A CPU (v8.2-v8.5
#   and v9.0 on, and every feature flag from Armv8.6 on off) with
#   FEAT_PAuth, FEAT_PAuth2, FEAT_FPAC and FEAT_FPACCOMBINE on and
#   FEAT_EPAC off, as on Neoverse V2 (C4A): `pacda` XORs the PAC into the
#   field (PAuth2) and a failed `autda` takes the FPAC exception. The
#   model's footprint function decodes and executes only, from the model's
#   register initialisers, the configuration's defaults and its
#   `[registers.reset]` values (the checker's `pc-step` adds the PC
#   update, and each instruction names the register it writes when it
#   completes, since the function swallows an UNDEFINED abort without a
#   trace event), with the EDSCR in Non-debug state (no external debugger
#   has halted the core). `autda` runs with `--stop-at
#   AArch64_PACFailException`: a failed authentication ends at that call
#   (the `stop` kind), and the exception entry it makes is checked once per
#   configuration on the canonical `autda x0, x1` run through
#   (`pac-fault.job.in`).
awk -F"$tab" '/^ +[0-9a-f]+: [0-9a-f]+ +\t/ && $2 ~ /^(pac|aut|xpac)|^(reta|bra|blra|ldra|ereta)[ab]/ {
	split($1, a, " "); print $2, a[2] }' "$out/dis.txt" | sort | uniq -c >"$out/pac-sites.txt"
cat "$out/pac-sites.txt"
pac_words=
for kind in pacda autda xpacd; do
	grep -q " $kind " "$out/pac-sites.txt" || fail "no $kind in the aarch64 code"
done
while read -r _count mn w; do
	w=$((0x$w))
	d=$((w & 31)) n=$((w >> 5 & 31))
	case $mn in
	pacda) [ $((w & 0xfffffc00)) -eq $((0xdac10800)) ] && [ $d -ne $n ] && [ $n -ne 31 ] && [ $d -ne 31 ] ;;
	autda) [ $((w & 0xfffffc00)) -eq $((0xdac11800)) ] && [ $d -ne $n ] && [ $n -ne 31 ] && [ $d -ne 31 ] ;;
	xpacd) [ $((w & 0xffffffe0)) -eq $((0xdac147e0)) ] && [ $d -ne 31 ] ;;
	*) false ;;
	esac || { fail "PAC instruction $mn $(printf %08x $w): not pacda/autda/xpacd on distinct general registers"; continue; }
	pac_words="$pac_words $mn:$w"
done <"$out/pac-sites.txt"
pac_config=$out/armv8p5-pac.toml
sed 's/^"__v8\([2345]\)_implemented" = false/"__v8\1_implemented" = true/' "$config" >"$pac_config"
# The Armv9.4-A configuration files: FEAT_CONSTPACFIELD and FEAT_PACQARMA3
# (the PAC algorithm QARMA3 in place of QARMA5, for the purity spot checks)
# as given, the PAC features as Neoverse V2 has them, and every feature
# from Armv8.6 on that the model's register initialisers would turn on
# (the configuration file leaves most to them) pinned off, so the CPU is
# an Armv9.0-A one with PAuth2, FPAC and FPACCOMBINE. The settings go at
# the end of `[registers.defaults]`, after any line for the same flag is
# removed, and each must then appear exactly once, so a changed Isla
# configuration fails here rather than running another CPU.
pac9_later="AMUv1p1 BF16 AA32BF16 DGH ECV FGT HPMN0 I8MM AA32I8MM F32MM F64MM MPAMv0p1
MPAMv1p1 MTPMU TWED AFP EBF16 HCX LPA2 LS64 LS64_ACCDATA LS64_V MTE3 PAN3 PMUv3p7 RPRES
WFxT XS CMOW Debugv8p8 GICv3_NMI HBC MOPS NMI PMUv3_EXT64 PMUv3_TH PMUv3p8 SCTLR2 TCR2
TIDCP1 ADERR AIE ANERR CLRBHB CSSC Debugv8p9 DoubleFault2 ECBHB FGT2 HAFT LRCPC3 MTE4
MTE_ASYM_FAULT MTE_ASYNC MTE_CANONICAL_TAGS MTE_NO_ADDRESS_TAGS MTE_PERM MTE_STORE_ONLY
MTE_TAGGED_FAR PCSRv8p9 PFAR PMUv3_EDGE PMUv3_ICNTR PMUv3_SS PMUv3p9 PRFMSLC RASSAv2 RASv2
RPRFM S1PIE S1POE S2PIE S2POE SPECRES2 SPE_CRR SPE_FDS SPMU THE ETEv1p1 ETEv1p2 ETEv1p3
SME_F64F64 SME_FA64 SME_I16I64 MEC SME2 SME2p1 SME_F16F16 ABLE CHK D128 EBEP GCS ITE
LSE128 LVA3 SEBEP SVE2p1 SVE_B16B16 SYSINSTR128 SYSREG128 TRBE_EXT TRBE_MPAM"
pac9_config() { # FILE CONSTPACFIELD QARMA3
	q5=true
	[ "$3" = false ] || q5=false
	set -- "$1" "FEAT_CONSTPACFIELD:$2 FEAT_PACQARMA3:$3 FEAT_PACQARMA5:$q5 FEAT_PACIMP:false
FEAT_PAuth:true FEAT_PAuth2:true FEAT_FPAC:true FEAT_FPACCOMBINE:true FEAT_EPAC:false"
	for f in $pac9_later; do
		set -- "$1" "$2 FEAT_$f:false"
	done
	[ "$(grep -cx '\[registers.reset\]' "$ISLA/configs/armv9p4.toml")" = 1 ] ||
		fail "armv9p4.toml: not one [registers.reset] section (the Isla configuration changed?)"
	sed -e 's/^\(assembler\|objdump\|nm\|linker\) = .*/\1 = "true"/' \
		-e 's/^"\(v8Ap[2-5]\|v9Ap0\)_IMPLEMENTED" = false$/"\1_IMPLEMENTED" = true/' \
		"$ISLA/configs/armv9p4.toml" |
		awk -v set="$2" '
			BEGIN { n = split(set, s, /[ \n]+/); for (i = 1; i <= n; i++) if (s[i] != "") { split(s[i], kv, ":"); val[kv[1]] = kv[2] } }
			/^"FEAT_[A-Za-z0-9_]*_IMPLEMENTED" = / { f = $1; gsub(/"/, "", f); sub(/_IMPLEMENTED$/, "", f); if (f in val) next }
			/^\[registers\.reset\]$/ { for (f in val) printf "\"%s_IMPLEMENTED\" = %s\n", f, val[f]; print ""; }
			{ print }' >"$1"
	for l in v8Ap2:true v8Ap3:true v8Ap4:true v8Ap5:true v8Ap6:false v8Ap7:false v8Ap8:false \
		v8Ap9:false v9Ap0:true v9Ap1:false v9Ap2:false v9Ap3:false v9Ap4:false $2; do
		[ "$(grep -c "^\"${l%%:*}_IMPLEMENTED\" = " "$1")" = 1 ] &&
			grep -qx "\"${l%%:*}_IMPLEMENTED\" = ${l#*:}" "$1" ||
			fail "$1: ${l%%:*}_IMPLEMENTED is not set once to ${l#*:} (the Isla configuration changed?)"
	done
}
pac9_config "$out/armv9p4-pac.toml" false false
pac9_config "$out/armv9p4-pac-constpacfield.toml" true false
pac9_config "$out/armv9p4-pac-qarma3.toml" false true
# The Armv9.4-A model parsed once into Isla's serialized form (`.irx`),
# which loads in half the time of the text; the configuration and the
# `--abstract` and `--stop-at` options still apply when a trace loads it.
# Four traces are also made from the text, below, and must be the same.
"$preprocess" -A "$ir9" -C "$out/armv9p4-pac.toml" -o "$out/armv9p4" >"$out/armv9p4.irx.err" 2>&1 || {
	echo "isla: isla-preprocess failed: $(grep -v 'No primop' "$out/armv9p4.irx.err" | head -3)" >&2
	exit 2
}
ir9x=$out/armv9p4.irx
pac_tcr() { # VA: Linux's TCR_EL1 for that user VA size (T0SZ, T1SZ, granules, TBI0)
	case $1 in
	36) echo 0x00000035755CB51C ;;
	39) echo 0x00000035B5593519 ;;
	42) echo 0x00000035F5567516 ;;
	47) echo 0x000000357551B511 ;;
	48) echo 0x00000035B5503510 ;;
	52) echo 0x00000036F54C750C ;;
	esac
}
pac_name() { # MNEMONIC WORD: the trace name of the instruction
	case $1 in
	xpacd) echo "xpacd-x$(($2 & 31))" ;;
	*) echo "$1-x$(($2 & 31))-x$(($2 >> 5 & 31))" ;;
	esac
}
# Canonical `pacda x0, x1`, `autda x0, x1` and `xpacd x0`: the functions
# sign, auth and strip compute, which every site's determinism job
# compares against.
pac_words="pacda:$((0xdac10820)) autda:$((0xdac11820)) xpacd:$((0xdac147e0)) $pac_words"
par=$(nproc 2>/dev/null || echo 4)
pac_configs=
for v in "" p2- p2c-; do
	for c in el1-36 el1-39 el1-42 el1-47 el1-48 el1-52 vhe-36 vhe-39 vhe-42 vhe-47 vhe-48 vhe-52; do
		pac_configs="$pac_configs $v$c"
	done
done
pac_uniq=
for mw in $pac_words; do
	case " $pac_uniq " in *" $mw "*) ;; *) pac_uniq="$pac_uniq $mw" ;; esac
done
pac_words=$pac_uniq
# Each model's own arguments; the v9.4 registers are structs (`.bits`).
pac85_args="-R PSTATE.EL=0b00 -R PSTATE.nRW=0b0 -R SCR_EL3=0x0000000000030401 -R VBAR_EL1=undefined:%bv64 -R VBAR_EL2=undefined:%bv64"
pac94_args="-f isla_footprint_no_init -R PSTATE.EL=0b00 -R PSTATE.nRW=0b0 -R SCR_EL3.bits=0x0000000000030401 -R VBAR_EL1=undefined:%bv64 -R VBAR_EL2=undefined:%bv64 -R _EDSCR.bits=0x00000002"
pac_model() { # CONFIG: sets m (85 or 94), mir, mcf, margs, step, regs, sctlr, el, va
	case $1 in
	p2c-*) m=94 b=.bits mir=$ir9x mcf=$out/armv9p4-pac-constpacfield.toml margs=$pac94_args step=pc-step ;;
	p2-*) m=94 b=.bits mir=$ir9x mcf=$out/armv9p4-pac.toml margs=$pac94_args step=pc-step ;;
	*) m=85 b= mir=$ir mcf=$pac_config margs=$pac85_args step= ;;
	esac
	case $1 in
	*el1-*) el=EL1 hcr=0x0100030080000000 ;;
	*) el=EL2 hcr=0x0000000488000000 ;;
	esac
	va=${1##*-}
	regs="-R HCR_EL2$b=$hcr -R SCTLR_$el$b=undefined:%bv64 -R TCR_$el$b=$(pac_tcr $va)"
	sctlr=SCTLR_$el$b
}
# Each trace is written whole or not at all (a killed isla-footprint
# leaves only the .part file), and every run makes its own.
pac_footprint() { # NAME IR CONFIG ISLA-ARGS...: run in the background by the caller
	t=$1 tir=$2 tcf=$3
	shift 3
	rm -f "${out:?}/${t:?}.trace" "${out:?}/${t:?}.failed"
	if "$footprint" -A "$tir" -C "$tcf" -s "$@" >"$out/$t.trace.part" 2>"$out/$t.err"; then
		mv "$out/$t.trace.part" "$out/$t.trace"
	else
		echo "isla-footprint $t: $(grep -v 'No primop' "$out/$t.err" | head -3)" >"$out/$t.failed"
	fi
}
# At most `par` background tasks at once: a task takes a token from the
# pipe on fd 3 before it starts and puts it back when it ends.
mkfifo "$out/tokens.fifo"
exec 3<>"$out/tokens.fifo"
rm -f "${out:?}/tokens.fifo"
for _ in $(seq "$par"); do echo >&3; done
pac_bg() { # NAME IR CONFIG ISLA-ARGS...: pac_footprint in the background
	read -r _ <&3
	(
		pac_footprint "$@"
		echo >&3
	) &
}
for c in $pac_configs; do
	pac_model $c
	for mw in $pac_words; do
		stop=
		[ $m = 85 ] || [ ${mw%%:*} != autda ] || stop="--stop-at AArch64_PACFailException"
		pac_bg "pac-$c-$(pac_name ${mw%%:*} ${mw#*:})" "$mir" "$mcf" $margs --abstract ComputePAC $stop \
			-R _PC=undefined:%bv64 $regs -x -i "$(le ${mw#*:})"
	done
	# The FPAC exception entry, on `autda x0, x1` run through.
	[ $m = 85 ] || pac_bg "pac-$c-autda-fault" "$mir" "$mcf" $margs --abstract ComputePAC \
		-R _PC=undefined:%bv64 $regs -x -i "$(le $((0xdac11820)))"
done
# Traces from the model's text, for the comparison with the serialized
# model below: one for each configuration file (`autda x0, x1` stopping at
# the FPAC call; `pacda x0, x1` with FEAT_CONSTPACFIELD; the first QARMA3
# purity spot check, in the loop below) and the FPAC exception entry under
# VHE. And `udf #0` (word 0), which the model's footprint function aborts
# without a trace event, for a negative control.
pac_model p2-el1-48
pac_bg pac-p2-el1-48-autda-x0-x1-text "$ir9" "$mcf" $margs --abstract ComputePAC \
	--stop-at AArch64_PACFailException -R _PC=undefined:%bv64 $regs -x -i "$(le $((0xdac11820)))"
pac_bg pac-p2-el1-48-udf "$mir" "$mcf" $margs -R _PC=undefined:%bv64 $regs -x -i "$(le 0)"
pac_model p2c-el1-48
pac_bg pac-p2c-el1-48-pacda-x0-x1-text "$ir9" "$mcf" $margs --abstract ComputePAC \
	-R _PC=undefined:%bv64 $regs -x -i "$(le $((0xdac10820)))"
pac_model p2-vhe-48
pac_bg pac-p2-vhe-48-autda-fault-text "$ir9" "$mcf" $margs --abstract ComputePAC \
	-R _PC=undefined:%bv64 $regs -x -i "$(le $((0xdac11820)))"
# The premise of `abstract ComputePAC`, that the model's PAC algorithm is a
# function of its arguments, spot-checked: `pacda x0, x1` with ComputePAC
# unfolded on concrete operands, key and PC (kernel at EL1, 48-bit VA,
# EnDA set), for eight inputs, in the v8.5 model and in the v9.4 one with
# QARMA5 and with QARMA3. `pac-pure.job.in` runs two copies of each trace,
# each with its own undefined values and initial RC.
pac_inputs="0000123456789abc:0000000000001000:84be85ce9804e94b:ec2802d4e0a488e9
00007fffffffeff0:0000ffff00001111:0123456789abcdef:fedcba9876543210
0000000000000008:0000000000000000:0000000000000000:0000000000000000
0000aaaaaaaaaaa8:5555555555555555:ffffffffffffffff:0000000000000000
00000f0f0f0f0f00:7f12345678900000:13198a2e03707344:a4093822299f31d0
0000400000000000:deadbeefcafef00d:243f6a8885a308d3:082efa98ec4e6c89
00001b2c3d4e5f60:0000000000002000:452821e638d01377:be5466cf34e90c6c
0000ffffffffffff:ffffffffffffffff:c0ac29b7c97c50dd:3f84d5b5b5470917"
pac_pures="85 94q5 94q3"
k=0
for in4 in $pac_inputs; do
	k=$((k + 1))
	p=${in4%%:*} r=${in4#*:}
	mod=${r%%:*} r=${r#*:}
	hi=${r%%:*} lo=${r#*:}
	set -- -R R0=0x$p -R R1=0x$mod -R APDAKeyHi_EL1=0x$hi -R APDAKeyLo_EL1=0x$lo -x -i "$(le $((0xdac10820)))"
	pac_bg "pac-pure85-$k" "$ir" "$pac_config" $pac85_args -R _PC=0x0000000000400000 \
		-R HCR_EL2=0x0100030080000000 -R SCTLR_EL1=0x0000000008000000 -R TCR_EL1="$(pac_tcr 48)" "$@"
	for q in q5:armv9p4-pac q3:armv9p4-pac-qarma3; do
		pac_bg "pac-pure94${q%%:*}-$k" "$ir9x" "$out/${q#*:}.toml" $pac94_args -R _PC=0x0000000000400000 \
			-R HCR_EL2.bits=0x0100030080000000 -R SCTLR_EL1.bits=0x0000000008000000 \
			-R TCR_EL1.bits="$(pac_tcr 48)" "$@"
	done
	[ $k -ne 1 ] || pac_bg "pac-pure94q3-1-text" "$ir9" "$out/armv9p4-pac-qarma3.toml" $pac94_args \
		-R _PC=0x0000000000400000 -R HCR_EL2.bits=0x0100030080000000 -R SCTLR_EL1.bits=0x0000000008000000 \
		-R TCR_EL1.bits="$(pac_tcr 48)" "$@"
done
wait
for f in "$out"/pac-*.failed; do
	[ -e "$f" ] && fail "$(cat "$f")"
done
# The serialized model gives the traces the text gives, up to the order of
# the paths (Isla collects them in parallel), of a struct's fields, and the
# type arguments in a serialized model's union constructor names
# (`isla-check --same`).
for t in p2-el1-48-autda-x0-x1 p2c-el1-48-pacda-x0-x1 p2-vhe-48-autda-fault pure94q3-1; do
	if "$out/isla-check" --same "$out/pac-$t.trace" "$out/pac-$t-text.trace" >"$out/pac-$t-text.log" 2>&1; then
		echo "armv9p4.irx: the trace pac-$t is the text model's"
	else
		fail "armv9p4.irx: the trace pac-$t differs from the text model's"
	fi
done
pac_jobs=
for c in $pac_configs; do
	pac_model $c
	for mw in $pac_words; do
		mn=${mw%%:*} w=${mw#*:}
		t=pac-$c-$(pac_name $mn $w)
		d=R$((w & 31)) n=R$((w >> 5 & 31))
		# In the v9.4 model an `autda` may end at the FPAC call instead.
		# Each insn line names the register the instruction writes when it
		# completes (checked under `pc-step`).
		kind=fallthrough stopl=
		if [ $m = 94 ] && [ $mn = autda ]; then
			kind=stop stopl="stop AArch64_PACFailException #b10"
		fi
		job=$out/$t.job
		{
			echo "insn $t.trace $kind $d"
			echo "$step"
			echo "$stopl"
			sed "s/@D@/$d/g" "$here/pac-footprint.job.in"
		} >"$job"
		pac_jobs="$pac_jobs $job"
		job=$out/$t-det.job
		if [ "$mn" = xpacd ]; then
			{
				echo "copy a"
				echo "insn pac-$c-xpacd-x0.trace fallthrough R0"
				echo "copy b"
				echo "insn $t.trace fallthrough $d"
				echo "$step"
				sed "s/@D@/$d/g" "$here/pac-strip-det.job.in"
			} >"$job"
		else
			{
				echo "copy a"
				echo "insn pac-$c-$mn-x0-x1.trace $kind R0"
				echo "copy b"
				echo "insn $t.trace $kind $d"
				echo "$step"
				echo "$stopl"
				# Both fault or neither: whether it faults is a function of
				# the same inputs as the result.
				[ $kind = fallthrough ] || echo "assert (= a.stop.1 b.stop.1)"
				sed -e "s/@OP@/$mn/g" -e "s/@D@/$d/g" -e "s/@N@/$n/g" -e "s/@SCTLR@/$sctlr/g" "$here/pac-det.job.in"
			} >"$job"
		fi
		pac_jobs="$pac_jobs $job"
	done
	# The round trip, `autda` as `fallthrough`: in the v9.4 model the FPAC
	# fault is among the paths the job must show infeasible.
	job=$out/pac-$c-rt.job
	{
		echo "insn pac-$c-pacda-x0-x1.trace fallthrough R0"
		echo "insn pac-$c-autda-x0-x1.trace fallthrough R0"
		echo "$step"
		sed -e "s/@VA@/$va/g" -e "s/@ZEROS@/#b$(printf "%0$((64 - va))d" 0)/" "$here/pac-rt.job.in"
	} >"$job"
	pac_jobs="$pac_jobs $job"
	# What `pacda x0, x1` and `autda x0, x1` compute in each model, in terms
	# of the abstract ComputePAC, with EnDA set (pac-spec-*.job.in) and
	# clear (pac-enda-off.job.in).
	for s in sign:pacda auth:autda; do
		tr=pac-$c-${s#*:}-x0-x1
		kind=fallthrough stopl=
		if [ $m = 94 ] && [ $s = auth:autda ]; then
			kind=stop stopl="stop AArch64_PACFailException #b10"
		fi
		job=$out/$tr-spec.job
		{
			echo "insn $tr.trace $kind R0"
			echo "$step"
			echo "$stopl"
			sed -e "s/@SCTLR@/$sctlr/g" -e "s/@B@/$va/g" -e "s/@B1@/$((va - 1))/g" \
				-e "s/@N@/$((55 - va))/g" -e "s/@N1@/$((56 - va))/g" "$here/pac-spec-${s%%:*}$m.job.in"
		} >"$job"
		pac_jobs="$pac_jobs $job"
		job=$out/$tr-off.job
		{
			echo "insn $tr.trace fallthrough R0"
			echo "$step"
			sed "s/@SCTLR@/$sctlr/g" "$here/pac-enda-off.job.in"
		} >"$job"
		pac_jobs="$pac_jobs $job"
	done
	if [ $m = 94 ]; then
		case $el in EL1) lvl='#b01' ;; *) lvl='#b10' ;; esac
		job=$out/pac-$c-fault.job
		{
			echo "insn pac-$c-autda-fault.trace fault R0"
			sed -e "s/@EL@/$el/g" -e "s/@LEVEL@/$lvl/g" "$here/pac-fault.job.in"
		} >"$job"
		pac_jobs="$pac_jobs $job"
	fi
done
for pm in $pac_pures; do
	k=0
	for in4 in $pac_inputs; do
		k=$((k + 1))
		job=$out/pac-pure$pm-$k.job
		{
			echo "copy a"
			echo "insn pac-pure$pm-$k.trace fallthrough R0"
			echo "copy b"
			echo "insn pac-pure$pm-$k.trace fallthrough R0"
			[ $pm = 85 ] || echo "pc-step"
			sed "s/@P@/#x${in4%%:*}/g" "$here/pac-pure.job.in"
		} >"$job"
		pac_jobs="$pac_jobs $job"
	done
done
for job in $pac_jobs; do
	read -r _ <&3
	(
		rc=0
		"$out/isla-check" "$job" >"$job.log" 2>&1 || rc=$?
		echo $rc >"$job.rc"
		echo >&3
	) &
done
wait
npac=0
for job in $pac_jobs; do
	npac=$((npac + 1))
	[ "$(cat "$job.rc")" = 0 ] || fail "isla-check $job: $(grep -m 1 FAIL "$job.log" || tail -n 1 "$job.log")"
done
echo "PAC: $npac jobs: $(echo $pac_words | wc -w) instructions in each of $(echo $pac_configs | wc -w) configurations (12 Armv8.5-A, 24 Armv9.4-A), and $((k * 3)) purity spot checks"

# 7. The aarch64 halves of the mixed seams (verus/ISLA.md, "Triage");
# their x86_64 halves are step 12. Each compiled shape is required
# exactly, so a change in the code fails here until its job is revisited.
fn_body() { # NAME-PART: the words and relocations of the one function whose symbol contains NAME-PART
	awk -v fn="$1" '
		/^[0-9a-f]+ <[^>]*>:$/ { infn = index($0, fn) > 0; if (infn) nfn++; next }
		/^$/ { infn = 0 }
		infn && /^ +[0-9a-f]+: [0-9a-f]+ / { print $2; next }
		infn && /R_AARCH64_/ { print $2, $3 }
		END { if (nfn != 1) print "functions:", nfn + 0 }' "$out/dis.txt" | tr '\n' ' '
}
# get_new_handler_addr, throw_bad_alloc_addr, malloc_conf_addr,
# je_malloc_conf_addr: `adrp x0, :got:SYM; ldr x0, [x0, :got_lo12:SYM];
# ret`, one job for the four (the relocated immediates are symbolic).
for f in 20get_new_handler_addr:_ZSt15get_new_handlerv \
	20throw_bad_alloc_addr:_ZSt17__throw_bad_allocv 16malloc_conf_addr:malloc_conf \
	19je_malloc_conf_addr:je_malloc_conf; do
	s=${f#*:}
	want="90000000 R_AARCH64_ADR_GOT_PAGE $s f9400000 R_AARCH64_LD64_GOT_LO12_NC $s d65f03c0 "
	body=$(fn_body "${f%%:*}")
	if [ "$body" = "$want" ]; then
		echo "GOT function ${f%%:*}: the job's shape"
	else
		fail "GOT function ${f%%:*}: not the job's shape: $body"
	fi
done
trace adrp-x0 --partial -i "1 immlo:2 10000 immhi:19 00000"
trace ldr-x0 --partial -i "1111100101 imm12:12 00000 00000"
trace ret -x -i "$(le 0xd65f03c0)"
cp "$here/got-fn.job.in" "$out/got-fn.job"
check "$out/got-fn.job"
# rseq::symbols: three GOT loads, the tuple's stores through x0, ret.
want=
for r in 8:__rseq_offset 9:__rseq_size 10:__rseq_flags; do
	a=${r%%:*}
	want="$want$(printf %08x $((0x90000000 | a))) R_AARCH64_ADR_GOT_PAGE ${r#*:} "
	want="$want$(printf %08x $((0xf9400000 | a << 5 | a))) R_AARCH64_LD64_GOT_LO12_NC ${r#*:} "
	trace "adrp-x$a" --partial -i "1 immlo:2 10000 immhi:19 $(bin5 $a)"
	trace "ldr-x$a" --partial -i "1111100101 imm12:12 $(bin5 $a) $(bin5 $a)"
done
want="${want}a9002408 f900080a d65f03c0 "
body=$(fn_body 4rseq7symbols)
if [ "$body" = "$want" ]; then
	echo "rseq::symbols: the job's shape"
else
	fail "rseq::symbols: not the job's shape: $body"
fi
trace stp-x8-x9-x0 -x -i "$(le 0xa9002408)"
trace str-x10-x0-16 -x -i "$(le 0xf900080a)"
cp "$here/rseq-symbols.job.in" "$out/rseq-symbols.job"
check "$out/rseq-symbols.job"
# rseq::thread_pointer: every `mrs xN, tpidr_el0` in the code (its own
# sites and the TLS sequences'), one job per register.
grep -E "^ +[0-9a-f]+: d53bd04[0-9a-f] +${tab}mrs${tab}x[0-9]+, TPIDR_EL0$" "$out/dis.txt" |
	awk '{ print $2 }' | sort | uniq -c >"$out/mrs-sites.txt"
[ -s "$out/mrs-sites.txt" ] || fail "no mrs of tpidr_el0 in the aarch64 code"
nmrs=$(grep -cE "${tab}mrs${tab}.*TPIDR_EL0$" "$out/dis.txt" || true)
[ "$nmrs" -eq "$(awk '{ s += $1 } END { print s + 0 }' "$out/mrs-sites.txt")" ] ||
	fail "an mrs of tpidr_el0 into a register the jobs do not cover"
while read -r _count w; do
	r=$((0x$w & 31))
	[ $r -ne 31 ] || { fail "mrs xzr, tpidr_el0"; continue; }
	trace "mrs-x$r" -x -i "$(le $((0x$w)))"
	{
		echo "insn mrs-x$r.trace fallthrough"
		sed "s/@R@/R$r/g" "$here/mrs-tp.job.in"
	} >"$out/mrs-tp-x$r.job"
	check "$out/mrs-tp-x$r.job"
done <"$out/mrs-sites.txt"
echo "mrs tpidr_el0 instances: $nmrs"
# sys::trap: every brk in the code is `brk #0x1` (LLVM's own traps are the
# same instruction), at EL1 and at EL2 (VHE).
nbrk=$(grep -cE "^ +[0-9a-f]+: d4200020 +${tab}brk${tab}#0x1$" "$out/dis.txt" || true)
nobrk=$(grep -E "${tab}brk${tab}" "$out/dis.txt" | grep -vcE ": d4200020 +${tab}brk${tab}#0x1$" || true)
echo "brk #0x1 instances: $nbrk; other brk: $nobrk"
[ "$nbrk" -gt 0 ] || fail "no brk #0x1 in the aarch64 code"
[ "$nobrk" -eq 0 ] || fail "$nobrk brk other than brk #0x1"
# MDCR_EL2.TDE clear, as Linux leaves it for its own EL0 (with TDE set, a
# hypervisor debugging its guest, the breakpoint goes to EL2 instead).
trace brk1 -x -i "$(le 0xd4200020)" -R MDCR_EL2=0x00000000
trace brk1-vhe -x -i "$(le 0xd4200020)" -R HCR_EL2=0x0000000488000000
cp "$here/brk1.job" "$here/brk1-vhe.job" "$out/"
check "$out/brk1.job"
check "$out/brk1-vhe.job"
# tls::set: the TLS store sites (tls::get's are the load jobs of section 5).
grep ' store ' "$out/tls-sites.txt" >"$out/tls-stores.txt" || fail "no TLS slot store in the aarch64 code"
while read -r _count _kind w1 w2 w3 w4; do
	a=$((0x$w1 & 31)) b=$((0x$w3 & 31)) c=$((0x$w4 & 31))
	ok=1
	[ $((0x$w1 & 0x9f00001f)) -eq $((0x90000000 | a)) ] || ok=0              # adrp xA, 0
	[ $((0x$w2)) -eq $((0xf9400000 | a << 5 | a)) ] || ok=0                    # ldr xA, [xA]
	[ $((0x$w3 & 0xffffffe0)) -eq $((0xd53bd040)) ] || ok=0                    # mrs xB, tpidr_el0
	[ $((0x$w4)) -eq $((0xf8206800 | a << 16 | b << 5 | c)) ] || ok=0          # str xC, [xB, xA]
	[ $a -ne $b ] && [ $a -ne $c ] && [ $b -ne $c ] && [ $c -ne 31 ] || ok=0
	if [ $ok -eq 0 ]; then
		fail "TLS store $w1 $w2 $w3 $w4: not the template's shape"
		continue
	fi
	trace "adrp-x$a" --partial -i "1 immlo:2 10000 immhi:19 $(bin5 $a)"
	trace "ldr-x$a" --partial -i "1111100101 imm12:12 $(bin5 $a) $(bin5 $a)"
	trace "mrs-x$b" -x -i "$(le $((0x$w3)))"
	trace "str-x$c-x$b-x$a" -x -i "$(le $((0x$w4)))"
	job=$out/tls-store-x$a-x$b-x$c.job
	{
		echo "insn adrp-x$a.trace fallthrough"
		echo "insn ldr-x$a.trace fallthrough"
		echo "insn mrs-x$b.trace fallthrough"
		echo "insn str-x$c-x$b-x$a.trace fallthrough"
		echo "writes R$a R$b"
		sed -e "s/@A@/R$a/g" -e "s/@B@/R$b/g" -e "s/@C@/R$c/g" "$here/tls-store.job.in"
	} >"$job"
	check "$job"
done <"$out/tls-stores.txt"

# 8. Negative controls: jobs that must fail, so a checker that accepts
# everything fails here. On the real traces: a wrong syndrome, a wrong
# exception level, a memory read where there is none, `svc` expected to
# fall through, the GOT load one entry off, the TLS load without the LDR
# relocation's constraint or without the slot's alignment, and `add x0,
# x1, #3` claimed to write nothing. On synthetic traces (controls/): a
# read of a register an earlier write left inexpressible, a write through
# a field accessor, a write of PSTATE.N under `flags-preserved`, and a
# `fallthrough` instruction continuing at PC + 8.
refute() { # NAME JOB-TEXT [REASON]
	printf '%s\n' "$2" >"$out/neg-$1.job"
	# Rejected means the checker judged a combination and failed it (a
	# `FAIL` line, naming REASON when given), not that it could not run
	# the job (a missing trace or an empty job from a `sed` on a file that
	# is not there).
	if "$out/isla-check" "$out/neg-$1.job" >"$out/neg-$1.log" 2>&1; then
		fail "negative control $1 passed"
	elif ! grep '^FAIL' "$out/neg-$1.log" | grep -qF -- "${3:-FAIL}"; then
		fail "negative control $1 did not fail for its reason${3:+ ($3)}: $(tail -n 1 "$out/neg-$1.log")"
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
refute pc-skip "insn $ctl/skip.trace fallthrough"
# PAC (section 6), on one configuration's jobs: the round trip without the
# VA bound, determinism without the modifier or either half of the key,
# or with SCTLR bit 26 equated in place of EnDA (bit 27), a job that does
# not name the abstract function, and a purity spot check claiming the
# PAC left the pointer unchanged.
pac=$out/pac-el1-48
refute pac-rt-va "$(grep -v '^assume (= ((_ extract 63 ' "$pac-rt.job")"
refute pac-det-mod "$(grep -v '^assume (= a.init.R1 ' "$pac-pacda-x0-x1-det.job")"
refute pac-det-keylo "$(grep -v '^assume (= a.init.APDAKeyLo_EL1 ' "$pac-pacda-x0-x1-det.job")"
refute pac-det-keyhi "$(grep -v '^assume (= a.init.APDAKeyHi_EL1 ' "$pac-pacda-x0-x1-det.job")"
refute pac-det-enda "$(sed 's/extract 27 27/extract 26 26/g' "$pac-autda-x0-x1-det.job")"
refute pac-abstract "$(grep -v '^abstract ' "$pac-pacda-x0-x1.job")"
refute pac-pure-id "$(sed 's/^assert (not (= a.final.R0 \(.*\)))$/assert (= a.final.R0 \1)/' "$out/pac-pure85-1.job")"
# The v8.5 model's PAC replacement claimed without its bit-54 inversion
# for a non-canonical pointer, and its failed `autda` claimed to return
# another error code.
refute pac-spec-flip "$(sed 's/(bvnot ((_ extract 54 54)/(bvneg ((_ extract 54 54)/' "$pac-pacda-x0-x1-spec.job")"
refute pac-spec-ec "$(sed 's/ #b01 / #b00 /' "$pac-autda-x0-x1-spec.job")"
# The same on an Armv9.4-A configuration, and what is new there:
# determinism of `autda` without the modifier, the PAuth2 XOR claimed to
# be an OR, `autda` claimed never to fault (its footprint as
# `fallthrough`, or with `not stop.1`), the fault's syndrome claimed to be
# 0b11 (key B), the FPAC exception entry with another syndrome, return
# address or EL, and a v9.4 job without the model's PC update (`pc-step`).
pac=$out/pac-p2-el1-48
refute pac9-rt-va "$(grep -v '^assume (= ((_ extract 63 ' "$pac-rt.job")"
refute pac9-det-mod "$(grep -v '^assume (= a.init.R1 ' "$pac-pacda-x0-x1-det.job")"
refute pac9-det-keylo "$(grep -v '^assume (= a.init.APDAKeyLo_EL1 ' "$pac-pacda-x0-x1-det.job")"
refute pac9-det-keyhi "$(grep -v '^assume (= a.init.APDAKeyHi_EL1 ' "$pac-pacda-x0-x1-det.job")"
refute pac9-det-enda "$(sed 's/extract 27 27/extract 26 26/g' "$pac-autda-x0-x1-det.job")"
refute pac9-det-mod-auth "$(grep -v '^assume (= a.init.R1 ' "$pac-autda-x0-x1-det.job")"
refute pac9-spec-xor "$(sed 's/(bvxor /(bvor /' "$pac-pacda-x0-x1-spec.job")"
refute pac9-nofault "$(sed -e '1s/ stop R0$/ fallthrough R0/' -e '/^stop /d' "$pac-autda-x0-x1.job")"
refute pac9-nostop "$(printf '%s\nassert (not stop.1)' "$(cat "$pac-autda-x0-x1.job")")"
refute pac9-stop-key "$(sed 's/^stop AArch64_PACFailException #b10$/stop AArch64_PACFailException #b11/' "$pac-autda-x0-x1.job")"
refute pac9-fault-esr "$(sed 's/#x0000000072000002/#x0000000072000003/' "$pac-fault.job")"
refute pac9-fault-elr "$(sed 's/^assert-exc (= final.ELR_EL1 init._PC)$/assert-exc (= final.ELR_EL1 (bvadd init._PC #x0000000000000004))/' "$pac-fault.job")"
refute pac9-fault-el "$(sed 's/^assert-exc (= final.PSTATE.EL #b01)$/assert-exc (= final.PSTATE.EL #b10)/' "$pac-fault.job")"
refute pac9-no-step "$(grep -v '^pc-step$' "$pac-pacda-x0-x1.job")"
# `udf #0`, which the model aborts as UNDEFINED and its footprint function
# swallows, claimed to complete: its trace writes nothing, so under
# `pc-step` it must fail by not writing the register it names.
refute pac9-udf "$(printf 'insn pac-p2-el1-48-udf.trace fallthrough R0\npc-step\nwrites R0')"
# On synthetic traces: a write before a `stop` call, a `stop` call that
# is not the last event, a user register written on a `fault` exception
# path, an exception path with no `assert-exc`, and under `pc-step` a PC
# write without `__BranchTaken`.
refute stop-write "$(printf 'insn %s stop\nstop Fault #b10\nwrites R3' "$ctl/stop-write.trace")"
refute stop-mid "$(printf 'insn %s stop\nstop Fault #b10' "$ctl/stop-mid.trace")"
refute exc-user "$(printf 'insn %s fault R0\npc-step\nwrites R0\nassert-exc true' "$ctl/exc-user.trace")"
refute exc-unchecked "$(printf 'insn %s fault R0\npc-step' "$ctl/exc.trace")"
refute pc-nobranch "$(printf 'insn %s fallthrough R0\npc-step' "$ctl/pc-nobranch.trace")"
# `isla-check --same` on two different traces (`pacda` and `autda`).
if "$out/isla-check" --same "$pac-pacda-x0-x1.trace" "$pac-autda-x0-x1.trace" >"$out/neg-same.log" 2>&1; then
	fail "negative control same passed"
else
	echo "negative control same: rejected, as it should be"
fi
# Section 7: the GOT function's load one entry off and its `ret` claimed
# to fall through, rseq::symbols storing the first value twice, the TLS
# store storing the offset, `brk` with the wrong syndrome or claimed to
# fall through, and `mrs` claimed to write nothing.
refute got-fn-off "$(sed 's/^assert (= mem.1.addr got)$/assert (= mem.1.addr (bvadd got #x0000000000000008))/' "$out/got-fn.job")"
refute got-fn-falls "$(sed 's/^assert (= final._PC init.R30)$/assert (= final._PC (bvadd init._PC #x000000000000000c))/' "$out/got-fn.job")"
refute rseq-store "$(sed 's/^assert (= wmem.2.val mem.2.val)$/assert (= wmem.2.val mem.1.val)/' "$out/rseq-symbols.job")"
set -- $(head -n 1 "$out/tls-stores.txt")
a=$((0x$3 & 31)) b=$((0x$5 & 31)) c=$((0x$6 & 31))
refute tls-store-val "$(sed "s/^assert (= wmem.1.val init.R$c)$/assert (= wmem.1.val mem.1.val)/" "$out/tls-store-x$a-x$b-x$c.job")"
refute tls-store-nowrite "$(sed 's/^mem-writes 1$/no-mem-write/' "$out/tls-store-x$a-x$b-x$c.job")"
refute brk-esr "$(sed 's/#xf2000001/#xf2000000/' "$out/brk1.job")"
refute brk-falls "insn brk1.trace fallthrough"
set -- $(head -n 1 "$out/mrs-sites.txt")
refute mrs-writes "insn mrs-x$((0x$2 & 31)).trace fallthrough"

# 9. The x86_64 code, extracted as in step 2.
rm -rf "$out/xobj" && mkdir -p "$out/xobj"
members=$("$llvm/llvm-ar" t "$xlib" | grep '^challoc-.*\.o$')
[ -n "$members" ] || { echo "isla: no challoc objects in $xlib" >&2; exit 2; }
(cd "$out/xobj" && "$llvm/llvm-ar" x "$xlib" $members)
for m in $members; do
	"$llvm/llvm-objdump" -dr --no-print-imm-hex "$out/xobj/$m"
done >"$out/dis-x86.txt"

# Isla's view of one x86 instruction: the model (verus/isla/x86/) in its
# system view, every register symbolic but the mode flags x86.toml fixes.
# The entry takes the instruction as 16 bytes; le128 spells them for
# --partial (most significant first), padded with 0x90. `--pessimistic`
# makes a Sail assertion that can fail an error instead of a path
# condition Isla assumes, so no model assertion is taken on trust.
xtrace() { # NAME BYTE...
	name=$1
	shift
	case " $traced " in *" $name "*) return 0 ;; esac
	traced="$traced $name"
	"$footprint" -A "$xir" -C "$here/x86/x86.toml" -s --pessimistic --partial -i "$(le128 "$@")" \
		>"$out/$name.trace" 2>"$out/$name.err" || {
		fail "isla-footprint $name: $(grep -v 'No primop' "$out/$name.err" | head -3)"
		: >"$out/$name.trace"
	}
}
le128() { # BYTE...: hex bytes in memory order; D32 is 4 bytes of the symbolic field `disp`
	s= n=0
	for b in "$@"; do
		if [ "$b" = D32 ]; then
			s="disp:32 $s" n=$((n + 4))
			continue
		fi
		v=$((0x$b)) bits=
		for _ in 1 2 3 4 5 6 7 8; do bits=$((v & 1))$bits v=$((v >> 1)); done
		s="$bits $s" n=$((n + 1))
	done
	while [ $n -lt 16 ]; do s="10010000 $s" n=$((n + 1)); done
	echo $s
}

# 10. `syscall` (sys::raw's x86_64 syscall0..syscall6): every instruction
# in the code that enters the kernel (syscall, sysenter, int) is
# `syscall`, encoding 0f 05, and Isla's trace of it.
nsys=$(grep -cE "^ +[0-9a-f]+: 0f 05 +${tab}syscall$" "$out/dis-x86.txt" || true)
nxother=$(grep -E "${tab}(syscall|sysenter|int[0-9a-z]*)(${tab}|$)" "$out/dis-x86.txt" |
	grep -vcE ": 0f 05 +${tab}syscall$" || true)
echo "syscall instances: $nsys; other syscall, sysenter or int: $nxother"
[ "$nsys" -gt 0 ] || fail "no syscall in the x86_64 code"
[ "$nxother" -eq 0 ] || fail "$nxother syscall/sysenter/int other than 0f 05 syscall"
xtrace syscall 0f 05
{
	cat "$here/x86/syscall.job"
	echo "insn syscall.trace jump"
} >"$out/syscall.job"
check "$out/syscall.job"

# 11. x86_64 negative controls, on the real trace, each failing for its
# own reason: the return address one byte off, R11 and the SS selector (a
# vector register's element) left out of `writes`, a memory read where
# there is none, rflags claimed preserved, SS's descriptor attributes
# without B, the job without EFER.SCE (the model's #UD path) or without
# the STAR bound (the model's own check failing), both caught as model
# faults through `isla_fault`, and the job without EFER.LMA or CS.L
# (paths outside 64-bit mode, some faulting, on which the asserts name
# values the paths leave undefined). A job asserting nothing
# about final.rip is refused outright.
sysjob=$out/syscall.job
refute sys-rcx "$(sed 's/^assert (= final.rcx (bvadd init.rip #x0000000000000002))$/assert (= final.rcx (bvadd init.rip #x0000000000000003))/' "$sysjob")" "assert (= final.rcx"
refute sys-r11 "$(sed 's/^writes rcx r11 /writes rcx /' "$sysjob")" "writes r11"
refute sys-ss "$(sed 's/^\(writes .*\) seg_visibles\.2 /\1 /' "$sysjob")" "writes seg_visibles.2"
refute sys-mem "$(sed 's/^mem-reads 0$/mem-reads 1/' "$sysjob")" "memory reads, expected"
refute sys-flags "$(sed 's/^no-mem-write$/no-mem-write\nflags-preserved/' "$sysjob")" "writes rflags.bits"
refute sys-ssattr "$(sed 's/#x0c93))$/#x0893))/' "$sysjob")" "assert (= final.seg_hidden_attrs.2"
refute sys-sce "$(grep -v '^assume (= ((_ extract 0 0) init.msrs.0) #b1)$' "$sysjob")" "a model fault"
refute sys-star "$(grep -v '^assume (bvult ((_ extract 47 32) init.msrs.4) #x7ff8)$' "$sysjob")" "a model fault"
refute sys-lma "$(grep -v '^assume (= ((_ extract 10 10) init.msrs.0) #b1)$' "$sysjob")" "final_rcx undefined"
refute sys-csl "$(grep -v '^assume (= ((_ extract 9 9) init.seg_hidden_attrs.1) #b1)$' "$sysjob")" "final_rcx undefined"
grep -v '^assert (= final.rip ' "$sysjob" >"$out/neg-sys-norip.job"
if "$out/isla-check" "$out/neg-sys-norip.job" >"$out/neg-sys-norip.log" 2>&1 ||
	! grep -q 'final.rip no assert names' "$out/neg-sys-norip.log"; then
	fail "negative control sys-norip: a job without a final.rip assert was not refused"
else
	echo "negative control sys-norip: refused, as it should be"
fi

# 12. The x86_64 halves of the mixed seams (verus/ISLA.md, "The x86_64
# mixed seams"): the GOT functions, rseq::symbols, the TLS slot load and
# store, the thread pointer load and `ud2`. Their data accesses need the
# processor's mode settled: from a fully symbolic state the model
# explores compatibility mode and the rest, and the runs do not finish.
# Three changes to the runs of steps 9-10 do it, all named in the records:
# - the entry calls Isla's `reset_registers` builtin after its cycle
#   marker, the hook through which Isla applies `--fun-assumption`
#   (Isla's Arm models call it at reset). The x86 entry
#   (x86/isla_entry.sail) has no such call, so the IR is patched here:
#   one declaration and one call in `isla_footprint`, its later jump
#   targets moved past the two new instructions. The patched model must
#   give `syscall` the very traces of the unpatched one (`isla-check
#   --same`), so the call changes nothing by itself;
# - `--fun-assumption 'n64_bit_modep((_ unit)) => true'`: the model's
#   64-bit mode test (IA32_EFER.LMA and CS.L, a pure read) answers true
#   without running, and the jobs name it (`fun n64_bit_modep true`).
#   Linux runs 64-bit user code in 64-bit mode, which the `syscall` job
#   assumes too (as EFER.LMA and CS.L), and it must still pass with the
#   assumption;
# - the model's application view (`app_view` true): linear addresses are
#   physical, no page walk, as Linux user code sees mapped memory (the
#   GOT, the stack, the TLS block; a page fault is the kernel's and the
#   instruction restarts). The system view's walks of symbolic page
#   tables did not finish.
xjobs=
xpir=$out/x86-reset.ir
awk '
	/^val zisla_cycle_count = "cycle_count" : \(%unit\) ->  %unit$/ { print; print ""; print "val zisla_reset_registers = \"reset_registers\" : (%unit) ->  %unit"; decl++; next }
	/^fn zisla_footprint\(zopcode\) \{$/ { inf = 1; n = 0 }
	inf && /^}$/ { inf = 0 }
	inf && /^  / {
		# Jump targets count the body instructions from 0.
		if (match($0, /goto [0-9]+/)) {
			t = substr($0, RSTART + 5, RLENGTH - 5) + 0
			if (t > cyc) $0 = substr($0, 1, RSTART + 4) (t + 2) substr($0, RSTART + RLENGTH)
		}
		print
		if ($0 ~ /^  zz[0-9]+ = zisla_cycle_count\(\(\)\)/) { cyc = n; ncyc++; print "  zz4reset : %unit;"; print "  zz4reset = zisla_reset_registers(());" }
		n++
		next
	}
	{ print }
	END { if (decl != 1 || ncyc != 1) exit 1 }' "$xir" >"$xpir" ||
	fail "x86 entry: not the shape run.sh patches (one cycle marker in isla_footprint)"
# A jump before the marker would be shifted wrongly by the awk above
# (its `cyc` is not yet set); the entry has none, and this checks it.
awk '/^fn zisla_footprint\(zopcode\) \{$/ { inf = 1; n = 0; next } inf && /^}$/ { exit }
	inf && /^  / { if (/zisla_cycle_count/) exit; if (/goto /) bad = 1; n++ }
	END { exit bad }' "$xir" || fail "x86 entry: a jump before the cycle marker"
xconf=$out/x86-app.toml
sed 's/^app_view = false$/app_view = true/' "$here/x86/x86.toml" >"$xconf"
[ "$(grep -c '^app_view = true$' "$xconf")" -eq 1 ] && [ "$(grep -c '^app_view' "$xconf")" -eq 1 ] ||
	fail "x86-app.toml: app_view not set exactly once"
"$footprint" -A "$xpir" -C "$here/x86/x86.toml" -s --pessimistic --partial -i "$(le128 0f 05)" \
	>"$out/syscall-reset.trace" 2>"$out/syscall-reset.err" || fail "isla-footprint syscall (patched entry)"
"$out/isla-check" --same "$out/syscall.trace" "$out/syscall-reset.trace" ||
	fail "the patched x86 entry changes the syscall traces"
# And the function assumption agrees with the bits it stands for: with
# it, in the system view, `syscall` still meets its job, which assumes
# EFER.LMA and CS.L outright.
"$footprint" -A "$xpir" -C "$here/x86/x86.toml" -s --pessimistic --partial \
	--fun-assumption 'n64_bit_modep((_ unit)) => true' -i "$(le128 0f 05)" \
	>"$out/syscall-pinned.trace" 2>"$out/syscall-pinned.err" || fail "isla-footprint syscall (64-bit mode assumed)"
{
	cat "$here/x86/syscall.job"
	echo "fun n64_bit_modep true"
	# The assumed bits are no longer read on every path: declare them, so
	# that the job's assumptions over them hold in every combination.
	printf 'decl init.msrs.0 64\ndecl init.msrs.4 64\ndecl init.msrs.5 64\ndecl init.seg_hidden_attrs.1 16\n'
	echo "insn syscall-pinned.trace jump"
} >"$out/syscall-pinned.job"
check "$out/syscall-pinned.job"
# Isla's view of one x86 instruction for these jobs: the patched entry,
# the application view, 64-bit mode. The arguments are bytes in memory
# order, `D32` a 4-byte displacement the linker fills (the symbolic
# opcode field `disp`); a `trap` job traces the model's x86_fault.
xptrace() { # NAME BYTE...: the trace x86-NAME.trace (apart from the Arm steps' names)
	name=x86-$1
	shift
	case " $traced " in *" $name "*) return 0 ;; esac
	traced="$traced $name"
	set -- --partial -i "$(le128 "$@")"
	case $name in x86-ud2) set -- --trace-function 'x86_fault<u>' "$@" ;; esac
	"$footprint" -A "$xpir" -C "$xconf" -s --pessimistic \
		--fun-assumption 'n64_bit_modep((_ unit)) => true' "$@" \
		>"$out/$name.trace" 2>"$out/$name.err" || {
		fail "isla-footprint $name: $(grep -v 'No primop' "$out/$name.err" | head -3)"
		: >"$out/$name.trace"
	}
}
xfn_body() { # NAME-PART: the bytes (one word per instruction) and relocations of the one function whose symbol contains NAME-PART
	awk -v fn="$1" '
		/^[0-9a-f]+ <[^>]*>:$/ { infn = index($0, fn) > 0; if (infn) nfn++; next }
		/^$/ { infn = 0 }
		infn && /^ +[0-9a-f]+: / {
			s = $0; sub(/^ +[0-9a-f]+: /, "", s); sub(/\t.*/, "", s); gsub(/ /, "", s)
			print s; next
		}
		infn && /R_X86_64_/ { print $2, $3 }
		END { if (nfn != 1) print "functions:", nfn + 0 }' "$out/dis-x86.txt" | tr '\n' ' '
}
# get_new_handler_addr, throw_bad_alloc_addr, malloc_conf_addr,
# je_malloc_conf_addr: `mov rax, [rip + SYM@GOTPCREL]; ret`, one job for
# the four.
for f in 20get_new_handler_addr:_ZSt15get_new_handlerv \
	20throw_bad_alloc_addr:_ZSt17__throw_bad_allocv 16malloc_conf_addr:malloc_conf \
	19je_malloc_conf_addr:je_malloc_conf; do
	want="488b0500000000 R_X86_64_GOTPCREL ${f#*:}-0x4 c3 "
	body=$(xfn_body "${f%%:*}")
	if [ "$body" = "$want" ]; then
		echo "x86 GOT function ${f%%:*}: the job's shape"
	else
		fail "x86 GOT function ${f%%:*}: not the job's shape: $body"
	fi
done
xptrace got-rax 48 8b 05 D32
xptrace ret c3
cp "$here/x86/got-fn.job" "$out/got-fn-x86.job"
xjobs="$xjobs $out/got-fn-x86.job"
# rseq::symbols: three GOT loads, the tuple's stores through rdi, ret.
want="488b0500000000 R_X86_64_GOTPCREL __rseq_offset-0x4 488b0d00000000 R_X86_64_GOTPCREL __rseq_size-0x4 "
want="${want}488b1500000000 R_X86_64_GOTPCREL __rseq_flags-0x4 488907 48894f08 48895710 c3 "
body=$(xfn_body 4rseq7symbols)
if [ "$body" = "$want" ]; then
	echo "x86 rseq::symbols: the job's shape"
else
	fail "x86 rseq::symbols: not the job's shape: $body"
fi
xptrace got-rcx 48 8b 0d D32
xptrace got-rdx 48 8b 15 D32
xptrace st-rax-rdi 48 89 07
xptrace st-rcx-rdi8 48 89 4f 08
xptrace st-rdx-rdi16 48 89 57 10
cp "$here/x86/rseq-symbols.job" "$out/rseq-symbols-x86.job"
xjobs="$xjobs $out/rseq-symbols-x86.job"
# The TLS slot (tls::get and tls::set): every R_X86_64_GOTTPOFF
# relocation, which must be against challoc_tls, sits in a `mov R, [rip
# + challoc_tls@GOTTPOFF]` whose next instruction is the load `mov R,
# fs:[R]` or the store `mov fs:[R], V`; one job per register assignment.
awk '
	/^ +[0-9a-f]+: / {
		n++; s = $0; sub(/^ +[0-9a-f]+: /, "", s); txt[n] = s
		b = s; sub(/\t.*/, "", b); sub(/ +$/, "", b); bytes[n] = b
		next
	}
	/R_X86_64_GOTTPOFF/ { if ($3 == "challoc_tls-0x4") rel[n] = 1; else other++; next }
	/challoc_tls/ { other++ }
	END {
		for (i = 1; i <= n; i++) if (rel[i]) {
			if (txt[i] !~ ("\tmovq\t\\(%rip\\), %[a-z0-9]+( |$)")) { other++; continue }
			r = txt[i]; sub(/.*\(%rip\), %/, "", r); sub(/ .*/, "", r)
			if (txt[i + 1] ~ ("\tmovq\t%fs:\\(%" r "\\), %" r "$")) kind = "load " r " -"
			else if (txt[i + 1] ~ ("\tmovq\t%[a-z0-9]+, %fs:\\(%" r "\\)$")) {
				v = txt[i + 1]; sub(/.*\tmovq\t%/, "", v); sub(/,.*/, "", v)
				kind = "store " r " " v
			} else { other++; continue }
			gsub(/ /, "-", bytes[i]); gsub(/ /, "-", bytes[i + 1])
			print kind, bytes[i], bytes[i + 1]
		}
		if (other) print "other", other
	}' "$out/dis-x86.txt" | sort | uniq -c >"$out/xtls-sites.txt"
cat "$out/xtls-sites.txt"
grep -q ' load ' "$out/xtls-sites.txt" || fail "no TLS slot load in the x86_64 code"
grep -q ' store ' "$out/xtls-sites.txt" || fail "no TLS slot store in the x86_64 code"
! grep -q ' other ' "$out/xtls-sites.txt" || fail "a challoc_tls access in the x86_64 code matches neither the load nor the store template"
ngottpoff=$(grep -c 'R_X86_64_GOTTPOFF' "$out/dis-x86.txt" || true)
[ "$ngottpoff" -eq "$(awk '$2 != "other" { s += $1 } END { print s + 0 }' "$out/xtls-sites.txt")" ] ||
	fail "a GOTTPOFF relocation outside the TLS shapes"
# One encoding per register (the traces and jobs are named by register).
dup=$(awk '$2 != "other" { print $2, $3 }' "$out/xtls-sites.txt" | sort | uniq -d)
[ -z "$dup" ] || fail "x86 TLS: two encodings for $dup"
while read -r _count kind r v b1 b2; do
	[ "$kind" = other ] && continue
	# The first instruction's bytes up to its displacement, the second's
	# whole (a base of rbp or r13 takes a zero disp8).
	p1=$(echo "$b1" | tr '-' ' ' | awk '{ print $1, $2, $3 }')
	[ "$(echo "$b1" | tr '-' ' ' | awk '{ print $4 $5 $6 $7, NF }')" = "00000000 7" ] ||
		{ fail "TLS $kind $r: first instruction $b1"; continue; }
	n2=$(echo "$b2" | tr '-' ' ' | wc -w)
	xptrace "got-$r" $p1 D32
	if [ "$kind" = load ]; then
		xptrace "tls-ld-$r" $(echo "$b2" | tr '-' ' ')
		{
			sed -e "s/@R@/$r/g" -e "s/@N@/$n2/g" "$here/x86/tls-load.job.in"
			echo "insn x86-got-$r.trace fallthrough 7"
			echo "insn x86-tls-ld-$r.trace fallthrough $n2"
		} >"$out/tls-load-$r.job"
		xjobs="$xjobs $out/tls-load-$r.job"
	else
		xptrace "tls-st-$r-$v" $(echo "$b2" | tr '-' ' ')
		{
			sed -e "s/@A@/$r/g" -e "s/@V@/$v/g" -e "s/@N@/$n2/g" "$here/x86/tls-store.job.in"
			echo "insn x86-got-$r.trace fallthrough 7"
			echo "insn x86-tls-st-$r-$v.trace fallthrough $n2"
		} >"$out/tls-store-$r-$v.job"
		xjobs="$xjobs $out/tls-store-$r-$v.job"
	fi
done <"$out/xtls-sites.txt"
# rseq::thread_pointer: every load from %fs:0 (`mov R, qword ptr fs:[0]`,
# 64 REX 8b /r with a SIB absolute 0), one job per register. Every other
# %fs access is a TLS site above.
grep -E "${tab}movq${tab}%fs:0, %[a-z0-9]+$" "$out/dis-x86.txt" |
	awk '{ s = $0; sub(/^ +[0-9a-f]+: /, "", s); r = s; sub(/.*%fs:0, %/, "", r); sub(/\t.*/, "", s); sub(/ +$/, "", s); gsub(/ /, "-", s); print r, s }' |
	sort | uniq -c >"$out/fs0-sites.txt"
cat "$out/fs0-sites.txt"
[ -s "$out/fs0-sites.txt" ] || fail "no load of %fs:0 in the x86_64 code"
nfs=$(grep -cE "%fs:" "$out/dis-x86.txt" || true)
[ "$nfs" -eq $(($(awk '{ s += $1 } END { print s + 0 }' "$out/fs0-sites.txt") + ngottpoff)) ] ||
	fail "an %fs access that is neither a TLS site nor a load of %fs:0"
dup=$(awk '{ print $2 }' "$out/fs0-sites.txt" | sort | uniq -d)
[ -z "$dup" ] || fail "x86 %fs:0 load: two encodings for $dup"
while read -r _count r b; do
	n=$(echo "$b" | tr '-' ' ' | wc -w)
	xptrace "fs0-$r" $(echo "$b" | tr '-' ' ')
	{
		sed "s/@R@/$r/g" "$here/x86/fs0.job.in"
		echo "insn x86-fs0-$r.trace fallthrough $n"
	} >"$out/fs0-$r.job"
	xjobs="$xjobs $out/fs0-$r.job"
done <"$out/fs0-sites.txt"
# sys::trap: every ud2 in the code is `0f 0b` (LLVM's own traps are the
# same instruction), and it faults.
nud2=$(grep -cE "^ +[0-9a-f]+: 0f 0b +${tab}ud2$" "$out/dis-x86.txt" || true)
noud=$(grep -E "${tab}ud[0-9a-z]*(${tab}|$)" "$out/dis-x86.txt" | grep -vcE ": 0f 0b +${tab}ud2$" || true)
echo "ud2 instances: $nud2; other ud: $noud"
[ "$nud2" -gt 0 ] || fail "no ud2 in the x86_64 code"
[ "$noud" -eq 0 ] || fail "$noud ud0/ud1 or other ud2 encodings"
xptrace ud2 0f 0b
cp "$here/x86/ud2.job" "$out/ud2.job"
xjobs="$xjobs $out/ud2.job"
# The x86 jobs, with the controls of step 13, run in parallel on the token
# pool of step 6 (the seven-instruction rseq::symbols job takes minutes).
xrun() { # JOB...: isla-check each in the background; JOB.rc, JOB.log
	for job in "$@"; do
		read -r _ <&3
		(
			rc=0
			ISLA_CHECK_JOBS=4 "$out/isla-check" "$job" >"$job.log" 2>&1 || rc=$?
			echo $rc >"$job.rc"
			echo >&3
		) &
	done
	wait
}

# 13. Negative controls for step 12, each failing for its own reason: the
# GOT function's load one entry off and its `ret` claimed to fall
# through, rseq::symbols storing the first value twice, the TLS load
# without the relocation's definition and the store storing the offset,
# the thread pointer read from the GS base, `ud2` claimed to fall
# through, to fault through the model-error path or to write rip before
# its fault, and a job that does not name the 64-bit mode assumption its
# traces use.
xnegs=
xrefute() { # NAME JOB-TEXT REASON: as refute, run with xrun below
	printf '%s\n' "$2" >"$out/neg-$1.job"
	printf '%s\n' "$3" >"$out/neg-$1.reason"
	xnegs="$xnegs $1"
}
xrefute x86-got-off "$(sed 's/^assert (= mem.1.addr (bvadd got #x0000000000000000))$/assert (= mem.1.addr (bvadd got #x0000000000000008))/' "$out/got-fn-x86.job")" "assert (= mem.1.addr"
xrefute x86-got-ret "$(sed 's/^insn x86-ret.trace jump$/insn x86-ret.trace fallthrough 1/' "$out/got-fn-x86.job")" "not proved that insn 2 falls through"
xrefute x86-rseq-store "$(sed 's/^assert (= (concat wmem.16.val \(.*\)) (concat mem.16.val .*$/assert (= (concat wmem.16.val \1) (concat mem.8.val mem.7.val mem.6.val mem.5.val mem.4.val mem.3.val mem.2.val mem.1.val))/' "$out/rseq-symbols-x86.job")" "assert (= (concat wmem.16.val"
set -- $(grep ' load ' "$out/xtls-sites.txt" | head -n 1)
xrefute x86-tls-nodisp "$(grep -v 'seg.1.disp' "$out/tls-load-$3.job")" "assert (= mem.1.addr"
set -- $(grep ' store ' "$out/xtls-sites.txt" | head -n 1)
xrefute x86-tls-store-val "$(sed "s/^assert (= \(.*\) init.$4)$/assert (= \1 off)/" "$out/tls-store-$3-$4.job")" "assert (= (concat wmem.8.val"
set -- $(head -n 1 "$out/fs0-sites.txt")
xrefute x86-fs0-gs "$(sed 's/^assert (= mem.1.addr (bvadd init.msrs.1 /assert (= mem.1.addr (bvadd init.msrs.2 /' "$out/fs0-$2.job")" "assert (= mem.1.addr"
xrefute x86-ud2-falls "$(sed 's/^insn x86-ud2.trace trap x86_fault<u>$/insn x86-ud2.trace fallthrough 2/' "$out/ud2.job")" "a model fault"
xrefute x86-ud2-error "$(sed 's/^insn x86-ud2.trace trap x86_fault<u>$/insn x86-ud2.trace trap x86_model_error<u>/' "$out/ud2.job")" "a model fault with no call of x86_model_error<u>"
# A synthetic variant of the real ud2 trace that writes rip before the
# fault: a trap must leave rip alone.
awk '/\(call \|x86_fault<u>\|\)/ && !d { print "  (write-reg |rip| nil #x0000000000000000)"; d = 1 } { print }' \
	"$out/x86-ud2.trace" >"$out/x86-ud2-rip.trace"
xrefute x86-ud2-rip "$(sed 's/^insn x86-ud2.trace /insn x86-ud2-rip.trace /' "$out/ud2.job")" "a model fault after a rip write"
xrefute x86-fun "$(grep -v '^fun n64_bit_modep true$' "$out/got-fn-x86.job")" "function assumption n64_bit_modep => true, which the job does not name"
xrun $xjobs $(for n in $xnegs; do echo "$out/neg-$n.job"; done)
for job in $xjobs; do
	if [ "$(cat "$job.rc")" = 0 ]; then
		tail -n 1 "$job.log"
	else
		fail "isla-check $job: $(grep -m 1 FAIL "$job.log" || tail -n 1 "$job.log")"
	fi
done
for n in $xnegs; do
	log=$out/neg-$n.job.log
	if [ "$(cat "$out/neg-$n.job.rc")" = 0 ]; then
		fail "negative control $n passed"
	elif ! grep '^FAIL' "$log" | grep -qF -- "$(cat "$out/neg-$n.reason")"; then
		fail "negative control $n did not fail for its reason ($(cat "$out/neg-$n.reason")): $(tail -n 1 "$log")"
	else
		echo "negative control $n: rejected, as it should be"
	fi
done

if [ "$fails" -ne 0 ]; then
	echo "isla: $fails check(s) failed (traces and jobs in $out)"
	exit 1
fi
echo "isla: all checks passed (traces and jobs in $out)"
