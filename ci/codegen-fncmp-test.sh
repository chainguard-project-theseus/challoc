#!/bin/sh
# ci/codegen-fncmp-test.sh: run scripts/codegen-fncmp.py on the dump pairs
# in scripts/testdata/codegen-fncmp and check its verdicts (the lint
# check). old.txt has one AArch64 function with an offset-0 load,
# `ldr q0, [x9]`. reloc.txt has the same load at offset #3952, which is a
# relocation and must count as reloc-only; post.txt has it post-indexed,
# `ldr q0, [x9], #3952`, a different instruction that must count as changed.
# pad-new.txt is pad-old.txt moved, with 3 trailing nops instead of 1
# (padding-only). erratum-new.txt is erratum-old.txt with GNU ld's
# erratum 843419 rewrite: the adrp at page offset 0xffc became adr and a
# stub follows the body (erratum-stub, in either direction), and
# erratum-branch.txt has the other form, the load after that adrp moved
# into a stub and replaced by a branch to it (erratum-stub). These are
# changed: erratum-far.txt, the adr rewrite at offset 0xff4;
# erratum-loose.txt, the adr rewrite followed by code that is no stub;
# erratum-out.txt, the load replaced by a branch out of the function;
# erratum-misdirect.txt, the load replaced by a branch to the stub's
# far-branch entry rather than to the stub's copy of the load.
# pair-swap.txt swaps the bodies of pair-old.txt's f and g, two changes
# that matching by content alone would miss; hash-swap.txt does the same
# under a new crate hash (Cs..._), checked with c++filt hidden (the
# mangled-name key) and, when c++filt is on PATH, with it (the demangled
# key). impl-swap.txt swaps impl-old.txt's two method bodies while the
# impl disambiguator goes from Ms0_ to the base-62 MsA_ and the backref
# after it from B4_ to B5_ (c++filt hidden). pair-rename.txt renames g to
# h (exact), and pair-removed.txt drops g (changed).
set -eu
d=scripts/testdata/codegen-fncmp
py=$(command -v python3)
rc=0
check() { # check OLD NEW WANT [PATH]: compare OLD with NEW, want WANT on line 1
  got=$(PATH=${4:-$PATH} "$py" scripts/codegen-fncmp.py "$d/$1" "$d/$2" | head -1)
  if [ "$got" = "$3" ]; then echo "ok   $1 $2${4:+ (c++filt hidden)}: $got"
  else echo "FAIL $1 $2${4:+ (c++filt hidden)}: got = $got, want = $3"; rc=1; fi
}
nofilt=$(mktemp -d)
trap 'rm -rf "$nofilt"' EXIT
check old.txt reloc.txt 'functions old 1 new 1: exact 0, reloc-only 1, padding-only 0, erratum-stub 0, changed 0'
check old.txt post.txt 'functions old 1 new 1: exact 0, reloc-only 0, padding-only 0, erratum-stub 0, changed 1'
check pad-old.txt pad-new.txt 'functions old 1 new 1: exact 0, reloc-only 0, padding-only 1, erratum-stub 0, changed 0'
check erratum-old.txt erratum-new.txt 'functions old 1 new 1: exact 0, reloc-only 0, padding-only 0, erratum-stub 1, changed 0'
check erratum-new.txt erratum-old.txt 'functions old 1 new 1: exact 0, reloc-only 0, padding-only 0, erratum-stub 1, changed 0'
check erratum-old.txt erratum-far.txt 'functions old 1 new 1: exact 0, reloc-only 0, padding-only 0, erratum-stub 0, changed 1'
check pair-old.txt pair-swap.txt 'functions old 2 new 2: exact 0, reloc-only 0, padding-only 0, erratum-stub 0, changed 2'
check erratum-old.txt erratum-branch.txt 'functions old 1 new 1: exact 0, reloc-only 0, padding-only 0, erratum-stub 1, changed 0'
check erratum-old.txt erratum-loose.txt 'functions old 1 new 1: exact 0, reloc-only 0, padding-only 0, erratum-stub 0, changed 1'
check erratum-old.txt erratum-out.txt 'functions old 1 new 1: exact 0, reloc-only 0, padding-only 0, erratum-stub 0, changed 1'
check erratum-old.txt erratum-misdirect.txt 'functions old 1 new 1: exact 0, reloc-only 0, padding-only 0, erratum-stub 0, changed 1'
check hash-old.txt hash-swap.txt 'functions old 2 new 2: exact 0, reloc-only 0, padding-only 0, erratum-stub 0, changed 2' "$nofilt"
if command -v c++filt >/dev/null; then
  check hash-old.txt hash-swap.txt 'functions old 2 new 2: exact 0, reloc-only 0, padding-only 0, erratum-stub 0, changed 2'
else
  echo "skip hash-old.txt hash-swap.txt with c++filt: not on PATH"
fi
check impl-old.txt impl-swap.txt 'functions old 2 new 2: exact 0, reloc-only 0, padding-only 0, erratum-stub 0, changed 2' "$nofilt"
check pair-old.txt pair-rename.txt 'functions old 2 new 2: exact 2, reloc-only 0, padding-only 0, erratum-stub 0, changed 0'
check pair-old.txt pair-removed.txt 'functions old 2 new 1: exact 1, reloc-only 0, padding-only 0, erratum-stub 0, changed 1'
exit $rc
