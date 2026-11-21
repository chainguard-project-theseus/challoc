#!/bin/sh
# ci/codegen-notices.sh ARCH REPORT: print the verdict lines of a
# `scripts/codegen-diff.sh compare` report as `::notice` workflow commands,
# which Checks lists in the check run's output text (ci/codegen.sh). The
# output text shows only the first 20, so each variant (`== q16`, `== q8`)
# gets at most two: its `functions old ..` line, or `.text identical` when
# the hash matched, or the codegen-fncmp.py failure; then, when functions
# changed, one `changed (N): ..` notice listing them, each as its
# demangled name (the report's mangled one when it gives none) without
# crate hashes, then `(old -> new)` instruction counts (`(old -> new;
# baseline M)` when the baseline is another function), `(new, N)` for a
# function with no baseline, or `(removed, N)` for a baseline function
# with no counterpart. At most 40 names, then a count of the rest. Each
# notice's title is `codegen ARCH VARIANT`. ci/codegen-fncmp-test.sh
# checks it on scripts/testdata/codegen-notices.
set -eu
[ $# -eq 2 ] || { echo "usage: $0 ARCH REPORT" >&2; exit 1; }
awk -v arch="$1" '
  function note(msg) {
    gsub(/%/, "%25", msg)
    print "::notice title=codegen " arch " " v "::" msg
  }
  function count(f) { sub(/^\(/, "", f); return f }
  # codegen-fncmp.py`s key for a mangled name: no crate hash or impl
  # disambiguator, and every backref spelled B_.
  function key(s) {
    gsub(/Cs[0-9A-Za-z]+_|Ms[0-9A-Za-z]*_/, "", s)
    gsub(/B[0-9A-Za-z]*_/, "B_", s)
    return s
  }
  # A changed function waits for the `= demangled` line that may follow.
  function put() {
    if (name == "") return
    gsub(/\[[0-9a-f]+\]/, "", name)
    if (++n <= 40) list = list (n > 1 ? "; " : "") name " (" insns ")"
    name = ""
  }
  function flush() {
    put()
    if (n > 0) note("changed (" n "): " list (n > 40 ? "; and " (n - 40) " more (see the log)" : ""))
    n = 0; list = ""
  }
  /^== / { flush(); v = $2; orphan = 0; next }
  v == "" { next }
  name != "" && /^       = / { sub(/^       = /, ""); name = $0; put(); next }
  { put() }
  /^   \.text hash .* IDENTICAL$/ { note(".text identical: byte-identical machine code"); next }
  /^   functions old / { sub(/^   /, ""); note($0); next }
  /^   scripts\/codegen-fncmp\.py failed/ { sub(/^   /, ""); note($0); next }
  # ### changed: NAME (A insns) vs baseline M (B insns)
  # ### changed: NAME (A insns) vs no baseline counterpart
  # When M is another function (codegen-fncmp.py pairs a leftover by
  # instruction similarity), the old count is M`s, so M is named too:
  # `(B -> A; baseline M)`. M under a new crate hash or impl
  # disambiguator, by codegen-fncmp.py`s key, is the same function.
  /^   ### changed: / {
    name = $3
    if ($0 ~ /vs no baseline counterpart$/) insns = "new, " count($4)
    else {
      insns = count($(NF - 1)) " -> " count($4)
      if (key($8) != key($3)) insns = insns "; baseline " $8
    }
    next
  }
  /^   baseline functions with no counterpart/ { orphan = 1; next }
  # NAME (B insns), or NAME = DEMANGLED (B insns)
  orphan && /^      [^ ]/ {
    name = $1
    if ($2 == "=") { name = $3; for (i = 4; i <= NF - 2; i++) name = name " " $i }
    insns = "removed, " count($(NF - 1))
    put(); next
  }
  { orphan = 0 }
  END { flush() }
' "$2"
