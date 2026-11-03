#!/bin/sh
# ci/heap-noalias.sh [q16|q8|self-test]...: no `&mut Heap` receiver is
# `noalias` in the LLVM IR rustc emits for the release library (the
# heap-noalias check).
# `Heap`'s methods write the home cache headers through a raw pointer
# while a `&mut Heap` is live, which is sound only because `Heap` is
# `!Unpin` and rustc then marks no `&mut Heap` `noalias` (the `_pin`
# field, src/heap/mod.rs). This checks what rustc emitted: for each
# quantum build (default both), one `cargo rustc --release` that also
# writes the crate's own IR as rustc's code generation left it, before
# any LLVM pass (`-C no-prepopulate-passes`) and without fat LTO
# (`CARGO_PROFILE_RELEASE_LTO=false`; with LTO, `--emit=llvm-ir` writes the
# merged, optimized module, where inlined methods have no `define` and
# their `noalias` is `!noalias` metadata in the caller). Each inherent
# `Heap` method (v0 symbols `_R[I]NvM[s.._]Nt..Cs.._7challoc4heap..NtB.._4Heap<name>`)
# that rustc emits out of line then has its own `define`, and a `%self`
# parameter that carries `noalias` but not `readonly` fails: a `&mut Heap`
# receiver. A `&self` receiver of a
# `Freeze` type is `noalias readonly` whatever `Unpin` says; those are
# listed, not failed. Fails when no `Heap` method with a `self` parameter
# is found, or only `&self` ones (no `&mut self` witness), so a symbol
# scheme the pattern does not know cannot pass unseen.
#
# Every run first scans the saved `define` lines in
# scripts/testdata/heap-noalias (no build), so a matcher that stops
# seeing a `noalias` self turns the check red: each file's verdict must
# be the one its `; want:` lines give (red.ll fails, clean.ll passes,
# none.ll has no `Heap` method and fails, ro-only.ll has only a `&self`
# and fails). Run with `self-test` alone, it does only that.
set -eu
[ $# -gt 0 ] || set -- q16 q8

# scan LL OUT: the verdict for the IR file LL, with the flagged methods
# and then the `&self` ones after it; returns 1 on a failure. Scratch
# files go to OUT.
scan() {
  defs=$2/heap-methods.txt
  bad=$2/heap-noalias.txt
  ro=$2/heap-readonly.txt
  grep -E '^define .* @_RI?NvM(s[0-9A-Za-z]*_)?(Nt)+Cs[0-9A-Za-z]+_7challoc4heap[0-9A-Za-z_]*NtB[0-9A-Za-z]*_4Heap[0-9]+[0-9A-Za-z_]*\(' "$1" >"$defs" || true
  # The `self` parameter: from the `(` or `, ` before its `ptr` to
  # `%self` (attributes carry no `%`; `dereferenceable(N)` and
  # `captures(..)` carry parentheses and commas).
  n=0
  : >"$bad"
  : >"$ro"
  while IFS= read -r d; do
    p=$(printf '%s\n' "$d" | grep -oE '(\(|, )ptr [^%]*%self[,)]' || true)
    [ -n "$p" ] || continue
    n=$((n + 1))
    name=$(printf '%s\n' "$d" | sed -E 's/^[^@]*@([^(]*)\(.*/  \1/')
    case $p in
      *' noalias '*' readonly '* | *' readonly '*' noalias '*) printf '%s\n' "$name" >>"$ro" ;;
      *' noalias '*) printf '%s\n' "$name" >>"$bad" ;;
    esac
  done <"$defs"
  st=0
  if [ "$n" -eq 0 ]; then
    echo "FAIL no Heap method with a self parameter in $1 (symbol pattern out of date?)"
    return 1
  elif [ "$n" -eq "$(wc -l <"$ro")" ]; then
    echo "FAIL no Heap method with a &mut self in $1, only &self (symbol pattern out of date?)"
    st=1
  elif [ -s "$bad" ]; then
    echo "FAIL $(wc -l <"$bad") of $n Heap methods take a noalias &mut self:"
    cat "$bad"
    st=1
  else
    echo "ok, $n Heap methods take self, no &mut self is noalias"
  fi
  if [ -s "$ro" ]; then
    echo "$(wc -l <"$ro") take a noalias readonly &self (allowed):"
    cat "$ro"
  fi
  return $st
}

# want LL: the verdict `scan` must give for the fixture LL, from its
# `; want:` lines (each names what the next `define` must count as).
want() {
  w=$(grep -A1 '^; want: ' "$1")
  names() { printf '%s\n' "$w" | grep -A1 "^; want: $1\$" | sed -nE 's/^define [^@]*@([^(]*)\(.*/  \1/p'; }
  f=$(names flagged)
  r=$(names readonly)
  n=$(grep -cE '^; want: (flagged|ok|readonly)$' "$1" || true)
  m=$(grep -cE '^; want: (flagged|ok)$' "$1" || true)
  if [ "$n" -eq 0 ]; then
    echo "FAIL no Heap method with a self parameter in $1 (symbol pattern out of date?)"
    return
  elif [ "$m" -eq 0 ]; then
    echo "FAIL no Heap method with a &mut self in $1, only &self (symbol pattern out of date?)"
  elif [ -n "$f" ]; then
    echo "FAIL $(printf '%s\n' "$f" | wc -l) of $n Heap methods take a noalias &mut self:"
    printf '%s\n' "$f"
  else
    echo "ok, $n Heap methods take self, no &mut self is noalias"
  fi
  [ -z "$r" ] || {
    echo "$(printf '%s\n' "$r" | wc -l) take a noalias readonly &self (allowed):"
    printf '%s\n' "$r"
  }
}

# self_test: scan each fixture; its verdict, and whether it fails, must
# be the wanted ones.
self_test() {
  t=scripts/testdata/heap-noalias
  out=target/heap-noalias-self-test
  mkdir -p "$out"
  st=0
  for f in red.ll:fail clean.ll:pass none.ll:fail ro-only.ll:fail; do
    ll=$t/${f%:*}
    [ -r "$ll" ] || {
      echo "self-test: FAIL no fixture $ll"
      return 1
    }
    if got=$(scan "$ll" "$out"); then res=pass; else res=fail; fi
    exp=$(want "$ll")
    if [ "$res" = "${f#*:}" ] && [ "$got" = "$exp" ]; then
      echo "self-test: ok   ${f%:*} (${f#*:}): $(printf '%s\n' "$got" | head -1)"
    else
      printf 'self-test: FAIL %s: got = (%s)\n%s\nwant = (%s)\n%s\n' "${f%:*}" "$res" "$got" "${f#*:}" "$exp"
      st=1
    fi
  done
  return $st
}

rc=0
self_test || rc=1
for q in "$@"; do
  case $q in
  self-test) continue ;;
  q16) features= ;;
  q8) features="--no-default-features --features quantum8" ;;
  *)
    echo "unknown variant $q (want q16, q8 or self-test)" >&2
    exit 1
    ;;
  esac
  target=target/heap-noalias-$q
  # shellcheck disable=SC2086 # $features is a word list
  CARGO_PROFILE_RELEASE_LTO=false CARGO_TARGET_DIR=$target cargo rustc --release --locked --lib -q $features -- --emit=llvm-ir -C no-prepopulate-passes
  ll=$target/release/deps/challoc.ll
  [ -s "$ll" ] || {
    echo "$q: no LLVM IR at $ll" >&2
    exit 1
  }
  if v=$(scan "$ll" "$target"); then echo "$q: $v"; else
    echo "$q: $v"
    rc=1
  fi
done
exit $rc
