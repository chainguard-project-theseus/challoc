#!/bin/sh
# verus/seams-mutants.sh: check that verus/seams.sh's loom self-tests have
# teeth. Each mutant below is one sed edit of seams.sh that weakens the
# `loom:` citation rule; the mutated copy's `--check` must report a
# self-test failure. Exits 1 if a mutant survives or no longer applies.
#
#   sh verus/seams-mutants.sh
#
# The copy runs as verus/.seams-mutant.sh (seams.sh finds the repository
# from its own path) and is removed on exit.
set -u
cd "$(dirname "$0")/.." || exit 2
m=verus/.seams-mutant.sh
trap 'rm -f "$m"' EXIT
fail=0
# shellcheck disable=SC2016 # sed scripts, not shell expansions
while IFS='|' read -r name expr; do
	[ -n "$name" ] || continue
	sed "$expr" verus/seams.sh > "$m"
	if cmp -s verus/seams.sh "$m"; then
		echo "seams-mutants: $name: the edit no longer applies"
		fail=1
		continue
	fi
	if sh "$m" --check 2>&1 | grep -q 'self-test: loom'; then
		echo "seams-mutants: $name: caught"
	else
		echo "seams-mutants: $name: SURVIVED"
		fail=1
	fi
done <<'EOF'
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
exit $fail
