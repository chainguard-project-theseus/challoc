#!/bin/sh
# Check that the `asm!` blocks listed in verus/emptyasm/blocks.txt (the
# register-allocation hints with the empty template `/* {0} */`) add no
# instruction to the code, on each architecture they are compiled for.
#
#   verus/emptyasm/run.sh [-s] [-n] [OUT]
#
# For each target (EMPTYASM_TARGETS, default "x86_64 aarch64"):
#
# 1. The library's assembly. Builds the library as `cargo build --release`
#    does, with `--emit=asm,obj` added: rustc prints the assembly of the
#    one fat-LTO codegen unit (from a copy of the module it generates the
#    object from) beside the object. Then, unless `-s`, builds it again
#    with `-C save-temps` (which keeps the object the shipped library is
#    linked from) and checks the two objects are byte-identical, so the
#    printed module is the shipped one. A cross target is not linked
#    (`-C linker=true`: the object is complete before the link), and when
#    no cross C++ compiler is found its C++ shim is compiled with the
#    host's `c++`, which only the skipped link reads.
# 2. Every instance in that assembly. LLVM brackets the text it prints for
#    each inline-asm block with `#APP`/`#NO_APP` (x86_64) or
#    `//APP`/`//NO_APP` (aarch64), after its assembler parser has read the
#    template with the operands substituted: an instruction or directive
#    comes out as a line of its own, a comment in the target's comment
#    syntax (`/* r10 */` becomes `# r10`). The `.loc` in force at the
#    block (reset at each function's start) names its source line. Every
#    block must come from a source line holding an `asm!(` call, and only
#    the listed ones may hold a comment (so a listed block, whose printed
#    text always holds one, is never attributed elsewhere); each listed
#    block must have at least one instance on its architecture and none on
#    the other, and each instance must hold nothing but comment lines.
# 3. The object path. Compiles each listed block's `asm!` call, verbatim
#    from the source, into a function returning its operand
#    (`--emit=obj`, opt-level 3, the same toolchain), beside the same
#    function without the block, and checks the two disassemble to the
#    same bytes: the integrated assembler writes nothing for the
#    template into an object.
#
# `-n` is the negative test: it copies the tree, adds `nop` to each listed
# template, runs the check on the copy (with `-s`) and succeeds only if
# the check fails on every listed block in both step 2 and step 3.
#
# OUT (default: a fresh temporary directory) keeps the assembly. The build
# goes to EMPTYASM_TARGET_DIR (default OUT/target); EMPTYASM_CARGO_FLAGS
# adds cargo options (for example `--no-default-features --features
# quantum8`). RUSTUP_TOOLCHAIN picks the toolchain (default 1.95.0, the
# codegen gate's), whose llvm-tools component provides llvm-objdump.
# Exit status 0 when every check holds. See
# verus/seams/class-table-word_index.md, "Statement".
set -eu
usage() {
	echo "usage: $0 [-s] [-n] [OUT]" >&2
	exit 2
}
tie=1
negative=0
while getopts sn o; do
	case $o in
	s) tie=0 ;;
	n) negative=1 ;;
	*) usage ;;
	esac
done
shift $((OPTIND - 1))
[ $# -le 1 ] || usage
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:-$(mktemp -d "${TMPDIR:-/tmp}/challoc-emptyasm.XXXXXX")}
mkdir -p "$out"
out=$(cd "$out" && pwd)
tdir=${EMPTYASM_TARGET_DIR:-$out/target}
toolchain=${RUSTUP_TOOLCHAIN:-1.95.0}
sysroot=$(rustc +"$toolchain" --print sysroot)
host=$(rustc +"$toolchain" -vV | sed -n 's/^host: //p')
objdump=$sysroot/lib/rustlib/$host/bin/llvm-objdump
[ -x "$objdump" ] || { echo "emptyasm: missing $objdump (rustup component add llvm-tools)" >&2; exit 2; }
targets=${EMPTYASM_TARGETS:-x86_64 aarch64}
tab=$(printf '\t')

if [ $negative -eq 1 ]; then
	neg=$out/negative
	rm -rf "$neg" && mkdir -p "$neg/tree"
	(cd "$root" && git ls-files --cached --others --exclude-standard |
		while IFS= read -r f; do [ ! -f "$f" ] || printf '%s\n' "$f"; done |
		tar -cf - -T -) | tar -xf - -C "$neg/tree"
	# Every empty template in the listed files gets an instruction.
	for file in $(awk -F"$tab" '!/^#/ && NF == 3 { print $1 }' "$here/blocks.txt" | sort -u); do
		grep -q 'asm!("/\* {0} \*/"' "$neg/tree/$file" || { echo "emptyasm: no empty template in $file" >&2; exit 2; }
		sed -i 's|asm!("/\* {0} \*/"|asm!("nop /* {0} */"|' "$neg/tree/$file"
	done
	status=0
	EMPTYASM_TARGET_DIR=$tdir sh "$neg/tree/verus/emptyasm/run.sh" -s "$neg/out" >"$neg/log" 2>&1 || status=$?
	cat "$neg/log"
	[ $status -eq 1 ] || { echo "emptyasm: negative test: the check on the copy exited $status, not 1" >&2; exit 1; }
	missed=0
	while IFS="$tab" read -r file fn arch; do
		case $file in '#'* | '') continue ;; esac
		for what in "an instance of [^ ]* holds [1-9][0-9]* instruction" "the probe adds code"; do
			if grep -q "^FAIL block $file $fn ($arch): $what" "$neg/log"; then
				echo "negative test: $file $fn ($arch) fails ($what), as it must"
			else
				echo "negative test: $file $fn ($arch) does not fail ($what)" >&2
				missed=1
			fi
		done
	done <"$here/blocks.txt"
	[ $missed -eq 0 ] || exit 1
	echo "emptyasm: negative test ok"
	exit 0
fi

fails=0
fail() {
	echo "FAIL $*"
	fails=$((fails + 1))
}

# Every `asm!(...)` call in the items `fn FN` of FILE, one per line as
# LINE<TAB>CALL: the line of `asm!(` and the call from `asm!(` to its
# matching `)` (parentheses in string literals not counted), lines joined
# by a space (verus/isla/run.sh's extractor, with the line).
asm_calls() { # FILE FN
	awk -v fn="$2" '
		$0 ~ "(^|[ \t])fn " fn "[(<]" { infn = 1; next }
		infn && /^(    )?}/ && !inasm { infn = 0 }
		infn && !inasm && /asm!\(/ { t = $0; sub(/^[ \t]+/, "", t); if (t ~ /^\/\//) next; inasm = 1; buf = ""; d = 0; instr = 0; at = NR }
		inasm {
			line = $0; sub(/^[ \t]+/, "", line)
			if (buf == "") line = substr(line, index(line, "asm!("))
			o = ""; done = 0
			for (k = 1; k <= length(line) && !done; k++) {
				c = substr(line, k, 1); o = o c
				if (c == "\"") instr = !instr
				else if (!instr && c == "(") d++
				else if (!instr && c == ")" && --d == 0) done = 1
			}
			buf = buf (buf == "" ? "" : " ") o
			if (done) { inasm = 0; print at "\t" buf }
		}' "$root/$1"
}

build() { # ARCH KIND: KIND asm (--emit=asm,obj) or shipped (-C save-temps)
	arch=$1 kind=$2
	triple=$arch-unknown-linux-gnu
	set --
	if [ "$triple" = "$host" ]; then
		sub=release
	else
		sub=$triple/release
		set -- --target "$triple"
		cxxvar=CXX_$(echo "$triple" | tr - _)
		if eval "[ -z \"\${$cxxvar:-}\" ]" && [ -z "${CXX:-}" ] && ! command -v "$arch-linux-gnu-g++" >/dev/null 2>&1; then
			eval "export $cxxvar=c++"
		fi
	fi
	deps=$tdir/$sub/deps
	# Force the crate's rebuild, and leave no output of an earlier one.
	rm -rf "$tdir/$sub/.fingerprint"/challoc-*
	rm -f "$deps/challoc.s" "$deps/challoc.o" "$deps"/challoc.*.rcgu.*
	# EMPTYASM_CARGO_FLAGS is a list of options: word splitting is intended.
	# shellcheck disable=SC2086
	set -- "$@" ${EMPTYASM_CARGO_FLAGS:-} --
	if [ "$kind" = asm ]; then
		set -- "$@" --emit=asm,obj
	else
		set -- "$@" -C save-temps
	fi
	[ "$triple" = "$host" ] || set -- "$@" -C linker=true
	echo "building $arch ($kind; CARGO_TARGET_DIR=$tdir)"
	(cd "$root" && CARGO_TARGET_DIR=$tdir cargo +"$toolchain" rustc -q --release --lib "$@")
	if [ "$kind" = asm ]; then
		cp "$deps/challoc.s" "$out/$arch.s"
		cp "$deps/challoc.o" "$out/$arch.o"
	else
		set -- "$deps"/challoc.*.rcgu.o
		[ $# -eq 1 ] && [ -f "$1" ] || { echo "emptyasm: not one codegen unit object in $deps" >&2; exit 2; }
		cp "$1" "$out/$arch.shipped.o"
	fi
}

# Every inline-asm block of an assembly file, one per line:
# LOC<TAB>CODE<TAB>COMMENTS<TAB>COMMENTED<TAB>FIRST, LOC the root-relative
# FILE:LINE of the `.loc` in force ("?" for none since the function's
# start), CODE and COMMENTS its counts of other and of comment lines,
# COMMENTED its count of lines holding a comment anywhere, FIRST its first
# other line.
regions() { # ARCH FILE
	if [ "$1" = x86_64 ]; then c='#'; else c='//'; fi
	awk -v c="$c" -v root="$root/" '
		{ t = $0; gsub(/^[ \t]+|[ \t]+$/, "", t) }
		!app && t == c "APP" { app = 1; code = 0; com = 0; any = 0; first = ""; next }
		app && t == c "NO_APP" { app = 0; print loc "\t" code "\t" com "\t" any "\t" first; next }
		app && t == "" { next }
		app && substr(t, 1, length(c)) == c { com++; any++; next }
		app { code++; if (index(t, c)) any++; if (first == "") first = t; next }
		/^[ \t]*\.file[ \t]+[0-9]+[ \t]/ {
			m = split($0, q, "\"")
			p = (m >= 5) ? q[4] : q[2]
			if (m >= 5 && substr(p, 1, 1) != "/") p = q[2] "/" p
			if (index(p, root) == 1) p = substr(p, length(root) + 1)
			file[$2] = p
			next
		}
		/^[ \t]*\.loc[ \t]/ { loc = ($2 in file) ? file[$2] ":" $3 : "?"; next }
		/^[ \t]*\.cfi_startproc/ || /^[^ \t.#\/][^ \t]*:/ { loc = "?" }
		END { if (app) print "?\t1\t0\t0\tunterminated block" }' "$2"
}

# The object path: each listed block of ARCH in a function returning its
# operand, compiled alone, and the same function without it, compiled
# alone (one crate each, so that rustc's function merging cannot fold
# one into the other).
probe() { # ARCH
	arch=$1
	p=$out/probe-$arch
	rm -rf "$p" && mkdir -p "$p"
	i=0
	while IFS="$tab" read -r file fn barch; do
		case $file in '#'* | '') continue ;; esac
		[ "$barch" = "$arch" ] || continue
		i=$((i + 1))
		call=$(asm_calls "$file" "$fn" | cut -f2)
		v=
		[ "$(printf '%s\n' "$call" | grep -c .)" -ne 1 ] ||
			v=$(printf '%s\n' "$call" | sed -n 's/.*inout(reg) \([a-z_][a-z0-9_]*\).*/\1/p')
		if [ -z "$v" ]; then
			fail "block $file $fn ($arch): the probe adds code (no single asm! call with an inout(reg) operand to probe)"
			continue
		fi
		for kind in with without; do
			{
				echo '#![no_std]'
				echo '#[no_mangle]'
				echo 'pub fn emptyasm_probe(v: usize) -> usize {'
				if [ $kind = with ]; then
					echo '    #[allow(unused_mut)]'
					echo "    let mut $v = v;"
					echo "    unsafe { core::arch::$call };"
				else
					echo "    let $v = v;"
				fi
				echo "    $v"
				echo '}'
			} >"$p/$i-$kind.rs"
			if ! rustc +"$toolchain" --edition 2021 --crate-type lib --emit obj -C opt-level=3 -C panic=abort \
				--target "$arch-unknown-linux-gnu" "$p/$i-$kind.rs" -o "$p/$i-$kind.o" 2>"$p/$i-$kind.err"; then
				fail "block $file $fn ($arch): the probe adds code (it does not compile: $(head -n 3 "$p/$i-$kind.err"))"
				continue 2
			fi
			# The function's instruction bytes and text, addresses dropped.
			"$objdump" -d "$p/$i-$kind.o" | awk '
				/^[0-9a-f]+ <.*>:$/ { infn = ($2 == "<emptyasm_probe>:"); next }
				infn && /^ +[0-9a-f]+:/ { l = $0; sub(/^ +[0-9a-f]+:[ \t]*/, "", l); print l }' >"$p/$i-$kind.dis"
		done
		if [ -s "$p/$i-with.dis" ] && cmp -s "$p/$i-with.dis" "$p/$i-without.dis"; then
			echo "block $file $fn ($arch): the probe's object code is the same with and without it ($(grep -c . "$p/$i-with.dis") instructions)"
		else
			fail "block $file $fn ($arch): the probe adds code: with the block: $(echo $(cat "$p/$i-with.dis")); without: $(echo $(cat "$p/$i-without.dis"))"
		fi
	done <"$here/blocks.txt"
}

for arch in $targets; do
	case $arch in x86_64 | aarch64) ;; *) echo "emptyasm: unknown target $arch" >&2; exit 2 ;; esac
	build "$arch" asm
	if [ $tie -eq 1 ]; then
		build "$arch" shipped
		if cmp -s "$out/$arch.o" "$out/$arch.shipped.o"; then
			echo "$arch: the object built with --emit=asm is the shipped build's, byte for byte"
		else
			fail "$arch: the object built with --emit=asm differs from the shipped build's"
		fi
	fi
	regions "$arch" "$out/$arch.s" >"$out/$arch.regions"
	echo "$arch: $(grep -c . "$out/$arch.regions" || true) inline-asm blocks in the assembly"
	# The listed lines of this architecture.
	: >"$out/$arch.listed"
	while IFS="$tab" read -r file fn barch; do
		case $file in '#'* | '') continue ;; esac
		lines=$(asm_calls "$file" "$fn" | cut -f1)
		if [ "$barch" = "$arch" ] && [ "$(printf '%s\n' "$lines" | grep -c .)" -eq 1 ]; then
			echo "$file:$lines" >>"$out/$arch.listed"
		fi
	done <"$here/blocks.txt"
	# Every block comes from an `asm!(` call; only listed ones hold a comment.
	cut -f1 "$out/$arch.regions" | sort -u >"$out/$arch.locs"
	while read -r loc; do
		f=${loc%:*} l=${loc##*:}
		case $loc in
		src/*:[0-9]*) ;;
		*)
			fail "$arch: a block at $loc, outside the crate's source or after no .loc"
			continue
			;;
		esac
		if [ ! -f "$root/$f" ] || ! sed -n "${l}p" "$root/$f" | grep -q 'asm!('; then
			fail "$arch: a block at $loc, which holds no asm! call"
		fi
	done <"$out/$arch.locs"
	awk -F"$tab" 'FILENAME == ARGV[1] { listed[$1] = 1; next } $4 > 0 && !($1 in listed) { print $1 }' \
		"$out/$arch.listed" "$out/$arch.regions" | sort -u >"$out/$arch.commented"
	while read -r loc; do
		fail "$arch: a block at $loc holds a comment but is no listed block's"
	done <"$out/$arch.commented"
	while IFS="$tab" read -r file fn barch; do
		case $file in '#'* | '') continue ;; esac
		lines=$(asm_calls "$file" "$fn" | cut -f1)
		if [ "$(printf '%s\n' "$lines" | grep -c .)" -ne 1 ]; then
			fail "block $file $fn ($barch): not exactly one asm! call in fn $fn (lines: $(echo $lines))"
			continue
		fi
		loc=$file:$lines
		awk -F"$tab" -v loc="$loc" '$1 == loc' "$out/$arch.regions" >"$out/$arch.block"
		n=$(grep -c . "$out/$arch.block" || true)
		if [ "$barch" != "$arch" ]; then
			if [ "$n" -eq 0 ]; then
				echo "block $file $fn ($barch): no instance in the $arch code"
			else
				fail "block $file $fn ($barch): $n instances in the $arch code"
			fi
			continue
		fi
		bad=$(awk -F"$tab" '$2 != 0 || $3 == 0' "$out/$arch.block" | head -n 1)
		if [ "$n" -eq 0 ]; then
			fail "block $file $fn ($barch): no instance of $loc in the $arch code"
		elif [ -n "$bad" ]; then
			fail "block $file $fn ($barch): an instance of $loc holds $(echo "$bad" | cut -f2) instruction or directive lines (the first: $(echo "$bad" | cut -f5)) and $(echo "$bad" | cut -f3) comment lines"
		else
			echo "block $file $fn ($barch): $n instances of $loc, each only a comment"
		fi
	done <"$here/blocks.txt"
	probe "$arch"
done
[ $fails -eq 0 ] || { echo "emptyasm: $fails checks failed"; exit 1; }
echo "emptyasm: every listed block is empty in the code"
