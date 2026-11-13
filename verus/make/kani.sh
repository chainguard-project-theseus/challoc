#!/bin/sh
# verus/make/kani.sh [--list] [ARGS]: the Kani harnesses (`make kani`,
# verus/make/proved-port.mk), vendored from the proved-port kit's
# kit/make by kit/sync.sh. Run it from the port's root. ARGS go to every
# `cargo kani` call, the `cargo kani list` that finds the harnesses too
# (before `list`, as KANI_FLAGS do), so `--features quantum8` lists the
# harnesses that feature gates.
#
# It runs every harness of the selected crates, one `cargo kani` call a
# crate, or one harness, or one shard of them, and fails when any call
# fails, when KANI_FAIL_PATTERN appears in a call's output, or when the
# selection leaves nothing to run. --list prints the harnesses instead,
# one `CRATE NAME FILE` a line, from `cargo kani list` (CRATE is `-` for
# cargo's default package selection, FILE the source file the list
# keys the harness by, from the workspace root). A harness is its name
# and its file: crates of cargo's default selection can each have a
# `tests::check`, and kani.sh counts, lists and shards each. It first
# runs `cargo fetch --locked`, since cargo kani takes no --locked.
#
# Nothing to run is a failure even where cargo kani would pass: cargo
# kani exits 0 on a crate with no harnesses, so before a run of every
# harness kani.sh lists them, and fails when the selected crates have
# none. A port with no harnesses yet declares so with KANI_NONE = yes in
# its Makefile, so its kani checks run from its first commit: kani.sh
# then lists the harnesses, passes when there are none, and fails when
# there are any (the declaration is stale, delete it so they run). An
# empty selection with no declaration still fails.
#
# The environment says what to run; empty is the same as unset:
#
#   KANI_CRATES        the packages with harnesses, each run with -p;
#                      empty runs cargo's default selection, no -p
#   KANI_ONLY          one crate of KANI_CRATES alone (pcre3's unicode half)
#   KANI_EXCLUDE       every crate of KANI_CRATES but this one (the rest)
#   KANI_HARNESS       one harness (Kani's --harness, which matches a
#                      substring of the full name) of the one selected
#                      crate: chainz's foreach of KANI_ONLY and KANI_HARNESS
#   KANI_SHARDS        split every harness of the selected crates into
#   KANI_SHARD         this many shards and run shard KANI_SHARD (1 to
#                      KANI_SHARDS) of them, each by its full name
#                      (--exact); shard 0 prints the split and runs
#                      nothing. Every shard splits them the same way.
#                      Harnesses of one name in several files of cargo's
#                      default selection go to one shard, since its
#                      --harness NAME runs all of them.
#   KANI_WEIGHTS       a file of `NAME SECONDS [alone]` rows, `#` lines
#                      comments (challoc's ci/kani-regular.sh table, NAME
#                      the full name `--list` prints) that the split balances
#                      by, longest first, each to the shard with the least
#                      time so far; a harness marked alone gets a shard of
#                      its own, from shard 1 up (they must leave a shard
#                      for the rest, or kani.sh exits 2), and one not
#                      listed counts as 2 seconds; a NAME's row weighs
#                      each harness of that name
#   KANI_FLAGS         flags for every cargo kani call, before `list` too
#                      (challoc: -Z unstable-options -Z stubbing
#                      -Z mem-predicates --ignore-global-asm, its features)
#   KANI_LIST_FLAGS    flags after `list` (challoc: -Z stubbing
#                      -Z mem-predicates)
#   KANI_JOBS          harnesses at once (-j); Kani takes -j only with
#                      terse output, so it implies --output-format terse
#   KANI_FORMAT        --output-format (terse, regular)
#   KANI_CBMC_ARGS     --cbmc-args, last (challoc: --verbosity 9)
#   KANI_FAIL_PATTERN  a grep -E pattern that fails a call whose output
#                      has it (challoc: `warning: ignoring`, CBMC leaving
#                      a bool comparison unconstrained). With it set, each
#                      call's output goes to target/kani/<crate>.log and
#                      only the per-harness lines are printed, with the
#                      log's tail on a failure.
#   KANI_VERSION       the release `cargo kani --version` must name
#   KANI_NONE          `yes`: the port has no harnesses yet, and a run
#                      passes only while that holds (above); any other
#                      value exits 2, and so does `yes` with KANI_HARNESS
#                      or KANI_SHARD, which pick harnesses
#
# Exit 0: every harness run verified, or KANI_NONE=yes and there are
# none; 1: one did not, nothing ran, or KANI_NONE=yes and there are some;
# 2: usage.
set -eu

out=target/kani

usage() {
  echo "kani: $*" >&2
  exit 2
}

list=
if [ "${1-}" = --list ]; then
  list=1
  shift
fi

crates=${KANI_CRATES:-}
only=${KANI_ONLY:-}
exclude=${KANI_EXCLUDE:-}
harness=${KANI_HARNESS:-}
shard=${KANI_SHARD:-}
shards=${KANI_SHARDS:-}
jobs=${KANI_JOBS:-}
format=${KANI_FORMAT:-}
pattern=${KANI_FAIL_PATTERN:-}
none=${KANI_NONE:-}

case "$none" in
  '') ;;
  yes) [ -z "$harness$shard$shards" ] || usage "KANI_NONE=yes declares no harnesses; KANI_HARNESS and KANI_SHARD pick some" ;;
  *) usage "KANI_NONE is yes (no harnesses yet) or empty, not $none" ;;
esac
case "$only$exclude$harness" in
  *[[:space:]]*) usage "KANI_ONLY, KANI_EXCLUDE and KANI_HARNESS each name one" ;;
esac
if [ -z "$crates" ] && [ -n "$only$exclude" ]; then
  usage "KANI_ONLY and KANI_EXCLUDE name a crate of KANI_CRATES, which is empty"
fi
for c in $only $exclude; do
  case " $crates " in
    *" $c "*) ;;
    *) usage "$c is not one of KANI_CRATES ($crates)" ;;
  esac
done
if [ -n "$shard$shards" ]; then
  case "$shard.$shards" in
    *[!0-9.]* | .* | *. | *.0) usage "KANI_SHARD and KANI_SHARDS are numbers, KANI_SHARDS at least 1" ;;
  esac
  [ "$shard" -le "$shards" ] || usage "KANI_SHARD $shard is past KANI_SHARDS $shards"
  [ -z "$harness" ] || usage "KANI_HARNESS and KANI_SHARD each pick the harnesses; set one"
fi
if [ -n "${KANI_WEIGHTS:-}" ] && [ ! -f "$KANI_WEIGHTS" ]; then
  usage "KANI_WEIGHTS: no file $KANI_WEIGHTS"
fi
if [ -n "$jobs" ]; then
  case "$format" in
    '') format=terse ;;
    terse) ;;
    *) usage "KANI_JOBS needs terse output; KANI_FORMAT is $format" ;;
  esac
fi

# The selected crates, `-` for cargo's default selection.
selected=
for c in ${crates:--}; do
  if [ -n "$only" ] && [ "$c" != "$only" ]; then continue; fi
  if [ "$c" = "$exclude" ]; then continue; fi
  selected="$selected $c"
done
# The same crate in KANI_ONLY and KANI_EXCLUDE leaves nothing to check,
# which is no pass.
[ -n "$selected" ] || { echo "kani: KANI_ONLY=\"$only\" KANI_EXCLUDE=\"$exclude\" leave no crate to check" >&2; exit 1; }
if [ -n "$harness" ]; then
  # shellcheck disable=SC2086
  n=$(echo $selected | wc -w)
  [ "$n" -eq 1 ] || usage "KANI_HARNESS needs one crate (KANI_ONLY); $n are selected"
fi

if [ -n "${KANI_VERSION:-}" ]; then
  case "$(cargo kani --version 2>/dev/null)" in
    *" $KANI_VERSION "* | *" $KANI_VERSION") ;;
    *) echo "kani: needs Kani $KANI_VERSION on PATH" >&2; exit 1 ;;
  esac
fi

mkdir -p "$out"
cargo fetch --locked

# pflag CRATE: the -p flag for CRATE, none for `-`, as one word: cargo
# kani's -p takes every bare word after it, so `-p CRATE list` would
# read `list` as a second package and refuse the list flags.
pflag() {
  [ "$1" = - ] || echo "-p=$1"
}

# fname CRATE: CRATE as a file name.
fname() {
  if [ "$1" = - ]; then echo default; else echo "$1"; fi
}

# harnesses CRATE [ARGS]: CRATE's harnesses, `CRATE NAME FILE` a line,
# from the kani-list.json `cargo kani list` writes, ARGS before `list`.
# The harnesses are the "standard-harnesses" and "contract-harnesses"
# maps, each a file key holding an array of names, one a line, as Kani
# 0.68.0 writes them; the "contracts" array names contract harnesses a
# second time, so it is not read. A line of those maps in another shape
# fails. The count must match the file's totals, which must give both
# counts, each a number on its own line. A file in another shape fails
# rather than read as no harnesses, which KANI_NONE=yes would pass.
harnesses() {
  lc=$1
  shift
  json=$out/list-$(fname "$lc").json
  # shellcheck disable=SC2046,SC2086
  cargo kani $(pflag "$lc") ${KANI_FLAGS:-} "$@" list ${KANI_LIST_FLAGS:-} --format json > /dev/null
  mv kani-list.json "$json"
  awk '
    /^ *"(standard|contract)-harnesses": *[{] *$/ { m = 1; next }
    !m { next }
    f == "" && /^ *[}],? *$/ { m = 0; next }
    f == "" && /^ *"[^"\\[:space:]]+": *\[ *$/ { f = $0; sub(/^ *"/, "", f); sub(/".*/, "", f); next }
    f != "" && /^ *"[A-Za-z0-9_:]+",? *$/ { n = $0; gsub(/[ ",]/, "", n); print n, f; next }
    f != "" && /^ *\],? *$/ { f = ""; next }
    { printf "kani: %s:%d: not a line of a harness map as Kani 0.68.0 writes it: %s\n", FILENAME, NR, $0 > "/dev/stderr"; exit 1 }
  ' "$json" > "$out/pairs" || exit 1
  LC_ALL=C sort "$out/pairs" > "$out/names"
  total=$(awk '
    /^ *"totals": *[{] *$/ { t = 1; next }
    t && /^ *"(standard|contract)-harnesses": *[0-9]+,? *$/ {
      v = $0; sub(/.*: */, "", v); s += v + 0; seen[$1] = 1; next
    }
    t && /}/ { t = 0 }
    END {
      if (!("\"standard-harnesses\":" in seen) || !("\"contract-harnesses\":" in seen)) print "-"
      else print s + 0
    }' "$json")
  if [ "$total" = - ]; then
    echo "kani: $json has no \"totals\" block giving \"standard-harnesses\" and \"contract-harnesses\", each on its own line; kani.sh reads the file Kani 0.68.0 writes" >&2
    exit 1
  fi
  if [ "$(wc -l < "$out/names")" -ne "$total" ]; then
    echo "kani: $json lists $total harnesses, read $(wc -l < "$out/names")" >&2
    exit 1
  fi
  awk -v c="$lc" '{ print c, $0 }' "$out/names"
}

# pick_shard: of the `CRATE NAME FILE` lines on stdin, shard $shard's.
# The split is of `CRATE NAME` units, what one --harness picks: under
# cargo's default selection it picks the name in every file, so those
# harnesses share a shard. The split goes to stderr.
pick_shard() {
  weights=${KANI_WEIGHTS:-/dev/null}
  awk -v shard="$shard" -v shards="$shards" -v def=2 -v weights="$weights" '
    FILENAME == weights { if (NF >= 2 && $1 !~ /^#/) { w[$1] = $2; if ($3 == "alone" && shards > 1) alone[$1] = 1 }; next }
    {
      u = $1 " " $2
      if (!(u in idx)) { n++; idx[u] = n; line[n] = u; name[n] = $2 }
      i = idx[u]; files[i]++; t[i] += ($2 in w) ? w[$2] : def
      r++; rec[r] = $0; of[r] = i
    }
    END {
      # A harness marked alone takes a shard of its own, from shard 1 up.
      first = 1; lone = 0
      for (i = 1; i <= n; i++) if (name[i] in alone) { at[i] = first; load[first] = t[i]; first++; lone += files[i] }
      if (first - 1 > shards) { print "kani: more harnesses marked alone than shards" > "/dev/stderr"; exit 2 }
      # The rest need a shard past them, or no shard would run them.
      if (first > shards && r > lone) {
        printf "kani: the %d harnesses marked alone fill all %d shards, leaving %d harnesses no shard\n", lone, shards, r - lone > "/dev/stderr"
        exit 2
      }
      # Longest first (ties by name), each to the least-loaded shard.
      for (i = 1; i <= n; i++) ord[i] = i
      for (i = 2; i <= n; i++)
        for (j = i; j > 1 && (t[ord[j]] > t[ord[j - 1]] || t[ord[j]] == t[ord[j - 1]] && line[ord[j]] < line[ord[j - 1]]); j--) {
          k = ord[j]; ord[j] = ord[j - 1]; ord[j - 1] = k
        }
      for (s = first; s <= shards; s++) load[s] = 0
      for (i = 1; i <= n; i++) {
        if (ord[i] in at) continue
        best = first
        for (s = first + 1; s <= shards; s++) if (load[s] < load[best]) best = s
        load[best] += t[ord[i]]; at[ord[i]] = best
      }
      for (s = 1; s <= shards; s++) {
        c = 0; for (i = 1; i <= n; i++) if (at[i] == s) c += files[i]
        printf "kani: shard %d: %d harnesses, ~%d s\n", s, c, load[s] > "/dev/stderr"
      }
      for (j = 1; j <= r; j++) if (at[of[j]] == shard) print rec[j]
    }' "$weights" -
}

if [ -n "$shards" ]; then
  : > "$out/all"
  for c in $selected; do harnesses "$c" "$@" >> "$out/all"; done
  pick_shard < "$out/all" > "$out/shard"
  echo "kani: shard $shard of $shards: $(wc -l < "$out/shard" | tr -d ' ') of $(wc -l < "$out/all" | tr -d ' ') harnesses"
  if [ -n "$list" ]; then
    cat "$out/shard"
    exit 0
  fi
  [ "$shard" -ne 0 ] || exit 0
  [ -s "$out/shard" ] || { echo "kani: shard $shard is empty" >&2; exit 1; }
elif [ -n "$list" ]; then
  for c in $selected; do harnesses "$c" "$@"; done
  exit 0
elif [ -z "$harness" ]; then
  # A run of every harness: cargo kani passes a crate with none, so count
  # them first. KANI_HARNESS needs no count: Kani fails a --harness that
  # matches nothing.
  : > "$out/all"
  for c in $selected; do harnesses "$c" "$@" >> "$out/all"; done
  found=$(wc -l < "$out/all" | tr -d ' ')
  if [ -n "$none" ]; then
    if [ "$found" -ne 0 ]; then
      echo "kani: KANI_NONE=yes declares no harnesses, but the selected crates have $found; delete KANI_NONE from the Makefile so they run" >&2
      exit 1
    fi
    echo "kani: no harnesses, as KANI_NONE=yes declares"
    exit 0
  fi
  if [ "$found" -eq 0 ]; then
    echo "kani: the selected crates ($selected ) have no harnesses, so nothing would run; a port with none yet sets KANI_NONE = yes in its Makefile" >&2
    exit 1
  fi
fi

status=0
ran=0
# run CRATE HARNESS_FLAGS [ARGS]: one cargo kani call.
run() {
  crate=$1
  picks=$2
  shift 2
  echo "kani: $crate$picks"
  ran=$((ran + 1))
  # The flags are words with no spaces; split them into arguments.
  # shellcheck disable=SC2046,SC2086
  set -- $(pflag "$crate") ${KANI_FLAGS:-} ${jobs:+-j "$jobs"} ${format:+--output-format "$format"} $picks "$@"
  # The CBMC arguments go last: --cbmc-args takes every word after it.
  # shellcheck disable=SC2086
  [ -z "${KANI_CBMC_ARGS:-}" ] || set -- "$@" --cbmc-args $KANI_CBMC_ARGS
  if [ -z "$pattern" ]; then
    cargo kani "$@" || status=1
    return 0
  fi
  log=$out/$(fname "$crate").log
  rc=0
  cargo kani "$@" > "$log" 2>&1 || rc=$?
  grep -E '^(Checking harness|VERIFICATION:-|Verification Time|Complete -)' "$log" || true
  if grep -n -E -e "$pattern" "$log"; then
    echo "kani: $log has KANI_FAIL_PATTERN ($pattern), above" >&2
    rc=1
  fi
  if [ "$rc" -ne 0 ]; then
    tail -n 200 "$log"
    status=1
  fi
}

for c in $selected; do
  if [ -n "$shards" ]; then
    picks=$(awk -v c="$c" '$1 == c && !seen[$2]++ { printf " --harness %s", $2 }' "$out/shard")
    [ -n "$picks" ] || continue
    run "$c" " --exact$picks" "$@"
  elif [ -n "$harness" ]; then
    run "$c" " --harness $harness" "$@"
  else
    run "$c" "" "$@"
  fi
done
[ "$ran" -gt 0 ] || { echo "kani: nothing ran" >&2; exit 1; }
exit "$status"
