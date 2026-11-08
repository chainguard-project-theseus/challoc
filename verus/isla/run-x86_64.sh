# The x86_64 half of verus/isla/run.sh (sections 9 to 14), sourced by it
# with its variables and helpers set: `xlib` (the x86_64 library), `xir`
# (the x86 model), `out`, `here`, `fail`, `report`, `check`, `ran`, `refute`,
# `refuted`, the token pool on fd 3 and the tools.
# shellcheck shell=sh disable=SC2154 # run.sh, which sources this file, sets its variables

# 9. The x86_64 code, extracted as in section 2.
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
check x86_64/syscall "$out/syscall.job"

# 11. The TLS slot accesses (tls::get and tls::set; section 13 traces them):
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

# 12. x86_64 negative controls, on the real trace, each failing for its
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

# 13. The x86_64 halves of the seams with an asm! call per architecture
# (verus/ISLA.md, "The x86_64 model"): the GOT functions, rseq::symbols,
# the TLS slot load and store, the thread pointer load and `ud2`. Their
# data accesses need the processor's mode settled: from a fully symbolic
# state the model explores compatibility mode and the rest, and the runs
# do not finish.
# Three changes to the runs of sections 10 and 12 do it, all named in seams.md:
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
# Each job's names (ARCH/JOB, for run.sh's last section) go in JOB.name.
xjobs=
xadd() { # NAMES JOB: NAMES space-separated
	echo "$1" | tr -s ' ' '\n' | grep . >"$2.name" || :
	xjobs="$xjobs $2"
}
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
check x86_64/syscall-pinned "$out/syscall-pinned.job"
# Isla's view of one x86 instruction for these jobs: the patched entry,
# the application view, 64-bit mode. The arguments are bytes in memory
# order, `D32` a 4-byte displacement the linker fills (the symbolic
# opcode field `disp`); a `trap` job traces the model's x86_fault.
xptrace() { # NAME BYTE...: the trace x86-NAME.trace (apart from the Arm sections' names)
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
# je_malloc_conf_addr, single_threaded_addr: `mov rax, [rip +
# SYM@GOTPCREL]; ret`, one job for the five.
# The job is recorded once for each function whose shape matches it
# (x86_64/got-fn-NAME), so a seam names a function this section checked.
xgotfns=
for f in 20get_new_handler_addr:_ZSt15get_new_handlerv \
	20throw_bad_alloc_addr:_ZSt17__throw_bad_allocv 16malloc_conf_addr:malloc_conf \
	19je_malloc_conf_addr:je_malloc_conf 20single_threaded_addr:__libc_single_threaded; do
	want="488b0500000000 R_X86_64_GOTPCREL ${f#*:}-0x4 c3 "
	body=$(xfn_body "${f%%:*}")
	if [ "$body" = "$want" ]; then
		echo "x86 GOT function ${f%%:*}: the job's shape"
		g=${f%%:*} && xgotfns="$xgotfns x86_64/got-fn-${g#"${g%%[!0-9]*}"}"
	else
		fail "x86 GOT function ${f%%:*}: not the job's shape: $body"
	fi
done
xptrace got-rax 48 8b 05 D32
xptrace ret c3
cp "$here/x86/got-fn.job" "$out/got-fn-x86.job"
xadd "$xgotfns" "$out/got-fn-x86.job"
# rseq::symbols: three GOT loads, the tuple's stores through rdi, ret.
want="488b0500000000 R_X86_64_GOTPCREL __rseq_offset-0x4 488b0d00000000 R_X86_64_GOTPCREL __rseq_size-0x4 "
want="${want}488b1500000000 R_X86_64_GOTPCREL __rseq_flags-0x4 488907 48894f08 48895710 c3 "
body=$(xfn_body 4rseq7symbols)
if [ "$body" = "$want" ]; then
	echo "x86 rseq::symbols: the job's shape"
else
	fail "x86 rseq::symbols: not the job's shape: $body"
	ran x86_64/rseq-symbols fail
fi
xptrace got-rcx 48 8b 0d D32
xptrace got-rdx 48 8b 15 D32
xptrace st-rax-rdi 48 89 07
xptrace st-rcx-rdi8 48 89 4f 08
xptrace st-rdx-rdi16 48 89 57 10
cp "$here/x86/rseq-symbols.job" "$out/rseq-symbols-x86.job"
xadd x86_64/rseq-symbols "$out/rseq-symbols-x86.job"
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
dup=$(awk '$2 != "other" { print $2, $3, $4 }' "$out/xtls-sites.txt" | sort | uniq -d)
[ -z "$dup" ] || fail "x86 TLS: two encodings for $dup"
while read -r _count kind r v b1 b2; do
	[ "$kind" = other ] && continue
	# The first instruction's bytes up to its displacement, the second's
	# whole (a base of rbp or r13 takes a zero disp8).
	p1=$(echo "$b1" | tr '-' ' ' | awk '{ print $1, $2, $3 }')
	[ "$(echo "$b1" | tr '-' ' ' | awk '{ print $4 $5 $6 $7, NF }')" = "00000000 7" ] ||
		{ fail "TLS $kind $r: first instruction $b1"; continue; }
	n2=$(echo "$b2" | tr '-' ' ' | wc -w)
	# shellcheck disable=SC2086 # p1 is the instruction's first bytes, one argument each
	xptrace "got-$r" $p1 D32
	if [ "$kind" = load ]; then
		# shellcheck disable=SC2046 # the instruction's bytes, one argument each
		xptrace "tls-ld-$r" $(echo "$b2" | tr '-' ' ')
		{
			sed -e "s/@R@/$r/g" -e "s/@N@/$n2/g" "$here/x86/tls-load.job.in"
			echo "insn x86-got-$r.trace fallthrough 7"
			echo "insn x86-tls-ld-$r.trace fallthrough $n2"
		} >"$out/tls-load-$r.job"
		xadd x86_64/tls-load "$out/tls-load-$r.job"
	else
		# shellcheck disable=SC2046 # the instruction's bytes, one argument each
		xptrace "tls-st-$r-$v" $(echo "$b2" | tr '-' ' ')
		{
			sed -e "s/@A@/$r/g" -e "s/@V@/$v/g" -e "s/@N@/$n2/g" "$here/x86/tls-store.job.in"
			echo "insn x86-got-$r.trace fallthrough 7"
			echo "insn x86-tls-st-$r-$v.trace fallthrough $n2"
		} >"$out/tls-store-$r-$v.job"
		xadd x86_64/tls-store "$out/tls-store-$r-$v.job"
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
	# shellcheck disable=SC2046 # the instruction's bytes, one argument each
	xptrace "fs0-$r" $(echo "$b" | tr '-' ' ')
	{
		sed "s/@R@/$r/g" "$here/x86/fs0.job.in"
		echo "insn x86-fs0-$r.trace fallthrough $n"
	} >"$out/fs0-$r.job"
	xadd x86_64/fs0 "$out/fs0-$r.job"
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
xadd x86_64/ud2 "$out/ud2.job"
# The x86 jobs, with the controls of section 14, run in parallel on the token
# pool run.sh sets up (the seven-instruction rseq::symbols job takes minutes).
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

# 14. Negative controls for section 13, each failing for its own reason: the
# GOT function's load one entry off and its `ret` claimed to fall
# through, rseq::symbols storing the first value twice, the TLS load
# without the relocation's definition and the store storing the offset,
# the thread pointer read from the GS base, `ud2` claimed to fall
# through, to fault through the model-error path or to write rip before
# its fault, and a job that does not name the 64-bit mode assumption its
# traces use.
xnegs=
xrefute() { # NAME JOB-TEXT REASON: as refute, run with xrun below
	[ $# -eq 3 ] && [ -n "$3" ] || { fail "negative control $1: no reason given"; return; }
	printf '%s\n' "$2" >"$out/neg-$1.job"
	printf '%s\n' "$3" >"$out/neg-$1.reason"
	xnegs="$xnegs $1"
}
xrefute x86-got-off "$(sed 's/^assert (= mem.1.addr (bvadd got #x0000000000000000))$/assert (= mem.1.addr (bvadd got #x0000000000000008))/' "$out/got-fn-x86.job")" "assert (= mem.1.addr"
xrefute x86-got-ret "$(sed 's/^insn x86-ret.trace jump$/insn x86-ret.trace fallthrough 1/' "$out/got-fn-x86.job")" "not proved that insn 2 falls through"
xrefute x86-rseq-store "$(sed 's/^assert (= (concat wmem.16.val \(.*\)) (concat mem.16.val .*$/assert (= (concat wmem.16.val \1) (concat mem.8.val mem.7.val mem.6.val mem.5.val mem.4.val mem.3.val mem.2.val mem.1.val))/' "$out/rseq-symbols-x86.job")" "assert (= (concat wmem.16.val"
if grep -q ' load ' "$out/xtls-sites.txt"; then
	# shellcheck disable=SC2046 # split the line into its fields
	set -- $(grep ' load ' "$out/xtls-sites.txt" | head -n 1)
	xrefute x86-tls-nodisp "$(grep -v 'seg.1.disp' "$out/tls-load-$3.job")" "assert (= mem.1.addr"
fi
if grep -q ' store ' "$out/xtls-sites.txt"; then
	# shellcheck disable=SC2046 # split the line into its fields
	set -- $(grep ' store ' "$out/xtls-sites.txt" | head -n 1)
	xrefute x86-tls-store-val "$(sed "s/^assert (= \(.*\) init.$4)$/assert (= \1 off)/" "$out/tls-store-$3-$4.job")" "assert (= (concat wmem.8.val"
fi
if [ -s "$out/fs0-sites.txt" ]; then
	# shellcheck disable=SC2046 # split the line into its fields
	set -- $(head -n 1 "$out/fs0-sites.txt")
	xrefute x86-fs0-gs "$(sed 's/^assert (= mem.1.addr (bvadd init.msrs.1 /assert (= mem.1.addr (bvadd init.msrs.2 /' "$out/fs0-$2.job")" "assert (= mem.1.addr"
fi
xrefute x86-ud2-falls "$(sed 's/^insn x86-ud2.trace trap x86_fault<u>$/insn x86-ud2.trace fallthrough 2/' "$out/ud2.job")" "a model fault"
xrefute x86-ud2-error "$(sed 's/^insn x86-ud2.trace trap x86_fault<u>$/insn x86-ud2.trace trap x86_model_error<u>/' "$out/ud2.job")" "a model fault with no call of x86_model_error<u>"
# A synthetic variant of the real ud2 trace that writes rip before the
# fault: a trap must leave rip alone.
awk '/\(call \|x86_fault<u>\|\)/ && !d { print "  (write-reg |rip| nil #x0000000000000000)"; d = 1 } { print }' \
	"$out/x86-ud2.trace" >"$out/x86-ud2-rip.trace"
xrefute x86-ud2-rip "$(sed 's/^insn x86-ud2.trace /insn x86-ud2-rip.trace /' "$out/ud2.job")" "a model fault after a rip write"
xrefute x86-fun "$(grep -v '^fun n64_bit_modep true$' "$out/got-fn-x86.job")" "function assumption n64_bit_modep => true, which the job does not name"
# shellcheck disable=SC2046,SC2086 # lists of jobs, one argument each
xrun $xjobs $(for n in $xnegs; do echo "$out/neg-$n.job"; done)
for job in $xjobs; do
	if [ "$(cat "$job.rc")" = 0 ]; then
		while read -r n; do ran "$n" ok; done <"$job.name"
		tail -n 1 "$job.log"
	else
		while read -r n; do ran "$n" fail; done <"$job.name"
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
