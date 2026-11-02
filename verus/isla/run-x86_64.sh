# The x86_64 half of verus/isla/run.sh (sections 8 to 11), sourced by it
# with its variables and helpers set: `xlib` (the x86_64 library), `xir`
# (the x86 model), `out`, `here`, `fail`, `report`, `check`, `refute`,
# `refuted` and the tools.
# shellcheck shell=sh disable=SC2154 # run.sh, which sources this file, sets its variables

# 8. The x86_64 code, extracted as in section 2.
rm -rf "$out/xobj" && mkdir -p "$out/xobj"
members=$("$llvm_ar" t "$xlib" | grep '^challoc-.*\.o$' || true)
[ -n "$members" ] || { echo "isla: no challoc objects in $xlib" >&2; exit 2; }
# shellcheck disable=SC2086 # members is a list of object names
(cd "$out/xobj" && "$llvm_ar" x "$xlib" $members)
for m in $members; do
	"$llvm_objdump" -dr --no-print-imm-hex "$out/xobj/$m"
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
	s='' n=0
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
	echo "${s% }"
}

# 9. `syscall` (sys::raw's x86_64 syscall0..syscall6): every instruction
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
check x86_64/syscall "$out/syscall.job"

# 10. The TLS slot accesses (tls::get and tls::set; no job traces them):
# every relocation against `challoc_tls` is a GOTTPOFF on a `movq
# (%rip), %R` followed by the load `movq %fs:(%R), %R` or the store `movq
# %V, %fs:(%R)` (V not R), as section 5 holds the aarch64 ones to their
# shapes, so a change to either sequence fails here. Its controls change
# a copy of the code: the first load's `movq %fs:(%R), %R` without
# `%fs:` (a read of the GOT entry's value as an address) or into %rsp
# (another register than the one it reads through), and the first
# store's through %rsp (not the register holding the offset).
xtls() { # DIS: each access's shape, counted, and a FAIL line for each access in neither shape
	awk -F"$tab" '
		/^ +[0-9a-f]+: / { n++; mn[n] = $2; o = $3; sub(/ *#.*$/, "", o); sub(/ +$/, "", o); op[n] = o; next }
		$3 ~ /R_X86_64_GOTTPOFF$/ && $4 == "challoc_tls-0x4" { got[n] = 1; next }
		/challoc_tls/ { print "other relocation:" $0; next }
		END {
			for (i = 1; i <= n; i++) if (got[i]) {
				r = op[i]; s = "other: " mn[i] " " op[i] "; " mn[i + 1] " " op[i + 1]
				if (mn[i] == "movq" && sub(/^\(%rip\), /, "", r) && r ~ /^%r[a-z0-9]+$/ && mn[i + 1] == "movq") {
					if (op[i + 1] == "%fs:(" r "), " r) s = "load " r
					else if (op[i + 1] ~ /^%r[a-z0-9]+, %fs:\(%r[a-z0-9]+\)$/ && index(op[i + 1], ", %fs:(" r ")") && index(op[i + 1], r ",") != 1) s = "store " op[i + 1]
				}
				print s
			}
		}' "$1" | sort | uniq -c >"$1.sites"
	cat "$1.sites"
	grep -q '^ *[0-9]* load ' "$1.sites" || echo "FAIL no TLS slot load in the x86_64 code"
	grep '^ *[0-9]* other' "$1.sites" | sed 's/^ *[0-9]* /FAIL a challoc_tls access matches neither the load nor the store template: /' || true
}
xtls "$out/dis-x86.txt" >"$out/xtls.log"
report "$out/xtls.log"
first_x() { # DIS SHAPE RE TO: DIS with RE replaced by TO in the second instruction of the first TLS access whose second instruction matches SHAPE
	awk -v shape="$2" -v re="$3" -v to="$4" '
		!d && /R_X86_64_GOTTPOFF\tchalloc_tls-0x4$/ { d = 1; print; next }
		d == 1 && /^ +[0-9a-f]+: / { if ($0 ~ shape) { sub(re, to); d = 2 } else d = 0 }
		{ print }' "$1"
}
xload='	movq	%fs:[(]%r[a-z0-9]+[)], %r[a-z0-9]+$'
xstore='	movq	%r[a-z0-9]+, %fs:[(]%r[a-z0-9]+[)]$'
first_x "$out/dis-x86.txt" "$xload" '%fs:[(]' '(' >"$out/neg-xtls-fs.txt"
xtls "$out/neg-xtls-fs.txt" >"$out/neg-xtls-fs.log"
refuted xtls-fs "$out/neg-xtls-fs.log" '; movq (%r'
first_x "$out/dis-x86.txt" "$xload" '[)], %r[a-z0-9]+$' '), %rsp' >"$out/neg-xtls-reg.txt"
xtls "$out/neg-xtls-reg.txt" >"$out/neg-xtls-reg.log"
refuted xtls-reg "$out/neg-xtls-reg.log" '), %rsp'
first_x "$out/dis-x86.txt" "$xstore" '%fs:[(]%r[a-z0-9]+[)]$' '%fs:(%rsp)' >"$out/neg-xtls-base.txt"
xtls "$out/neg-xtls-base.txt" >"$out/neg-xtls-base.log"
refuted xtls-base "$out/neg-xtls-base.log" ', %fs:(%rsp)'

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
