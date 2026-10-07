#!/bin/sh
# verus/seams.sh: the seam ledger, every trusted item the Verus proofs rest on.
#
#   sh verus/seams.sh            print the ledger (verus/SEAMS.md) on stdout
#   sh verus/seams.sh --write    regenerate verus/SEAMS.md
#   sh verus/seams.sh --check    exit 1 if verus/SEAMS.md is stale
#
# Scans src/ (test files, `#[cfg(test)]` items and the Kani harnesses'
# `#[cfg(kani)]` items skipped) for the items
# Verus takes on trust:
#   external_body        `#[verifier::external_body]` exec fn (body unchecked)
#   external_body proof  `#[verifier::external_body]` proof fn (an axiom)
#   external_body type   `#[verifier::external_body]` struct or enum
#   external_body const  `#[verifier::external_body]` exec const or static
#   axiom                `axiom fn`, `broadcast axiom fn`, `proof fn axiom_*`
#   assume_spec          `assume_specification[ path ]`
#   ext_type_spec        `#[verifier::external_type_specification]`
#   ext_trait_spec       `#[verifier::external_trait_specification]`
#   spec_impl            `#[cfg(verus_only)] impl vstd::std_specs::...`
#   abort_standin        `#[cfg(verus_only)]` `-> !` fn (verified or
#                        external_body) that Verus sees in place of an
#                        abort the build keeps (`cfg(not(verus_only))`)
#   no_decreases         `#[verifier::exec_allows_no_decreases_clause]`
#   assume_new           `Tracked::assume_new()`/`Ghost::assume_new()`
#                        outside tests and outside external_body bodies
#                        (those inside are noted on their function's entry)
# and prints, per item, its place, the contract Verus assumes (the
# signature with its requires/ensures, verbatim), a category and a
# proposed discharging tool. Categories and tools come from heuristics on
# the item's kind, path, name and body; verus/seams.overrides (one
# `name<TAB>category<TAB>tool[<TAB>status]` per line, `#` comments; the
# name as printed in the ledger, `file:name` for one of several items of
# that name, or `file:name#N` for the Nth seam of that name in the file, in
# ledger (source) order, where one file has several) overrides them and
# carries every status other than "trusted".
# The output depends only on the tree: files are read in byte order.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT" || exit 2
export LC_ALL=C

mode=print
case ${1:-} in
  '') ;;
  --write) mode=write ;;
  --check) mode=check ;;
  *) sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac

files=$(find src -name '*.rs' ! -name tests.rs ! -name linux_tests.rs \
  ! -name testutil.rs | sort)
[ -n "$files" ] || { echo "seams: no sources under src/" >&2; exit 2; }

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

# The entry's body is not checked by Verus: an external_body item, or an
# abort stand-in that is one.
function unchecked(e) {
    return ekind[e] ~ /^external_body/ || (ekind[e] == "abort_standin" && eattrs[e] ~ /verifier::external_body/)
}

# Start an entry at the current line.
function begin(kind, name,    ind) {
    n++
    efile[n] = FILE; eline[n] = LN; ekind[n] = kind; ename[n] = name
    eattrs[n] = attrs; edoc[n] = doc; ebody[n] = ""; enote[n] = ""
    econtract[n] = ""
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
    pendtest = 0; lastfn = ""; pendimpl = ""; sitecap = 0
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
        if (sitecap) {
            sitetext[nsite] = sitetext[nsite] " " c
            if (depth + opens - closes <= sited) sitecap = 0
        } else if (capvo && t != "" && t !~ /^#/) {
            nsite++; sitefile[nsite] = FILE; siteline[nsite] = LN
            sitetext[nsite] = " " c; sitefn[nsite] = ename[cur]
            if (opens > closes) { sitecap = 1; sited = depth }
        }
        if (t != "") capvo = t ~ /^#\[cfg\(verus_only\)\]$/
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
        if (t ~ /^#\[cfg\(kani\)\]/) pendtest = 1
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
    } else if (attrs ~ /cfg\(verus_only\)/ && t ~ /^impl .*vstd::std_specs::/) {
        kind = "spec_impl"; name = t; sub(/^impl /, "", name); sub(/ *\{$/, "", name)
        sub(/^vstd::std_specs::[a-z_]+::/, "", name); sub(/ for crate::/, " for ", name)
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
    } else if (isfn && attrs ~ /cfg\(verus_only\)/ && c ~ /-> *!/) {
        kind = "abort_standin"; name = qual(fname)
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

    if (kind == "" && attrs ~ /^ *#\[cfg\(verus_only\)\]$/ && !isfn && t !~ /^(pub(\([a-z]+\))? )?(use|mod|impl|const|static|struct|enum|type|trait) /) {
        nsite++; sitefile[nsite] = FILE; siteline[nsite] = LN
        sitetext[nsite] = " " c; sitefn[nsite] = lastfn
        if (opens > closes) { sitecap = 1; sited = depth }
    } else if (sitecap) {
        sitetext[nsite] = sitetext[nsite] " " c
        if (depth + opens - closes <= sited) sitecap = 0
    }
    if (kind == "" && c ~ /(Tracked|Ghost)::assume_new\(\)/) {
        e = begin(c ~ /Tracked::assume_new/ ? "assume_new" : "ghost_assume_new", lastfn)
        econtract[e] = rawt
    }

    if (kind != "") {
        cur = begin(kind, name); phase = 1; pd = 0; bd = 0; clauses = 0
        cap = !feed(raw, c)
    }
    attrs = ""; doc = ""

    if (t ~ /^(pub(\([a-z]+\))? )?(unsafe )?impl[< ]/) {
        if (opens > closes) { nimpl++; implname[nimpl] = impl_name(t); impld[nimpl] = depth }
        else pendimpl = impl_name(t)
    } else if (pendimpl != "" && opens > closes) {
        nimpl++; implname[nimpl] = pendimpl; impld[nimpl] = depth; pendimpl = ""
    }
    depth += opens - closes
    while (nimpl > 0 && depth <= impld[nimpl]) nimpl--
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
FILE == "verus/seams.overrides" {
    if ($0 ~ /^#/ || $0 ~ /^[ \t]*$/) next
    nf = split($0, f, "\t")
    if (nf < 3) { print "seams.overrides:" LN ": want name<TAB>category<TAB>tool" > "/dev/stderr"; bad = 1; next }
    ocat[f[1]] = f[2]; otool[f[1]] = f[3]; ostat[f[1]] = nf >= 4 ? f[4] : ""
    oseen[f[1]] = 0; nover++; okey[nover] = f[1]
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
    if (k == "abort_standin") return "panic/abort"
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
    ncat = split("inline asm|FFI/syscall|function pointer|UnsafeCell/volatile/transmute|raw pointer (internal)|raw pointer from C caller|panic/abort|atomics primitive|platform/kernel fact|C-program contract|termination|core library|allocator invariant", cats, "|")
    ntool = split("Islaris/Isla|RefinedRust|Kani|loom/GenMC|Verus|Miri (evidence only)|none (assumption)", tools, "|")
    nkind = split("external_body|external_body proof|external_body type|external_body const|axiom|assume_spec|ext_type_spec|ext_trait_spec|spec_impl|abort_standin|no_decreases|assume_new|ghost_assume_new", kinds, "|")
    for (i = 1; i <= ncat; i++) catok[cats[i]] = 1
    for (i = 1; i <= ntool; i++) toolok[tools[i]] = 1
    for (e = 1; e <= n; e++) {
        key = efile[e] ":" ename[e]
        keyn = key "#" (++nocc[key])
        if (keyn in ocat) ok = keyn; else if (key in ocat) ok = key; else if (ename[e] in ocat) ok = ename[e]; else ok = ""
        if (ok != "") {
            oseen[ok] = 1; ecat[e] = ocat[ok]; etool[e] = otool[ok]
            estat[e] = ostat[ok] == "" ? "trusted" : ostat[ok]
            if (etool[e] == "") etool[e] = tool_of(ecat[e])
        } else {
            ecat[e] = guess(e); etool[e] = tool_of(ecat[e]); estat[e] = "trusted"
        }
        if (!(ecat[e] in catok)) { print "seams: " efile[e] ":" eline[e] ": " ename[e] ": no category (add it to verus/seams.overrides)" > "/dev/stderr"; bad = 1 }
        else if (!(etool[e] in toolok)) { print "seams: " efile[e] ":" eline[e] ": " ename[e] ": unknown tool " etool[e] > "/dev/stderr"; bad = 1 }
        ncatn[ecat[e]]++; ntooln[etool[e]]++; nkindn[ekind[e]]++
        ct[ecat[e] SUBSEP etool[e]]++
    }
    for (i = 1; i <= nover; i++) if (!oseen[okey[i]]) { print "seams: verus/seams.overrides: " okey[i] " matches no seam" > "/dev/stderr"; bad = 1 }

    split("asm|ffi|fnptr|cell|ptr|c-ptr|abort|atomic|platform|c-contract|termination|core|invariant", scat, "|")
    split("Isla|RR|Kani|GenMC|Verus|Miri|none", stool, "|")
    split("body|proof|type|const|axiom|spec|ext-type|ext-trait|spec-impl|abort|no-dec|assume|assume-ghost", skind, "|")
    for (i = 1; i <= ncat; i++) cshort[cats[i]] = scat[i]
    for (j = 1; j <= ntool; j++) tshort[tools[j]] = stool[j]
    for (i = 1; i <= nkind; i++) kshort[kinds[i]] = skind[i]
    print "## Totals"
    print ""
    print n " seams. The short names are the index's."
    print ""
    print "| category | short | seams |"
    print "|---|---|---:|"
    for (i = 1; i <= ncat; i++) if (ncatn[cats[i]] > 0) print "| " cats[i] " | " scat[i] " | " ncatn[cats[i]] " |"
    print ""
    print "| tool | short | seams |"
    print "|---|---|---:|"
    for (j = 1; j <= ntool; j++) if (ntooln[tools[j]] > 0) print "| " tools[j] " | " stool[j] " | " ntooln[tools[j]] " |"
    print ""
    print "| kind | short | seams |"
    print "|---|---|---:|"
    for (i = 1; i <= nkind; i++) if (nkindn[kinds[i]] > 0) print "| " kinds[i] " | " skind[i] " | " nkindn[kinds[i]] " |"
    print ""
    print "Seams by category and tool:"
    print ""
    print "| category | tool | seams |"
    print "|---|---|---:|"
    for (i = 1; i <= ncat; i++) for (j = 1; j <= ntool; j++)
        if (ct[cats[i] SUBSEP tools[j]] > 0) print "| " cats[i] " | " tools[j] " | " ct[cats[i] SUBSEP tools[j]] " |"
    print ""
    print "## Index"
    print ""
    print "Per file, in source order, with the short names above. Item names"
    print "too long for a row are abbreviated (`...`); the entries below carry"
    print "the full name, the line and the contract."
    pf = ""
    for (e = 1; e <= n; e++) {
        if (efile[e] != pf) {
            pf = efile[e]
            print ""; print "### " pf; print ""
            print "| # | item | kind | category | tool | status |"
            print "|---|---|---|---|---|---|"
        }
        s = estat[e]; sub(/ .*$/, "", s)
        tail = " | " kshort[ekind[e]] " | " cshort[ecat[e]] " | " tshort[etool[e]] " | " s " |"
        head = "| S" e " | `"
        print head md(short(ename[e], 80 - length(head) - length(tail) - 1)) "`" tail
    }
    print ""
    print "## Entries"
    for (e = 1; e <= n; e++) {
        print ""
        print "### S" e ". `" ename[e] "`"
        print ""
        print efile[e] ":" eline[e] ". Kind: " ekind[e] ". Category: " ecat[e] "."
        print "Tool: " etool[e] ". Status: " estat[e] "."
        s = edoc[e]; sub(/\. .*$/, ".", s)
        if (s != "") { print ""; print "> " s }
        if (enote[e] != "") { s = enote[e]; sub(/; $/, ".", s); print ""; print "Body " s }
        if (ekind[e] == "abort_standin") {
            sites = ""
            s = ename[e]; sub(/^.*::/, "", s)
            for (i = 1; i <= nsite; i++) if (sitetext[i] ~ ("[^A-Za-z0-9_]" s "\\("))
                sites = sites (sites == "" ? "" : ", ") sitefile[i] ":" siteline[i] " (`" sitefn[i] "`)"
            if (sites != "") { print ""; print "Stands in at: " sites "." }
        }
        print ""
        print "```rust"
        print econtract[e]
        print "```"
    }
    if (bad) exit 1
}
EOF
)

header() {
  cat <<'EOF'
# Seams: what the Verus proofs take on trust

Generated by `sh verus/seams.sh --write` from `src/` and
`verus/seams.overrides`; do not edit it by hand. `sh verus/seams.sh
--check` fails when this file is stale.

Every theorem in [THEOREMS.md](THEOREMS.md) is conditional: Verus checks
the verified code against the contracts of the items it cannot see into
and assumes those contracts hold. A *seam* is one such item, the place
where a result from another tool (or a stated assumption) plugs in:

- an `external_body` function, proof function, type or constant (its body
  is not checked; its signature, `requires` and `ensures` are assumed);
- an `axiom fn` or `axiom_*` proof function (its `ensures` is assumed);
- an `assume_specification`, `external_type_specification` or
  `external_trait_specification` (a contract for code outside the crate);
- a `#[cfg(verus_only)]` `std_specs` impl (a ghost fact about a derived
  `PartialEq` or a `Display`);
- an abort stand-in: a `#[cfg(verus_only)]` `-> !` function Verus sees in
  place of an abort that the build keeps at the call site (under
  `#[cfg(not(verus_only))]`, unchecked), even when the stand-in's own
  body verifies;
- an `exec_allows_no_decreases_clause` function (termination assumed);
- a `Tracked::assume_new()` or `Ghost::assume_new()` outside tests and
  outside `external_body` bodies (an entry notes the ones inside).

Each entry gives the place, the kind, the contract Verus assumes (copied
verbatim from the source: the signature with its `requires` and
`ensures`), a category (why Verus cannot check it), a proposed tool and a
status. The categories are inline asm, FFI/syscall, function pointer,
UnsafeCell/volatile/transmute, raw pointer (internal), raw pointer from C
caller, panic/abort, atomics primitive, platform/kernel fact, C-program
contract and termination, plus two this ledger adds: core library (a
specification of a `core` function or type, checkable against `core`
itself) and allocator invariant (a ghost fact about challoc's own
protocol whose producer is unverified code). The tools are Islaris/Isla,
RefinedRust, Kani, loom/GenMC, Verus (the fact is provable in Verus
itself, once its producer states it or a lemma derives it), Miri
(evidence only) and none
(assumption). `seams.sh` picks a category from the kind, the path and the
body; `verus/seams.overrides` corrects it and sets the tool where the
category's default does not fit.

## Recording a non-Verus result

A seam leaves "trusted" only with a record: one file per seam under
`verus/seams/`, named after the item (`verus/seams/sys-raw-syscall3.md`;
the architecture after the module path when items of one name differ by
architecture, `verus/seams/sys-raw-aarch64-syscall3.md`), in five parts.

1. **Contract.** The Verus contract, copied from the entry below, at a
   named commit.
2. **Statement.** What the other tool proved, in its own terms (the Kani
   harness, the RefinedRust specification, the Islaris trace property,
   the GenMC test and memory model), with the tool's version and the
   command that reproduces it.
3. **Translation.** Why the statement implies the contract: each
   `requires` mapped to a precondition of the other tool, each `ensures`
   to a postcondition, and every gap named (the bound of a bounded check,
   a model's simplifications, an assumed memory model).
4. **Review.** Who checked the translation, and when.
5. **Tests.** The tests or CI jobs that keep the statement checked: a
   changed contract or body must fail one of them.

The status goes in the fourth column of `verus/seams.overrides`
(`name<TAB>category<TAB>tool<TAB>status`), as `discharged
(verus/seams/<item>.md)`, or `evidence (verus/seams/<item>.md)` for Miri
runs and other results that test rather than prove. A seam whose tool is
"none (assumption)" stays trusted; its record, if it has one, cites the
documentation (kernel, ISA, C standard) its contract follows.
EOF
}

# Busybox awk reads files given by name very slowly from some file
# systems; the files go through one pipe instead, each behind a marker line.
stream() {
  for f in verus/seams.overrides $files; do
    printf '\001FILE %s\n' "$f"
    cat "$f" || printf '\001FAIL %s\n' "$f"
  done
}

gen() {
  out=$(stream | awk "$marker
$overrides
$prog
$render") || return 1
  header
  echo
  printf '%s\n' "$out"
}

case $mode in
  print) gen ;;
  write) out=$(gen) && printf '%s\n' "$out" > verus/SEAMS.md ;;
  check)
    out=$(gen) || exit 1
    if printf '%s\n' "$out" | cmp -s - verus/SEAMS.md; then
      echo "verus/SEAMS.md is up to date"
    else
      echo "verus/SEAMS.md is stale: run sh verus/seams.sh --write" >&2
      exit 1
    fi ;;
esac
