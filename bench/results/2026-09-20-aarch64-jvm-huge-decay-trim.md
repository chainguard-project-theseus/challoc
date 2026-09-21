# JVM benchmark rows: the mixed-size soak, 2026-09-21, aarch64

- commit: f419b88 (uncommitted changes in src/)
- date: 2026-09-21T01:42:21Z
- host: Neoverse-V2, 16 CPUs visible to the container; GCE c4a-standard-16 (Google Axion, Neoverse V2), zone us-east4-a
- kernel: 7.0.0-1011-gcp, container: Wolfi
- java: /usr/lib/jvm/java-21-openjdk/bin/java (Wolfi openjdk-21-21.0.12.1-r2); `java -version`:
      openjdk version "21.0.12.1" 2026-08-18
      OpenJDK Runtime Environment openjdk-21-21.0.12.1-r2 (build 21.0.12.1+-wolfi-r2)
      OpenJDK 64-Bit Server VM openjdk-21-21.0.12.1-r2 (build 21.0.12.1+-wolfi-r2, mixed mode, sharing)
- generator: This is ApacheBench, Version 2.3 <$Revision$> (Wolfi apache2-utils 2.4.68); wrk and oha are not in Wolfi
- load average at start: 2.94 2.90 3.22; at end: 6.45 6.88 6.12; 1253 s total
- method: bench/jvm/HttpBench.java in `soak` mode (com.sun.net.httpserver on a ThreadPoolExecutor of 16 core and 400 maximum threads, idle threads retired after 2 s, a SynchronousQueue; `-Xms512m -Xmx512m -XX:+AlwaysPreTouch -Dsun.net.httpserver.nodelay=true -XX:NativeMemoryTracking=summary`, so the Java heap is constant and resident from the start and RSS differs by native memory only), served once per allocator for 1200 s to two `ab -k` processes (one on /json, a ~300-byte body built per request; one on /blob, a body of a log-uniform size between 256 and 1048576 bytes drawn per request and copied out of a per-request ByteBuffer.allocateDirect of that size, i.e. one malloc(n) per request in the JVM with n spread over every size class from 256 bytes to 1 MiB, freed when the buffer's Cleaner runs after a GC) in 20 windows of 60 s alternating 256 total connections (ab -c 128 x2) and 8 (ab -c 4 x2), so the pool grows to hundreds of threads and shrinks to 16 every other minute all run long; RSS sampled every 5 s from /proc/<pid>/smaps_rollup (Rss) and /proc/<pid>/status (VmHWM, Threads) into a CSV per allocator (beside this file): idle = the last sample before the load (10 s after the server answered), peak = the largest sample, final = the last sample after the load, slope = the least-squares slope of Rss over the samples of the last 600 s in MiB per minute (growth after warm-up is the fragmentation signal), tail mean = the mean Rss over the same samples; req/s and p99 (ms) are ab's per endpoint per window, summarised over the high and the low windows; CPU us/req is the server's user+sys over a window divided by the requests both generators completed in it; NMT (`jcmd VM.native_memory summary`) at the end. Percentages are challoc against the other allocator: positive means challoc is larger, slower or serves fewer requests.
- allocators:
  - challoc-trim: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 7ed09e4e6424620f) with `-XX:+UnlockExperimentalVMOptions -XX:TrimNativeHeapInterval=5000` (the JVM's service thread calls challoc's malloc_trim(0) every 5000 ms: every Empty slab and huge extent purged regardless of age, glibc-trim's economics on challoc)

## Mixed-size soak, 1200 s per allocator (pool 16..400 threads, keep-alive 2 s; /blob log-uniform 256..1048576 bytes; 60 s windows alternating 256 and 8 connections)

| allocator | idle Rss MiB | peak Rss MiB | final Rss MiB | tail mean Rss MiB (last 600 s) | slope MiB/min (last 600 s) | VmHWM MiB | NMT committed MiB (end) | allocator overhead MiB (end) | json req/s high | json p99 ms high (max) | blob req/s high | blob p99 ms high (max) | blob MiB/s high | json req/s low | blob req/s low | CPU us/req high | CPU us/req low | max threads | largest pool | GCs | exit |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-trim | 580.0 | 1201.5 | 846.2 | 1116.1 | -6.30 | 1225.8 | 701.2 | 144.9 | 77050 | 4 | 44646 | 85 | 5309 | 67929 | 36334 | 51.2 | 53.6 | 127 | 95 | 21815 | 0 |

### Rss at the end of each window (MiB; h = 256 connections, l = 8)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-trim | 1025.7 | 1042.7 | 1174.6 | 1062.8 | 1111.8 | 1033.3 | 1152.1 | 1050.5 | 900.6 | 957.2 | 1155.1 | 1057.9 | 1166.4 | 1015.1 | 934.8 | 1052.6 | 1168.5 | 1059.8 | 880.3 | 846.2 |

### Threads (/proc/<pid>/status) at the end of each window

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-trim | 107 | 50 | 76 | 52 | 78 | 52 | 65 | 52 | 74 | 52 | 66 | 52 | 70 | 52 | 78 | 52 | 69 | 52 | 74 | 52 |

### req/s per window (json / blob)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-trim | 75637 / 44317 | 68309 / 36524 | 75964 / 44491 | 68219 / 36331 | 77193 / 44800 | 68473 / 36304 | 77024 / 44659 | 68219 / 36320 | 77459 / 44766 | 67019 / 36203 | 77515 / 44630 | 67365 / 36484 | 77041 / 44471 | 67284 / 36372 | 77426 / 44639 | 67930 / 36337 | 77562 / 44836 | 68223 / 36082 | 77683 / 44847 | 68248 / 36385 |

### p99 ms per window (json / blob)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-trim | 4 / 84 | 0 / 0 | 4 / 85 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 84 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 |

### Native Memory Tracking at the end (MiB, the last sample; "allocator overhead" = Rss - NMT committed at the same instant)

| allocator | Rss MiB at the sample | NMT committed MiB | allocator overhead MiB (Rss - NMT) | malloc'd MiB (all categories) | mmap committed MiB | Java Heap | Class | Thread | Code | GC | Compiler | Internal | Other | Symbol | NMT's own overhead MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-trim | 846.2 | 701.2 | 144.9 | 119.5 | 581.8 | 512.0 | 0.6 | 2.3 | 9.8 | 70.1 | 0.2 | 1.4 | 87.1 | 1.3 | 0.4 |

NMT (`-XX:NativeMemoryTracking=summary`, `jcmd <pid> VM.native_memory summary scale=KB`) is the JVM's own account of the native memory it asked for: "committed" counts the bytes the JVM malloc'd (with NMT's 16-byte header each) plus the pages it committed by mmap (Java heap, metaspace, code cache, thread stacks and GC structures). Rss minus NMT committed at the same instant is what the allocator adds on its own account (caching, size-class rounding, fragmentation, retained pages, metadata) less what the JVM committed but never touched (NMT counts mmap commits, not resident pages, so the gap understates the allocator by the same untouched amount under every variant). "malloc'd" is NMT's malloc total across categories: `Other` holds the direct ByteBuffers (`Unsafe.allocateMemory`), `Internal` the JVM's miscellany, `Thread` the stacks (touched pages) and per-thread arenas, `Class` metaspace, `Code` the code cache, `GC` the card table and remembered sets, `Compiler` the JIT arenas. NMT's own overhead (its header per malloc plus its tables, the last column) is the same bytes for every allocator, so the comparisons between allocators hold; the header does push a malloc(65536) to 65552 bytes, across a power-of-two class boundary in every size-class allocator (see the reading). jcmd is a second JVM started from the harness shell (no LD_PRELOAD) and wakes the server's attach listener (one more thread, the same for every variant).

### Final Rss by mapping (MiB, /proc/<pid>/smaps at the last sample; "anon" is unnamed anonymous memory: the Java heap, metaspace, code cache, thread stacks and every allocator's unnamed arenas; challoc names its regions)

| allocator | [heap] | anon | file-backed | named anonymous regions |
|---|---:|---:|---:|---|
| challoc-trim | 0.0 | 536.6 | 35.2 | challoc:classes=166.8 challoc:huge=81.4 challoc:meta=26.1 |

Series: 2026-09-20-aarch64-jvm-huge-decay-trim-challoc-trim.csv (elapsed_s,phase,rss_kb,pss_kb,anon_kb,vmrss_kb,vmhwm_kb,threads, one line per 5 s; phase is idle, w<i>:high|low or final) and 2026-09-20-aarch64-jvm-huge-decay-trim-challoc-trim-windows.csv (one line per window: ab's req/s and p99 per endpoint, the server's CPU per request, Rss, threads and the pool's size at the window's end). Raw: /src/target/jvm/soak-out/soak.<allocator>/{samples.csv,windows.csv,w<i>-json.ab,w<i>-blob.ab,gc.log,smaps-by-name.txt,nmt-peak.txt,nmt-final.txt,stats.txt,stderr}.
