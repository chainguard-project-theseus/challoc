#!/bin/sh
# verus/kani/run.sh: run every Kani harness (docs/testing.md, "Kani").
#
#   sh verus/kani/run.sh [cargo kani options, e.g. --harness <path>]
#
# First checks that every record verus/seams.overrides marks discharged
# or evidence with tool Kani exists, is the record the ledger entry of
# its seam names, and still quotes that entry's contract (records of
# other tools, such as Isla's, quote other text first). A changed
# contract fails here until the record and its harness are revisited.
# Then runs the harnesses under both quanta, since the layout,
# class-table and geometry harnesses depend on `NCLASSES`, and exits
# non-zero if the check or either run fails.
# Needs Kani 0.68.0 (`sh verus/kani/setup.sh`). The flags:
# `--ignore-global-asm` because `api/libc.rs` and `sys/tls.rs` hold
# `global_asm!` (symbol aliases and the TLS slot) that no harness reaches;
# `-Z stubbing` for `config::kani_config::get_contract` (Kani has no C
# string literals). KANI_JOBS sets the parallel harnesses (default 1:
# the formatting harnesses need up to 20 GB each, so raise it only on
# a machine with the memory for that many at once).
set -eu
cd "$(dirname "$0")/../.."

records=$(awk -F'\t' '$3 == "Kani" && $4 ~ /^(discharged|evidence) \(verus\/seams\/.*\.md\)$/ { r = $4; sub(/^[a-z]+ \(/, "", r); sub(/\)$/, "", r); print r }' verus/seams.overrides)
for r in $records; do
	[ -f "$r" ] || { echo "kani: $r is named in verus/seams.overrides but missing" >&2; exit 1; }
done
# A record's header names its seam (`Seam S<n> of ...`), which must be
# the ledger entry whose status names the record, and the record's first
# ```rust block must be that entry's (a record with none fails).
stale=$(awk '
    FNR == 1 { if (prev != "" && prev != "verus/SEAMS.md" && !done) print prev; prev = FILENAME; inblock = 0; done = 0; s = "" }
    FILENAME == "verus/SEAMS.md" && /^### S[0-9]+\. / { s = $2; sub(/\.$/, "", s); done = 0; next }
    FILENAME == "verus/SEAMS.md" && /Status: (discharged|evidence) \(verus\/seams\/.*\.md\)\.$/ {
        r = $0; sub(/.*Status: [a-z]+ \(/, "", r); sub(/\)\.$/, "", r); owner[r] = s; next
    }
    FILENAME != "verus/SEAMS.md" && s == "" && /^Seam S[0-9]+ / { s = $2; next }
    /^```rust$/ && !done { inblock = 1; block = ""; next }
    inblock && /^```$/ {
        inblock = 0; done = 1
        if (FILENAME == "verus/SEAMS.md") ledger[s] = block
        else if (s == "" || owner[FILENAME] != s || ledger[s] != block) print FILENAME
        next
    }
    inblock { block = block $0 "\n" }
    END { if (prev != "verus/SEAMS.md" && !done) print prev }
' verus/SEAMS.md $records)
if [ -n "$stale" ]; then
	echo "kani: these records name another seam than the ledger entry naming them, or quote a contract it no longer has (or none):" >&2
	echo "$stale" >&2
	exit 1
fi
echo "kani: $(echo "$records" | wc -w) records match the ledger"

jobs=${KANI_JOBS:-1}
status=0
for q in quantum16 quantum8; do
	echo "== $q"
	cargo kani -Z unstable-options -Z stubbing --ignore-global-asm \
		--output-format terse -j "$jobs" \
		--no-default-features --features "$q" "$@" || status=1
done
exit $status
