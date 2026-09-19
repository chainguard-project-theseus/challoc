#!/usr/bin/env bash
# ralph-lane.sh: the coordinator's helper for the parallel implementation
# lanes (CLAUDE.md, Environment; docs/plan.md, "Execution in lanes").
#
#   scripts/ralph-lane.sh <lane> [base]   prepare the lane: create the worktree
#                                         ../challoc-wt/<lane> on branch loop/<lane>
#                                         from base (default main) if absent, and
#                                         print the environment its iteration exports
#   scripts/ralph-lane.sh --merge <lane>  rebase loop/<lane> onto main, fast-forward
#                                         main to it (refusing on a rebase conflict
#                                         and listing the conflicting files), fold
#                                         PROGRESS.d/<lane>-*.md into PROGRESS.md and
#                                         delete them in the same commit, then print
#                                         the make test and scorecard commands to run
#
# Runs from the main checkout (the one whose .git is a directory), shell only,
# no network: nothing is fetched or pushed. The lane's branch is rebased in the
# lane's own worktree (a branch checked out there cannot be rebased from here);
# a temporary worktree is used when the lane's is gone.
set -euo pipefail

die() { echo "ralph-lane: $*" >&2; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
main_checkout() { [ -d "$ROOT/.git" ] || die "run from the main checkout ($ROOT/.git is not a directory: this is a worktree)"; }
WT_BASE="$(dirname "$ROOT")/challoc-wt"
HOST_ARCH="$(uname -m | sed -e 's/x86_64/amd64/' -e 's/aarch64/arm64/')"

lane_env() { # lane_env LANE: the environment a lane's iteration exports
  local lane="$1"
  cat <<ENV
export CHALLOC_GCE_PREFIX=challoc-$lane
export TARGET_VOLUME=challoc-target-$lane-$HOST_ARCH
unset CHALLOC_GCE_WARM
ENV
}

cmd_prepare() {
  local lane="$1" base="${2:-main}" wt branch
  case "$lane" in ''|-*|*/*|*' '*) die "lane must be a plain name (lane-a, lane-b, ...)";; esac
  branch="loop/$lane"; wt="$WT_BASE/$lane"
  git rev-parse --verify --quiet "$base^{commit}" >/dev/null || die "no such base: $base"
  if [ -e "$wt" ]; then
    echo "ralph-lane: $wt exists ($(git -C "$wt" branch --show-current 2>/dev/null || echo '?') at $(git -C "$wt" rev-parse --short HEAD 2>/dev/null || echo '?'))"
  else
    mkdir -p "$WT_BASE"
    if git show-ref --verify --quiet "refs/heads/$branch"; then
      git worktree add "$wt" "$branch"
    else
      git worktree add -b "$branch" "$wt" "$base"
    fi
    echo "ralph-lane: created $wt on $branch from $base ($(git rev-parse --short "$base"))"
  fi
  cat <<MSG

Lane $lane
  worktree  $wt
  branch    $branch
  VMs       challoc-$lane-arm64, challoc-$lane-amd64 (create + provision once; matrix cells challoc-$lane-matrix-<cell>)
  progress  $wt/PROGRESS.d/$lane-<task>.md (never PROGRESS.md, never bench/results/scorecard-*.md)

The iteration exports, before anything else runs:

$(lane_env "$lane")
cd $wt

(TARGET_VOLUME carries the host arch; an emulated run passes
TARGET_VOLUME=challoc-target-$lane-amd64 with PLATFORM=linux/amd64.)

Prompt preamble for the iteration, ahead of scripts/ralph-prompt.md:
  "You are lane $lane (Lane mode): worktree $wt, branch $branch, tasks per
  docs/plan.md 'Execution in lanes'; export the environment above first."

When the lane reports: scripts/ralph-lane.sh --merge $lane
MSG
}

# fold_block FILE SECTION KEY BLOCKFILE: insert the bullet block in BLOCKFILE
# into PROGRESS.md (FILE) under the "## SECTION" header, after the last
# bullet whose first line starts with "- KEY " (the phase grouping), else
# after the section's last bullet, else right after the header.
fold_block() {
  local file="$1" section="$2" key="$3" blk="$4" tmp
  tmp="$(mktemp "$ROOT/.tmp-fold.XXXXXX")"
  awk -v section="$section" -v key="$key" -v blk="$blk" '
    function block_end(i,   j) { j = i; while (j + 1 <= n && L[j + 1] != "" && L[j + 1] !~ /^- / && L[j + 1] !~ /^## /) j++; return j }
    { L[NR] = $0; n = NR }
    END {
      hdr = 0; stop = n
      for (i = 1; i <= n; i++) if (L[i] == "## " section) { hdr = i; break }
      if (!hdr) { print "ralph-lane: no \"## " section "\" in PROGRESS.md" > "/dev/stderr"; exit 3 }
      for (i = hdr + 1; i <= n; i++) if (L[i] ~ /^## /) { stop = i - 1; break }
      at = 0; last = 0
      for (i = hdr + 1; i <= stop; i++) {
        if (L[i] ~ /^- /) { e = block_end(i); last = e; if (index(L[i], "- " key " ") == 1 || index(L[i], "- " key ":") == 1) at = e }
      }
      if (!at) at = last ? last : hdr + 1
      for (i = 1; i <= n; i++) {
        print L[i]
        if (i == at) { while ((getline line < blk) > 0) print line; close(blk) }
      }
    }' "$file" > "$tmp"
  mv "$tmp" "$file"
}

# fold_file SRC LANE: every bullet of every known section of SRC into PROGRESS.md.
fold_file() {
  local src="$1" section blocks blk n=0 first key
  for section in "Done" "Design decisions" "Notes for the human"; do
    blocks="$(mktemp "$ROOT/.tmp-blocks.XXXXXX")"
    # The section's bullet blocks, each followed by a marker line.
    awk -v section="$section" '
      $0 == "## " section { on = 1; next }
      /^## / { if (on && inblk) print "@@BLOCK@@"; on = 0; inblk = 0; next }
      !on { next }
      /^- / { if (inblk) print "@@BLOCK@@"; inblk = 1; print; next }
      $0 == "" { if (inblk) print "@@BLOCK@@"; inblk = 0; next }
      inblk { print }
      END { if (on && inblk) print "@@BLOCK@@" }' "$src" > "$blocks"
    blk="$(mktemp "$ROOT/.tmp-block.XXXXXX")"
    : > "$blk"
    while IFS= read -r line; do
      if [ "$line" = "@@BLOCK@@" ]; then
        first="$(head -1 "$blk")"
        key="$(echo "$first" | sed -nE 's/^- (Phase [0-9.]+).*/\1/p')"
        [ -n "$key" ] || key="@@none@@"
        fold_block PROGRESS.md "$section" "$key" "$blk"
        n=$((n + 1))
        : > "$blk"
      else
        printf '%s\n' "$line" >> "$blk"
      fi
    done < "$blocks"
    rm -f "$blocks" "$blk"
  done
  echo "ralph-lane: folded $n entr$( [ "$n" = 1 ] && echo y || echo ies) from $src"
}

cmd_merge() {
  local lane="$1" branch wt tmpwt= old new files conflicts
  case "$lane" in ''|-*|*/*|*' '*) die "--merge needs a lane name";; esac
  branch="loop/$lane"; wt="$WT_BASE/$lane"
  git show-ref --verify --quiet "refs/heads/$branch" || die "no branch $branch"
  [ "$(git branch --show-current)" = main ] || die "the main checkout must have main checked out (it has '$(git branch --show-current)')"
  [ -z "$(git status --porcelain)" ] || die "the main checkout is not clean; commit or stash first"
  if [ -d "$wt" ]; then
    [ "$(git -C "$wt" branch --show-current)" = "$branch" ] || die "$wt has '$(git -C "$wt" branch --show-current)' checked out, not $branch"
    [ -z "$(git -C "$wt" status --porcelain)" ] || die "$wt is not clean; the lane must commit everything first"
  else
    tmpwt="$WT_BASE/.merge-$lane"
    rm -rf "$tmpwt"; git worktree prune
    git worktree add "$tmpwt" "$branch" >/dev/null
    wt="$tmpwt"
  fi
  old="$(git rev-parse main)"
  echo "ralph-lane: rebasing $branch ($(git rev-parse --short "$branch")) onto main ($(git rev-parse --short main)) in $wt"
  if ! git -C "$wt" rebase main >/dev/null 2>&1; then
    conflicts="$(git -C "$wt" diff --name-only --diff-filter=U 2>/dev/null || true)"
    git -C "$wt" rebase --abort 2>/dev/null || true
    [ -z "$tmpwt" ] || git worktree remove --force "$tmpwt"
    echo "ralph-lane: REFUSED: rebase of $branch onto main conflicts in:" >&2
    echo "${conflicts:-(no unmerged paths reported; run 'git -C $wt rebase main' by hand)}" | sed 's/^/    /' >&2
    exit 2
  fi
  new="$(git rev-parse "$branch")"
  if [ "$new" = "$old" ]; then
    [ -z "$tmpwt" ] || git worktree remove --force "$tmpwt"
    echo "ralph-lane: $branch has nothing beyond main; nothing to merge"; exit 0
  fi
  git merge --ff-only -q "$branch"
  echo "ralph-lane: main fast-forwarded $(git rev-parse --short "$old") -> $(git rev-parse --short "$new")"
  [ -z "$tmpwt" ] || git worktree remove --force "$tmpwt"

  files="$(git diff --name-only "$old" "$new")"
  echo "ralph-lane: files the lane changed:"
  echo "$files" | sed 's/^/    /'
  if echo "$files" | grep -qE '^(PROGRESS\.md|bench/results/scorecard-(aarch64|x86_64)\.md)$'; then
    echo "ralph-lane: WARNING: the lane touched PROGRESS.md or a reference scorecard file; lanes must not (check the diff)" >&2
  fi

  # Fold the lane's progress notes, delete them, one commit.
  local notes n=0 f
  notes="$(git ls-files "PROGRESS.d/$lane-*.md")"
  if [ -z "$notes" ]; then
    echo "ralph-lane: no PROGRESS.d/$lane-*.md to fold (the lane wrote none)"
  else
    for f in $notes; do fold_file "$f" "$lane"; n=$((n + 1)); done
    git rm -q -- $notes
    git add PROGRESS.md
    git commit -q -m "PROGRESS: fold lane $lane ($(echo $notes | sed 's#PROGRESS.d/##g' | tr ' ' ',')) at $(git rev-parse --short "$new")

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
    echo "ralph-lane: folded $n file(s) into PROGRESS.md, commit $(git rev-parse --short HEAD)"
  fi

  cat <<NEXT

Next, from this checkout ($ROOT, main at $(git rev-parse --short HEAD)), with CHALLOC_GCE_PREFIX unset:

  make test                                   # the merged tree
  bench/gce/challoc-bench scorecard arm64      # once per merge batch, on the reference VMs
  bench/gce/challoc-bench scorecard amd64
  git add bench/results/scorecard-aarch64.md bench/results/scorecard-x86_64.md
  git commit -m "PROGRESS: scorecard lines for $(git rev-parse --short "$new")"

Then the lane continues from the merged main: in $WT_BASE/$lane, 'git rebase main' is a no-op now; start its next task.
NEXT
}

case "${1:-}" in
  --merge) shift; main_checkout; cmd_merge "${1:-}" ;;
  -h|--help|'') sed -n '2,/^set -euo pipefail/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 1 ;;
  *) main_checkout; cmd_prepare "$@" ;;
esac
