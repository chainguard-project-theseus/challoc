#!/usr/bin/env python3
"""Fork/COW bench for Valkey (or Redis) under different allocators.

One run: start the server (valkey-server or redis-server), load N keys
"key:%012d" with V-byte values, optionally MEMORY PURGE, BGSAVE stretched
by rdb-key-save-delay, overwrite W random keys at a constant rate during
the save, sample the child's smaps
(Private_Dirty by VMA name) and the parent's RSS, then (optionally) watch
the parent's RSS recover with activedefrag on. Prints one JSON line.
"""
import argparse, json, os, random, socket, subprocess, threading, time

def resp(*args):
    out = [b"*%d\r\n" % len(args)]
    for a in args:
        if isinstance(a, str):
            a = a.encode()
        out.append(b"$%d\r\n%s\r\n" % (len(a), a))
    return b"".join(out)

class Conn:
    def __init__(self, port):
        self.s = socket.create_connection(("127.0.0.1", port))
        self.s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.buf = b""
    def _fill(self):
        d = self.s.recv(1 << 20)
        if not d:
            raise EOFError
        self.buf += d
    def _line(self):
        while b"\r\n" not in self.buf:
            self._fill()
        line, self.buf = self.buf.split(b"\r\n", 1)
        return line
    def _read(self):
        line = self._line()
        t = line[:1]
        if t in (b"+", b":"):
            return line[1:]
        if t == b"-":
            raise RuntimeError(line.decode())
        if t == b"$":
            n = int(line[1:])
            if n < 0:
                return None
            while len(self.buf) < n + 2:
                self._fill()
            v, self.buf = self.buf[:n], self.buf[n + 2:]
            return v
        if t == b"*":
            return [self._read() for _ in range(int(line[1:]))]
        raise RuntimeError("bad reply %r" % line)
    def cmd(self, *a):
        self.s.sendall(resp(*a))
        return self._read()
    def pipeline(self, cmds):
        self.s.sendall(b"".join(cmds))
        for _ in cmds:
            self._read()

def info(c, sec):
    d = {}
    for l in c.cmd("INFO", sec).decode().splitlines():
        if ":" in l:
            k, v = l.split(":", 1)
            d[k] = v
    return d

def smaps_by_name(pid):
    """Private_Dirty bytes summed per span: adjacent anonymous VMAs merged
    into one span (challoc's reservations: the class superblock, the huge
    region, the metadata region are each one contiguous span of VMAs, as
    the kernel here has no CONFIG_ANON_VMA_NAME). A span is keyed by its
    size in GiB and its start; named VMAs keep their name."""
    vmas = []
    try:
        with open("/proc/%d/smaps" % pid) as f:
            for l in f:
                p = l.split()
                if not p:
                    continue
                if not p[0].endswith(":"):
                    s, e = (int(x, 16) for x in p[0].split("-"))
                    name = " ".join(p[5:]) if len(p) > 5 else ""
                    if name.startswith("/"):
                        name = "[file]"
                    vmas.append([s, e, name, 0])
                elif p[0] == "Private_Dirty:":
                    vmas[-1][3] += int(p[1]) * 1024
    except (FileNotFoundError, ProcessLookupError):
        return None
    out = {}
    spans = []
    for s, e, name, d in vmas:
        if name:
            out[name] = out.get(name, 0) + d
            continue
        if spans and spans[-1][1] == s:
            spans[-1][1] = e
            spans[-1][2] += d
        else:
            spans.append([s, e, d])
    for s, e, d in spans:
        size = e - s
        key = "span%.1fG@%x" % (size / (1 << 30), s) if size >= (1 << 30) else "anon<1G"
        out[key] = out.get(key, 0) + d
    return out


def rss(pid):
    try:
        with open("/proc/%d/smaps_rollup" % pid) as f:
            for l in f:
                if l.startswith("Rss:"):
                    return int(l.split()[1]) * 1024
    except FileNotFoundError:
        return None

def children(pid):
    try:
        with open("/proc/%d/task/%d/children" % (pid, pid)) as f:
            return [int(x) for x in f.read().split()]
    except FileNotFoundError:
        return []

def run_bench(a, n):
    out = subprocess.run([a.bench, "-p", str(a.port), "-n", str(n), "-c", "50", "-P", "16", "-r", str(a.keys),
                          "-d", str(a.vsize), "-t", "set", "-q", "--csv"], capture_output=True, text=True).stdout
    for l in out.splitlines():
        if l.startswith('"SET"'):
            return float(l.split(",")[1].strip('"'))
    return None

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--lib", default="")
    ap.add_argument("--label", required=True)
    ap.add_argument("--port", type=int, default=7100)
    ap.add_argument("--keys", type=int, default=10_000_000)
    ap.add_argument("--vsize", type=int, default=64)
    ap.add_argument("--writes", type=int, default=0)
    ap.add_argument("--save-delay", type=int, default=-10)
    ap.add_argument("--expect-s", type=float, default=60)
    ap.add_argument("--purge", action="store_true")
    ap.add_argument("--recover", type=float, default=0, help="seconds to watch RSS after the save")
    ap.add_argument("--defrag", action="store_true")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--malloc-conf", default="")
    ap.add_argument("--bench", default="", help="valkey-benchmark or redis-benchmark: SET throughput outside and inside the save")
    ap.add_argument("--extra", default="", help="extra server args, space separated")
    ap.add_argument("--op", default="SET", help="SET, DEL or GET during the save")
    a = ap.parse_args()

    data = os.environ.get("COWBENCH_DATA", "/tmp/cowbench") + "/%s-%d" % (a.label, a.port)
    os.makedirs(data, exist_ok=True)
    env = dict(os.environ)
    if a.lib:
        env["LD_LIBRARY_PATH"] = a.lib
    if a.malloc_conf:
        env["MALLOC_CONF"] = a.malloc_conf
    srv = subprocess.Popen([a.server, "--port", str(a.port), "--save", "", "--appendonly", "no",
                            "--dir", data, "--rdbcompression", "no", "--enable-debug-command", "yes",
                            "--rdb-key-save-delay", "0", "--io-threads", "1", "--logfile", data + "/log"]
                           + (a.extra.split() if a.extra else []), env=env)
    try:
        for _ in range(200):
            try:
                c = Conn(a.port); c.cmd("PING"); break
            except (ConnectionRefusedError, EOFError):
                time.sleep(0.05)
        rnd = random.Random(a.seed)
        val = ("%x" % rnd.getrandbits(4 * a.vsize)).rjust(a.vsize, "0")[:a.vsize].encode()
        t0 = time.time()
        B = 2000
        for i in range(0, a.keys, B):
            c.pipeline([resp("SET", b"key:%012d" % j, val) for j in range(i, min(i + B, a.keys))])
        im = info(c, "memory")
        r = {"op": a.op, "label": a.label, "keys": a.keys, "vsize": a.vsize, "writes": a.writes, "purge": a.purge,
             "load_s": round(time.time() - t0, 1), "used_memory": int(im["used_memory"]),
             "rss_before": int(im["used_memory_rss"]),
             "encoding": c.cmd("OBJECT", "ENCODING", "key:%012d" % 5).decode(),
             "mem_allocator": im.get("mem_allocator")}
        tput = {}
        if a.bench:
            tput["outside"] = run_bench(a, 300000)
            r["rss_before"] = int(info(c, "memory")["used_memory_rss"])
        if a.purge:
            c.cmd("MEMORY", "PURGE")
            time.sleep(0.2)
            r["rss_after_purge"] = int(info(c, "memory")["used_memory_rss"])
        c.cmd("CONFIG", "SET", "rdb-key-save-delay", str(a.save_delay))
        pid = srv.pid
        t0 = time.time()
        c.cmd("BGSAVE")
        child = None
        for _ in range(500):
            ch = children(pid)
            if ch:
                child = ch[0]; break
            time.sleep(0.01)
        best = {"total": -1}
        peak = [0]
        stop = threading.Event()
        def sampler():
            while not stop.is_set():
                sm = smaps_by_name(child)
                prss = rss(pid)
                if sm is None:
                    break
                tot = sum(sm.values())
                if tot > best["total"]:
                    best.clear()
                    best.update({"total": tot, "by": {k: v for k, v in sm.items() if v > (1 << 20)}})
                if prss:
                    peak[0] = max(peak[0], prss + tot)
                stop.wait(1.0)
        th = threading.Thread(target=sampler); th.start()
        if a.writes:
            w = Conn(a.port)
            steps = 200
            per = max(1, a.writes // steps)
            gap = 0.7 * a.expect_s / steps
            done = 0
            nxt = time.time()
            while done < a.writes:
                n = min(per, a.writes - done)
                if a.op == "SET":
                    w.pipeline([resp("SET", b"key:%012d" % rnd.randrange(a.keys), val) for _ in range(n)])
                else:
                    w.pipeline([resp(a.op, b"key:%012d" % rnd.randrange(a.keys)) for _ in range(n)])
                done += n
                nxt += gap
                d = nxt - time.time()
                if d > 0:
                    time.sleep(d)
            r["write_s"] = round(time.time() - t0, 1)
        if a.bench:
            tput["inside"] = run_bench(a, 300000)
        while info(c, "persistence")["rdb_bgsave_in_progress"] == "1":
            time.sleep(0.2)
        stop.set(); th.join()
        ip = info(c, "persistence")
        r.update({"save_s": round(time.time() - t0, 1), "rdb_last_cow_size": int(ip["rdb_last_cow_size"]),
                  "child_dirty_peak": best["total"], "cow_by_vma": best.get("by", {}),
                  "peak_parent_rss_plus_child_dirty": peak[0],
                  "rss_after_save": int(info(c, "memory")["used_memory_rss"])})
        if tput:
            r["set_ops_s"] = tput
        if a.recover:
            c.cmd("CONFIG", "SET", "rdb-key-save-delay", "0")
            if a.defrag:
                c.cmd("CONFIG", "SET", "activedefrag", "yes")
            series = []
            t1 = time.time()
            while time.time() - t1 < a.recover:
                im = info(c, "memory")
                st = info(c, "stats")
                series.append((round(time.time() - t1), int(im["used_memory_rss"]), int(im["used_memory"]),
                               int(st.get("active_defrag_hits", 0)), int(st.get("active_defrag_running", 0))))
                time.sleep(max(1.0, a.recover / 30))
            r["recover"] = series
            r["rss_end"] = series[-1][1]
            if os.environ.get("COWBENCH_DUMP"):
                with open(os.environ["COWBENCH_DUMP"], "wb") as f:
                    f.write(c.cmd("MEMORY", "MALLOC-STATS"))
                    f.write(b"\n----\n")
                    f.write(c.cmd("INFO", "memory"))
        print(json.dumps(r), flush=True)
    finally:
        srv.terminate(); srv.wait()
        try:
            os.unlink(data + "/dump.rdb")
        except FileNotFoundError:
            pass

if __name__ == "__main__":
    main()
