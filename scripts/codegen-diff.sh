#!/bin/sh
# codegen-diff.sh: prove that a source change did not change the machine code.
#
#   sh scripts/codegen-diff.sh capture [DIR]   build both quantum variants in
#                                             release and save the .so, its
#                                             disassembly and section sizes
#                                             under DIR (default .tmp-codegen)
#   sh scripts/codegen-diff.sh compare [DIR]   rebuild and compare with DIR
#
# Runs inside the dev container (needs cargo, objdump, readelf, objcopy).
# `compare` prints, per variant:
#   .text hash          identical => byte-identical machine code, done
#   exact functions     functions whose instruction text matches exactly
#                       once addresses are stripped
#   reloc-only          functions that match once hex immediates are masked
#                       too (moving code between files shifts .rodata and
#                       every PC-relative offset with it; the instructions
#                       are the same)
#   changed             functions with no counterpart in the baseline
# A refactor that moves code without changing it shows .text of the same
# size, zero changed functions and a mix of exact and reloc-only. Anything
# in `changed` is a real code change and needs the benchmark protocol.
set -eu
MODE=${1:-}; DIR=${2:-.tmp-codegen}
[ -n "$MODE" ] || { sed -n '2,/^set -eu/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 1; }
mkdir -p "$DIR"

build() { # build VARIANT -> prints path of the .so
  case "$1" in
    q16) cargo build --release -q 2>&1 | tail -3 >&2; echo target/release/libchalloc.so;;
    q8)  CARGO_TARGET_DIR=target/q8 cargo build --release -q --no-default-features --features quantum8 2>&1 | tail -3 >&2; echo target/q8/release/libchalloc.so;;
  esac
}
text_hash() { objcopy -O binary --only-section=.text "$1" "$DIR/.text.bin" && sha256sum "$DIR/.text.bin" | cut -c1-16; rm -f "$DIR/.text.bin"; }
sections() { readelf -S -W "$1" | awk '$2 ~ /^\.(text|rodata|data\.rel\.ro|data|bss)$/ {printf "%s=%s ", $2, $6}'; }
disasm() { objdump -d --no-show-raw-insn "$1"; }
# Per-function fingerprints: one line "<ninsns> <hash-exact> <hash-masked> <name>".
fingerprints() {
  awk '
    function flush() { if (name != "") { printf "%d %s %s %s\n", n, ex, ma, name }; name = ""; n = 0; ex = ""; ma = "" }
    /^[0-9a-f]+ <.*>:$/ { flush(); name = $2; next }
    /^ *[0-9a-f]+:\t/ {
      line = $0; sub(/^ *[0-9a-f]+:\t/, "", line)
      gsub(/<[^>]*>/, "<sym>", line)              # symbolic targets
      e = line
      m = line; gsub(/0x[0-9a-f]+/, "0xI", m); gsub(/#-?[0-9]+/, "#I", m); gsub(/\$-?0x[0-9a-f]+/, "$I", m)
      ex = ex "|" e; ma = ma "|" m; n++
      next }
    END { flush() }
  ' "$1" | while read -r n ex ma name; do
      printf '%s %s %s %s\n' "$n" "$(printf '%s' "$ex" | cksum | cut -d' ' -f1)" "$(printf '%s' "$ma" | cksum | cut -d' ' -f1)" "$name"
    done
}

case "$MODE" in
  capture)
    git rev-parse HEAD > "$DIR/commit.txt"
    for v in q16 q8; do
      so=$(build $v); cp "$so" "$DIR/lib-$v.so"
      disasm "$so" > "$DIR/disasm-$v.txt"
      fingerprints "$DIR/disasm-$v.txt" > "$DIR/fp-$v.txt"
      echo "$v: .text $(text_hash "$so") $(sections "$so") functions $(wc -l < "$DIR/fp-$v.txt")"
    done
    echo "captured at $(cat "$DIR/commit.txt")";;
  compare)
    echo "baseline $(cat "$DIR/commit.txt"), tree $(git rev-parse HEAD)$(git diff --quiet HEAD -- src Cargo.toml build.rs || echo ' + uncommitted src changes')"
    rc=0
    for v in q16 q8; do
      so=$(build $v)
      oh=$(text_hash "$DIR/lib-$v.so"); nh=$(text_hash "$so")
      disasm "$so" > "$DIR/disasm-$v-new.txt"
      fingerprints "$DIR/disasm-$v-new.txt" > "$DIR/fp-$v-new.txt"
      echo "== $v"
      echo "   .text hash   old $oh  new $nh $( [ "$oh" = "$nh" ] && echo IDENTICAL || echo differs)"
      echo "   sections old: $(sections "$DIR/lib-$v.so")"
      echo "   sections new: $(sections "$so")"
      if [ "$oh" != "$nh" ]; then
        # multiset match on (ninsns, exact) then (ninsns, masked)
        awk 'NR==FNR { ex[$1" "$2]++; ma[$1" "$3]++; next }
             { if (ex[$1" "$2] > 0) { ex[$1" "$2]--; e++ } else if (ma[$1" "$3] > 0) { ma[$1" "$3]--; r++ } else { c++; changed = changed "\n      " $4 " (" $1 " insns)" } }
             END { printf "   exact functions %d, reloc-only %d, changed %d%s\n", e, r, c, changed }' \
          "$DIR/fp-$v.txt" "$DIR/fp-$v-new.txt" | tee "$DIR/report-$v.txt"
        grep -q 'changed 0$' "$DIR/report-$v.txt" || rc=2
        echo "   baseline functions $(wc -l < "$DIR/fp-$v.txt"), new $(wc -l < "$DIR/fp-$v-new.txt")"
      fi
    done
    exit $rc;;
  *) echo "unknown mode $MODE" >&2; exit 1;;
esac
