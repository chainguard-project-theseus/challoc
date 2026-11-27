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
# h (exact), and pair-removed.txt drops g (changed). Then it runs
# ci/codegen-notices.sh on scripts/testdata/codegen-notices: report.txt
# must give want.txt exactly, and report-big.txt (more than 40 changed
# functions, and a codegen-fncmp.py failure) want-big.txt. Then it runs
# ci/codegen.sh with codegen-diff-standin.sh as compare, exiting 0 and
# then 2: ci/codegen.sh must exit the same and end with want.txt's notices.
# Last, it runs scripts/codegen-diff.sh with stand-in tools on PATH (see
# that section): a failed build, a failing objcopy, an empty .text and a
# failing sha256sum must stop it with exit 1 and no verdict.
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
# ci/codegen-notices.sh on saved compare reports: every notice line.
n=scripts/testdata/codegen-notices
notices() { # notices ARCH REPORT WANT
  if sh ci/codegen-notices.sh "$1" "$n/$2" | cmp -s - "$n/$3"; then
    echo "ok   codegen-notices $2"
  else
    echo "FAIL codegen-notices $2: got"
    sh ci/codegen-notices.sh "$1" "$n/$2"
    rc=1
  fi
}
notices x86_64 report.txt want.txt
notices aarch64 report-big.txt want-big.txt
# ci/codegen.sh with the stand-in compare: compare's exit status is the
# script's, and the report's notices end the output.
arch=$(uname -m)
out=$(mktemp)
for want in 0 2; do
  got=0
  CHAINGUARD_BASE_REVISION=HEAD CODEGEN_DIFF="$PWD/$n/codegen-diff-standin.sh" STANDIN_RC=$want \
    sh ci/codegen.sh > "$out" 2>&1 || got=$?
  tail -n "$(wc -l < "$n/want.txt")" "$out" > "$out.tail"
  if [ "$got" = "$want" ] && sed "s/ x86_64 / $arch /" "$n/want.txt" | cmp -s - "$out.tail"; then
    echo "ok   codegen.sh with compare exiting $want"
  else
    echo "FAIL codegen.sh with compare exiting $want: exit $got, output"
    cat "$out"
    rc=1
  fi
done
rm -f "$out" "$out.tail"
# scripts/codegen-diff.sh itself, in a scratch directory with stand-ins
# for cargo, objcopy, objdump, readelf, git and (last) sha256sum on PATH:
# a failed build, a failing objcopy, an empty .text and a failing
# sha256sum each stop capture or compare
# with exit 1 and no `.text hash` verdict, even with an earlier build's
# library in place, and the builds land in target/ and target/q8
# whatever CARGO_TARGET_DIR says.
cd_script=$PWD/scripts/codegen-diff.sh
t=$(mktemp -d)
mkdir "$t/bin" "$t/tree"
cat > "$t/bin/cargo" <<'EOF'
#!/bin/sh
[ -z "${STUB_CARGO_FAIL:-}" ] || { echo "error: stub build failed"; exit 101; }
mkdir -p "$CARGO_TARGET_DIR/release" && echo stub > "$CARGO_TARGET_DIR/release/libchalloc.so"
EOF
cat > "$t/bin/objcopy" <<'EOF'
#!/bin/sh
[ -z "${STUB_OBJCOPY_FAIL:-}" ] || { echo "objcopy: stub failure" >&2; exit 1; }
for a; do :; done # the last operand is the output
if [ -n "${STUB_TEXT_EMPTY:-}" ]; then : > "$a"; else echo text > "$a"; fi
EOF
printf '#!/bin/sh\necho "0000000000001000 <f>:"\n' > "$t/bin/objdump"
printf '#!/bin/sh\nexit 0\n' > "$t/bin/readelf"
printf '#!/bin/sh\ncase "$*" in *rev-parse*) echo 0000stub;; esac\nexit 0\n' > "$t/bin/git"
chmod +x "$t/bin/"*
diffrun() { # diffrun WANT_RC NAME MODE [VAR=VALUE...]: run, check the exit
  want=$1 name=$2 mode=$3; shift 3
  got=0
  (cd "$t/tree" && env PATH="$t/bin:$PATH" CARGO_TARGET_DIR="$t/elsewhere" "$@" \
    sh "$cd_script" "$mode" "$t/base") > "$t/out" 2>&1 || got=$?
  if [ "$got" != "$want" ]; then
    echo "FAIL codegen-diff $name: exit $got, want $want, output"; cat "$t/out"; rc=1; return 1
  fi
  if [ "$want" != 0 ] && grep -q '\.text hash\|^q[0-9]*: \.text' "$t/out"; then
    echo "FAIL codegen-diff $name: printed a verdict"; cat "$t/out"; rc=1; return 1
  fi
  echo "ok   codegen-diff $name"
}
diffrun 0 capture capture &&
  if [ -s "$t/tree/target/release/libchalloc.so" ] && [ -s "$t/tree/target/q8/release/libchalloc.so" ] &&
    [ ! -e "$t/elsewhere" ]; then
    echo "ok   codegen-diff builds into target/ and target/q8"
  else
    echo "FAIL codegen-diff builds: not in target/ and target/q8, or in CARGO_TARGET_DIR"; rc=1
  fi
diffrun 0 "compare, same build" compare &&
  if [ "$(grep -c 'IDENTICAL' "$t/out")" = 2 ]; then
    echo "ok   codegen-diff compare, same build: IDENTICAL twice"
  else
    echo "FAIL codegen-diff compare, same build: want IDENTICAL twice"; cat "$t/out"; rc=1
  fi
# diffrun sets rc on a failure; `|| :` keeps set -e from ending the run.
diffrun 1 "compare, failed build over an earlier library" compare STUB_CARGO_FAIL=1 || :
diffrun 1 "compare, objcopy fails" compare STUB_OBJCOPY_FAIL=1 || :
diffrun 1 "compare, empty .text" compare STUB_TEXT_EMPTY=1 || :
diffrun 1 "capture, empty .text" capture STUB_TEXT_EMPTY=1 || :
# A sha256sum that fails and prints nothing: an empty digest on both
# sides must not read IDENTICAL.
real_sha=$(command -v sha256sum)
cat > "$t/bin/sha256sum" <<EOF
#!/bin/sh
[ -z "\${STUB_SHA_FAIL:-}" ] || exit 1
exec $real_sha "\$@"
EOF
chmod +x "$t/bin/sha256sum"
diffrun 1 "compare, sha256sum fails" compare STUB_SHA_FAIL=1 || :
diffrun 1 "capture, sha256sum fails" capture STUB_SHA_FAIL=1 || :
rm -rf "$t"
exit $rc
