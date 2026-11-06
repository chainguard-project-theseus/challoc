"""The executor: `cb run <suite>` on the machine it is started on.

Steps (the run protocol, inside one host):
  1. build or locate every variant's library, compile cb's tools
  2. host capture (host.json) and the challoc features line
  3. quiet gate (load average)
  4. blocks: the sentinel, then every row in the block's shuffled order,
     each variant at its planned positions; one fresh process per run under
     tools/rt (RT_JSON, counters where a PMU is reachable), a random
     0..4 KiB environment pad, ASLR on. Every run is appended to
     samples.jsonl as it finishes, every block's health to blocks.jsonl.
  5. the analysis (report.py): summary.json, report.md, the exit code.

Output: target/results/cb/<run>/ by default (the Makefile bind-mounts
target/results from the host, so a run in the dev container is readable
on the host): plan.json, manifest.json, host.json, samples.jsonl,
blocks.jsonl, summary.json, report.md, logs/.
"""

from __future__ import annotations

import datetime
import json
import os
import random
import re
import signal
import subprocess
import sys
import time
from typing import Any

from . import host as hostmod
from . import report
from .plan import Plan, VariantSpec, env_pad, make_plan, parse_variant
from .rows import parse
from .knobs import KnobError, Knobs
from .rows import Parsed
from .spec import BENCH_DIR, ROOT_DIR, Row, Rows, SpecError, Suite, container_key, expand, load_cells, load_rows, load_suite, parse_container
from .variants import BuildError, Variant, git, identity_notes, pin_sources, prepare

TOOLS = {
    "rt": "bench/tools/rt.c",
    "sentinel": "bench/tools/sentinel.c",
    "valkey_model": "bench/probes/valkey_model.c",
    "churn": "bench/probes/churn.c",
    "execloop": "bench/tools/execloop.c",
}


# Where boot.sh mounts the perf bundle (bench/cb/perf-build.sh) on a cell
# VM; on PATH for every row and probe when it is there.
PERF_DIR = "/opt/perf"


def log(msg: str) -> None:
    print(msg, flush=True)


def perf_on_path() -> None:
    b = os.path.join(PERF_DIR, "bin")
    if os.path.isfile(os.path.join(b, "perf")) and b not in os.environ.get("PATH", "").split(":"):
        os.environ["PATH"] = b + ":" + os.environ.get("PATH", "/usr/bin:/bin")


def compile_tools(root: str, tools_dir: str) -> None:
    os.makedirs(tools_dir, exist_ok=True)
    for name, src in TOOLS.items():
        s = os.path.join(root, src)
        out = os.path.join(tools_dir, name)
        if os.path.exists(out) and os.path.getmtime(out) >= os.path.getmtime(s):
            continue
        r = subprocess.run(["cc", "-O2", "-Wall", "-Wextra", "-Werror", "-o", out, s], capture_output=True, text=True)
        if r.returncode != 0:
            raise BuildError(f"{src} did not compile:\n{r.stderr}")


def ensure_bench_programs(root: str, bin_dir: str, rows: list[Row], allow_build: bool) -> None:
    missing = sorted({a[len("{bin}/") :] for r in rows for a in r.cmd if a.startswith("{bin}/") and not os.path.exists(os.path.join(bin_dir, a[len("{bin}/") :]))})
    if not missing:
        return
    if not allow_build:
        raise BuildError(f"bench programs missing from {bin_dir}: {', '.join(missing)} (run bench/build.sh, or drop --no-bench-build)")
    log(f"== bench programs missing ({', '.join(missing)}): running bench/build.sh (a few minutes, once per target volume)")
    logf = os.path.join(root, "target", "bench-build.log")
    with open(logf, "wb") as f:
        r = subprocess.run(["sh", os.path.join(root, "bench", "build.sh")], stdout=f, stderr=subprocess.STDOUT, cwd=root)
    if r.returncode != 0:
        tail = open(logf, errors="replace").read().splitlines()[-20:]
        raise BuildError("bench/build.sh failed:\n" + "\n".join(tail))


def features_line(root: str, v: Variant | None, work: str, tools_dir: str) -> str | None:
    """The challoc features probe (tests/c/features.c, from the variant's
    own source tree: the keys move with the library) under that library:
    what it sees on this kernel and in this container."""
    if v is None:
        return None
    lib = v.lib
    tree = root if v.tree in ("", "tree") else os.path.join(work, "src", v.tree[:16])
    src = os.path.join(tree, "tests", "c", "features.c")
    if not (lib and os.path.exists(src)):
        return None
    exe = os.path.join(tools_dir, "features")
    env = dict(os.environ, CHALLOC_LIB=lib)
    r = subprocess.run(["sh", os.path.join(tree, "scripts", "ctest-cc.sh"), src, "-o", exe], capture_output=True, text=True, env=env, cwd=tree)
    if r.returncode != 0:
        return None
    r = subprocess.run([exe], capture_output=True, text=True, env=dict(os.environ, LD_PRELOAD=lib), timeout=60)
    for line in (r.stdout + r.stderr).splitlines():
        if line.startswith("features: "):
            return line
    return None


def clean_env() -> dict[str, str]:
    env = dict(os.environ)
    for k in ("LD_PRELOAD", "MALLOC_CONF", "CHALLOC_CONF", "RT_OUT", "RT_JSON", "RT_PMU", "CB_PAD"):
        env.pop(k, None)
    return env


def _read_rt(rt_file: str, rc: int | None, timeout: float) -> dict[str, Any]:
    rt: dict[str, Any] = {}
    try:
        with open(rt_file) as f:
            lines = [ln for ln in f.read().splitlines() if ln.strip()]
        if len(lines) == 1:
            rt = json.loads(lines[0])
    except (OSError, json.JSONDecodeError):
        rt = {}
    if not rt:
        rt = {"exit": 124 if rc in (-9, None) else (rc or 1), "error": "no rt record" + (f" (timeout {timeout:.0f} s)" if rc == -9 else "")}
    return rt


def run_driver(
    row: Row,
    variant: Variant,
    values: dict[str, str],
    tools_dir: str,
    logs: str,
    tag: str,
    pad: int,
) -> tuple[dict[str, Any], Parsed]:
    """One run of a driver row (bench/drivers/*.sh): the script gets the
    variant (CB_ALLOC, CB_LIB, CB_CONF, ...) and the row's env, and writes
    its metrics as JSON lines to RESULT_JSONL; the row's `metric` is the
    score, every metric goes into `extra`. Runs under tools/rt, so wall
    time, rusage and counters cover the whole driver."""
    rt_file = os.path.join(logs, f"{tag}.rt")
    out_file = os.path.join(logs, f"{tag}.out")
    jsonl = os.path.join(logs, f"{tag}.jsonl")
    scratch = os.path.join(logs, f"{tag}.d")
    for p in (rt_file, jsonl):
        if os.path.exists(p):
            os.unlink(p)
    os.makedirs(scratch, exist_ok=True)
    lib, sha = variant.lib_for(row.features)
    env = clean_env()
    env.update({k: expand(v, values) for k, v in row.env.items()})
    env.update(
        {
            "RT_OUT": rt_file,
            "RT_JSON": "1",
            "CB_PAD": "x" * pad,
            "RESULT_JSONL": jsonl,
            "CB_VARIANT": variant.label,
            "CB_ALLOC": "challoc" if variant.spec.kind == "challoc" else variant.spec.source,
            "CB_LIB": lib,
            "CB_LIB_SHA256": sha,
            "CB_CONF": variant.spec.conf,
            "CB_OUT": scratch,
            "CB_ROOT": values["root"],
            "CB_BIN": values["bin"],
            "CB_TOOLS": tools_dir,
        }
    )
    cmd = [os.path.join(tools_dir, "rt"), "sh", os.path.join(BENCH_DIR, row.driver)]
    timeout = row.timeout_s or max(600.0, row.est_s * 2 + 300)
    with open(out_file, "wb") as f:
        p = subprocess.Popen(cmd, stdout=f, stderr=subprocess.STDOUT, env=env, start_new_session=True)
        try:
            p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)
            p.wait()
    rt = _read_rt(rt_file, p.returncode, timeout)
    metrics: dict[str, float] = {}
    try:
        with open(jsonl) as f:
            for line in f:
                if line.strip():
                    d = json.loads(line)
                    for k, v in (d.get("metrics") or {}).items():
                        if isinstance(v, (int, float)) and not isinstance(v, bool):
                            metrics[k] = float(v)
    except (OSError, json.JSONDecodeError):
        pass
    if rt.get("exit") != 0:
        return rt, Parsed(None, extra=metrics, error=f"driver exit {rt.get('exit')} (see {out_file})")
    score = metrics.get(row.metric)
    if score is None or score <= 0:
        return rt, Parsed(None, extra=metrics, error=f"driver wrote no positive {row.metric} to RESULT_JSONL")
    return rt, Parsed(score, extra=metrics)


def run_one(
    row: Row,
    variant: Variant,
    argv: list[str],
    tools_dir: str,
    logs: str,
    tag: str,
    pad: int,
    values: dict[str, str] | None = None,
) -> tuple[dict[str, Any], str]:
    rt_file = os.path.join(logs, f"{tag}.rt")
    out_file = os.path.join(logs, f"{tag}.out")
    for p in (rt_file,):
        if os.path.exists(p):
            os.unlink(p)
    env = clean_env()
    env.update({"RT_OUT": rt_file, "RT_JSON": "1", "CB_PAD": "x" * pad})
    # `rt env VARS prog`: the allocator is loaded into the program, not rt.
    inner = ["env"]
    for k, v in row.env.items():
        inner.append(f"{k}={expand(v, values) if values is not None else v}")
    lib, _ = variant.lib_for(row.features)
    if lib:
        inner.append(f"LD_PRELOAD={lib}")
    if variant.spec.conf:
        inner.append(f"MALLOC_CONF={variant.spec.conf}")
    cmd = [os.path.join(tools_dir, "rt"), *inner, *argv]
    timeout = max(60.0, row.est_s * 6)
    with open(out_file, "wb") as f:
        p = subprocess.Popen(cmd, stdout=f, stderr=subprocess.STDOUT, env=env, start_new_session=True)
        try:
            p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)
            p.wait()
    rt = _read_rt(rt_file, p.returncode, timeout)
    text = open(out_file, encoding="utf-8", errors="replace").read()
    return rt, text


def default_run_id(suite: str, head: str) -> str:
    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%MZ")
    suffix = "".join(random.choice("abcdefghijkmnpqrstuvwxyz23456789") for _ in range(3))
    return f"{ts}-{head[:7]}-{suite}-{suffix}"


def execute(args) -> int:
    root = ROOT_DIR
    perf_on_path()
    rows = load_rows(args.rows_file)
    # A cell VM of `cb launch` runs its own entry of the launch file
    # (bench/cb/gce.py): the rows of its shard, the launch's variants
    # pinned to (commit, tree) with their sources shipped as tarballs.
    launch = vm_entry = None
    if getattr(args, "launch", None):
        with open(args.launch) as f:
            launch = json.load(f)
        if args.vm not in launch["vms"]:
            raise SpecError(f"{args.vm} is not a VM of launch {launch['run']}")
        vm_entry = launch["vms"][args.vm]
        pin_sources({v["source"]: v["tree"] for v in launch["variants"] if v["kind"] == "challoc"}, args.sources or "")
        args.suite, args.cell, args.run_id = launch["suite"], vm_entry["cell"], launch["run"]
    suite = load_suite(args.suite, rows)
    cells = load_cells(args.cells_path or [])
    if launch is not None and args.cell not in cells:
        # A cell from an overlay the launcher was given (--cells-path) that
        # this tree does not load: the launch file carries what the VM needs.
        from .spec import Cell

        ci = launch["cells"][args.cell]
        cells[args.cell] = Cell(args.cell, "gce", dict(ci), [], "launch.json")
    if args.cell not in cells:
        raise SpecError(f"unknown cell {args.cell!r}")
    cell = cells[args.cell]
    if cell.provider != "local" and launch is None:
        raise SpecError(f"cell {args.cell} is a {cell.provider} cell: `cb run` executes here, use --cell local (`cb launch` runs GCE cells)")
    refs = {"base": args.base, "head": args.head}
    if launch is not None:
        only = set(vm_entry.get("variants") or [])
        specs = [
            VariantSpec(v["label"], v["kind"], v["source"], v.get("conf", ""), v.get("role", "variant"), dict(v.get("knobs") or {}), v.get("features", ""))
            for v in launch["variants"]
            if not only or v["label"] in only
        ]
        refs = dict(launch.get("refs", refs))
    else:
        vitems = [v.strip() for v in args.variants.split(",")] if args.variants else suite.variants
        citems = [c.strip() for c in args.controls.split(",") if c.strip()] if args.controls is not None else suite.controls
        specs = [parse_variant(v, refs) for v in vitems] + [parse_variant(c, refs, role="control") for c in citems]
    row_names = list(vm_entry["rows"]) if vm_entry else ([r.strip() for r in args.rows.split(",")] if args.rows else suite.rows)
    for r in row_names:
        if r not in rows.rows:
            raise SpecError(f"unknown row {r!r}")
    smoke = args.smoke or suite.kind == "smoke"
    if smoke:
        blocks, warmup = int(suite.local.get("blocks", 1)) if not args.smoke else 1, 0
    elif launch is not None:
        blocks, warmup = int(vm_entry.get("blocks", launch["blocks"])), int(vm_entry.get("warmup_blocks", launch["warmup_blocks"]))
    else:
        blocks = args.blocks or int(suite.local.get("blocks", suite.blocks))
        warmup = args.warmup if args.warmup is not None else suite.warmup_blocks
    target = os.path.join(root, "target")
    work = os.path.join(target, "cb")
    tools_dir = os.path.join(work, "tools")
    bin_dir = args.bin or os.path.join(target, "bench-bin")
    nproc = os.cpu_count() or 1
    values = {**rows.vars, **((launch.get("vars") or {}) if launch else {}), **dict(kv.split("=", 1) for kv in (args.var or [])), "bin": bin_dir, "tools": tools_dir, "root": root, "nproc": str(nproc), "nproc2": str(2 * nproc), "nproc16": str(min(nproc, 16))}

    # Rows whose files are missing are dropped with a note (python rows
    # outside the image, espresso before bench/build.sh).
    head_commit = ""
    try:
        head_commit = launch["cb_commit"] if launch else git(root, "rev-parse", args.head)
    except BuildError:
        head_commit = "unknown"
    run_id = args.run_id or default_run_id(suite.name, head_commit)
    out = args.out or os.path.join(target, "results", "cb", run_id)
    logs = os.path.join(out, "logs")
    os.makedirs(logs, exist_ok=True)
    log(f"== cb run {suite.name}: run {run_id}, cell {cell.name}, out {os.path.relpath(out, root)}")
    for w in hostmod.preflight() if not hostmod.in_container() else []:
        log(f"== preflight: WARNING {w}")

    log("== tools and bench programs")
    compile_tools(root, tools_dir)
    ensure_bench_programs(root, bin_dir, [rows.rows[r] for r in row_names], not args.no_bench_build)
    kept = []
    for r in row_names:
        missing = [expand(n, values) for n in rows.rows[r].needs if not os.path.exists(expand(n, values))]
        if not rows.rows[r].is_driver:
            prog = expand(rows.rows[r].cmd[0], values)
            if "/" not in prog and not any(os.path.exists(os.path.join(d, prog)) for d in os.environ.get("PATH", "").split(":")):
                missing.append(prog)
        if missing:
            log(f"   {r}: skipped, missing {', '.join(missing)}")
        else:
            kept.append(r)
    row_names = kept

    log("== variants")
    variants = prepare(root, specs, work, values, log, features=[rows.rows[r].features for r in row_names])
    for v in variants:
        what = f"{v.spec.source} = {v.commit[:12]}" if v.spec.kind == "challoc" else v.spec.source
        knobs = "  " + " ".join(f"{k}={x}" for k, x in v.spec.knobs.items()) if v.spec.knobs else ""
        log(f"   {v.label:<10} {what}  {v.lib or '(no LD_PRELOAD)'}  {v.sha256[:16]}{'  MALLOC_CONF=' + v.spec.conf if v.spec.conf else ''}{knobs}")
        for fs, (lib, sha) in v.extra.items():
            log(f"   {'':<10} features {fs}: {lib}  {sha[:16]}")
    notes = identity_notes(variants)
    for n in notes:
        log(f"   {n}")
    if args.build_only:
        return 0

    log("== host")
    probe_v = next((v for v in reversed(variants) if v.spec.kind == "challoc" and v.spec.role != "control"), None)
    h = hostmod.capture(cell.name, {"features": features_line(root, probe_v, work, tools_dir), "features_of": probe_v.label if probe_v else None})
    if launch is not None:
        container = parse_container(launch["cells"][cell.name].get("container") or "default")
    else:
        container = parse_container(os.environ.get("CB_CONTAINER") or cell.get("container", "default"))
    h["container_profile"] = container
    h["comparability"] = hostmod.comparability_key(h, container_key(container))
    h["indicative"] = cell.provider == "local"
    if launch is not None:
        h["vm"] = args.vm
        h["launch"] = {k: vm_entry.get(k) for k in ("shard", "replicate", "attempt", "look", "variants")}
        ci = launch["cells"][cell.name]
        h["requested"] = {k: ci.get(k) for k in ("family", "image", "image_project", "machine")}
        exp = ci.get("expect") or {}
        if exp.get("kernel") and not re.match(exp["kernel"], h["kernel"]):
            _write_json(os.path.join(out, "host.json"), h)
            raise SpecError(f"cell {cell.name}: booted kernel {h['kernel']} does not match the expected {exp['kernel']!r} of image family {ci.get('family')}")
        if exp.get("page_size") and int(exp["page_size"]) != h["page_size"]:
            _write_json(os.path.join(out, "host.json"), h)
            raise SpecError(f"cell {cell.name}: page size {h['page_size']}, expected {exp['page_size']}")
    quiet = cell.get("quiet", {"load1": 4.0, "wait_s": 30})
    log(f"   {h['kernel']} {h['arch']} {h['cpu'].get('model')} nproc {h['nproc']} THP {h['thp'].get('enabled')} governor {h['governor']} load {h['loadavg'][0]:.2f}")
    if h.get("features"):
        log(f"   {h['features']}")
    probe = subprocess.run([os.path.join(tools_dir, "rt"), "--probe"], capture_output=True, text=True)
    h["pmu_probe"] = probe.stdout.strip()
    log(f"   {h['pmu_probe'].split(' x=')[0]}")

    seed = vm_entry["seed"] if vm_entry else (args.seed if args.seed is not None else random.randrange(1 << 31))
    s2 = Suite(**{**suite.__dict__, "kind": "smoke" if smoke else suite.kind})
    plan = make_plan(s2, rows, [v.spec for v in variants], run_id=run_id, cell=cell.name, blocks=blocks, warmup_blocks=warmup, seed=seed, row_names=row_names)
    est = plan.est_seconds(rows)
    log(f"== plan: {len(plan.rows)} rows x {len(variants)} variants, {warmup} warm-up + {blocks} blocks, {plan.runs()} runs, about {est / 60:.0f} min, seed {seed}")
    if args.dry_run:
        print(json.dumps(plan.to_json(), indent=1))
        return 0

    manifest = {
        "schema": "challoc.bench.manifest/v1",
        "run": run_id,
        "suite": suite.name,
        "cell": cell.name,
        "variants": [v.to_json() for v in variants],
        "notes": notes,
        "cb_commit": head_commit,
        "launch_variants": [v["label"] for v in launch["variants"]] if launch else None,
        "refs": refs,
        "argv": sys.argv,
        "values": values,
        "started": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
    }
    _write_json(os.path.join(out, "plan.json"), plan.to_json())
    _write_json(os.path.join(out, "host.json"), h)
    _write_json(os.path.join(out, "manifest.json"), manifest)

    if "running" in quiet:
        gate = hostmod.quiet_gate_running(float(quiet["running"]), float(quiet.get("wait_s", 30)), log)
    else:
        gate = hostmod.quiet_gate(float(quiet.get("load1", 4.0)), float(quiet.get("wait_s", 30)), log)
    h["quiet_gate"] = gate
    _write_json(os.path.join(out, "host.json"), h)

    by_label = {v.label: v for v in variants}
    if launch is not None:
        vm = {"name": args.vm, "zone": h.get("zone", ""), "spot": h.get("spot", False), "boot_id": h.get("boot_id")}
        shard, replicate = vm_entry["shard"], int(vm_entry["replicate"])
    else:
        vm = {"name": f"local:{h['hostname']}", "zone": h.get("zone", ""), "spot": False, "boot_id": h.get("boot_id")}
        shard, replicate = "s00", 0
    t_start = time.monotonic()
    samples_path = os.path.join(out, "samples.jsonl")
    blocks_path = os.path.join(out, "blocks.jsonl")
    failures = 0
    knobs = None
    if any(v.spec.knobs for v in variants):
        try:
            knobs = Knobs([v.spec.knobs for v in variants])
        except KnobError as e:
            raise SpecError(str(e)) from None
        log(f"== knobs: host values {knobs.original}")
        h["knobs_original"] = knobs.original
        _write_json(os.path.join(out, "host.json"), h)
    try:
        for blk in plan.blocks:
            t0, s0 = hostmod.cpu_times()
            sent = subprocess.run([os.path.join(tools_dir, "sentinel")], capture_output=True, text=True)
            try:
                sentinel = json.loads(sent.stdout)
            except json.JSONDecodeError:
                sentinel = {}
            load_start = hostmod.loadavg()[0]
            label = "warm-up" if blk.warmup else f"block {blk.index - warmup + 1}/{blocks}"
            log(f"== {label} (sentinel {sentinel.get('total_ms', 0):.0f} ms, load {load_start:.2f})")
            for ri, rname in enumerate(blk.rows):
                row = rows.rows[rname]
                argv = [expand(a, values) for a in row.cmd]
                line = []
                for pos, vlabel in enumerate(blk.order[rname]):
                    v = by_label[vlabel]
                    pad = env_pad(seed, blk.index, rname, pos)
                    tag = f"b{blk.index:02d}-{rname}-{pos}-{vlabel}"
                    applied: dict[str, str] = {}
                    if knobs is not None:
                        try:
                            applied = knobs.apply(v.spec.knobs)
                        except KnobError as e:
                            raise SpecError(str(e)) from None
                    if row.is_driver:
                        rt, parsed = run_driver(row, v, values, tools_dir, logs, tag, pad)
                    else:
                        rt, text = run_one(row, v, argv, tools_dir, logs, tag, pad, values)
                        parsed = parse(row, text, rt) if rt.get("exit") == 0 else None
                    score = parsed.score if parsed else None
                    err = "" if score is not None else (parsed.error if parsed else f"exit {rt.get('exit')}")
                    check_fail = ""
                    if score is not None and v.spec.kind == "challoc" and "min" in row.check and score < row.check["min"]:
                        check_fail = f"score {score} below the check minimum {row.check['min']}"
                    sample = {
                        "schema": "challoc.bench.sample/v1",
                        "run": run_id,
                        "cell": cell.name,
                        "comparability": h["comparability"],
                        "vm": vm,
                        "shard": shard,
                        "replicate": replicate,
                        "block": blk.index,
                        "pos": pos,
                        "seed": blk.seed,
                        "warmup": blk.warmup,
                        "row": rname,
                        "variant": vlabel,
                        "lib_sha256": v.lib_for(row.features)[1],
                        "conf": v.spec.conf,
                        "knobs": applied,
                        "score": score,
                        "unit": row.unit,
                        "better": row.better,
                        "extra": parsed.extra if parsed else {},
                        "rt": {k: rt.get(k) for k in ("wall_s", "user_s", "sys_s", "maxrss_kib", "exit", "signal", "vcsw", "ivcsw")},
                        "ops": parsed.ops if parsed else None,
                        "pmc": rt.get("pmc"),
                        "pmc_note": rt.get("pmc_note", rt.get("error", "")),
                        "health": {"sentinel_ms": sentinel.get("total_ms"), "load1": load_start},
                        "env": {"pad": pad, "pin": None},
                        "error": err or check_fail,
                        "ts": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
                    }
                    with open(samples_path, "a") as f:
                        f.write(json.dumps(sample, separators=(",", ":")) + "\n")
                    if score is None or check_fail:
                        failures += 1
                        line.append(f"{vlabel}=FAIL({err or check_fail}; {os.path.relpath(os.path.join(logs, tag + '.out'), root)})")
                    else:
                        line.append(f"{vlabel}={score:g}")
                log(f"   {rname:<14} " + " ".join(line))
            t1, s1 = hostmod.cpu_times()
            steal = 100.0 * (s1 - s0) / (t1 - t0) if t1 > t0 else 0.0
            with open(blocks_path, "a") as f:
                f.write(json.dumps({"schema": "challoc.bench.block/v1", "run": run_id, "vm": vm["name"], "comparability": h["comparability"], "block": blk.index, "warmup": blk.warmup, "sentinel": sentinel, "load1_start": load_start, "load1_end": hostmod.loadavg()[0], "steal_pct": round(steal, 3)}) + "\n")
    finally:
        if knobs is not None:
            knobs.restore()
    manifest["finished"] = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    manifest["wall_s"] = round(time.monotonic() - t_start, 1)
    _write_json(os.path.join(out, "manifest.json"), manifest)
    with open(os.path.join(out, "done.json"), "w") as f:
        json.dump({"run": run_id, "failures": failures, "wall_s": manifest["wall_s"]}, f)

    if smoke:
        log(f"== smoke: {plan.runs()} runs, {failures} failed")
        return 0 if failures == 0 else 1
    if launch is not None:
        # The verdict needs every VM's samples: the launcher pools them.
        log(f"== shard {shard} r{replicate} done: {plan.runs()} runs, {failures} failed, {manifest['wall_s']:.0f} s")
        return 0
    summary = report.analyze_dirs([out], rows)
    _write_json(os.path.join(out, "summary.json"), summary)
    text = report.render(summary)
    with open(os.path.join(out, "report.md"), "w") as f:
        f.write(text)
    print()
    print(text)
    return summary["exit"]


def _write_json(path: str, obj: Any) -> None:
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f, indent=1, default=str)
        f.write("\n")
    os.replace(tmp, path)
