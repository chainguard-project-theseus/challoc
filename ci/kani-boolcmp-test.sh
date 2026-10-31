#!/bin/sh
# ci/kani-boolcmp-test.sh: scripts/kani-boolcmp.sh over src/, which must
# be clean, and over its fixtures in scripts/testdata/kani-boolcmp (the
# lint check). probe.rs is a positive control: appended to src/arena.rs,
# its `p1_bool_lt` makes CBMC 6.11 print `warning: ignoring >=`, so it must
# be flagged. In cases.rs exactly the lines marked `// want: hit` must be,
# also when carry.rs is scanned first: its allow marker on line 7 must not
# exempt line 8 of the next file.
set -eu
d=scripts/testdata/kani-boolcmp
rc=0
if sh scripts/kani-boolcmp.sh; then echo "ok   src/: no hits"
else echo "FAIL src/: got = hits above, want = none"; rc=1; fi

got=$(sh scripts/kani-boolcmp.sh "$d/probe.rs" | cut -d: -f2 | sort -un | tr '\n' ' ') || true
want="$(grep -n 'assert_eq!(x < y' "$d/probe.rs" | cut -d: -f1) "
if [ "$got" = "$want" ]; then echo "ok   probe.rs: lines $got"
else echo "FAIL probe.rs: got = lines $got, want = lines $want"; rc=1; fi

want=$(grep -n '// want: hit$' "$d/cases.rs" | cut -d: -f1 | tr '\n' ' ')
for first in "" "$d/carry.rs"; do
  # shellcheck disable=SC2086
  got=$(sh scripts/kani-boolcmp.sh $first "$d/cases.rs" | grep "^$d/cases.rs:" | cut -d: -f2 | sort -un | tr '\n' ' ') || true
  if [ "$got" = "$want" ]; then echo "ok   ${first:+$first, }cases.rs: lines $got"
  else echo "FAIL ${first:+$first, }cases.rs: got = lines $got, want = lines $want"; rc=1; fi
done
exit $rc
