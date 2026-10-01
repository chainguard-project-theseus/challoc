"""`cb launch` and `cb gc`: the GCE fan-out (bench/README.md, "cb launch").

    cb launch SUITE [--base REV] [--head REV] [--cells SEL] [--replicas N] ...

  1. resolve the suite's cells (machine type, image family, zones) and its
     variants (every rev to a commit and a tree)
  2. pack the rows into shards of about `shard_seconds` of runs each; one
     VM per (cell, shard, replicate)
  3. bundle: `git archive` of every variant tree and of the harness (this
     checkout, uncommitted and untracked files included) to
     gs://<bucket>/src/<tree>.tar.gz, content-addressed, uploaded once;
     then runs/<run>/launch.json, the table every VM reads its work from
  4. bulk-create spot VMs (one call per cell and zone) with bench/cb/boot.sh
     as the startup script, `max-run-duration` with DELETE, labels
     purpose=challoc-bench and cb-run=<run>, at most --max-vms at once
     (the rest wait for a slot)
  5. poll: a VM that finished (runs/<run>/done/<vm>.json) is deleted; one
     that vanished or stopped without finishing (preempted, stocked out)
     is rescheduled under a new name, on demand after two spot failures
  6. download runs/<run>/res/, pool every VM's samples (report.py) and, when
     the suite allows a second look, extend the rows that came out
     inconclusive with `extend_replicas` more VMs per cell
  7. summary.json, report.md and launch-summary.json (wall clock, VM
     minutes, cost estimate) under target/results/cb/<run>/

Exit codes: 0 neutral or improvement, 1 regression or broken, 2
inconclusive, 3 infrastructure failure (nothing measured, or gcloud
failed). Needs gcloud authenticated for the project in bench/cells'
[defaults] (`project`, `bucket`, `service_account`).
"""

from __future__ import annotations

import collections
import concurrent.futures
import datetime
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any

from . import report
from .plan import derive_seed, pack_shards, parse_variant, row_vm_seconds
from .spec import BENCH_DIR, ROOT_DIR, Cell, SpecError, load_cells, load_rows, load_suite, parse_container, select_cells
from .variants import BuildError, git, resolve_rev

BOOT_SH = os.path.join(BENCH_DIR, "cb", "boot.sh")
IMAGE_LOCK = os.path.join(BENCH_DIR, "image.lock")
LABEL = "purpose=challoc-bench"
# Price estimates, $/h (us-east4, 2026-09; verify in billing): on demand,
# spot. Unknown machines fall back to per-vCPU rates.
PRICES = {
    "c4a-standard-16": (0.79, 0.29),
    "c4-standard-16": (0.85, 0.33),
    "c4a-standard-4": (0.20, 0.07),
    "c4-standard-4": (0.21, 0.08),
}
PER_VCPU = (0.05, 0.02)
# Seconds a VM spends before its first run (boot, Docker, pull, build) and
# after (upload, poweroff): the planner's cost estimate and deadlines.
VM_OVERHEAD_S = 240
POLL_S = 10


def log(msg: str) -> None:
    ts = datetime.datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] {msg}", flush=True)


class InfraError(Exception):
    pass


def gcloud(*args: str, project: str | None = None, fmt_json: bool = False, check: bool = True, timeout: float = 600) -> Any:
    cmd = ["gcloud", *args, "--quiet"]
    if project:
        cmd += ["--project", project]
    if fmt_json:
        cmd += ["--format", "json"]
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise InfraError(f"gcloud {' '.join(args[:3])}: timed out after {timeout:.0f} s") from None
    if check and r.returncode != 0:
        raise InfraError(f"gcloud {' '.join(args[:4])}: {r.stderr.strip()[-2000:]}")
    if fmt_json:
        return json.loads(r.stdout or "null") if r.returncode == 0 else None
    return r


class Api:
    """The GCS and Compute REST calls the launcher makes in its loops, with
    one `gcloud auth print-access-token` per 30 minutes instead of a gcloud
    process (seconds each on a busy laptop) per call."""

    def __init__(self, project: str) -> None:
        self.project = project
        self._tok = ""
        self._t = 0.0
        self._lock = threading.Lock()

    def token(self) -> str:
        with self._lock:
            if time.time() - self._t > 1800 or not self._tok:
                r = gcloud("auth", "print-access-token", timeout=60)
                self._tok = r.stdout.strip()
                self._t = time.time()
            return self._tok

    def call(self, method: str, url: str, data: bytes | None = None, ctype: str = "application/json", ok404: bool = False) -> bytes | None:
        import urllib.error
        import urllib.request

        for attempt in range(4):
            req = urllib.request.Request(url, data=data, method=method, headers={"Authorization": f"Bearer {self.token()}", "Content-Type": ctype})
            try:
                with urllib.request.urlopen(req, timeout=120) as r:
                    return r.read()
            except urllib.error.HTTPError as e:
                if e.code == 404 and ok404:
                    return None
                if e.code in (429, 500, 502, 503) and attempt < 3:
                    time.sleep(1 + attempt * 2)
                    continue
                if e.code == 401 and attempt < 3:
                    self._t = 0
                    continue
                raise InfraError(f"{method} {url.split('?')[0]}: HTTP {e.code} {e.read()[:500]!r}") from None
            except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
                if attempt < 3:
                    time.sleep(1 + attempt * 2)
                    continue
                raise InfraError(f"{method} {url.split('?')[0]}: {e}") from None
        return None

    # GCS
    def ls(self, bucket: str, prefix: str) -> list[str]:
        import urllib.parse

        out, tok = [], ""
        while True:
            q = {"prefix": prefix, "fields": "items(name),nextPageToken", "maxResults": "1000"}
            if tok:
                q["pageToken"] = tok
            d = json.loads(self.call("GET", f"https://storage.googleapis.com/storage/v1/b/{bucket}/o?{urllib.parse.urlencode(q)}") or b"{}")
            out += [i["name"] for i in d.get("items", [])]
            tok = d.get("nextPageToken", "")
            if not tok:
                return out

    def put(self, bucket: str, name: str, data: bytes, ctype: str = "application/octet-stream") -> None:
        import urllib.parse

        self.call("POST", f"https://storage.googleapis.com/upload/storage/v1/b/{bucket}/o?uploadType=media&name={urllib.parse.quote(name, safe='')}", data, ctype)

    def get(self, bucket: str, name: str) -> bytes | None:
        import urllib.parse

        return self.call("GET", f"https://storage.googleapis.com/storage/v1/b/{bucket}/o/{urllib.parse.quote(name, safe='')}?alt=media", ok404=True)

    # Compute
    def instances(self, flt: str) -> list[dict[str, Any]]:
        import urllib.parse

        out, tok = [], ""
        while True:
            q = {"filter": flt, "fields": "items/*/instances(name,status,zone,creationTimestamp,labels),nextPageToken"}
            if tok:
                q["pageToken"] = tok
            d = json.loads(self.call("GET", f"https://compute.googleapis.com/compute/v1/projects/{self.project}/aggregated/instances?{urllib.parse.urlencode(q)}") or b"{}")
            for z in d.get("items", {}).values():
                for i in z.get("instances", []):
                    i["zone"] = i["zone"].rsplit("/", 1)[-1]
                    out.append(i)
            tok = d.get("nextPageToken", "")
            if not tok:
                return out

    def delete_instance(self, zone: str, name: str) -> None:
        """Asynchronous: returns once GCE has accepted the delete."""
        self.call("DELETE", f"https://compute.googleapis.com/compute/v1/projects/{self.project}/zones/{zone}/instances/{name}", ok404=True)


def price(machine: str, spot: bool) -> float:
    if machine in PRICES:
        return PRICES[machine][1 if spot else 0]
    try:
        vcpus = int(machine.rsplit("-", 1)[1])
    except (IndexError, ValueError):
        vcpus = 16
    return vcpus * PER_VCPU[1 if spot else 0]


def read_image_lock(path: str = IMAGE_LOCK) -> dict[str, str]:
    out: dict[str, str] = {}
    if not os.path.exists(path):
        return out
    for line in open(path):
        line = line.split("#", 1)[0].strip()
        if "=" in line:
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip().strip('"')
    return out


def resolve_family(family: str, project: str) -> dict[str, Any] | None:
    """The image a family points at now (name, creation time), or None
    when the family does not exist or cannot be read: such a cell is
    PENDING, not failed (image families get renamed and removed)."""
    r = gcloud("compute", "images", "describe-from-family", family, f"--project={project}", "--format=json(name,creationTimestamp,status,architecture)", check=False, timeout=120)
    if r.returncode != 0:
        return None
    try:
        d = json.loads(r.stdout)
    except json.JSONDecodeError:
        return None
    return d if d.get("status", "READY") == "READY" else None


def parse_container_arg(s: str) -> dict[str, Any]:
    """`default`, `privileged,cpus=4`, `default,cpuset=0-3,memory=8g`."""
    parts = [p.strip() for p in s.split(",") if p.strip()]
    d: dict[str, Any] = {"profile": parts[0] if parts and "=" not in parts[0] else "default"}
    for p in parts:
        if "=" in p:
            k, v = p.split("=", 1)
            d[k] = v
    return parse_container(d, "--container")


# ---- bundles -------------------------------------------------------------------


def harness_tree(root: str) -> str:
    """The tree of this checkout as it is on disk: tracked changes and
    untracked (not ignored) files included, via a scratch index."""
    idx = git(root, "rev-parse", "--git-path", "index")
    idx = idx if os.path.isabs(idx) else os.path.join(root, idx)
    with tempfile.TemporaryDirectory() as td:
        tmp = os.path.join(td, "index")
        if os.path.exists(idx):
            shutil.copy(idx, tmp)
        env = dict(os.environ, GIT_INDEX_FILE=tmp)
        for cmd in (["add", "-A", "."], ["write-tree"]):
            r = subprocess.run(["git", "-C", root, *cmd], capture_output=True, text=True, env=env)
            if r.returncode != 0:
                raise BuildError(f"git {' '.join(cmd)}: {r.stderr.strip()}")
        return r.stdout.strip()


def upload_trees(api: Api, root: str, bucket: str, trees: list[str]) -> dict[str, str]:
    """`git archive` of each tree to gs://bucket/src/<tree>.tar.gz unless it
    is there already. Returns tree -> object name."""
    have = set(api.ls(bucket, "src/"))
    out = {}
    for t in trees:
        obj = f"src/{t}.tar.gz"
        out[t] = obj
        if obj in have:
            continue
        a = subprocess.run(["git", "-C", root, "archive", "--format=tar.gz", t], capture_output=True)
        if a.returncode != 0:
            raise BuildError(f"git archive {t}: {a.stderr.decode(errors='replace')}")
        api.put(bucket, obj, a.stdout, "application/gzip")
    return out


# ---- the launch ------------------------------------------------------------------


class Launch:
    def __init__(self, args: Any) -> None:
        self.args = args
        self.t0 = time.time()
        self.times: dict[str, float] = {}
        self.rows = load_rows()
        self.suite = load_suite(args.suite, self.rows)
        self.cells = load_cells(args.cells_path or [])
        d = self.cells[next(iter(self.cells))].fields  # [defaults] are in every cell
        self.project = args.project or d.get("project")
        self.bucket = args.bucket or d.get("bucket")
        self.sa = args.service_account or d.get("service_account")
        if not (self.project and self.bucket and self.sa):
            raise SpecError("cb launch needs project, bucket and service_account in bench/cells [defaults] (or --project/--bucket/--service-account)")
        sel = args.cells or self.suite.cells
        self.cell_names = select_cells(sel, self.cells)
        for n in self.cell_names:
            if self.cells[n].provider != "gce":
                raise SpecError(f"cell {n} is a {self.cells[n].provider} cell; cb launch runs gce cells")
        self.max_vms = int(args.max_vms or os.environ.get("CB_MAX_VMS", 32))
        self.api = Api(self.project)
        self.lock = threading.Lock()
        self.vms: dict[str, dict[str, Any]] = {}  # the launch file's table
        self.state: dict[str, dict[str, Any]] = {}  # launcher-side: created, deleted, status
        self.pending: list[str] = []
        self.failed_creates: list[str] = []

    # -- plan ----------------------------------------------------------------

    def plan(self) -> None:
        a, s = self.args, self.suite
        refs = {"base": a.base, "head": a.head}
        vitems = [v.strip() for v in a.variants.split(",")] if a.variants else s.variants
        citems = [c.strip() for c in a.controls.split(",") if c.strip()] if a.controls is not None else s.controls
        specs = [parse_variant(v, refs) for v in vitems] + [parse_variant(c, refs, role="control") for c in citems]
        self.variants = []
        for v in specs:
            e = {"label": v.label, "kind": v.kind, "source": v.source, "conf": v.conf, "role": v.role, "knobs": v.knobs}
            if v.kind == "challoc":
                if v.source == "tree":
                    raise SpecError("variant source `tree` is local-only; commit it (or name the rev) for cb launch")
                commit, tree = resolve_rev(ROOT_DIR, v.source)
                e.update(rev=v.source, source=commit, tree=tree)
            self.variants.append(e)
        self.row_names = [r.strip() for r in a.rows.split(",")] if a.rows else list(s.rows)
        for r in self.row_names:
            if r not in self.rows.rows:
                raise SpecError(f"unknown row {r!r}")
        self.blocks = a.blocks or s.blocks
        self.warmup = s.warmup_blocks if a.warmup is None else a.warmup
        self.replicas = a.replicas or s.replicas
        self.looks = a.looks or s.looks
        self.seed = a.seed if a.seed is not None else random.randrange(1 << 31)
        self.shard_seconds = a.shard_seconds or s.shard_seconds
        nvar = len(self.variants)
        # Paired rows are packed into shards (every VM runs every variant);
        # one-variant-per-vm rows (soaks) get a VM per (row, variant,
        # replicate) of their own.
        self.paired_rows = [r for r in self.row_names if self.rows.rows[r].placement == "paired"]
        self.solo_rows = [r for r in self.row_names if self.rows.rows[r].placement != "paired"]
        self.shards = pack_shards(self.rows, self.paired_rows, nvar, self.blocks, self.warmup, self.shard_seconds) if self.paired_rows else []
        head = next((v for v in self.variants if v.get("rev") == a.head), None) or next((v for v in self.variants if v["kind"] == "challoc"), {"source": "none"})
        rid = "".join(random.choice("abcdefghijkmnpqrstuvwxyz23456789") for _ in range(5))
        ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dt%H%Mz")
        self.rid = rid
        self.run = (a.run_id or f"{ts}-{head['source'][:7]}-{s.name}-{rid}").lower()
        self.refs = refs
        self.cell_info = {}
        self.pending_cells: dict[str, str] = {}
        override = parse_container_arg(a.container) if getattr(a, "container", None) else s.container
        knobbed = [v["label"] for v in self.variants if v.get("knobs")]
        for n in self.cell_names:
            c = self.cells[n]
            host = c.get("host") or {}
            if not isinstance(host, dict) or "family" not in host:
                raise SpecError(f"cell {n}: no host image family")
            container = override or parse_container(c.get("container", "default"), f"cell {n}")
            if knobbed and container["profile"] != "privileged":
                raise SpecError(f"variants {', '.join(knobbed)} set host knobs: they need the privileged container profile (suite `container`, or --container privileged); cell {n} runs {container['profile']}")
            self.cell_info[n] = {
                "arch": c.get("arch"),
                "machine": c.get("machine"),
                "family": host["family"],
                "image_project": host.get("project"),
                "zones": list(c.get("zones") or []),
                "container": container,
                "runtime": c.get("runtime", "docker"),
                "bootstrap": c.get("bootstrap", "startup-script"),
                "prep": list(c.get("prep") or []) + list(s.prep),
                "expect": dict(c.get("expect") or {}),
                "pmu": c.get("pmu"),
                "spot": c.get("spot", "prefer"),
                "max_run": c.get("max_run", "45m"),
                "disk": c.get("disk", "hyperdisk-balanced"),
                "boot_disk_gb": int(c.get("boot_disk_gb", 30)),
                "quiet": c.get("quiet"),
                "internal": os.path.basename(c.source).startswith("internal"),
            }
        self.resolve_images()
        for n in self.cell_names:
            if n in self.pending_cells:
                continue
            for r in range(self.replicas):
                spot = self.cell_info[n]["spot"] != "never"
                for k, rows in enumerate(self.shards):
                    self._add_vm(n, k, r, rows, look=1, spot=spot)
                for j, row in enumerate(self.solo_rows):
                    for v in self.variants:
                        if not self._can_run(v, row):
                            continue
                        self._add_vm(n, len(self.shards) + j, r, [row], look=1, variant=v["label"], spot=spot)

    def _can_run(self, v: dict[str, Any], row: str) -> bool:
        from .plan import VariantSpec

        spec = VariantSpec(v["label"], v["kind"], v["source"], v.get("conf", ""), v.get("role", "variant"))
        r = self.rows.rows[row]
        return spec.can_run(r.requires, r.allocs)

    def resolve_images(self) -> None:
        """Every cell's image family to the image it points at now; the
        VMs of a launch all boot that image, whatever the family does
        meanwhile. A missing family makes the cell PENDING."""
        seen: dict[tuple[str, str], dict[str, Any] | None] = {}
        for n, ci in self.cell_info.items():
            key = (ci["family"], ci["image_project"])
            if key not in seen:
                seen[key] = resolve_family(*key) if not self.args.dry_run else {"name": f"{key[0]} (not resolved: dry run)"}
            img = seen[key]
            if img is None:
                self.pending_cells[n] = f"image family {ci['family']} ({ci['image_project']}) not found"
                log(f"   {n}: PENDING, {self.pending_cells[n]}")
                continue
            ci["image"] = img["name"]
            ci["image_created"] = img.get("creationTimestamp")

    def _cellslug(self, cell: str) -> str:
        return cell.replace("_", "-")[:20]

    def _add_vm(self, cell: str, k: int, r: int, rows: list[str], look: int, attempt: int = 0, spot: bool = True, variant: str | None = None) -> str:
        vslug = f"-v{[v['label'] for v in self.variants].index(variant)}" if variant else ""
        base = f"cb-{self.rid}-{self._cellslug(cell)}-s{k:02d}{vslug}-r{r:02d}" + (f"-l{look}" if look > 1 else "")
        name = base + (f"-a{attempt}" if attempt else "")
        self.vms[name] = {
            "cell": cell,
            "shard": f"s{k:02d}",
            "replicate": r,
            "rows": rows,
            "seed": derive_seed(self.seed, base),
            "attempt": attempt,
            "look": look,
        }
        if variant:
            # One variant on this VM: one block, no warm-up (a soak is its
            # own warm-up); compared unpaired across VMs (report.py).
            self.vms[name].update(variants=[variant], blocks=1, warmup_blocks=0)
        self.state[name] = {"base": base, "spot": spot, "status": "pending", "k": k, "variant": variant}
        self.pending.append(name)
        return name

    def est(self) -> tuple[float, float]:
        """(VM-minutes, $) of the planned VMs."""
        mins = cost = 0.0
        for name, v in self.vms.items():
            ci = self.cell_info[v["cell"]]
            m = (VM_OVERHEAD_S + self.vm_run_s(name)) / 60
            mins += m
            cost += m / 60 * price(ci["machine"], True)
        return mins, cost

    def vm_run_s(self, name: str) -> float:
        v = self.vms[name]
        if v.get("variants"):
            return sum(self.rows.rows[r].est_s * len(v["variants"]) * int(v.get("blocks", 1)) for r in v["rows"])
        return sum(row_vm_seconds(self.rows, r, len(self.variants), self.blocks, self.warmup) for r in v["rows"])

    def vm_max_run(self, name: str) -> str:
        """The VM's max-run-duration: the cell's, or 2 x the estimate plus
        10 minutes when that is longer (soaks)."""
        ci = self.cell_info[self.vms[name]["cell"]]
        need = 2 * (VM_OVERHEAD_S + self.vm_run_s(name)) + 600
        return f"{int(max(_dur_s(ci['max_run']), need) // 60)}m"

    # -- bundles -------------------------------------------------------------

    def bundle(self) -> None:
        trees = sorted({v["tree"] for v in self.variants if v["kind"] == "challoc"})
        self.harness = harness_tree(ROOT_DIR)
        objs = upload_trees(self.api, ROOT_DIR, self.bucket, trees + [self.harness])
        self.sources = {t: {"object": objs[t]} for t in trees}
        lockd = read_image_lock()
        self.image = {k: v for k, v in lockd.items() if k in ("arm64", "amd64")}
        if self.args.no_image:
            self.image = {}
        try:
            cb_commit = git(ROOT_DIR, "rev-parse", "HEAD")
        except BuildError:
            cb_commit = "unknown"
        self.cb_commit = cb_commit
        self.upload_launch()

    def launch_doc(self) -> dict[str, Any]:
        return {
            "schema": "challoc.bench.launch/v1",
            "run": self.run,
            "suite": self.suite.name,
            "bucket": self.bucket,
            "harness": {"tree": self.harness, "object": f"src/{self.harness}.tar.gz"},
            "sources": self.sources,
            "variants": self.variants,
            "refs": self.refs,
            "rows": self.row_names,
            "shards": self.shards,
            "blocks": self.blocks,
            "warmup_blocks": self.warmup,
            "replicas": self.replicas,
            "looks": self.looks,
            "seed": self.seed,
            "alpha": self.suite.alpha,
            "image": self.image,
            "cells": self.cell_info,
            "pending_cells": self.pending_cells,
            "vms": self.vms,
            "cb_commit": self.cb_commit,
            "vars": dict(kv.split("=", 1) for kv in (self.args.var or [])),
        }

    def upload_launch(self) -> None:
        with self.lock:
            doc = json.dumps(self.launch_doc(), indent=1)
        self.api.put(self.bucket, f"runs/{self.run}/launch.json", doc.encode(), "application/json")

    # -- VMs -------------------------------------------------------------------

    def metadata(self, cell: str) -> str:
        """The VM's instance metadata (gcloud's ^|^ delimiter form, values
        may hold commas): boot.sh reads its work from here, so the host
        needs no JSON parser (a Chainguard OS host has no python3)."""
        ci = self.cell_info[cell]
        c = ci["container"]
        md = {
            "cb-bucket": self.bucket,
            "cb-run": self.run,
            "cb-harness": f"src/{self.harness}.tar.gz",
            "cb-sources": " ".join(x["object"] for x in self.sources.values()),
            "cb-image": self.image.get(ci["arch"], ""),
            "cb-runtime": ci["runtime"],
            "cb-container": c["profile"],
            "cb-cpus": c.get("cpus", ""),
            "cb-memory": c.get("memory", ""),
            "cb-cpuset": c.get("cpuset", ""),
            "cb-prep": " ".join(ci["prep"]),
        }
        return "^|^" + "|".join(f"{k}={v}" for k, v in md.items())

    def _create_group(self, cell: str, names: list[str], spot: bool, max_run: str) -> list[str]:
        """Bulk-create names in the cell's zones, first zone first; returns
        the names that could not be created anywhere."""
        ci = self.cell_info[cell]
        left = list(names)
        errors = []
        # bootstrap: startup-script (boot.sh as the startup script; the
        # guest agent runs it as root at boot: Ubuntu, Debian and the
        # Chainguard OS images alike) or ssh-systemd-run (boot.sh in the
        # cb-boot metadata key, started over ssh as a transient system unit
        # once ssh answers, for images whose agent runs no scripts).
        boot_key = "startup-script" if ci["bootstrap"] == "startup-script" else "cb-boot"
        for zone in ci["zones"]:
            if not left:
                break
            flags = [
                "compute", "instances", "bulk", "create",
                f"--predefined-names={','.join(left)}",
                "--min-count=1",
                f"--zone={zone}",
                f"--machine-type={ci['machine']}",
                f"--image={ci['image']}",
                f"--image-project={ci['image_project']}",
                f"--boot-disk-size={ci['boot_disk_gb']}GB",
                f"--boot-disk-type={ci['disk']}",
                f"--max-run-duration={max_run}",
                "--instance-termination-action=DELETE",
                f"--service-account={self.sa}",
                "--scopes=cloud-platform",
                f"--metadata={self.metadata(cell)}",
                f"--metadata-from-file={boot_key}={BOOT_SH}",
                f"--labels={LABEL},cb-run={self.run},cb-cell={cell}",
            ]
            if spot:
                flags.append("--provisioning-model=SPOT")
            elif "-metal" in ci["machine"]:
                flags.append("--maintenance-policy=TERMINATE")
            if ci.get("pmu") and not self.args.no_pmu:
                flags.append(f"--performance-monitoring-unit={ci['pmu']}")
            t = time.time()
            r = gcloud(*flags, project=self.project, check=False, timeout=300)
            now = time.time()
            created = self._existing(set(left))
            for n in created:
                self.state[n].update(status="created", created=t, zone=zone, created_done=now)
            left = [n for n in left if n not in created]
            if r.returncode != 0:
                errors.append(f"{zone}: {r.stderr.strip().splitlines()[-1] if r.stderr.strip() else r.returncode}")
                log(f"   create in {zone}: {len(created)} created, {len(left)} not: {errors[-1][:300]}")
            else:
                log(f"   created {len(created)} {ci['machine']} {'spot' if spot else 'on-demand'} VMs in {zone} ({now - t:.0f} s, image {ci['image']})")
            if created and ci["bootstrap"] == "ssh-systemd-run":
                for n in created:
                    threading.Thread(target=self._ssh_start, args=(n, zone), daemon=True).start()
        return left

    def _ssh_start(self, name: str, zone: str) -> None:
        """The ssh-systemd-run bootstrap: once ssh answers, fetch boot.sh
        from the cb-boot metadata key and start it as a transient system
        unit (a job started from an ssh session dies with the session's
        scope on a Chainguard OS host; a system unit outlives it)."""
        cmd = (
            "curl -sf -H Metadata-Flavor:Google http://metadata.google.internal/computeMetadata/v1/instance/attributes/cb-boot -o /tmp/cb-boot.sh"
            " && sudo systemd-run --collect --quiet --unit=cb-boot bash /tmp/cb-boot.sh && echo cb-boot-started"
        )
        deadline = time.time() + 600
        while time.time() < deadline:
            r = gcloud("compute", "ssh", name, f"--zone={zone}", f"--command={cmd}", "--ssh-flag=-oConnectTimeout=10", "--ssh-flag=-oStrictHostKeyChecking=no", project=self.project, check=False, timeout=120)
            if "cb-boot-started" in (r.stdout or ""):
                log(f"   {name}: boot.sh started over ssh")
                return
            if self.state.get(name, {}).get("status") != "created":
                return
            time.sleep(15)
        log(f"   {name}: ssh never answered; the VM will time out")

    def _existing(self, names: set[str]) -> set[str]:
        try:
            got = self.api.instances(f"labels.cb-run={self.run}")
        except InfraError:
            got = []
        return {i["name"] for i in got} & names

    def create_pending(self) -> None:
        with self.lock:
            active = sum(1 for s in self.state.values() if s["status"] in ("created", "creating"))
            room = max(0, self.max_vms - active)
            take, self.pending = self.pending[:room], self.pending[room:]
            for n in take:
                self.state[n]["status"] = "creating"
        if not take:
            return
        groups: dict[tuple[str, bool, str], list[str]] = collections.defaultdict(list)
        for n in take:
            groups[(self.vms[n]["cell"], self.state[n]["spot"], self.vm_max_run(n))].append(n)
        with concurrent.futures.ThreadPoolExecutor(len(groups)) as ex:
            futs = {ex.submit(self._create_group, c, ns, spot, mr): (c, ns) for (c, spot, mr), ns in groups.items()}
            for f in futs:
                left = f.result()
                for n in left:
                    self._failed(n, "create failed in every zone")

    def _failed(self, name: str, why: str) -> None:
        """One attempt of a shard is lost: reschedule it (spot twice, then
        on demand) unless it has had three attempts."""
        st = self.state[name]
        st["status"] = "failed"
        st["why"] = why
        v = self.vms[name]
        log(f"   {name}: {why}")
        if v["attempt"] >= 2 or self.args.no_reschedule:
            log(f"   {name}: giving up on {v['cell']} {v['shard']} r{v['replicate']}")
            return
        att = v["attempt"] + 1
        n2 = self._add_vm(v["cell"], st["k"], v["replicate"], v["rows"], v["look"], attempt=att, spot=att < 2 and st["spot"], variant=st.get("variant"))
        self.state[n2]["rescheduled_from"] = name
        log(f"   rescheduled as {n2} ({'spot' if self.state[n2]['spot'] else 'on demand'})")
        self.upload_launch()

    def delete(self, names: list[str]) -> None:
        for n in names:
            z = self.state[n].get("zone")
            self.state[n]["deleted"] = time.time()
            if z:
                try:
                    self.api.delete_instance(z, n)
                except InfraError as e:
                    log(f"   delete {n}: {e} (cb gc will retry)")

    def poll(self, deadline: float) -> None:
        """Until every VM of the table is done or given up, or the deadline."""
        seen: set[str] = set()
        while True:
            self.create_pending()
            try:
                done = {n.rsplit("/", 1)[-1].removesuffix(".json") for n in self.api.ls(self.bucket, f"runs/{self.run}/done/")}
            except InfraError as e:
                log(f"   poll: {e}")
                done = set()
            try:
                live = {i["name"]: i["status"] for i in self.api.instances(f"labels.cb-run={self.run}")}
            except InfraError as e:
                log(f"   poll: {e}")
                live = None
            newly = []
            for n, st in list(self.state.items()):
                if st["status"] != "created":
                    continue
                if n in done:
                    st["status"] = "done"
                    st["done"] = time.time()
                    newly.append(n)
                    continue
                if live is None:
                    continue
                if n in live:
                    seen.add(n)
                    st["gce_status"] = live[n]
                    if live[n] in ("TERMINATED", "STOPPED", "SUSPENDED"):
                        # Stopped without a done marker: preempted with STOP, or boot.sh died.
                        st["deleted_pending"] = True
                        self.delete([n])
                        self._failed(n, f"stopped ({live[n]}) without finishing")
                elif time.time() - st.get("created_done", st.get("created", 0)) > 30:
                    st["deleted"] = time.time()
                    self._failed(n, "vanished (preempted) without finishing" if n in seen else "never appeared")
            if newly:
                self.delete(newly)
                ndone = sum(1 for s in self.state.values() if s["status"] == "done")
                log(f"   done {ndone}/{len(self.state) - sum(1 for s in self.state.values() if s['status'] == 'failed')}: {', '.join(sorted(newly))}")
                self.times.setdefault("first_done", time.time() - self.t0)
            active = [n for n, s in self.state.items() if s["status"] in ("created", "creating")]
            if not active and not self.pending:
                return
            if time.time() > deadline:
                log(f"== deadline: {len(active)} VMs still running; deleting them")
                self.delete([n for n in active if self.state[n]["status"] == "created"])
                for n in active:
                    self.state[n]["status"] = "timeout"
                return
            time.sleep(POLL_S)

    # -- results ---------------------------------------------------------------

    def download(self, out: str) -> list[str]:
        res = os.path.join(out, "res")
        os.makedirs(res, exist_ok=True)
        prefix = f"runs/{self.run}/res/"
        names = [n for n in self.api.ls(self.bucket, prefix) if not n.endswith(".tgz")]

        def fetch(n: str) -> None:
            p = os.path.join(res, n[len(prefix):])
            os.makedirs(os.path.dirname(p), exist_ok=True)
            data = self.api.get(self.bucket, n)
            if data is not None:
                with open(p, "wb") as f:
                    f.write(data)

        with concurrent.futures.ThreadPoolExecutor(16) as ex:
            list(ex.map(fetch, names))
        dirs = []
        for n in sorted(os.listdir(res)):
            d = os.path.join(res, n)
            if os.path.exists(os.path.join(d, "samples.jsonl")):
                dirs.append(d)
        return dirs

    def analyze(self, dirs: list[str]) -> dict[str, Any]:
        return report.analyze_dirs(dirs, self.rows)

    def extend(self, summary: dict[str, Any], dirs: list[str]) -> bool:
        """The second look: `extend_replicas` more VMs per cell for the
        gate rows that came out inconclusive there."""
        n_ext = self.args.extend_replicas if self.args.extend_replicas is not None else self.suite.extend_replicas
        if self.looks < 2 or n_ext <= 0:
            return False
        key_cell = {}
        for d in dirs:
            h = report.load_dir(d)["host"]
            if h.get("comparability"):
                key_cell[h["comparability"]] = h.get("cell")
        todo: dict[str, list[str]] = collections.defaultdict(list)
        for r in summary["results"]:
            if r["kind"] == "gate" and r["verdict"] == "inconclusive" and r["row"] in self.paired_rows:
                c = key_cell.get(r["comparability"])
                if c in self.cell_info and r["row"] not in todo[c]:
                    todo[c].append(r["row"])
        if not todo:
            return False
        for c, rows in todo.items():
            rows = [r for r in self.row_names if r in rows]
            shards = pack_shards(self.rows, rows, len(self.variants), self.blocks, self.warmup, self.shard_seconds)
            log(f"== look 2 on {c}: {', '.join(rows)} inconclusive; {n_ext} x {len(shards)} more VMs")
            for r in range(n_ext):
                for k, srows in enumerate(shards):
                    self._add_vm(c, k, self.replicas + r, srows, look=2, spot=self.cell_info[c]["spot"] != "never")
        self.upload_launch()
        return True

    def cost(self) -> dict[str, Any]:
        mins = 0.0
        usd = 0.0
        now = time.time()
        for n, st in self.state.items():
            if "created" not in st:
                continue
            end = st.get("deleted") or st.get("done") or now
            m = max(1.0, (end - st["created"]) / 60 + 0.5)  # + the delete itself; 1 min minimum
            mins += m
            usd += m / 60 * price(self.cell_info[self.vms[n]["cell"]]["machine"], st["spot"])
        return {"vm_minutes": round(mins, 1), "usd_estimate": round(usd, 2), "vms_created": sum(1 for s in self.state.values() if "created" in s)}


def vm_timings(dirs: list[str]) -> dict[str, Any]:
    """Medians of the VM-side stamps (boot.sh done.json)."""
    keys: dict[str, list[float]] = collections.defaultdict(list)
    for d in dirs:
        p = os.path.join(d, "done.json")
        if os.path.exists(p):
            for k, v in (json.load(open(p)).get("times") or {}).items():
                keys[k].append(float(v))
    return {k: round(statistics.median(v), 1) for k, v in keys.items()}


def launch(args: Any) -> int:
    L = Launch(args)
    L.plan()
    mins, usd = L.est()
    nvm = len(L.vms)
    log(f"== cb launch {L.suite.name}: run {L.run}")
    log(f"   cells {', '.join(L.cell_names)}; {len(L.shards)} shards x {L.replicas} replicas{f' + {len(L.solo_rows)} one-variant-per-vm rows' if L.solo_rows else ''} = {nvm} VMs (at most {L.max_vms} at once)")
    for k, s in enumerate(L.shards):
        log(f"   s{k:02d}: {', '.join(s)}")
    for r in L.solo_rows:
        log(f"   {r}: one VM per variant and replicate (unpaired)")
    for n, ci in L.cell_info.items():
        if n not in L.pending_cells:
            c = ci["container"]
            log(f"   {n}: {ci['machine']} {ci.get('image')} ({ci['image_project']}), {ci['runtime']}, bootstrap {ci['bootstrap']}, container {c['profile']}{', cpus ' + c['cpus'] if c.get('cpus') else ''}{', cpuset ' + c['cpuset'] if c.get('cpuset') else ''}{', memory ' + c['memory'] if c.get('memory') else ''}{', prep ' + ' '.join(ci['prep']) if ci['prep'] else ''}")
    for v in L.variants:
        log(f"   {v['label']:<8} {v.get('rev', v['source'])} {v['source'][:12] if v['kind'] == 'challoc' else ''}")
    log(f"   estimate {mins:.0f} VM-minutes, ${usd:.2f} spot (budget ${L.suite.budget_usd:.0f})")
    if usd > L.suite.budget_usd and not args.allow_cost:
        print(f"cb launch: estimated ${usd:.2f} is over the suite budget ${L.suite.budget_usd:.2f}; --allow-cost to run anyway", file=sys.stderr)
        return 3
    if args.dry_run:
        print(json.dumps(L.launch_doc() if hasattr(L, "harness") else {"vms": L.vms, "shards": L.shards, "cells": L.cell_info, "pending_cells": L.pending_cells}, indent=1, default=str))
        return 0
    out = args.out or os.path.join(ROOT_DIR, "target", "results", "cb", L.run)
    os.makedirs(out, exist_ok=True)
    if not L.vms:
        log(f"cb launch: every cell is PENDING ({'; '.join(f'{k}: {v}' for k, v in L.pending_cells.items())}); nothing to run")
        _write(os.path.join(out, "launch-summary.json"), {"run": L.run, "headline": "PENDING", "pending_cells": L.pending_cells})
        return 2
    try:
        log("== bundle")
        L.bundle()
        L.times["bundled"] = time.time() - L.t0
        log(f"   harness {L.harness[:12]}, {len(L.sources)} variant trees, image {', '.join(f'{k}={v[-12:]}' for k, v in L.image.items()) or 'none (VMs build it)'} ({L.times['bundled']:.0f} s)")
        log("== create")
        max_run_s = max([_dur_s(L.vm_max_run(n)) for n in L.vms] or [0])
        deadline = time.time() + max_run_s + 120
        L.create_pending()
        L.times["created"] = time.time() - L.t0
        log("== poll")
        L.poll(deadline)
        L.times["look1_done"] = time.time() - L.t0
        dirs = L.download(out)
        summary = L.analyze(dirs) if dirs else None
        look = 1
        if summary and summary["headline"] == "INCONCLUSIVE" and L.extend(summary, dirs):
            look = 2
            L.poll(time.time() + max_run_s + 120)
            L.times["look2_done"] = time.time() - L.t0
            dirs = L.download(out)
            summary = L.analyze(dirs)
    except (InfraError, BuildError, Exception) as e:
        if not isinstance(e, (InfraError, BuildError)):
            import traceback

            traceback.print_exc()
        log(f"cb launch: {e}")
        created = [n for n, s in L.state.items() if s["status"] in ("created", "creating")]
        L.delete([n for n in created if L.state[n].get("zone")])
        _write(os.path.join(out, "launch-summary.json"), {"run": L.run, "error": str(e), "cost": L.cost()})
        return 3
    finally:
        L.times["verdict"] = time.time() - L.t0
        cleanup(L)
    failed = {n: s.get("why") for n, s in L.state.items() if s["status"] in ("failed", "timeout")}
    lost = [n for n, s in L.state.items() if s["status"] in ("failed", "timeout") and not any(
        o.get("rescheduled_from") == n and o["status"] == "done" for o in L.state.values())]
    ls = {
        "schema": "challoc.bench.launch-summary/v1",
        "run": L.run,
        "suite": L.suite.name,
        "cells": L.cell_names,
        "looks": look,
        "vms": {n: {**L.vms[n], **{k: v for k, v in s.items() if k in ("status", "zone", "spot", "why", "rescheduled_from")}} for n, s in L.state.items()},
        "failed": failed,
        "shards_lost": lost,
        "wall_s": {k: round(v, 1) for k, v in L.times.items()},
        "vm_times_median_s": vm_timings(dirs),
        "cost": L.cost(),
        "pending_cells": L.pending_cells,
        "images": {n: {k: ci.get(k) for k in ("family", "image", "image_project", "image_created")} for n, ci in L.cell_info.items()},
        "headline": summary["headline"] if summary else "INFRA",
    }
    _write(os.path.join(out, "launch-summary.json"), ls)
    if not summary:
        log("cb launch: no samples came back")
        return 3
    summary["launch"] = {k: ls[k] for k in ("wall_s", "vm_times_median_s", "cost", "looks", "shards_lost", "pending_cells", "images")}
    _write(os.path.join(out, "summary.json"), summary)
    text = report.render(summary)
    text += (
        f"\nwall clock {L.times['verdict'] / 60:.1f} min (bundle {L.times.get('bundled', 0):.0f} s, created {L.times.get('created', 0):.0f} s, "
        f"first VM done {L.times.get('first_done', 0):.0f} s); VM medians {ls['vm_times_median_s']}; "
        f"{ls['cost']['vms_created']} VMs, {ls['cost']['vm_minutes']} VM-minutes, about ${ls['cost']['usd_estimate']}"
        + (f"; lost shards {', '.join(lost)}" if lost else "") + "\n"
        + "".join(f"\nPENDING {k}: {v}\n" for k, v in L.pending_cells.items())
    )
    with open(os.path.join(out, "report.md"), "w") as f:
        f.write(text)
    print()
    print(text)
    log(f"results: {os.path.relpath(out, ROOT_DIR)}")
    if lost and summary["exit"] == 0:
        return 2
    return summary["exit"]


def cleanup(L: Launch) -> None:
    """Delete anything still labelled with this run, whatever happened, and
    wait (up to 3 min) until GCE lists none of them."""
    t = time.time()
    while True:
        try:
            rest = L.api.instances(f"labels.cb-run={L.run}")
        except InfraError as e:
            log(f"cleanup: {e}; run `cb gc --run {L.run}`")
            return
        if not rest:
            return
        if time.time() - t > 180:
            log(f"cleanup: {len(rest)} VMs of {L.run} still listed after 3 min; run `cb gc --run {L.run} --older-than 0`")
            return
        for i in rest:
            if i["status"] not in ("STOPPING",) and not i.get("_asked"):
                try:
                    L.api.delete_instance(i["zone"], i["name"])
                except InfraError:
                    pass
        time.sleep(10)


def _dur_s(s: str) -> float:
    s = str(s)
    mult = {"s": 1, "m": 60, "h": 3600}
    if s[-1] in mult:
        return float(s[:-1]) * mult[s[-1]]
    return float(s)


def _write(path: str, obj: Any) -> None:
    with open(path + ".tmp", "w") as f:
        json.dump(obj, f, indent=1, default=str)
        f.write("\n")
    os.replace(path + ".tmp", path)


def gc(args: Any) -> int:
    """Delete stray cb VMs: name cb-*, label purpose=challoc-bench, older
    than --older-than minutes (and of --run, when given)."""
    cells = load_cells([])
    project = args.project or cells[next(iter(cells))].get("project")
    flt = f"name~^cb- AND labels.purpose=challoc-bench" + (f" AND labels.cb-run={args.run}" if args.run else "")
    got = gcloud("compute", "instances", "list", f"--filter={flt}", "--format=json(name,zone,creationTimestamp,status,labels)", project=project, check=True)
    items = json.loads(got.stdout or "[]")
    now = datetime.datetime.now(datetime.timezone.utc)
    doomed = collections.defaultdict(list)
    for i in items:
        age = (now - datetime.datetime.fromisoformat(i["creationTimestamp"])).total_seconds() / 60
        if age >= args.older_than:
            doomed[i["zone"].rsplit("/", 1)[-1]].append(i["name"])
            print(f"{'would delete' if args.dry_run else 'deleting'} {i['name']} ({i['status']}, {age:.0f} min, run {i.get('labels', {}).get('cb-run')})")
    if not args.dry_run:
        for z, ns in doomed.items():
            gcloud("compute", "instances", "delete", *ns, f"--zone={z}", project=project, check=False)
    print(f"cb gc: {sum(len(v) for v in doomed.values())} VMs {'to delete' if args.dry_run else 'deleted'}, {len(items)} cb VMs seen")
    return 0


def add_launch_args(p: Any) -> None:
    p.add_argument("suite")
    p.add_argument("--base", default="main")
    p.add_argument("--head", default="HEAD")
    p.add_argument("--variants")
    p.add_argument("--controls")
    p.add_argument("--rows")
    p.add_argument("--cells", help="cell selector (default the suite's, e.g. tag:gate); ref-amd64 for one arch")
    p.add_argument("--cells-path", action="append")
    p.add_argument("--container", help="container profile on every cell, overriding the suite and cells: default, privileged, perfmon, plus ,cpus=N,memory=SIZE")
    p.add_argument("--var", action="append", help="NAME=VALUE for a {NAME} in rows.toml, passed to every VM (e.g. valkey_soak_secs=300)")
    p.add_argument("--replicas", type=int, help="VMs per (cell, shard) (default the suite's)")
    p.add_argument("--blocks", type=int)
    p.add_argument("--warmup", type=int)
    p.add_argument("--looks", type=int, help="1 disables the second look")
    p.add_argument("--extend-replicas", type=int)
    p.add_argument("--shard-seconds", type=int)
    p.add_argument("--seed", type=int)
    p.add_argument("--max-vms", type=int, help="VMs alive at once (default CB_MAX_VMS or 32)")
    p.add_argument("--project")
    p.add_argument("--bucket")
    p.add_argument("--service-account")
    p.add_argument("--run-id")
    p.add_argument("--out")
    p.add_argument("--no-pmu", action="store_true", help="do not request the PMU on the VMs")
    p.add_argument("--no-image", action="store_true", help="ignore bench/image.lock: every VM builds the image (slow)")
    p.add_argument("--no-reschedule", action="store_true")
    p.add_argument("--allow-cost", action="store_true")
    p.add_argument("--dry-run", action="store_true")
