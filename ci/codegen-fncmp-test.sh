#!/bin/sh
# ci/codegen-fncmp-test.sh: run scripts/codegen-fncmp.py on the dump pairs
# in scripts/testdata/codegen-fncmp and check its verdicts (the lint
# check). old.txt has one AArch64 function with an offset-0 load,
# `ldr q0, [x9]`. reloc.txt has the same load at offset #3952, which is a
# relocation and must count as reloc-only; post.txt has it post-indexed,
# `ldr q0, [x9], #3952`, a different instruction that must count as changed.
set -eu
d=scripts/testdata/codegen-fncmp
rc=0
check() { # check NEW WANT: compare old.txt with NEW, want WANT on line 1
  got=$(python3 scripts/codegen-fncmp.py "$d/old.txt" "$d/$1" | head -1)
  if [ "$got" = "$2" ]; then echo "ok   $1: $got"
  else echo "FAIL $1: got = $got, want = $2"; rc=1; fi
}
check reloc.txt 'functions old 1 new 1: exact 0, reloc-only 1, changed 0'
check post.txt 'functions old 1 new 1: exact 0, reloc-only 0, changed 1'
exit $rc
