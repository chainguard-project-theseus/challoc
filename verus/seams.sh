#!/bin/sh
# verus/seams.sh: the seam ledger, every trusted item the Verus proofs rest on.
#
#   sh verus/seams.sh            print the totals on stdout
#   sh verus/seams.sh --write    regenerate the ledger under verus/ledger/
#   sh verus/seams.sh --check    exit 1 if the ledger under verus/ledger/ is stale
#                                or a status cites a check that is gone
#
# The ledger is one file per source file with seams: the seams of
# src/a/b.rs are in verus/ledger/src/a/b.rs.md. Pull requests that add
# seams to different source files touch different ledger files.
#
# Scans src/ (test files and `#[cfg(test)]` items skipped) for the items
# Verus takes on trust:
#   external_body        `#[verifier::external_body]` exec fn (body unchecked)
#   external_body proof  `#[verifier::external_body]` proof fn (an axiom)
#   external_body type   `#[verifier::external_body]` struct or enum
#   external_body const  `#[verifier::external_body]` exec const or static
#   axiom                `axiom fn`, `broadcast axiom fn`, `proof fn axiom_*`
#   assume_spec          `assume_specification[ path ]`
#   ext_type_spec        `#[verifier::external_type_specification]`
#   ext_trait_spec       `#[verifier::external_trait_specification]`
#   spec_impl            `impl ...SpecImpl for T` (a vstd::std_specs trait):
#                        every PartialEqSpecImpl, and a fmt-trait one
#                        (DisplaySpecImpl, ...) unless T's trait impl is
#                        inside `verus!` in the same file
#   no_decreases         `#[verifier::exec_allows_no_decreases_clause]`
#   assume_new           `Tracked::assume_new()`/`Ghost::assume_new()`
#                        inside `verus!`, outside tests and outside
#                        external_body bodies (those inside are noted on
#                        their function's entry; code outside `verus!` is
#                        not verified, so its own are no seam)
# and prints, per item, its place, the contract Verus assumes (the
# signature with its requires/ensures, verbatim), a category and a
# proposed discharging tool. Categories and tools come from heuristics on
# the item's kind, path, name and body. The seams of src/a/b.rs may have
# an overrides file, verus/ledger/src/a/b.rs.overrides (one
# `name<TAB>category<TAB>tool[<TAB>status]` per line, `#` comments; the
# name as printed in that file's ledger, or `name#N` for the Nth seam of
# that name in the file, in source order), which overrides them and
# carries every status other than "trusted". --check also fails when a
# status cites `Kani: <harness>` and no `#[kani::proof]` fn has that path,
# or `const assert: <label> in <file>` and not exactly one assertion there
# carries that label, or `Isla: <file>` and that file lists no such seam
# or the seam's asm! calls differ from the templates.txt next to it, or
# `emptyasm: <fn> in <file>` and the seam is not `<fn>` of `<file>`, not
# exactly one row of verus/emptyasm/blocks.txt names them, or the seam's
# item holds other than one asm! call with the empty template
# `"/* {0} */"`; and when a line verus/kani/pins.txt lists (the source a
# Kani harness runs a copy of) is not in place, or a copy those lines
# mark differs from its original but for the rows marked `-only`.
# The output depends only on the tree: files are read in byte order.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT" || exit 2
export LC_ALL=C
tab=$(printf '\t')

mode=print
case ${1:-} in
  '') ;;
  --write) mode='write' ;;
  --check) mode='check' ;;
  *) sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac

files=$(find src -name '*.rs' ! -name tests.rs ! -name linux_tests.rs \
  ! -name testutil.rs | sort)
[ -n "$files" ] || { echo "seams: no sources under src/" >&2; exit 2; }
ovfiles=$(find verus/ledger -name '*.rs.overrides' 2>/dev/null | sort)

prog=$(cat <<'EOF'
function trim(s) { sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); return s }

# The line with string and char literal contents and `//` comments
# blanked, same length, so braces and keywords in it are code.
function code(s,    out, i, n, ch, instr, j) {
    if (s ~ /^[ \t]*\/\//) return ""
    if (index(s, "\"") == 0 && index(s, "'") == 0) {
        i = index(s, "//")
        return i ? substr(s, 1, i - 1) : s
    }
    out = ""; n = length(s); instr = 0
    for (i = 1; i <= n; i++) {
        ch = substr(s, i, 1)
        if (instr) {
            if (ch == "\\") { out = out "xx"; i++; continue }
            if (ch == "\"") { instr = 0; out = out ch } else out = out "x"
            continue
        }
        if (ch == "\"") { instr = 1; out = out ch; continue }
        if (ch == "/" && substr(s, i + 1, 1) == "/") {
            for (j = i; j <= n; j++) out = out " "
            break
        }
        if (ch == "'") {
            if (substr(s, i + 2, 1) == "'" && substr(s, i + 1, 1) != "\\") {
                out = out "'x'"; i += 2; continue
            }
            if (substr(s, i + 1, 1) == "\\") {
                j = index(substr(s, i + 2), "'")
                if (j > 0 && j <= 9) {
                    out = out "'"; for (; j > 0; j--) out = out "x"
                    out = out "'"; i = i + 1 + length(substr(s, i + 2, index(substr(s, i + 2), "'"))); continue
                }
            }
        }
        out = out ch
    }
    return out
}

function count(s, ch,    n, i) {
    n = 0
    for (i = index(s, ch); i > 0; i = index(s, ch)) { n++; s = substr(s, i + 1) }
    return n
}

# Name of the type an `impl` line implements.
function impl_name(t,    s, d, i, ch, k) {
    s = t
    sub(/^.*impl/, "", s)
    if (substr(s, 1, 1) == "<") {
        d = 0
        for (i = 1; i <= length(s); i++) {
            ch = substr(s, i, 1)
            if (ch == "<") d++
            else if (ch == ">") { d--; if (d == 0) break }
        }
        s = substr(s, i + 1)
    }
    s = trim(s)
    k = index(s, " for ")
    while (k > 0) { s = substr(s, k + 5); k = index(s, " for ") }
    sub(/[< {].*$/, "", s)
    sub(/^.*::/, "", s)
    return s
}

function qual(n) { return nimpl > 0 ? implname[nimpl] "::" n : n }

# Trait and type of a trait impl line, as a key: `impl fmt::Display for T`
# and `impl vstd::std_specs::fmt::DisplaySpecImpl for crate::m::T` both
# give Display and T.
function trait_key(t,    tr) {
    tr = t; sub(/ for .*$/, "", tr); sub(/^impl(<[^>]*>)? /, "", tr)
    sub(/^([A-Za-z0-9_]+::)+/, "", tr); sub(/<.*$/, "", tr); sub(/SpecImpl$/, "", tr)
    return tr SUBSEP impl_name(t)
}

# The bracketed path of an `assume_specification`.
function spec_path(s,    i, d, ch, k) {
    k = index(s, "[")
    if (k == 0) return ""
    s = substr(s, k + 1); d = 1
    for (i = 1; i <= length(s); i++) {
        ch = substr(s, i, 1)
        if (ch == "[") d++
        else if (ch == "]") { d--; if (d == 0) break }
    }
    return trim(substr(s, 1, i - 1))
}

# The entry's body is not checked by Verus: an external_body item.
function unchecked(e) {
    return ekind[e] ~ /^external_body/
}

# Start an entry at the current line.
function begin(kind, name,    ind) {
    n++
    efile[n] = FILE; eline[n] = LN; ekind[n] = kind; ename[n] = name
    eattrs[n] = attrs; edoc[n] = doc; ebody[n] = ""; enote[n] = ""
    econtract[n] = ""; efmt[n] = ""
    ind = $0; sub(/[^ \t].*$/, "", ind); eind[n] = length(ind)
    return n
}

function addline(e, raw) {
    if (substr(raw, 1, eind[e]) ~ /^[ \t]*$/) raw = substr(raw, eind[e] + 1)
    econtract[e] = econtract[e] (econtract[e] == "" ? "" : "\n") raw
}

# Feed one line of an item being captured; returns 1 once the item ends.
# Phase 1 is the signature and clauses (the contract), phase 2 the body.
function feed(raw, c,    i, len, ch, t, cut) {
    len = length(c); t = trim(c)
    if (phase == 1) {
        if (pd == 0 && bd == 0 && t ~ /^(requires|ensures|recommends|returns|decreases|opens_invariants|no_unwind|default_ensures)([ \t]|$)/) clauses = 1
        cut = 0
        for (i = 1; i <= len; i++) {
            ch = substr(c, i, 1)
            if (ch == "(" || ch == "[") pd++
            else if (ch == ")" || ch == "]") pd--
            else if (ch == "}") bd--
            else if (ch == "{") {
                if (pd == 0 && bd == 0 && !wholebody && (!clauses || trim(substr(c, 1, i - 1)) == "")) { cut = i; break }
                bd++
            } else if (ch == ";" && pd == 0 && bd == 0) {
                addline(cur, raw); return 1
            }
        }
        if (wholebody && bd == 0 && pd == 0 && t ~ /}$/) { addline(cur, raw); return 1 }
        if (!cut) { addline(cur, raw); return 0 }
        if (trim(substr(raw, 1, cut - 1)) != "") addline(cur, substr(raw, 1, cut - 1))
        sub(/[ \t]+$/, "", econtract[cur])
        phase = 2; bodyd = 0; c = substr(c, cut)
    }
    bodyd += count(c, "{") - count(c, "}")
    ebody[cur] = ebody[cur] " " c
    return bodyd <= 0
}

function finish_doc(    s) {
    s = doc; sub(/\. .*$/, ".", s); return s
}

newfile {
    newfile = 0
    depth = 0; nimpl = 0; skip = 0; cap = 0; attrs = ""; doc = ""
    pendtest = 0; lastfn = ""; pendimpl = ""; inverus = 0
}

{
    raw = $0; c = code(raw); t = trim(c); rawt = trim(raw)
    opens = count(c, "{"); closes = count(c, "}")

    if (skip) {
        depth += opens - closes
        if (depth <= skipd) skip = 0
        next
    }

    if (cap) {
        if (phase == 2 && c ~ /(Tracked|Ghost)::assume_new\(\)/ && !unchecked(cur)) {
            e = begin(c ~ /Tracked::assume_new/ ? "assume_new" : "ghost_assume_new", ename[cur])
            econtract[e] = rawt
        }
        if (phase == 2 && unchecked(cur)) {
            if (c ~ /Ghost::assume_new\(\)/) enote[cur] = enote[cur] "mints Ghost at line " LN "; "
            if (c ~ /Tracked::assume_new\(\)/) enote[cur] = enote[cur] "mints Tracked at line " LN "; "
        }
        if (feed(raw, c)) cap = 0
        depth += opens - closes
        while (nimpl > 0 && depth <= impld[nimpl]) nimpl--
        next
    }

    if (rawt ~ /^\/\/\//) {
        s = rawt; sub(/^\/\/\/ ?/, "", s)
        if (s != "") doc = doc (doc == "" ? "" : " ") s
        next
    }
    if (t == "") next
    if (t ~ /^#!?\[/) {
        attrs = attrs " " t
        if (t ~ /^#\[cfg\((all\()?test[,)]/) pendtest = 1
        if (t ~ /^#\[cfg\(test\)\]/) pendtest = 1
        next
    }

    # An item or statement line, with the attributes above it.
    # A test-only item is skipped from its first line through its body,
    # which may open lines below a multi-line fn signature (pendtest 2).
    if (pendtest) {
        attrs = ""; doc = ""
        if (opens > closes) { pendtest = 0; skip = 1; skipd = depth; depth += opens - closes }
        else if (pendtest == 1 && t ~ /(^|[ \t])fn [A-Za-z_]/ && t !~ /;$/) pendtest = 2
        else if (pendtest == 1 || t ~ /;$/ || closes > opens) pendtest = 0
        next
    }

    kind = ""; name = ""; wholebody = 0
    isfn = match(t, /(^|[ \t])fn [A-Za-z_][A-Za-z0-9_]*/)
    if (isfn) { fname = substr(t, RSTART, RLENGTH); sub(/^.*fn /, "", fname) }
    if (t ~ /assume_specification/ && t !~ /^use /) {
        kind = "assume_spec"; name = spec_path(t)
    } else if (attrs ~ /external_type_specification/) {
        kind = "ext_type_spec"; name = t
        if (match(name, /\(.*\)/)) name = substr(name, RSTART + 1, RLENGTH - 2)
        else { sub(/^.*struct /, "", name); sub(/[^A-Za-z0-9_].*$/, "", name) }
    } else if (attrs ~ /external_trait_specification/) {
        kind = "ext_trait_spec"; name = t; sub(/^.*trait /, "", name); sub(/[^A-Za-z0-9_].*$/, "", name)
        wholebody = 1
    } else if (t ~ /^impl[< ].*SpecImpl(<[^>]*>)? for /) {
        kind = "spec_impl"; name = t; sub(/^impl(<[^>]*>)? /, "", name); sub(/ *\{$/, "", name)
        sub(/^([a-z_]+::)+/, "", name); sub(/ for crate::/, " for ", name)
        wholebody = 1
    } else if (attrs ~ /verifier::external_body/ && t ~ /(^|[ \t])(struct|enum) /) {
        kind = "external_body type"; name = t; sub(/^.*(struct|enum) /, "", name); sub(/[^A-Za-z0-9_].*$/, "", name)
        name = qual(name)
    } else if (!isfn && attrs ~ /verifier::external_body/ && t ~ /(^|[ \t])(const|static) [A-Z_][A-Z0-9_]*[:<]/) {
        match(t, /(const|static) [A-Z_][A-Z0-9_]*/); name = substr(t, RSTART, RLENGTH)
        kind = "external_body const"; sub(/^[a-z]+ /, "", name)
        name = qual(name)
    } else if (isfn && t ~ /(^|[ \t])axiom fn /) {
        kind = "axiom"; name = qual(fname)
    } else if (isfn && attrs ~ /verifier::external_body/ && t ~ /(^|[ \t])proof fn /) {
        kind = "external_body proof"; name = qual(fname)
    } else if (isfn && attrs ~ /verifier::external_body/) {
        kind = "external_body"; name = qual(fname)
    } else if (isfn && fname ~ /^axiom_/ && t ~ /(^|[ \t])proof fn /) {
        kind = "axiom"; name = qual(fname)
    } else if (isfn && attrs ~ /exec_allows_no_decreases_clause/) {
        kind = "no_decreases"; name = qual(fname)
    }
    if (isfn) lastfn = qual(fname)

    if (kind == "" && inverus && c ~ /(Tracked|Ghost)::assume_new\(\)/) {
        e = begin(c ~ /Tracked::assume_new/ ? "assume_new" : "ghost_assume_new", lastfn)
        econtract[e] = rawt
    }

    if (kind != "") {
        cur = begin(kind, name); phase = 1; pd = 0; bd = 0; clauses = 0
        cap = !feed(raw, c)
        if (kind == "spec_impl" && name ~ /^(Display|Debug|LowerHex|UpperHex|Octal|Binary|LowerExp|UpperExp|Pointer)SpecImpl/) efmt[cur] = trait_key(t)
    }
    attrs = ""; doc = ""

    if (t ~ /^(pub(\([a-z]+\))? )?(unsafe )?impl[< ]/) {
        if (inverus && t ~ / for / && t !~ /SpecImpl/) verified_impl[FILE SUBSEP trait_key(t)] = 1
        if (opens > closes) { nimpl++; implname[nimpl] = impl_name(t); impld[nimpl] = depth }
        else if (opens == 0) pendimpl = impl_name(t)
    } else if (pendimpl != "" && opens > closes) {
        nimpl++; implname[nimpl] = pendimpl; impld[nimpl] = depth; pendimpl = ""
    }
    if (t ~ /^verus! *\{/) { inverus = 1; verusd = depth }
    depth += opens - closes
    while (nimpl > 0 && depth <= impld[nimpl]) nimpl--
    if (inverus && depth <= verusd) inverus = 0
}
EOF
)

marker=$(cat <<'EOF'
/^\001FILE / { FILE = substr($0, 7); LN = 0; newfile = 1; next }
/^\001FAIL / { print "seams: cannot read " substr($0, 7) > "/dev/stderr"; bad = 1; next }
{ LN++ }
EOF
)

overrides=$(cat <<'EOF'
FILE ~ /^verus\/ledger\/.*\.rs\.overrides$/ {
    if ($0 ~ /^#/ || $0 ~ /^[ \t]*$/) next
    nf = split($0, f, "\t")
    if (nf < 3) { print FILE ":" LN ": want name<TAB>category<TAB>tool" > "/dev/stderr"; bad = 1; next }
    # The key is the source file and the name: src/a/b.rs:name[#N].
    k = substr(FILE, 14, length(FILE) - 23) ":" f[1]
    ocat[k] = f[2]; otool[k] = f[3]; ostat[k] = nf >= 4 ? f[4] : ""
    oseen[k] = 0; nover++; okey[nover] = k; ofile[k] = FILE
    next
}
EOF
)

render=$(cat <<'EOF'
function tool_of(cat) {
    if (cat == "inline asm") return "Islaris/Isla"
    if (cat == "FFI/syscall") return "none (assumption)"
    if (cat == "function pointer") return "Kani"
    if (cat == "UnsafeCell/volatile/transmute") return "RefinedRust"
    if (cat == "raw pointer (internal)") return "RefinedRust"
    if (cat == "raw pointer from C caller") return "Miri (evidence only)"
    if (cat == "panic/abort") return "Kani"
    if (cat == "atomics primitive") return "loom/GenMC"
    if (cat == "platform/kernel fact") return "none (assumption)"
    if (cat == "C-program contract") return "none (assumption)"
    if (cat == "termination") return "loom/GenMC"
    if (cat == "core library") return "Kani"
    if (cat == "allocator invariant") return "RefinedRust"
    return "?"
}

# The heuristic category of entry e.
function guess(e,    k, b, nm, f) {
    k = ekind[e]; b = ebody[e]; nm = ename[e]; f = efile[e]
    if (k == "no_decreases") return "termination"
    if (b ~ /asm!/) return "inline asm"
    if (k == "assume_spec" || k == "ext_type_spec" || k == "ext_trait_spec" || k == "spec_impl") {
        if (nm ~ /Atomic/) return "atomics primitive"
        if (nm ~ /UnsafeCell|volatile|transmute/) return "UnsafeCell/volatile/transmute"
        if (nm ~ /^(core::)?ptr::|^<\*|::add$|::offset$|from_raw_parts|null/) return "raw pointer (internal)"
        if (nm ~ /^crate::|^sys::|^super::/) return ""
        return "core library"
    }
    if (k == "assume_new" || k == "ghost_assume_new") return f ~ /^src\/api\// ? "raw pointer from C caller" : "raw pointer (internal)"
    if (b ~ /(raw::)?syscall[0-6]?\(|sys::(mmap|munmap|madvise|mprotect|read|write|open|close)/) return "FFI/syscall"
    if (b ~ /\(\*?[a-z_.]*\.0\)\(|[^a-z]fn\(/) return "function pointer"
    if (b ~ /format_args!|write!|writeln!|panic!/) return b ~ /panic!/ && b !~ /write!|writeln!/ ? "panic/abort" : "function pointer"
    if (b ~ /volatile|UnsafeCell|transmute|\.get\(\)/) return "UnsafeCell/volatile/transmute"
    if (b ~ /compare_exchange|fetch_[a-z]+\(|\.load\(|\.store\(/) return "atomics primitive"
    if (b ~ /\*mut|\*const|ptr::|as_ptr|\.add\(|from_raw_parts/) return f ~ /^src\/api\// ? "raw pointer from C caller" : "raw pointer (internal)"
    if (k == "axiom" || k == "external_body proof") return nm ~ /lock_inv|lock_frame|lock$/ ? "allocator invariant" : ""
    return ""
}

# Item name x in at most w characters: the crate and `core` paths
# dropped, then the middle elided.
function short(x, w,    h) {
    sub(/^crate::/, "", x); sub(/^core::sync::atomic::/, "", x); sub(/^core::/, "", x)
    if (w < 16) w = 16
    if (length(x) <= w) return x
    h = int((w - 3) / 2)
    return substr(x, 1, w - 3 - h) "..." substr(x, length(x) - h + 1)
}

function md(s) { gsub(/\|/, "\\|", s); return s }

END {
    if (bad) exit 2
    # A fmt-trait spec impl (DisplaySpecImpl, ...) is no seam when its
    # type's trait impl is inside `verus!` in the same file: vstd makes
    # that verified `fmt` require `fmt_req`, and every `format_args!` site
    # prove it. One whose trait impl is in another file stays listed; the
    # scan does not match impls across files.
    m = 0
    for (e = 1; e <= n; e++) {
        if (efmt[e] != "" && (efile[e] SUBSEP efmt[e]) in verified_impl) continue
        if (++m == e) continue
        efile[m] = efile[e]; eline[m] = eline[e]; ekind[m] = ekind[e]; ename[m] = ename[e]
        eattrs[m] = eattrs[e]; edoc[m] = edoc[e]; ebody[m] = ebody[e]; enote[m] = enote[e]
        econtract[m] = econtract[e]; eind[m] = eind[e]; efmt[m] = efmt[e]
    }
    n = m
    ncat = split("inline asm|FFI/syscall|function pointer|UnsafeCell/volatile/transmute|raw pointer (internal)|raw pointer from C caller|panic/abort|atomics primitive|platform/kernel fact|C-program contract|termination|core library|allocator invariant", cats, "|")
    ntool = split("Islaris/Isla|RefinedRust|Kani|loom/GenMC|Verus|Miri (evidence only)|none (assumption)", tools, "|")
    nkind = split("external_body|external_body proof|external_body type|external_body const|axiom|assume_spec|ext_type_spec|ext_trait_spec|spec_impl|no_decreases|assume_new|ghost_assume_new", kinds, "|")
    for (i = 1; i <= ncat; i++) catok[cats[i]] = 1
    for (i = 1; i <= ntool; i++) toolok[tools[i]] = 1
    for (e = 1; e <= n; e++) {
        key = efile[e] ":" ename[e]
        keyn = key "#" (++nocc[key])
        if (keyn in ocat) ok = keyn; else if (key in ocat) ok = key; else ok = ""
        if (ok != "") {
            oseen[ok] = 1; ecat[e] = ocat[ok]; etool[e] = otool[ok]
            estat[e] = ostat[ok] == "" ? "trusted" : ostat[ok]
            if (etool[e] == "") etool[e] = tool_of(ecat[e])
        } else {
            ecat[e] = guess(e); etool[e] = tool_of(ecat[e]); estat[e] = "trusted"
        }
        if (!(ecat[e] in catok)) { print "seams: " efile[e] ":" eline[e] ": " ename[e] ": no category (add it to verus/ledger/" efile[e] ".overrides)" > "/dev/stderr"; bad = 1 }
        else if (!(etool[e] in toolok)) { print "seams: " efile[e] ":" eline[e] ": " ename[e] ": unknown tool " etool[e] > "/dev/stderr"; bad = 1 }
        ncatn[ecat[e]]++; ntooln[etool[e]]++; nkindn[ekind[e]]++
        ct[ecat[e] SUBSEP etool[e]]++
    }
    for (i = 1; i <= nover; i++) if (!oseen[okey[i]]) { k = okey[i]; sub(/^[^:]*:/, "", k); print "seams: " ofile[okey[i]] ": " k " matches no seam" > "/dev/stderr"; bad = 1 }

    if (bad) exit 1
    if (mode == "summary") {
        print n (n == 1 ? " seam" : " seams") ", in " (nf = nfiles_with_seams()) (nf == 1 ? " source file." : " source files.")
        print ""
        print "| category | seams |"
        print "|---|---:|"
        for (i = 1; i <= ncat; i++) if (ncatn[cats[i]] > 0) print "| " cats[i] " | " ncatn[cats[i]] " |"
        print ""
        print "| tool | seams |"
        print "|---|---:|"
        for (j = 1; j <= ntool; j++) if (ntooln[tools[j]] > 0) print "| " tools[j] " | " ntooln[tools[j]] " |"
        print ""
        print "| kind | seams |"
        print "|---|---:|"
        for (i = 1; i <= nkind; i++) if (nkindn[kinds[i]] > 0) print "| " kinds[i] " | " nkindn[kinds[i]] " |"
        print ""
        print "| category | tool | seams |"
        print "|---|---|---:|"
        for (i = 1; i <= ncat; i++) for (j = 1; j <= ntool; j++)
            if (ct[cats[i] SUBSEP tools[j]] > 0) print "| " cats[i] " | " tools[j] " | " ct[cats[i] SUBSEP tools[j]] " |"
        exit 0
    }
    # One ledger file per source file; entries are grouped by file, in
    # source order. A name that several seams of one file share gets #N.
    for (e = 1; e <= n; e++) {
        key = efile[e] ":" ename[e]
        eid[e] = nocc[key] > 1 ? ename[e] "#" (++nid[key]) : ename[e]
    }
    for (e = 1; e <= n; e = last + 1) {
        sf = efile[e]
        for (last = e; last < n && efile[last + 1] == sf; last++) ;
        out = outdir "/" sf ".md"
        d = out; sub(/\/[^\/]*$/, "", d)
        if (system("mkdir -p '" d "'") != 0) exit 2
        print "# Seams in `" sf "`" > out
        print "" > out
        print "Generated by `sh verus/seams.sh --write`; do not edit it by hand." > out
        print "[SEAMS.md](" rel(sf) "SEAMS.md) says what a seam, a category and a tool" > out
        print "are. Set a category, tool or status in `" substr(sf, length(sf) - index(rev(sf), "/") + 2) ".overrides` next to this" > out
        print "file." > out
        print "" > out
        print "| item | kind | category | tool | status |" > out
        print "|---|---|---|---|---|" > out
        for (i = e; i <= last; i++) {
            s = estat[i]; sub(/ .*$/, "", s)
            tail = " | " ekind[i] " | " ecat[i] " | " etool[i] " | " s " |"
            print "| `" md(short(eid[i], 80 - length(tail) - 5)) "`" tail > out
        }
        for (i = e; i <= last; i++) {
            print "" > out
            print "## `" eid[i] "`" > out
            print "" > out
            print sf ":" eline[i] ". Kind: " ekind[i] ". Category: " ecat[i] "." > out
            print "Tool: " etool[i] ". Status: " estat[i] "." > out
            s = edoc[i]; sub(/\. .*$/, ".", s)
            if (s != "") { print "" > out; print "> " s > out }
            if (enote[i] != "") { s = enote[i]; sub(/; $/, ".", s); print "" > out; print "Body " s > out }
            print "" > out
            print "```rust" > out
            print econtract[i] > out
            print "```" > out
        }
        close(out)
    }
}

# The path from verus/ledger/<sf>.md back up to verus/.
function rel(f,    r, i) {
    r = "../"
    for (i = 1; i <= length(f); i++) if (substr(f, i, 1) == "/") r = r "../"
    return r
}

function rev(s,    r, i) { r = ""; for (i = length(s); i >= 1; i--) r = r substr(s, i, 1); return r }

function nfiles_with_seams(    e, c, p) { c = 0; p = ""; for (e = 1; e <= n; e++) if (efile[e] != p) { c++; p = efile[e] }; return c }
EOF
)

# Busybox awk reads files given by name very slowly from some file
# systems; the files go through one pipe instead, each behind a marker line.
stream() {
  for f in $ovfiles $files; do
    printf '\001FILE %s\n' "$f"
    cat "$f" || printf '\001FAIL %s\n' "$f"
  done
}

# gen MODE DIR: the totals on stdout (summary), or the ledger files under DIR.
gen() {
  stream | awk -v mode="$1" -v outdir="$2" "$marker
$overrides
$prog
$render"
}

# The checks the overrides statuses cite: one line `<overrides file> <tag>
# <item>` per item of each `Kani: a, b and c`, `const assert: a, b and
# c`, `Isla: FILE` or `emptyasm: a` list (up to `;`, `)` or the next
# tag), tag `Kani`, `assert`, `Isla` (its item `<seam> in FILE`) or
# `emptyasm` (its item `<seam> @ <fn> in <file>`); `none` for a
# "discharged" or "partly checked" status, or a list, that cites nothing.
cites() {
  for o in $ovfiles; do
    awk -F '\t' -v o="$o" 'BEGIN { tag["Kani: "] = "Kani"; tag["const assert: "] = "assert"; tag["Isla: "] = "Isla"; tag["emptyasm: "] = "emptyasm" }
    !/^#/ && NF >= 4 {
      k = 0
      for (t in tag) {
        s = $4
        while ((i = index(s, t)) > 0) {
          s = substr(s, i + length(t)); l = s; sub(/[;)].*$/, "", l)
          n = split(l, h, /,? and |, */); m = 0
          for (j = 1; j <= n; j++) {
            if (index(h[j], ": ")) break
            if (h[j] != "") { print o, tag[t], (tag[t] == "Isla" ? $1 " in " h[j] : (tag[t] == "emptyasm" ? $1 " @ " h[j] : h[j])); m++ }
          }
          k++; if (m == 0) print o, "none", $1
        }
      }
      if (k == 0 && $4 ~ /^(discharged|trusted \(partly checked)/) print o, "none", $1
    }' "$o"
  done
}

# harness PATH: exit 0 if PATH names a `#[kani::proof]` fn.
# `kani_proofs::m::f` is `f` in src/kani/m.rs; `a::b::m::f` is `f` in the
# inline `mod m` of src/a/b.rs (or src/a/b/mod.rs), one `mod` per segment
# after the file's. Inline mods are found by rustfmt's indentation.
harness() {
  # shellcheck disable=SC2046 # one word per path segment
  set -- $(echo "$1" | sed 's/::/ /g')
  [ $# -ge 2 ] && [ "$1" != kani ] || return 1
  if [ "$1" = kani_proofs ]; then shift; set -- kani "$@"; fi
  segs=$*; n=$(($# - 1)); file=''; mods=''
  while [ "$n" -ge 1 ]; do
    p=src/$(echo "$segs" | cut -d' ' -f1-"$n" | tr ' ' /)
    if [ -f "$p.rs" ]; then file=$p.rs; elif [ -f "$p/mod.rs" ]; then file=$p/mod.rs; fi
    [ -n "$file" ] && break
    n=$((n - 1))
  done
  [ -n "$file" ] || return 1
  mods=$(echo "$segs" | cut -d' ' -f$((n + 1))- | awk '{ $NF = ""; print }')
  awk -v mods="$mods" -v f="${segs##* }" '
    BEGIN { nm = split(mods, m, " "); d = 0; ind = ""; pub = "(pub(\\([a-z]+\\))? )?" }
    d < nm && $0 ~ ("^" ind pub "mod " m[d + 1] " \\{$") { d++; ind = ind "    "; p = 0; next }
    d > 0 && $0 == substr(ind, 5) "}" { exit 1 }
    d == nm && $0 ~ ("^" ind pub "fn " f "[(<]") { rc = !p; done = 1; exit }
    $0 ~ ("^" ind "#\\[kani::proof\\]$") { p = 1; next }
    $0 !~ ("^" ind "(#\\[|//)") { p = 0 }
    END { exit done ? rc : 1 }
  ' "$file"
}

# isla OVERRIDES '<seam> in FILE': exit 0 if FILE (verus/isla/seams.md) lists
# the seam of OVERRIDES' source file (a "## `SRC` `<seam>` (ARCH)" heading).
# islatemplates checks the seam's asm! templates; verus/isla/run.sh checks
# the rest: contract, architecture and jobs.
isla() {
  name=${2% in *}; file=${2##* in }; src=${1#verus/ledger/}; src=${src%.overrides}
  [ -f "$file" ] || return 1
  awk -v h="## \`$src\` \`$name\` (" 'index($0, h) == 1 { f = 1 } END { exit !f }' "$file"
}

# islaasm SRC LINE: the asm! calls of the item that starts at SRC:LINE
# (up to the `}` that closes its body, or its `;`), one per line as
# `asm<TAB><call>`, verbatim up to whitespace: each source line trimmed,
# the lines joined by one space, from `asm!(` to its matching `)` and the
# `;` right after it if any. Then `trap<TAB><literal>` for each string
# literal outside them that names an instruction that traps or reads or
# writes a system register (svc, hvc, smc, brk, udf, hlt, mrs, msr,
# eret, tlbi, syscall, sysenter, sysret, int, int3, ud2, wrmsr, rdmsr,
# or raw .inst/.word bytes, in any case), which an asm! call could take
# as a constant, and for each `asm!` written with `{`, `[` or a space
# before its `(`, which this scan does not read.
islaasm() {
  awk -v n="$2" '
    # The column of the first `asm!(` in s not part of a longer name, or 0.
    function at(s) { if (!match(s, /(^|[^A-Za-z0-9_])asm!\(/)) return 0; return substr(s, RSTART, 1) == "a" ? RSTART : RSTART + 1 }
    NR < n { next }
    {
      line = $0; sub(/^[ \t]+/, "", line); out = ""; esc = 0
      # Only `asm!(` is read; a call with other delimiters fails the check.
      if (!inasm && line ~ /(^|[^A-Za-z0-9_])asm!([ \t]*[{[]|[ \t]+\()/) print "trap\tasm! with another delimiter: " line
      if (!inasm && (k = at(line)) > 0) { lead = substr(line, 1, k - 1); line = substr(line, k); inasm = 1; buf = ""; d = 0 }
      else lead = ""
      # The code before a call on its own line counts for braces too.
      for (i = 1; i <= length(lead); i++) { c = substr(lead, i, 1); if (c == "{") { body++; opened = 1 } else if (c == "}") body-- }
      for (i = 1; i <= length(line); i++) {
        c = substr(line, i, 1)
        if (instr) {
          if (inasm) out = out c; else lit = lit c
          if (esc) { esc = 0; continue }
          if (c == "\\") { esc = 1; continue }
          if (c != "\"") continue
          instr = 0
          if (!inasm && tolower(lit) ~ /(^|[^a-z0-9_])(svc|hvc|smc|brk|udf|hlt|eret|mrs|msr|tlbi|syscall|sysenter|sysret|int3|int [0-9]|ud2|wrmsr|rdmsr|\.inst|\.word)([^a-z0-9_]|$)/) print "trap\t\"" lit
          continue
        }
        if (!inasm && c == "/" && substr(line, i + 1, 1) == "/") break
        if (inasm) out = out c
        if (c == "\"") { instr = 1; lit = ""; continue }
        if (inasm) {
          if (c == "(") d++
          else if (c == ")" && --d == 0) {
            if (substr(line, i + 1, 1) == ";") { out = out ";"; i++ }
            print "asm\t" buf (buf == "" ? "" : " ") out
            inasm = 0; out = ""
            # Another call may start later on the line.
            rest = substr(line, i + 1)
            if ((k = at(rest)) > 0) {
              for (j = 1; j < k; j++) { c = substr(rest, j, 1); if (c == "{") { body++; opened = 1 } else if (c == "}") body-- }
              line = substr(rest, k); i = 0; inasm = 1; buf = ""; d = 0
            }
          }
          continue
        }
        if (c == "{") { body++; opened = 1 }
        else if (c == "}") body--
        else if (c == ";" && !opened && body == 0) exit
      }
      if (inasm) buf = buf (buf == "" ? "" : " ") out
      if (opened && body == 0) exit
    }' "$1"
}

# islatemplates TEMPLATES ITEMS: TEMPLATES (verus/isla/templates.txt,
# `SRC<TAB>FN<TAB>ASM` lines and `#` comments) against the items of the
# seams whose status cites `Isla:`, one `SRC<TAB>LINE<TAB>NAME<TAB>KIND`
# line each in ITEMS; FN is NAME without its `Type::` and `#N`. Prints a
# line for each asm! call of an item that TEMPLATES does not list under
# its SRC and FN (a changed or added template, such as a new `hvc`), each
# TEMPLATES line no such call reads (a template that changed or is gone,
# or a function that is no such seam), each exec item (kind
# `external_body`) with no asm! call, and each string literal in an item
# that names a trapping or system-register instruction (islaasm). Prints
# nothing when they agree.
islatemplates() {
  : > "$tmp/isla-have"
  while IFS="$tab" read -r s l name kind; do
    fn=${name%%#*}; fn=${fn##*::}
    islaasm "$s" "$l" | awk -F "$tab" -v s="$s" -v fn="$fn" -v name="$name" -v kind="$kind" -v have="$tmp/isla-have" '
      $1 == "asm" { print s "\t" fn "\t" substr($0, 5) >> have; n++ }
      $1 == "trap" { print "seams: Isla: " s " " name ": unchecked assembly (a string literal naming a trap or system-register instruction, or asm! without `(`): " substr($0, 6) }
      END { if (kind == "external_body" && !n) print "seams: Isla: " s " " name ": no asm! call to check" }'
  done < "$2"
  sort -u "$tmp/isla-have" > "$tmp/isla-have.sorted"
  grep -v -e '^#' -e '^$' "$1" | sort -u > "$tmp/isla-want"
  comm -23 "$tmp/isla-have.sorted" "$tmp/isla-want" | awk -F "$tab" -v t="$1" '{
    print "seams: Isla: " $1 " " $2 ": an asm! call that " t " does not list: " substr($0, length($1 $2) + 3) }'
  comm -13 "$tmp/isla-have.sorted" "$tmp/isla-want" | awk -F "$tab" -v t="$1" '{
    print "seams: Isla: " t ": no asm! call of a seam in " $1 " " $2 " reads: " substr($0, length($1 $2) + 3) }'
}

# assertion '<label> in FILE': exit 0 if exactly one top-level
# `const _: () = assert!(` in FILE sits under a `//` comment block whose
# first line starts with `// <label>`. An attribute right above the
# comment block (a line starting `#[` or ending `]`, as a multi-line
# `cfg` does), even across blank lines, fails it: a gated assertion is
# not evaluated in every build.
assertion() {
  label=${1% in *}; file=${1##* in }
  [ -f "$file" ] || return 1
  awk -v l="// $label" '
    /^\/\// { if (!c) g = (prev ~ /^#\[/ || prev ~ /\]$/); if (!c && index($0, l) == 1) h = 1; c = 1; prev = $0; next }
    h && /^const _: \(\) = assert!\($/ { n += g ? 2 : 1 }
    { c = 0; h = 0; if (NF) prev = $0 }
    END { exit n != 1 }
  ' "$file"
}

# emptyasm OVERRIDES '<seam> @ <fn> in FILE': exit 0 if FILE is the source
# file of OVERRIDES, <fn> is <seam> without its `Type::` and `#N`, and
# exactly one row of verus/emptyasm/blocks.txt names FILE and <fn>
# (verus/emptyasm/run.sh, `make emptyasm`, checks those blocks).
# emptyitem checks the seam's item.
emptyasm() {
  seam=${2%% @ *}; c=${2#* @ }; fn=${c% in *}; file=${c##* in }
  src=${1#verus/ledger/}; src=${src%.overrides}
  s=${seam%%#*}; s=${s##*::}
  [ "$file" = "$src" ] && [ "$fn" = "$s" ] || return 1
  awk -F '\t' -v fn="$fn" -v file="$file" '
    !/^#/ && $1 == file && $2 == fn { n++ }
    END { exit n != 1 }
  ' verus/emptyasm/blocks.txt
}

# emptyitem SRC LINE: exit 0 if the item that starts at SRC:LINE holds
# exactly one asm! call, whose template is the empty `"/* {0} */"` and whose
# first operand is `inout(reg) <ident>` (so the register comes out as it
# went in), and no string literal that islaasm flags.
emptyitem() {
  islaasm "$1" "$2" | awk -F '\t' '
    $1 == "asm" && $2 ~ /^asm!\("\/\* [{]0[}] \*\/", inout\(reg\) [A-Za-z_][A-Za-z0-9_]*, / { e++ }
    { n++ }
    END { exit !(n == 1 && e == 1) }'
}

# pins PINS: for each file PINS (verus/kani/pins.txt, `FILE<TAB>LINE`
# lines, with an optional third field, and `#` comments) names, the first
# of its lines that FILE does not hold in place: the lines must appear in
# FILE in that order, each on the line after the one before (lines
# compared with leading and trailing whitespace removed; blank and `//`
# comment lines in FILE skipped), but where a `...` line allows any lines
# in between. The first line of each run (the first line, and each line
# after a `...`) must appear in FILE exactly once, so a second copy of a
# run (in a block comment, say) cannot stand in for the real one. A row
# with more than three fields fails. Prints nothing when every file holds
# its lines.
pins() {
  [ -f "$1" ] || { echo "seams: $1 is gone"; return; }
  awk -F '\t' -v pf="$1" '!/^#/ && NF > 3 { print "seams: " pf ": a row has more than three fields: " $0 }' "$1"
  awk -F '\t' '!/^#/ && (NF == 2 || NF == 3) { print $1 }' "$1" | sort -u | while IFS= read -r f; do
    if [ ! -f "$f" ]; then echo "seams: $1: $f is gone"; continue; fi
    awk -F '\t' -v pf="$1" -v f="$f" '
      FILENAME == pf {
        if ($0 !~ /^#/ && (NF == 2 || NF == 3) && $1 == f) { p[++n] = $2; if (n == 1 || p[n - 1] == "...") a[$2] = 0 }
        next
      }
      !started { gap = 1; started = 1 }
      { t = $0; sub(/^[ \t]+/, "", t); sub(/[ \t]+$/, "", t) }
      t == "" || t ~ /^\/\// { next }
      t in a { a[t]++ }
      done || k >= n { next }
      t == p[k + 1] { k++; gap = 0; if (p[k + 1] == "...") { k++; gap = 1 } next }
      !gap { done = 1 }
      END {
        if (k < n) print "seams: " pf ": " f " no longer has, in place: " p[k + 1]
        for (t in a) if (a[t] != 1) print "seams: " pf ": " f " has " a[t] " lines " t ", want 1"
      }
    ' "$1" "$f"
  done
}

# copies PINS: for each NAME the third field of PINS's rows names
# (`NAME:orig`, `NAME:orig-only`, `NAME:copy`, `NAME:copy-only`), where
# the copy's lines differ from the original's: the `NAME:orig` rows, in
# order, must equal the `NAME:copy` rows, and each side's rows (orig and
# orig-only, copy and copy-only) must be consecutive rows of PINS, so a
# row added inside either side must say which side it is on. As pins
# holds every row in place, equal rows mean the copy repeats the
# original's lines but for its `-only` rows. Prints nothing when every
# NAME's copy matches.
copies() {
  [ -f "$1" ] || return 0
  awk -F '\t' -v pf="$1" '
    /^#/ { next }
    NF != 2 && NF != 3 { next }
    { r++ }
    NF == 2 { next }
    {
      nm = $3; sub(/:[^:]*$/, "", nm); role = $3; sub(/^.*:/, "", role)
      if (nm == "" || (role != "orig" && role != "orig-only" && role != "copy" && role != "copy-only")) {
        print "seams: " pf ": row " r ": " $3 " is not NAME:orig, NAME:orig-only, NAME:copy or NAME:copy-only"; next
      }
      if (!(nm in seen)) { seen[nm] = 1; names[++nn] = nm }
      side = (role ~ /^orig/) ? "orig" : "copy"
      k = nm SUBSEP side
      if (!(k in first)) first[k] = r
      last[k] = r; cnt[k]++
      if (role == "orig") o[nm, ++no[nm]] = $2
      if (role == "copy") c[nm, ++nc[nm]] = $2
    }
    END {
      for (i = 1; i <= nn; i++) {
        nm = names[i]
        for (s = 1; s <= 2; s++) {
          side = s == 1 ? "orig" : "copy"; k = nm SUBSEP side
          if (!(k in first)) { print "seams: " pf ": " nm " has no " side " rows"; continue }
          if (last[k] - first[k] + 1 != cnt[k]) print "seams: " pf ": " nm ": the " side " rows are not consecutive"
        }
        if (!((nm SUBSEP "orig") in first) || !((nm SUBSEP "copy") in first)) continue
        if (no[nm] + 0 == 0) { print "seams: " pf ": " nm " has no NAME:orig rows"; continue }
        if (no[nm] != nc[nm]) { print "seams: " pf ": " nm ": the copy has " (nc[nm] + 0) " shared rows, the original " no[nm]; continue }
        for (j = 1; j <= no[nm]; j++) if (o[nm, j] != c[nm, j]) {
          print "seams: " pf ": " nm ": the copy has " c[nm, j] " where the original has " o[nm, j]; break
        }
      }
    }
  ' "$1"
}

case $mode in
  print) gen summary '' ;;
  write)
    tmp=$(mktemp -d) && trap 'rm -rf "$tmp"' EXIT
    gen write "$tmp/ledger"
    mkdir -p verus/ledger
    find verus/ledger -name '*.rs.md' -exec rm -f {} +
    if [ -d "$tmp/ledger" ]; then (cd "$tmp/ledger" && find . -name '*.rs.md' -print) | while read -r f; do
      mkdir -p "verus/ledger/$(dirname "$f")"
      cp "$tmp/ledger/$f" "verus/ledger/$f"
    done; fi ;;
  check)
    tmp=$(mktemp -d) && trap 'rm -rf "$tmp"' EXIT
    gen write "$tmp/ledger"
    mkdir -p "$tmp/ledger"
    want=$( (cd "$tmp/ledger" && find . -name '*.rs.md') | sort)
    have=$( (cd verus/ledger 2>/dev/null && find . -name '*.rs.md') | sort)
    stale=0
    [ "$want" = "$have" ] || stale=1
    for f in $want; do cmp -s "$tmp/ledger/$f" "verus/ledger/$f" || stale=1; done
    # An overrides file whose source file has no seams is left over.
    for o in $ovfiles; do
      md=${o#verus/ledger/}; md=${md%.overrides}.md
      [ -f "$tmp/ledger/$md" ] || { echo "seams: $o: its source file has no seams" >&2; stale=1; }
    done
    # A status cites a check that is gone (renamed or removed), or none.
    cites | while read -r o t c; do
      case $t in
        Kani) harness "$c" || echo "seams: $o: Kani: $c names no #[kani::proof] fn" ;;
        assert) assertion "$c" || echo "seams: $o: const assert: $c names no one labeled assertion" ;;
        Isla) isla "$o" "$c" || echo "seams: $o: Isla: $c: no such seam heading" ;;
        emptyasm) emptyasm "$o" "$c" || echo "seams: $o: emptyasm: ${c#* @ } is not this seam's fn and file, or names no one row of verus/emptyasm/blocks.txt" ;;
        *) echo "seams: $o: $c: the status cites no check" ;;
      esac
    done > "$tmp/cites"
    # Self-test: paths that name no harness must be rejected, so a change
    # that makes the resolver accept everything fails here.
    for c in kani_proofs::ptr::no_such_harness kani::ptr::ptr_add \
        heap::no_such_mod::no_such_harness; do
      if harness "$c"; then echo "seams: self-test: $c resolved, want rejected"; fi
    done >> "$tmp/cites"
    # The same for assertion labels: one no file carries, one two blocks
    # carry, and a real label cited in the wrong file. The backticks are
    # part of the labels.
    # shellcheck disable=SC2016
    for c in '`no_such_label` in src/arena.rs' '`axiom_array_layout` in src/arena.rs' \
        '`axiom_arena_layout` in src/lib.rs'; do
      if assertion "$c"; then echo "seams: self-test: $c resolved, want rejected"; fi
    done >> "$tmp/cites"
    # And for Isla: a seam the file does not list, a file that is gone,
    # and a seam it lists under another source file.
    for c in "no_such_seam in verus/isla/seams.md" "get_tp in verus/isla/no_such_file.md" \
        "syscall0#2 in verus/isla/seams.md"; do
      if isla verus/ledger/src/sys/tls.rs.overrides "$c"; then echo "seams: self-test: Isla: $c resolved, want rejected"; fi
    done >> "$tmp/cites"
    # And for emptyasm: a seam's own fn and file that no row names, a fn
    # other than the seam's, a listed fn cited from another source file's
    # seam, and a listed fn cited for another seam; a correct cite must
    # resolve.
    for c in 'shared_ptr @ shared_ptr in src/api/mod.rs' 'shared_base @ no_such_fn in src/api/mod.rs' \
        'shared_base @ word_index in src/class/table.rs' 'shared_ptr @ shared_base in src/api/mod.rs'; do
      if emptyasm verus/ledger/src/api/mod.rs.overrides "$c"; then echo "seams: self-test: emptyasm: $c resolved, want rejected"; fi
    done >> "$tmp/cites"
    emptyasm verus/ledger/src/api/mod.rs.overrides 'shared_base @ shared_base in src/api/mod.rs' ||
      echo "seams: self-test: emptyasm: shared_base @ shared_base in src/api/mod.rs rejected, want resolved" >> "$tmp/cites"
    # The item of each seam a status cites `emptyasm:` for must hold one
    # asm! call with the empty template (emptyitem), so a status stays
    # true when verus/emptyasm/run.sh does not run.
    cites | while read -r o t c; do
      [ "$t" = emptyasm ] || continue
      seam=${c%% @ *}; src=${o#verus/ledger/}; src=${src%.overrides}
      l=$(awk -v h="## \`$seam\`" -v s="$src" '
        $0 == h { on = 1; next }
        on && index($0, s ":") == 1 { l = $0; sub(/^[^:]*:/, "", l); sub(/\..*$/, "", l); print l; exit }
      ' "$tmp/ledger/$src.md" 2>/dev/null || true)
      if [ -z "$l" ]; then
        echo "seams: $o: emptyasm: $seam has no ledger entry"
      elif ! emptyitem "$src" "$l"; then
        echo "seams: $o: emptyasm: $seam ($src:$l) holds other than one asm! call with the empty template"
      fi
    done >> "$tmp/cites"
    # Self-test: an item with one empty template passes; one whose
    # template holds an instruction, one with two calls, one with none and
    # one whose operand is `out(reg)` (its register undefined after the
    # call) must be rejected.
    # shellcheck disable=SC2016
    printf '%s\n' 'fn ok() {' '    unsafe {' '        asm!("/* {0} */", inout(reg) x, options(nomem));' '    }' '}' \
      'fn nop() {' '    unsafe {' '        asm!("nop /* {0} */", inout(reg) x, options(nomem));' '    }' '}' \
      'fn two() {' '    unsafe {' '        asm!("/* {0} */", inout(reg) x, options(nomem));' \
      '        asm!("/* {0} */", inout(reg) y, options(nomem));' '    }' '}' \
      'fn none() {' '    let x = 1;' '}' \
      'fn out() {' '    unsafe {' '        asm!("/* {0} */", out(reg) x, options(nomem));' '    }' '}' > "$tmp/ea.rs"
    emptyitem "$tmp/ea.rs" 1 || echo "seams: self-test: emptyasm: ok rejected, want accepted" >> "$tmp/cites"
    for l in 6 11 17 20; do
      if emptyitem "$tmp/ea.rs" "$l"; then echo "seams: self-test: emptyasm: the item at line $l accepted, want rejected"; fi
    done >> "$tmp/cites"
    # The source lines Kani harnesses run copies of (verus/kani/pins.txt)
    # must be in place, so a change to an original or its copy fails here
    # until the copy, the list and the status citing the harness are
    # revisited; and a status that names verus/kani/pins.txt needs rows
    # there for its source file. Self-test: lines in place pass (with a
    # `...` row between two of them); a changed line, a skipped line,
    # lines a `...` row is missing between, a second copy of the lines
    # earlier in the file (in a block comment) and a row with four fields
    # fail, as does a pins file that is gone.
    pins verus/kani/pins.txt >> "$tmp/cites"
    for o in $ovfiles; do
      grep -q 'verus/kani/pins\.txt' "$o" || continue
      src=${o#verus/ledger/}; src=${src%.overrides}
      awk -F '\t' -v s="$src" '!/^#/ && (NF == 2 || NF == 3) && $1 == s { n++ } END { exit !n }' verus/kani/pins.txt 2>/dev/null ||
        echo "seams: $o: a status names verus/kani/pins.txt, which has no row for $src"
    done >> "$tmp/cites"
    printf '%s\n' 'fn f() {' '    // a comment' '' '    let a = 1;' '    a' '}' 'fn g() {}' > "$tmp/pin.rs"
    printf '%s\t%s\n' "$tmp/pin.rs" 'fn f() {' "$tmp/pin.rs" 'let a = 1;' "$tmp/pin.rs" 'a' \
      "$tmp/pin.rs" '...' "$tmp/pin.rs" 'fn g() {}' > "$tmp/pins-ok.txt"
    [ -z "$(pins "$tmp/pins-ok.txt")" ] || echo "seams: self-test: pins: lines in place rejected" >> "$tmp/cites"
    printf '%s\t%s\n' "$tmp/pin.rs" 'fn f() {' "$tmp/pin.rs" 'let a = 2;' > "$tmp/pins-changed.txt"
    printf '%s\t%s\n' "$tmp/pin.rs" 'fn f() {' "$tmp/pin.rs" 'a' > "$tmp/pins-skip.txt"
    printf '%s\t%s\n' "$tmp/pin.rs" 'a' "$tmp/pin.rs" 'fn g() {}' > "$tmp/pins-gap.txt"
    { printf '%s\n' '/*' 'fn f() {' '    let a = 1;' '    a' '}' '*/'; cat "$tmp/pin.rs"; } > "$tmp/pin-decoy.rs"
    awk -F '\t' -v OFS='\t' -v d="$tmp/pin-decoy.rs" '{ $1 = d; print }' "$tmp/pins-ok.txt" > "$tmp/pins-decoy.txt"
    printf '%s\t%s\t%s\t%s\n' "$tmp/pin.rs" 'fn f() {' 'w:orig' 'x' > "$tmp/pins-fields.txt"
    for p in "$tmp/pins-changed.txt" "$tmp/pins-skip.txt" "$tmp/pins-gap.txt" "$tmp/pins-decoy.txt" "$tmp/pins-fields.txt" "$tmp/no-such-pins.txt"; do
      [ -n "$(pins "$p")" ] || echo "seams: self-test: pins: $p accepted, want rejected"
    done >> "$tmp/cites"
    # A copy whose lines differ from its original's (a Kani copy without
    # the original's asm! block) must repeat the original's lines but for
    # the rows marked `-only` (copies). Self-test: a matching copy passes,
    # with its pins in place; a copy changed with its rows updated (pins
    # still passes), an unmarked row inside the original or inside the
    # copy, a row with an unknown role, and a NAME with no copy rows fail.
    copies verus/kani/pins.txt >> "$tmp/cites"
    printf '%s\n' 'fn o() {' '    let a = 1;' '    hint(a);' '    a' '}' 'fn c() {' '    let a = 1;' '    a' '}' > "$tmp/cp.rs"
    printf '%s\n' 'fn o() {' '    let a = 1;' '    hint(a);' '    a' '}' 'fn c() {' '    let a = 2;' '    a' '}' > "$tmp/cp-changed.rs"
    cprows() { # FILE COPY-FIRST-LINE [ORIG-HINT-TAG] [NAME]
      printf '%s\n' "$1${tab}fn o() {${tab}${4:-w}:orig-only" "$1${tab}let a = 1;${tab}${4:-w}:orig" \
        "$1${tab}hint(a);${3:+${tab}}${3:-}" "$1${tab}a${tab}${4:-w}:orig" "$1${tab}}${tab}${4:-w}:orig" "$1${tab}..." \
        "$1${tab}fn c() {${tab}w:copy-only" "$1${tab}$2${tab}w:copy" "$1${tab}a${tab}w:copy" "$1${tab}}${tab}w:copy"
    }
    cprows "$tmp/cp.rs" 'let a = 1;' w:orig-only > "$tmp/copies-ok.txt"
    [ -z "$(pins "$tmp/copies-ok.txt")$(copies "$tmp/copies-ok.txt")" ] || echo "seams: self-test: copies: a matching copy rejected" >> "$tmp/cites"
    cprows "$tmp/cp-changed.rs" 'let a = 2;' w:orig-only > "$tmp/copies-changed.txt"
    [ -z "$(pins "$tmp/copies-changed.txt")" ] || echo "seams: self-test: copies: the changed copy's pins rejected, want accepted" >> "$tmp/cites"
    cprows "$tmp/cp.rs" 'let a = 1;' > "$tmp/copies-unmarked.txt"
    cprows "$tmp/cp.rs" 'let a = 1;' w:body > "$tmp/copies-role.txt"
    cprows "$tmp/cp.rs" 'let a = 1;' v:orig-only v > "$tmp/copies-nocopy.txt"
    awk -F '\t' -v OFS='\t' '$3 == "w:copy" && $2 == "a" { print $1, $2; next } { print }' "$tmp/copies-ok.txt" > "$tmp/copies-copygap.txt"
    for p in "$tmp/copies-changed.txt" "$tmp/copies-unmarked.txt" "$tmp/copies-copygap.txt" "$tmp/copies-role.txt" "$tmp/copies-nocopy.txt"; do
      [ -n "$(copies "$p")" ] || echo "seams: self-test: copies: $p accepted, want rejected"
    done >> "$tmp/cites"
    # The asm! templates of the seams a status cites `Isla: FILE` for must
    # be the lines of the templates.txt next to FILE (islatemplates), so a
    # status stays true when no Isla tool runs.
    cites | while read -r o t c; do
      [ "$t" = Isla ] || continue
      name=${c% in *}; file=${c##* in }; src=${o#verus/ledger/}; src=${src%.overrides}
      r=$(awk -v h="## \`$name\`" -v nm="$name" -v f="$file" -v s="$src" '
        $0 == h { on = 1; next }
        on && index($0, s ":") == 1 {
          l = $0; sub(/^[^:]*:/, "", l); sub(/\..*$/, "", l)
          k = $0; sub(/^.*Kind: /, "", k); sub(/\. Category:.*$/, "", k)
          print f "\t" s "\t" l "\t" nm "\t" k; exit
        }' "$tmp/ledger/$src.md" 2>/dev/null || true)
      if [ -n "$r" ]; then echo "$r"; else echo "seams: $o: Isla: $name has no ledger entry" >> "$tmp/cites"; fi
    done > "$tmp/isla-items"
    cut -f1 "$tmp/isla-items" | sort -u | while read -r f; do
      if [ -f "$(dirname "$f")/templates.txt" ]; then
        grep "^$f$tab" "$tmp/isla-items" | cut -f2- > "$tmp/isla-items-f"
        islatemplates "$(dirname "$f")/templates.txt" "$tmp/isla-items-f"
      else
        echo "seams: Isla: $f: no templates.txt next to it"
      fi
    done >> "$tmp/cites"
    # Self-test: a seam's item whose asm! calls drift from the templates
    # file must be rejected for each way they can: a call the file does not
    # list (an added `hvc`), a line no call reads (a changed template), a
    # trap instruction in a string literal (in either case), an `asm!` with
    # braces, and an exec item with no call.
    # shellcheck disable=SC2016
    printf '%s\n' 'fn probe() {' '    unsafe {' '        asm!("svc #0", options(nostack));' \
      '        asm!(' '            "hvc #0",' '            options(nostack),' '        );' '    }' \
      '    let s = ".inst 0xd4000002";' '    let t = "SVC #1";' '    asm!{options(nomem)};' '}' 'fn empty() {' '    let x = 1;' '}' > "$tmp/st.rs"
    printf '%s\t%s\t%s\n' "$tmp/st.rs" probe 'asm!("svc #0", options(nostack));' \
      "$tmp/st.rs" probe 'asm!("svc #1", options(nostack));' > "$tmp/st-templates.txt"
    printf '%s\t%s\t%s\t%s\n' "$tmp/st.rs" 1 'probe#2' external_body "$tmp/st.rs" 13 empty external_body \
      > "$tmp/st-items"
    islatemplates "$tmp/st-templates.txt" "$tmp/st-items" > "$tmp/st-out"
    for w in 'probe#2: unchecked assembly' ': ".inst 0xd4000002"' ': "SVC #1"' ': asm! with another delimiter: asm!{options(nomem)};' \
        'empty: no asm! call to check' \
        'probe: an asm! call that '"$tmp"'/st-templates.txt does not list: asm!( "hvc #0", options(nostack), );' \
        'no asm! call of a seam in '"$tmp"'/st.rs probe reads: asm!("svc #1", options(nostack));'; do
      grep -qF -- "$w" "$tmp/st-out" || echo "seams: self-test: Isla templates: want a line with: $w"
    done >> "$tmp/cites"
    [ "$(wc -l < "$tmp/st-out")" -eq 6 ] ||
      { echo "seams: self-test: Isla templates: want 6 lines, got:"; cat "$tmp/st-out"; } >> "$tmp/cites"
    # A gated assertion (a one-line and a multi-line `cfg`, and one with a
    # blank line before the comment) must be
    # rejected too: a gated assertion is not evaluated in every build.
    # shellcheck disable=SC2016
    printf '#[cfg(target_arch = "riscv64")]\n// `gated`, checked.\nconst _: () = assert!(\n    true\n);\n' > "$tmp/g1.rs"
    # shellcheck disable=SC2016
    printf '#[cfg(any(\n    target_arch = "riscv64",\n))]\n// `gated`, checked.\nconst _: () = assert!(\n    true\n);\n' > "$tmp/g2.rs"
    # shellcheck disable=SC2016
    printf '#[cfg(target_arch = "riscv64")]\n\n// `gated`, checked.\nconst _: () = assert!(\n    true\n);\n' > "$tmp/g3.rs"
    for c in "\`gated\` in $tmp/g1.rs" "\`gated\` in $tmp/g2.rs" "\`gated\` in $tmp/g3.rs"; do
      if assertion "$c"; then echo "seams: self-test: $c resolved, want rejected"; fi
    done >> "$tmp/cites"
    bad=0
    if [ -s "$tmp/cites" ]; then cat "$tmp/cites" >&2; bad=1; fi
    if [ $stale = 0 ] && [ $bad = 0 ]; then
      echo "verus/ledger is up to date"
    else
      [ $stale = 0 ] || echo "verus/ledger is stale: run sh verus/seams.sh --write" >&2
      exit 1
    fi ;;
esac
