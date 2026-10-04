#!/bin/sh
# verus/refinedrust/run.sh: re-check every seam discharged with RefinedRust
# (docs/testing.md, "RefinedRust").
#
#   sh verus/refinedrust/run.sh
#
# 1. Checks that every record verus/seams.overrides marks discharged with
#    tool RefinedRust exists, is the record the ledger entry of its seam
#    names, still quotes that entry's contract and the function's `rr::`
#    attributes, and names the function's proof, which proofs/assumptions.v
#    prints and proofs/statements.v states. A changed contract or
#    specification fails here until the record is revisited.
# 2. Builds the RefinedRust frontend with frontend.patch (once per patch,
#    cached), and for each quantum translates the whole crate with it:
#    `cargo refinedrust` under `--cfg refinedrust`, which registers the
#    `rr` tool attributes (src/lib.rs) and changes nothing else. Only
#    functions with `rr::` specifications are translated, each needs its
#    proof under proofs/.
# 3. Checks the generated Rocq code against proofs/ (dune), with the
#    lemmas the proofs use (theories/) and the stdlib proofs of the
#    callees (callees/), and fails unless `Print Assumptions` of every
#    proof names only the two axioms RefinedRust itself rests on.
#
# Tools, from the environment:
#   RR_ENV     sourced first when it exists (default /opt/rr/rr-env.sh):
#              the opam switch with Rocq, Iris and RefinedRust's theories,
#              dune, rustup
#   RR_ROOT    the RefinedRust checkout (default
#              /opt/rr/src/refinedrust-dev): rr_frontend/, stdlib/
#   RR_CACHE   frontend build, generated code, cargo targets and dune
#              workspaces (default target/refinedrust)
#   CARGO_HOME the registry holding challoc's dependencies (taken before
#              RR_ENV is sourced; default ~/.cargo)
# About four minutes with a warm cache; the first run also downloads the
# frontend's nightly with rustc-dev, builds the frontend and, per
# quantum, the stdlib's Rocq libraries (about fifteen minutes).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
cd "$root"

records=$(awk -F'\t' '$3 == "RefinedRust" && $4 ~ /^discharged \(verus\/seams\/.*\.md\)$/ { r = $4; sub(/^discharged \(/, "", r); sub(/\)$/, "", r); print r }' verus/seams.overrides)
for r in $records; do
	[ -f "$r" ] || { echo "refinedrust: $r is named in verus/seams.overrides but missing" >&2; exit 1; }
done
# As verus/kani/run.sh: the record's header (`Seam S<n> of ...`) names the
# ledger entry whose status names the record, and the record's first
# ```rust block is that entry's.
stale=$(awk '
    FNR == 1 { inblock = 0; done = 0; s = "" }
    FILENAME == "verus/SEAMS.md" && /^### S[0-9]+\. / { s = $2; sub(/\.$/, "", s); done = 0; next }
    FILENAME == "verus/SEAMS.md" && /Status: discharged \(verus\/seams\/.*\.md\)\.$/ {
        r = $0; sub(/.*Status: discharged \(/, "", r); sub(/\)\.$/, "", r); owner[r] = s; next
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
	echo "refinedrust: these records name another seam than the ledger entry naming them, or quote a contract it no longer has:" >&2
	echo "$stale" >&2
	exit 1
fi
# Each record names its proof (`RefinedRust proof: \`<lemma>_proof\``),
# which proofs/assumptions.v must print and proofs/statements.v must give
# its template's statement; and quotes the function's `rr::` attributes
# (lines `#[cfg_attr(refinedrust, rr::...)]`), which must be exactly the
# ones above the function in src/: a changed specification fails here
# until the record follows.
lemmas=
for r in $records; do
	l=$(sed -n 's/^RefinedRust proof: `\([A-Za-z0-9_]*_proof\)`.*/\1/p' "$r")
	[ -n "$l" ] || { echo "refinedrust: $r names no RefinedRust proof" >&2; exit 1; }
	grep -q "^Print Assumptions $l\.$" "$here/proofs/assumptions.v" ||
		{ echo "refinedrust: proofs/assumptions.v does not print $l ($r)" >&2; exit 1; }
	base=${l%_proof}
	grep -q "^  Definition ${base}_statement : ∀ π, ${base}_lemma π :=$" "$here/proofs/statements.v" ||
		{ echo "refinedrust: proofs/statements.v does not state $l ($r)" >&2; exit 1; }
	fn=$(sed -n '1s/^# `\(.*\)`$/\1/p' "$r")
	fn=${fn##*::}
	quoted=$(grep '^#\[cfg_attr(refinedrust, rr::' "$r" || true)
	srcs=$(grep -rln "fn $fn[(<]" src)
	[ "$(echo "$srcs" | wc -w)" -eq 1 ] || { echo "refinedrust: $r: no single file defines fn $fn" >&2; exit 1; }
	# The proof is the record's function's: the frontend names the lemma
	# after the function's module path, its impl type if any, and its
	# name; the record's file is named the same way.
	want=$(basename "$r" .md | tr - _)
	module=$(echo "$srcs" | sed 's|^src/||; s|/mod\.rs$||; s|\.rs$||; s|/|_|g')
	case $base in
	"$want") ;;
	*) echo "refinedrust: $r names $l, not ${want}_proof" >&2; exit 1 ;;
	esac
	case $base in
	"${module}_$fn" | "${module}"_*_"$fn") ;;
	*) echo "refinedrust: $r names $l, not a proof of fn $fn in $srcs" >&2; exit 1 ;;
	esac
	# The attribute and doc lines directly above `fn <name>`.
	found=$(awk -v fn="$fn" '
	    { t = $0; sub(/^[ \t]+/, "", t) }
	    t ~ /^(#\[|\/\/\/)/ { above[n++] = t; next }
	    index(t, "fn " fn "(") || index(t, "fn " fn "<") {
	        for (i = 0; i < n; i++) if (above[i] ~ /^#\[cfg_attr\(refinedrust, rr::/) print above[i]
	        exit
	    }
	    { n = 0 }
	' "$srcs")
	[ -n "$quoted" ] && [ "$quoted" = "$found" ] || {
		echo "refinedrust: $r does not quote the rr:: attributes of fn $fn in $srcs" >&2
		exit 1
	}
	lemmas="$lemmas $l"
done
echo "refinedrust: $(echo "$records" | wc -w) records match the ledger and the specifications"

cargo_home=${CARGO_HOME:-$HOME/.cargo}
env=${RR_ENV:-/opt/rr/rr-env.sh}
# shellcheck disable=SC1090
[ -f "$env" ] && . "$env"
rr=${RR_ROOT:-/opt/rr/src/refinedrust-dev}
cache=${RR_CACHE:-$root/target/refinedrust}
mkdir -p "$cache"
cache=$(cd "$cache" && pwd)
for t in "$rr/rr_frontend/rust-toolchain.toml" "$rr/stdlib/ptr" ; do
	[ -e "$t" ] || { echo "refinedrust: missing $t (set RR_ROOT)" >&2; exit 2; }
done
nightly=$(sed -n 's/^channel = "\(.*\)"/\1/p' "$rr/rr_frontend/rust-toolchain.toml")

# 2. The patched frontend, rebuilt when the patch or the checkout changes.
fe=$cache/frontend
stamp="$(cksum < "$here/frontend.patch") $(cd "$rr" && { git rev-parse HEAD; git diff HEAD -- rr_frontend; } 2>/dev/null | cksum)"
if [ ! -x "$fe/rr_frontend/target/release/cargo-refinedrust" ] || [ "$(cat "$fe/stamp" 2>/dev/null)" != "$stamp" ]; then
	echo "refinedrust: building the frontend ($nightly)"
	rm -rf "$fe"
	mkdir -p "$fe"
	(cd "$rr" && tar cf - --exclude=target rr_frontend) | (cd "$fe" && tar xf -)
	(cd "$fe" && GIT_CEILING_DIRECTORIES=$cache git apply "$here/frontend.patch")
	RUSTUP_HOME=$fe/rustup rustup toolchain install "$nightly" --profile minimal \
		-c rustc-dev -c llvm-tools-preview
	(cd "$fe/rr_frontend" && RUSTUP_HOME=$fe/rustup cargo +"$nightly" build --release)
	echo "$stamp" > "$fe/stamp"
fi

if grep -n 'Admitted\|admit\|Abort' "$here"/proofs/*.v "$here"/theories/*.v "$here"/callees/*.v; then
	echo "refinedrust: a proof is not closed" >&2
	exit 1
fi
nseams=$(echo "$lemmas" | wc -w)
want=$((nseams + $(grep -c '^Print Assumptions' "$here/callees/callees.v")))
status=0
for q in quantum16 quantum8; do
	echo "== $q"
	out=$cache/$q/output
	# A fresh translation: cargo would skip an unchanged crate.
	rm -rf "$out" "$cache/$q/target"/debug/.fingerprint/challoc-*
	PATH=$fe/rr_frontend/target/release:$PATH RUSTUP_HOME=$fe/rustup CARGO_HOME=$cargo_home \
		RUSTC_WRAPPER=$here/rustc-wrapper.sh RR_CONFIG=$here/RefinedRust.toml \
		RR_OUTPUT_DIR=$out RR_LIB_LOAD_PATHS=$rr/stdlib RUSTFLAGS="--cfg refinedrust" \
		CARGO_TARGET_DIR=$cache/$q/target cargo +"$nightly" refinedrust -- \
		--no-default-features --features "$q"
	rm -rf "$out/build_script_build"
	# The frontend writes a default proof for every annotated function;
	# the committed ones replace them, and each must exist.
	for p in "$out"/challoc/proofs/proof_*.v; do
		[ -e "$p" ] || { echo "refinedrust: the frontend wrote no proof templates to $out/challoc/proofs" >&2; exit 1; }
		[ -f "$here/proofs/${p##*/}" ] || {
			echo "refinedrust: no proofs/${p##*/} for an annotated function (the frontend's default is $p)" >&2
			exit 1
		}
	done
	cp "$here"/proofs/*.v "$out/challoc/proofs/"

	# One dune workspace for the stdlib, the lemmas and the crate's proofs.
	ws=$cache/$q/ws
	mkdir -p "$ws"
	echo '(lang dune 3.21)' > "$ws/dune-workspace"
	ln -sfn "$rr/stdlib" "$ws/stdlib"
	ln -sfn "$here/theories" "$ws/theories"
	ln -sfn "$out" "$ws/challoc"
	rm -rf "$ws/callees"
	cp -R "$here/callees" "$ws/callees"
	# A changed line makes dune run the `Print Assumptions` files again
	# (it would keep their old results), so their output is in the log.
	nonce="(* run.sh $$ $(date +%s) *)"
	echo "$nonce" >> "$out/challoc/proofs/assumptions.v"
	echo "$nonce" >> "$ws/callees/callees.v"
	log=$cache/$q/dune.log
	if ! (cd "$ws" && dune build --cache=disabled --display short @theories/default @callees/default @challoc/default) > "$log" 2>&1; then
		cat "$log"
		echo "refinedrust: $q: the proofs do not check" >&2
		status=1
		continue
	fi
	# Each `Print Assumptions` prints "Axioms:" and one `name : type` per
	# axiom (continuations indented), or "Closed under the global context".
	# Inside an "Axioms:" block every unindented line must be one of the
	# two allowed axioms, until a line dune or the proofs print ends it.
	got=$(awk '
	    /^Closed under the global context$/ { n++; ax = 0; next }
	    /^Axioms:$/ { n++; ax = 1; next }
	    ax && /^(Did [0-9]+ steps\.|Warning:|File |Error)/ { ax = 0; next }
	    ax && /^[^ ]/ {
	        if ($0 ~ /^ProofIrrelevance\.proof_irrelevance :/ || $0 ~ /^FunctionalExtensionality\.functional_extensionality_dep :/) next
	        print $0
	    }
	    END { print "count", n + 0 }
	' "$log")
	if [ "$got" != "count $want" ]; then
		cat "$log"
		echo "refinedrust: $q: expected $want proofs resting only on proof_irrelevance and functional_extensionality_dep; got:" >&2
		echo "$got" >&2
		status=1
		continue
	fi
	echo "refinedrust: $q: $want proofs check ($nseams seams, the rest their stdlib callees)"
done
exit $status
