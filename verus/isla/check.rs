//! isla-check: compose per-instruction Isla traces into one instruction
//! sequence and check a job's footprint and value properties with Z3
//! (verus/ISLA.md). Std only; built by verus/isla/run.sh with `rustc -O`.
//!
//!   isla-check JOB
//!
//! The job is a line-oriented file (`#` comments):
//!
//!   insn FILE KIND             next instruction: the `isla-footprint -s`
//!                              output in FILE, and what a normal trace of
//!                              it does: `fallthrough` (no exception, and
//!                              the PC provably ends at PC+4), `ret` (no
//!                              exception; the PC as the asserts say), `svc`
//!                              or `brk` (takes an exception to EL1 or EL2)
//!   copy NAME                  start a copy of the machine: the `insn`
//!                              lines after it run from their own initial
//!                              state, named `NAME.init.R` and so on (a
//!                              relational job: two copies and assumptions
//!                              equating their inputs); without `copy`
//!                              lines there is one unnamed copy
//!   abstract FN                Isla's `--abstract FN` calls are an
//!                              uninterpreted function of their arguments,
//!                              shared by every copy (the record argues FN
//!                              is a function); any other abstract call is
//!                              an error
//!   writes REG...              the registers (or REG.FIELD) the sequence may
//!                              write besides the PC and PSTATE's non-flag
//!                              fields (anything else written is an error;
//!                              system registers an exception entry writes
//!                              are allowed only for `svc` and `brk`)
//!   no-mem-write               no trace writes memory
//!   flags-preserved            no trace writes PSTATE.{N,Z,C,V}
//!   mem-reads N                every normal combination reads memory N
//!                              times (in each copy)
//!   mem-writes N               every normal combination writes memory N
//!                              times (in each copy)
//!   decl NAME BITS             a free bit-vector constant for assume/assert
//!                              (not a name the checker generates)
//!   assume SMT                 a residual assumption (named in the record)
//!   assert SMT                 a property of every normal combination
//!
//! In SMT text, `init.R` and `final.R` are register R before and after the
//! sequence (`init.R.F`, `final.R.F` for field F of a struct register such
//! as PSTATE), `seg.K.NAME` is the symbolic opcode field NAME of
//! instruction K (from 1, `isla-footprint --partial`), `mem.J.addr`,
//! `mem.J.val` are the J-th memory read of the sequence (from 1; address as
//! Isla's model issues it, value as read) and `wmem.J.addr`, `wmem.J.val`
//! the J-th memory write (address, value written). In a copy named C each
//! of these is `C.init.R`, `C.seg.K.NAME`, `C.mem.J.addr` and so on, with K
//! counted within the copy.
//!
//! For every choice of one trace per instruction (of every copy) the
//! checker links the traces (each register read equals the value the
//! copy's sequence last wrote to it, or its `init.` value; reads of a
//! register the sequence never wrote
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

/// One copy of the machine: its name prefix for SMT symbols (`""` for the
/// unnamed copy, `"a_"` for `copy a`) and its instructions.
struct Copy {
    name: String,
    prefix: String,
    insns: Vec<Insn>,
}

#[derive(Default)]
struct Job {
    copies: Vec<Copy>,
    abstracts: BTreeSet<String>,
    writes: BTreeSet<String>,
    no_mem_write: bool,
    flags_preserved: bool,
    mem_reads: Option<usize>,
    mem_writes: Option<usize>,
    decls: Vec<(String, u32)>,
    assumes: Vec<String>,
    asserts: Vec<String>,
}

impl Job {
    fn insns(&self) -> impl Iterator<Item = &Insn> {
        self.copies.iter().flat_map(|c| c.insns.iter())
    }
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
                if !["fallthrough", "ret", "svc", "brk"].contains(&kind) {
                    die(&format!("insn: unknown kind {kind}"));
                }
                let f = dir.join(f);
                if job.copies.is_empty() {
                    job.copies.push(Copy {
                        name: String::new(),
                        prefix: String::new(),
                        insns: Vec::new(),
                    });
                }
                let insn = load_insn(&f.to_string_lossy(), kind);
                job.copies.last_mut().unwrap().insns.push(insn);
            }
            "copy" => {
                let ok = !rest.is_empty()
                    && rest.bytes().all(|c| c.is_ascii_alphanumeric())
                    && rest.as_bytes()[0].is_ascii_lowercase();
                if !ok
                    || job
                        .copies
                        .iter()
                        .any(|c| c.name.is_empty() || c.name == rest)
                {
                    die(&format!(
                        "copy {rest}: bad, repeated or after an unnamed insn"
                    ));
                }
                job.copies.push(Copy {
                    name: rest.to_string(),
                    prefix: format!("{rest}_"),
                    insns: Vec::new(),
                });
            }
            "abstract" => {
                job.abstracts.insert(rest.to_string());
            }
            "writes" => job.writes.extend(rest.split_whitespace().map(String::from)),
            "no-mem-write" => job.no_mem_write = true,
            "flags-preserved" => job.flags_preserved = true,
            "mem-reads" => {
                job.mem_reads = Some(rest.parse().unwrap_or_else(|_| die("mem-reads N")))
            }
            "mem-writes" => {
                job.mem_writes = Some(rest.parse().unwrap_or_else(|_| die("mem-writes N")))
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
    // A `decl` must not name what the checker generates itself: a trace's
    // constants (`t1_v5`, `a_t1_v5`) or an abstract function.
    for (n, _) in &job.decls {
        let s = n.replace('.', "_");
        let base = job
            .copies
            .iter()
            .find_map(|c| {
                s.strip_prefix(c.prefix.as_str())
                    .filter(|_| !c.prefix.is_empty())
            })
            .unwrap_or(&s);
        let trace_const = base
            .strip_prefix('t')
            .and_then(|r| r.split_once('_'))
            .is_some_and(|(k, _)| !k.is_empty() && k.bytes().all(|c| c.is_ascii_digit()));
        if trace_const || s.starts_with("abstract_") {
            die(&format!("decl {n}: a name the checker generates"));
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
/// decoder's search position), `BTypeNext` (the value FEAT_BTI's
/// PSTATE.BTYPE takes at the next instruction, already free as a PSTATE
/// non-flag field) and the `__`-prefixed helpers.
fn is_model_internal(r: &str) -> bool {
    r == "SEE" || r == "BTypeNext" || r.starts_with("__")
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

/// The job's own names in SMT text from `job_smt`: `init_`, `final_`,
/// `seg_`, `mem_` and `wmem_` names (after a copy's prefix) and `decl`
/// constants.
fn job_names<'a>(job: &Job, text: &'a str) -> Vec<&'a str> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .filter(|t| {
            let base = job
                .copies
                .iter()
                .filter(|c| !c.prefix.is_empty())
                .find_map(|c| t.strip_prefix(c.prefix.as_str()))
                .unwrap_or(t);
            ["init_", "final_", "seg_", "mem_", "wmem_"]
                .iter()
                .any(|p| base.starts_with(p))
                || job.decls.iter().any(|(n, _)| n.replace('.', "_") == *t)
        })
        .collect()
}

struct Built {
    smt: String,
    abnormal: Vec<String>,
    violations: Vec<String>,
    /// Memory reads and writes, per copy.
    mem_reads: Vec<usize>,
    mem_writes: Vec<usize>,
    /// Per `fallthrough` instruction: a label and the SMT formula that it
    /// continues at PC + 4.
    falls: Vec<(String, String)>,
}

/// The SMT script for one combination (trace `pick[k]` of instruction k,
/// counting through the copies in order): declarations, path conditions,
/// register links and the job's declarations and assumptions; plus what
/// the combination's events break. With fewer picks than instructions, the
/// script for that prefix: its traces, no `final.` values for the copy it
/// stops in, and only the assumptions whose job names (`init.`, `seg.`,
/// `mem.` and the rest) it declares. Every constraint of a prefix's script
/// is one of the full script's, so an unsatisfiable prefix has no feasible
/// completion.
fn build(job: &Job, pick: &[usize]) -> Built {
    let mut smt = String::new();
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut decl = |smt: &mut String, name: &str, ty: Ty| {
        if declared.insert(name.to_string()) {
            writeln!(smt, "(declare-const {name} {})", ty.smt()).unwrap();
        }
    };
    // Abstract function -> (argument sorts, result sort), declared once
    // for every copy, so equal arguments give equal results everywhere.
    let mut funs: BTreeMap<String, (Vec<Ty>, Ty)> = BTreeMap::new();
    let mut abnormal = Vec::new();
    let mut violations = Vec::new();
    let mut mem_reads = Vec::new();
    let mut mem_writes = Vec::new();
    let mut falls = Vec::new();
    let mut picks = pick.iter();
    let full = pick.len() == job.insns().count();
    for copy in &job.copies {
        if picks.len() == 0 {
            break;
        }
        let p = &copy.prefix;
        let label = if copy.name.is_empty() {
            String::new()
        } else {
            format!("copy {} ", copy.name)
        };
        // Register -> (SMT term of its current value, sort).
        let mut state: BTreeMap<String, Option<(String, Ty)>> = BTreeMap::new();
        let mut mem = 0usize;
        let mut wmem = 0usize;
        let mut partial = false;
        for (k, insn) in copy.insns.iter().enumerate() {
            let Some(&t) = picks.next() else {
                partial = true;
                break;
            };
            let k1 = k + 1;
            for (name, bits) in insn.segs.values() {
                decl(&mut smt, &format!("{p}seg_{k1}_{name}"), Ty::Bv(*bits));
            }
            let pre = format!("{p}t{k1}_");
            let mut vars: HashMap<String, Ty> = HashMap::new();
            let mut wrote_pc = false;
            let mut wrote_exc = false;
            // The PC as the instruction first reads it and as it last
            // writes it (None inside: not expressible).
            let mut pc_in: Option<Option<String>> = None;
            let mut pc_out: Option<Option<String>> = None;
            for ev in &insn.traces[t] {
                let Sx::List(l) = ev else { continue };
                let head = l.first().and_then(atom).unwrap_or("");
                let ren = |a: &str| -> Option<(String, Ty)> {
                    if let Some((n, w)) = insn.segs.get(a) {
                        Some((format!("{p}seg_{k1}_{n}"), Ty::Bv(*w)))
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
                    "abstract-call" => {
                        // `(abstract-call |F| RET ARG...)`: RET, a fresh
                        // constant, is F applied to the arguments.
                        let f = reg_name(&l[1]);
                        let ret = l.get(2).and_then(|r| emit(r, &ren));
                        let args: Option<Vec<(String, Ty)>> =
                            l.iter().skip(3).map(|a| emit(a, &ren)).collect();
                        let (Some((ret, rt)), Some(args)) = (ret, args) else {
                            violations.push(format!(
                                "{label}insn {k1}: abstract call of {f} not expressible"
                            ));
                            continue;
                        };
                        if !job.abstracts.contains(&f) {
                            violations.push(format!(
                                "{label}insn {k1}: abstract call of {f}, which the job does not name"
                            ));
                            continue;
                        }
                        let sig = (args.iter().map(|a| a.1).collect::<Vec<_>>(), rt);
                        match funs.get(&f) {
                            Some(s) if *s != sig => {
                                violations.push(format!(
                                    "{label}insn {k1}: abstract call of {f} with another signature"
                                ));
                                continue;
                            }
                            Some(_) => {}
                            None => {
                                let params: Vec<String> = sig.0.iter().map(|t| t.smt()).collect();
                                writeln!(
                                    smt,
                                    "(declare-fun abstract_{f} ({}) {})",
                                    params.join(" "),
                                    rt.smt()
                                )
                                .unwrap();
                                funs.insert(f.clone(), sig);
                            }
                        }
                        let a: Vec<&str> = args.iter().map(|a| a.0.as_str()).collect();
                        writeln!(smt, "(assert (= {ret} (abstract_{f} {})))", a.join(" ")).unwrap();
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
                                        Sx::List(q) if q.len() == 2 => (
                                            format!("{r}.{}", reg_name(&q[0])),
                                            emit(&q[1], &ren),
                                            atom(&q[1]).is_some_and(is_var),
                                        ),
                                        _ => (format!("{r}.?"), None, false),
                                    })
                                    .collect()
                            }
                            v => vec![(r.clone(), emit(v, &ren), atom(v).is_some_and(is_var))],
                        };
                        if r == "_PC" {
                            let v = locs.first().and_then(|l| l.1.as_ref()).map(|v| v.0.clone());
                            if head == "write-reg" {
                                pc_out = Some(v);
                            } else if pc_in.is_none() {
                                pc_in = Some(v);
                            }
                        }
                        if head == "read-reg" {
                            for (loc, val, var) in locs {
                                let Some((val, ty)) = val else { continue };
                                match state.get(&loc) {
                                    Some(None) => violations.push(format!(
                                        "{label}insn {k1} reads {loc}, which an earlier write left inexpressible"
                                    )),
                                    Some(Some((c, _))) => {
                                        writeln!(smt, "(assert (= {val} {c}))").unwrap()
                                    }
                                    None if var => {
                                        let i = format!("{p}init_{}", loc.replace('.', "_"));
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
                        let exc = insn.kind == "svc" || insn.kind == "brk";
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
                                violations.push(format!("{label}insn {k1} writes PSTATE.{f}"));
                            }
                        } else if is_user_reg(&r) || !exc {
                            // An exception entry writes system state; user
                            // registers never.
                            violations.push(format!("{label}insn {k1} writes {written}"));
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
                            violations
                                .push(format!("{label}insn {k1}: memory read not expressible"));
                            continue;
                        };
                        let bytes: u32 = atom(&l[4]).and_then(|b| b.parse().ok()).unwrap_or(0);
                        if vt != Ty::Bv(8 * bytes) {
                            violations.push(format!(
                                "{label}insn {k1}: memory read of {bytes} bytes has sort {vt:?}"
                            ));
                        }
                        if l.len() != 5 {
                            violations.push(format!("{label}insn {k1}: tagged memory read"));
                        }
                        decl(&mut smt, &format!("{p}mem_{mem}_addr"), at);
                        decl(&mut smt, &format!("{p}mem_{mem}_val"), vt);
                        writeln!(smt, "(assert (= {p}mem_{mem}_addr {a}))").unwrap();
                        writeln!(smt, "(assert (= {p}mem_{mem}_val {v}))").unwrap();
                    }
                    "write-mem" => {
                        // `(write-mem vS KIND ADDR DATA BYTES)`.
                        wmem += 1;
                        if job.no_mem_write {
                            violations.push(format!("{label}insn {k1} writes memory"));
                        }
                        let (Some((a, at)), Some((v, vt))) = (
                            l.get(3).and_then(|x| emit(x, &ren)),
                            l.get(4).and_then(|x| emit(x, &ren)),
                        ) else {
                            violations
                                .push(format!("{label}insn {k1}: memory write not expressible"));
                            continue;
                        };
                        let bytes: u32 = l
                            .get(5)
                            .and_then(atom)
                            .and_then(|b| b.parse().ok())
                            .unwrap_or(0);
                        if vt != Ty::Bv(8 * bytes) {
                            violations.push(format!(
                                "{label}insn {k1}: memory write of {bytes} bytes has sort {vt:?}"
                            ));
                        }
                        if l.len() != 6 {
                            violations.push(format!("{label}insn {k1}: tagged memory write"));
                        }
                        decl(&mut smt, &format!("{p}wmem_{wmem}_addr"), at);
                        decl(&mut smt, &format!("{p}wmem_{wmem}_val"), vt);
                        writeln!(smt, "(assert (= {p}wmem_{wmem}_addr {a}))").unwrap();
                        writeln!(smt, "(assert (= {p}wmem_{wmem}_val {v}))").unwrap();
                    }
                    // Events with no effect on registers or memory: path
                    // bookkeeping, model-level assumptions about registers the
                    // configuration fixed, and tracing.
                    "assume-reg" | "assume" | "branch" | "branch-address" | "cycle" | "call"
                    | "return" | "define-enum" | "instr" | "mark-reg" => {}
                    // Anything else (a tag access, an announced write, an
                    // abstract primop, an event a newer Isla adds) fails
                    // closed.
                    other => violations.push(format!("{label}insn {k1}: unknown event {other}")),
                }
            }
            let normal = match insn.kind.as_str() {
                "svc" | "brk" => wrote_exc && wrote_pc,
                _ => !wrote_exc && wrote_pc,
            };
            if !normal {
                abnormal.push(format!("{label}insn {k1} ({}) trace {}", insn.file, t + 1));
            }
            // A `fallthrough` instruction must continue at its own PC + 4
            // (checked like an assert), so a job's later instructions are
            // the ones that run.
            if insn.kind == "fallthrough" {
                match (pc_in, pc_out) {
                    (Some(Some(i)), Some(Some(o))) => falls.push((
                        format!("{label}insn {k1} falls through"),
                        format!("(= {o} (bvadd {i} #x0000000000000004))"),
                    )),
                    _ => violations.push(format!(
                        "{label}insn {k1}: PC read or write not expressible"
                    )),
                }
            }
        }
        if partial {
            break;
        }
        // `final.R` for every location with an expressible value; a job that
        // asserts about a poisoned one names an undeclared constant, which Z3
        // rejects (a failure, not a pass).
        for (r, v) in &state {
            let Some((v, ty)) = v else { continue };
            let f = format!("{p}final_{}", r.replace('.', "_"));
            decl(&mut smt, &f, *ty);
            writeln!(smt, "(assert (= {f} {v}))").unwrap();
        }
        mem_reads.push(mem);
        mem_writes.push(wmem);
    }
    for (n, b) in &job.decls {
        decl(&mut smt, &n.replace('.', "_"), Ty::Bv(*b));
    }
    for a in &job.assumes {
        let a = job_smt(a);
        if full || job_names(job, &a).iter().all(|n| declared.contains(*n)) {
            writeln!(smt, "(assert {a})").unwrap();
        }
    }
    Built {
        smt,
        abnormal,
        violations,
        mem_reads,
        mem_writes,
        falls,
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
    if job.copies.is_empty() || job.copies.iter().any(|c| c.insns.is_empty()) {
        die("job has no insn lines, or a copy with none");
    }
    let counts: Vec<usize> = job.insns().map(|i| i.traces.len()).collect();
    let total: usize = counts.iter().product();
    // Depth first over the choices of one trace per instruction: `pick` is
    // the current prefix and `next` the trace to try after it. A prefix
    // whose script is unsatisfiable is not extended (build's doc).
    let mut pick: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let (mut feasible, mut checked, mut failed, mut pruned) = (0, 0, 0, 0usize);
    loop {
        if next == counts[pick.len()] {
            match pick.pop() {
                Some(t) => {
                    next = t + 1;
                    continue;
                }
                None => break,
            }
        }
        pick.push(next);
        next = 0;
        if pick.len() < counts.len() {
            if z3(&build(&job, &pick).smt, "") == "unsat" {
                pruned += counts[pick.len()..].iter().product::<usize>();
                next = pick.pop().unwrap() + 1;
            }
            continue;
        }
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
                if job
                    .mem_reads
                    .is_some_and(|n| b.mem_reads.iter().any(|m| *m != n))
                {
                    failed += 1;
                    println!(
                        "FAIL {label}: {:?} memory reads, expected {:?}",
                        b.mem_reads, job.mem_reads
                    );
                }
                if job
                    .mem_writes
                    .is_some_and(|n| b.mem_writes.iter().any(|m| *m != n))
                {
                    failed += 1;
                    println!(
                        "FAIL {label}: {:?} memory writes, expected {:?}",
                        b.mem_writes, job.mem_writes
                    );
                }
                for (what, f) in &b.falls {
                    if z3(&b.smt, &format!("(assert (not {f}))")) != "unsat" {
                        failed += 1;
                        println!("FAIL {label}: not proved that {what}");
                    }
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
        next = pick.pop().unwrap() + 1;
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
        "{}: {total} combinations ({pruned} cut at an infeasible prefix), {feasible} feasible, {checked} assertion checks, {verdict}",
        args[1]
    );
    process::exit(if failed == 0 { 0 } else { 1 });
}
