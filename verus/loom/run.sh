#!/bin/sh
# verus/loom/run.sh: run every loom model (src/loom_tests/,
# verus/WEAKMEM.md) that a seam record relies on, and the controls.
#
#   sh verus/loom/run.sh [test name filter, default loom_]
#
# First checks the records: verus/SEAMS.md is current (`seams.sh
# --check`); every record verus/seams.overrides marks `discharged` or
# `evidence` with tool loom/GenMC exists, is the record the ledger entry
# of its seam names, still quotes that entry's contract (its first
# ```rust block), and names in its `Models:` line only loom tests that
# exist and are not ignored; and the source still has, in order, the
# lines the models copy (verus/loom/pins.txt). A changed contract, or a
# changed copied line, fails here until the record, the copy and the
# list are revisited.
#
# Then runs the tests under loom 0.7.2 with verus/loom/loom-0.7.2-dpor.patch
# (stock loom's DPOR misses executions these models need; the patch says
# which): it copies the crate from the cargo registry, applies the patch
# and points cargo at the copy with `--config patch...`, then restores
# Cargo.lock, which the patch rewrites. Every model is exhaustive except
# the three-thread lock model and its control (5 preemptions); the LOOM_*
# variables that bound or cut short a run are unset. `src/loom_tests/dpor.rs`
# fails unless the patch is in. CARGO_TARGET_DIR defaults to target/loom
# (the cfg(loom) build shares nothing with the others).
set -eu
cd "$(dirname "$0")/../.."
root=$(pwd)

sh verus/seams.sh --check
records=$(awk -F'\t' '$3 == "loom/GenMC" && $4 ~ /^(discharged|evidence) \(verus\/seams\/.*\.md\)$/ { r = $4; sub(/^[a-z]+ \(/, "", r); sub(/\)$/, "", r); print r }' verus/seams.overrides)
for r in $records; do
	[ -f "$r" ] || { echo "loom: $r is named in verus/seams.overrides but missing" >&2; exit 1; }
done
# As verus/kani/run.sh: a record's header names its seam (`Seam S<n> of
# ...`), which must be the ledger entry whose status names the record,
# and the record's first ```rust block must be that entry's.
stale=$(awk '
    FNR == 1 { inblock = 0; done = 0; s = "" }
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
' verus/SEAMS.md $records)
if [ -n "$stale" ]; then
	echo "loom: these records name another seam than the ledger entry naming them, or quote a contract it no longer has:" >&2
	echo "$stale" >&2
	exit 1
fi
for r in $records; do
	models=$(sed -n 's/^Models: //p' "$r" | grep -o '`loom_[a-z0-9_]*`' | tr -d '`')
	[ -n "$models" ] || { echo "loom: $r has no Models: line" >&2; exit 1; }
	for m in $models; do
		grep -q "fn $m()" src/loom_tests/*.rs || { echo "loom: $r names $m, which src/loom_tests/ does not define" >&2; exit 1; }
		if grep -h -B3 "fn $m()" src/loom_tests/*.rs | grep -q '#\[ignore'; then
			echo "loom: $r names $m, which is ignored" >&2
			exit 1
		fi
	done
done
for f in $(awk -F'\t' '!/^#/ && NF == 2 { print $1 }' verus/loom/pins.txt | sort -u); do
	missing=$(awk -F'\t' -v f="$f" '
        FILENAME == "verus/loom/pins.txt" { if ($0 !~ /^#/ && NF == 2 && $1 == f) p[++n] = $2; next }
        { t = $0; sub(/^[ \t]+/, "", t); sub(/[ \t]+$/, "", t); if (k < n && t == p[k + 1]) k++ }
        END { if (k < n) print p[k + 1] }
    ' verus/loom/pins.txt "$f")
	[ -z "$missing" ] || { echo "loom: $f no longer has, in the order of verus/loom/pins.txt: $missing" >&2; exit 1; }
done
# What the WordPerm, CountPerm and state_claim records rely on, checked by
# name (a write through a pointer cast these patterns do not name is
# caught only by review):
# - every atomic write in non-test bitmap.rs is a vspec/word.rs wrapper
#   call (the scan stops at the `#[cfg(test)] mod tests` line, which must
#   be the file's last item);
# - class/table.rs, which casts bitmap word addresses to `&AtomicU64` and
#   passes them on, writes atomics only on the three lines listed (the
#   owner word, a test's corrupting write of it, the watermark);
# - arena.rs names the bound counts only in a vspec/count.rs wrapper
#   call, the spec `count_id`, and `Arenas::init`'s two lines writing 0;
# - `STATE` has two writes, `state_claim`'s compare-exchange and
#   `state_publish`'s `READY` store.
bad=$(awk '/^#\[cfg\(test\)\]$/ { t = 1; next } t && /^mod tests/ { exit } { t = 0 }
    /\.(fetch_[a-z]+|store|swap|compare_exchange|compare_exchange_weak)\(/ && !/crate::vspec::word::/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' src/bitmap.rs
    awk '{ x = $0; sub(/^[ \t]+/, "", x) }
    /\.(fetch_[a-z]+|store|swap|compare_exchange|compare_exchange_weak)\(/ && !/^[ \t]*\/\// && x != "w.store(arena, Ordering::Relaxed)" \
        && x != "(*((self.owners_base + group * 2) as *const AtomicU16)).store(arena, Ordering::Relaxed)" \
        && x != "self.committed_slabs.store(end, Ordering::Release);" { print FILENAME ":" FNR ": " $0 }' src/class/table.rs
    awk '{ x = $0; sub(/^[ \t]+/, "", x) }
    (/\.bound([^A-Za-z0-9_]|$)/ || /\(\*bound\)/) && !/crate::vspec::count::/ && !/count_id\(/ && !/^[ \t]*\/\// \
        && x != "let bound = &raw mut (*this).bound;" \
        && x != "ptr::write(&raw mut (*bound)[i], AtomicU32::new(0));" { print FILENAME ":" FNR ": " $0 }' src/arena.rs)
nstate=$(find src -name '*.rs' ! -path 'src/loom_tests/*' -exec grep -hE '(^|[^A-Za-z0-9_])STATE\.(store|swap|compare_exchange|fetch_)' {} + | wc -l | tr -d ' ')
[ "$nstate" = 2 ] || bad="$bad
src/api/mod.rs: $nstate writes of STATE, not 2 (record api-state_claim.md)"
if [ -n "$bad" ]; then
	echo "loom: an atomic access outside what the records allow (vspec-word-WordPerm.md, vspec-count-CountPerm.md, api-state_claim.md):" >&2
	echo "$bad" >&2
	exit 1
fi
echo "loom: $(echo "$records" | wc -w) records match the ledger; the copied lines are in place"

# loom 0.7.2 with the DPOR patch, in a fresh copy.
unset LOOM_MAX_PREEMPTIONS LOOM_MAX_BRANCHES LOOM_MAX_PERMUTATIONS LOOM_MAX_DURATION \
	LOOM_CHECKPOINT_FILE LOOM_CHECKPOINT_INTERVAL
export RUSTFLAGS="--cfg loom"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target/loom}"
cargo fetch -q
src=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/loom-0.7.2 2>/dev/null | head -n 1)
[ -n "$src" ] || { echo "loom: loom-0.7.2 is not in the cargo registry" >&2; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/challoc-loom.XXXXXX")
lock=$work/Cargo.lock
cp Cargo.lock "$lock"
trap 'cp "$lock" "$root/Cargo.lock"; rm -rf "$work"' EXIT INT TERM
cp -R "$src" "$work/loom"
(cd "$work/loom" && git apply -p1 "$root/verus/loom/loom-0.7.2-dpor.patch")

cargo --config "patch.crates-io.loom.path=\"$work/loom\"" test --release --lib "${1:-loom_}"
