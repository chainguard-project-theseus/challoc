# The aarch64 half of verus/isla/run.sh (sections 2 to 8), sourced by it
# with its variables and helpers set: `lib` (the aarch64 library),
# `out`, `here`, `fail`, `check`, `ran`, `refute`, `le`, `bin5`, the token pool
# on fd 3 and the tools.
# shellcheck shell=sh disable=SC2154 # run.sh, which sources this file, sets its variables

# 2. The aarch64 code: one object with every codegen unit.
rm -rf "$out/obj" && mkdir -p "$out/obj"
members=$("$llvm_ar" t "$lib" | grep '^challoc-.*\.o$' || true)
[ -n "$members" ] || { echo "isla: no challoc objects in $lib" >&2; exit 2; }
# shellcheck disable=SC2086 # members is a list of object names
(cd "$out/obj" && "$llvm_ar" x "$lib" $members)
for m in $members; do
	"$llvm_objdump" -dr --no-print-imm-hex "$out/obj/$m"
done >"$out/dis.txt"

# 3. Isla's view of one instruction. The model runs at Non-secure EL0 in
# AArch64 state (SCR_EL3.{NS,RW}), as Linux user space does, with a
# symbolic PC and vector bases; `-s` simplifies the trace.
config=$out/armv8p5.toml
# The configuration's default toolchain names a cross assembler Isla only
# uses for assembly input; this harness passes opcodes.
sed 's/^\(assembler\|objdump\|nm\|linker\) = .*/\1 = "true"/' "$configs/armv8p5.toml" >"$config"
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
check aarch64/svc0 "$out/svc0.job"
check aarch64/svc0-vhe "$out/svc0-vhe.job"

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
grep ' load ' "$out/tls-sites.txt" >"$out/tls-loads.txt" || true
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
	check aarch64/tls-load "$job"
done <"$out/tls-loads.txt"

# 6. Pointer authentication (pac::sign, auth, strip and the round trip
# axiom_auth_sign; verus/isla/seams.md). Every PAC-family instruction
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
	[ "$(grep -cx '\[registers.reset\]' "$configs/armv9p4.toml")" = 1 ] ||
		fail "armv9p4.toml: not one [registers.reset] section (the Isla configuration changed?)"
	sed -e 's/^\(assembler\|objdump\|nm\|linker\) = .*/\1 = "true"/' \
		-e 's/^"\(v8Ap[2-5]\|v9Ap0\)_IMPLEMENTED" = false$/"\1_IMPLEMENTED" = true/' \
		"$configs/armv9p4.toml" |
		awk -v set="$2" '
			BEGIN { n = split(set, s, /[ \n]+/); for (i = 1; i <= n; i++) if (s[i] != "") { split(s[i], kv, ":"); val[kv[1]] = kv[2] } }
			/^"FEAT_[A-Za-z0-9_]*_IMPLEMENTED" = / { f = $1; gsub(/"/, "", f); sub(/_IMPLEMENTED$/, "", f); if (f in val) next }
			/^\[registers\.reset\]$/ { for (f in val) printf "\"%s_IMPLEMENTED\" = %s\n", f, val[f]; print ""; }
			{ print }' >"$1"
	for l in v8Ap2:true v8Ap3:true v8Ap4:true v8Ap5:true v8Ap6:false v8Ap7:false v8Ap8:false \
		v8Ap9:false v9Ap0:true v9Ap1:false v9Ap2:false v9Ap3:false v9Ap4:false $2; do
		{ [ "$(grep -c "^\"${l%%:*}_IMPLEMENTED\" = " "$1")" = 1 ] &&
			grep -qx "\"${l%%:*}_IMPLEMENTED\" = ${l#*:}" "$1"; } ||
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
	*) m=85 b='' mir=$ir mcf=$pac_config margs=$pac85_args step='' ;;
	esac
	case $1 in
	*el1-*) el=EL1 hcr=0x0100030080000000 ;;
	*) el=EL2 hcr=0x0000000488000000 ;;
	esac
	va=${1##*-}
	regs="-R HCR_EL2$b=$hcr -R SCTLR_$el$b=undefined:%bv64 -R TCR_$el$b=$(pac_tcr "$va")"
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
pac_bg() { # NAME IR CONFIG ISLA-ARGS...: pac_footprint in the background
	read -r _ <&3
	(
		pac_footprint "$@"
		echo >&3
	) &
}
# shellcheck disable=SC2086 # margs, regs and stop are argument lists
for c in $pac_configs; do
	pac_model "$c"
	for mw in $pac_words; do
		stop=
		[ "$m" = 85 ] || [ "${mw%%:*}" != autda ] || stop="--stop-at AArch64_PACFailException"
		pac_bg "pac-$c-$(pac_name "${mw%%:*}" "${mw#*:}")" "$mir" "$mcf" $margs --abstract ComputePAC $stop \
			-R _PC=undefined:%bv64 $regs -x -i "$(le "${mw#*:}")"
	done
	# The FPAC exception entry, on `autda x0, x1` run through.
	[ "$m" = 85 ] || pac_bg "pac-$c-autda-fault" "$mir" "$mcf" $margs --abstract ComputePAC \
		-R _PC=undefined:%bv64 $regs -x -i "$(le $((0xdac11820)))"
done
# Traces from the model's text, for the comparison with the serialized
# model below: one for each configuration file (`autda x0, x1` stopping at
# the FPAC call; `pacda x0, x1` with FEAT_CONSTPACFIELD; the first QARMA3
# purity spot check, in the loop below) and the FPAC exception entry under
# VHE. And `udf #0` (word 0), which the model's footprint function aborts
# without a trace event, for a negative control.
pac_model p2-el1-48
# shellcheck disable=SC2086 # margs and regs are argument lists
pac_bg pac-p2-el1-48-autda-x0-x1-text "$ir9" "$mcf" $margs --abstract ComputePAC \
	--stop-at AArch64_PACFailException -R _PC=undefined:%bv64 $regs -x -i "$(le $((0xdac11820)))"
# shellcheck disable=SC2086 # margs and regs are argument lists
pac_bg pac-p2-el1-48-udf "$mir" "$mcf" $margs -R _PC=undefined:%bv64 $regs -x -i "$(le 0)"
pac_model p2c-el1-48
# shellcheck disable=SC2086 # margs and regs are argument lists
pac_bg pac-p2c-el1-48-pacda-x0-x1-text "$ir9" "$mcf" $margs --abstract ComputePAC \
	-R _PC=undefined:%bv64 $regs -x -i "$(le $((0xdac10820)))"
pac_model p2-vhe-48
# shellcheck disable=SC2086 # margs and regs are argument lists
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
# shellcheck disable=SC2086 # pac85_args and pac94_args are argument lists
for in4 in $pac_inputs; do
	k=$((k + 1))
	p=${in4%%:*} r=${in4#*:}
	mod=${r%%:*} r=${r#*:}
	hi=${r%%:*} lo=${r#*:}
	set -- -R "R0=0x$p" -R "R1=0x$mod" -R "APDAKeyHi_EL1=0x$hi" -R "APDAKeyLo_EL1=0x$lo" -x -i "$(le $((0xdac10820)))"
	pac_bg "pac-pure85-$k" "$ir" "$pac_config" $pac85_args -R _PC=0x0000000000400000 \
		-R HCR_EL2=0x0100030080000000 -R SCTLR_EL1=0x0000000008000000 -R TCR_EL1="$(pac_tcr 48)" "$@"
	for q in q5:armv9p4-pac q3:armv9p4-pac-qarma3; do
		pac_bg "pac-pure94${q%%:*}-$k" "$ir9x" "$out/${q#*:}.toml" $pac94_args -R _PC=0x0000000000400000 \
			-R HCR_EL2.bits=0x0100030080000000 -R SCTLR_EL1.bits=0x0000000008000000 \
			-R TCR_EL1.bits="$(pac_tcr 48)" "$@"
	done
	[ "$k" -ne 1 ] || pac_bg "pac-pure94q3-1-text" "$ir9" "$out/armv9p4-pac-qarma3.toml" $pac94_args \
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
# Each job's name (ARCH/JOB, for run.sh's last section) goes in JOB.name.
pac_jobs=
pac_add() { # NAME JOB
	echo "$1" >"$2.name"
	pac_jobs="$pac_jobs $2"
}
for c in $pac_configs; do
	pac_model "$c"
	for mw in $pac_words; do
		mn=${mw%%:*} w=${mw#*:}
		t=pac-$c-$(pac_name "$mn" "$w")
		d=R$((w & 31)) n=R$((w >> 5 & 31))
		# In the v9.4 model an `autda` may end at the FPAC call instead.
		# Each insn line names the register the instruction writes when it
		# completes (checked under `pc-step`).
		kind=fallthrough stopl=
		if [ "$m" = 94 ] && [ "$mn" = autda ]; then
			kind=stop stopl="stop AArch64_PACFailException #b10"
		fi
		job=$out/$t.job
		{
			echo "insn $t.trace $kind $d"
			echo "$step"
			echo "$stopl"
			sed "s/@D@/$d/g" "$here/pac-footprint.job.in"
		} >"$job"
		pac_add "aarch64/pac-$mn" "$job"
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
		pac_add "aarch64/pac-$mn-det" "$job"
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
	pac_add "aarch64/pac-rt" "$job"
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
		pac_add "aarch64/pac-${s#*:}-spec" "$job"
		job=$out/$tr-off.job
		{
			echo "insn $tr.trace fallthrough R0"
			echo "$step"
			sed "s/@SCTLR@/$sctlr/g" "$here/pac-enda-off.job.in"
		} >"$job"
		pac_add "aarch64/pac-${s#*:}-off" "$job"
	done
	if [ $m = 94 ]; then
		case $el in EL1) lvl='#b01' ;; *) lvl='#b10' ;; esac
		job=$out/pac-$c-fault.job
		{
			echo "insn pac-$c-autda-fault.trace fault R0"
			sed -e "s/@EL@/$el/g" -e "s/@LEVEL@/$lvl/g" "$here/pac-fault.job.in"
		} >"$job"
		pac_add "aarch64/pac-autda-fault" "$job"
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
			[ "$pm" = 85 ] || echo "pc-step"
			sed "s/@P@/#x${in4%%:*}/g" "$here/pac-pure.job.in"
		} >"$job"
		pac_add "aarch64/pac-pure" "$job"
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
	if [ "$(cat "$job.rc")" = 0 ]; then
		ran "$(cat "$job.name")" ok
	else
		ran "$(cat "$job.name")" fail
		fail "isla-check $job: $(grep -m 1 FAIL "$job.log" || tail -n 1 "$job.log")"
	fi
done
echo "PAC: $npac jobs: $(echo "$pac_words" | wc -w) instructions in each of $(echo "$pac_configs" | wc -w) configurations (12 Armv8.5-A, 24 Armv9.4-A), and $((k * 3)) purity spot checks"

# 7. The aarch64 halves of the seams with an asm! call per architecture
# (verus/isla/seams.md); their x86_64 halves are section 13. Each
# compiled shape is required exactly, so a change in the code fails here
# until its job is revisited.
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
# The job is recorded once for each function whose shape matches it
# (aarch64/got-fn-NAME), so a seam names a function this section checked.
gotfns=
for f in 20get_new_handler_addr:_ZSt15get_new_handlerv \
	20throw_bad_alloc_addr:_ZSt17__throw_bad_allocv 16malloc_conf_addr:malloc_conf \
	19je_malloc_conf_addr:je_malloc_conf; do
	s=${f#*:}
	want="90000000 R_AARCH64_ADR_GOT_PAGE $s f9400000 R_AARCH64_LD64_GOT_LO12_NC $s d65f03c0 "
	body=$(fn_body "${f%%:*}")
	if [ "$body" = "$want" ]; then
		echo "GOT function ${f%%:*}: the job's shape"
		g=${f%%:*} && gotfns="$gotfns ${g#"${g%%[!0-9]*}"}"
	else
		fail "GOT function ${f%%:*}: not the job's shape: $body"
	fi
done
trace adrp-x0 --partial -i "1 immlo:2 10000 immhi:19 00000"
trace ldr-x0 --partial -i "1111100101 imm12:12 00000 00000"
trace ret -x -i "$(le 0xd65f03c0)"
cp "$here/got-fn.job.in" "$out/got-fn.job"
if "$out/isla-check" "$out/got-fn.job"; then r=ok; else r=fail; fail "isla-check $out/got-fn.job"; fi
for g in $gotfns; do ran "aarch64/got-fn-$g" $r; done
# rseq::symbols: three GOT loads, the tuple's stores through x0, ret.
want=
for r in 8:__rseq_offset 9:__rseq_size 10:__rseq_flags; do
	a=${r%%:*}
	want="$want$(printf %08x $((0x90000000 | a))) R_AARCH64_ADR_GOT_PAGE ${r#*:} "
	want="$want$(printf %08x $((0xf9400000 | a << 5 | a))) R_AARCH64_LD64_GOT_LO12_NC ${r#*:} "
	trace "adrp-x$a" --partial -i "1 immlo:2 10000 immhi:19 $(bin5 "$a")"
	trace "ldr-x$a" --partial -i "1111100101 imm12:12 $(bin5 "$a") $(bin5 "$a")"
done
want="${want}a9002408 f900080a d65f03c0 "
body=$(fn_body 4rseq7symbols)
if [ "$body" = "$want" ]; then
	echo "rseq::symbols: the job's shape"
else
	fail "rseq::symbols: not the job's shape: $body"
	ran aarch64/rseq-symbols fail
fi
trace stp-x8-x9-x0 -x -i "$(le 0xa9002408)"
trace str-x10-x0-16 -x -i "$(le 0xf900080a)"
cp "$here/rseq-symbols.job.in" "$out/rseq-symbols.job"
check aarch64/rseq-symbols "$out/rseq-symbols.job"
# rseq::thread_pointer: every `mrs xN, tpidr_el0` in the code (its own
# sites and the TLS sequences'), one job per register.
grep -E "^ +[0-9a-f]+: d53bd0[45][0-9a-f] +${tab}mrs${tab}x[0-9]+, TPIDR_EL0$" "$out/dis.txt" |
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
	check aarch64/mrs-tp "$out/mrs-tp-x$r.job"
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
check aarch64/brk1 "$out/brk1.job"
check aarch64/brk1-vhe "$out/brk1-vhe.job"
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
	check aarch64/tls-store "$job"
done <"$out/tls-stores.txt"

# 8. Negative controls: jobs that must fail, each with the `FAIL` reason
# its last argument names, so a checker that accepts everything, or that
# rejects a control for some other reason, fails here. On the real
# traces: a wrong syndrome, a wrong exception level, a memory read where
# there is none, `svc` expected to fall through, the TLS load's GOT load
# one entry off, without the LDR relocation's constraint or without the
# slot's alignment, and `add x0, x1, #3` claimed to write nothing. On
# synthetic traces (controls/): a read of a register an earlier write
# left inexpressible, a write through a field accessor, a write of
# PSTATE.N under `flags-preserved`, a `fallthrough` instruction
# continuing at PC + 8, a write before a `stop` call, a `stop` call that
# is not the last event, a user register written on a `fault` exception
# path, an exception path with no `assert-exc`, and under `pc-step` a PC
# write without `__BranchTaken`.
refute svc-esr "$(sed 's/#x56000000/#x56000001/' "$out/svc0.job")" 'assert (= final.ESR_EL1 #x56000001)'
refute svc-el "$(sed 's/^assert (= final.PSTATE.EL #b01)$/assert (= final.PSTATE.EL #b10)/' "$out/svc0.job")" 'assert (= final.PSTATE.EL #b10)'
refute svc-mem "$(sed 's/^mem-reads 0$/mem-reads 1/' "$out/svc0.job")" 'memory reads, expected Some(1)'
refute svc-falls "insn svc0.trace fallthrough" ': an exception'
if [ -s "$out/tls-loads.txt" ]; then
	first=$(head -n 1 "$out/tls-loads.txt")
	# shellcheck disable=SC2086 # split the line into its fields
	set -- $first
	a=$((0x$3 & 31)) b=$((0x$5 & 31)) c=$((0x$6 & 31))
	tls=$out/tls-load-x$a-x$b-x$c.job
	refute got-off "$(sed 's/^assert (= mem.1.addr got)$/assert (= mem.1.addr (bvadd got #x0000000000000008))/' "$tls")" 'assert (= mem.1.addr (bvadd got #x0000000000000008))'
	refute no-lo12 "$(grep -v '^assume (= seg.2.imm12 ' "$tls")" 'assert (= mem.1.addr got)'
	refute no-align "$(grep -v '^assume (= ((_ extract 2 0) (bvadd init.TPIDR_EL0 mem.1.val)) #b000)$' "$tls")" ': an exception'
fi
trace add-x0-x1 -x -i "$(le 0x91000c20)"
refute add-writes "insn add-x0-x1.trace fallthrough" 'insn 1 writes R0'
ctl=$here/controls
refute poison "$(printf 'insn %s\ninsn %s\nwrites R0 R1\nassert (= final.R1 final.R1)' \
	"$ctl/poison-1.trace" "$ctl/poison-2.trace")" 'insn 2 reads R0, which an earlier write left inexpressible'
refute field-write "insn $ctl/field-write.trace fallthrough" 'insn 1 writes R5.x'
refute flags "$(printf 'insn %s\nflags-preserved' "$ctl/flags.trace")" 'insn 1 writes PSTATE.N'
refute pc-skip "insn $ctl/skip.trace fallthrough" 'not proved that insn 1 falls through'
refute stop-write "$(printf 'insn %s stop\nstop Fault #b10\nwrites R3' "$ctl/stop-write.trace")" 'insn 1 writes R3 before its stop call'
refute stop-mid "$(printf 'insn %s stop\nstop Fault #b10' "$ctl/stop-mid.trace")" 'abstract call of Fault not expressible'
refute exc-user "$(printf 'insn %s fault R0\npc-step\nwrites R0\nassert-exc true' "$ctl/exc-user.trace")" 'insn 1 writes R0 and takes an exception'
refute exc-unchecked "$(printf 'insn %s fault R0\npc-step' "$ctl/exc.trace")" 'an exception path and no assert-exc for it'
refute pc-nobranch "$(printf 'insn %s fallthrough R0\npc-step' "$ctl/pc-nobranch.trace")" 'insn 1 writes the PC without setting __BranchTaken'
# PAC (section 6), on one configuration's jobs: the round trip without the
# VA bound, determinism without the modifier or either half of the key,
# or with SCTLR bit 26 equated in place of EnDA (bit 27), a job that does
# not name the abstract function, and a purity spot check claiming the
# PAC left the pointer unchanged.
pac=$out/pac-el1-48
refute pac-rt-va "$(grep -v '^assume (= ((_ extract 63 ' "$pac-rt.job")" 'assert (= final.R0 init.R0)'
refute pac-det-mod "$(grep -v '^assume (= a.init.R1 ' "$pac-pacda-x0-x1-det.job")" 'assert (= a.final.R0 b.final.R0)'
refute pac-det-keylo "$(grep -v '^assume (= a.init.APDAKeyLo_EL1 ' "$pac-pacda-x0-x1-det.job")" 'assert (= a.final.R0 b.final.R0)'
refute pac-det-keyhi "$(grep -v '^assume (= a.init.APDAKeyHi_EL1 ' "$pac-pacda-x0-x1-det.job")" 'assert (= a.final.R0 b.final.R0)'
refute pac-det-enda "$(sed 's/extract 27 27/extract 26 26/g' "$pac-autda-x0-x1-det.job")" 'assert (= a.final.R0 b.final.R0)'
refute pac-abstract "$(grep -v '^abstract ' "$pac-pacda-x0-x1.job")" 'abstract call of ComputePAC, which the job does not name'
refute pac-pure-id "$(sed 's/^assert (not (= a.final.R0 \(.*\)))$/assert (= a.final.R0 \1)/' "$out/pac-pure85-1.job")" 'assert (= a.final.R0 #x0000123456789abc)'
# The v8.5 model's PAC replacement claimed without its bit-54 inversion
# for a non-canonical pointer, and its failed `autda` claimed to return
# another error code.
refute pac-spec-flip "$(sed 's/(bvnot ((_ extract 54 54)/(bvneg ((_ extract 54 54)/' "$pac-pacda-x0-x1-spec.job")" 'assert (= final.R0 (concat '
refute pac-spec-ec "$(sed 's/ #b01 / #b00 /' "$pac-autda-x0-x1-spec.job")" 'assert (= final.R0 (ite '
# The same on an Armv9.4-A configuration, and what is new there:
# determinism of `autda` without the modifier, the PAuth2 XOR claimed to
# be an OR, `autda` claimed never to fault (its footprint as
# `fallthrough`, or with `not stop.1`), the fault's syndrome claimed to be
# 0b11 (key B), the FPAC exception entry with another syndrome, return
# address or EL, and a v9.4 job without the model's PC update (`pc-step`).
pac=$out/pac-p2-el1-48
refute pac9-rt-va "$(grep -v '^assume (= ((_ extract 63 ' "$pac-rt.job")" ': a stop call'
refute pac9-det-mod "$(grep -v '^assume (= a.init.R1 ' "$pac-pacda-x0-x1-det.job")" 'assert (= a.final.R0 b.final.R0)'
refute pac9-det-keylo "$(grep -v '^assume (= a.init.APDAKeyLo_EL1 ' "$pac-pacda-x0-x1-det.job")" 'assert (= a.final.R0 b.final.R0)'
refute pac9-det-keyhi "$(grep -v '^assume (= a.init.APDAKeyHi_EL1 ' "$pac-pacda-x0-x1-det.job")" 'assert (= a.final.R0 b.final.R0)'
refute pac9-det-enda "$(sed 's/extract 27 27/extract 26 26/g' "$pac-autda-x0-x1-det.job")" 'assert (= a.final.R0 b.final.R0)'
refute pac9-det-mod-auth "$(grep -v '^assume (= a.init.R1 ' "$pac-autda-x0-x1-det.job")" 'assert (= a.final.R0 b.final.R0)'
refute pac9-spec-xor "$(sed 's/(bvxor /(bvor /' "$pac-pacda-x0-x1-spec.job")" 'assert (= final.R0 (concat '
refute pac9-nofault "$(sed -e '1s/ stop R0$/ fallthrough R0/' -e '/^stop /d' "$pac-autda-x0-x1.job")" ': a stop call'
refute pac9-nostop "$(printf '%s\nassert (not stop.1)' "$(cat "$pac-autda-x0-x1.job")")" 'assert (not stop.1)'
refute pac9-stop-key "$(sed 's/^stop AArch64_PACFailException #b10$/stop AArch64_PACFailException #b11/' "$pac-autda-x0-x1.job")" 'not proved that insn 1 calls AArch64_PACFailException with argument 1 = #b11'
refute pac9-fault-esr "$(sed 's/#x0000000072000002/#x0000000072000003/' "$pac-fault.job")" 'assert (= final.ESR_EL1.bits #x0000000072000003)'
refute pac9-fault-elr "$(sed 's/^assert-exc (= final.ELR_EL1 init._PC)$/assert-exc (= final.ELR_EL1 (bvadd init._PC #x0000000000000004))/' "$pac-fault.job")" 'assert (= final.ELR_EL1 (bvadd init._PC #x0000000000000004))'
refute pac9-fault-el "$(sed 's/^assert-exc (= final.PSTATE.EL #b01)$/assert-exc (= final.PSTATE.EL #b10)/' "$pac-fault.job")" 'assert (= final.PSTATE.EL #b10)'
refute pac9-no-step "$(grep -v '^pc-step$' "$pac-pacda-x0-x1.job")" ': no PC write'
# `udf #0`, which the model aborts as UNDEFINED and its footprint function
# swallows, claimed to complete: its trace writes nothing, so under
# `pc-step` it must fail by not writing the register it names.
refute pac9-udf "$(printf 'insn pac-p2-el1-48-udf.trace fallthrough R0\npc-step\nwrites R0')" 'insn 1 falls through without writing R0 (an aborted instruction?)'
# `isla-check --same` on two different traces (`pacda` and `autda`).
if "$out/isla-check" --same "$pac-pacda-x0-x1.trace" "$pac-autda-x0-x1.trace" >"$out/neg-same.log" 2>&1; then
	fail "negative control same passed"
elif ! grep -q "^FAIL: .* and .* differ$" "$out/neg-same.log"; then
	fail "negative control same did not fail for its reason (the traces differ): $(tail -n 1 "$out/neg-same.log")"
else
	echo "negative control same: rejected, as it should be"
fi
# Section 7: the GOT function's load one entry off and its `ret` claimed
# to fall through, rseq::symbols storing the first value twice, the TLS
# store storing the offset or writing nothing, `brk` with the wrong
# syndrome or claimed to fall through, and `mrs` claimed to write nothing.
refute got-fn-off "$(sed 's/^assert (= mem.1.addr got)$/assert (= mem.1.addr (bvadd got #x0000000000000008))/' "$out/got-fn.job")" 'assert (= mem.1.addr (bvadd got #x0000000000000008))'
refute got-fn-falls "$(sed 's/^assert (= final._PC init.R30)$/assert (= final._PC (bvadd init._PC #x000000000000000c))/' "$out/got-fn.job")" 'assert (= final._PC (bvadd init._PC #x000000000000000c))'
refute rseq-store "$(sed 's/^assert (= wmem.2.val mem.2.val)$/assert (= wmem.2.val mem.1.val)/' "$out/rseq-symbols.job")" 'assert (= wmem.2.val mem.1.val)'
if [ -s "$out/tls-stores.txt" ]; then
	# shellcheck disable=SC2046 # split the line into its fields
	set -- $(head -n 1 "$out/tls-stores.txt")
	a=$((0x$3 & 31)) b=$((0x$5 & 31)) c=$((0x$6 & 31))
	refute tls-store-val "$(sed "s/^assert (= wmem.1.val init.R$c)$/assert (= wmem.1.val mem.1.val)/" "$out/tls-store-x$a-x$b-x$c.job")" 'assert (= wmem.1.val mem.1.val)'
	refute tls-store-nowrite "$(sed 's/^mem-writes 1$/no-mem-write/' "$out/tls-store-x$a-x$b-x$c.job")" 'insn 4 writes memory'
fi
refute brk-esr "$(sed 's/#xf2000001/#xf2000000/' "$out/brk1.job")" 'assert (= final.ESR_EL1 #xf2000000)'
refute brk-falls "insn brk1.trace fallthrough" ': an exception'
if [ -s "$out/mrs-sites.txt" ]; then
	# shellcheck disable=SC2046 # split the line into its fields
	set -- $(head -n 1 "$out/mrs-sites.txt")
	refute mrs-writes "insn mrs-x$((0x$2 & 31)).trace fallthrough" "insn 1 writes R$((0x$2 & 31))"
fi
