#!/bin/sh
# ci/codegen-notices.sh ARCH REPORT: print the verdict lines of a
# `scripts/codegen-diff.sh compare` report as `::notice` workflow commands,
# which Checks lists in the check run's output text (ci/codegen.sh). Per
# variant (`== q16`, `== q8`): its `functions old ..` line, or `.text
# identical` when the hash matched, or the codegen-fncmp.py failure; then
# one notice per `### changed:` function (with its demangled name, when
# the report gives one) and per baseline function with no counterpart, at
# most 40 per variant, then a count of the rest. Each notice's title is
# `codegen ARCH VARIANT`. ci/codegen-fncmp-test.sh checks it on
# scripts/testdata/codegen-notices.
set -eu
[ $# -eq 2 ] || { echo "usage: $0 ARCH REPORT" >&2; exit 1; }
awk -v arch="$1" '
  function note(msg) {
    gsub(/%/, "%25", msg)
    print "::notice title=codegen " arch " " v "::" msg
  }
  # A changed function waits for the `= demangled` line that may follow.
  function put() { if (held != "") { if (++n <= 40) note(held); held = "" } }
  function flush() { put(); if (n > 40) note((n - 40) " more changed functions (see the log)") }
  /^== / { flush(); v = $2; n = 0; orphan = 0; next }
  v == "" { next }
  held != "" && /^       = / { sub(/^       = /, ""); held = held " = " $0; put(); next }
  { put() }
  /^   \.text hash .* IDENTICAL$/ { note(".text identical: byte-identical machine code"); next }
  /^   functions old / { sub(/^   /, ""); note($0); next }
  /^   scripts\/codegen-fncmp\.py failed/ { sub(/^   /, ""); note($0); next }
  /^   ### changed: / { sub(/^   ### /, ""); held = $0; next }
  /^   baseline functions with no counterpart/ { orphan = 1; next }
  orphan && /^      [^ ]/ { sub(/^ +/, ""); held = "no counterpart: " $0; put(); next }
  { orphan = 0 }
  END { flush() }
' "$2"
