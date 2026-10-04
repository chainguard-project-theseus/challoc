"""Host capture (host.json), the comparability key, the quiet gate and the
host-side preflight.

Everything is read from /proc and /sys, so it works in the dev container
(which sees the Docker VM's kernel) and on a Linux host alike. Values that
are absent are recorded as absent, never guessed.
"""

from __future__ import annotations

import glob
import gzip
import os
import platform
import re
import shutil
import socket
import subprocess
import time
from typing import Any

# The /proc/config.gz keys worth a line in host.json.
CONFIG_KEYS = re.compile(
    r"^(CONFIG_(RSEQ|SCHED_MM_CID|PSI|MEMCG|PERF_EVENTS|HZ|HZ_[0-9]+|TRANSPARENT_HUGEPAGE|TRANSPARENT_HUGEPAGE_[A-Z]+|"
    r"ARM64_MTE|ARM64_PTR_AUTH|ARM64_BTI|ARM64_VA_BITS|ARM64_[0-9]+K_PAGES|X86_5LEVEL|ADDRESS_MASKING|USERFAULTFD|"
    r"PAGE_SIZE_[0-9]+KB|SECCOMP|SECCOMP_FILTER|CGROUPS|ANON_VMA_NAME|MSEAL_SYSTEM_MAPPINGS|LRU_GEN|NUMA_BALANCING|"
    r"PREEMPT[A-Z_]*|NO_HZ[A-Z_]*|SLUB[A-Z_]*|RANDOMIZE_BASE))="
)

# ARM part numbers (MIDR) of the cores the cells run on.
ARM_PARTS = {
    "0xd0c": "Neoverse-N1",
    "0xd40": "Neoverse-V1",
    "0xd49": "Neoverse-N2",
    "0xd4f": "Neoverse-V2",
    "0xd8e": "Neoverse-N3",
    "0xd84": "Neoverse-V3",
    "0x000": "Apple-virtual",
}


def _read(path: str) -> str | None:
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read().strip()
    except OSError:
        return None


def _bracket(s: str | None) -> str | None:
    """The selected value of a sysfs choice list: `always [madvise] never`."""
    if s is None:
        return None
    m = re.search(r"\[([^\]]+)\]", s)
    return m.group(1) if m else s


def loadavg() -> tuple[float, float, float]:
    s = _read("/proc/loadavg")
    if s:
        a = s.split()
        return (float(a[0]), float(a[1]), float(a[2]))
    try:
        return os.getloadavg()
    except OSError:
        return (0.0, 0.0, 0.0)


def cpu_times() -> tuple[int, int]:
    """(total, steal) jiffies from /proc/stat's cpu line."""
    s = _read("/proc/stat") or ""
    for line in s.splitlines():
        if line.startswith("cpu "):
            f = [int(x) for x in line.split()[1:]]
            steal = f[7] if len(f) > 7 else 0
            return (sum(f[:8]), steal)
    return (0, 0)


def steal_pct(interval: float = 0.5) -> float:
    t0, s0 = cpu_times()
    time.sleep(interval)
    t1, s1 = cpu_times()
    return 100.0 * (s1 - s0) / (t1 - t0) if t1 > t0 else 0.0


def cpu_model() -> dict[str, Any]:
    info = _read("/proc/cpuinfo") or ""
    out: dict[str, Any] = {}
    for line in info.splitlines():
        k, _, v = line.partition(":")
        k, v = k.strip(), v.strip()
        if k == "model name" and "model" not in out:
            out["model"] = v
        elif k == "microcode" and "microcode" not in out:
            out["microcode"] = v
        elif k == "CPU implementer" and "implementer" not in out:
            out["implementer"] = v
        elif k == "CPU part" and "part" not in out:
            out["part"] = v
        elif k == "CPU variant" and "variant" not in out:
            out["variant"] = v
        elif k in ("flags", "Features") and "flags" not in out:
            out["flags"] = v
    if "model" not in out and "part" in out:
        out["model"] = ARM_PARTS.get(out["part"], f"arm-{out.get('implementer', '?')}-{out['part']}")
    if "model" not in out:
        out["model"] = platform.processor() or platform.machine()
    return out


def thp() -> dict[str, Any]:
    base = "/sys/kernel/mm/transparent_hugepage"
    out: dict[str, Any] = {
        "enabled": _bracket(_read(f"{base}/enabled")),
        "defrag": _bracket(_read(f"{base}/defrag")),
        "shmem_enabled": _bracket(_read(f"{base}/shmem_enabled")),
    }
    sizes = {}
    for d in sorted(glob.glob(f"{base}/hugepages-*kB")):
        sizes[os.path.basename(d)[len("hugepages-") :]] = _bracket(_read(f"{d}/enabled"))
    if sizes:
        out["sizes"] = sizes
    kh = {}
    for f in ("pages_to_scan", "scan_sleep_millisecs", "alloc_sleep_millisecs", "max_ptes_none", "defrag"):
        v = _read(f"{base}/khugepaged/{f}")
        if v is not None:
            kh[f] = v
    if kh:
        out["khugepaged"] = kh
    return out


def caches() -> list[dict[str, str]]:
    out = []
    for d in sorted(glob.glob("/sys/devices/system/cpu/cpu0/cache/index*")):
        e = {k: _read(f"{d}/{k}") for k in ("level", "type", "size", "shared_cpu_list")}
        out.append({k: v for k, v in e.items() if v is not None})
    return out


def kernel_config() -> dict[str, str] | None:
    try:
        with gzip.open("/proc/config.gz", "rt") as f:
            return dict(line.strip().split("=", 1) for line in f if CONFIG_KEYS.match(line))
    except OSError:
        return None


def glibc_version() -> str | None:
    try:
        return os.confstr("CS_GNU_LIBC_VERSION")
    except (ValueError, OSError):
        return None


def in_container() -> str | None:
    if os.path.exists("/.dockerenv"):
        return "docker"
    if os.path.exists("/run/.containerenv"):
        return "podman"
    return None


def metadata_server() -> str:
    """Whether this process can open a connection to the cloud metadata
    server (a cb launch VM blocks it from the container): reachable,
    blocked or absent."""
    try:
        with socket.create_connection(("169.254.169.254", 80), timeout=1):
            return "reachable"
    except ConnectionRefusedError:
        return "blocked"
    except OSError:
        return "absent"


def pmu_devices() -> list[str]:
    """The kernel's perf event sources (/sys/bus/event_source/devices):
    the core PMU (armv8_pmuv3_0, cpu, cpu_core), uncore units, and arm_spe_0
    where the Statistical Profiling Extension is exposed and its module is
    loaded (prep `modprobe:arm_spe_pmu`)."""
    try:
        return sorted(os.listdir("/sys/bus/event_source/devices"))
    except OSError:
        return []


def perf_version() -> str | None:
    """`perf version` of the perf on PATH (the bundle boot.sh mounts at
    /opt/perf on a cb VM), None when there is none."""
    exe = shutil.which("perf")
    if not exe:
        return None
    try:
        r = subprocess.run([exe, "version"], capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return None
    if r.returncode != 0:
        return f"broken: {r.stderr.strip()[:120]}"
    return r.stdout.strip()


def capture(cell: str, extra: dict[str, Any] | None = None) -> dict[str, Any]:
    cpu = cpu_model()
    flags = cpu.pop("flags", "")
    governor = _read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
    mem = _read("/proc/meminfo") or ""
    mt = re.search(r"MemTotal:\s+(\d+)", mem)
    host: dict[str, Any] = {
        "schema": "challoc.bench.host/v1",
        "cell": cell,
        # In a container the hostname is the container id; the Makefile passes the host's.
        "hostname": os.environ.get("CB_HOST") or socket.gethostname(),
        "machine": os.environ.get("CB_MACHINE") or ("local" if cell == "local" else ""),
        "zone": os.environ.get("CB_ZONE", ""),
        "spot": os.environ.get("CB_SPOT", "") == "1",
        "arch": platform.machine(),
        "kernel": platform.release(),
        "kernel_version": platform.version(),
        "boot_id": _read("/proc/sys/kernel/random/boot_id"),
        "cpu": cpu,
        "cpu_flags": flags.split(),
        "nproc": os.cpu_count(),
        "nproc_available": len(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else os.cpu_count(),
        "numa_nodes": len(glob.glob("/sys/devices/system/node/node[0-9]*")),
        "caches": caches(),
        "page_size": os.sysconf("SC_PAGE_SIZE"),
        "mem_total_kib": int(mt.group(1)) if mt else None,
        "thp": thp(),
        "governor": governor,
        "perf_event_paranoid": _read("/proc/sys/kernel/perf_event_paranoid"),
        "pmu_devices": pmu_devices(),
        "perf": perf_version(),
        "glibc": glibc_version(),
        "container": in_container(),
        "image": os.environ.get("CB_IMAGE", ""),
        "host_cpu": os.environ.get("HOST_CPU", ""),
        "kernel_config": kernel_config(),
        "loadavg": loadavg(),
    }
    if os.environ.get("CB_ZONE"):
        host["metadata_server"] = metadata_server()
    if extra:
        host.update(extra)
    return host


def comparability_key(host: dict[str, Any], profile: str = "default") -> str:
    """machine/cpu-model/kernel/image/container-profile. The stats engine
    never pools data across keys; a local run's key names the host."""
    machine = host.get("machine") or "local"
    if machine == "local":
        machine = f"local:{host.get('hostname', '?')}"
    model = str(host.get("cpu", {}).get("model", "?")).replace("/", "-").replace(" ", "-")
    image = host.get("image") or "noimage"
    return f"{machine}/{model}/{host.get('kernel', '?')}/{image}/{profile}"


def quiet_gate(load1: float, wait_s: float, log=print) -> dict[str, Any]:
    """Wait up to wait_s for the 1-minute load below load1. Never fails:
    a host that does not get quiet is recorded as noisy."""
    t0 = time.monotonic()
    while True:
        l1 = loadavg()[0]
        if l1 < load1:
            return {"quiet": True, "load1": l1, "waited_s": round(time.monotonic() - t0, 1)}
        if time.monotonic() - t0 >= wait_s:
            log(f"== quiet gate: load1 {l1:.2f} still >= {load1} after {wait_s:.0f} s; continuing, host marked noisy")
            return {"quiet": False, "load1": l1, "waited_s": round(time.monotonic() - t0, 1)}
        time.sleep(5)


def procs_running() -> int:
    for line in (_read("/proc/stat") or "").splitlines():
        if line.startswith("procs_running "):
            return int(line.split()[1])
    return 1


def quiet_gate_running(running: float, wait_s: float, log=print) -> dict[str, Any]:
    """The quiet gate of a freshly booted cell VM: load1 lags by minutes
    after the image pull and the build, so wait (at most wait_s) until the
    runnable-task count, sampled over 3 s, averages at most `running` (the
    sampler itself is one) and steal is under 0.5%. Never fails; a host
    that does not settle is recorded as noisy."""
    t0 = time.monotonic()
    while True:
        xs = []
        c0, s0 = cpu_times()
        for _ in range(6):
            xs.append(procs_running())
            time.sleep(0.5)
        c1, s1 = cpu_times()
        mean = sum(xs) / len(xs)
        steal = 100.0 * (s1 - s0) / (c1 - c0) if c1 > c0 else 0.0
        waited = round(time.monotonic() - t0, 1)
        if mean <= running and steal < 0.5:
            return {"quiet": True, "running": mean, "steal_pct": round(steal, 3), "load1": loadavg()[0], "waited_s": waited}
        if waited >= wait_s:
            log(f"== quiet gate: {mean:.1f} runnable, steal {steal:.2f}% after {waited:.0f} s; continuing, host marked noisy")
            return {"quiet": False, "running": mean, "steal_pct": round(steal, 3), "load1": loadavg()[0], "waited_s": waited}


def preflight() -> list[str]:
    """Host-side checks before a local run (bench-needs-quiet-host): the
    load average and foreign containers. Returns warnings."""
    warns = []
    l1 = loadavg()[0]
    ncpu = os.cpu_count() or 1
    if l1 > max(2.0, ncpu / 2):
        warns.append(f"load average {l1:.1f} on {ncpu} CPUs: numbers from this run are noise")
    if shutil.which("docker"):
        r = subprocess.run(["docker", "ps", "--format", "{{.Image}} {{.Names}}"], capture_output=True, text=True)
        others = [ln for ln in r.stdout.splitlines() if ln.strip()]
        if others:
            ryuk = [ln for ln in others if "ryuk" in ln]
            warns.append(f"{len(others)} container(s) running{' (testcontainers ryuk: another session is running tests)' if ryuk else ''}: " + "; ".join(others[:5]))
    return warns
