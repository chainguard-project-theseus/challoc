#!/bin/sh
# verus/seams-mutants.sh: check that verus/seams.sh's self-tests have
# teeth, and that its hashes catch an edited seam. Each script mutant
# below is one sed edit of seams.sh that weakens the `loom:` citation
# rule, the `GenMC:` citation rule, the hash rule, the claims nothing
# rule, the one-line `const assert:` form, the not-Copy rule for
# tracked seams or the rule that every tracked type has a not-Copy
# check; the mutated copy's `--check`
# must report a self-test failure of that rule. Each tree mutant is one
# sed edit of a source file in a copy of the tree, followed by `--write`
# and `--check` there: an edit to a discharged seam's attribute,
# `requires`, `ensures` or body, to the attributes of a seam that
# claims nothing, or one that gives such a seam an `unsafe impl Send` or
# makes it a `tracked` struct, or one that weakens or removes a tracked
# type's not-Copy check in src/vspec/notcopy.rs or adds a tracked type
# with none, must fail `--check` with the
# message of the rule it breaks, and one that only adds a
# comment, reindents or moves the seam (or rewraps the check) must pass.
# Exits 1 if a mutant survives, a passing edit
# fails, or an edit no longer applies.
#
#   sh verus/seams-mutants.sh
#
# The script copy runs as verus/.seams-mutant.sh (seams.sh finds the
# repository from its own path) and is removed on exit; the tree copies go
# to a temporary directory.
set -u
cd "$(dirname "$0")/.." || exit 2
m=verus/.seams-mutant.sh
t=$(mktemp -d) || exit 2
trap 'rm -f "$m"; rm -rf "$t"' EXIT
fail=0

# script WANT: the script mutants on stdin, `name|sed expression` lines,
# each caught when its --check prints a line with WANT.
script() {
	while IFS='|' read -r name expr; do
		[ -n "$name" ] || continue
		sed "$expr" verus/seams.sh > "$m"
		if cmp -s verus/seams.sh "$m"; then
			echo "seams-mutants: $name: the edit no longer applies"
			fail=1
			continue
		fi
		if sh "$m" --check 2>&1 | grep -q "$1"; then
			echo "seams-mutants: $name: caught"
		else
			echo "seams-mutants: $name: SURVIVED"
			fail=1
		fi
	done
}

# tree FILE SEAM [SRC]: the tree mutants on stdin, `name|want|sed
# expression` lines, each a sed edit of FILE in a copy of the tree; want
# is `pass` when --check must pass after --write, else text its failure
# must print on a line naming SEAM of source file SRC (FILE by default).
tree() {
	while IFS='|' read -r name want expr; do
		[ -n "$name" ] || continue
		rm -rf "$t/tree" && mkdir "$t/tree" && cp -R src verus tests "$t/tree/" || exit 2
		rm -f "$t/tree/$m"
		sed "$expr" "$1" > "$t/tree/$1"
		if cmp -s "$1" "$t/tree/$1"; then
			echo "seams-mutants: $name: the edit no longer applies"
			fail=1
			continue
		fi
		sh "$t/tree/verus/seams.sh" --write > /dev/null 2>&1
		if out=$(sh "$t/tree/verus/seams.sh" --check 2>&1); then rc=0; else rc=1; fi
		if [ "$want" = pass ]; then
			if [ $rc = 0 ]; then
				echo "seams-mutants: $name: passes"
			else
				echo "seams-mutants: $name: FAILED, want passing:"
				echo "$out"
				fail=1
			fi
		elif [ $rc = 1 ] && echo "$out" | grep -F "$want" | grep -qF ": $2 (${3:-$1}:"; then
			echo "seams-mutants: $name: caught"
		else
			echo "seams-mutants: $name: SURVIVED"
			fail=1
		fi
	done
}

# shellcheck disable=SC2016 # sed scripts, not shell expansions
script 'self-test: loom' <<'EOF'
any backquoted span names the fn|s/^        if (sp == name) return 1$/        return 1/
a #[should_panic] control is accepted|s/ \&\& !index(attrs, "#\[should_panic")//
the file need not name the source|s/END { exit !(file \&\& found) }/END { exit !found }/
no #[test] needed|s/index(attrs, "#\[test\]") \&\& //
a bare fn name stands for Type::fn|s/lt_name=\${lt_name%%#\*}$/lt_name=${lt_name%%#*}; lt_name=${lt_name##*::}/
no module prefix needed|s/^       lt_name=\${lt_m##\*\/}::\$lt_name ;;$/       ;;/
a top-level file without src/|s/lt_s2=\$lt_src ;; esac/;; esac/
cites emits no loomdischarged|/print o, "loomdischarged", \$1/d
checkcite skips loom cites|s/^    loom) loomtest .*/    loom) ;;/
checkcite accepts loomdischarged|s/^    loomdischarged) echo .*/    loomdischarged) ;;/
EOF

# shellcheck disable=SC2016 # sed scripts, not shell expansions
script 'self-test: GenMC' <<'EOF'
a fail control is accepted|s/ \&\& NF == 5 \&\& trim(\$5) == "pass"//
the models need not match|s/ \&\& trim(\$3) == want//
the template need not name the fn|s/^    names(\$0) { found = 1 }$/    { found = 1 }/
the template need not name the source|s/END { exit !(found \&\& file) }/END { exit !found }/
a method seam matches without its Type::|s/gt_name=\${gt_name%%#\*}$/gt_name=${gt_name%%#*}; gt_name=${gt_name##*::}/
no module prefix needed|s/^       gt_name=\${gt_m##\*\/}::\$gt_name ;;$/       ;;/
a top-level file without src/|s/gt_s2=\$gt_src ;; esac/;; esac/
cites reads no GenMC tag|s/ tag\["GenMC: "\] = "GenMC";//
cites drops the seam from a GenMC item|s/ || tag\[t\] == "GenMC" ? / ? /
cites emits no genmcdischarged|/print o, "genmcdischarged", \$1/d
checkcite skips GenMC cites|s/^    GenMC) genmctest .*/    GenMC) ;;/
checkcite accepts genmcdischarged|s/^    genmcdischarged) echo .*/    genmcdischarged) ;;/
any case name resolves|s/trim(\$1) == c \&\& //
EOF

# shellcheck disable=SC2016 # sed scripts, not shell expansions
script 'self-test: hash' <<'EOF'
comments are hashed|s/t = trim(c); r = strip(raw, c)$/t = trim(c); r = raw/
whitespace is hashed as written|/^function norm(s)/s/gsub([^;]*; //
the contract is not hashed|s/printf "%s", norm(ehc\[e\]) > f/printf "%s", "x" > f/
the body is not hashed|s/if (norm(ehb\[e\]) != "") {/if (0) {/
attributes are not hashed|s/ehc\[n\] = hattrs;/ehc[n] = "";/
a partly checked status needs no hashes|s/ || estat\[e\] ~ \/^trusted .(partly checked\///
changed hashes are accepted|s/else if (chk \&\& rec != ehash\[e\])/else if (0)/
hashes on a trusted status are accepted|s/} else if (!chk \&\& rec != "")/} else if (0)/
EOF

# shellcheck disable=SC2016 # sed scripts, not shell expansions
script 'self-test: claims nothing' <<'EOF'
a claims nothing status needs no hashes|s/ || cn$//
no reason needed|s/print at "a claims nothing status gives no reason.*/at = at/
any kind may claim nothing|s/print at "claims nothing, but its kind.*/at = at/
an item need not be external_body|s/print at "claims nothing, but the item is not external_body.*/at = at/
a contract may hold ensures|s/print at "claims nothing, but its contract holds.*/at = at/
accept_recursive_types is allowed|s/|\[a-z_\]\*_recursive_types\[a-z_\]\*)(/)(/
a check may be cited|s/print at "claims nothing, but the status cites a check.*/at = at/
GenMC may be cited|s/|loom|GenMC|callers): /|loom|callers): /
any tool may be proposed|s/print at "claims nothing, but proposes the tool.*/at = at/
a tracked type may claim nothing|s/print at "claims nothing, but the item is a tracked type.*/at = at/
the specified type's unsafe impls are allowed|s/^    if (nm in unsafeimpl)$/    if (0)/
the item's own struct's unsafe impls are allowed|s/^    if (own != "" \&\& own != nm \&\& (own in unsafeimpl))$/    if (0)/
generic unsafe impls are not matched|s/unsafe impl(<\.\*>)? +/unsafe impl +/
EOF

# shellcheck disable=SC2016 # sed scripts, not shell expansions
script 'self-test: const assert: one line' <<'EOF'
an assertion on one line is not read|s/assert!\\(([$]|[.][*]\\);[$])\//assert!\\($\//
EOF

# shellcheck disable=SC2016 # sed scripts, not shell expansions
script 'self-test: not Copy' <<'EOF'
NotClone need not be named|s/ \&\& s ~ ("\[<:\]" t g " as NotClone<_>>::HOLDS")//
any type's check matches|s/("\[<:\]" t g " as NotCopy<_>>::HOLDS") \&\& s ~ ("\[<:\]" t g " as NotClone/(" as NotCopy<_>>::HOLDS") \&\& s ~ (" as NotClone/
a longer name matches|s/"\[<:\]" t g " as Not/t g " as Not/g
comments count|s/sub(\/\\\/\\\/\.\*\$\/, "", x); //
tracked types are not held to it|s/ \&\& index(estat\[e\], "const assert: ")) notcopycites(e, ok)$/ \&\& 0) notcopycites(e, ok)/
a failing cite is ignored|s/if (!(keys\[i\] in ok)) {/if (0) {/
the second impls are not required|s/if (n != 1 || !ic || !il) exit 1/if (n != 1) exit 1/
EOF

# shellcheck disable=SC2016 # sed scripts, not shell expansions
script 'self-test: tracked' <<'EOF'
enums need no check|/^    \/\^\[ \\t\]\*(pub/s/(struct|enum)/struct/
a restricted visibility is not read|/^    \/\^\[ \\t\]\*(pub/s/(pub(\\(\[^)\]\*\\))?/(pub/
indented items need no check|/^    \/\^\[ \\t\]\*(pub/s/\^\[ \\t\]\*(pub/^(pub/
a missing check is accepted|s/^    elif ! { notcopy/    elif false \&\& { notcopy/
a gated check is accepted|s/ \&\& assertion "[^"]*"; }; then$/; }; then/
a second type of a name shares its check|s/d = (n in seen) ? "dup" : ""/d = ""/
generic arguments are not read|s/g = "(<\[^<>\]\*>)?"/g = ""/
EOF

# The tracked type Unbacked of src/vspec/unbacked.rs, not a seam, and
# its check in src/vspec/notcopy.rs, which every tracked type needs.
# shellcheck disable=SC2016 # sed scripts, not shell expansions
tree src/vspec/notcopy.rs Unbacked src/vspec/unbacked.rs <<'EOF'
its check removed|a tracked type with no check|/^const _: () = assert!(<Unbacked as NotCopy/d
its Clone check dropped|a tracked type with no check|s/^\(const _: () = assert!(<Unbacked as NotCopy<_>>::HOLDS\) \&\& <Unbacked as NotClone<_>>::HOLDS);$/\1);/
its label misspelled|a tracked type with no check|s/^\/\/ `Unbacked` is neither/\/\/ `Unbaked` is neither/
its check gated|a tracked type with no check|/^\/\/ `Unbacked` is neither/i #[cfg(any())]
a comment added|pass|/^\/\/ `Unbacked` is neither/s/$/ More words./
EOF

# A generic tracked type's check at another argument.
# shellcheck disable=SC2016 # sed scripts, not shell expansions
tree src/vspec/notcopy.rs Held src/arena.rs <<'EOF'
another argument|pass|s/<Held<()> as/<Held<u16> as/g
EOF

# A tracked struct added to src/vspec/zero.rs with no check.
# shellcheck disable=SC2016 # sed scripts, not shell expansions
tree src/vspec/zero.rs Extra <<'EOF'
a tracked struct with no check|a tracked type with no check|/^pub tracked struct Zeroed {$/i pub(crate) tracked struct Extra { ghost x: int }
EOF

# A second tracked struct named Unbacked added to src/vspec/zero.rs.
# shellcheck disable=SC2016 # sed scripts, not shell expansions
tree src/vspec/zero.rs Unbacked <<'EOF'
a second type of a checked name|a second tracked type|/^pub tracked struct Zeroed {$/i pub(crate) tracked struct Unbacked { ghost x: int }
EOF

# The seam is the external_body tracked type WordPerm of src/vspec/word.rs,
# whose status cites its not-Copy check in src/vspec/notcopy.rs.
# shellcheck disable=SC2016 # sed scripts, not shell expansions
tree src/vspec/notcopy.rs WordPerm src/vspec/word.rs <<'EOF'
the Clone check dropped|neither Copy nor Clone|s/^const _: () = assert!(<WordPerm as NotCopy<_>>::HOLDS \&\& <WordPerm as NotClone<_>>::HOLDS);$/const _: () = assert!(<WordPerm as NotCopy<_>>::HOLDS);/
another type checked|neither Copy nor Clone|s/^const _: () = assert!(<WordPerm as NotCopy<_>>::HOLDS \&\& <WordPerm as NotClone/const _: () = assert!(<HeapPerm as NotCopy<_>>::HOLDS \&\& <HeapPerm as NotClone/
the check removed|neither Copy nor Clone|/^const _: () = assert!(<WordPerm as NotCopy/d
the Clone impl removed|neither Copy nor Clone|/^impl<T: Clone> NotClone<IfClone> for T {}$/d
a comment added|pass|/^\/\/ `WordPerm` is neither/s/$/ More words./
rewrapped|pass|s/^const _: () = assert!(<WordPerm as NotCopy<_>>::HOLDS \&\& /const _: () = assert!(\n    <WordPerm as NotCopy<_>>::HOLDS \&\& /
EOF

# The seam is the external type specification of core::ffi::c_void in
# src/vspec/config.rs, whose status says it claims nothing.
# shellcheck disable=SC2016 # sed scripts, not shell expansions
tree src/vspec/config.rs core::ffi::c_void <<'EOF'
external_body dropped|not external_body|/^\/\/\/ addresses its assembly returns/,/^pub struct ExCVoid/{/^#\[verifier::external_body\]$/d;}
an attribute added|its contract changed|/^\/\/\/ addresses its assembly returns/,/^pub struct ExCVoid/s/^#\[verifier::external_body\]$/#[verifier::external_body] #[allow(dead_code)]/
a comment added|pass|s/^pub struct ExCVoid(core::ffi::c_void);$/& \/\/ a comment/
EOF

# The seam is the external_body type Handler in src/api/cxx.rs, whose
# status says it claims nothing.
# shellcheck disable=SC2016 # sed scripts, not shell expansions
tree src/api/cxx.rs Handler <<'EOF'
an unsafe impl of Send added|the type Handler has|/^struct Handler(unsafe extern "C" fn());$/a unsafe impl Send for Handler {}
made a tracked struct|a tracked type|s/^struct Handler(unsafe extern "C" fn());$/tracked &/
EOF

# The seam is desc_stride of src/slab.rs, discharged by a Kani harness.
# shellcheck disable=SC2016 # sed scripts, not shell expansions
tree src/slab.rs desc_stride <<'EOF'
an edited requires|its contract changed|/^pub fn desc_stride/,/^}/s/class < NCLASSES,/class <= NCLASSES,/
an edited ensures|its contract changed|/^pub fn desc_stride/,/^}/s/r <= u32::MAX,/r < u32::MAX,/
an edited attribute|its contract changed|/^\/\/\/ Descriptor stride for/,/^pub fn desc_stride/s/^#\[inline\]$/#[inline(always)]/
an edited body|its body changed|/^pub fn desc_stride/,/^}/s/DESC_STRIDE\[class\] as usize/DESC_STRIDE[class] as usize + 0/
a comment added|pass|/^pub fn desc_stride/,/^}/s/class < NCLASSES,/class < NCLASSES, \/\/ a comment/
reindented|pass|/^pub fn desc_stride/,/^}/s/^    /        /
moved down a line|pass|1{h;s/.*//;G;}
EOF
exit $fail
