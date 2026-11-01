#!/bin/sh
# kani-boolcmp.sh [FILE...]: flag ordering comparisons on bools in Kani code.
#
# Kani 0.68.0 with CBMC 6.11.0 cannot encode `<`, `<=`, `>` or `>=` on two
# non-constant bools, directly or through a tuple or struct that holds one
# (its derived PartialOrd): CBMC prints `warning: ignoring >=`, gives the
# comparison a fresh value, and the harness can pass vacuously. A terse
# run, which the kani check uses, hides that warning. Rewrite the
# comparison as `!a & b` (a < b), `!a | b` (a <= b), `a & !b` (a > b),
# `a | !b` (a >= b), or cast to u8 first; `==`, `!=`, `cmp`, `partial_cmp`
# and literal bools are fine.
#
#   sh scripts/kani-boolcmp.sh            every .rs file under src/
#   sh scripts/kani-boolcmp.sh FILE...    those files
#
# The scan is syntactic and conservative. It reads src/kani/** whole and,
# in other files, each item marked `#[cfg(kani)]` (or `cfg(any(.., kani))`,
# `cfg(all(kani, ..))`, a file-level `#![cfg(kani)]`); there it records the
# names bound as `bool` or as a tuple type with a bool element (`x: bool`,
# `a: &(u32, bool)`, `let (x, y): (bool, bool)`, `kani::any::<bool>()`,
# `= true`, a `for` pattern over a literal array of bools), then flags:
#   - an ordering operator with such a name, or a parenthesized tuple
#     holding one, on either side, unless the other side is a literal;
#   - `.lt(`, `.le(`, `.gt(` or `.ge(` called on such a name;
#   - `#[derive(PartialOrd)]` or `Ord` on a struct or enum holding a bool.
# Everywhere in src/ it also flags a function with a bool or
# bool-tuple parameter that orders that parameter (`key_less`), since a
# harness reaches the code it calls.
#
# A reviewed exception carries `kani-boolcmp: allow (<reason>)` in a
# comment on the flagged line or in the comment block just above it. The scan does not see
# types it cannot read on the line (a bool returned by a call, a field of
# a struct from another file); treat a hit as a lead and the regular-format
# run (kani-ignoring in .chainguard/ci/kani.yaml) as the backstop.
#
# Exit 0: no unallowed hit; 1: a hit (each printed as FILE:LINE: ...).
set -eu
if [ $# -eq 0 ]; then
  # The crate's file names hold no spaces.
  # shellcheck disable=SC2046
  set -- $(find src -name '*.rs' | sort)
fi
awk '
function reset_fn() { split("", nb); split("", nt); bound = 0 }
function reset_file() { reset_fn(); depth = 0; region = 0; pending = 0 }
# strip LINE: drop string and char literals and comments, so braces and
# operators inside them are not read.
function strip(s,   out, i, c, n, q) {
  out = ""; n = length(s); i = 1
  while (i <= n) {
    c = substr(s, i, 1)
    if (inblock) {
      if (substr(s, i, 2) == "*/") { inblock = 0; i += 2 } else i++
      continue
    }
    if (substr(s, i, 2) == "//") break
    if (substr(s, i, 2) == "/*") { inblock = 1; i += 2; continue }
    if (c == "\"") {
      i++
      while (i <= n) {
        q = substr(s, i, 1)
        if (q == "\\") { i += 2; continue }
        if (q == "\"") break
        i++
      }
      out = out "\"\""; i++; continue
    }
    if (c == "\047" && substr(s, i + 2, 1) == "\047" && substr(s, i + 1, 1) != "\\") {
      out = out "0"; i += 3; continue
    }
    if (c == "\047" && substr(s, i + 1, 1) == "\\") {
      q = index(substr(s, i + 2), "\047")
      if (q > 0) { out = out "0"; i += q + 2; continue }
    }
    out = out c; i++
  }
  return out
}
# boolpos TYPE: the comma-separated positions of the bool elements of a
# tuple type, "*" for bool itself, "" for neither.
function boolpos(t,   inner, n, parts, i, d, c, cur, k, out) {
  gsub(/[ \t]/, "", t)
  if (t == "bool") return "*"
  if (substr(t, 1, 1) != "(") return ""
  inner = substr(t, 2, length(t) - 2); d = 0; cur = ""; k = 0; out = ""
  for (i = 1; i <= length(inner) + 1; i++) {
    c = (i <= length(inner)) ? substr(inner, i, 1) : ","
    if (c == "(" || c == "[" || c == "<") d++
    if (c == ")" || c == "]" || c == ">") d--
    if (c == "," && d == 0) {
      if (cur == "bool" || index(cur, "bool") > 0 && substr(cur, 1, 1) == "(")
        out = out (out == "" ? "" : ",") k
      k++; cur = ""
    } else cur = cur c
  }
  return out
}
# bind NAME TYPE: record NAME as a bool or a bool tuple.
function bind(name, t,   p) {
  p = boolpos(t)
  if (p == "*") { nb[name] = 1; bound++ }
  else if (p != "") { nt[name] = p; bound++ }
}
# record S: the bindings on a stripped line.
function record(s,   r, m, name, t, rest, d, i, c, pat, j, parts) {
  r = s
  # NAME: TYPE (let, parameters, fields); TYPE is bool or a tuple type.
  while (match(r, /[A-Za-z_][A-Za-z0-9_]*[ \t]*:[ \t]*(&[ \t]*(mut[ \t]+)?)?(bool|\()/)) {
    m = substr(r, RSTART, RLENGTH); rest = substr(r, RSTART + RLENGTH - 1)
    name = m; sub(/[ \t]*:.*/, "", name)
    if (substr(m, length(m), 1) != "(") t = "bool"
    else {
      d = 0; t = ""
      for (i = 1; i <= length(rest); i++) {
        c = substr(rest, i, 1); t = t c
        if (c == "(") d++
        if (c == ")") { d--; if (d == 0) break }
      }
    }
    if (substr(r, RSTART - 1, 1) != ":" && substr(rest, 1, 1) != ":") bind(name, t)
    r = substr(r, RSTART + RLENGTH)
  }
  # let (A, B): (TYPE, TYPE): each name takes its element of the type.
  if (match(s, /let[ \t]+\([^)]*\)[ \t]*:[ \t]*\(/)) {
    pat = substr(s, RSTART, RLENGTH); sub(/^let[ \t]+\(/, "", pat); sub(/\)[ \t]*:.*$/, "", pat)
    rest = substr(s, RSTART + RLENGTH - 1); d = 0; t = ""
    for (i = 1; i <= length(rest); i++) {
      c = substr(rest, i, 1); t = t c
      if (c == "(") d++
      if (c == ")") { d--; if (d == 0) break }
    }
    t = boolpos(t); gsub(/[ \t]/, "", pat)
    j = split(pat, parts, ",")
    for (i = 1; i <= j; i++) {
      sub(/^mut/, "", parts[i])
      if (("," t ",") ~ ("," (i - 1) ",")) bind(parts[i], "bool")
    }
  }
  # let NAME = kani::any::<TYPE>() and let NAME = true/false.
  if (match(s, /let[ \t]+(mut[ \t]+)?[A-Za-z_][A-Za-z0-9_]*[ \t]*=[ \t]*/)) {
    m = substr(s, RSTART, RLENGTH); rest = substr(s, RSTART + RLENGTH)
    name = m; sub(/^let[ \t]+(mut[ \t]+)?/, "", name); sub(/[ \t]*=.*/, "", name)
    if (rest ~ /^(true|false)[ \t]*;/) bind(name, "bool")
    if (match(rest, /any::<[^>]*>/)) bind(name, substr(rest, RSTART + 6, RLENGTH - 7))
    if (match(rest, /any::<\(.*\)>/)) bind(name, substr(rest, RSTART + 6, RLENGTH - 7))
  }
  # for PATTERN in [literal bools ...]: every name of the pattern.
  if (match(s, /for[ \t]+.*[ \t]in[ \t]+\[/) && s ~ /\[[^]]*(true|false)/) {
    pat = s; sub(/^.*for[ \t]+/, "", pat); sub(/[ \t]in[ \t].*$/, "", pat)
    gsub(/[(),]/, " ", pat)
    j = split(pat, parts, /[ \t]+/)
    for (i = 1; i <= j; i++) if (parts[i] ~ /^[a-z_][A-Za-z0-9_]*$/ && parts[i] != "mut") bind(parts[i], "bool")
  }
}
# boolish EXPR: 1 when EXPR is a recorded bool, a bool element of a
# recorded tuple, a whole recorded tuple, or a tuple expression holding one.
function boolish(e,   root, acc, k, inner, d, cur, i, c) {
  gsub(/^[ \t!&*]+|[ \t]+$/, "", e)
  if (substr(e, 1, 1) == "(") {
    # A parenthesized expression or tuple: boolish when one of its
    # top-level elements is a boolish name.
    inner = substr(e, 2, length(e) - 2); d = 0; cur = ""
    for (i = 1; i <= length(inner) + 1; i++) {
      c = (i <= length(inner)) ? substr(inner, i, 1) : ","
      if (c == "(" || c == "[") d++
      if (c == ")" || c == "]") d--
      if (c == "," && d == 0) {
        if (cur ~ /^[ \t!&*]*[A-Za-z_][A-Za-z0-9_]*(\.[0-9]+)*[ \t]*$/ && boolish(cur)) return 1
        if (cur ~ /^[ \t]*\(/ && boolish(cur)) return 1
        cur = ""
      } else cur = cur c
    }
    return 0
  }
  root = e; sub(/\..*/, "", root)
  acc = e; if (!sub(/^[^.]*\./, "", acc)) acc = ""
  if (root in nb && acc == "") return 1
  if (root in nt) {
    if (acc == "") return 1
    k = acc; sub(/\..*/, "", k)
    if (("," nt[root] ",") ~ ("," k ",")) return 1
  }
  return 0
}
function literal(e) {
  gsub(/^[ \t!&*-]+|[ \t]+$/, "", e)
  return e ~ /^([0-9][0-9_]*([a-z][a-z0-9]*)?|true|false)$/
}
# operand LINE POS DIR: the expression next to the operator at POS.
function operand(s, pos, dir,   i, c, d, e) {
  if (dir < 0) {
    i = pos - 1
    while (i > 0 && substr(s, i, 1) ~ /[ \t]/) i--
    if (substr(s, i, 1) == ")") {
      d = 0
      for (; i > 0; i--) {
        c = substr(s, i, 1)
        if (c == ")") d++
        if (c == "(") { d--; if (d == 0) break }
      }
      e = substr(s, i, pos - i)
      if (i > 1 && substr(s, i - 1, 1) ~ /[A-Za-z0-9_]/) return "call"
      return e
    }
    e = substr(s, 1, pos - 1)
    if (match(e, /[!&*]*[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z0-9_]+)*[ \t]*$/)) {
      # A field or method of something longer (`f(x).b`, `a::b`) is not a name.
      if (RSTART > 1 && substr(e, RSTART - 1, 1) ~ /[.):]/) return "call"
      return substr(e, RSTART, RLENGTH)
    }
    return ""
  }
  e = substr(s, pos)
  sub(/^[ \t]+/, "", e)
  if (substr(e, 1, 1) == "(") {
    d = 0
    for (i = 1; i <= length(e); i++) {
      c = substr(e, i, 1)
      if (c == "(") d++
      if (c == ")") { d--; if (d == 0) break }
    }
    return substr(e, 1, i)
  }
  if (match(e, /^[!&*-]*[A-Za-z0-9_]+(\.[A-Za-z0-9_]+)*/)) {
    if (substr(e, RLENGTH + 1, 1) ~ /[(:]/) return "call"
    return substr(e, RSTART, RLENGTH)
  }
  return ""
}
function hit(msg) {
  if (allow_line == FNR || allow_line == FNR - 1) return
  printf "%s:%d: %s\n", FILENAME, FNR, msg
  bad = 1
}
# scan S: flag the comparisons on a stripped line.
function scan(s,   i, n, c, p, nx, op, l, r) {
  n = length(s)
  for (i = 1; i <= n; i++) {
    c = substr(s, i, 1)
    if (c != "<" && c != ">") continue
    p = substr(s, i - 1, 1); nx = substr(s, i + 1, 1)
    if (nx == c || p == c) continue                     # << >> <<= >>=
    if (c == ">" && (p == "-" || p == "=")) continue    # -> => ==>
    if (c == "<" && p == ":") continue                  # ::<
    if (c == "<" && nx == "=" && substr(s, i + 2, 1) == "=") { i += 2; continue } # <== <==>
    op = c; if (nx == "=") op = c "="
    # rustfmt (the rustfmt check) spaces binary operators and never a
    # generic bracket, so an unspaced `<` or `>` is a generic.
    if (p !~ /[ \t]/ || (substr(s, i + length(op), 1) !~ /[ \t]/ && i + length(op) <= n)) continue
    l = operand(s, i, -1); r = operand(s, i + length(op), 1)
    if ((boolish(l) && !literal(r)) || (boolish(r) && !literal(l)))
      hit("ordering comparison `" op "` on a bool or bool tuple: " l " " op " " r)
    i += length(op) - 1
  }
  r = s
  while (match(r, /[A-Za-z_][A-Za-z0-9_.]*\.(lt|le|gt|ge)\(/)) {
    l = substr(r, RSTART, RLENGTH); sub(/\.(lt|le|gt|ge)\($/, "", l)
    if (boolish(l)) hit("ordering method on a bool or bool tuple: " substr(r, RSTART, RLENGTH))
    r = substr(r, RSTART + RLENGTH)
  }
}
FNR == 1 {
  reset_file(); inblock = 0; whole = (FILENAME ~ /(^|\/)src\/kani\//)
  allow_line = 0; derive_ord = 0; instruct = 0; inparams = 0; fnd = -1; fnopen = 0
}
{
  # The allow marker covers its own line and the line after it; a comment
  # block that holds it extends to the end of the block.
  if ($0 ~ /kani-boolcmp: allow \(.+\)/) allow_line = FNR
  else if (allow_line == FNR - 1 && $0 ~ /^[ \t]*\/\//) allow_line = FNR
  s = strip($0)
  # cfg(kani), cfg(any(.., kani, ..)), cfg(all(kani, ..)); not cfg(not(kani)).
  # An inner #![cfg(kani)] before any item makes the whole file Kani code.
  if (s ~ /#!?\[cfg\(/ && s ~ /[(, ]kani[,) ]/ && s !~ /not\([^)]*kani/) {
    if (s ~ /#!\[/ && depth == 0) whole = 1
    else if (!region) pending = 1
  }
  kani = whole || region
  # A function header: forget the previous function and read its
  # parameters, everywhere in src/: a function that orders a bool
  # parameter is reachable from a harness. Kani code also records its
  # local bindings. The bindings last until the function body closes.
  if (s ~ /(^|[ \t])fn[ \t]+[A-Za-z_]/) {
    reset_fn(); record(s); fnd = depth; fnopen = 0
    pd = gsub(/\(/, "(", s) - gsub(/\)/, ")", s); inparams = (pd > 0)
  } else if (inparams) {
    record(s); pd += gsub(/\(/, "(", s) - gsub(/\)/, ")", s); if (pd <= 0) inparams = 0
  } else if (kani) record(s)
  # A struct or enum that derives PartialOrd or Ord: any bool in it, as a
  # field, a tuple element or a variant payload.
  if (kani && s ~ /#\[derive\(.*(PartialOrd|Ord)/) derive_ord = FNR
  if (kani && s ~ /(^|[ \t])(struct|enum)[ \t]/) { instruct = (derive_ord && derive_ord >= FNR - 3); sdepth = depth }
  if (instruct && s ~ /(^|[^A-Za-z0-9_])bool([^A-Za-z0-9_]|$)/) hit("type with a bool derives PartialOrd or Ord (line " derive_ord ")")
  if (kani || bound > 0) scan(s)
  # Brace depth, for the end of a #[cfg(kani)] item, a type and a function.
  n = length(s)
  for (i = 1; i <= n; i++) {
    c = substr(s, i, 1)
    if (c == "{") {
      depth++
      if (pending) { region = depth; pending = 0 }
      if (fnd >= 0 && depth == fnd + 1 && !inparams) fnopen = 1
    }
    if (c == "}") {
      if (region && depth == region) { region = 0; reset_fn() }
      if (instruct && depth == sdepth + 1) instruct = 0
      if (fnopen && depth == fnd + 1) { reset_fn(); fnd = -1; fnopen = 0 }
      depth--
    }
    if (c == ";") {
      if (pending) pending = 0
      if (instruct && depth == sdepth) instruct = 0          # struct T(u32, u64);
      if (fnd >= 0 && !fnopen && !inparams && depth == fnd) { reset_fn(); fnd = -1 }  # fn f(..);
    }
  }
}
END { exit bad }
' "$@"
