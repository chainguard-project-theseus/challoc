//! perf-budget: check the fast-path cost bounds of the hot exports
//! (verus/THEOREMS.md theorem 45) on an x86_64 libchalloc.so. Std only;
//! built by scripts/perf-budget.sh with `rustc -O`.
//!
//!   perf-budget LIB TABLE            check LIB against TABLE
//!   perf-budget --print LIB TABLE    print the measured `paths`/`budget` lines
//!   perf-budget --explain LIB TABLE  also print every fast path
//!
//! The tool runs `objdump` (GNU binutils; `$PERF_BUDGET_OBJDUMP` overrides
//! it, which scripts/perf-budget.sh uses for its self-test) on LIB: `-T`
//! for the exports' addresses and sizes, `-t` for the static objects,
//! `-d -w -M intel` for the code, with the raw bytes, which give each
//! instruction's length: a path falls through to address + length, so an
//! instruction the parser skipped is a gap the walk fails on. For every
//! `export` line of TABLE it scans the export's whole body (address to
//! address + size) and fails on a `lock` prefix, `xchg`, `xadd`,
//! `cmpxchg*`, a fence, `call`, `syscall`/`sysenter`/`int N`, an
//! indirect jump or an unparsed line (P2). Then for every `budget` line it
//! enumerates every path from the entry: a path follows direct jumps and
//! both arms of every conditional branch, and ends at a `ret` (a fast
//! path, counted), at an edge to one of the export's named exit functions
//! (a slow or medium path, not counted), or at `int3`/`ud2` (a trap, not
//! counted). Any other edge out of the body, an instruction the classifier
//! does not know, or a back-edge (P3: the fast-path graph is acyclic) is
//! an error. For each fast path it counts instructions, explicit memory
//! loads, explicit memory stores, conditional branches and 64-byte cache
//! lines (P1, P4), and compares the maxima with the `budget` line and the
//! sorted list of every path's counts with the `paths` line: any
//! difference fails, so every change to the hot exports' machine code that
//! moves a count on any fast path has to update the table.
//!
//! Counting rules. An instruction with a memory operand is a load, a
//! store, or both (a read-modify-write destination), by mnemonic; `lea`
//! and `nop` access nothing. The `ret`'s implicit read of the return
//! address is part of the `ret` instruction, not a load (its line is the
//! caller's stack line the `call` wrote). A vector load is one load.
//!
//! Configurations. A `cell` names a byte (or wider) of a static object;
//! a budget's configuration fixes cell values. Every load of a cell then
//! yields the constant, and the tool tracks constants through register
//! moves, `movzx`, `test`, `cmp` and the flags, so a conditional branch
//! whose flags are known follows one arm only. Every other value is a
//! symbolic atom; addresses are linear combinations of atoms (`lea`,
//! 64-bit `add`/`sub`/`inc`/`dec`, register copies), so accesses through
//! the same base expression fall in one group. A `rip`-relative address
//! is the library's load base (an atom) plus its link-time address, so it
//! is never a flag constant; only an address of exactly that form is a
//! static, or a cell.
//!
//! Cache lines. A static access touches the lines of its link-time address
//! range, exactly: the library is loaded page-aligned. The accesses of one
//! group touch, at most, the largest number of distinct lines over every
//! residue of the group's base modulo 64 that the premises allow: the base
//! is a multiple of the alignment of its atoms (the load base of 4096; an
//! atom from `shl r, k` of `2^k`; from `and r, m` of `m`'s lowest set
//! bit; from a load named by an `align-load` line of that line's
//! alignment, else 1), and every access is naturally aligned to its size
//! up to 8 bytes (Rust's alignment rule for the u8..u64 and pointer fields
//! challoc reads; a vector load reads whole u64 entries). An indexed load
//! from a static is attributed to the static its constant part lies in
//! (the tables are indexed from their start). Groups are summed, so two
//! groups on one line count twice: an upper bound.
//!
//! TABLE lines (`#` comments):
//!
//!   align-load SRC N        values loaded through SRC are multiples of N
//!                           (SRC: `fs`, the one load through the fs
//!                           segment a path may make, or a static object's
//!                           demangled path)
//!   cell NAME OBJ OFF SIZE  NAME is SIZE bytes at OBJ + OFF
//!   export SYM EXIT...      SYM (a dynamic symbol) and the functions
//!                           (demangled paths) whose entry ends a path
//!   budget SYM CONFIG INSNS LOADS STORES BRANCHES LINES
//!                           CONFIG is `-` (no cell fixed, every path) or
//!                           NAME=VALUE[,NAME=VALUE...]
//!   paths SYM CONFIG I/L/S/B/N...
//!                           every fast path's five counts, sorted
use std::collections::{BTreeMap, BTreeSet};
use std::process::{exit, Command};

fn die(msg: &str) -> ! {
    eprintln!("perf-budget: {msg}");
    exit(2)
}

fn objdump(args: &[&str], lib: &str) -> String {
    let prog = std::env::var("PERF_BUDGET_OBJDUMP").unwrap_or_else(|_| "objdump".into());
    let out = Command::new(&prog)
        .args(args)
        .arg(lib)
        .output()
        .unwrap_or_else(|e| die(&format!("cannot run objdump: {e}")));
    if !out.status.success() {
        die(&format!(
            "objdump {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn parse_hex(s: &str) -> Option<u64> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).ok()
}

fn parse_num(s: &str) -> Option<u64> {
    if let Some(h) = s.strip_prefix("0x") {
        u64::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Rust symbol demangling: v0 (`_R`, see `demangle_v0`) or the legacy
/// Itanium-style form (`_ZN` (len name)* `E`), the hash
/// component dropped; anything else is returned unchanged.
fn demangle(s: &str) -> String {
    if let Some(v0) = s.strip_prefix("_R") {
        return demangle_v0(v0)
            .map(|(p, _)| p)
            .unwrap_or_else(|| s.to_string());
    }
    let Some(mut rest) = s.strip_prefix("_ZN") else {
        return s.to_string();
    };
    let mut parts = Vec::new();
    while !rest.starts_with('E') {
        let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(len) = n.parse::<usize>() else {
            return s.to_string();
        };
        rest = &rest[n.len()..];
        if rest.len() < len {
            return s.to_string();
        }
        parts.push(&rest[..len]);
        rest = &rest[len..];
    }
    if let Some(last) = parts.last() {
        if last.len() == 17 && last.starts_with('h') {
            parts.pop();
        }
    }
    parts.join("::")
}

/// Rust v0 mangling (rustc 1.98.1 emits it for this crate; 1.95.0 the
/// legacy form above), nested paths and crate roots only (`N` namespace
/// path ident, `C` [disambiguator] ident); returns the path and the rest.
/// Anything else (impls, generics, backrefs) is None, and the caller
/// keeps the raw symbol.
fn demangle_v0(s: &str) -> Option<(String, &str)> {
    fn ident(s: &str) -> Option<(&str, &str)> {
        let s = s.strip_prefix('u').map_or(s, |r| r); // punycode: kept raw
        let n: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
        let len: usize = n.parse().ok()?;
        let mut rest = &s[n.len()..];
        if rest.starts_with('_') {
            rest = &rest[1..];
        }
        (rest.len() >= len).then(|| (&rest[..len], &rest[len..]))
    }
    fn disamb(s: &str) -> &str {
        match s.strip_prefix('s') {
            Some(r) => r.find('_').map_or(r, |i| &r[i + 1..]),
            None => s,
        }
    }
    if let Some(r) = s.strip_prefix('C') {
        let (id, rest) = ident(disamb(r))?;
        return Some((id.to_string(), rest));
    }
    let r = s.strip_prefix('N')?;
    let ns = r.chars().next()?;
    let (inner, rest) = demangle_v0(&r[ns.len_utf8()..])?;
    let (id, rest) = ident(disamb(rest))?;
    Some((format!("{inner}::{id}"), rest))
}

// ---------------------------------------------------------------- operands

#[derive(Clone, Copy, Debug, PartialEq)]
struct Reg {
    id: usize, // 0..16 general, 16..48 vector, RIP
    bits: u32,
    high8: bool,
}
const RIP: usize = 48;
const NREGS: usize = 49;

fn reg(name: &str) -> Option<Reg> {
    const R64: [&str; 16] = [
        "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15",
    ];
    const R32: [&str; 8] = ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"];
    const R16: [&str; 8] = ["ax", "cx", "dx", "bx", "sp", "bp", "si", "di"];
    const R8: [&str; 8] = ["al", "cl", "dl", "bl", "spl", "bpl", "sil", "dil"];
    const H8: [&str; 4] = ["ah", "ch", "dh", "bh"];
    let mk = |id, bits| {
        Some(Reg {
            id,
            bits,
            high8: false,
        })
    };
    if name == "rip" {
        return mk(RIP, 64);
    }
    if let Some(i) = R64.iter().position(|r| *r == name) {
        return mk(i, 64);
    }
    if let Some(i) = R32.iter().position(|r| *r == name) {
        return mk(i, 32);
    }
    if let Some(i) = R16.iter().position(|r| *r == name) {
        return mk(i, 16);
    }
    if let Some(i) = R8.iter().position(|r| *r == name) {
        return mk(i, 8);
    }
    if let Some(i) = H8.iter().position(|r| *r == name) {
        return Some(Reg {
            id: i,
            bits: 8,
            high8: true,
        });
    }
    if let Some(n) = name.strip_prefix('r') {
        let (num, suf) = n.split_at(n.find(|c: char| !c.is_ascii_digit()).unwrap_or(n.len()));
        if let Ok(i) = num.parse::<usize>() {
            if (8..16).contains(&i) {
                return match suf {
                    "d" => mk(i, 32),
                    "w" => mk(i, 16),
                    "b" => mk(i, 8),
                    _ => None,
                };
            }
        }
    }
    for (p, bits) in [("xmm", 128), ("ymm", 256), ("zmm", 512)] {
        if let Some(n) = name.strip_prefix(p) {
            if let Ok(i) = n.parse::<usize>() {
                if i < 32 {
                    return mk(16 + i, bits);
                }
            }
        }
    }
    None
}

#[derive(Clone, Debug)]
struct Mem {
    seg: Option<String>,
    base: Option<Reg>,
    index: Option<(Reg, i64)>,
    disp: i64,
    /// `rip`-relative: the absolute address objdump's comment gives.
    abs: Option<u64>,
    size: u32,
}

#[derive(Clone, Debug)]
enum Op {
    Reg(Reg),
    Imm(u64),
    Mem(Mem),
    Target(u64),
}

#[derive(Clone, Debug)]
struct Insn {
    addr: u64,
    /// Length in bytes (the raw encoding objdump prints).
    len: u64,
    prefixes: Vec<String>,
    mnem: String,
    ops: Vec<Op>,
    text: String,
}

fn parse_mem(s: &str, comment_abs: Option<u64>) -> Result<Mem, String> {
    let open = s.find('[').ok_or("no [")?;
    let close = s.rfind(']').ok_or("no ]")?;
    // `QWORD PTR fs:[...]`: the size words, then an optional segment.
    let mut words: Vec<&str> = s[..open].split_whitespace().collect();
    let mut seg = None;
    if let Some(w) = words.last() {
        if let Some(sg) = w.strip_suffix(':') {
            seg = Some(sg.to_string());
            words.pop();
        }
    }
    let size = match words.join(" ").trim_end_matches("PTR").trim() {
        "" => 0,
        "BYTE" => 1,
        "WORD" => 2,
        "DWORD" => 4,
        "FWORD" => 6,
        "QWORD" => 8,
        "TBYTE" => 10,
        "XMMWORD" => 16,
        "YMMWORD" => 32,
        "ZMMWORD" => 64,
        h => return Err(format!("unknown operand size {h:?}")),
    };
    let inner = &s[open + 1..close];
    let mut m = Mem {
        seg,
        base: None,
        index: None,
        disp: 0,
        abs: None,
        size,
    };
    let mut tok = String::new();
    let mut sign = 1i64;
    let mut terms = Vec::new();
    for ch in inner.chars().chain(std::iter::once('+')) {
        if ch == '+' || ch == '-' {
            if !tok.is_empty() {
                terms.push((sign, std::mem::take(&mut tok)));
            }
            sign = if ch == '-' { -1 } else { 1 };
        } else {
            tok.push(ch);
        }
    }
    for (sign, t) in terms {
        if let Some((r, sc)) = t.split_once('*') {
            let r = reg(r).ok_or(format!("bad index {r}"))?;
            let sc = parse_num(sc).ok_or("bad scale")? as i64;
            m.index = Some((r, sc));
        } else if let Some(r) = reg(&t) {
            if m.base.is_some() {
                m.index = Some((r, 1));
            } else {
                m.base = Some(r);
            }
        } else {
            let v = parse_num(&t).ok_or(format!("bad term {t}"))? as i64;
            m.disp = m.disp.wrapping_add(sign.wrapping_mul(v));
        }
    }
    if m.base.map(|b| b.id) == Some(RIP) {
        m.abs = Some(comment_abs.ok_or("rip-relative operand without its address comment")?);
    }
    for r in m.base.iter().chain(m.index.iter().map(|(r, _)| r)) {
        if r.bits != 64 {
            return Err("32-bit address".into());
        }
    }
    Ok(m)
}

fn parse_insn(line: &str) -> Option<Result<Insn, String>> {
    // `ADDR:\tRAW BYTES \tTEXT` (`objdump -d -w`).
    let (a, rest) = line.split_once(":\t")?;
    let addr = parse_hex(a.trim())?;
    let (raw, text) = rest.split_once('\t').unwrap_or((rest, ""));
    let len = raw.split_whitespace().count() as u64;
    if len == 0
        || raw
            .split_whitespace()
            .any(|b| b.len() != 2 || parse_hex(b).is_none())
    {
        return Some(Err(format!("no raw encoding in `{}`", line.trim())));
    }
    let text = text.trim().to_string();
    let (body, comment) = match text.split_once(" # ") {
        Some((b, c)) => (b.trim(), Some(c.trim())),
        None => (text.as_str(), None),
    };
    let comment_abs = comment.and_then(|c| parse_hex(c.split(' ').next().unwrap_or("")));
    Some((|| {
        let mut words = body.split_whitespace().peekable();
        let mut prefixes = Vec::new();
        const PREFIXES: [&str; 18] = [
            "lock", "rep", "repz", "repe", "repnz", "repne", "bnd", "notrack", "data16", "addr32",
            "cs", "ds", "es", "ss", "fs", "gs", "rex", "xacquire",
        ];
        while let Some(w) = words.peek() {
            if PREFIXES.contains(w) || *w == "xrelease" || w.starts_with("rex.") {
                prefixes.push(w.to_string());
                words.next();
            } else {
                break;
            }
        }
        let mnem = words.next().unwrap_or("").to_string();
        let opstr: String = words.collect::<Vec<_>>().join(" ");
        let mut ops = Vec::new();
        if !opstr.is_empty() {
            let is_branch = mnem.starts_with('j') || mnem == "call";
            for o in opstr.split(',') {
                let o = o.trim();
                if o.contains('[') {
                    ops.push(Op::Mem(parse_mem(o, comment_abs)?));
                } else if let Some(r) = reg(o) {
                    ops.push(Op::Reg(r));
                } else if is_branch {
                    let t = o.split(' ').next().unwrap_or("");
                    ops.push(Op::Target(parse_hex(t).ok_or(format!("bad target {o}"))?));
                } else if let Some(v) = parse_num(o) {
                    ops.push(Op::Imm(v));
                } else if o.starts_with("st") || o.starts_with('k') || o.starts_with('{') {
                    return Err(format!("unsupported operand {o}"));
                } else {
                    return Err(format!("unparsed operand {o}"));
                }
            }
        }
        Ok(Insn {
            addr,
            len,
            prefixes,
            mnem,
            ops,
            text: text.clone(),
        })
    })())
}

// -------------------------------------------------------- classification

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Plain, // no control transfer
    Jcc,   // conditional branch
    Jmp,   // direct jump
    Ret,
    Trap, // int3, ud2
}

/// How an instruction touches a memory operand in position `pos`
/// (0 = first, the destination in Intel syntax): (load, store).
fn mem_effect(mnem: &str, pos: usize) -> Option<(bool, bool)> {
    const NOMEM: [&str; 2] = ["lea", "nop"];
    // Reads every operand, writes none (or writes only registers).
    const READ: [&str; 5] = ["cmp", "test", "bt", "ucomiss", "ucomisd"];
    // A memory destination is written without being read.
    const STORE: [&str; 13] = [
        "mov", "movq", "movd", "movdqu", "movdqa", "movups", "movaps", "movupd", "movapd",
        "movnti", "movss", "movsd", "movhps",
    ];
    // A memory destination is read and written.
    const RMW: [&str; 21] = [
        "add", "sub", "and", "or", "xor", "adc", "sbb", "inc", "dec", "neg", "not", "shl", "shr",
        "sar", "rol", "ror", "bts", "btr", "btc", "xadd", "cmpxchg",
    ];
    if NOMEM.contains(&mnem) {
        return Some((false, false));
    }
    if READ.contains(&mnem) || pos > 0 {
        return Some((true, false));
    }
    if STORE.contains(&mnem) || mnem.starts_with("set") {
        return Some((false, true));
    }
    if RMW.contains(&mnem) || mnem == "xchg" {
        return Some((true, true));
    }
    None
}

/// Mnemonics the value tracker knows: (sets the flags, writes its first
/// operand). Everything else is an error, so a new instruction in a hot
/// export is classified by hand before it is counted.
fn known(mnem: &str) -> Option<(bool, bool)> {
    const FLAGS_WRITE: [&str; 25] = [
        "add", "sub", "and", "or", "xor", "adc", "sbb", "inc", "dec", "neg", "shl", "shr", "sar",
        "rol", "ror", "imul", "popcnt", "tzcnt", "lzcnt", "bsf", "bsr", "andn", "blsr", "blsi",
        "bzhi",
    ];
    const FLAGS_ONLY: [&str; 5] = ["cmp", "test", "bt", "ucomiss", "ucomisd"];
    const WRITE_ONLY: [&str; 41] = [
        "mov",
        "movzx",
        "movsx",
        "movsxd",
        "lea",
        "movq",
        "movd",
        "movdqu",
        "movdqa",
        "movups",
        "movaps",
        "movupd",
        "movapd",
        "movss",
        "movsd",
        "movhps",
        "movmskps",
        "movmskpd",
        "pmovmskb",
        "pshufd",
        "pcmpeqd",
        "pcmpeqq",
        "pcmpeqb",
        "pcmpeqw",
        "shufps",
        "andps",
        "andnps",
        "orps",
        "xorps",
        "pand",
        "pandn",
        "por",
        "pxor",
        "punpcklqdq",
        "punpckhqdq",
        "not",
        "bswap",
        "shrx",
        "shlx",
        "sarx",
        "rorx",
    ];
    const NONE: [&str; 2] = ["nop", "endbr64"];
    if FLAGS_WRITE.contains(&mnem) {
        Some((true, true))
    } else if FLAGS_ONLY.contains(&mnem) {
        Some((true, false))
    } else if WRITE_ONLY.contains(&mnem) || mnem.starts_with("cmov") || mnem.starts_with("set") {
        Some((false, true))
    } else if NONE.contains(&mnem) {
        Some((false, false))
    } else {
        None
    }
}

fn kind(i: &Insn) -> Kind {
    match i.mnem.as_str() {
        "jmp" => Kind::Jmp,
        "ret" => Kind::Ret,
        "int3" | "ud2" => Kind::Trap,
        m if m.starts_with('j') => Kind::Jcc,
        _ => Kind::Plain,
    }
}

/// P2: what no hot export may contain anywhere in its body.
fn forbidden(i: &Insn) -> Option<&'static str> {
    if i.prefixes.iter().any(|p| p == "lock") || i.text.split_whitespace().any(|w| w == "lock") {
        return Some("lock prefix");
    }
    match i.mnem.as_str() {
        "xchg" => Some("xchg"),
        "xadd" => Some("xadd"),
        m if m.starts_with("cmpxchg") => Some("cmpxchg"),
        "mfence" | "lfence" | "sfence" => Some("fence"),
        "call" => Some("call"),
        "syscall" | "sysenter" | "int" | "int1" | "into" => Some("system call or interrupt"),
        "jmp" if !matches!(i.ops.first(), Some(Op::Target(_))) => Some("indirect jump"),
        m if m.starts_with('j') && !matches!(i.ops.first(), Some(Op::Target(_))) => {
            Some("indirect jump")
        }
        _ => None,
    }
}

// ------------------------------------------------------------ the tracker

/// A linear combination of atoms plus a constant.
#[derive(Clone, Debug, PartialEq)]
struct Val {
    terms: Vec<(usize, i64)>,
    c: i64,
}

impl Val {
    fn konst(c: u64) -> Val {
        Val {
            terms: vec![],
            c: c as i64,
        }
    }
    fn atom(a: usize) -> Val {
        Val {
            terms: vec![(a, 1)],
            c: 0,
        }
    }
    fn as_const(&self) -> Option<u64> {
        self.terms.is_empty().then_some(self.c as u64)
    }
    fn add(&self, o: &Val, k: i64) -> Val {
        let mut m: BTreeMap<usize, i64> = self.terms.iter().copied().collect();
        for &(a, c) in &o.terms {
            *m.entry(a).or_insert(0) += c * k;
        }
        Val {
            terms: m.into_iter().filter(|&(_, c)| c != 0).collect(),
            c: self.c.wrapping_add(o.c.wrapping_mul(k)),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Flags {
    zf: bool,
    cf: bool,
    sf: bool,
    of: bool,
}

#[derive(Clone)]
struct Atom {
    align: u64,
}

#[derive(Clone, Debug)]
struct Access {
    addr: Val,
    size: u32,
    load: bool,
    store: bool,
    at: u64,
}

#[derive(Clone)]
struct State {
    regs: Vec<Val>,
    flags: Option<Flags>,
    atoms: Vec<Atom>,
    insns: u32,
    branches: u32,
    accesses: Vec<Access>,
    trail: Vec<u64>,
    cells_read: BTreeSet<String>,
    fs_loads: u32,
}

/// The fs segment base: TLS accesses add it to their address.
const FS_ATOM: usize = 0;
/// The library's load base: a `rip`-relative address is this atom plus
/// the link-time address, so it never folds into a flag constant, and only
/// an address with this term (once) is looked up as a static or a cell.
const IMAGE_ATOM: usize = 1;

impl Val {
    /// The link-time address of a static access: `IMAGE + c`, no other term.
    fn static_addr(&self) -> Option<u64> {
        (self.terms == [(IMAGE_ATOM, 1)]).then_some(self.c as u64)
    }
}

fn mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

struct Ctx<'a> {
    by_addr: &'a BTreeMap<u64, Insn>,
    statics: &'a [(u64, u64, String)],
    align_load: &'a BTreeMap<String, u64>,
    cells: BTreeMap<u64, (String, u32, u64)>, // address -> (name, size, value)
    lo: u64,
    hi: u64,
    exits: &'a [String],
    fnames: &'a BTreeMap<u64, String>,
}

#[derive(Default)]
struct Walk {
    fast_paths: u32,
    exit_edges: u32,
    traps: u32,
    max: [u32; 5], // insns loads stores branches lines
    witness: [Vec<u64>; 5],
    paths: Vec<(Vec<u64>, [u32; 5], Vec<String>)>,
    cells_read: BTreeSet<String>,
}

impl State {
    fn fresh(&mut self, align: u64) -> Val {
        self.atoms.push(Atom {
            align: align.max(1),
        });
        Val::atom(self.atoms.len() - 1)
    }
}

fn static_of<'a>(statics: &'a [(u64, u64, String)], a: u64) -> Option<&'a str> {
    statics
        .iter()
        .find(|(s, n, _)| *s <= a && a < s + (*n).max(1))
        .map(|(_, _, name)| name.as_str())
}

impl<'a> Ctx<'a> {
    fn addr_of(&self, st: &State, m: &Mem) -> Val {
        let mut v = Val::konst(m.disp as u64);
        if let Some(a) = m.abs {
            v = Val::konst(a).add(&Val::atom(IMAGE_ATOM), 1);
        } else if let Some(b) = m.base {
            v = v.add(&st.regs[b.id], 1);
        }
        if let Some((r, s)) = m.index {
            v = v.add(&st.regs[r.id], s);
        }
        if m.seg.as_deref() == Some("fs") || m.seg.as_deref() == Some("gs") {
            v = v.add(&Val::atom(FS_ATOM), 1);
        }
        v
    }

    /// The value a load of `size` bytes at `addr` produces.
    fn load_value(&self, st: &mut State, addr: &Val, size: u32) -> Val {
        if let Some(a) = addr.static_addr() {
            if let Some((name, csize, v)) = self.cells.get(&a) {
                if *csize != size {
                    die(&format!(
                        "cell {name} read with size {size}, table says {csize}"
                    ));
                }
                st.cells_read.insert(name.clone());
                return Val::konst(*v);
            }
        }
        // At most one fs access per path (`step` checks).
        let src = if addr.terms.iter().any(|&(a, _)| a == FS_ATOM) {
            Some("fs".to_string())
        } else if addr.terms.contains(&(IMAGE_ATOM, 1)) {
            // A table indexed from its start: the constant part is in it.
            static_of(self.statics, addr.c as u64).map(demangle)
        } else {
            None
        };
        let align = src
            .and_then(|s| self.align_load.get(&s).copied())
            .unwrap_or(1);
        st.fresh(align)
    }

    fn read(&self, st: &mut State, op: &Op, bits: u32) -> Val {
        match op {
            Op::Imm(v) => Val::konst(v & mask(bits)),
            Op::Reg(r) => {
                let v = &st.regs[r.id];
                if r.high8 {
                    return match v.as_const() {
                        Some(c) => Val::konst((c >> 8) & 0xff),
                        None => st.fresh(1),
                    };
                }
                if r.bits >= 64 && r.id < 16 {
                    return v.clone();
                }
                match v.as_const() {
                    Some(c) => Val::konst(c & mask(r.bits.min(bits))),
                    None => st.fresh(1),
                }
            }
            Op::Mem(m) => {
                let a = self.addr_of(st, m);
                self.load_value(st, &a, m.size)
            }
            Op::Target(_) => die("target as value"),
        }
    }

    fn write(&self, st: &mut State, r: Reg, v: Val) {
        if r.id >= 16 {
            st.regs[r.id] = st.fresh(1);
            return;
        }
        if r.bits == 64 {
            st.regs[r.id] = v;
        } else if r.bits == 32 {
            // Truncation keeps the low bits, so a lone atom's alignment.
            st.regs[r.id] = match (v.as_const(), v.terms.as_slice()) {
                (Some(c), _) => Val::konst(c & 0xffff_ffff),
                (None, [(a, 1)]) if v.c == 0 => {
                    let al = st.atoms[*a].align;
                    st.fresh(al)
                }
                _ => st.fresh(1),
            };
        } else {
            let old = st.regs[r.id].as_const();
            st.regs[r.id] = match (old, v.as_const()) {
                (Some(o), Some(c)) if r.high8 => Val::konst((o & !0xff00) | ((c & 0xff) << 8)),
                (Some(o), Some(c)) => Val::konst((o & !mask(r.bits)) | (c & mask(r.bits))),
                _ => st.fresh(1),
            };
        }
    }

    fn op_bits(op: &Op) -> u32 {
        match op {
            Op::Reg(r) => r.bits,
            Op::Mem(m) => m.size * 8,
            _ => 64,
        }
    }

    /// Execute one non-control instruction on `st`.
    fn step(&self, st: &mut State, i: &Insn) -> Result<(), String> {
        let (sets_flags, writes) =
            known(&i.mnem).ok_or_else(|| format!("unclassified instruction `{}`", i.text))?;
        for (pos, op) in i.ops.iter().enumerate() {
            if let Op::Mem(m) = op {
                let (load, store) = mem_effect(&i.mnem, pos)
                    .ok_or_else(|| format!("unclassified memory operand in `{}`", i.text))?;
                if load || store {
                    if m.size == 0 {
                        return Err(format!("memory operand without a size in `{}`", i.text));
                    }
                    let addr = self.addr_of(st, m);
                    // `align-load fs` is the TLS heap slot's premise: one
                    // access through fs per path (tracked or not), or the
                    // premise could cover a second thread-local.
                    if addr.terms.iter().any(|&(a, _)| a == FS_ATOM) {
                        st.fs_loads += 1;
                        if st.fs_loads > 1 {
                            return Err(format!(
                                "{:x}: a second access through fs on one path; \
                                 `align-load fs` names the heap slot only",
                                i.addr
                            ));
                        }
                    }
                    st.accesses.push(Access {
                        addr,
                        size: m.size,
                        load,
                        store,
                        at: i.addr,
                    });
                }
            }
        }
        let bits = i.ops.first().map(Self::op_bits).unwrap_or(64);
        let m = i.mnem.as_str();
        let mut flags = None;
        let mut result: Option<Val> = None;
        match (m, i.ops.as_slice()) {
            ("cmp", [a, b]) | ("test", [a, b]) => {
                let x = self.read(st, a, bits).as_const();
                let y = self.read(st, b, bits).as_const();
                if let (Some(x), Some(y)) = (x, y) {
                    let (x, y) = (x & mask(bits), y & mask(bits));
                    let top = |v: u64| (v >> (bits - 1)) & 1 == 1;
                    flags = Some(if m == "cmp" {
                        let r = x.wrapping_sub(y) & mask(bits);
                        Flags {
                            zf: r == 0,
                            cf: x < y,
                            sf: top(r),
                            of: top((x ^ y) & (x ^ r)),
                        }
                    } else {
                        let r = x & y;
                        Flags {
                            zf: r == 0,
                            cf: false,
                            sf: top(r),
                            of: false,
                        }
                    });
                }
            }
            ("mov", [_, s]) | ("movzx", [_, s]) => {
                let sb = Self::op_bits(s);
                result = Some(self.read(st, s, sb));
            }
            ("lea", [_, Op::Mem(mm)]) => result = Some(self.addr_of(st, mm)),
            ("add" | "sub", [Op::Reg(d), s]) if d.bits == 64 => {
                let x = st.regs[d.id].clone();
                let y = self.read(st, s, 64);
                result = Some(x.add(&y, if m == "add" { 1 } else { -1 }));
            }
            ("inc" | "dec", [Op::Reg(d)]) if d.bits == 64 => {
                let k = if m == "inc" { 1 } else { -1 };
                result = Some(st.regs[d.id].add(&Val::konst(1), k));
            }
            ("shl", [Op::Reg(d), Op::Imm(k)]) => {
                let x = self.read(st, &Op::Reg(*d), d.bits);
                result = Some(match x.as_const() {
                    Some(c) => Val::konst(c << (k & 63)),
                    None => st.fresh(1u64 << (*k).min(6)),
                });
            }
            ("and", [Op::Reg(d), Op::Imm(k)]) => {
                let x = self.read(st, &Op::Reg(*d), d.bits);
                result = Some(match x.as_const() {
                    Some(c) => Val::konst(c & k),
                    None if *k != 0 => st.fresh(1u64 << k.trailing_zeros().min(6)),
                    None => Val::konst(0),
                });
            }
            ("xor", [Op::Reg(a), Op::Reg(b)]) if a == b => result = Some(Val::konst(0)),
            _ => {}
        }
        if m == "imul" && i.ops.len() == 1 {
            // One operand: rdx:rax = rax * src, the operand is only read.
            for r in [0, 2] {
                st.regs[r] = st.fresh(1);
            }
        } else if writes {
            if let Some(Op::Reg(d)) = i.ops.first() {
                let v = match result {
                    Some(v) => v,
                    None => st.fresh(1),
                };
                self.write(st, *d, v);
            }
        }
        if sets_flags {
            st.flags = flags;
        }
        Ok(())
    }

    fn cond(f: Flags, m: &str) -> Option<bool> {
        Some(match m {
            "ja" | "jnbe" => !f.cf && !f.zf,
            "jae" | "jnb" | "jnc" => !f.cf,
            "jb" | "jc" | "jnae" => f.cf,
            "jbe" | "jna" => f.cf || f.zf,
            "je" | "jz" => f.zf,
            "jne" | "jnz" => !f.zf,
            "jg" | "jnle" => !f.zf && f.sf == f.of,
            "jge" | "jnl" => f.sf == f.of,
            "jl" | "jnge" => f.sf != f.of,
            "jle" | "jng" => f.zf || f.sf != f.of,
            "js" => f.sf,
            "jns" => !f.sf,
            "jo" => f.of,
            "jno" => !f.of,
            _ => return None,
        })
    }

    /// Where an edge to `t` goes: Ok(true) inside the body, Ok(false) a
    /// named exit.
    fn edge(&self, from: &Insn, t: u64) -> Result<bool, String> {
        if (self.lo..self.hi).contains(&t) {
            if !self.by_addr.contains_key(&t) {
                return Err(format!(
                    "{:x}: jump into the middle of an instruction",
                    from.addr
                ));
            }
            return Ok(true);
        }
        match self.fnames.get(&t) {
            Some(n) if self.exits.iter().any(|e| e == n) => Ok(false),
            Some(n) => Err(format!(
                "{:x}: edge to {n}, which is not one of the export's exits ({})",
                from.addr,
                self.exits.join(", ")
            )),
            None => Err(format!(
                "{:x}: edge to {t:x}, not a function entry",
                from.addr
            )),
        }
    }

    fn walk(&self, pc: u64, mut st: State, out: &mut Walk) -> Result<(), String> {
        let mut pc = pc;
        loop {
            if st.trail.contains(&pc) {
                return Err(format!(
                    "P3: cycle through {pc:x} (back-edge on path {})",
                    hexs(&st.trail)
                ));
            }
            if out.fast_paths + out.exit_edges + out.traps > 1_000_000 {
                return Err("more than a million paths".into());
            }
            let i = self
                .by_addr
                .get(&pc)
                .filter(|_| (self.lo..self.hi).contains(&pc))
                .ok_or_else(|| format!("fell out of the body at {pc:x}"))?;
            st.trail.push(pc);
            // Fall through by the encoded length: a line objdump printed
            // and the parser skipped leaves a gap, which fails above.
            let next = pc + i.len;
            match kind(i) {
                Kind::Plain => {
                    st.insns += 1;
                    self.step(&mut st, i)?;
                    pc = next;
                }
                Kind::Trap => {
                    out.traps += 1;
                    return Ok(());
                }
                Kind::Ret => {
                    st.insns += 1;
                    self.finish(st, out);
                    return Ok(());
                }
                Kind::Jmp => {
                    st.insns += 1;
                    let Some(Op::Target(t)) = i.ops.first() else {
                        return Err(format!("indirect jump `{}`", i.text));
                    };
                    if !self.edge(i, *t)? {
                        out.exit_edges += 1;
                        return Ok(());
                    }
                    pc = *t;
                }
                Kind::Jcc => {
                    st.insns += 1;
                    st.branches += 1;
                    let Some(Op::Target(t)) = i.ops.first() else {
                        return Err(format!("indirect jump `{}`", i.text));
                    };
                    // Unknown flags, or a condition on an untracked flag
                    // (jp, jrcxz): both arms.
                    let taken = st.flags.and_then(|f| Self::cond(f, &i.mnem));
                    let inside = self.edge(i, *t)?;
                    if taken != Some(false) {
                        if inside {
                            self.walk(*t, st.clone(), out)?;
                        } else {
                            out.exit_edges += 1;
                        }
                    }
                    if taken == Some(true) {
                        return Ok(());
                    }
                    pc = next;
                }
            }
        }
    }

    fn finish(&self, st: State, out: &mut Walk) {
        let mut loads = 0;
        let mut stores = 0;
        for a in &st.accesses {
            loads += a.load as u32;
            stores += a.store as u32;
        }
        let (lines, desc) = self.lines(&st);
        let counts = [st.insns, loads, stores, st.branches, lines];
        for k in 0..5 {
            if counts[k] > out.max[k] || out.fast_paths == 0 {
                out.max[k] = counts[k];
                out.witness[k] = st.trail.clone();
            }
        }
        out.fast_paths += 1;
        out.cells_read.extend(st.cells_read.iter().cloned());
        out.paths.push((st.trail, counts, desc));
    }

    /// P4: distinct 64-byte lines a path touches (see the header).
    fn lines(&self, st: &State) -> (u32, Vec<String>) {
        let mut desc = Vec::new();
        let mut statics = BTreeSet::new();
        let mut groups: BTreeMap<Vec<(usize, i64)>, Vec<&Access>> = BTreeMap::new();
        for a in &st.accesses {
            if let Some(s) = a.addr.static_addr() {
                for b in s..s + a.size as u64 {
                    statics.insert(b >> 6);
                }
            } else {
                groups.entry(a.addr.terms.clone()).or_default().push(a);
            }
        }
        let mut total = statics.len() as u32;
        let mut named: BTreeSet<String> = BTreeSet::new();
        for a in &st.accesses {
            let Some(s) = a.addr.static_addr() else {
                continue;
            };
            let n = static_of(self.statics, s)
                .map(demangle)
                .unwrap_or_else(|| format!("{s:x}"));
            let base = self
                .statics
                .iter()
                .find(|(_, _, nm)| demangle(nm) == n)
                .map(|x| x.0);
            named.insert(match base {
                Some(b) => format!("{n}+{:#x} (line {:x})", s - b, s >> 6),
                None => format!("{n} (line {:x})", s >> 6),
            });
        }
        desc.extend(named.into_iter().map(|s| format!("static {s}")));
        for (terms, accs) in &groups {
            let align = terms
                .iter()
                .map(|&(a, c)| {
                    let al = if a == FS_ATOM { 1 } else { st.atoms[a].align };
                    let ca = 1u64 << (c.unsigned_abs().trailing_zeros().min(6));
                    (al * ca).min(64)
                })
                .min()
                .unwrap_or(64)
                .min(64);
            let mut best = None;
            for r in (0..64i64).filter(|r| (*r as u64) % align == 0) {
                let ok = accs.iter().all(|a| {
                    let n = 1i64 << (31 - a.size.min(8).leading_zeros());
                    (r + a.addr.c).rem_euclid(n) == 0
                });
                if !ok {
                    continue;
                }
                let mut ls = BTreeSet::new();
                for a in accs {
                    for b in 0..a.size as i64 {
                        ls.insert((r + a.addr.c + b).div_euclid(64));
                    }
                }
                best = best.max(Some(ls.len() as u32));
            }
            let n = best.unwrap_or_else(|| die("no base residue satisfies the alignment premises"));
            total += n;
            let offs: Vec<String> = accs
                .iter()
                .map(|a| format!("{:#x}/{}", a.addr.c, a.size))
                .collect();
            let at: Vec<String> = accs.iter().map(|a| format!("{:x}", a.at)).collect();
            desc.push(format!(
                "group base-align {align} lines {n}: offsets {} at {}",
                offs.join(" "),
                at.join(" ")
            ));
        }
        (total, desc)
    }
}

fn hexs(v: &[u64]) -> String {
    v.iter()
        .map(|a| format!("{a:x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

const METRICS: [&str; 5] = ["insns", "loads", "stores", "branches", "lines"];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, rest) = match args.first().map(|s| s.as_str()) {
        Some("--print") => ("print", &args[1..]),
        Some("--explain") => ("explain", &args[1..]),
        _ => ("check", &args[..]),
    };
    let [lib, table] = rest else {
        die("usage: perf-budget [--print|--explain] LIB TABLE");
    };
    let header = objdump(&["-f"], lib);
    if !header.contains("i386:x86-64") {
        die(&format!(
            "{lib} is not an x86_64 object; the checker reads x86_64 code only"
        ));
    }

    // Dynamic symbols (exports) and all symbols (functions and statics).
    let mut exports: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for l in objdump(&["-T", "-w"], lib).lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 7 && f.contains(&"DF") && f.contains(&".text") {
            let (Some(a), Some(n)) = (parse_hex(f[0]), parse_hex(f[f.len() - 3])) else {
                continue;
            };
            exports.insert(f[f.len() - 1].to_string(), (a, n));
        }
    }
    let mut statics = Vec::new();
    let mut fnames: BTreeMap<u64, String> = BTreeMap::new();
    for l in objdump(&["-t", "-w"], lib).lines() {
        // ADDR FLAGS... SECTION SIZE [.hidden] NAME
        let f: Vec<&str> = l.split_whitespace().collect();
        let Some(sec) = f
            .iter()
            .position(|w| w.starts_with('.') || w.starts_with('*'))
        else {
            continue;
        };
        if f.len() < sec + 3 {
            continue;
        }
        let (Some(a), Some(n)) = (parse_hex(f[0]), parse_hex(f[sec + 1])) else {
            continue;
        };
        let name = f[f.len() - 1].to_string();
        if l.contains(" F .text") {
            fnames.insert(a, demangle(&name));
        } else if l.contains(" O ") {
            statics.push((a, n, name));
        }
    }
    for (name, (a, _)) in &exports {
        fnames.entry(*a).or_insert_with(|| name.clone());
    }

    let mut by_addr = BTreeMap::new();
    for l in objdump(&["-d", "-M", "intel", "-w"], lib).lines() {
        match parse_insn(l) {
            Some(Ok(i)) => {
                by_addr.insert(i.addr, i);
            }
            Some(Err(e)) => {
                // Only fatal inside a checked body; record as a poisoned slot.
                if let Some((a, _)) = l.split_once(":\t") {
                    if let Some(a) = parse_hex(a.trim()) {
                        by_addr.insert(
                            a,
                            Insn {
                                addr: a,
                                len: 0,
                                prefixes: vec![],
                                mnem: format!("<unparsed: {e}>"),
                                ops: vec![],
                                text: l.trim().to_string(),
                            },
                        );
                    }
                }
            }
            None => {}
        }
    }

    let text = std::fs::read_to_string(table).unwrap_or_else(|e| die(&format!("{table}: {e}")));
    let mut align_load = BTreeMap::new();
    let mut cells: BTreeMap<String, (u64, u32)> = BTreeMap::new();
    let mut profiles: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut export_exits: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut budgets = Vec::new();
    for (n, l) in text.lines().enumerate() {
        let l = l.split('#').next().unwrap().trim();
        if l.is_empty() {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        let bad = || -> ! { die(&format!("{table}:{}: cannot parse `{l}`", n + 1)) };
        match f[0] {
            "align-load" if f.len() == 3 => {
                align_load.insert(f[1].to_string(), parse_num(f[2]).unwrap_or_else(|| bad()));
            }
            "cell" if f.len() == 5 => {
                let base = statics
                    .iter()
                    .find(|(_, _, s)| demangle(s) == f[2])
                    .unwrap_or_else(|| die(&format!("{table}:{}: no static {}", n + 1, f[2])))
                    .0;
                let off = parse_num(f[3]).unwrap_or_else(|| bad());
                let size = parse_num(f[4]).unwrap_or_else(|| bad()) as u32;
                cells.insert(f[1].to_string(), (base + off, size));
            }
            "export" if f.len() >= 2 => {
                export_exits.insert(
                    f[1].to_string(),
                    f[2..].iter().map(|s| s.to_string()).collect(),
                );
            }
            "budget" if f.len() == 8 => {
                let mut want = [0u32; 5];
                for k in 0..5 {
                    want[k] = f[3 + k].parse::<u32>().unwrap_or_else(|_| bad());
                }
                budgets.push((n + 1, f[1].to_string(), f[2].to_string(), want));
            }
            "paths" if f.len() >= 4 => {
                let key = (f[1].to_string(), f[2].to_string());
                profiles.insert(
                    key,
                    f[3..].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                );
            }
            _ => bad(),
        }
    }

    let mut failures = Vec::new();
    let mut seen_profiles = BTreeSet::new();
    for (name, exits) in &export_exits {
        let Some(&(lo, size)) = exports.get(name) else {
            failures.push(format!("{name}: not a dynamic symbol of {lib}"));
            continue;
        };
        for e in exits {
            if !fnames.values().any(|n| n == e) {
                failures.push(format!("{name}: exit {e} is not a function of {lib}"));
            }
        }
        // P2 over the whole body.
        let mut ninsns = 0;
        for (_, i) in by_addr.range(lo..lo + size) {
            ninsns += 1;
            if i.mnem.starts_with("<unparsed") {
                failures.push(format!("{name}: {:x}: {} `{}`", i.addr, i.mnem, i.text));
            } else if let Some(what) = forbidden(i) {
                failures.push(format!("P2 {name}: {what} at {:x}: `{}`", i.addr, i.text));
            }
        }
        if mode != "print" {
            println!("{name}: body {ninsns} instructions at {lo:x}..{:x}: no lock, xchg/xadd/cmpxchg, fence, call, syscall or indirect jump", lo + size);
        }
    }

    let mut printed = Vec::new();
    for (line, name, config, want) in &budgets {
        // Marked seen first, so a row that fails below is not also
        // reported as a `paths` line without a budget.
        seen_profiles.insert((name.clone(), config.clone()));
        let Some(exits) = export_exits.get(name) else {
            failures.push(format!(
                "{table}:{line}: budget for {name}, which has no export line"
            ));
            continue;
        };
        let Some(&(lo, size)) = exports.get(name) else {
            continue;
        };
        let mut cfg = BTreeMap::new();
        if config != "-" {
            for kv in config.split(',') {
                let Some((k, v)) = kv.split_once('=') else {
                    die(&format!("{table}:{line}: bad configuration {kv}"))
                };
                let Some(&(a, sz)) = cells.get(k) else {
                    die(&format!("{table}:{line}: no cell {k}"))
                };
                let v =
                    parse_num(v).unwrap_or_else(|| die(&format!("{table}:{line}: bad value {v}")));
                if v & !mask(sz * 8) != 0 {
                    die(&format!(
                        "{table}:{line}: {k}={v:#x} does not fit its {sz} bytes"
                    ));
                }
                cfg.insert(a, (k.to_string(), sz, v));
            }
        }
        let ctx = Ctx {
            by_addr: &by_addr,
            statics: &statics,
            align_load: &align_load,
            cells: cfg.clone(),
            lo,
            hi: lo + size,
            exits,
            fnames: &fnames,
        };
        let mut st = State {
            regs: Vec::new(),
            flags: None,
            // FS_ATOM, and IMAGE_ATOM: the load base is page-aligned.
            atoms: vec![Atom { align: 1 }, Atom { align: 4096 }],
            insns: 0,
            branches: 0,
            accesses: vec![],
            trail: vec![],
            cells_read: BTreeSet::new(),
            fs_loads: 0,
        };
        for _ in 0..NREGS {
            let v = st.fresh(1);
            st.regs.push(v);
        }
        let mut out = Walk::default();
        if let Err(e) = ctx.walk(lo, st, &mut out) {
            failures.push(format!("{name} [{config}]: {e}"));
            continue;
        }
        for (_, (k, _, _)) in &cfg {
            if !out.cells_read.contains(k) {
                failures.push(format!(
                    "{name} [{config}]: no fast path reads cell {k}; its offset in {table} is stale"
                ));
            }
        }
        if out.fast_paths == 0 {
            failures.push(format!("{name} [{config}]: no path reaches a ret"));
            continue;
        }
        // Every fast path's counts, not just the maxima: a cost added to a
        // path below the worst one fails too.
        let mut prof: Vec<String> = out
            .paths
            .iter()
            .map(|(_, c, _)| format!("{}/{}/{}/{}/{}", c[0], c[1], c[2], c[3], c[4]))
            .collect();
        prof.sort();
        printed.push(format!("paths  {name:<20} {config:<14} {}", prof.join(" ")));
        let key = (name.clone(), config.clone());
        match profiles.get(&key) {
            None => failures.push(format!(
                "{name} [{config}]: no `paths {name} {config}` line in {table}"
            )),
            Some(want) if *want != prof => failures.push(format!(
                "{name} [{config}]: the fast paths' counts (insns/loads/stores/branches/lines, sorted) changed: \
                 measured {}, {table} says {}",
                prof.join(" "),
                want.join(" ")
            )),
            Some(_) => {}
        }
        let got = out.max;
        printed.push(format!(
            "budget {name:<20} {config:<14} {:>5} {:>5} {:>6} {:>8} {:>5}",
            got[0], got[1], got[2], got[3], got[4]
        ));
        if mode == "print" {
            continue;
        }
        println!(
            "{name} [{config}]: {} fast paths, {} exit edges, {} traps; max insns {} loads {} stores {} branches {} lines {}",
            out.fast_paths, out.exit_edges, out.traps, got[0], got[1], got[2], got[3], got[4]
        );
        for k in 0..5 {
            if got[k] != want[k] {
                let dir = if got[k] > want[k] { "GREW" } else { "shrank" };
                failures.push(format!(
                    "{name} [{config}]: max {} {dir}: measured {}, {table}:{line} says {} (worst path: {})",
                    METRICS[k], got[k], want[k], hexs(&out.witness[k])
                ));
            }
        }
        if mode == "explain" {
            for (trail, c, desc) in &out.paths {
                println!(
                    "  path insns {} loads {} stores {} branches {} lines {}: {}",
                    c[0],
                    c[1],
                    c[2],
                    c[3],
                    c[4],
                    hexs(trail)
                );
                for d in desc {
                    println!("    {d}");
                }
            }
        }
    }
    for (name, config) in profiles.keys() {
        if !seen_profiles.contains(&(name.clone(), config.clone())) {
            failures.push(format!(
                "{table}: `paths {name} {config}` has no budget line"
            ));
        }
    }
    if mode == "print" {
        for p in printed {
            println!("{p}");
        }
    }
    if !failures.is_empty() {
        for f in &failures {
            eprintln!("FAIL {f}");
        }
        eprintln!(
            "perf-budget: {} failure(s). A hot export's fast path changed: if the change is intended, \
             rerun with --print and update {table} and verus/THEOREMS.md theorem 45",
            failures.len()
        );
        exit(1);
    }
    if mode == "check" {
        println!("perf-budget: OK ({} budgets)", budgets.len());
    }
}
