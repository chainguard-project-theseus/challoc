"""Variant libraries: challoc built from git revs, and the stock allocators.

A challoc variant is built from `git archive REV` into its own source
directory and CARGO_TARGET_DIR under target/cb/, keyed by the rev's tree
hash, so two labels of one tree (an A/A) share one build and a rebuilt rev
is a cache hit. Paths are remapped (--remap-path-prefix), so identical
sources give a byte-identical .so; the manifest records every library's
sha256 and flags two different trees with identical libraries (the change
does not reach the library) and one tree with two different libraries
(a nondeterministic build). Runs where cargo works: the dev container or a
Linux host.
"""

from __future__ import annotations

import dataclasses
import hashlib
import os
import shutil
import subprocess
from typing import Any

from .plan import STOCK, VariantSpec
from .spec import SpecError


class BuildError(Exception):
    pass


# Sources shipped with a launch (cb launch, bench/cb/gce.py): a cell VM has
# the harness tree but no git repository, so the launcher resolves every rev
# to (commit, tree) and uploads `git archive` of each tree. `pin_sources`
# records both; resolve_rev and build_challoc then use them instead of git.
PINNED: dict[str, str] = {}  # commit -> tree
SOURCE_DIR = ""  # holds <tree>.tar.gz


def pin_sources(pins: dict[str, str], source_dir: str) -> None:
    global SOURCE_DIR
    PINNED.update(pins)
    SOURCE_DIR = source_dir


@dataclasses.dataclass
class Variant:
    spec: VariantSpec
    lib: str  # LD_PRELOAD path; "" for glibc
    sha256: str
    commit: str = ""
    tree: str = ""
    cached: bool = False

    @property
    def label(self) -> str:
        return self.spec.label

    def to_json(self) -> dict[str, Any]:
        return {
            "label": self.label,
            "kind": self.spec.kind,
            "source": self.spec.source,
            "conf": self.spec.conf,
            "role": self.spec.role,
            "lib": self.lib,
            "lib_sha256": self.sha256,
            "commit": self.commit,
            "tree": self.tree,
            "cached": self.cached,
        }


def git(root: str, *args: str, input: bytes | None = None) -> str:
    r = subprocess.run(["git", "-c", "safe.directory=*", "-C", root, *args], capture_output=True, input=input)
    if r.returncode != 0:
        raise BuildError(f"git {' '.join(args)}: {r.stderr.decode(errors='replace').strip()}")
    return r.stdout.decode().strip()


def resolve_rev(root: str, rev: str) -> tuple[str, str]:
    """(commit, tree) of a rev; the tree is what the build is keyed on."""
    if rev in PINNED:
        return rev, PINNED[rev]
    commit = git(root, "rev-parse", "--verify", f"{rev}^{{commit}}")
    tree = git(root, "rev-parse", f"{commit}^{{tree}}")
    return commit, tree


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _cargo_build(srcdir: str, tdir: str, log: str, features: str = "") -> None:
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = tdir
    env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + f" --remap-path-prefix={srcdir}=/challoc").strip()
    cmd = ["cargo", "build", "--release"]
    if features:
        cmd += ["--features", features]
    with open(log, "wb") as f:
        r = subprocess.run(cmd, cwd=srcdir, env=env, stdout=f, stderr=subprocess.STDOUT)
    text = open(log, encoding="utf-8", errors="replace").read()
    if r.returncode != 0:
        errs = [ln for ln in text.splitlines() if ln.startswith(("error", "warning"))][:20]
        raise BuildError(f"cargo build in {srcdir} failed (see {log}):\n" + "\n".join(errs))
    # A "Finished" without "Compiling challoc" is a stale reuse, not a build.
    if "Compiling challoc" not in text:
        raise BuildError(f"cargo reused a previous build in {tdir} instead of compiling (stale mtimes?); see {log}")


def build_challoc(root: str, spec: VariantSpec, work: str, log=print) -> Variant:
    os.makedirs(work, exist_ok=True)
    if spec.source == "tree":
        tdir = os.path.join(work, "build", "tree")
        os.makedirs(tdir, exist_ok=True)
        # Touch the crate root: rsynced or restored files may carry old mtimes.
        os.utime(os.path.join(root, "src", "lib.rs"))
        _cargo_build(root, tdir, os.path.join(tdir, "build.log"))
        so = os.path.join(tdir, "release", "libchalloc.so")
        commit = git(root, "rev-parse", "HEAD")
        return Variant(spec, so, sha256_file(so), commit=commit + "+tree", tree="tree")
    commit, tree = resolve_rev(root, spec.source)
    key = tree[:16]
    tdir = os.path.join(work, "build", key)
    so = os.path.join(tdir, "release", "libchalloc.so")
    stamp = os.path.join(tdir, "cb-built")
    if os.path.exists(so) and os.path.exists(stamp) and open(stamp).read().strip() == tree:
        log(f"   {spec.label}: {spec.source} = {commit[:12]} (tree {key}), cached")
        return Variant(spec, so, sha256_file(so), commit=commit, tree=tree, cached=True)
    srcdir = os.path.join(work, "src", key)
    shutil.rmtree(srcdir, ignore_errors=True)
    os.makedirs(srcdir)
    shipped = os.path.join(SOURCE_DIR, f"{tree}.tar.gz") if SOURCE_DIR else ""
    if shipped and os.path.exists(shipped):
        subprocess.run(["tar", "-xzf", shipped, "-C", srcdir], check=True)
    else:
        archive = subprocess.run(["git", "-c", "safe.directory=*", "-C", root, "archive", commit], capture_output=True)
        if archive.returncode != 0:
            raise BuildError(f"git archive {commit}: {archive.stderr.decode(errors='replace')}")
        subprocess.run(["tar", "-x", "-C", srcdir], input=archive.stdout, check=True)
    if not os.path.exists(os.path.join(srcdir, "Cargo.toml")):
        raise BuildError(f"git archive {spec.source} produced no Cargo.toml")
    # git archive stamps files with the commit time; make every file newer
    # than any earlier build in this target dir.
    for dp, _, fns in os.walk(srcdir):
        for fn in fns:
            os.utime(os.path.join(dp, fn))
    os.makedirs(tdir, exist_ok=True)
    if os.path.exists(stamp):
        os.unlink(stamp)
    log(f"   {spec.label}: building {spec.source} = {commit[:12]} (tree {key})")
    _cargo_build(srcdir, tdir, os.path.join(tdir, "build.log"))
    if not os.path.exists(so):
        raise BuildError(f"build of {spec.source} produced no {so}")
    with open(stamp, "w") as f:
        f.write(tree + "\n")
    return Variant(spec, so, sha256_file(so), commit=commit, tree=tree)


def stock_variant(spec: VariantSpec, values: dict[str, str]) -> Variant:
    lib = STOCK[spec.source]["lib"]
    if lib:
        lib = lib.format(**values) if "{" in lib else lib
        if not os.path.exists(lib):
            raise BuildError(f"{spec.label}: {lib} does not exist (the dev image has jemalloc and tcmalloc; mimalloc comes from bench/build.sh)")
        return Variant(spec, lib, sha256_file(lib))
    return Variant(spec, "", "")


def prepare(root: str, specs: list[VariantSpec], work: str, values: dict[str, str], log=print) -> list[Variant]:
    """Every variant's library. Distinct challoc trees build in parallel
    (the release profile is fat LTO with one codegen unit, so one build
    leaves most CPUs idle); labels of one tree share the first build."""
    for s in specs:
        if s.kind not in ("stock", "challoc"):
            raise SpecError(f"variant {s.label}: unknown kind {s.kind}")
    first: dict[str, VariantSpec] = {}
    for s in specs:
        if s.kind == "challoc":
            key = s.source if s.source == "tree" else resolve_rev(root, s.source)[1]
            first.setdefault(key, s)
    built: dict[str, Variant] = {}
    if len(first) > 1:
        import concurrent.futures

        with concurrent.futures.ThreadPoolExecutor(len(first)) as ex:
            futs = {k: ex.submit(build_challoc, root, s, work, log) for k, s in first.items()}
            for k, f in futs.items():
                built[k] = f.result()
    out = []
    for s in specs:
        if s.kind == "stock":
            out.append(stock_variant(s, values))
            continue
        key = s.source if s.source == "tree" else resolve_rev(root, s.source)[1]
        if key in built and built[key].spec is s:
            out.append(built[key])
        else:
            out.append(build_challoc(root, s, work, log))
    return out


def identity_notes(variants: list[Variant]) -> list[str]:
    """The quick.sh build check: identical libraries from different trees,
    different libraries from one tree."""
    notes = []
    ch = [v for v in variants if v.spec.kind == "challoc"]
    for i, a in enumerate(ch):
        for b in ch[i + 1 :]:
            if a.tree == b.tree and a.sha256 != b.sha256:
                notes.append(f"WARNING: {a.label} and {b.label} are one tree but the .so files differ: nondeterministic build?")
            elif a.tree != b.tree and a.sha256 == b.sha256:
                notes.append(f"NOTE: {a.label} and {b.label} differ in source but the .so files are byte-identical: the change does not reach the library")
            elif a.tree == b.tree:
                notes.append(f"{a.label} and {b.label} are the same build ({a.sha256[:16]})")
    return notes
