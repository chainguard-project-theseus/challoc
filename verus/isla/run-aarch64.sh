# The aarch64 half of verus/isla/run.sh (sections 2 to 6), sourced by it
# with its variables and helpers set: `lib` (the aarch64 library),
# `out`, `here`, `fail`, `check`, `refute`, `le`, `bin5` and the tools.

# 2. The aarch64 code: one object with every codegen unit.
rm -rf "$out/obj" && mkdir -p "$out/obj"
members=$("$llvm_ar" t "$lib" | grep '^challoc-.*\.o$' || true)
[ -n "$members" ] || { echo "isla: no challoc objects in $lib" >&2; exit 2; }
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
# smc in the code is `svc #0`, and Isla's trace of it.
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

# 5. The TLS slot load (tls::get_tp; tls::get's aarch64 arm has its shape):
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

# 6. Negative controls: jobs that must fail, each with the `FAIL` reason
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
