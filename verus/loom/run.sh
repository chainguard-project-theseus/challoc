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
# What the WordPerm record, the count and watermark wrappers' records and the
# state_claim and DescPerm accessor records rely on, checked by name (a
# write through a pointer cast these patterns do not name is caught only
# by review):
# - every atomic write in non-test bitmap.rs is a vspec/word.rs wrapper
#   call (the scan stops at the `#[cfg(test)] mod tests` line, which must
#   be the file's last item);
# - class/table.rs, which casts bitmap word addresses to `&AtomicU64` and
#   passes them on, writes atomics only on the two lines listed (the
#   owner word, `store_owner`'s, and a test's corrupting write of it) and
#   through vspec/atom.rs's `store_usize` wrapper (the watermark) and,
#   since upstream #87, `store_usize_at` (its copy in the class line): the
#   owner word's value, which `load_u16_at` reads through its permission,
#   is `store_owner`'s; and only class/table.rs names the owner words'
#   invariants (`owner_invs`), which hold those permissions, so no other
#   file can reach a word through one;
# - arena.rs names the bound counts only in a vspec/count.rs wrapper
#   call, the spec `count_id`, and ghost `assert`/`forall` lines
#   (`Arenas::init` builds each count with `vspec::count::new(0)`);
# - arena.rs names the arena count (`self.count`), and policy.rs the
#   position (`.scaled16`), only in a vspec/count.rs wrapper call and the
#   spec `count_id` (each is made with `vspec::count::new`; the ghost
#   `self.count.ptr()` of `Arenas::init`'s field permissions aside);
# - arena.rs writes a table entry (`self.table[..]`) only through
#   vspec/entry.rs's `store` (no `.table[..].store(` or other write on a
#   line, and `elem_at(table, ..)` only in an entry `store` call or in
#   `put_entry`'s write of the atomic `vspec::entry::new` made;
#   `Arenas::init` makes each entry with `vspec::entry::new`): the
#   entry wrappers' records;
# - nothing writes the watermark (`committed_slabs`) but through
#   vspec/atom.rs's `store_usize`, and only class/table.rs (and the loom
#   models) calls the vspec/atom.rs wrappers (`new_usize`,
#   `load_usize_in`, `load_usize_seen`, `load_u16_at`, `store_usize` and,
#   since upstream #87, the class line copy's `store_usize_at`;
#   `heap/shared.rs` reads the copy through
#   `class::load_mirror` and makes its pointer with `usize_ptr`), and
#   non-test heap/shared.rs writes no class line's copy (`.committed`)
#   directly: `store_usize_at`, from class/table.rs, is its one writer;
#   and only heap/shared.rs (and the loom models) makes such a pointer
#   (`usize_ptr`, into the never-moving `Shared`'s class lines);
# - vspec/count.rs builds a `CountPerm` only in `new`, vspec/atom.rs
#   a `UsizePerm` only in `new_usize`, and vspec/byte.rs a `BytePerm`
#   only in `new`, with one literal (the fields are
#   private to those modules, so no other module can): one permission per
#   atomic, minted with it. Each file names its type only in the struct
#   and impl lines and that one literal (no `Self { .. }`, alias, rename,
#   derive, trait impl or child module); and no `const` item initialiser
#   in src names `count::new`, `new_usize`, `Policy::new`, `byte::new`,
#   `Deferred::now` or `Deferred::later` (each use of
#   such an item would copy one atomic and its permission);
# - `STATE` has two writes, `state_claim`'s compare-exchange and
#   `state_publish`'s `READY` store; `LINE_STATE` has two writes outside
#   api/mod.rs's tests, `line_compare_exchange`'s and `line_store`'s, and
#   no other file names it (the loom models aside): the LINE_STATE
#   wrappers' records (verus/seams/api-line_*.md);
# - non-test sys/cgroup.rs writes a deferred record's phase (`.phase`)
#   only through vspec/byte.rs's wrappers, and no other file (the loom
#   models aside) calls them: the vspec/byte.rs wrappers' records;
# - the `free` path's dispatch byte (`Shared::free_flags`, a
#   `vspec::flags::RouteByte`, whose `AtomicU8` field is private to
#   vspec/flags.rs, so rustc confines every atomic operation on it to the
#   wrappers there): outside the loom models, the vspec/flags.rs wrappers
#   are called exactly once each (`new`, `load`, `fetch_or`), each in
#   heap/shared.rs on the line verus/loom/pins.txt pins (verified code,
#   so `fetch_or` meets its `requires`); the one line that writes
#   through the field's offset (`S_FREE_FLAGS`) writes the byte
#   `flags::new` made; and no line names `free_flags` or `RouteByte` with
#   `addr_of`, `&raw`, `as *mut`, `transmute`, `write_bytes`, `zeroed`,
#   `MaybeUninit`, `mem::swap`/`replace`/`take` or `ptr::write`/
#   `replace`/`swap`/`read`, assigns or `&mut`-borrows a `free_flags`
#   field (the ghost `Tracked(&mut parts.free_flags)` aside), calls the
#   wrappers by a shorter path or imports from vspec/flags.rs anything
#   but `RouteByte` (heuristic: a write through a pointer or name these
#   patterns do not see is caught only by review): the flag-byte
#   wrappers' records (verus/seams/vspec-flags-*.md);
# - non-test slab.rs names a descriptor's `state`, `shadow`, `arena` and
#   `dirty_at` (private fields, so no other module can), on any receiver,
#   only in a `Relaxed` load, a `DescPerm` spec body, or the eight write
#   lines of `init`, `set_state`, `set_dirty_at` and the `#[cfg(test)]`
#   `set_arena`, each in its function and each exactly once; no line
#   starts a split write chain, takes a field's address, names
#   `SlabDesc { .. }` or writes through `as *mut SlabDesc`, `write_bytes`,
#   `write_volatile`, `addr_of` or `transmute`: the DescPerm accessors'
#   records (verus/seams/slab-SlabDesc-*.md).
bad=$(awk '/^#\[cfg\(test\)\]$/ { t = 1; next } t && /^mod tests/ { exit } { t = 0 }
    /\.(fetch_[a-z]+|store|swap|compare_exchange|compare_exchange_weak)\(/ && !/crate::vspec::word::/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' src/bitmap.rs
    awk '{ x = $0; sub(/^[ \t]+/, "", x) }
    /\.(fetch_[a-z]+|store|swap|compare_exchange|compare_exchange_weak)\(/ && !/^[ \t]*\/\// && x != "unsafe { (*w).store(arena, Ordering::Relaxed) }" \
        && x != "(*((self.owners_base + group * 2) as *const AtomicU16)).store(arena, Ordering::Relaxed)" { print FILENAME ":" FNR ": " $0 }' src/class/table.rs
    awk '{ x = $0; sub(/^[ \t]+/, "", x) }
    (/\.bound([^A-Za-z0-9_]|$)/ || /\(\*bound\)/) && !/crate::vspec::count::/ && !/count_id\(/ && !/^[ \t]*\/\// \
        && !/assert\(/ && !/forall\|/ { print FILENAME ":" FNR ": " $0 }' src/arena.rs
    awk '/self\.count([^A-Za-z0-9_(]|$)/ && !/self\.count\.ptr\(\)/ && !/crate::vspec::count::/ && !/count_id\(/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' src/arena.rs
    awk '/\.table\[[^]]*\]\.(store|swap|fetch_[a-z]+|compare_exchange|compare_exchange_weak)\(/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' src/arena.rs
    awk '/elem_at\(table,/ && !/crate::vspec::entry::store\(/ && !/ptr_mut_write\(elem_at\(table, i, MAX_ARENAS\), Tracked\(t\), e\);/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' src/arena.rs
    awk '/\.scaled16([^A-Za-z0-9_]|$)/ && !/crate::vspec::count::/ && !/count_id\(/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' src/policy.rs
    awk 'function bad() { print FILENAME ":" FNR ": " $0 }
    BEGIN {
        want["init" SUBSEP "self.state.store(State::Fresh as u8, Ordering::Relaxed);"] = 1
        want["init" SUBSEP "self.shadow.store(State::Fresh as u8 + 1, Ordering::Relaxed);"] = 1
        want["init" SUBSEP "self.arena.store(arena, Ordering::Relaxed);"] = 1
        want["init" SUBSEP "self.dirty_at.store(0, Ordering::Relaxed);"] = 1
        want["set_state" SUBSEP "self.state.store(s as u8, Ordering::Relaxed);"] = 1
        want["set_state" SUBSEP "self.shadow.store(s as u8 + 1, Ordering::Relaxed);"] = 1
        want["set_dirty_at" SUBSEP "self.dirty_at.store(ns, Ordering::Relaxed);"] = 1
        want["test set_arena" SUBSEP "self.arena.store(arena, Ordering::Relaxed);"] = 1
    }
    /^#\[cfg\(test\)\]$/ { t = 1; next }
    t && /^mod tests/ { exit }
    { t = 0 }
    /^[ \t]*\/\// { next }
    { x = $0; sub(/[ \t]*\/\/.*$/, "", x); sub(/^[ \t]+/, "", x); sub(/[ \t]+$/, "", x) }
    match(x, /fn [a-z_]+/) { f = substr(x, RSTART + 3, RLENGTH - 3); if (prev == "#[cfg(test)]") f = "test " f }
    x != "" { prev = x }
    x ~ /^\.(store|swap|fetch_[a-z]+|compare_exchange|compare_exchange_weak|as_ptr|get_mut|into_inner)\(/ || x ~ /addr_of|write_bytes|write_volatile|as \*mut SlabDesc|transmute/ { bad(); next }
    x ~ /SlabDesc *\{/ && x != "pub struct SlabDesc {" && x != "impl SlabDesc {" { bad(); next }
    x ~ /&(mut )?[A-Za-z0-9_.()*]*\.(state|shadow|arena|dirty_at)([^A-Za-z0-9_(.]|$)/ { bad(); next }
    {
        s = x
        while (match(s, /\.(state|shadow|arena|dirty_at)([^A-Za-z0-9_(]|$)/)) {
            fld = substr(s, RSTART + 1, RLENGTH - 1); sub(/[^A-Za-z_]$/, "", fld)
            after = substr(s, RSTART + 1 + length(fld))
            if (index(after, ".load(Ordering::Relaxed)") == 1) {
                # a read: Verus takes its value as any value unless a permission says otherwise
            } else if (after ~ /^\.(store|swap|fetch_[a-z]+|compare_exchange|compare_exchange_weak)\(/) {
                if ((f SUBSEP x) in want) seen[f SUBSEP x]++
                else { bad(); next }
            } else if (!((x == "self.state" && f == "state") || (x == "self.arena" && f == "arena"))) {
                bad(); next
            }
            s = substr(s, RSTART + RLENGTH)
        }
    }
    END {
        for (k in want) if (seen[k] != want[k]) {
            split(k, p, SUBSEP)
            print FILENAME ": " (seen[k] + 0) " writes, not " want[k] ", in " p[1] ": " p[2]
        }
    }' src/slab.rs
    find src -name '*.rs' ! -path 'src/loom_tests/*' -exec awk '/committed_slabs\.(fetch_[a-z]+|store|swap|compare_exchange|compare_exchange_weak)\(/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' {} +
    awk '/^#\[cfg\(test\)\]$/ { t = 1; next } t && /^(impl|mod) / { t = 2 } t == 2 && /^}/ { t = 0; next } t == 2 { next } { t = 0 }
        /\.committed\.(fetch_[a-z]+|store|swap|compare_exchange|compare_exchange_weak)\(/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' src/heap/shared.rs
    find src -name '*.rs' ! -path 'src/loom_tests/*' ! -path src/vspec/atom.rs ! -path src/class/table.rs \
        -exec awk '/(new_usize|load_usize_in|load_usize_seen|load_u16_at|store_usize|store_usize_at)\(/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' {} +
    find src -name '*.rs' ! -path 'src/loom_tests/*' ! -path src/vspec/atom.rs ! -path src/heap/shared.rs \
        -exec awk '/usize_ptr\(/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' {} +
    find src -name '*.rs' ! -path 'src/loom_tests/*' ! -path src/class/table.rs \
        -exec awk '/owner_invs/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' {} +
    for p in 'src/vspec/count.rs CountPerm new' 'src/vspec/atom.rs UsizePerm new_usize' 'src/vspec/byte.rs BytePerm new'; do
        set -- $p
        awk -v ty="$2" -v fn="$3" 'function bad() { print FILENAME ":" FNR ": " $0 }
        /^[ \t]*\/\// { next }
        { x = $0; sub(/[ \t]*\/\/.*$/, "", x) }
        match(x, /fn [a-z_0-9]+/) { f = substr(x, RSTART + 3, RLENGTH - 3) }
        x ~ /(^|[^A-Za-z0-9_])Self([^A-Za-z0-9_:]|$)/ || x ~ /derive|^[ \t]*(pub[^ ]* )?mod / || x ~ ("for " ty "([^A-Za-z0-9_]|$)") { bad(); next }
        x == ("pub tracked struct " ty " {") || x == ("impl " ty " {") { next }
        {
            s = x
            while (match(s, "(^|[^A-Za-z0-9_])" ty)) {
                s = substr(s, RSTART + RLENGTH)
                if (s ~ /^[A-Za-z0-9_]/) continue
                if (s ~ /^[ \t]*(\{|$)/ && f == fn) n++
                else if (s !~ /^[>),|]/ && s !~ /^\]/) { bad(); break }
            }
        }
        END { if (n != 1) print FILENAME ": " (n + 0) " " ty " literals in " fn ", not 1" }' "$1"
    done
    find src -name '*.rs' -exec awk '/^[ \t]*(pub(\([a-z]+\))? )?const [A-Z_0-9]+[ \t]*:/ { c = 1 }
        c && /(count::new|new_usize|Policy::new|byte::new|Deferred::now|Deferred::later)\(/ { print FILENAME ":" FNR ": " $0 }
        c && /;[ \t]*(\/\/.*)?$/ { c = 0 }' {} +
    awk '/^#\[cfg\((test|kani)\)\]$/ { t = 1; next } t && /^(impl|mod) / { t = 2 } t == 2 && /^}/ { t = 0; next } t == 2 { next } { t = 0 }
        /\.phase\.(fetch_[a-z]+|store|swap|compare_exchange|compare_exchange_weak)\(/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' src/sys/cgroup.rs
    find src -name '*.rs' ! -path 'src/loom_tests/*' ! -path src/vspec/byte.rs ! -path src/sys/cgroup.rs \
        -exec awk '/vspec::byte::|byte::(new|load|store|compare_exchange)\(/ && !/^[ \t]*\/\// { print FILENAME ":" FNR ": " $0 }' {} +
    find src -name '*.rs' ! -path 'src/loom_tests/*' ! -path src/vspec/flags.rs -exec awk '/^[ \t]*\/\// { next }
        { x = $0; sub(/^[ \t]+/, "", x) }
        /(free_flags|RouteByte)/ && /(addr_of|&raw |as \*mut|transmute|write_bytes|zeroed|MaybeUninit)/ { print FILENAME ":" FNR ": " $0; next }
        /(free_flags|RouteByte)/ && /(mem::(swap|replace|take)|ptr::(write|replace|swap|read))/ { print FILENAME ":" FNR ": " $0; next }
        /free_flags[ \t]*=([^=]|$)/ { print FILENAME ":" FNR ": " $0; next }
        /&mut [^;]*free_flags/ && !/Tracked\(&mut parts\.free_flags\)/ { print FILENAME ":" FNR ": " $0; next }
        /(^|[^A-Za-z0-9_:])flags::(new|load|fetch_or)\(/ { print FILENAME ":" FNR ": " $0; next }
        /use [^;]*vspec::/ && /flags/ && x != "use crate::vspec::flags::RouteByte;" { print FILENAME ":" FNR ": " $0; next }
        /S_FREE_FLAGS/ && /(write|copy|replace|swap)/ {
            if (x != "ptr_mut_write(field_at(this, S_FREE_FLAGS), Tracked(pa), crate::vspec::flags::new(flags));") print FILENAME ":" FNR ": " $0
            else nw++
        }
        /vspec::flags::(new|load|fetch_or)\(/ {
            if (FILENAME != "src/heap/shared.rs") { print FILENAME ":" FNR ": " $0; next }
            if (x == "ptr_mut_write(field_at(this, S_FREE_FLAGS), Tracked(pa), crate::vspec::flags::new(flags));") nn++
            else if (x == "crate::vspec::flags::load(&self.free_flags, Ordering::Relaxed)") nl++
            else if (x == "crate::vspec::flags::fetch_or(&self.free_flags, FREE_ROUTE, Ordering::Relaxed);") nf++
            else print FILENAME ":" FNR ": " $0
        }
        END { if (nw != 1 || nn != 1 || nl != 1 || nf != 1) print "src/heap/shared.rs: flag-byte writes through S_FREE_FLAGS " (nw + 0) ", calls of new " (nn + 0) ", load " (nl + 0) ", fetch_or " (nf + 0) ", not 1 each" }' {} +
    )
nstate=$(find src -name '*.rs' ! -path 'src/loom_tests/*' -exec grep -hE '(^|[^A-Za-z0-9_])STATE\.(store|swap|compare_exchange|fetch_)' {} + | wc -l | tr -d ' ')
[ "$nstate" = 2 ] || bad="$bad
src/api/mod.rs: $nstate writes of STATE, not 2 (records api-state_claim.md, api-state_load.md, api-state_publish.md)"
nline=$(awk '/^#\[cfg\(test\)\]$/ { t = 1; next } t && /^mod tests/ { exit } { t = 0 }
    /(^|[^A-Za-z0-9_])LINE_STATE\.(store|swap|compare_exchange|compare_exchange_weak|fetch_[a-z]+)\(/ && !/^[ \t]*\/\// { n++ }
    END { print n + 0 }' src/api/mod.rs)
nother=$(find src -name '*.rs' ! -path src/api/mod.rs ! -path 'src/loom_tests/*' -exec grep -lE 'LINE_STATE' {} + | wc -l | tr -d ' ')
[ "$nline" = 2 ] && [ "$nother" = 0 ] || bad="$bad
src/api/mod.rs: $nline writes of LINE_STATE outside the tests, not 2, or another file names it ($nother) (records api-line_compare_exchange.md, api-line_store.md)"
if [ -n "$bad" ]; then
	echo "loom: an atomic access outside what the records allow (vspec-word-WordPerm.md, the vspec/count.rs, vspec/atom.rs, vspec/byte.rs and vspec/flags.rs wrappers' records, api-state_*.md, the LINE_STATE wrappers' records, slab-SlabDesc-*.md):" >&2
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
