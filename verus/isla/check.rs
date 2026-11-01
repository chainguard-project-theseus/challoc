//! isla-check: compose per-instruction Isla traces into one instruction
//! sequence and check a job's footprint and value properties with Z3
//! (verus/ISLA.md). Std only; built by verus/isla/run.sh with `rustc -O`.
//!
//!   isla-check JOB
//!   isla-check --same TRACE-FILE TRACE-FILE
//!
//! The second form succeeds when the two files hold the same traces, up to
//! their order (Isla collects paths in parallel), the order of struct
//! fields and the type arguments of union constructor names (for a
//! model's traces from its serialized form against its text form's).
//!
//! The job is a line-oriented file (`#` comments):
//!
//!   arch aarch64|x86_64        the model the traces come from (default
//!                              aarch64; must precede the insn and copy
//!                              lines)
//!   insn FILE KIND [REG]       next instruction: the `isla-footprint -s`
//!                              output in FILE, and what a normal trace of
//!                              it does: `fallthrough` (no exception, and
//!                              the PC provably ends at PC+4), `ret` (no
//!                              exception; the PC as the asserts say), `svc`
//!                              or `brk` (takes an exception to EL1 or EL2),
//!                              `fault` (falls through as `fallthrough`, or
//!                              takes an exception and then writes no user
//!                              register, not even one `writes` lists), or
//!                              `stop` (falls through, or ends at a call of
//!                              a `stop` function having written nothing but
//!                              the model's bookkeeping and no memory); a
//!                              `fault` or `stop` instruction is the last of
//!                              its copy. REG (required under `pc-step` for
//!                              those three kinds): a register every trace
//!                              that falls through must write. On x86_64
//!                              the kinds are `fallthrough LEN` (no model
//!                              fault, and rip provably ends at rip + LEN,
//!                              the instruction's length in bytes), `jump`
//!                              (writes rip, no model fault; the last of its
//!                              copy, and the job must assert `final.rip`,
//!                              the target) and `trap FN` (a model fault,
//!                              with no rip write, whose trace calls FN,
//!                              traced with `--trace-function FN`; the last
//!                              of its copy)
//!   copy NAME                  start a copy of the machine: the `insn`
//!                              lines after it run from their own initial
//!                              state, named `NAME.init.R` and so on (a
//!                              relational job: two copies and assumptions
//!                              equating their inputs); without `copy`
//!                              lines there is one unnamed copy
//!   abstract FN                Isla's `--abstract FN` calls are an
//!                              uninterpreted function of their arguments,
//!                              shared by every copy (the seams.md entry
//!                              of a job naming FN argues it is a
//!                              function); any other abstract call is an
//!                              error
//!   fun FN VALUE               Isla's `--fun-assumption FN(..) => VALUE`:
//!                              the traces may make it (at the model's
//!                              `reset_registers`) and use it in place of
//!                              a call of FN; any other function
//!                              assumption is an error (the seams.md entry
//!                              of a job naming it lists it as a residual)
//!   stop FN ARG...             Isla's `--stop-at FN`: a `stop` instruction's
//!                              trace may end at a call of FN (its last
//!                              event an `abstract-call` of FN with no
//!                              result), whose arguments are proved equal to
//!                              the ARGs (SMT atoms); a job with a `stop`
//!                              line needs a feasible combination that stops
//!   writes REG...              the registers (or REG.FIELD, or REG.I for
//!                              element I of an x86 vector register) the
//!                              sequence may write besides the PC and, on
//!                              aarch64, PSTATE's non-flag fields (anything
//!                              else written is an error; the system
//!                              registers an exception entry writes are
//!                              allowed only on the exception paths of
//!                              `svc`, `brk` and `fault`)
//!   pc-step                    the traces end before the model's PC update
//!                              (for a model whose footprint function
//!                              decodes and executes only), so after each
//!                              instruction the checker applies it as the
//!                              model's `__InstructionExecute` does: unless
//!                              the trace set `__BranchTaken` (which every
//!                              write of `_PC` must come with), `_PC`
//!                              becomes `_PC + 4`. That footprint function
//!                              also swallows an UNDEFINED abort without a
//!                              trace event, hence the `insn` line's REG
//!   no-mem-write               no trace writes memory
//!   flags-preserved            no trace writes PSTATE.{N,Z,C,V} (aarch64)
//!                              or rflags (x86_64)
//!   mem-reads N                every normal combination reads memory N
//!                              times (in each copy)
//!   mem-writes N               every normal combination writes memory N
//!                              times (in each copy)
//!   decl NAME BITS             a free bit-vector constant for assume/assert
//!                              (not a trace's own constant or an abstract
//!                              function; `decl init.R` or `decl mem.J.val`
//!                              names a value some combinations leave
//!                              undefined, so that assumptions over it hold
//!                              in every combination)
//!   assume SMT                 a residual assumption (named in verus/isla/seams.md)
//!   assert SMT                 a property of every normal combination
//!   assert-exc SMT             a property of every normal combination in
//!                              which a `fault` instruction took its
//!                              exception (a job with such a feasible
//!                              combination needs an `assert-exc`, and a
//!                              job with an `assert-exc` needs one)
//!
//! In SMT text, `init.R` and `final.R` are register R before and after the
//! sequence (`init.R.F`, `final.R.F` for field F of a struct register such
//! as PSTATE, `init.R.I` for element I of an x86 vector register such as
//! the model's `msrs`), `seg.K.NAME` is the symbolic opcode field NAME of
//! instruction K (from 1, `isla-footprint --partial`), `mem.J.addr`,
//! `mem.J.val` are the J-th memory read of the sequence (from 1; address as
//! Isla's model issues it, value as read) and `wmem.J.addr`, `wmem.J.val`
//! the J-th memory write (address, value written), and `stop.K` (Bool) is
//! true when `stop` instruction K ended at its `stop` call. In a copy named
//! C each of these is `C.init.R`, `C.seg.K.NAME`, `C.mem.J.addr` and so on,
//! with K counted within the copy.
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
//! trace (an exception or a stop where none is expected, or no PC write) is
//! an error. For a feasible normal one, each `assert` must follow (its
//! negation unsat, and an assert naming a value the combination leaves
//! undefined fails), the footprint lines must hold, and every event must be
//! one the checker knows. A job with no feasible combination fails.
//!
//! On x86_64 the traces come from the Isla entry in verus/isla/x86/ (the
//! sail-x86-from-acl2 model): its own registers (`isla_*`) are the
//! entry's bookkeeping, never linked between instructions, and a write of
//! `isla_fault` marks a path where the model raised a fault or a model
//! error (abnormal). A write of a whole vector or struct register counts
//! as a write of each element or field whose new value is not the very
//! term the location held, so an element written back unchanged is not a
//! write and anything the checker cannot tell apart is one. `pc-step` and
//! `stop` are aarch64 only. Exit
//! status 0 when everything holds, 1 when something fails, 2 on a usage
//! error or a Z3 answer other than sat/unsat (an undeclared name included).
//!
//! The children of a satisfiable prefix are judged in parallel, on up to
//! ISLA_CHECK_JOBS threads (default: the CPUs), each its own Z3 run; the
//! properties of a feasible combination are one Z3 run, each in its own
//! `(push)`/`(pop)` scope. The results and the report are in the order of
//! the combinations.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// The register a trace that falls through must write (`pc-step`).
    completes: Option<String>,
    /// x86_64: a `fallthrough` instruction's length in bytes.
    len: Option<u64>,
    /// x86_64: the function a `trap` instruction's fault goes through.
    trap_fn: Option<String>,
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
        completes: None,
        len: None,
        trap_fn: None,
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

#[derive(Clone, Copy, Default, PartialEq)]
enum Arch {
    #[default]
    Aarch64,
    X86_64,
}

#[derive(Default)]
struct Job {
    arch: Arch,
    copies: Vec<Copy>,
    abstracts: BTreeSet<String>,
    /// `fun` lines: Isla function assumptions (function, result) the
    /// traces may use.
    funs: BTreeSet<(String, String)>,
    /// `stop` functions -> the arguments their call must have.
    stops: BTreeMap<String, Vec<String>>,
    writes: BTreeSet<String>,
    no_mem_write: bool,
    flags_preserved: bool,
    pc_step: bool,
    mem_reads: Option<usize>,
    mem_writes: Option<usize>,
    decls: Vec<(String, u32)>,
    assumes: Vec<String>,
    asserts: Vec<String>,
    assert_excs: Vec<String>,
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
            "arch" => {
                if !job.copies.is_empty() {
                    die("arch: must precede the insn and copy lines");
                }
                job.arch = match rest {
                    "aarch64" => Arch::Aarch64,
                    "x86_64" => Arch::X86_64,
                    _ => die(&format!("arch: unknown {rest}")),
                }
            }
            "insn" => {
                let mut w = rest.split_whitespace();
                let f = w.next().unwrap_or_else(|| die("insn: no file"));
                let kind = w.next().unwrap_or("fallthrough");
                let known: &[&str] = match job.arch {
                    Arch::Aarch64 => &["fallthrough", "ret", "svc", "brk", "fault", "stop"],
                    Arch::X86_64 => &["jump", "fallthrough", "trap"],
                };
                if !known.contains(&kind) {
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
                let mut insn = load_insn(&f.to_string_lossy(), kind);
                let third = w.next().map(String::from);
                match (job.arch, kind) {
                    (Arch::Aarch64, _) => insn.completes = third,
                    (Arch::X86_64, "fallthrough") => {
                        insn.len = Some(
                            third
                                .and_then(|n| n.parse().ok())
                                .unwrap_or_else(|| die("insn FILE fallthrough LEN: no length")),
                        )
                    }
                    (Arch::X86_64, "trap") => {
                        insn.trap_fn =
                            Some(third.unwrap_or_else(|| die("insn FILE trap FN: no function")))
                    }
                    _ if third.is_some() => die("insn FILE jump: too many words"),
                    _ => {}
                }
                if w.next().is_some() {
                    die("insn FILE KIND [REG]: too many words");
                }
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
            "fun" => {
                let mut w = rest.split_whitespace();
                let (Some(f), Some(v), None) = (w.next(), w.next(), w.next()) else {
                    die("fun FN VALUE")
                };
                job.funs.insert((f.to_string(), v.to_string()));
            }
            "abstract" => {
                job.abstracts.insert(rest.to_string());
            }
            "stop" => {
                let mut w = rest.split_whitespace();
                let f = w.next().unwrap_or_else(|| die("stop FN ARG..."));
                job.stops
                    .insert(f.to_string(), w.map(String::from).collect());
            }
            "writes" => job.writes.extend(rest.split_whitespace().map(String::from)),
            "no-mem-write" => job.no_mem_write = true,
            "flags-preserved" => job.flags_preserved = true,
            "pc-step" => job.pc_step = true,
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
            "assert-exc" => job.assert_excs.push(rest.to_string()),
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
        if trace_const || s.starts_with("abstract_") || base.starts_with("stop_") {
            die(&format!("decl {n}: a name the checker generates"));
        }
    }
    // A `stop` instruction ends its copy's sequence when it stops, and a
    // `fault` one when it takes its exception: nothing may follow either.
    for c in &job.copies {
        if c.insns
            .iter()
            .rev()
            .skip(1)
            .any(|i| ["stop", "fault", "trap"].contains(&i.kind.as_str()))
        {
            die("a `stop`, `fault` or `trap` instruction that is not the last of its copy");
        }
        // Under `pc-step` a trace that ends without writing anything also
        // looks like a fall-through: the model's footprint function
        // swallows an UNDEFINED or similar exception without a trace event.
        // So each such instruction names a register it writes when it
        // completes.
        if job.pc_step
            && c.insns.iter().any(|i| {
                ["fallthrough", "fault", "stop"].contains(&i.kind.as_str()) && i.completes.is_none()
            })
        {
            die("pc-step: an insn line without the register its completion writes");
        }
        // An x86 `jump` is any write of rip, so it ends its copy, and where
        // the sequence goes is what the job asserts.
        if job.arch == Arch::X86_64 && c.insns.iter().rev().skip(1).any(|i| i.kind == "jump") {
            die("x86_64: a `jump` instruction that is not the last of its copy");
        }
        let rip = format!("{}final_rip", c.prefix);
        if job.arch == Arch::X86_64
            && c.insns.last().is_some_and(|i| i.kind == "jump")
            && !job
                .asserts
                .iter()
                .any(|a| job_names(&job, &job_smt(a)).contains(&rip.as_str()))
        {
            die("x86_64: a copy whose final.rip no assert names");
        }
    }
    if job.arch == Arch::X86_64 && (job.pc_step || !job.stops.is_empty()) {
        die("x86_64: pc-step and stop are aarch64 only");
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
/// The other registers an exception entry may write (besides the PC,
/// PSTATE and the model's bookkeeping): the fault address registers, the
/// EL3 return state, and the model's record of the branch type. Any other
/// write on an exception path is an error.
const EXC_ENTRY_REGS: [&str; 10] = [
    "FAR_EL1",
    "FAR_EL2",
    "FAR_EL3",
    "PFAR_EL1",
    "PFAR_EL2",
    "PFAR_EL3",
    "HPFAR_EL2",
    "ELR_EL3",
    "SPSR_EL3",
    "Branchtypetaken",
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
        || r == "_FFR"
        || r == "TPIDR_EL0"
        || r == "TPIDRRO_EL0"
        || r == "TPIDR2_EL0"
        || r == "GCSPR_EL0"
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
/// `seg_`, `mem_`, `wmem_` and `stop_` names (after a copy's prefix) and
/// `decl` constants.
fn job_names<'a>(job: &Job, text: &'a str) -> Vec<&'a str> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .filter(|t| {
            let base = job
                .copies
                .iter()
                .filter(|c| !c.prefix.is_empty())
                .find_map(|c| t.strip_prefix(c.prefix.as_str()))
                .unwrap_or(t);
            ["init_", "final_", "seg_", "mem_", "wmem_", "stop_"]
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
    /// What each instruction's trace must show beyond the asserts, as a
    /// label and an SMT formula: that a falling-through instruction
    /// continues at PC + 4, and that a `stop` call has the job's arguments.
    falls: Vec<(String, String)>,
    /// A `fault` instruction took its exception.
    exc_taken: bool,
    /// A `stop` instruction ended at its `stop` call.
    stopped: bool,
    /// The SMT names the script declares.
    declared: BTreeSet<String>,
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
    let mut exc_taken = false;
    let mut any_stopped = false;
    let mut picks = pick.iter();
    let full = pick.len() == job.insns().count();
    let pc_reg = if job.arch == Arch::X86_64 {
        "rip"
    } else {
        "_PC"
    };
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
        // A trace variable a register read bound -> the term it was linked to
        // (x86_64 uses it to tell an element written back unchanged).
        let mut alias: HashMap<String, String> = HashMap::new();
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
            let mut faulted = false;
            // A call of the `trap` instruction's function (x86_64).
            let mut called = false;
            // The PC as the instruction first reads it and as it last
            // writes it (None inside: not expressible).
            let mut pc_in: Option<Option<String>> = None;
            let mut pc_out: Option<Option<String>> = None;
            // A trace Isla ended at a `--stop-at` call: its last event.
            let trace = &insn.traces[t];
            let stop_at = trace
                .len()
                .checked_sub(1)
                .filter(|&i| is_stop_call(&trace[i]));
            let stopped = stop_at.is_some();
            // Writes that only an exception path may make, and the user
            // registers the job lets the instruction write, which its
            // exception path may not: judged once the whole trace is seen.
            let mut sys_writes: Vec<String> = Vec::new();
            let mut user_writes: Vec<String> = Vec::new();
            // The last value the trace wrote to `__BranchTaken` (`pc-step`).
            let mut branch_taken: Option<bool> = None;
            // The registers the trace writes (`pc-step`'s completion check).
            let mut wrote_regs: BTreeSet<String> = BTreeSet::new();
            for (i, ev) in trace.iter().enumerate() {
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
                    "abstract-call" if stop_at == Some(i) => {
                        // `(abstract-call |F| (_ poison) ARG...)`, the call
                        // `--stop-at F` ended the trace at.
                        let f = reg_name(&l[1]);
                        let Some(want) = job.stops.get(&f) else {
                            violations.push(format!(
                                "{label}insn {k1} stops at {f}, which the job does not name"
                            ));
                            continue;
                        };
                        if l.len() - 3 != want.len() {
                            violations.push(format!(
                                "{label}insn {k1}: {f} called with {} arguments, the job names {}",
                                l.len() - 3,
                                want.len()
                            ));
                            continue;
                        }
                        for (j, (a, w)) in l[3..].iter().zip(want).enumerate() {
                            match emit(a, &ren) {
                                Some((a, _)) => falls.push((
                                    format!(
                                        "{label}insn {k1} calls {f} with argument {} = {w}",
                                        j + 1
                                    ),
                                    format!("(= {a} {})", job_smt(w)),
                                )),
                                None => violations.push(format!(
                                    "{label}insn {k1}: argument {} of {f} not expressible",
                                    j + 1
                                )),
                            }
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
                            // A vector register (x86_64): element I is the
                            // location `R.I`, elements listed from index 0.
                            Sx::List(s)
                                if job.arch == Arch::X86_64
                                    && s.len() > 2
                                    && atom(&s[0]) == Some("_")
                                    && atom(&s[1]) == Some("vec") =>
                            {
                                s[2..]
                                    .iter()
                                    .enumerate()
                                    .map(|(i, e)| {
                                        (
                                            format!("{r}.{i}"),
                                            emit(e, &ren),
                                            atom(e).is_some_and(is_var),
                                        )
                                    })
                                    .collect()
                            }
                            // A plain value: the register, or the one field a
                            // write's accessor names (an accessor the checker
                            // cannot read poisons the whole register, below).
                            v => {
                                let loc = match (head, &field) {
                                    ("write-reg", Some(f)) => format!("{r}.{f}"),
                                    _ => r.clone(),
                                };
                                vec![(loc, emit(v, &ren), atom(v).is_some_and(is_var))]
                            }
                        };
                        if head == "write-reg" && field.as_deref() == Some("?") {
                            violations.push(format!(
                                "{label}insn {k1} writes {r} through an accessor the checker cannot read"
                            ));
                        }
                        if job.arch == Arch::X86_64 && r.starts_with("isla_") {
                            // The x86 entry's bookkeeping: the opcode, the
                            // fetch base, the fault marker. Local to one
                            // instruction's trace, whose own definitions
                            // relate them to rip.
                            if head == "write-reg" && r == "isla_fault" {
                                faulted = true;
                            }
                            continue;
                        }
                        // The PC as the instruction first reads and last writes it.
                        if r == pc_reg {
                            let v = locs.first().and_then(|l| l.1.as_ref()).map(|v| v.0.clone());
                            if head == "write-reg" {
                                pc_out = Some(v);
                            } else if pc_in.is_none() {
                                pc_in = Some(v);
                            }
                        }
                        if head == "write-reg" && job.arch == Arch::X86_64 {
                            if r == "rip" {
                                wrote_pc = true;
                            }
                            for (loc, val, _) in &locs {
                                let unchanged = match (state.get(loc), val) {
                                    (Some(Some((c, _))), Some((v, _))) => {
                                        c == v || alias.get(v) == Some(c)
                                    }
                                    _ => false,
                                };
                                if r == "rip" || unchanged {
                                    continue;
                                }
                                if (job.flags_preserved && r == "rflags")
                                    || (!job.writes.contains(&r) && !job.writes.contains(loc))
                                {
                                    violations.push(format!("{label}insn {k1} writes {loc}"));
                                }
                            }
                            for (loc, val, _) in locs {
                                state.insert(loc, val);
                            }
                            continue;
                        }
                        if head == "read-reg" {
                            for (loc, val, var) in locs {
                                let Some((val, ty)) = val else { continue };
                                match state.get(&loc) {
                                    Some(None) => violations.push(format!(
                                        "{label}insn {k1} reads {loc}, which an earlier write left inexpressible"
                                    )),
                                    Some(Some((c, _))) => {
                                        writeln!(smt, "(assert (= {val} {c}))").unwrap();
                                        if var {
                                            alias.insert(val, c.clone());
                                        }
                                    }
                                    None if var => {
                                        let i = format!("{p}init_{}", loc.replace('.', "_"));
                                        decl(&mut smt, &i, ty);
                                        writeln!(smt, "(assert (= {val} {i}))").unwrap();
                                        alias.insert(val, i.clone());
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
                        wrote_regs.insert(r.clone());
                        if r == "__BranchTaken" {
                            branch_taken = match atom(&l[3]) {
                                Some("true") => Some(true),
                                Some("false") => Some(false),
                                _ => {
                                    violations.push(format!(
                                        "{label}insn {k1}: __BranchTaken written with a symbolic value"
                                    ));
                                    None
                                }
                            };
                        }
                        let exc = ["svc", "brk", "fault"].contains(&insn.kind.as_str());
                        if stopped && !is_model_internal(&r) {
                            violations.push(format!(
                                "{label}insn {k1} writes {written} before its stop call"
                            ));
                        } else if r == "_PC" {
                            wrote_pc = true;
                        } else if EXC_REGS.contains(&r.as_str()) {
                            wrote_exc = true;
                        } else if is_model_internal(&r) {
                        } else if job.writes.contains(&r) || job.writes.contains(&written) {
                            if is_user_reg(&r) {
                                user_writes.push(written);
                            }
                        } else if r == "PSTATE" {
                            // PSTATE's other fields are bookkeeping (BTYPE) or,
                            // in an exception entry, the new EL and masks.
                            let f = field.as_deref().unwrap_or("?");
                            if job.flags_preserved && (FLAGS.contains(&f) || f == "?") {
                                violations.push(format!("{label}insn {k1} writes PSTATE.{f}"));
                            }
                        } else if is_user_reg(&r) || !exc || !EXC_ENTRY_REGS.contains(&r.as_str()) {
                            // An exception entry writes its own system registers;
                            // user registers and any other register never.
                            violations.push(format!("{label}insn {k1} writes {written}"));
                        } else {
                            sys_writes.push(written);
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
                        if stopped {
                            violations.push(format!(
                                "{label}insn {k1} writes memory before its stop call"
                            ));
                        }
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
                    "call" if insn.trap_fn.as_deref() == l.get(1).map(reg_name).as_deref() => {
                        called = true;
                    }
                    // An Isla function assumption (`--fun-assumption F(..) => V`):
                    // made at the model's reset, then used in place of a call of
                    // F. Only the job's, which verus/isla/seams.md names as residuals.
                    "function-assumption" | "use-function-assumption" => {
                        let f = l.get(1).map(reg_name).unwrap_or_default();
                        let v = l.get(2).and_then(atom).unwrap_or("").to_string();
                        if !job.funs.contains(&(f.clone(), v.clone())) {
                            violations.push(format!(
                                "{label}insn {k1}: function assumption {f} => {v}, which the job does not name"
                            ));
                        }
                    }
                    "assume-reg" | "assume" | "branch" | "branch-address" | "cycle" | "call"
                    | "return" | "define-enum" | "instr" | "mark-reg" => {}
                    // The Armv9.4-A model announces an exception entry (for
                    // Isla's memory-model tools) with the primop
                    // `sail_take_exception`, which changes no register or
                    // memory: the entry's writes are their own events.
                    "abstract-primop"
                        if l.get(1).map(reg_name).as_deref() == Some("sail_take_exception") => {}
                    // Anything else (a tag access, an announced write, an
                    // abstract primop, an event a newer Isla adds) fails
                    // closed.
                    other => violations.push(format!("{label}insn {k1}: unknown event {other}")),
                }
            }
            // The model's PC update after the instruction (`pc-step`): a
            // stopped trace has no "after".
            if job.pc_step && !stopped {
                if branch_taken == Some(true) {
                    if !wrote_pc {
                        violations.push(format!(
                            "{label}insn {k1} sets __BranchTaken and writes no PC"
                        ));
                    }
                } else if wrote_pc {
                    violations.push(format!(
                        "{label}insn {k1} writes the PC without setting __BranchTaken"
                    ));
                } else {
                    // The PC: as last written or linked, else the literal the
                    // configuration fixed it to, else a fresh initial value.
                    let cur = match (state.get("_PC"), &pc_in) {
                        (Some(Some((c, _))), _) => Some(c.clone()),
                        (Some(None), _) => None,
                        (None, Some(lit)) => lit.clone(),
                        (None, None) => {
                            let i = format!("{p}init__PC");
                            decl(&mut smt, &i, Ty::Bv(64));
                            state.insert("_PC".to_string(), Some((i.clone(), Ty::Bv(64))));
                            Some(i)
                        }
                    };
                    match cur {
                        Some(c) => {
                            let n = format!("(bvadd {c} #x0000000000000004)");
                            state.insert("_PC".to_string(), Some((n.clone(), Ty::Bv(64))));
                            wrote_pc = true;
                            pc_in.get_or_insert(Some(c));
                            pc_out = Some(Some(n));
                        }
                        None => violations.push(format!(
                            "{label}insn {k1}: the PC an earlier write left inexpressible"
                        )),
                    }
                }
            }
            let normal = match (job.arch, insn.kind.as_str()) {
                (Arch::X86_64, "trap") => faulted && called && !wrote_pc,
                (Arch::X86_64, _) => !faulted && wrote_pc,
                (_, "svc" | "brk") => !stopped && wrote_exc && wrote_pc,
                (_, "fault") => !stopped && wrote_pc,
                (_, "stop") if stopped => !wrote_exc && !wrote_pc,
                _ => !stopped && !wrote_exc && wrote_pc,
            };
            if !normal {
                // What makes it abnormal, for the report (and the controls).
                let why = if insn.kind == "trap" {
                    match &insn.trap_fn {
                        Some(f) if faulted && !called => {
                            format!("a model fault with no call of {f}")
                        }
                        _ if faulted => "a model fault after a rip write".to_string(),
                        _ => "no model fault".to_string(),
                    }
                } else if faulted {
                    "a model fault".to_string()
                } else if stopped {
                    "a stop call".to_string()
                } else if wrote_exc {
                    "an exception".to_string()
                } else if !wrote_pc {
                    "no PC write".to_string()
                } else {
                    "no exception".to_string()
                };
                abnormal.push(format!(
                    "{label}insn {k1} ({}) trace {}: {why}",
                    insn.file,
                    t + 1
                ));
            }
            if wrote_exc {
                // An exception path writes no user register at all.
                for w in &user_writes {
                    violations.push(format!(
                        "{label}insn {k1} writes {w} and takes an exception"
                    ));
                }
                exc_taken |= insn.kind == "fault";
            } else {
                for w in &sys_writes {
                    violations.push(format!("{label}insn {k1} writes {w}"));
                }
            }
            if insn.kind == "stop" {
                let s = format!("{p}stop_{k1}");
                decl(&mut smt, &s, Ty::Bool);
                writeln!(smt, "(assert (= {s} {stopped}))").unwrap();
                any_stopped |= stopped;
            }
            // A `fallthrough` instruction (and a `fault` or `stop` one that
            // neither took its exception nor stopped) must continue at its
            // own PC + 4 (checked like an assert), so a job's later
            // instructions are the ones that run.
            let falls_through = match insn.kind.as_str() {
                "fallthrough" => true,
                "fault" => !wrote_exc,
                "stop" => !stopped,
                _ => false,
            };
            if falls_through && job.pc_step {
                if let Some(r) = &insn.completes {
                    if !wrote_regs.contains(r) {
                        violations.push(format!(
                            "{label}insn {k1} falls through without writing {r} (an aborted instruction?)"
                        ));
                    }
                }
            }
            if falls_through {
                match (pc_in, pc_out) {
                    (Some(Some(i)), Some(Some(o))) => falls.push((
                        format!("{label}insn {k1} falls through"),
                        format!("(= {o} (bvadd {i} (_ bv{} 64)))", insn.len.unwrap_or(4)),
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
        exc_taken,
        stopped: any_stopped,
        declared,
    }
}

/// Z3's answers to `smt` followed by each of `extras` in turn, each in its
/// own `(push)`/`(pop)` scope with a `(check-sat)`: one run of Z3 for all
/// of them (`smt` is a combination's whole script, large for long jobs).
fn z3_each(smt: &str, extras: &[String]) -> Vec<String> {
    static RUN: AtomicUsize = AtomicUsize::new(0);
    let z3 = env::var("Z3").unwrap_or_else(|_| "z3".into());
    let n = RUN.fetch_add(1, Ordering::Relaxed);
    let path = env::temp_dir().join(format!("isla-check-{}-{n}.smt2", process::id()));
    let mut script = smt.to_string();
    for e in extras {
        writeln!(script, "(push)\n{e}\n(check-sat)\n(pop)").unwrap();
    }
    fs::write(&path, script).unwrap_or_else(|e| die(&format!("{e}")));
    let out = Command::new(&z3)
        .arg("-smt2")
        .arg(&path)
        .output()
        .unwrap_or_else(|e| die(&format!("{z3}: {e}")));
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    let answers: Vec<String> = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    if answers.len() != extras.len() || answers.iter().any(|a| a != "sat" && a != "unsat") {
        let keep = env::temp_dir().join(format!("isla-check-failed-{}.smt2", process::id()));
        let _ = fs::copy(&path, &keep);
        die(&format!(
            "z3 said: {} (query kept in {})",
            s.trim(),
            keep.display()
        ));
    }
    let _ = fs::remove_file(&path);
    answers
}

/// Z3's answer to `smt`, then `extra`, then `(check-sat)`.
fn z3(smt: &str, extra: &str) -> String {
    z3_each(smt, &[extra.to_string()]).remove(0)
}

/// `f` applied to `0..n` on up to ISLA_CHECK_JOBS threads (default: the
/// CPUs), the results in order.
fn par_map<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let jobs = env::var("ISLA_CHECK_JOBS")
        .ok()
        .and_then(|j| j.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |p| p.get()))
        .clamp(1, n.max(1));
    let mut out: Vec<Option<T>> = (0..n).map(|_| None).collect();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..jobs)
            .map(|j| {
                let f = &f;
                s.spawn(move || (j..n).step_by(jobs).map(|i| (i, f(i))).collect::<Vec<_>>())
            })
            .collect();
        for h in handles {
            for (i, r) in h
                .join()
                .unwrap_or_else(|_| die("a checker thread panicked"))
            {
                out[i] = Some(r);
            }
        }
    });
    out.into_iter().map(|r| r.unwrap()).collect()
}

/// A trace up to what does not change its meaning: the
/// order Isla prints a struct value's fields in (a hash map's, which
/// differs from run to run) and the type arguments it appends to union
/// constructor names in a serialized model (`|None<b>|` for `|None|`).
fn canonical(s: &Sx) -> Sx {
    match s {
        Sx::Atom(a) if a.starts_with('|') => {
            let mut out = String::new();
            let mut depth = 0;
            for c in a.chars() {
                match c {
                    '<' => depth += 1,
                    '>' if depth > 0 => depth -= 1,
                    _ if depth == 0 => out.push(c),
                    _ => {}
                }
            }
            Sx::Atom(out)
        }
        Sx::Atom(a) => Sx::Atom(a.clone()),
        Sx::List(l) => {
            let mut l: Vec<Sx> = l.iter().map(canonical).collect();
            if l.len() > 2 && atom(&l[0]) == Some("_") && atom(&l[1]) == Some("struct") {
                l[2..].sort_by_key(|f| format!("{f:?}"));
            }
            Sx::List(l)
        }
    }
}

/// `(abstract-call |F| (_ poison) ARG...)`: the call Isla's `--stop-at F`
/// ends a trace with (no result: the execution stops there).
fn is_stop_call(ev: &Sx) -> bool {
    let Sx::List(l) = ev else { return false };
    l.first().and_then(atom) == Some("abstract-call")
        && matches!(l.get(2), Some(Sx::List(r))
            if r.len() == 2 && atom(&r[0]) == Some("_") && atom(&r[1]) == Some("poison"))
}

/// The search over combinations and its tallies.
struct Search<'a> {
    job: &'a Job,
    /// Traces per instruction, through the copies in order.
    counts: Vec<usize>,
    feasible: usize,
    checked: usize,
    failed: usize,
    pruned: usize,
    feasible_exc: usize,
    feasible_stop: usize,
}

impl Search<'_> {
    /// Extend the satisfiable prefix `pick` by each trace of the next
    /// instruction: a satisfiable prefix is extended in turn, a full
    /// combination is judged.
    fn visit(&mut self, pick: &mut Vec<usize>) {
        let job = self.job;
        let level = pick.len();
        let sat: Vec<bool> = par_map(self.counts[level], |t| {
            let mut p = pick.clone();
            p.push(t);
            z3(&build(job, &p).smt, "") == "sat"
        });
        for (t, sat) in sat.into_iter().enumerate() {
            pick.push(t);
            if level + 1 < self.counts.len() {
                if sat {
                    self.visit(pick);
                } else {
                    self.pruned += self.counts[level + 1..].iter().product::<usize>();
                }
            } else {
                self.judge(pick, sat);
            }
            pick.pop();
        }
    }

    /// A full combination, `sat` when its script is satisfiable.
    fn judge(&mut self, pick: &[usize], sat: bool) {
        let job = self.job;
        let b = build(job, pick);
        if env::var_os("ISLA_CHECK_DUMP").is_some() {
            println!("; combination {pick:?}\n{}", b.smt);
        }
        if !sat {
            return;
        }
        self.feasible += 1;
        let label = format!(
            "combination {:?}",
            pick.iter().map(|p| p + 1).collect::<Vec<_>>()
        );
        if !b.abnormal.is_empty() {
            self.failed += 1;
            println!(
                "FAIL {label}: feasible abnormal path: {}",
                b.abnormal.join(", ")
            );
            return;
        }
        for v in &b.violations {
            self.failed += 1;
            println!("FAIL {label}: {v}");
        }
        if job
            .mem_reads
            .is_some_and(|n| b.mem_reads.iter().any(|m| *m != n))
        {
            self.failed += 1;
            println!(
                "FAIL {label}: {:?} memory reads, expected {:?}",
                b.mem_reads, job.mem_reads
            );
        }
        if job
            .mem_writes
            .is_some_and(|n| b.mem_writes.iter().any(|m| *m != n))
        {
            self.failed += 1;
            println!(
                "FAIL {label}: {:?} memory writes, expected {:?}",
                b.mem_writes, job.mem_writes
            );
        }
        // Each property must follow: its negation unsatisfiable. One Z3
        // run for all of them; the reports in the order they are listed.
        let excs: &[String] = if b.exc_taken { &job.assert_excs } else { &[] };
        let mut what: Vec<String> = Vec::new();
        let mut negs: Vec<String> = Vec::new();
        for (w, f) in &b.falls {
            what.push(format!("not proved that {w}"));
            negs.push(format!("(assert (not {f}))"));
        }
        for a in job.asserts.iter().chain(excs) {
            self.checked += 1;
            // A name this combination leaves undefined (a `final.R` of a
            // register no trace here writes or reads) is a failure of the
            // assert, not a Z3 error.
            let a_smt = job_smt(a);
            let undefined: Vec<&str> = job_names(job, &a_smt)
                .into_iter()
                .filter(|n| !b.declared.contains(*n))
                .collect();
            if undefined.is_empty() {
                what.push(format!("assert {a}"));
                negs.push(format!("(assert (not {a_smt}))"));
            } else {
                what.push(format!(
                    "assert {a}: {} undefined in this combination",
                    undefined.join(", ")
                ));
                negs.push(String::new());
            }
        }
        let asked: Vec<String> = negs.iter().filter(|n| !n.is_empty()).cloned().collect();
        let mut answers = if asked.is_empty() {
            Vec::new().into_iter()
        } else {
            z3_each(&b.smt, &asked).into_iter()
        };
        for (w, n) in what.iter().zip(&negs) {
            let proved = !n.is_empty() && answers.next().as_deref() == Some("unsat");
            if !proved {
                self.failed += 1;
                println!("FAIL {label}: {w}");
            }
        }
        if b.exc_taken && job.assert_excs.is_empty() {
            self.failed += 1;
            println!("FAIL {label}: an exception path and no assert-exc for it");
        }
        self.feasible_exc += b.exc_taken as usize;
        self.feasible_stop += b.stopped as usize;
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    // `isla-check --same A B`: the two trace files hold the same traces,
    // in any order.
    if args.len() == 4 && args[1] == "--same" {
        let read = |f: &str| {
            let src = fs::read_to_string(f).unwrap_or_else(|e| die(&format!("{f}: {e}")));
            let mut t: Vec<String> = parse_all(&src)
                .iter()
                .map(|s| format!("{:?}", canonical(s)))
                .collect();
            t.sort();
            t
        };
        let (a, b) = (read(&args[2]), read(&args[3]));
        if a.is_empty() || format!("{a:?}") != format!("{b:?}") {
            println!("FAIL: {} and {} differ", args[2], args[3]);
            process::exit(1);
        }
        println!("{} and {}: the same {} traces", args[2], args[3], a.len());
        process::exit(0);
    }
    if args.len() != 2 {
        die("usage: isla-check JOB, or isla-check --same TRACE-FILE TRACE-FILE");
    }
    let job = load_job(&args[1]);
    if job.copies.is_empty() || job.copies.iter().any(|c| c.insns.is_empty()) {
        die("job has no insn lines, or a copy with none");
    }
    let counts: Vec<usize> = job.insns().map(|i| i.traces.len()).collect();
    let total: usize = counts.iter().product();
    // Depth first over the choices of one trace per instruction. A prefix
    // whose script is unsatisfiable is not extended (build's doc); the
    // children of a prefix are judged in parallel, in order.
    let mut s = Search {
        job: &job,
        counts,
        feasible: 0,
        checked: 0,
        failed: 0,
        pruned: 0,
        feasible_exc: 0,
        feasible_stop: 0,
    };
    s.visit(&mut Vec::new());
    let Search {
        feasible,
        checked,
        mut failed,
        pruned,
        feasible_exc,
        feasible_stop,
        ..
    } = s;
    if feasible == 0 {
        failed += 1;
        println!("FAIL: no feasible combination (the assumptions contradict the traces)");
    }
    // The paths a job's `assert-exc` or `stop` lines describe must exist.
    if !job.assert_excs.is_empty() && feasible_exc == 0 {
        failed += 1;
        println!("FAIL: assert-exc lines, and no feasible exception path");
    }
    if !job.stops.is_empty() && feasible_stop == 0 {
        failed += 1;
        println!("FAIL: stop lines, and no feasible combination that stops");
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
