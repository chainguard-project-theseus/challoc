# JVM benchmark rows: the mixed-size soak, 2026-09-20, x86_64

- commit: f419b88 (uncommitted changes in src/)
- date: 2026-09-20T23:08:52Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; GCE c4-standard-16 (Intel Xeon Platinum 8581C), zone us-east4-a
- kernel: 7.0.0-1011-gcp, container: Wolfi
- java: /usr/lib/jvm/java-21-openjdk/bin/java (Wolfi openjdk-21-21.0.12.1-r2); `java -version`:
      openjdk version "21.0.12.1" 2026-08-18
      OpenJDK Runtime Environment openjdk-21-21.0.12.1-r2 (build 21.0.12.1+-wolfi-r2)
      OpenJDK 64-Bit Server VM openjdk-21-21.0.12.1-r2 (build 21.0.12.1+-wolfi-r2, mixed mode, sharing)
- generator: This is ApacheBench, Version 2.3 <$Revision$> (Wolfi apache2-utils 2.4.68); wrk and oha are not in Wolfi
- load average at start: 1.32 0.53 0.19; at end: 9.55 11.50 11.80; 7687 s total
- method: bench/jvm/HttpBench.java in `soak` mode (com.sun.net.httpserver on a ThreadPoolExecutor of 16 core and 400 maximum threads, idle threads retired after 2 s, a SynchronousQueue; `-Xms512m -Xmx512m -XX:+AlwaysPreTouch -Dsun.net.httpserver.nodelay=true -XX:NativeMemoryTracking=summary`, so the Java heap is constant and resident from the start and RSS differs by native memory only), served once per allocator for 1200 s to two `ab -k` processes (one on /json, a ~300-byte body built per request; one on /blob, a body of a log-uniform size between 256 and 1048576 bytes drawn per request and copied out of a per-request ByteBuffer.allocateDirect of that size, i.e. one malloc(n) per request in the JVM with n spread over every size class from 256 bytes to 1 MiB, freed when the buffer's Cleaner runs after a GC) in 20 windows of 60 s alternating 256 total connections (ab -c 128 x2) and 8 (ab -c 4 x2), so the pool grows to hundreds of threads and shrinks to 16 every other minute all run long; RSS sampled every 5 s from /proc/<pid>/smaps_rollup (Rss) and /proc/<pid>/status (VmHWM, Threads) into a CSV per allocator (beside this file): idle = the last sample before the load (10 s after the server answered), peak = the largest sample, final = the last sample after the load, slope = the least-squares slope of Rss over the samples of the last 600 s in MiB per minute (growth after warm-up is the fragmentation signal), tail mean = the mean Rss over the same samples; req/s and p99 (ms) are ab's per endpoint per window, summarised over the high and the low windows; CPU us/req is the server's user+sys over a window divided by the requests both generators completed in it; NMT (`jcmd VM.native_memory summary`) at the end. Percentages are challoc against the other allocator: positive means challoc is larger, slower or serves fewer requests.
- allocators:
  - challoc: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 856dfded39db7edb)
  - challoc-hd2000: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 856dfded39db7edb) with CHALLOC_CONF=huge_dirty_decay_ms:2000 (the same library as challoc under the key)
  - challoc-hd500: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 856dfded39db7edb) with CHALLOC_CONF=huge_dirty_decay_ms:500 (the same library as challoc under the key)
  - challoc-hd0: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 856dfded39db7edb) with CHALLOC_CONF=huge_dirty_decay_ms:0 (the same library as challoc under the key)
  - glibc-arena2: no LD_PRELOAD, MALLOC_ARENA_MAX=2 (glibc 2.44 malloc; the fleet's usual mitigation for the per-thread arenas)
  - glibc-trim: no LD_PRELOAD, `-XX:+UnlockExperimentalVMOptions -XX:TrimNativeHeapInterval=5000` (glibc 2.44 malloc; the JVM's service thread calls malloc_trim(0) every 5000 ms: what a well-tuned Java image ships today)

## Mixed-size soak, 1200 s per allocator (pool 16..400 threads, keep-alive 2 s; /blob log-uniform 256..1048576 bytes; 60 s windows alternating 256 and 8 connections)

| allocator | idle Rss MiB | peak Rss MiB | final Rss MiB | tail mean Rss MiB (last 600 s) | slope MiB/min (last 600 s) | VmHWM MiB | NMT committed MiB (end) | allocator overhead MiB (end) | json req/s high | json p99 ms high (max) | blob req/s high | blob p99 ms high (max) | blob MiB/s high | json req/s low | blob req/s low | CPU us/req high | CPU us/req low | max threads | largest pool | GCs | exit |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 585.8 | 1226.6 | 1032.1 | 1121.1 | -2.55 | 1237.0 | 706.5 | 325.6 | 92865 | 4 | 35760 | 47 | 4246 | 76386 | 27145 | 68.8 | 58.1 | 149 | 118 | 21435 | 0 |
| challoc-hd2000 | 585.9 | 1242.9 | 1012.4 | 1105.4 | -4.72 | 1249.7 | 745.8 | 266.6 | 91629 | 4 | 35490 | 44 | 4214 | 77624 | 25571 | 71.5 | 58.8 | 163 | 130 | 21182 | 0 |
| challoc-hd500 | 585.8 | 1254.0 | 942.2 | 1092.8 | -3.50 | 1272.3 | 646.8 | 295.4 | 89661 | 4 | 35031 | 41 | 4162 | 78448 | 24076 | 75.4 | 59.4 | 182 | 147 | 21056 | 0 |
| challoc-hd0 | 585.9 | 1226.0 | 870.4 | 1050.4 | -4.59 | 1257.0 | 651.8 | 218.6 | 85579 | 4 | 35420 | 39 | 4216 | 78735 | 22775 | 83.7 | 64.9 | 143 | 111 | 20404 | 0 |
| glibc-arena2 | 577.7 | 989.0 | 986.5 | 985.8 | +0.33 | 997.4 | 773.4 | 213.1 | 97158 | 3 | 35591 | 47 | 4219 | 76838 | 28545 | 64.2 | 56.3 | 122 | 98 | 22014 | 0 |
| glibc-trim | 576.0 | 1018.2 | 632.5 | 864.7 | -13.80 | 1079.3 | 617.4 | 15.1 | 97196 | 3 | 36010 | 47 | 4271 | 77027 | 28247 | 63.9 | 56.7 | 127 | 91 | 22024 | 0 |

### challoc against each allocator (soak; positive = challoc worse; the slope row is the difference in MiB/min, challoc minus the other)

| metric | vs challoc-hd2000 | vs challoc-hd500 | vs challoc-hd0 | vs glibc-arena2 | vs glibc-trim |
|---|---:|---:|---:|---:|---:|
| peak Rss | -1.3% | -2.2% | +0.0% | +24.0% | +20.5% |
| final Rss | +1.9% | +9.5% | +18.6% | +4.6% | +63.2% |
| tail mean Rss | +1.4% | +2.6% | +6.7% | +13.7% | +29.7% |
| VmHWM | -1.0% | -2.8% | -1.6% | +24.0% | +14.6% |
| allocator overhead (end) | +22.2% | +10.2% | +48.9% | +52.8% | +2060.7% |
| json req/s high | -1.3% | -3.5% | -7.8% | +4.6% | +4.7% |
| json p99 high (max) | +0.0% | +0.0% | +0.0% | +33.3% | +33.3% |
| blob req/s high | -0.8% | -2.0% | -1.0% | -0.5% | +0.7% |
| blob p99 high (max) | +6.8% | +14.6% | +20.5% | +0.0% | +0.0% |
| json req/s low | +1.6% | +2.7% | +3.1% | +0.6% | +0.8% |
| blob req/s low | -5.8% | -11.3% | -16.1% | +5.2% | +4.1% |
| CPU us/req high | -3.8% | -8.8% | -17.8% | +7.2% | +7.7% |
| CPU us/req low | -1.2% | -2.2% | -10.5% | +3.2% | +2.5% |
| slope MiB/min (difference) | +2.17 | +0.95 | +2.04 | -2.88 | +11.25 |

### Rss at the end of each window (MiB; h = 256 connections, l = 8)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 1168.6 | 1003.0 | 1206.4 | 1026.8 | 1168.5 | 1027.3 | 1184.1 | 1028.3 | 1197.3 | 1004.4 | 1162.5 | 1023.5 | 1214.2 | 1014.8 | 1207.0 | 1019.8 | 1206.2 | 1024.7 | 1209.4 | 1032.1 |
| challoc-hd2000 | 1146.0 | 961.5 | 1082.1 | 994.3 | 1137.5 | 1010.7 | 1058.3 | 1022.7 | 1136.3 | 1018.8 | 1085.5 | 1003.9 | 1132.2 | 975.5 | 1133.0 | 1014.8 | 1162.8 | 976.8 | 1100.9 | 1012.4 |
| challoc-hd500 | 1170.1 | 909.3 | 1066.7 | 987.8 | 1117.4 | 969.4 | 1072.7 | 950.6 | 1204.7 | 1013.2 | 1084.4 | 972.6 | 1107.9 | 975.6 | 1079.2 | 1019.8 | 1219.6 | 1027.8 | 1110.9 | 942.2 |
| challoc-hd0 | 1076.0 | 887.6 | 1158.6 | 832.7 | 1144.8 | 863.9 | 1186.5 | 912.0 | 1107.1 | 968.8 | 1037.6 | 937.4 | 1102.9 | 935.6 | 1129.3 | 894.3 | 1087.0 | 968.3 | 1104.9 | 870.4 |
| glibc-arena2 | 984.2 | 984.1 | 984.5 | 983.9 | 984.2 | 984.1 | 983.4 | 984.3 | 984.1 | 984.1 | 984.9 | 983.7 | 983.8 | 986.0 | 985.2 | 986.6 | 986.5 | 986.5 | 986.4 | 986.5 |
| glibc-trim | 728.1 | 798.5 | 973.3 | 735.1 | 971.4 | 696.4 | 635.2 | 844.7 | 811.7 | 867.1 | 765.2 | 859.0 | 750.1 | 873.9 | 701.9 | 775.0 | 646.6 | 679.7 | 978.1 | 632.5 |

### Threads (/proc/<pid>/status) at the end of each window

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54 | 49 | 52 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 52 | 51 |
| challoc-hd2000 | 55 | 49 | 52 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 51 | 51 | 52 | 51 | 52 | 51 |
| challoc-hd500 | 54 | 49 | 52 | 51 | 52 | 51 | 57 | 51 | 52 | 51 | 54 | 51 | 59 | 51 | 51 | 51 | 52 | 51 | 51 | 51 |
| challoc-hd0 | 61 | 51 | 53 | 51 | 54 | 51 | 52 | 51 | 54 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 51 | 51 | 52 | 51 |
| glibc-arena2 | 51 | 49 | 53 | 51 | 52 | 51 | 51 | 51 | 52 | 51 | 52 | 51 | 52 | 51 | 51 | 51 | 52 | 51 | 51 | 51 |
| glibc-trim | 53 | 50 | 56 | 52 | 55 | 52 | 53 | 52 | 53 | 52 | 53 | 52 | 53 | 52 | 53 | 52 | 53 | 52 | 53 | 52 |

### req/s per window (json / blob)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 92565 / 35647 | 75951 / 27036 | 92743 / 35823 | 76278 / 27250 | 92459 / 35809 | 76658 / 27189 | 93463 / 35745 | 76685 / 27010 | 92617 / 35618 | 76665 / 27102 | 92780 / 35725 | 75918 / 27251 | 92566 / 35806 | 76348 / 27082 | 92907 / 35782 | 76385 / 27280 | 93382 / 35745 | 76662 / 27149 | 93171 / 35897 | 76311 / 27105 |
| challoc-hd2000 | 91150 / 35432 | 77812 / 25717 | 91549 / 35466 | 77737 / 25607 | 91661 / 35467 | 77565 / 25498 | 91597 / 35575 | 78141 / 25575 | 91861 / 35404 | 77163 / 25666 | 91697 / 35423 | 77365 / 25569 | 91748 / 35533 | 77774 / 25574 | 91215 / 35456 | 78044 / 25638 | 92025 / 35591 | 77656 / 25544 | 91784 / 35555 | 76984 / 25325 |
| challoc-hd500 | 88525 / 35145 | 78725 / 24018 | 89536 / 34942 | 78445 / 24038 | 89942 / 35102 | 78838 / 24121 | 90187 / 35113 | 78055 / 24034 | 89434 / 34876 | 78715 / 24042 | 89730 / 34990 | 77804 / 24105 | 89556 / 34966 | 77901 / 24195 | 89964 / 34984 | 78593 / 23978 | 89708 / 35114 | 78806 / 24059 | 90031 / 35076 | 78601 / 24172 |
| challoc-hd0 | 84246 / 35200 | 78721 / 22836 | 85722 / 35493 | 78974 / 22838 | 85472 / 35431 | 79087 / 22645 | 85430 / 35335 | 78678 / 22731 | 85801 / 35281 | 78711 / 22769 | 85862 / 35558 | 78275 / 22739 | 85680 / 35470 | 78979 / 22773 | 86223 / 35537 | 78431 / 22811 | 85661 / 35429 | 78998 / 22859 | 85692 / 35467 | 78492 / 22749 |
| glibc-arena2 | 96669 / 35327 | 76457 / 28307 | 97430 / 35418 | 76256 / 28667 | 96743 / 35461 | 76728 / 28472 | 97420 / 35650 | 76304 / 28471 | 96758 / 35713 | 76964 / 28435 | 97019 / 35600 | 77281 / 28679 | 97451 / 35545 | 76821 / 28593 | 96970 / 35651 | 77224 / 28481 | 97488 / 35688 | 77054 / 28715 | 97637 / 35860 | 77288 / 28630 |
| glibc-trim | 96519 / 35726 | 76659 / 28447 | 96984 / 35868 | 76630 / 28216 | 96760 / 36077 | 76554 / 27923 | 97290 / 36035 | 77583 / 28362 | 97407 / 36115 | 77460 / 28219 | 97027 / 36087 | 75936 / 28195 | 97701 / 36089 | 77371 / 28192 | 97824 / 35787 | 77446 / 28191 | 97085 / 36068 | 77406 / 28450 | 97361 / 36252 | 77223 / 28277 |

### p99 ms per window (json / blob)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 4 / 46 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 46 | 0 / 1 |
| challoc-hd2000 | 4 / 43 | 0 / 1 | 4 / 44 | 0 / 1 | 4 / 43 | 0 / 1 | 4 / 44 | 0 / 1 | 4 / 44 | 0 / 1 | 4 / 43 | 0 / 1 | 4 / 44 | 0 / 1 | 4 / 44 | 0 / 1 | 4 / 44 | 0 / 1 | 4 / 44 | 0 / 1 |
| challoc-hd500 | 4 / 40 | 0 / 1 | 4 / 41 | 0 / 1 | 4 / 40 | 0 / 1 | 4 / 39 | 0 / 1 | 4 / 40 | 0 / 1 | 4 / 40 | 0 / 1 | 4 / 40 | 0 / 1 | 4 / 40 | 0 / 1 | 4 / 39 | 0 / 1 | 4 / 40 | 0 / 1 |
| challoc-hd0 | 4 / 38 | 0 / 1 | 4 / 38 | 0 / 1 | 4 / 38 | 0 / 1 | 4 / 39 | 0 / 1 | 4 / 38 | 0 / 1 | 4 / 38 | 0 / 1 | 4 / 38 | 0 / 1 | 4 / 38 | 0 / 1 | 4 / 39 | 0 / 1 | 4 / 37 | 0 / 1 |
| glibc-arena2 | 3 / 45 | 0 / 1 | 3 / 45 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 46 | 0 / 1 |
| glibc-trim | 3 / 46 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 47 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 45 | 0 / 1 | 3 / 46 | 0 / 1 | 3 / 46 | 0 / 1 |

### Native Memory Tracking at the end (MiB, the last sample; "allocator overhead" = Rss - NMT committed at the same instant)

| allocator | Rss MiB at the sample | NMT committed MiB | allocator overhead MiB (Rss - NMT) | malloc'd MiB (all categories) | mmap committed MiB | Java Heap | Class | Thread | Code | GC | Compiler | Internal | Other | Symbol | NMT's own overhead MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 1032.1 | 706.5 | 325.6 | 124.8 | 581.7 | 512.0 | 0.6 | 2.4 | 9.7 | 71.2 | 0.2 | 1.4 | 91.3 | 1.3 | 0.4 |
| challoc-hd2000 | 1012.4 | 745.8 | 266.6 | 164.0 | 581.8 | 512.0 | 0.6 | 2.4 | 9.8 | 71.2 | 0.2 | 1.4 | 130.5 | 1.3 | 0.4 |
| challoc-hd500 | 942.2 | 646.8 | 295.4 | 64.8 | 582.0 | 512.0 | 0.6 | 2.5 | 9.9 | 71.2 | 0.2 | 1.5 | 31.3 | 1.3 | 0.4 |
| challoc-hd0 | 870.4 | 651.8 | 218.6 | 69.6 | 582.1 | 512.0 | 0.5 | 2.4 | 10.2 | 71.2 | 0.2 | 1.4 | 36.1 | 1.3 | 0.4 |
| glibc-arena2 | 986.5 | 773.4 | 213.1 | 191.7 | 581.7 | 512.0 | 0.6 | 2.3 | 9.8 | 71.0 | 0.2 | 1.4 | 158.2 | 1.3 | 0.4 |
| glibc-trim | 632.5 | 617.4 | 15.1 | 35.0 | 582.4 | 512.0 | 0.6 | 2.3 | 10.4 | 71.0 | 0.2 | 1.4 | 1.8 | 1.3 | 0.4 |

NMT (`-XX:NativeMemoryTracking=summary`, `jcmd <pid> VM.native_memory summary scale=KB`) is the JVM's own account of the native memory it asked for: "committed" counts the bytes the JVM malloc'd (with NMT's 16-byte header each) plus the pages it committed by mmap (Java heap, metaspace, code cache, thread stacks and GC structures). Rss minus NMT committed at the same instant is what the allocator adds on its own account (caching, size-class rounding, fragmentation, retained pages, metadata) less what the JVM committed but never touched (NMT counts mmap commits, not resident pages, so the gap understates the allocator by the same untouched amount under every variant). "malloc'd" is NMT's malloc total across categories: `Other` holds the direct ByteBuffers (`Unsafe.allocateMemory`), `Internal` the JVM's miscellany, `Thread` the stacks (touched pages) and per-thread arenas, `Class` metaspace, `Code` the code cache, `GC` the card table and remembered sets, `Compiler` the JIT arenas. NMT's own overhead (its header per malloc plus its tables, the last column) is the same bytes for every allocator, so the comparisons between allocators hold; the header does push a malloc(65536) to 65552 bytes, across a power-of-two class boundary in every size-class allocator (see the reading). jcmd is a second JVM started from the harness shell (no LD_PRELOAD) and wakes the server's attach listener (one more thread, the same for every variant).

### Final Rss by mapping (MiB, /proc/<pid>/smaps at the last sample; "anon" is unnamed anonymous memory: the Java heap, metaspace, code cache, thread stacks and every allocator's unnamed arenas; challoc names its regions)

| allocator | [heap] | anon | file-backed | named anonymous regions |
|---|---:|---:|---:|---|
| challoc | 0.0 | 537.0 | 37.6 | challoc:classes=229.3 challoc:huge=200.5 challoc:meta=27.8 |
| challoc-hd2000 | 0.0 | 537.0 | 37.7 | challoc:classes=226.3 challoc:huge=183.4 challoc:meta=28.0 |
| challoc-hd500 | 0.0 | 537.2 | 37.5 | challoc:classes=229.4 challoc:huge=108.9 challoc:meta=29.2 |
| challoc-hd0 | 0.0 | 537.4 | 37.6 | challoc:classes=226.2 challoc:huge=41.9 challoc:meta=27.3 |
| glibc-arena2 | 199.2 | 750.4 | 36.9 | none (kernel without anonymous VMA names, or no allocator names its mappings) |
| glibc-trim | 0.6 | 595.0 | 36.8 | none (kernel without anonymous VMA names, or no allocator names its mappings) |

Series: 2026-09-20-x86_64-jvm-huge-decay-challoc.csv 2026-09-20-x86_64-jvm-huge-decay-challoc-hd2000.csv 2026-09-20-x86_64-jvm-huge-decay-challoc-hd500.csv 2026-09-20-x86_64-jvm-huge-decay-challoc-hd0.csv 2026-09-20-x86_64-jvm-huge-decay-glibc-arena2.csv 2026-09-20-x86_64-jvm-huge-decay-glibc-trim.csv (elapsed_s,phase,rss_kb,pss_kb,anon_kb,vmrss_kb,vmhwm_kb,threads, one line per 5 s; phase is idle, w<i>:high|low or final) and 2026-09-20-x86_64-jvm-huge-decay-challoc-windows.csv 2026-09-20-x86_64-jvm-huge-decay-challoc-hd2000-windows.csv 2026-09-20-x86_64-jvm-huge-decay-challoc-hd500-windows.csv 2026-09-20-x86_64-jvm-huge-decay-challoc-hd0-windows.csv 2026-09-20-x86_64-jvm-huge-decay-glibc-arena2-windows.csv 2026-09-20-x86_64-jvm-huge-decay-glibc-trim-windows.csv (one line per window: ab's req/s and p99 per endpoint, the server's CPU per request, Rss, threads and the pool's size at the window's end). Raw: /src/target/jvm/soak-out/soak.<allocator>/{samples.csv,windows.csv,w<i>-json.ab,w<i>-blob.ab,gc.log,smaps-by-name.txt,nmt-peak.txt,nmt-final.txt,stats.txt,stderr}.

## Reading (lane huge-decay, 2026-09-21)

The question: does a decay period of the huge tier's own (`huge_dirty_decay_ms`, the key this run introduces; the library is 27f2f59, whose `src/` the VM had as an uncommitted tree over f419b88, hence the header's label) close the 300..400 MiB challoc holds above glibc-arena2 and glibc-trim at the end of every load lull of this soak (`2026-09-20-x86_64-jvm-soak.md`), 222 MiB of which were the huge tier's empty extents inside the 10 s window. Same VM shape, same harness, same day; the four challoc rows are one library under `CHALLOC_CONF`, the two glibc rows are the harness's. Rss at the low windows' ends and the stats print at exit are from `windows.csv` and the `stderr` of each run (`target/jvm/soak-out/soak.<variant>/`).

| variant | huge window | peak Rss MiB | final Rss MiB | tail mean Rss MiB | slope MiB/min | CPU us/req high / low | blob req/s low | Rss at the low windows' ends, mean (min..max) MiB | `challoc:huge` at the end MiB | `Huge:` at exit | eager purges (bytes) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| challoc | follows `dirty_decay_ms` (10 s; today's default) | 1226.6 | 1032.1 | 1121.1 | -2.55 | 68.8 / 58.1 | 27145 | 1020 (1003..1032) | 200.5 | 0 empty extents, 6 purged extents (44804 pages) | 221839 (351.1 GB) |
| challoc-hd2000 | 2000 ms | 1242.9 | 1012.4 | 1105.4 | -4.72 | 71.5 / 58.8 | 25571 | 999 (961..1023) | 183.4 | 1 empty extent (18577 pages), 8 purged (20162 pages) | 263512 (1.10 TB) |
| challoc-hd500 | 500 ms | 1254.0 | 942.2 | 1092.8 | -3.50 | 75.4 / 59.4 | 24076 | 977 (909..1028) | 108.9 | 3 empty extents (70780 pages), 6 purged (146 pages) | 309039 (2.76 TB) |
| challoc-hd0 | 0 (purge at the free) | 1226.0 | 870.4 | 1050.4 | -4.59 | 83.7 / 64.9 | 22775 | 907 (833..969) | 41.9 | 0 empty extents, 40 purged extents (75664 pages) | 5974268 (3.32 TB) |
| glibc-arena2 | - | 989.0 | 986.5 | 985.8 | +0.33 | 64.2 / 56.3 | 28545 | - | - | - | - |
| glibc-trim | `malloc_trim(0)` every 5 s | 1018.2 | 632.5 | 864.7 | -13.80 | 63.9 / 56.7 | 28247 | - | - | - | - |
| challoc-trim | `malloc_trim(0)` every 5 s (the JVM's TrimNativeHeapInterval on challoc) | 1188.2 | 813.9 | 1080.7 | -6.29 | 67.9 / 57.6 | 27855 | 968 (809..1013) | 46.5 | - | - |

What the numbers say:

1. The window barely matters until it is shorter than the JVM's GC cycle.
   At 2000 ms the tail mean moves -1.4% and `challoc:huge` at the end
   is the same ~183 MiB. The extents at the end of a lull are not
   idle memory waiting out a 10 s window: the blob generator at 8
   connections still moves about 4 GiB/s of direct buffers, the JVM's
   direct-memory limit (the 512 MiB heap size) forces a GC about every
   120 ms, the Cleaner frees one cycle's worth of buffers (~200 MiB of runs
   above 256 KiB) in a burst, and the next cycle takes the same sizes back
   out of the `Empty` list before an age-based curve has a step to purge
   (the step of a 10 s window is 50 ms, of a 2 s window 10 ms; the
   smootherstep weight at 120 ms of age is 0.9999 and 0.98). The empty
   extents the stats print sees at exit (one empty extent of 73 MiB in the 2000 ms run, none in the default run (its last burst had just been reused)) are the last burst
   with nothing left to reuse them, which is what the previous file's
   "222 MiB inside the 10 s window" was: a snapshot of one burst, not a
   backlog.
2. Below the cycle the curve turns into refaults. At 500 ms about a tenth
   of every burst is past its first steps before it is reused (weight 0.9
   at 120 ms of 500), -2.5% tail mean for +10% CPU per
   request in the high windows and -11% blob req/s in the low
   ones. At 0 every freed run is `MADV_DONTNEED`ed at the free and faulted
   back in by the next allocation: 6.0 M eager purges, 3.3 TB
   over 1200 s (2.8 GB/s of purge and refault), `challoc:huge` 42 MiB
   at the end, Rss at the low windows' ends -113 MiB, the tail mean
   -6.3% (closing 52% of the gap to glibc-arena2's tail mean and
   28% of glibc-trim's), for +22% CPU per request in the high
   windows, +12% in the low, -16% blob req/s in the low windows
   and a blob p99 in the high windows +21%. The RSS bought is linear
   in the CPU spent; no value on this axis is free.
3. Why glibc-trim gets to a lower tail mean with +7.2%..+7.7% less CPU per request than challoc:
   `malloc_trim(0)` every 5 s purges whatever is free at that instant
   regardless of age, then nothing for 5 s, so each burst's pages are
   purged at most once per 5 s (tens of MB/s of refaults) instead of once
   per cycle, and its Rss at the window ends spans 632..1018 MiB in
   the two files because the sample lands at a random phase of those 5 s.
   An age-based window cannot reproduce that on a churn whose cycle is
   shorter than any window worth having; a purge on a timer regardless of
   age can. `challoc-trim` in the companion file
   `2026-09-20-x86_64-jvm-huge-decay-trim.md` is that experiment on challoc
   itself: the same library under `-XX:TrimNativeHeapInterval=5000`, so the
   JVM's service thread calls challoc's `malloc_trim(0)` (`purge_all`:
   every `Empty` slab and huge extent, eagerly) every 5 s; its row is read
   below.
4. The other half of the gap is untouched by any of this: `challoc:classes`
   stays at 226..255 MiB in every challoc row (the half-occupied slabs the
   Cleaner's bursts leave behind, 134 MiB in the previous file's reading),
   and the JVM's own NMT committed moves 617..746 MiB between runs at the
   final sample (its metaspace, code and GC structures are not the
   allocator's), so single final samples differ by more than the
   allocator's share; the tail mean over the last 600 s is the figure the
   table is read by.

5. The `challoc-trim` row (companion file, run after the six above on the
   same VM; its Rss at the low windows' ends is from its `windows.csv`):
   the same library, no key, the JVM calling challoc's `malloc_trim(0)`
   every 5 s. Tail mean -3.6% against the default and peak -3.1%,
   at CPU per request 67.9 / 57.6 against 68.8 / 58.1 (-1% / -1%) and blob req/s in the low windows +2.6%:
   it costs nothing (a purge of what is free at that instant, once per
   5 s) and buys more than `huge_dirty_decay_ms:500` at +9..10% CPU, though
   less than 0. Its named regions at the end: `challoc:classes` 165.5 +
   `challoc:huge` 46.5 + meta = 239 MiB against the default's
   458: the age-blind purge also drains the class tier's fully
   empty slabs (~90 MiB waiting in the 10 s window), which no huge key
   touches. Its window ends span 809..1150 MiB, the same sawtooth as
   glibc-trim's, and its tail mean is still above glibc-arena2's
   986 and glibc-trim's 865: what is left is the
   half-occupied slabs, which no purge can give back, and the thread
   caches. This is the mechanism to build if the JVM gap is to close:
   a purge on a timer (or at a lull's start) regardless of age, not a
   shorter window.

## Valkey guard: the soak under the strongest candidate

`make valkey-soak ALLOCS=challoc DURATION=1200` twice on the same VM after the JVM rows (`make valkey STAGE=build`, Valkey 9.1.2 with the contrib patch, `quantum8`; the acceptance protocol's phases: 600 s write, 600 s delete, 60 s settle, activedefrag on, no cgroup peak reset), the default configuration first and `CHALLOC_CONF=huge_dirty_decay_ms:0` second: the value that moved the JVM most, so if it leaves Valkey alone every milder value does. Summaries under `target/valkey/soak-hd-default/` and `soak-hd-0/` on the VM (`2026-09-21-x86_64-valkey-soak.md`, not fetched: `RESULT_DIR` outside `bench/results/` because the two challoc-only runs are a guard, not an acceptance file). Positive = the candidate higher.

| measure | challoc, default | challoc, `huge_dirty_decay_ms:0` | candidate vs default |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 762153 | 760023 | -0.3% |
| write: valkey-benchmark p99 ms | 1.239 | 1.247 | +0.6% |
| write: memtier req/s | 939552 | 935670 | -0.4% |
| write: memtier p99 ms | 0.975 | 0.975 | 0 |
| delete: valkey-benchmark req/s | 1345557 | 1336882 | -0.6% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.735 | 0 |
| delete: memtier req/s | 1227176 | 1218487 | -0.7% |
| delete: memtier p99 ms | 0.783 | 0.791 | +1.0% |
| peak RSS MiB (VmHWM) | 577.8 | 577.8 | 0 |
| RSS MiB at end of write phase | 577.5 | 577.6 | 0 |
| RSS MiB at end of delete phase | 129.9 | 130.2 | +0.2% |
| RSS MiB after settle | 129.9 | 130.2 | +0.2% |
| allocator_frag_ratio after settle | 1.79 | 1.79 | - |
| keys after settle | 105174 | 105134 | - |

Unchanged: every throughput row within 0.7%, the write-phase p99 +0.008 / 0 ms, VmHWM identical, after settle +0.3 MiB; all inside the spread two same-day soaks of one server showed on 2026-09-17 (`2026-09-17-aarch64-gce-slab-geometry-soak{1,2}-valkey-soak.md`, the stock column 0.8..1.0% apart on the write rows). The key can only act where a huge run is freed, and this server frees one at a dict rehash (a handful of runs of a few MiB per phase), so there is nothing for it to move.

## Quick tier (`make quick OLD=main`, one round each, on the VM)

| run | new vs old per row (positive = new worse) | verdict |
|---|---|---|
| the code change alone (default conf) | larson1 -1.1%, larson10 -1.6%, xmalloc +0.3%, alloc-test1 -0.5%, valkey1 +1.1%, churn compacts | go |
| `NEW_CONF=huge_dirty_decay_ms:0` | larson1 -5.6%, larson10 +5.0%, xmalloc +1.3%, alloc-test1 -0.9%, valkey1 +1.1%, churn compacts | go (larson10 at the 5% gate) |
| `NEW_CONF=huge_dirty_decay_ms:500` | larson1 -1.8%, larson10 +0.6%, xmalloc +0.2%, alloc-test1 -1.0%, valkey1 +0.8%, churn compacts | go |

Three rounds of the same two libraries (old `9b9c9d89cb8039f3` = main, new `856dfded39db7edb` = 27f2f59's `src/`): larson10 -1.6%, +5.0%, +0.6% and larson1 -1.1%, -5.6%, -1.8%, the x86_64 larson rows' known one-round spread moving both ways in the same round; valkey1 +1.1%, +1.1%, +0.8%.

## Verdict

No value of `huge_dirty_decay_ms` closes most of the JVM gap without paying for it in CPU, and the shipped default stays as it is (the key follows `dirty_decay_ms` unless set): 2000 is inside the noise; 500 buys 1.4..2.5% of tail-mean Rss for 9..10% CPU per request; 0 buys 6..8% (about half the gap to glibc-arena2, a quarter of glibc-trim's) for 22..26% CPU per request and 16% of the lulls' blob req/s. The huge extents at a lull's end are one GC burst in flight, not a backlog, and any age-based window shorter than the ~120 ms GC cycle purges and refaults every burst. The mechanism that does close the gap is a purge of whatever is free at the moment regardless of age, on a timer: `challoc-trim` (companion file) took 3.6..5.8% off the tail mean and 3..8% off the peak for no CPU, and drained the class tier's empty slabs besides the huge tier's. Valkey is unchanged under the strongest value on x86_64 and within one run's spread on aarch64, with nothing for the key to act on there; the quick tier is `go` on both architectures with and without the key.
