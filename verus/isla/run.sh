#!/bin/sh
# Check the inline-assembly seams discharged with Isla (verus/ISLA.md)
# against the Sail Armv8.5-A model, on the aarch64 code built from this
# tree, and the aarch64 evidence for the mixed seams that stay trusted.
#
#   verus/isla/run.sh [-a LIBCHALLOC.a] [OUT]
#
# Builds the aarch64 static library (or takes the one given with -a),
# checks that the asm templates are the ones the records describe, finds
# every instance of each checked template in the compiled code, runs
# isla-footprint on each distinct instruction (the PAC ones in parallel,
# one per CPU) and checks each job with isla-check (verus/isla/check.rs).
# OUT (default: a fresh temporary directory) keeps the traces and the
# jobs. Exit status 0 when every check holds. About four minutes after the
# build on 16 CPUs.
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
	if grep -q '@[A-Z0-9]*@' "$rec"; then
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
# 31) or `xpacd Xd` (d not 31). The model runs with FEAT_PAuth (and the
# v8.2-v8.5 features an Armv8.5-A CPU has) on, at Non-secure EL0 with the
# PAC traps off: SCR_EL3.{API,APK} set, as firmware sets them, and either
# a kernel at EL1 with HCR_EL2 = HCR_HOST_NVHE_FLAGS (RW, API, APK) or a
# VHE kernel (E2H, TGE, RW). TCR is Linux's for each user VA size it supports:
# 36 (16K pages), 39 (4K), 42 (64K), 47 (16K), 48 (4K) and 52 bits (64K;
# TBI0 on), SCTLR (EnDA) symbolic, and ComputePAC
# abstract: Isla leaves the PAC algorithm an uninterpreted function.
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
pac_configs="el1-36 el1-39 el1-42 el1-47 el1-48 el1-52 vhe-36 vhe-39 vhe-42 vhe-47 vhe-48 vhe-52"
pac_uniq=
for mw in $pac_words; do
	case " $pac_uniq " in *" $mw "*) ;; *) pac_uniq="$pac_uniq $mw" ;; esac
done
pac_words=$pac_uniq
# Each trace is written whole or not at all (a killed isla-footprint
# leaves only the .part file), and every run makes its own.
pac_footprint() { # NAME ISLA-ARGS...: run in the background by the caller
	t=$1
	shift
	rm -f "$out/$t.trace" "$out/$t.failed"
	if "$footprint" -A "$ir" -C "$pac_config" -R PSTATE.EL=0b00 -R PSTATE.nRW=0b0 \
		-R SCR_EL3=0x0000000000030401 -R VBAR_EL1=undefined:%bv64 -R VBAR_EL2=undefined:%bv64 \
		-s "$@" >"$out/$t.trace.part" 2>"$out/$t.err"; then
		mv "$out/$t.trace.part" "$out/$t.trace"
	else
		echo "isla-footprint $t: $(grep -v 'No primop' "$out/$t.err" | head -3)" >"$out/$t.failed"
	fi
}
i=0
for c in $pac_configs; do
	case $c in
	el1-*) regs="-R HCR_EL2=0x0000030080000000 -R SCTLR_EL1=undefined:%bv64 -R TCR_EL1=$(pac_tcr ${c#*-})" ;;
	vhe-*) regs="-R HCR_EL2=0x0000000488000000 -R SCTLR_EL2=undefined:%bv64 -R TCR_EL2=$(pac_tcr ${c#*-})" ;;
	esac
	for mw in $pac_words; do
		pac_footprint "pac-$c-$(pac_name ${mw%%:*} ${mw#*:})" --abstract ComputePAC \
			-R _PC=undefined:%bv64 $regs -x -i "$(le ${mw#*:})" &
		i=$((i + 1))
		[ $((i % par)) -ne 0 ] || wait
	done
done
# The premise of `abstract ComputePAC`, that the model's PAC algorithm is a
# function of its four arguments, spot-checked: `pacda x0, x1` with
# ComputePAC unfolded on concrete operands, key and PC (kernel at EL1,
# 48-bit VA, EnDA set), for eight inputs. `pac-pure.job.in` runs two copies
# of each trace, each with its own undefined values and initial RC.
pac_inputs="0000123456789abc:0000000000001000:84be85ce9804e94b:ec2802d4e0a488e9
00007fffffffeff0:0000ffff00001111:0123456789abcdef:fedcba9876543210
0000000000000008:0000000000000000:0000000000000000:0000000000000000
0000aaaaaaaaaaa8:5555555555555555:ffffffffffffffff:0000000000000000
00000f0f0f0f0f00:7f12345678900000:13198a2e03707344:a4093822299f31d0
0000400000000000:deadbeefcafef00d:243f6a8885a308d3:082efa98ec4e6c89
00001b2c3d4e5f60:0000000000002000:452821e638d01377:be5466cf34e90c6c
0000ffffffffffff:ffffffffffffffff:c0ac29b7c97c50dd:3f84d5b5b5470917"
k=0
for in4 in $pac_inputs; do
	k=$((k + 1))
	p=${in4%%:*} r=${in4#*:}
	m=${r%%:*} r=${r#*:}
	hi=${r%%:*} lo=${r#*:}
	pac_footprint "pac-pure-$k" -R _PC=0x0000000000400000 -R HCR_EL2=0x0000030080000000 \
		-R SCTLR_EL1=0x0000000008000000 -R TCR_EL1="$(pac_tcr 48)" -R R0=0x$p -R R1=0x$m \
		-R APDAKeyHi_EL1=0x$hi -R APDAKeyLo_EL1=0x$lo -x -i "$(le $((0xdac10820)))" &
	i=$((i + 1))
	[ $((i % par)) -ne 0 ] || wait
done
wait
for f in "$out"/pac-*.failed; do
	[ -e "$f" ] && fail "$(cat "$f")"
done
pac_jobs=
for c in $pac_configs; do
	va=${c#*-}
	case $c in el1-*) sctlr=SCTLR_EL1 ;; *) sctlr=SCTLR_EL2 ;; esac
	for mw in $pac_words; do
		mn=${mw%%:*} w=${mw#*:}
		t=pac-$c-$(pac_name $mn $w)
		d=R$((w & 31)) n=R$((w >> 5 & 31))
		job=$out/$t.job
		{
			echo "insn $t.trace fallthrough"
			sed "s/@D@/$d/g" "$here/pac-footprint.job.in"
		} >"$job"
		pac_jobs="$pac_jobs $job"
		job=$out/$t-det.job
		if [ "$mn" = xpacd ]; then
			{
				echo "copy a"
				echo "insn pac-$c-xpacd-x0.trace fallthrough"
				echo "copy b"
				echo "insn $t.trace fallthrough"
				sed "s/@D@/$d/g" "$here/pac-strip-det.job.in"
			} >"$job"
		else
			{
				echo "copy a"
				echo "insn pac-$c-$mn-x0-x1.trace fallthrough"
				echo "copy b"
				echo "insn $t.trace fallthrough"
				sed -e "s/@OP@/$mn/g" -e "s/@D@/$d/g" -e "s/@N@/$n/g" -e "s/@SCTLR@/$sctlr/g" "$here/pac-det.job.in"
			} >"$job"
		fi
		pac_jobs="$pac_jobs $job"
	done
	job=$out/pac-$c-rt.job
	{
		echo "insn pac-$c-pacda-x0-x1.trace fallthrough"
		echo "insn pac-$c-autda-x0-x1.trace fallthrough"
		sed -e "s/@VA@/$va/g" -e "s/@ZEROS@/#b$(printf "%0$((64 - va))d" 0)/" "$here/pac-rt.job.in"
	} >"$job"
	pac_jobs="$pac_jobs $job"
done
k=0
for in4 in $pac_inputs; do
	k=$((k + 1))
	job=$out/pac-pure-$k.job
	{
		echo "copy a"
		echo "insn pac-pure-$k.trace fallthrough"
		echo "copy b"
		echo "insn pac-pure-$k.trace fallthrough"
		sed "s/@P@/#x${in4%%:*}/g" "$here/pac-pure.job.in"
	} >"$job"
	pac_jobs="$pac_jobs $job"
done
i=0
for job in $pac_jobs; do
	("$out/isla-check" "$job" >"$job.log" 2>&1; echo $? >"$job.rc") &
	i=$((i + 1))
	[ $((i % par)) -ne 0 ] || wait
done
wait
npac=0
for job in $pac_jobs; do
	npac=$((npac + 1))
	[ "$(cat "$job.rc")" = 0 ] || fail "isla-check $job: $(grep -m 1 FAIL "$job.log" || tail -n 1 "$job.log")"
done
echo "PAC: $npac jobs: $(echo $pac_words | wc -w) instructions in each of $(echo $pac_configs | wc -w) configurations, and $k purity spot checks"

# 7. Evidence for the aarch64 halves of the mixed seams (verus/ISLA.md,
# "Mixed seams"): the ledger keeps them trusted until their x86_64 halves
# are checked. Each compiled shape is required exactly, so a change in the
# code fails here until its job is revisited.
fn_body() { # NAME-PART: the words and relocations of the one function whose symbol contains NAME-PART
	awk -v fn="$1" '
		/^[0-9a-f]+ <[^>]*>:$/ { infn = index($0, fn) > 0; if (infn) nfn++; next }
		/^$/ { infn = 0 }
		infn && /^ +[0-9a-f]+: [0-9a-f]+ / { print $2; next }
		infn && /R_AARCH64_/ { print $2, $3 }
		END { if (nfn != 1) print "functions:", nfn + 0 }' "$out/dis.txt" | tr '\n' ' '
}
# throw_bad_alloc_addr, malloc_conf_addr, je_malloc_conf_addr: `adrp x0,
# :got:SYM; ldr x0, [x0, :got_lo12:SYM]; ret`, one job for the three
# (the relocated immediates are symbolic).
for f in 20throw_bad_alloc_addr:_ZSt17__throw_bad_allocv 16malloc_conf_addr:malloc_conf \
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
refute pac-pure-id "$(sed 's/^assert (not (= a.final.R0 \(.*\)))$/assert (= a.final.R0 \1)/' "$out/pac-pure-1.job")"
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

if [ "$fails" -ne 0 ]; then
	echo "isla: $fails check(s) failed (traces and jobs in $out)"
	exit 1
fi
echo "isla: all checks passed (traces and jobs in $out)"
