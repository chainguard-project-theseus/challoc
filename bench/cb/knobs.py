"""Host knobs: sysfs, debugfs and sysctl settings a variant runs under.

A variant is (library, MALLOC_CONF, host knobs). Knobs that can be flipped
at run time are written by the executor right before each run of the
variant and put back to the host's value when a variant without that knob
runs, so a knob gets the same within-host ABBA pairing as a library
(`cb run`, run.py). Writing them needs a writable /sys (and debugfs for the
`sched.` and `rseq.` knobs): the container profile `privileged`; cb launch
refuses a knob variant on any other profile.

Names (the value is written as given; a sysfs choice file such as THP's
`always [madvise] never` reads back as its bracketed word):

  thp, thp.enabled          /sys/kernel/mm/transparent_hugepage/enabled
  thp.defrag, thp.shmem_enabled
  thp.<N>kB                 .../hugepages-<N>kB/enabled (mTHP sizes)
  khugepaged.<file>         .../khugepaged/<file>
  sched.llc_balancing       /sys/kernel/debug/sched/llc_balancing/enabled
                            (kernel 7.2 cache-aware scheduling)
  sched.<file>              /sys/kernel/debug/sched/<file>
  rseq.<file>               /sys/kernel/debug/rseq/<file> (slice_ext_nsec)
  sysctl.<a.b.c>            /proc/sys/a/b/c
"""

from __future__ import annotations

import os
import re
import subprocess

from .spec import SpecError

THP = "/sys/kernel/mm/transparent_hugepage"
DEBUGFS = "/sys/kernel/debug"


class KnobError(Exception):
    pass


def path_of(name: str) -> str:
    if name in ("thp", "thp.enabled"):
        return f"{THP}/enabled"
    if name in ("thp.defrag", "thp.shmem_enabled"):
        return f"{THP}/{name[4:]}"
    m = re.fullmatch(r"thp\.(\d+kB)", name)
    if m:
        return f"{THP}/hugepages-{m.group(1)}/enabled"
    m = re.fullmatch(r"khugepaged\.(\w+)", name)
    if m:
        return f"{THP}/khugepaged/{m.group(1)}"
    if name == "sched.llc_balancing":
        d = f"{DEBUGFS}/sched/llc_balancing"
        return f"{d}/enabled" if os.path.isdir(d) or not os.path.exists(d) else d
    m = re.fullmatch(r"(sched|rseq)\.(\w+)", name)
    if m:
        return f"{DEBUGFS}/{m.group(1)}/{m.group(2)}"
    m = re.fullmatch(r"sysctl\.([\w.]+)", name)
    if m:
        return "/proc/sys/" + m.group(1).replace(".", "/")
    raise KnobError(f"unknown knob {name!r} (bench/cb/knobs.py lists the names)")


def check_names(knobs: dict[str, str], where: str) -> None:
    for k in knobs:
        try:
            path_of(k)
        except KnobError as e:
            raise SpecError(f"{where}: {e}") from None


def _read(path: str) -> str:
    with open(path) as f:
        v = f.read().strip()
    m = re.search(r"\[([^\]]+)\]", v)
    return m.group(1) if m else v


def _ensure_debugfs(path: str) -> None:
    if not path.startswith(DEBUGFS + "/") or os.path.exists(path):
        return
    if os.path.isdir(DEBUGFS) and not os.listdir(DEBUGFS):
        subprocess.run(["mount", "-t", "debugfs", "debugfs", DEBUGFS], capture_output=True)


def read(name: str) -> str:
    p = path_of(name)
    _ensure_debugfs(p)
    try:
        return _read(p)
    except OSError as e:
        raise KnobError(f"knob {name}: cannot read {p}: {e.strerror} (does this kernel have it?)") from None


def write(name: str, value: str) -> None:
    p = path_of(name)
    _ensure_debugfs(p)
    try:
        with open(p, "w") as f:
            f.write(value)
    except OSError as e:
        hint = " (knobs need the privileged container profile)" if e.errno in (13, 30, 1) else ""
        raise KnobError(f"knob {name}={value}: cannot write {p}: {e.strerror}{hint}") from None
    got = _read(p)
    if got != value:
        raise KnobError(f"knob {name}: wrote {value!r}, reads back {got!r}")


class Knobs:
    """The host's original value of every knob any variant sets; `apply`
    puts the host in one variant's state, `restore` back to the original."""

    def __init__(self, variant_knobs: list[dict[str, str]]) -> None:
        names = sorted({k for kn in variant_knobs for k in kn})
        self.original = {n: read(n) for n in names}
        self.current = dict(self.original)

    def apply(self, knobs: dict[str, str]) -> dict[str, str]:
        want = {n: knobs.get(n, orig) for n, orig in self.original.items()}
        for n, v in want.items():
            if self.current.get(n) != v:
                write(n, v)
                self.current[n] = v
        return {n: v for n, v in want.items() if n in knobs}

    def restore(self) -> None:
        for n, v in self.original.items():
            if self.current.get(n) != v:
                try:
                    write(n, v)
                    self.current[n] = v
                except KnobError:
                    pass
