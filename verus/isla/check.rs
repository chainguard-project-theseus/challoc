//! isla-check: compose per-instruction Isla traces into one instruction
//! sequence and check a job's footprint and value properties with Z3
//! (verus/ISLA.md). Std only; built by verus/isla/run.sh with `rustc -O`.
//!
//!   isla-check JOB
//!
//! The job is a line-oriented file (`#` comments):
//!
//!   insn FILE fallthrough|svc  next instruction: the `isla-footprint -s`
//!                              output in FILE, and what a normal trace of
//!                              it does (falls through to PC+4 / takes the
//!                              SVC exception to EL1 or EL2)
//!   writes REG...              the registers (or REG.FIELD) the sequence may
//!                              write besides the PC and PSTATE's non-flag
//!                              fields (anything else written is an error;
//!                              system registers an exception entry writes
//!                              are allowed only for `svc`)
//!   no-mem-write               no trace writes memory
//!   flags-preserved            no trace writes PSTATE.{N,Z,C,V}
//!   mem-reads N                every normal combination reads memory N times
//!   decl NAME BITS             a free bit-vector constant for assume/assert
//!   assume SMT                 a residual assumption (named in the record)
//!   assert SMT                 a property of every normal combination
//!
//! In SMT text, `init.R` and `final.R` are register R before and after the
//! sequence (`init.R.F`, `final.R.F` for field F of a struct register such
//! as PSTATE), `seg.K.NAME` is the symbolic opcode field NAME of
//! instruction K (from 1, `isla-footprint --partial`), and `mem.J.addr`,
//! `mem.J.val` are the J-th memory read of the sequence (from 1; address as
//! Isla's model issues it, value as read).
//!
//! For every choice of one trace per instruction the checker links the
//! traces (each register read equals the value the sequence last wrote to
//! it, or its `init.` value; reads of a register the sequence never wrote
//! and that Isla gave a literal are left as they are; a read of a register
//! whose last write the checker could not express is an error), conjoins
//! every trace's path condition (dropping conjuncts it cannot express,
//! which only weakens it) and the job's assumptions, and asks Z3 whether
//! the combination is feasible. A feasible combination with an abnormal
//! trace (an exception where none is expected, or no PC write) is an
//! error. For a feasible normal one, each `assert` must follow (its
//! negation unsat), the footprint lines must hold, and every event must be
//! one the checker knows. A job with no feasible combination fails. Exit
//! status 0 when everything holds, 1 when something fails, 2 on a usage
//! error or a Z3 answer other than sat/unsat (an undeclared name included).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::process::Command;
use std::{env, fs, process};

#[derive(Clone, Debug)]
enum Sx {
    Atom(String),
    List(Vec<Sx>),
}

fn parse_all(src: &str) -> Vec<Sx> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut stack: Vec<Vec<Sx>> = vec![Vec::new()];
    while i < b.len() {
        let c = b[i];
        match c {
            b';' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'(' => {
                stack.push(Vec::new());
                i += 1;
            }
            b')' => {
                let l = stack.pop().expect("unbalanced )");
                stack.last_mut().expect("unbalanced )").push(Sx::List(l));
                i += 1;
            }
            b' ' | b'\n' | b'\t' | b'\r' => i += 1,
            b'|' | b'"' => {
                let s = i;
                i += 1;
                while i < b.len() && b[i] != c {
                    i += 1;
                }
                i += 1;
                stack
                    .last_mut()
                    .unwrap()
                    .push(Sx::Atom(src[s..i].to_string()));
            }
            _ => {
                let s = i;
                while i < b.len() && !b" \n\t\r()".contains(&b[i]) {
                    i += 1;
                }
                stack
                    .last_mut()
                    .unwrap()
                    .push(Sx::Atom(src[s..i].to_string()));
            }
        }
    }
    assert!(stack.len() == 1, "unbalanced (");
    stack.pop().unwrap()
}

fn atom(s: &Sx) -> Option<&str> {
    match s {
        Sx::Atom(a) => Some(a),
        _ => None,
    }
}

fn is_var(a: &str) -> bool {
    a.len() > 1 && a.starts_with('v') && a[1..].bytes().all(|c| c.is_ascii_digit())
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ty {
    Bool,
    Bv(u32),
}

impl Ty {
    fn smt(self) -> String {
        match self {
            Ty::Bool => "Bool".into(),
            Ty::Bv(w) => format!("(_ BitVec {w})"),
        }
    }
}

fn smt_type(s: &Sx) -> Option<Ty> {
    match s {
        Sx::Atom(a) if a == "Bool" => Some(Ty::Bool),
        Sx::List(l)
            if l.len() == 3 && atom(&l[0]) == Some("_") && atom(&l[1]) == Some("BitVec") =>
        {
            Some(Ty::Bv(atom(&l[2])?.parse().ok()?))
        }
        _ => None,
    }
}

/// The expression as SMT-LIB over bit vectors with its sort, Isla's
/// variables renamed by `ren` (which also gives their sorts), or None when
/// it uses something plain SMT-LIB lacks (structs, enums, Isla's
/// `(_ bv-1 N)`), an operator this checker does not know, or a dropped
/// variable.
fn emit(s: &Sx, ren: &dyn Fn(&str) -> Option<(String, Ty)>) -> Option<(String, Ty)> {
    match s {
        Sx::Atom(a) => {
            if is_var(a) {
                ren(a)
            } else if let Some(h) = a.strip_prefix("#x") {
                Some((a.clone(), Ty::Bv(4 * h.len() as u32)))
            } else if let Some(b) = a.strip_prefix("#b") {
                Some((a.clone(), Ty::Bv(b.len() as u32)))
            } else if a == "true" || a == "false" {
                Some((a.clone(), Ty::Bool))
            } else {
                None
            }
        }
        Sx::List(l) => {
            let head = l.first()?;
            // An indexed literal `(_ bvN W)`.
            if let [Sx::Atom(u), Sx::Atom(k), Sx::Atom(w)] = &l[..] {
                if u == "_" {
                    let n = k.strip_prefix("bv")?;
                    n.parse::<u128>().ok()?;
                    return Some((format!("(_ {k} {w})"), Ty::Bv(w.parse().ok()?)));
                }
            }
            let mut args = Vec::new();
            for x in &l[1..] {
                args.push(emit(x, ren)?);
            }
            let text = |op: &str| {
                let mut o = format!("({op}");
                for (a, _) in &args {
                    o.push(' ');
                    o.push_str(a);
                }
                o.push(')');
                o
            };
            let w = |i: usize| match args.get(i) {
                Some((_, Ty::Bv(w))) => Some(*w),
                _ => None,
            };
            match head {
                // An indexed operator `((_ extract i j) e)` and friends.
                Sx::List(ix) => {
                    let name = atom(ix.get(1)?)?;
                    let n: Vec<u32> = ix[2..]
                        .iter()
                        .map(|x| atom(x)?.parse().ok())
                        .collect::<Option<_>>()?;
                    let op = format!(
                        "(_ {name} {})",
                        n.iter()
                            .map(|x| x.to_string())
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                    let ty = match name {
                        "extract" => Ty::Bv(n[0] - n[1] + 1),
                        "zero_extend" | "sign_extend" => Ty::Bv(w(0)? + n[0]),
                        "repeat" => Ty::Bv(w(0)? * n[0]),
                        "rotate_left" | "rotate_right" => Ty::Bv(w(0)?),
                        _ => return None,
                    };
                    Some((text(&op), ty))
                }
                Sx::Atom(op) => {
                    let ty = match op.as_str() {
                        "=" | "not" | "and" | "or" | "xor" | "=>" | "distinct" | "bvult"
                        | "bvule" | "bvugt" | "bvuge" | "bvslt" | "bvsle" | "bvsgt" | "bvsge" => {
                            Ty::Bool
                        }
                        "bvadd" | "bvsub" | "bvmul" | "bvand" | "bvor" | "bvxor" | "bvnot"
                        | "bvneg" | "bvshl" | "bvlshr" | "bvashr" | "bvudiv" | "bvurem"
                        | "bvsdiv" | "bvsrem" | "bvsmod" | "bvnand" | "bvnor" | "bvxnor" => {
                            Ty::Bv(w(0)?)
                        }
                        "concat" => Ty::Bv(
                            args.iter()
                                .map(|(_, t)| if let Ty::Bv(w) = t { *w } else { 0 })
                                .sum(),
                        ),
                        "bvcomp" => Ty::Bv(1),
                        "ite" => args.get(1)?.1,
                        _ => return None,
                    };
                    Some((text(op), ty))
                }
            }
        }
    }
}

fn reg_name(s: &Sx) -> String {
    atom(s).unwrap_or("?").trim_matches('|').to_string()
}

/// One instruction: its `isla-footprint` traces and its expected kind.
struct Insn {
    file: String,
    kind: String,
    /// Symbolic opcode fields: Isla variable -> (field name, width).
    segs: BTreeMap<String, (String, u32)>,
    traces: Vec<Vec<Sx>>,
}

fn load_insn(file: &str, kind: &str) -> Insn {
    let src = fs::read_to_string(file).unwrap_or_else(|e| die(&format!("{file}: {e}")));
    let mut segs = BTreeMap::new();
    let mut traces = Vec::new();
    for top in parse_all(&src) {
        let Sx::List(l) = top else { continue };
        match l.first().and_then(atom) {
            Some("segments") => {
                for s in &l[1..] {
                    if let Sx::List(f) = s {
                        let name = reg_name(&f[0]);
                        let bits = atom(&f[1])
                            .and_then(|b| b.parse().ok())
                            .unwrap_or_else(|| die("segments"));
                        segs.insert(atom(&f[2]).unwrap_or("").to_string(), (name, bits));
                    }
                }
            }
            Some("trace") => traces.push(l[1..].to_vec()),
            _ => {}
        }
    }
    if traces.is_empty() {
        die(&format!("{file}: no traces (did isla-footprint fail?)"));
    }
    Insn {
        file: file.to_string(),
        kind: kind.to_string(),
        segs,
        traces,
    }
}

fn die(msg: &str) -> ! {
    eprintln!("isla-check: {msg}");
    process::exit(2)
}

#[derive(Default)]
struct Job {
    insns: Vec<Insn>,
    writes: BTreeSet<String>,
    no_mem_write: bool,
    flags_preserved: bool,
    mem_reads: Option<usize>,
    decls: Vec<(String, u32)>,
    assumes: Vec<String>,
    asserts: Vec<String>,
}

fn load_job(path: &str) -> Job {
    let src = fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let dir = std::path::Path::new(path)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    let mut job = Job::default();
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        match key {
            "insn" => {
                let mut w = rest.split_whitespace();
                let f = w.next().unwrap_or_else(|| die("insn: no file"));
                let kind = w.next().unwrap_or("fallthrough");
                if kind != "fallthrough" && kind != "svc" {
                    die(&format!("insn: unknown kind {kind}"));
                }
                let f = dir.join(f);
                job.insns.push(load_insn(&f.to_string_lossy(), kind));
            }
            "writes" => job.writes.extend(rest.split_whitespace().map(String::from)),
            "no-mem-write" => job.no_mem_write = true,
            "flags-preserved" => job.flags_preserved = true,
            "mem-reads" => {
                job.mem_reads = Some(rest.parse().unwrap_or_else(|_| die("mem-reads N")))
            }
            "decl" => {
                let mut w = rest.split_whitespace();
                let n = w
                    .next()
                    .unwrap_or_else(|| die("decl NAME BITS"))
                    .to_string();
                let b = w
                    .next()
                    .and_then(|b| b.parse().ok())
                    .unwrap_or_else(|| die("decl NAME BITS"));
                job.decls.push((n, b));
            }
            "assume" => job.assumes.push(rest.to_string()),
            "assert" => job.asserts.push(rest.to_string()),
            _ => die(&format!("{path}: unknown line {line}")),
        }
    }
    job
}

/// PSTATE fields `flags-preserved` covers.
const FLAGS: [&str; 4] = ["N", "Z", "C", "V"];
/// Registers an exception entry writes; a trace writing one of them took
/// an exception.
const EXC_REGS: [&str; 9] = [
    "ESR_EL1", "ELR_EL1", "SPSR_EL1", "FAR_EL1", "ESR_EL2", "ELR_EL2", "SPSR_EL2", "FAR_EL2",
    "ESR_EL3",
];

/// Registers user code holds values in: a write to one the job does not
/// list is an error even inside an exception entry.
fn is_user_reg(r: &str) -> bool {
    (r.starts_with('R') && r[1..].parse::<u32>().is_ok_and(|n| n <= 30))
        || r.starts_with("SP_EL0")
        || r.starts_with("_V")
        || r.starts_with("_Z")
        || r.starts_with("_P")
        || r == "FPSR"
        || r == "FPCR"
        || r == "TPIDR_EL0"
}

/// The Sail model's own bookkeeping, not architectural state: `SEE` (the
/// decoder's search position) and the `__`-prefixed helpers.
fn is_model_internal(r: &str) -> bool {
    r == "SEE" || r.starts_with("__")
}

/// Rewrite the job's dotted names in SMT text to SMT symbols
/// (`init.R0` -> `init_R0`).
fn job_smt(s: &str) -> String {
    let mut out = String::new();
    let mut tok = String::new();
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '#' {
            tok.push(c);
        } else {
            if tok.contains('.') && !tok.starts_with('#') {
                out.push_str(&tok.replace('.', "_"));
            } else {
                out.push_str(&tok);
            }
            tok.clear();
            out.push(c);
        }
    }
    out.pop();
    out
}

struct Built {
    smt: String,
    abnormal: Vec<String>,
    violations: Vec<String>,
    mem_reads: usize,
}

/// The SMT script for one combination (trace `pick[k]` of instruction k):
/// declarations, path conditions, register links and the job's
/// declarations and assumptions; plus what the combination's events break.
fn build(job: &Job, pick: &[usize]) -> Built {
    let mut smt = String::new();
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut decl = |smt: &mut String, name: &str, ty: Ty| {
        if declared.insert(name.to_string()) {
            writeln!(smt, "(declare-const {name} {})", ty.smt()).unwrap();
        }
    };
    // Register -> (SMT term of its current value, sort).
    let mut state: BTreeMap<String, Option<(String, Ty)>> = BTreeMap::new();
    let mut abnormal = Vec::new();
    let mut violations = Vec::new();
    let mut mem = 0usize;
    for (k, (insn, &t)) in job.insns.iter().zip(pick).enumerate() {
        let k1 = k + 1;
        for (name, bits) in insn.segs.values() {
            decl(&mut smt, &format!("seg_{k1}_{name}"), Ty::Bv(*bits));
        }
        let pre = format!("t{k1}_");
        let mut vars: HashMap<String, Ty> = HashMap::new();
        let mut wrote_pc = false;
        let mut wrote_exc = false;
        for ev in &insn.traces[t] {
            let Sx::List(l) = ev else { continue };
            let head = l.first().and_then(atom).unwrap_or("");
            let ren = |a: &str| -> Option<(String, Ty)> {
                if let Some((n, w)) = insn.segs.get(a) {
                    Some((format!("seg_{k1}_{n}"), Ty::Bv(*w)))
                } else {
                    vars.get(a).map(|ty| (format!("{pre}{a}"), *ty))
                }
            };
            match head {
                "declare-const" => {
                    let v = atom(&l[1]).unwrap_or("");
                    if insn.segs.contains_key(v) {
                        continue;
                    }
                    if let Some(ty) = smt_type(&l[2]) {
                        decl(&mut smt, &format!("{pre}{v}"), ty);
                        vars.insert(v.to_string(), ty);
                    }
                }
                "define-const" => {
                    let v = atom(&l[1]).unwrap_or("").to_string();
                    if let Some((e, ty)) = emit(&l[2], &ren) {
                        writeln!(smt, "(define-fun {pre}{v} () {} {e})", ty.smt()).unwrap();
                        vars.insert(v, ty);
                    }
                }
                "assert" => {
                    // A dropped conjunct only weakens the path condition:
                    // a property proved without it still holds.
                    if let Some((e, _)) = emit(&l[1], &ren) {
                        writeln!(smt, "(assert {e})").unwrap();
                    }
                }
                "read-reg" | "write-reg" => {
                    let r = reg_name(&l[1]);
                    // The field a write names (`((_ field |EL|))`), if any.
                    let field = match &l[2] {
                        Sx::List(acc) => match acc.first() {
                            Some(Sx::List(f)) if f.len() == 3 => Some(reg_name(&f[2])),
                            _ => Some("?".to_string()),
                        },
                        _ => None,
                    };
                    // Isla gives a struct register's whole value even for a
                    // field access: track each field as its own location
                    // `R.F`; a plain register is the location `R`.
                    let locs: Vec<(String, Option<(String, Ty)>, bool)> = match &l[3] {
                        Sx::List(s)
                            if atom(&s[0]) == Some("_") && atom(&s[1]) == Some("struct") =>
                        {
                            s[2..]
                                .iter()
                                .map(|f| match f {
                                    Sx::List(p) if p.len() == 2 => (
                                        format!("{r}.{}", reg_name(&p[0])),
                                        emit(&p[1], &ren),
                                        atom(&p[1]).is_some_and(is_var),
                                    ),
                                    _ => (format!("{r}.?"), None, false),
                                })
                                .collect()
                        }
                        v => vec![(r.clone(), emit(v, &ren), atom(v).is_some_and(is_var))],
                    };
                    if head == "read-reg" {
                        for (loc, val, var) in locs {
                            let Some((val, ty)) = val else { continue };
                            match state.get(&loc) {
                                Some(None) => violations.push(format!(
                                    "insn {k1} reads {loc}, which an earlier write left inexpressible"
                                )),
                                Some(Some((c, _))) => writeln!(smt, "(assert (= {val} {c}))").unwrap(),
                                None if var => {
                                    let i = format!("init_{}", loc.replace('.', "_"));
                                    decl(&mut smt, &i, ty);
                                    writeln!(smt, "(assert (= {val} {i}))").unwrap();
                                    state.insert(loc, Some((i, ty)));
                                }
                                // A literal the model's configuration fixed.
                                None => {}
                            }
                        }
                        continue;
                    }
                    let written = match &field {
                        Some(f) => format!("{r}.{f}"),
                        None => r.clone(),
                    };
                    if r == "_PC" {
                        wrote_pc = true;
                    } else if EXC_REGS.contains(&r.as_str()) {
                        wrote_exc = true;
                    } else if is_model_internal(&r)
                        || job.writes.contains(&r)
                        || job.writes.contains(&written)
                    {
                    } else if r == "PSTATE" {
                        // PSTATE's other fields are bookkeeping (BTYPE) or,
                        // in an exception entry, the new EL and masks.
                        let f = field.as_deref().unwrap_or("?");
                        if job.flags_preserved && (FLAGS.contains(&f) || f == "?") {
                            violations.push(format!("insn {k1} writes PSTATE.{f}"));
                        }
                    } else if is_user_reg(&r) || insn.kind != "svc" {
                        // An exception entry writes system state; user
                        // registers never.
                        violations.push(format!("insn {k1} writes {written}"));
                    }
                    for (loc, val, _) in locs {
                        // An inexpressible value poisons the location: a
                        // later read of it is an error, never linked to a
                        // stale or initial value.
                        state.insert(loc, val);
                    }
                }
                "read-mem" => {
                    mem += 1;
                    let (Some((v, vt)), Some((a, at))) = (emit(&l[1], &ren), emit(&l[3], &ren))
                    else {
                        violations.push(format!("insn {k1}: memory read not expressible"));
                        continue;
                    };
                    let bytes: u32 = atom(&l[4]).and_then(|b| b.parse().ok()).unwrap_or(0);
                    if vt != Ty::Bv(8 * bytes) {
                        violations.push(format!(
                            "insn {k1}: memory read of {bytes} bytes has sort {vt:?}"
                        ));
                    }
                    decl(&mut smt, &format!("mem_{mem}_addr"), at);
                    decl(&mut smt, &format!("mem_{mem}_val"), vt);
                    writeln!(smt, "(assert (= mem_{mem}_addr {a}))").unwrap();
                    writeln!(smt, "(assert (= mem_{mem}_val {v}))").unwrap();
                }
                "write-mem" => {
                    if job.no_mem_write {
                        violations.push(format!("insn {k1} writes memory"));
                    }
                }
                // Events with no effect on registers or memory: path
                // bookkeeping, model-level assumptions about registers the
                // configuration fixed, and tracing.
                "assume-reg" | "assume" | "branch" | "branch-address" | "cycle" | "call"
                | "return" | "define-enum" | "instr" | "mark-reg" => {}
                // Anything else (a tag access, an announced write, an
                // event a newer Isla adds) fails closed.
                other => violations.push(format!("insn {k1}: unknown event {other}")),
            }
        }
        let normal = match insn.kind.as_str() {
            "svc" => wrote_exc && wrote_pc,
            _ => !wrote_exc && wrote_pc,
        };
        if !normal {
            abnormal.push(format!("insn {k1} ({}) trace {}", insn.file, t + 1));
        }
    }
    // `final.R` for every location with an expressible value; a job that
    // asserts about a poisoned one names an undeclared constant, which Z3
    // rejects (a failure, not a pass).
    for (r, v) in &state {
        let Some((v, ty)) = v else { continue };
        let f = format!("final_{}", r.replace('.', "_"));
        decl(&mut smt, &f, *ty);
        writeln!(smt, "(assert (= {f} {v}))").unwrap();
    }
    for (n, b) in &job.decls {
        decl(&mut smt, &n.replace('.', "_"), Ty::Bv(*b));
    }
    for a in &job.assumes {
        writeln!(smt, "(assert {})", job_smt(a)).unwrap();
    }
    Built {
        smt,
        abnormal,
        violations,
        mem_reads: mem,
    }
}

/// Z3's answer to `smt`, then `extra`, then `(check-sat)`.
fn z3(smt: &str, extra: &str) -> String {
    let z3 = env::var("Z3").unwrap_or_else(|_| "z3".into());
    let path = env::temp_dir().join(format!("isla-check-{}.smt2", process::id()));
    fs::write(&path, format!("{smt}{extra}\n(check-sat)\n"))
        .unwrap_or_else(|e| die(&format!("{e}")));
    let out = Command::new(&z3)
        .arg("-smt2")
        .arg(&path)
        .output()
        .unwrap_or_else(|e| die(&format!("{z3}: {e}")));
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s != "sat" && s != "unsat" {
        let keep = env::temp_dir().join("isla-check-failed.smt2");
        let _ = fs::copy(&path, &keep);
        die(&format!("z3 said: {s} (query kept in {})", keep.display()));
    }
    let _ = fs::remove_file(&path);
    s
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        die("usage: isla-check JOB");
    }
    let job = load_job(&args[1]);
    if job.insns.is_empty() {
        die("job has no insn lines");
    }
    let counts: Vec<usize> = job.insns.iter().map(|i| i.traces.len()).collect();
    let total: usize = counts.iter().product();
    let mut pick = vec![0usize; counts.len()];
    let (mut feasible, mut checked, mut failed) = (0, 0, 0);
    for _ in 0..total {
        let b = build(&job, &pick);
        if env::var_os("ISLA_CHECK_DUMP").is_some() {
            println!("; combination {pick:?}\n{}", b.smt);
        }
        if z3(&b.smt, "") == "sat" {
            feasible += 1;
            let label = format!(
                "combination {:?}",
                pick.iter().map(|p| p + 1).collect::<Vec<_>>()
            );
            if !b.abnormal.is_empty() {
                failed += 1;
                println!(
                    "FAIL {label}: feasible abnormal path: {}",
                    b.abnormal.join(", ")
                );
            } else {
                for v in &b.violations {
                    failed += 1;
                    println!("FAIL {label}: {v}");
                }
                if job.mem_reads.is_some_and(|n| n != b.mem_reads) {
                    failed += 1;
                    println!(
                        "FAIL {label}: {} memory reads, expected {:?}",
                        b.mem_reads, job.mem_reads
                    );
                }
                for a in &job.asserts {
                    checked += 1;
                    if z3(&b.smt, &format!("(assert (not {}))", job_smt(a))) != "unsat" {
                        failed += 1;
                        println!("FAIL {label}: assert {a}");
                    }
                }
            }
        }
        for k in (0..pick.len()).rev() {
            pick[k] += 1;
            if pick[k] < counts[k] {
                break;
            }
            pick[k] = 0;
        }
    }
    if feasible == 0 {
        failed += 1;
        println!("FAIL: no feasible combination (the assumptions contradict the traces)");
    }
    let verdict = if failed == 0 {
        "ok".to_string()
    } else {
        format!("{failed} FAILED")
    };
    println!(
        "{}: {total} combinations, {feasible} feasible, {checked} assertion checks, {verdict}",
        args[1]
    );
    process::exit(if failed == 0 { 0 } else { 1 });
}
