#!/bin/sh
# verus/make/headers.sh [PORT_ROOT]: hold a port's copies of the kit's
# Verus files to the kit's text (`make verus-headers`,
# verus/make/proved-port.mk). Vendored from the proved-port kit's
# kit/make by kit/sync.sh, which copies beside it, under
# verus/make/templates/, the templates kit/make/VERUS-FILES names, each
# with `.tmpl` added to its name (verus/FLOOR.tmpl), as they are at the
# kit commit verus/count/KIT records: verus/FLOOR, RLIMIT,
# REACH, MINTS and count.toml, .chainguard/ci/verus.yaml, miri.yaml and
# .chainguard/reviewers.yaml, and .chainguard/review-prompt.md. The kit's
# other templates (conformance, consumers, fuzz, test, lint, the
# environments, develop.yaml, deps.yaml) are starting points the port
# reshapes, and this does not hold them. The vendored copies themselves
# (verus/count, verus/make) are kit/sync.sh --check's.
#
# For each of those templates the port has (one it lacks is skipped and
# named), the header is the file's leading `#` comment lines up to its
# `# kit:` line. The port's header must hold the template's text, where
# each placeholder (`{NAME}`, `{NAME: guidance}`, `{Guidance}`) stands for
# any text the port fills in, and a placeholder that fills whole lines
# (a list item, a sentence of its own) may be deleted with its lines. The
# comparison ignores white space and the `#` that starts each line, so a
# filled placeholder may rewrap its paragraph. Comment lines the port
# adds after its `# kit:` line are its own.
#
# .chainguard/review-prompt.md is Markdown, with no `#` header: the port's
# prompt must hold, anywhere and ignoring white space, the template's
# vendored-kit rule, its `- The vendored kit` list item, which says how
# verus/count, verus/make and KIT may change. It carries no kit line.
#
# VERUS_FLOOR (from the environment, as proved-port.mk passes it; a path
# under PORT_ROOT, default verus/FLOOR) is the port's ratchet file, which
# the verus/FLOOR template's header holds (chainz: verus/SHARES).
#
# The port's `# kit: proved-port VERSION` line must name the commit
# verus/count/KIT records (in full or a prefix of 7 or more characters),
# or its tag: the headers are the kit's text at that version. VERSION is
# one word, or the templates' `vX.Y.Z (SHA)` form, where SHA names that
# commit as above and vX.Y.Z is its tag; when the commit has no tag (a
# copy taken between releases) vX.Y.Z is the release before it, which
# KIT does not record, so any vX.Y.Z passes.
#
# Exit 0: every header matches; 1: one drifted or a file is missing; 2:
# usage.
set -eu

[ $# -le 1 ] || { echo "usage: $0 [PORT_ROOT]" >&2; exit 2; }
root=${1:-.}
floor=${VERUS_FLOOR:-verus/FLOOR}
templates=$root/verus/make/templates
kit=$root/verus/count/KIT
[ -d "$templates" ] || { echo "headers: no $templates; re-run kit/sync.sh" >&2; exit 1; }
[ -f "$kit" ] || { echo "headers: no $kit; re-run kit/sync.sh" >&2; exit 1; }
sha=$(sed -n 's/^commit //p' "$kit")
tag=$(sed -n 's/^tag //p' "$kit")
[ -n "$sha" ] || { echo "headers: $kit records no commit" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# header FILE: FILE's leading `#` lines up to and including its first
# `# kit:` line; fails when the leading lines have none.
header() {
  awk '!/^#/ { exit } { print } /^# kit:/ { found = 1; exit } END { exit !found }' "$1"
}

# kit_rule FILE: FILE's vendored-kit rule, the list item that starts
# `- The vendored kit`, up to the next item, heading or blank line.
kit_rule() {
  awk '
    on && (/^- / || /^#/ || /^[ \t]*$/) { exit }
    /^- The vendored kit/ { on = 1 }
    on { print }' "$1"
}

# version LINE: the VERSION of a `# kit: proved-port VERSION` line, with
# the `vX.Y.Z (SHA)` form printed as `vX.Y.Z SHA`.
version() {
  echo "$1" | sed -n \
    -e 's/^# kit: proved-port \(v[0-9][0-9.]*\) (\([0-9a-f]*\)) *$/\1 \2/p' \
    -e 's/^# kit: proved-port \([^ ]*\) *$/\1/p'
}

# commit SHA: whether SHA is the commit KIT records, in full or a prefix
# of 7 or more characters.
commit() {
  [ ${#1} -ge 7 ] || return 1
  case $sha in "$1"*) return 0 ;; esac
  return 1
}

# known VERSION: whether VERSION names the commit or tag KIT records.
known() {
  [ -n "$1" ] || return 1
  case $1 in
    *' '*)
      commit "${1#* }" || return 1
      [ "$tag" = none ] || [ "${1%% *}" = "$tag" ]
      ;;
    "$sha" | "$tag") [ "$1" != none ] ;;
    *) commit "$1" ;;
  esac
}

# match TEMPLATE PORT: whether the port header (PORT, its `# kit:` line
# left out) holds the template's text (TEMPLATE, likewise), each
# placeholder matching any text. On a mismatch it prints the first
# stretch of the template's text the port does not have.
match() {
  awk '
    FNR == NR { t[++nt] = $0; next }
    { p[++np] = $0 }
    # squeeze S: S without its white space.
    function squeeze(s) { gsub(/[ \t\n]+/, "", s); return s }
    # tidy S: S with each run of white space one space, for messages.
    function tidy(s) { gsub(/[ \t\n]+/, " ", s); sub(/^ /, "", s); sub(/ $/, "", s); return s }
    # lit(S): end the current literal stretch with S, start a placeholder.
    function lit(s) { n++; L[n] = s }
    END {
      # The template with each line'"'"'s leading # dropped, joined by
      # newlines.
      text = ""
      for (i = 1; i <= nt; i++) { s = t[i]; sub(/^#/, "", s); text = text s "\n" }
      ptext = ""
      for (i = 1; i <= np; i++) { s = p[i]; sub(/^#/, "", s); ptext = ptext s "\n" }
      # Split the template at its placeholders: a { followed by a
      # capital letter, up to the next }.
      n = 0; cur = ""; lead = 0; trail = 0
      while (match(text, /[{][A-Z]/)) {
        before = substr(text, 1, RSTART - 1)
        rest = substr(text, RSTART)
        shut = index(rest, "}")
        if (shut == 0) { cur = cur before; text = rest; break }
        after = substr(rest, shut + 1)
        # A placeholder that fills whole lines takes them with it: only
        # spaces or a list dash before it on its line, only spaces or a
        # stop after it on its line.
        k = 0
        for (j = length(before); j > 0; j--) if (substr(before, j, 1) == "\n") { k = j; break }
        head = substr(before, k + 1)
        nl = index(after, "\n")
        tail = nl ? substr(after, 1, nl - 1) : after
        if (head ~ /^[ -]*$/ && tail ~ /^[.,;]?[ ]*$/) {
          before = substr(before, 1, k)
          after = nl ? substr(after, nl + 1) : ""
        }
        cur = cur before
        if (n == 0 && squeeze(cur) == "") lead = 1
        else lit(cur)
        cur = ""
        text = after
      }
      cur = cur text
      if (squeeze(cur) == "") trail = 1
      else lit(cur)
      if (n > 0 && squeeze(L[n]) == "") n--
      P = squeeze(ptext)
      pos = 1
      for (i = 1; i <= n; i++) {
        s = squeeze(L[i])
        if (s == "") continue
        if (i == 1 && !lead) {
          if (substr(P, 1, length(s)) != s) { bad = i; break }
          pos = length(s) + 1
          if (i == n && !trail && length(P) != length(s)) { bad = i; break }
          continue
        }
        if (i == n && !trail) {
          at = length(P) - length(s) + 1
          if (at < pos || substr(P, at) != s) { bad = i; break }
          pos = length(P) + 1
          continue
        }
        at = index(substr(P, pos), s)
        if (at == 0) { bad = i; break }
        pos += at - 1 + length(s)
      }
      if (n == 0 && !lead && !trail && P != "") bad = 1
      if (bad) {
        m = tidy(L[bad])
        if (length(m) > 160) m = substr(m, 1, 160) "..."
        print m
        exit 1
      }
    }' "$1" "$2"
}

find "$templates" -type f | LC_ALL=C sort > "$tmp/list"
[ -s "$tmp/list" ] || { echo "headers: $templates holds no templates; re-run kit/sync.sh" >&2; exit 1; }
drift=
while IFS= read -r t; do
  # src: the vendored template; rel: the kit template it copies, without
  # the `.tmpl` kit/sync.sh adds so that no vendored path ends in the
  # name of a file the port's code owners protect (reviewers.yaml).
  src=verus/make/templates/${t#"$templates"/}
  rel=${t#"$templates"/}
  rel=${rel%.tmpl}
  # path: the port's file the template rel holds.
  path=$rel
  [ "$rel" != verus/FLOOR ] || path=$floor
  f=$root/$path
  if [ ! -f "$f" ]; then
    echo "skip $path: the port has none"
    continue
  fi
  if [ "$rel" = .chainguard/review-prompt.md ]; then
    # The rule, between placeholders that stand for the rest of the
    # port's prompt, so match finds it anywhere in the file.
    {
      echo '{The port text before it}'
      kit_rule "$t"
      echo '{The port text after it}'
    } > "$tmp/t.body"
    # With no rule between them, the two placeholders match any prompt:
    # fail rather than pass every port's.
    if [ "$(wc -l < "$tmp/t.body")" -le 2 ]; then
      echo "headers: $src has no \`- The vendored kit\` item; re-run kit/sync.sh" >&2
      drift=1
      continue
    fi
    if ! missing=$(match "$tmp/t.body" "$f"); then
      echo "headers: $path: the vendored-kit rule drifted from the kit's (the \`- The vendored kit\` item of $src); it lacks: $missing" >&2
      drift=1
      continue
    fi
    echo "ok $path"
    continue
  fi
  # A template with no kit line would hold nothing, so it fails rather
  # than pass every port's copy.
  if ! header "$t" > "$tmp/t"; then
    echo "headers: $src has no \`# kit: proved-port\` line, so it holds nothing; re-run kit/sync.sh" >&2
    drift=1
    continue
  fi
  if ! header "$f" > "$tmp/p"; then
    echo "headers: $path: its leading comment has no \`# kit: proved-port VERSION\` line (the kit's: $src)" >&2
    drift=1
    continue
  fi
  line=$(tail -n 1 "$tmp/p")
  v=$(version "$line")
  if ! known "$v"; then
    echo "headers: $path: \`$line\` does not name the kit commit verus/count/KIT records ($sha, tag $tag)" >&2
    drift=1
  fi
  sed '$d' "$tmp/t" > "$tmp/t.body"
  sed '$d' "$tmp/p" > "$tmp/p.body"
  if ! missing=$(match "$tmp/t.body" "$tmp/p.body"); then
    echo "headers: $path: the header drifted from the kit's ($src); it lacks: $missing" >&2
    drift=1
    continue
  fi
  echo "ok $path"
done < "$tmp/list"
if [ -n "$drift" ]; then
  echo "headers: restore the kit's text, or change kit/templates in the proved-port kit and re-sync" >&2
  exit 1
fi
