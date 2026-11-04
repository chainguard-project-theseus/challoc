#!/bin/sh
# ci/kani-regular.sh SHARD SHARDS: one shard of every Kani harness, in
# regular format with CBMC's verbose output, failing on any
# `warning: ignoring` line (the kani-ignoring check, postsubmit).
#
# Kani 0.68.0 with CBMC 6.11.0 leaves `<`, `<=`, `>` and `>=` on two
# non-constant bools unconstrained: CBMC prints `warning: ignoring >=`,
# substitutes a fresh value, and the harness can pass vacuously. The kani
# check runs `--output-format terse -j 4`, and terse output (the only
# format `-j` takes) hides CBMC's warnings, so this check runs the same
# harnesses one at a time in regular format. scripts/kani-boolcmp.sh is
# the presubmit lint for the same bug; this is the backstop for what a
# syntactic scan cannot see.
#
# `cargo kani list` names the harnesses, and every shard splits them the
# same way. A harness marked `alone` gets a shard of its own, from shard 1
# up; the rest go longest first, each to the shard with the least time so
# far, by the seconds below: one harness per `cargo kani` call, in regular
# format with `--verbosity 9`, at quantum 16, on an 8 vCPU guest, less the
# 5 s rebuild of each call (2026-10-31). The `select_ptrs` rows are
# CBMC's verification times with all eight in one call, run beside the
# same call at quantum 8 (2026-11-04).
# A harness not listed counts as
# $default seconds, so a new one lands in some shard without an edit
# here; add it to the table when it takes more than half a minute.
#
#   QUANTUM=16 sh ci/kani-regular.sh 1 4      shard 1 of 4
#   QUANTUM=16 sh ci/kani-regular.sh 0 4      print the split, run nothing
#
# Exit 0: every harness of the shard verified and CBMC ignored nothing;
# 1: a harness failed or CBMC printed `warning: ignoring`; 2: usage.
set -eu
shard=${1:-}; shards=${2:-}
case "$shard.$shards" in
  *[!0-9.]* | .* | *. | *.0) echo "usage: QUANTUM=16|8 ci/kani-regular.sh SHARD SHARDS" >&2; exit 2 ;;
esac
default=2
weights='
  kani_proofs::misc::formatter_write_fmt 1261 alone
  kani_proofs::slice::select_ptrs_by_key_12 416
  kani_proofs::slice::select_ptrs_12 380
  sys::cgroup::kani_fmt::display_cgroup 355
  kani_proofs::slice::select_ptrs_short 349
  config::kani_config::display_bytes 342
  kani_proofs::slice::select_ptrs_by_key_short 322
  config::kani_config::set_listed_contract 304
  api::print::kani_print::word_returns 296
  kani_proofs::slice::slice_is_sorted 249
  kani_proofs::slice::sort_ptrs_by_key_12 216
  kani_proofs::slice::select_ptrs_by_key_11 195
  kani_proofs::slice::sort_ptrs_12 171
  api::print::kani_print::hist_line_returns 171
  kani_proofs::slice::select_ptrs_11 140
  kani_proofs::slice::sort_ptrs_by_key_short 132
  kani_proofs::slice::sort_ptrs_short 130
  config::kani_config::display_issue_rounded 125
  kani_proofs::slice::select_ptrs_10 115
  kani_proofs::ptr::ptr_copy_nonoverlapping 104
  kani_proofs::slice::sort_ptrs_11 98
  kani_proofs::ptr::ptr_write_unaligned 71
  kani_proofs::slice::sort_ptrs_by_key_11 70
  kani_proofs::slice::select_ptrs_by_key_10 67
  layout::kani_classes::class_table_entries 58
  kani_proofs::slice::sort_ptrs_10 57
  config::kani_config::display_issue_bad_value 57
  config::kani_config::display_issue_out_of_range 56
  kani_proofs::stats::fig_mul_exact 54
  kani_proofs::slice::sort_ptrs_by_key_10 33
  heap::cache::kani_cache::home_ptr_at_is_the_home_header 28
  config::kani_config::display_issue_unknown_key 26
  config::kani_config::get_contract 25
'

set -- -Z unstable-options -Z stubbing -Z mem-predicates --ignore-global-asm \
  --no-default-features --features "quantum${QUANTUM:?}"
out=target/kani-regular
mkdir -p "$out"
cargo kani "$@" list -Z stubbing -Z mem-predicates --format json >/dev/null
# Every harness name in kani-list.json, then this shard's share.
# A harness is a bare string in an array (keys end in `: [` or `: {`, file
# paths hold `/`); the count must match the file's totals.
sed -n 's/^ *"\([A-Za-z0-9_:]*\)",\{0,1\}$/\1/p' kani-list.json | sort -u > "$out/harnesses.txt"
total=$(sed -n '/"totals"/,/}/s/^ *"\(standard\|contract\)-harnesses": *\([0-9]*\).*/\2/p' kani-list.json | awk '{ s += $1 } END { print s + 0 }')
if [ "$(wc -l < "$out/harnesses.txt")" -ne "$total" ]; then
  echo "kani-list.json lists $total harnesses, read $(wc -l < "$out/harnesses.txt")" >&2; exit 1
fi
echo "$weights" | awk -v shard="$shard" -v shards="$shards" -v def="$default" '
  FNR == NR { if (NF >= 2) { w[$1] = $2; if ($3 == "alone" && shards > 1) alone[$1] = 1 }; next }
  { n++; name[n] = $1; t[n] = ($1 in w) ? w[$1] : def }
  END {
    # A harness marked alone takes a shard of its own, from shard 1 up.
    first = 1
    for (i = 1; i <= n; i++) if (name[i] in alone) { at[i] = first; load[first] = t[i]; first++ }
    # Longest first (ties by name), each to the least-loaded shard.
    for (i = 1; i <= n; i++) ord[i] = i
    for (i = 2; i <= n; i++)
      for (j = i; j > 1 && (t[ord[j]] > t[ord[j - 1]] || t[ord[j]] == t[ord[j - 1]] && name[ord[j]] < name[ord[j - 1]]); j--) {
        k = ord[j]; ord[j] = ord[j - 1]; ord[j - 1] = k
      }
    if (first > shards) { print "more harnesses marked alone than shards" > "/dev/stderr"; exit 2 }
    for (s = first; s <= shards; s++) load[s] = 0
    for (i = 1; i <= n; i++) {
      if (ord[i] in at) continue
      best = first
      for (s = first + 1; s <= shards; s++) if (load[s] < load[best]) best = s
      load[best] += t[ord[i]]; at[ord[i]] = best
    }
    for (s = 1; s <= shards; s++) {
      c = 0; for (i = 1; i <= n; i++) if (at[i] == s) c++
      printf "shard %d: %d harnesses, ~%d s\n", s, c, load[s] > "/dev/stderr"
    }
    for (i = 1; i <= n; i++) if (at[i] == shard) print name[i]
  }' - "$out/harnesses.txt" > "$out/shard.txt"
echo "shard $shard of $shards at quantum $QUANTUM: $(wc -l < "$out/shard.txt") of $(wc -l < "$out/harnesses.txt") harnesses"
[ "$shard" -ne 0 ] || exit 0
[ -s "$out/shard.txt" ] || { echo "shard $shard is empty" >&2; exit 1; }
while read -r h; do set -- "$@" --harness "$h"; done < "$out/shard.txt"
rc=0
cargo kani "$@" --exact --output-format regular --cbmc-args --verbosity 9 > "$out/regular.log" 2>&1 || rc=$?
grep -E '^(Checking harness|VERIFICATION:-|Verification Time|Complete -)' "$out/regular.log" || true
if grep -n 'warning: ignoring' "$out/regular.log"; then
  echo "CBMC ignored a comparison (above): an ordering comparison on bools; see scripts/kani-boolcmp.sh" >&2
  rc=1
fi
if [ "$rc" -ne 0 ]; then tail -n 200 "$out/regular.log"; fi
exit "$rc"
