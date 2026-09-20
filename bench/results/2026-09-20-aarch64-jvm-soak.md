# JVM benchmark rows: the mixed-size soak, 2026-09-20, aarch64

- commit: 585e20b
- date: 2026-09-20T15:40:03Z
- host: Neoverse-V2, 16 CPUs visible to the container; GCE c4a-standard-16 (Google Axion, Neoverse V2), zone us-east4-a
- kernel: 7.0.0-1011-gcp, container: Wolfi
- java: /usr/lib/jvm/java-21-openjdk/bin/java (Wolfi openjdk-21-21.0.12.1-r2); `java -version`:
      openjdk version "21.0.12.1" 2026-08-18
      OpenJDK Runtime Environment openjdk-21-21.0.12.1-r2 (build 21.0.12.1+-wolfi-r2)
      OpenJDK 64-Bit Server VM openjdk-21-21.0.12.1-r2 (build 21.0.12.1+-wolfi-r2, mixed mode, sharing)
- generator: This is ApacheBench, Version 2.3 <$Revision$> (Wolfi apache2-utils 2.4.68); wrk and oha are not in Wolfi
- load average at start: 6.79 8.55 8.24; at end: 6.48 7.23 7.10; 6207 s total
- method: bench/jvm/HttpBench.java in `soak` mode (com.sun.net.httpserver on a ThreadPoolExecutor of 16 core and 400 maximum threads, idle threads retired after 2 s, a SynchronousQueue; `-Xms512m -Xmx512m -XX:+AlwaysPreTouch -Dsun.net.httpserver.nodelay=true -XX:NativeMemoryTracking=summary`, so the Java heap is constant and resident from the start and RSS differs by native memory only), served once per allocator for 1200 s to two `ab -k` processes (one on /json, a ~300-byte body built per request; one on /blob, a body of a log-uniform size between 256 and 1048576 bytes drawn per request and copied out of a per-request ByteBuffer.allocateDirect of that size, i.e. one malloc(n) per request in the JVM with n spread over every size class from 256 bytes to 1 MiB, freed when the buffer's Cleaner runs after a GC) in 20 windows of 60 s alternating 256 total connections (ab -c 128 x2) and 8 (ab -c 4 x2), so the pool grows to hundreds of threads and shrinks to 16 every other minute all run long; RSS sampled every 5 s from /proc/<pid>/smaps_rollup (Rss) and /proc/<pid>/status (VmHWM, Threads) into a CSV per allocator (beside this file): idle = the last sample before the load (10 s after the server answered), peak = the largest sample, final = the last sample after the load, slope = the least-squares slope of Rss over the samples of the last 600 s in MiB per minute (growth after warm-up is the fragmentation signal), tail mean = the mean Rss over the same samples; req/s and p99 (ms) are ab's per endpoint per window, summarised over the high and the low windows; CPU us/req is the server's user+sys over a window divided by the requests both generators completed in it; NMT (`jcmd VM.native_memory summary`) at the end. Percentages are challoc against the other allocator: positive means challoc is larger, slower or serves fewer requests.
- allocators:
  - challoc: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD 585e20b, sha256 9d2aacce25fe33dd)
  - glibc: no LD_PRELOAD (glibc 2.44 malloc, default arenas: up to 8 x CPUs)
  - glibc-arena2: no LD_PRELOAD, MALLOC_ARENA_MAX=2 (glibc 2.44 malloc; the fleet's usual mitigation for the per-thread arenas)
  - glibc-trim: no LD_PRELOAD, `-XX:+UnlockExperimentalVMOptions -XX:TrimNativeHeapInterval=5000` (glibc 2.44 malloc; the JVM's service thread calls malloc_trim(0) every 5000 ms: what a well-tuned Java image ships today)
  - jemalloc: LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1)

## Mixed-size soak, 1200 s per allocator (pool 16..400 threads, keep-alive 2 s; /blob log-uniform 256..1048576 bytes; 60 s windows alternating 256 and 8 connections)

| allocator | idle Rss MiB | peak Rss MiB | final Rss MiB | tail mean Rss MiB (last 600 s) | slope MiB/min (last 600 s) | VmHWM MiB | NMT committed MiB (end) | allocator overhead MiB (end) | json req/s high | json p99 ms high (max) | blob req/s high | blob p99 ms high (max) | blob MiB/s high | json req/s low | blob req/s low | CPU us/req high | CPU us/req low | max threads | largest pool | GCs | exit |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 582.0 | 1271.0 | 1081.7 | 1174.6 | -3.68 | 1271.8 | 683.5 | 398.3 | 72606 | 4 | 44149 | 89 | 5251 | 62029 | 34163 | 53.6 | 56.4 | 116 | 83 | 20726 | 0 |
| glibc | 602.5 | 1433.2 | 1327.2 | 1327.8 | -0.98 | 1483.0 | 714.7 | 612.5 | 73716 | 4 | 43878 | 91 | 5219 | 64101 | 36388 | 49.2 | 53.2 | 71 | 116 | 21359 | 0 |
| glibc-arena2 | 578.7 | 1098.6 | 1030.6 | 1038.0 | -1.91 | 1108.1 | 765.5 | 265.1 | 74478 | 4 | 44085 | 89 | 5242 | 63530 | 36197 | 49.5 | 53.7 | 68 | 81 | 21392 | 0 |
| glibc-trim | 600.6 | 1145.4 | 889.3 | 959.6 | -2.59 | 1285.1 | 885.7 | 3.6 | 73920 | 4 | 43417 | 91 | 5160 | 64871 | 36358 | 50.7 | 53.9 | 74 | 72 | 21289 | 0 |
| jemalloc | 585.8 | 2065.6 | 1653.9 | 1777.5 | -1.85 | 2179.4 | 726.3 | 927.5 | 74464 | 4 | 43264 | 89 | 5142 | 63762 | 35411 | 51.4 | 55.1 | 78 | 76 | 21060 | 0 |

### challoc against each allocator (soak; positive = challoc worse; the slope row is the difference in MiB/min, challoc minus the other)

| metric | vs glibc | vs glibc-arena2 | vs glibc-trim | vs jemalloc |
|---|---:|---:|---:|---:|
| peak Rss | -11.3% | +15.7% | +11.0% | -38.5% |
| final Rss | -18.5% | +5.0% | +21.6% | -34.6% |
| tail mean Rss | -11.5% | +13.2% | +22.4% | -33.9% |
| VmHWM | -14.2% | +14.8% | -1.0% | -41.6% |
| allocator overhead (end) | -35.0% | +50.2% | +10856.8% | -57.1% |
| json req/s high | +1.5% | +2.6% | +1.8% | +2.6% |
| json p99 high (max) | +0.0% | +0.0% | +0.0% | +0.0% |
| blob req/s high | -0.6% | -0.1% | -1.7% | -2.0% |
| blob p99 high (max) | -2.2% | +0.0% | -2.2% | +0.0% |
| json req/s low | +3.3% | +2.4% | +4.6% | +2.8% |
| blob req/s low | +6.5% | +6.0% | +6.4% | +3.7% |
| CPU us/req high | +8.9% | +8.3% | +5.7% | +4.3% |
| CPU us/req low | +6.0% | +5.0% | +4.6% | +2.4% |
| slope MiB/min (difference) | -2.70 | -1.77 | -1.09 | -1.83 |

### Rss at the end of each window (MiB; h = 256 connections, l = 8)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 1240.6 | 1072.1 | 1251.6 | 1080.4 | 1243.5 | 1085.2 | 1267.2 | 1093.9 | 1252.7 | 1090.5 | 1234.2 | 1086.5 | 1254.2 | 1089.6 | 1238.3 | 1084.6 | 1224.0 | 1087.1 | 1242.8 | 1081.7 |
| glibc | 1211.7 | 1313.3 | 1317.6 | 1328.8 | 1312.9 | 1292.9 | 1327.0 | 1336.9 | 1317.4 | 1361.4 | 1321.8 | 1373.1 | 1319.2 | 1325.2 | 1303.2 | 1301.2 | 1301.2 | 1325.2 | 1313.2 | 1327.2 |
| glibc-arena2 | 985.3 | 1089.0 | 1031.1 | 1034.4 | 1030.7 | 1030.6 | 1030.6 | 1030.6 | 1030.6 | 1030.6 | 1030.6 | 1044.6 | 1030.6 | 1074.6 | 1030.6 | 1030.6 | 1030.6 | 1030.6 | 1030.6 | 1030.6 |
| glibc-trim | 954.7 | 989.3 | 1081.5 | 903.1 | 810.1 | 869.7 | 1082.2 | 940.2 | 1055.5 | 865.6 | 974.4 | 935.2 | 1145.4 | 941.9 | 715.6 | 927.8 | 1117.4 | 926.8 | 719.4 | 907.1 |
| jemalloc | 1921.3 | 1485.4 | 1724.8 | 1577.6 | 1798.6 | 1510.9 | 1976.4 | 1495.8 | 1956.3 | 1627.1 | 1924.7 | 1535.4 | 1856.7 | 1566.8 | 1727.1 | 1661.9 | 1932.5 | 1570.3 | 1921.8 | 1653.9 |

### Threads (/proc/<pid>/status) at the end of each window

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 79 | 49 | 81 | 51 | 85 | 51 | 71 | 51 | 84 | 51 | 77 | 51 | 94 | 51 | 76 | 51 | 86 | 51 | 78 | 51 |
| glibc | 66 | 49 | 63 | 51 | 61 | 51 | 62 | 51 | 64 | 51 | 59 | 51 | 61 | 51 | 59 | 51 | 65 | 51 | 66 | 51 |
| glibc-arena2 | 63 | 49 | 61 | 51 | 66 | 51 | 62 | 51 | 62 | 51 | 67 | 51 | 64 | 51 | 68 | 51 | 64 | 51 | 67 | 51 |
| glibc-trim | 70 | 50 | 66 | 52 | 58 | 52 | 66 | 52 | 70 | 52 | 68 | 52 | 65 | 52 | 60 | 52 | 68 | 52 | 64 | 52 |
| jemalloc | 73 | 49 | 65 | 51 | 66 | 51 | 76 | 51 | 72 | 51 | 72 | 51 | 70 | 51 | 63 | 51 | 72 | 51 | 71 | 51 |

### req/s per window (json / blob)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 72395 / 43905 | 61892 / 34249 | 72833 / 44394 | 61914 / 34171 | 72568 / 43887 | 62066 / 34020 | 72813 / 44167 | 61493 / 34220 | 72681 / 44120 | 61156 / 33996 | 72484 / 44123 | 62506 / 34315 | 72709 / 44412 | 62332 / 34233 | 72680 / 44154 | 62836 / 34242 | 71974 / 43829 | 62170 / 34068 | 72927 / 44496 | 61927 / 34120 |
| glibc | 72261 / 43379 | 64060 / 36375 | 73944 / 43983 | 64644 / 36667 | 73420 / 43983 | 63801 / 36099 | 73499 / 43734 | 64273 / 36447 | 74016 / 44051 | 64162 / 36422 | 74203 / 43997 | 63362 / 36171 | 73941 / 43860 | 64374 / 36275 | 73689 / 44013 | 64563 / 36473 | 73926 / 43830 | 63699 / 36424 | 74257 / 43953 | 64075 / 36527 |
| glibc-arena2 | 74264 / 43926 | 63538 / 36239 | 74524 / 44043 | 63615 / 36146 | 75317 / 43970 | 63426 / 36325 | 74361 / 44138 | 63654 / 36143 | 74363 / 44053 | 63587 / 36169 | 74686 / 44276 | 63777 / 36384 | 74532 / 44103 | 63250 / 36074 | 73784 / 44169 | 63620 / 36223 | 73976 / 44019 | 63197 / 36043 | 74977 / 44155 | 63640 / 36227 |
| glibc-trim | 72969 / 43691 | 64884 / 36277 | 75046 / 44099 | 65067 / 36541 | 74087 / 43861 | 65493 / 36735 | 74805 / 44208 | 65289 / 36638 | 74682 / 44023 | 64980 / 36451 | 74919 / 43601 | 65433 / 36347 | 73573 / 43303 | 65089 / 36326 | 73108 / 42213 | 63483 / 35970 | 73568 / 42792 | 64389 / 36075 | 72439 / 42383 | 64606 / 36223 |
| jemalloc | 73185 / 42779 | 64267 / 35377 | 75044 / 43449 | 64477 / 35747 | 74884 / 43501 | 63556 / 35292 | 74056 / 43056 | 63944 / 35558 | 75303 / 43546 | 63496 / 35317 | 73384 / 42412 | 62696 / 34988 | 75008 / 43643 | 64135 / 35572 | 75556 / 44249 | 63235 / 35504 | 74792 / 43334 | 64115 / 35436 | 73427 / 42671 | 63700 / 35318 |

### p99 ms per window (json / blob)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 4 / 88 | 0 / 0 | 4 / 87 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 87 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 88 | 0 / 0 |
| glibc | 4 / 91 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 90 | 0 / 0 | 4 / 90 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 90 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 89 | 0 / 0 |
| glibc-arena2 | 4 / 89 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 88 | 0 / 0 |
| glibc-trim | 4 / 90 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 87 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 87 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 90 | 0 / 0 | 4 / 89 | 0 / 0 | 4 / 91 | 0 / 0 |
| jemalloc | 4 / 89 | 0 / 0 | 4 / 87 | 0 / 0 | 4 / 87 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 87 | 0 / 0 | 4 / 88 | 0 / 0 | 4 / 86 | 0 / 0 | 4 / 86 | 0 / 0 | 4 / 86 | 0 / 0 | 4 / 88 | 0 / 0 |

### Native Memory Tracking at the end (MiB, the last sample; "allocator overhead" = Rss - NMT committed at the same instant)

| allocator | Rss MiB at the sample | NMT committed MiB | allocator overhead MiB (Rss - NMT) | malloc'd MiB (all categories) | mmap committed MiB | Java Heap | Class | Thread | Code | GC | Compiler | Internal | Other | Symbol | NMT's own overhead MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 1081.7 | 683.5 | 398.3 | 101.3 | 582.2 | 512.0 | 0.6 | 2.6 | 10.0 | 70.2 | 0.2 | 1.4 | 68.8 | 1.3 | 0.4 |
| glibc | 1327.2 | 714.7 | 612.5 | 131.5 | 583.2 | 512.0 | 0.6 | 3.3 | 10.2 | 70.0 | 0.2 | 1.4 | 99.3 | 1.3 | 0.4 |
| glibc-arena2 | 1030.6 | 765.5 | 265.1 | 182.7 | 582.8 | 512.0 | 0.6 | 3.2 | 9.8 | 70.0 | 0.2 | 1.4 | 150.5 | 1.3 | 0.4 |
| glibc-trim | 889.3 | 885.7 | 3.6 | 302.8 | 582.9 | 512.0 | 0.6 | 3.3 | 9.9 | 70.0 | 0.2 | 1.4 | 270.6 | 1.3 | 0.4 |
| jemalloc | 1653.9 | 726.3 | 927.5 | 143.8 | 582.6 | 512.0 | 0.6 | 3.3 | 9.6 | 70.1 | 0.2 | 1.4 | 111.5 | 1.3 | 0.4 |

NMT (`-XX:NativeMemoryTracking=summary`, `jcmd <pid> VM.native_memory summary scale=KB`) is the JVM's own account of the native memory it asked for: "committed" counts the bytes the JVM malloc'd (with NMT's 16-byte header each) plus the pages it committed by mmap (Java heap, metaspace, code cache, thread stacks and GC structures). Rss minus NMT committed at the same instant is what the allocator adds on its own account (caching, size-class rounding, fragmentation, retained pages, metadata) less what the JVM committed but never touched (NMT counts mmap commits, not resident pages, so the gap understates the allocator by the same untouched amount under every variant). "malloc'd" is NMT's malloc total across categories: `Other` holds the direct ByteBuffers (`Unsafe.allocateMemory`), `Internal` the JVM's miscellany, `Thread` the stacks (touched pages) and per-thread arenas, `Class` metaspace, `Code` the code cache, `GC` the card table and remembered sets, `Compiler` the JIT arenas. NMT's own overhead (its header per malloc plus its tables, the last column) is the same bytes for every allocator, so the comparisons between allocators hold; the header does push a malloc(65536) to 65552 bytes, across a power-of-two class boundary in every size-class allocator (see the reading). jcmd is a second JVM started from the harness shell (no LD_PRELOAD) and wakes the server's attach listener (one more thread, the same for every variant).

### Final Rss by mapping (MiB, /proc/<pid>/smaps at the last sample; "anon" is unnamed anonymous memory: the Java heap, metaspace, code cache, thread stacks and every allocator's unnamed arenas; challoc names its regions)

| allocator | [heap] | anon | file-backed | named anonymous regions |
|---|---:|---:|---:|---|
| challoc | 0.0 | 537.3 | 35.2 | challoc:classes=246.8 challoc:huge=237.6 challoc:meta=24.8 |
| glibc | 38.7 | 1253.7 | 34.8 | none (kernel without anonymous VMA names, or no allocator names its mappings) |
| glibc-arena2 | 230.2 | 765.5 | 34.9 | none (kernel without anonymous VMA names, or no allocator names its mappings) |
| glibc-trim | 2.8 | 851.5 | 34.9 | none (kernel without anonymous VMA names, or no allocator names its mappings) |
| jemalloc | 0.0 | 1616.8 | 37.0 | none (kernel without anonymous VMA names, or no allocator names its mappings) |

Series: 2026-09-20-aarch64-jvm-soak-challoc.csv 2026-09-20-aarch64-jvm-soak-glibc.csv 2026-09-20-aarch64-jvm-soak-glibc-arena2.csv 2026-09-20-aarch64-jvm-soak-glibc-trim.csv 2026-09-20-aarch64-jvm-soak-jemalloc.csv (elapsed_s,phase,rss_kb,pss_kb,anon_kb,vmrss_kb,vmhwm_kb,threads, one line per 5 s; phase is idle, w<i>:high|low or final) and 2026-09-20-aarch64-jvm-soak-challoc-windows.csv 2026-09-20-aarch64-jvm-soak-glibc-windows.csv 2026-09-20-aarch64-jvm-soak-glibc-arena2-windows.csv 2026-09-20-aarch64-jvm-soak-glibc-trim-windows.csv 2026-09-20-aarch64-jvm-soak-jemalloc-windows.csv (one line per window: ab's req/s and p99 per endpoint, the server's CPU per request, Rss, threads and the pool's size at the window's end). Raw: /src/target/jvm/bench-out/soak.<allocator>/{samples.csv,windows.csv,w<i>-json.ab,w<i>-blob.ab,gc.log,smaps-by-name.txt,nmt-peak.txt,nmt-final.txt,stats.txt,stderr}.

## Reading

- The question this soak was built for (the human, 2026-09-20: the 5-minute
  server row with one buffer size never lets glibc's arenas fragment, and
  the JVM's biggest native consumers are mmapped by the JVM itself): does
  a growing and shrinking pool with mixed-size direct buffers separate the
  allocators? It does. Stock glibc sits at 1301..1373 MiB from the second
  window on and never gives anything back (its low windows: 1313, 1329,
  1293, 1337, 1361, 1373, 1325, 1301, 1325, 1327 MiB; peak 1433, VmHWM
  1483), jemalloc's 64 KiB-page build reaches 2066 MiB (VmHWM 2179) and
  holds 1486..1662 in its low windows, while challoc oscillates between
  1224..1267 at the end of the high windows and 1072..1094 at the end of
  the low ones, glibc-arena2 holds a fixed 1031 and glibc-trim swings
  716..1145. At the end glibc holds 613 MiB more than the JVM asked for
  (Rss 1327 against NMT committed 715), jemalloc 928, challoc 398,
  glibc-arena2 265, glibc-trim 4. NMT's own categories are within 1 MiB of
  each other for everything but `Other` (the live direct buffers at the
  instant of the sample, 69..271 MiB, GC timing); the Java heap is the same
  512 MiB under every variant.
- Criterion (a row more than 10% behind the best of glibc, glibc-trim and
  jemalloc on RSS, slope, req/s or p99 gets the mechanism named): challoc
  is behind glibc-trim on peak Rss (+11.0%: 1271 against 1145 MiB), final
  Rss (+21.6%: 1082 against 889) and tail mean (+22.4%: 1175 against 960),
  and behind glibc-arena2 (not in the criterion's set) by +15.7% / +5.0% /
  +13.2%; VmHWM is even with glibc-trim (-1.0%) and +14.8% over
  glibc-arena2. Ahead of stock glibc on every RSS figure (peak -11.3%,
  final -18.5%, tail mean -11.5%, VmHWM -14.2%) and far ahead of jemalloc
  (-38.5%, -34.6%, -33.9%, -41.6%). Slope: challoc's -3.68 MiB/min is the
  lowest of the five (glibc-trim -2.59, glibc-arena2 -1.91, jemalloc
  -1.85, glibc -0.98); every slope is negative here (the last low
  window's decay still under way at the end) and no allocator grows
  across the run on this VM: challoc's high-window maxima are 1244, 1264,
  1257, 1267, 1252, 1258, 1255, 1253, 1270, 1258 MiB. req/s within the
  band (json high -1.5..-2.6%, blob high +0.1..+2.0% more, low windows
  -2.4..-6.5%), p99 within the band (4 ms json, 89 ms blob against 89..91).
- Mechanism of the RSS gap against glibc-trim and glibc-arena2, from
  challoc's own stats print at exit (`bench/probes/statsdump.so`,
  `malloc_stats_print`, 2 s after the last low window): allocated 171.0
  MB, active 305.4 MB, resident 650.8 MB, mapped 1139.4 MB, retained 488.6
  MB, 64 arenas, `opt.dirty_decay_ms: 10000`, background thread absent.
  Two parts. (1) 301 MiB of dirty pages, 291 of them counted with arena 0
  because the huge allocator (every direct buffer above 256 KiB, three
  quarters of the bytes of a log-uniform draw between 256 bytes and 1 MiB)
  follows arena 0: `Huge: 121 runs (19132 pages), 2 empty extents (56849
  pages)`, 75 MiB live against 222 MiB of empty extents waiting inside the
  10 s decay window (`purge::decay` walks every arena and the huge
  allocator on any thread's periodic tick, so these are the last seconds'
  frees, not an idle arena's leftovers; over the run 198710 eager purges
  returned 204.5 GB, 170 MB/s). (2) Slab occupancy: active 305 MB against
  allocated 171 MB (56%): the Cleaner frees the buffers of one GC in a
  burst and leaves every touched slab partly occupied by the survivors
  (small classes allocated 58 / active 110 MB, large classes 105 / 181).
  The final smaps agrees: `challoc:classes` 246.8 + `challoc:huge` 237.6 +
  `challoc:meta` 24.8 = 509 MiB against 101.3 MiB malloc'd per NMT.
  glibc-trim's `malloc_trim(0)` every 5 s hands back every free page at
  once (its overhead at the end is 3.6 MiB; its Rss drops to 716..810 in
  some windows), glibc-arena2's two arenas keep a fixed 1031 MiB with no
  churn, and challoc returns about 170 MiB within the first 10 s of each
  lull and then holds ~1085 MiB for the rest of the low window. Owner of
  the decay policy: lane B (`src/purge.rs`, `src/policy.rs`); nothing here
  changes `src/`.
- CPU per request: challoc is +8.9% against glibc in the high windows
  (53.6 us against 49.2; +8.3% arena2, +5.7% trim, +4.3% jemalloc) and
  +2.4..+6.0% in the low ones; inside the 10% band, attributed anyway with
  `sudo perf` on the VM host over a 10 s slice of a high window (HttpBench
  soak under each allocator, two `ab -c 128`, after 20 s of load;
  `.tmp-vm-perf-soak.sh`): challoc 141.4 G cycles, 162.6 G instructions,
  151.9k minor faults, 72.7 s task-clock; glibc 135.9 G, 160.1 G, 1.7k
  faults, 70.3 s; glibc-trim 132.9 G, 156.6 G, 89.0k faults, 68.8 s.
  +1.6% instructions, +4.0% cycles, +3.5% task-clock, and 150k more minor
  faults than glibc in 10 s: glibc's arenas keep the freed direct-buffer
  pages resident (1.7k faults in 10 s, the flat 1301..1373 MiB), challoc
  purges them with MADV_DONTNEED inside the 10 s decay (198710 eager
  purges, 204.5 GB over the run, 170 MB/s) and the next `direct.put`
  faults them back in, glibc-trim's `malloc_trim` does the same at half
  the rate (89k). libchalloc.so is 1.41% of samples against libc.so.6's
  4.11% under glibc (malloc, memcpy and the socket writes together), so the
  allocator's own instructions are not the gap; the kernel (52.6% against
  53.0%) and the JIT code (35.6% against 36.9%) carry the fault handling
  and the stalls.
- The pool did not reach 256 threads: the largest pool size was 72..116
  (`/stats`) and the peak thread count 68..116 (`/proc/<pid>/status`),
  because the blob generator was the limit in the high windows (ab's
  single thread at 93..94% of a core moving 5.1..5.3 GiB/s of log-uniform
  bodies through 128 keep-alive connections; the json generator at 55%),
  so the server saw fewer than 256 requests in flight. The pool still
  cycled 16 -> ~80..116 -> 16 every two minutes for the whole run (the
  Threads row at the window ends, after the 2 s keep-alive retired the
  extras), which is the thread churn the soak wanted; a multi-process
  generator would raise the ceiling.
- jemalloc: Wolfi's aarch64 jemalloc is the 64 KiB-page build (`Page
  size: 65536` in its print: allocated 186.1 MB, active 477.9 MB, resident
  1209.9 MB, mapped 1321.0 MB, 65 arenas), so every partly freed slab
  keeps a 64 KiB page resident; that is the 2066 MiB peak and the 928 MiB
  of allocator overhead at the end, the same finding as the server row.
- Everything ran in full: 1200 s per allocator, five allocators; the file
  header's total is the soak alone (the server row ran before it in the
  same chain, and again after it with the NMT sampler fix, 585e20b).

## Verdict: is it really better than glibc?

Against glibc as shipped, yes, and by more than on x86_64: peak 1271
against 1433 MiB, tail mean 1175 against 1328, VmHWM 1272 against 1483,
398 against 613 MiB held beyond what the JVM asked for at the end, and
the lowest slope of the five; against jemalloc's 64 KiB-page build it is
not close (1271 against 2066 peak). Against glibc tuned the way a good
Java image ships it, no: glibc-trim's tail mean is 22% below challoc's
(960 against 1175 MiB) and glibc-arena2's 13% (1038), both at equal req/s
and 6..9% less server CPU per request in the high windows; only VmHWM is
even with glibc-trim. The gap is the ~400 MiB challoc holds at the end of
every lull, 301 MiB of dirty pages inside its 10 s decay window (222 of
them the huge allocator's empty extents) and 134 MiB of half-occupied
slabs, against a `malloc_trim(0)` every 5 s that returns everything.
