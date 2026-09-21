# JVM benchmark rows: the mixed-size soak, 2026-09-20, aarch64

- commit: f419b88 (uncommitted changes in src/)
- date: 2026-09-20T23:08:34Z
- host: Neoverse-V2, 16 CPUs visible to the container; GCE c4a-standard-16 (Google Axion, Neoverse V2), zone us-east4-a
- kernel: 7.0.0-1011-gcp, container: Wolfi
- java: /usr/lib/jvm/java-21-openjdk/bin/java (Wolfi openjdk-21-21.0.12.1-r2); `java -version`:
      openjdk version "21.0.12.1" 2026-08-18
      OpenJDK Runtime Environment openjdk-21-21.0.12.1-r2 (build 21.0.12.1+-wolfi-r2)
      OpenJDK 64-Bit Server VM openjdk-21-21.0.12.1-r2 (build 21.0.12.1+-wolfi-r2, mixed mode, sharing)
- generator: This is ApacheBench, Version 2.3 <$Revision$> (Wolfi apache2-utils 2.4.68); wrk and oha are not in Wolfi
- load average at start: 0.77 0.32 0.13; at end: 6.60 7.27 7.56; 7476 s total
- method: bench/jvm/HttpBench.java in `soak` mode (com.sun.net.httpserver on a ThreadPoolExecutor of 16 core and 400 maximum threads, idle threads retired after 2 s, a SynchronousQueue; `-Xms512m -Xmx512m -XX:+AlwaysPreTouch -Dsun.net.httpserver.nodelay=true -XX:NativeMemoryTracking=summary`, so the Java heap is constant and resident from the start and RSS differs by native memory only), served once per allocator for 1200 s to two `ab -k` processes (one on /json, a ~300-byte body built per request; one on /blob, a body of a log-uniform size between 256 and 1048576 bytes drawn per request and copied out of a per-request ByteBuffer.allocateDirect of that size, i.e. one malloc(n) per request in the JVM with n spread over every size class from 256 bytes to 1 MiB, freed when the buffer's Cleaner runs after a GC) in 20 windows of 60 s alternating 256 total connections (ab -c 128 x2) and 8 (ab -c 4 x2), so the pool grows to hundreds of threads and shrinks to 16 every other minute all run long; RSS sampled every 5 s from /proc/<pid>/smaps_rollup (Rss) and /proc/<pid>/status (VmHWM, Threads) into a CSV per allocator (beside this file): idle = the last sample before the load (10 s after the server answered), peak = the largest sample, final = the last sample after the load, slope = the least-squares slope of Rss over the samples of the last 600 s in MiB per minute (growth after warm-up is the fragmentation signal), tail mean = the mean Rss over the same samples; req/s and p99 (ms) are ab's per endpoint per window, summarised over the high and the low windows; CPU us/req is the server's user+sys over a window divided by the requests both generators completed in it; NMT (`jcmd VM.native_memory summary`) at the end. Percentages are challoc against the other allocator: positive means challoc is larger, slower or serves fewer requests.
- allocators:
  - challoc: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 7ed09e4e6424620f)
  - challoc-hd2000: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 7ed09e4e6424620f) with CHALLOC_CONF=huge_dirty_decay_ms:2000 (the same library as challoc under the key)
  - challoc-hd500: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 7ed09e4e6424620f) with CHALLOC_CONF=huge_dirty_decay_ms:500 (the same library as challoc under the key)
  - challoc-hd0: LD_PRELOAD=/src/target/jvm/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 7ed09e4e6424620f) with CHALLOC_CONF=huge_dirty_decay_ms:0 (the same library as challoc under the key)
  - glibc-arena2: no LD_PRELOAD, MALLOC_ARENA_MAX=2 (glibc 2.44 malloc; the fleet's usual mitigation for the per-thread arenas)
  - glibc-trim: no LD_PRELOAD, `-XX:+UnlockExperimentalVMOptions -XX:TrimNativeHeapInterval=5000` (glibc 2.44 malloc; the JVM's service thread calls malloc_trim(0) every 5000 ms: what a well-tuned Java image ships today)

## Mixed-size soak, 1200 s per allocator (pool 16..400 threads, keep-alive 2 s; /blob log-uniform 256..1048576 bytes; 60 s windows alternating 256 and 8 connections)

| allocator | idle Rss MiB | peak Rss MiB | final Rss MiB | tail mean Rss MiB (last 600 s) | slope MiB/min (last 600 s) | VmHWM MiB | NMT committed MiB (end) | allocator overhead MiB (end) | json req/s high | json p99 ms high (max) | blob req/s high | blob p99 ms high (max) | blob MiB/s high | json req/s low | blob req/s low | CPU us/req high | CPU us/req low | max threads | largest pool | GCs | exit |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 582.0 | 1300.8 | 1090.8 | 1185.1 | -4.18 | 1316.1 | 707.4 | 383.4 | 75709 | 4 | 44378 | 83 | 5278 | 67175 | 35281 | 53.2 | 54.9 | 130 | 95 | 21495 | 0 |
| challoc-hd2000 | 582.1 | 1304.2 | 1082.8 | 1180.5 | -4.24 | 1311.8 | 741.2 | 341.5 | 75914 | 4 | 43600 | 80 | 5185 | 68382 | 33760 | 54.9 | 55.6 | 137 | 102 | 21586 | 0 |
| challoc-hd500 | 581.9 | 1309.3 | 1022.7 | 1168.8 | -4.51 | 1319.0 | 638.0 | 384.7 | 75377 | 4 | 43109 | 75 | 5128 | 67581 | 32270 | 58.1 | 57.6 | 147 | 112 | 21127 | 0 |
| challoc-hd0 | 582.2 | 1260.4 | 843.4 | 1088.1 | -7.36 | 1290.9 | 616.9 | 226.5 | 75438 | 4 | 43872 | 63 | 5230 | 68870 | 29687 | 67.2 | 64.4 | 129 | 94 | 20868 | 0 |
| glibc-arena2 | 578.0 | 1033.8 | 1029.8 | 980.6 | +1.81 | 1055.5 | 682.0 | 347.9 | 78112 | 4 | 44754 | 86 | 5318 | 66642 | 36873 | 49.4 | 53.3 | 74 | 65 | 22106 | 0 |
| glibc-trim | 600.8 | 1186.3 | 809.7 | 968.9 | -7.13 | 1261.0 | 809.3 | 0.3 | 76366 | 4 | 44742 | 86 | 5319 | 65502 | 36690 | 50.8 | 54.4 | 80 | 87 | 21653 | 0 |

### challoc against each allocator (soak; positive = challoc worse; the slope row is the difference in MiB/min, challoc minus the other)

| metric | vs challoc-hd2000 | vs challoc-hd500 | vs challoc-hd0 | vs glibc-arena2 | vs glibc-trim |
|---|---:|---:|---:|---:|---:|
| peak Rss | -0.3% | -0.6% | +3.2% | +25.8% | +9.7% |
| final Rss | +0.7% | +6.7% | +29.3% | +5.9% | +34.7% |
| tail mean Rss | +0.4% | +1.4% | +8.9% | +20.9% | +22.3% |
| VmHWM | +0.3% | -0.2% | +1.9% | +24.7% | +4.4% |
| allocator overhead (end) | +12.3% | -0.4% | +69.3% | +10.2% | +112070.6% |
| json req/s high | +0.3% | -0.4% | -0.4% | +3.2% | +0.9% |
| json p99 high (max) | +0.0% | +0.0% | +0.0% | +0.0% | +0.0% |
| blob req/s high | -1.8% | -2.9% | -1.1% | +0.8% | +0.8% |
| blob p99 high (max) | +3.8% | +10.7% | +31.7% | -3.5% | -3.5% |
| json req/s low | +1.8% | +0.6% | +2.5% | -0.8% | -2.5% |
| blob req/s low | -4.3% | -8.5% | -15.9% | +4.5% | +4.0% |
| CPU us/req high | -3.1% | -8.4% | -20.8% | +7.7% | +4.7% |
| CPU us/req low | -1.3% | -4.7% | -14.8% | +3.0% | +0.9% |
| slope MiB/min (difference) | +0.06 | +0.33 | +3.18 | -5.99 | +2.95 |

### Rss at the end of each window (MiB; h = 256 connections, l = 8)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 1266.6 | 1075.7 | 1281.8 | 1070.2 | 1258.2 | 1087.5 | 1289.2 | 1094.1 | 1279.5 | 1088.9 | 1274.5 | 1087.9 | 1280.1 | 1087.3 | 1290.3 | 1093.5 | 1260.8 | 1083.3 | 1284.2 | 1090.8 |
| challoc-hd2000 | 1246.2 | 1046.3 | 1274.1 | 1002.9 | 1283.2 | 967.8 | 1274.2 | 1069.1 | 1300.9 | 1083.4 | 1290.4 | 1069.8 | 1280.2 | 1060.4 | 1255.7 | 1072.4 | 1289.9 | 1054.7 | 1296.1 | 1082.8 |
| challoc-hd500 | 1201.5 | 853.5 | 1114.0 | 1058.8 | 1296.7 | 1050.8 | 1261.3 | 981.2 | 1291.2 | 999.2 | 1298.9 | 1071.9 | 1197.8 | 1067.2 | 1268.9 | 1068.0 | 1276.1 | 1051.2 | 1198.7 | 1022.7 |
| challoc-hd0 | 1039.3 | 985.1 | 1173.0 | 841.4 | 1088.8 | 964.8 | 1217.3 | 850.6 | 1232.9 | 851.0 | 1170.1 | 934.4 | 1111.0 | 1025.0 | 1192.6 | 982.5 | 1115.9 | 891.1 | 1204.7 | 843.4 |
| glibc-arena2 | 976.3 | 975.7 | 976.8 | 975.9 | 976.1 | 976.0 | 976.0 | 976.0 | 976.0 | 976.1 | 976.2 | 976.0 | 976.1 | 976.0 | 976.0 | 976.0 | 976.1 | 975.9 | 975.9 | 1029.8 |
| glibc-trim | 792.2 | 958.1 | 1079.1 | 927.6 | 1126.5 | 831.0 | 1013.8 | 903.5 | 1140.7 | 942.9 | 717.2 | 923.6 | 1121.1 | 965.3 | 726.2 | 868.4 | 1112.4 | 942.8 | 1166.0 | 809.7 |

### Threads (/proc/<pid>/status) at the end of each window

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 100 | 49 | 86 | 51 | 108 | 51 | 75 | 51 | 108 | 51 | 74 | 51 | 115 | 51 | 81 | 51 | 95 | 51 | 79 | 51 |
| challoc-hd2000 | 93 | 49 | 132 | 51 | 132 | 51 | 100 | 51 | 121 | 51 | 112 | 51 | 70 | 51 | 81 | 51 | 81 | 51 | 110 | 51 |
| challoc-hd500 | 140 | 49 | 140 | 51 | 123 | 51 | 115 | 51 | 137 | 51 | 130 | 51 | 126 | 51 | 138 | 51 | 131 | 51 | 108 | 51 |
| challoc-hd0 | 112 | 51 | 103 | 51 | 98 | 51 | 99 | 51 | 108 | 51 | 106 | 51 | 129 | 51 | 109 | 51 | 108 | 51 | 111 | 51 |
| glibc-arena2 | 71 | 49 | 64 | 51 | 71 | 51 | 68 | 51 | 63 | 51 | 71 | 51 | 67 | 51 | 69 | 51 | 72 | 51 | 64 | 51 |
| glibc-trim | 66 | 50 | 66 | 52 | 69 | 52 | 75 | 52 | 68 | 52 | 70 | 52 | 66 | 52 | 66 | 52 | 66 | 52 | 68 | 52 |

### req/s per window (json / blob)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 75836 / 44157 | 66754 / 35363 | 76055 / 44419 | 67390 / 35291 | 76126 / 44343 | 66924 / 35186 | 75749 / 44432 | 67146 / 35261 | 75614 / 44217 | 66597 / 35188 | 75580 / 44515 | 66678 / 35225 | 75008 / 44401 | 67394 / 35476 | 75050 / 44498 | 67658 / 35146 | 75906 / 44308 | 67381 / 35441 | 76165 / 44493 | 67825 / 35234 |
| challoc-hd2000 | 76578 / 43262 | 68267 / 33491 | 76604 / 43373 | 68559 / 33299 | 76264 / 43509 | 68824 / 33108 | 75446 / 43658 | 68306 / 34160 | 76091 / 43644 | 67722 / 33813 | 75468 / 43458 | 68578 / 33991 | 75518 / 43684 | 68370 / 34072 | 76175 / 44000 | 68472 / 33823 | 75342 / 43865 | 68625 / 33940 | 75653 / 43551 | 68099 / 33900 |
| challoc-hd500 | 76082 / 42872 | 68367 / 31593 | 76047 / 42928 | 67461 / 32408 | 75889 / 43006 | 67605 / 32306 | 75414 / 43345 | 67712 / 32294 | 75542 / 43231 | 68244 / 32342 | 74882 / 42984 | 67825 / 32471 | 74954 / 43012 | 68056 / 32421 | 75539 / 43129 | 65537 / 32090 | 74511 / 43419 | 67216 / 32264 | 74909 / 43166 | 67783 / 32510 |
| challoc-hd0 | 77560 / 43221 | 69151 / 29824 | 75218 / 43920 | 69200 / 29763 | 75630 / 43648 | 69304 / 29639 | 74996 / 44399 | 69463 / 29755 | 75535 / 44293 | 68831 / 29706 | 74506 / 43896 | 69854 / 29892 | 75511 / 43595 | 69324 / 29867 | 75231 / 44055 | 67608 / 29449 | 75423 / 43651 | 67946 / 29378 | 74767 / 44045 | 68014 / 29601 |
| glibc-arena2 | 75854 / 44234 | 68227 / 37047 | 79080 / 44859 | 67135 / 36911 | 77990 / 44783 | 67385 / 37131 | 78684 / 44908 | 67501 / 37054 | 78458 / 44760 | 66447 / 36755 | 78517 / 44544 | 65838 / 36691 | 78422 / 44955 | 65948 / 36953 | 77663 / 44815 | 66652 / 36749 | 78460 / 44871 | 65524 / 36716 | 77989 / 44812 | 65760 / 36721 |
| glibc-trim | 75554 / 44271 | 65605 / 36984 | 76709 / 44864 | 66156 / 36922 | 76401 / 44960 | 66118 / 36734 | 76351 / 44658 | 65325 / 36777 | 76563 / 44651 | 66033 / 36824 | 76360 / 45077 | 65450 / 36482 | 75971 / 44670 | 64257 / 36347 | 76442 / 44775 | 64796 / 36585 | 76961 / 44761 | 65952 / 36754 | 76348 / 44731 | 65328 / 36494 |

### p99 ms per window (json / blob)

| allocator | w0 h | w1 l | w2 h | w3 l | w4 h | w5 l | w6 h | w7 l | w8 h | w9 l | w10 h | w11 l | w12 h | w13 l | w14 h | w15 l | w16 h | w17 l | w18 h | w19 l |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 82 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 82 | 0 / 0 |
| challoc-hd2000 | 4 / 78 | 0 / 1 | 4 / 78 | 0 / 1 | 4 / 77 | 0 / 1 | 4 / 80 | 0 / 0 | 4 / 79 | 0 / 0 | 4 / 79 | 0 / 0 | 4 / 80 | 0 / 0 | 4 / 79 | 0 / 0 | 4 / 79 | 0 / 0 | 4 / 79 | 0 / 0 |
| challoc-hd500 | 4 / 72 | 0 / 1 | 4 / 73 | 0 / 1 | 4 / 74 | 0 / 1 | 4 / 74 | 0 / 1 | 4 / 75 | 0 / 1 | 4 / 75 | 0 / 1 | 4 / 75 | 0 / 1 | 4 / 74 | 0 / 1 | 4 / 74 | 0 / 1 | 4 / 75 | 0 / 1 |
| challoc-hd0 | 4 / 63 | 0 / 1 | 4 / 11 | 0 / 1 | 4 / 13 | 0 / 1 | 4 / 10 | 0 / 1 | 4 / 10 | 0 / 1 | 4 / 11 | 0 / 1 | 4 / 12 | 0 / 1 | 4 / 11 | 0 / 1 | 4 / 12 | 0 / 1 | 4 / 10 | 0 / 1 |
| glibc-arena2 | 4 / 86 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 84 | 0 / 0 | 4 / 84 | 0 / 0 | 4 / 84 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 84 | 0 / 0 | 4 / 83 | 0 / 0 | 4 / 83 | 0 / 0 |
| glibc-trim | 4 / 86 | 0 / 0 | 4 / 84 | 0 / 0 | 4 / 84 | 0 / 0 | 4 / 85 | 0 / 0 | 4 / 84 | 0 / 0 | 4 / 85 | 0 / 0 | 4 / 85 | 0 / 0 | 4 / 85 | 0 / 0 | 4 / 84 | 0 / 0 | 4 / 85 | 0 / 0 |

### Native Memory Tracking at the end (MiB, the last sample; "allocator overhead" = Rss - NMT committed at the same instant)

| allocator | Rss MiB at the sample | NMT committed MiB | allocator overhead MiB (Rss - NMT) | malloc'd MiB (all categories) | mmap committed MiB | Java Heap | Class | Thread | Code | GC | Compiler | Internal | Other | Symbol | NMT's own overhead MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 1090.8 | 707.4 | 383.4 | 124.9 | 582.5 | 512.0 | 0.6 | 2.3 | 10.5 | 70.3 | 0.2 | 1.4 | 92.3 | 1.3 | 0.4 |
| challoc-hd2000 | 1082.8 | 741.2 | 341.5 | 159.4 | 581.9 | 512.0 | 0.6 | 2.3 | 9.8 | 70.5 | 0.2 | 1.4 | 126.6 | 1.3 | 0.4 |
| challoc-hd500 | 1022.7 | 638.0 | 384.7 | 56.2 | 581.8 | 512.0 | 0.6 | 2.3 | 9.7 | 70.4 | 0.2 | 1.4 | 23.6 | 1.3 | 0.4 |
| challoc-hd0 | 843.4 | 616.9 | 226.5 | 34.3 | 582.6 | 512.0 | 0.5 | 2.5 | 10.4 | 70.4 | 0.2 | 1.4 | 1.7 | 1.3 | 0.4 |
| glibc-arena2 | 1029.8 | 682.0 | 347.9 | 99.2 | 582.7 | 512.0 | 0.6 | 3.2 | 9.9 | 70.1 | 0.2 | 1.4 | 66.9 | 1.3 | 0.4 |
| glibc-trim | 809.7 | 809.3 | 0.3 | 226.8 | 582.5 | 512.0 | 0.6 | 3.0 | 9.8 | 70.1 | 0.2 | 1.4 | 194.5 | 1.3 | 0.4 |

NMT (`-XX:NativeMemoryTracking=summary`, `jcmd <pid> VM.native_memory summary scale=KB`) is the JVM's own account of the native memory it asked for: "committed" counts the bytes the JVM malloc'd (with NMT's 16-byte header each) plus the pages it committed by mmap (Java heap, metaspace, code cache, thread stacks and GC structures). Rss minus NMT committed at the same instant is what the allocator adds on its own account (caching, size-class rounding, fragmentation, retained pages, metadata) less what the JVM committed but never touched (NMT counts mmap commits, not resident pages, so the gap understates the allocator by the same untouched amount under every variant). "malloc'd" is NMT's malloc total across categories: `Other` holds the direct ByteBuffers (`Unsafe.allocateMemory`), `Internal` the JVM's miscellany, `Thread` the stacks (touched pages) and per-thread arenas, `Class` metaspace, `Code` the code cache, `GC` the card table and remembered sets, `Compiler` the JIT arenas. NMT's own overhead (its header per malloc plus its tables, the last column) is the same bytes for every allocator, so the comparisons between allocators hold; the header does push a malloc(65536) to 65552 bytes, across a power-of-two class boundary in every size-class allocator (see the reading). jcmd is a second JVM started from the harness shell (no LD_PRELOAD) and wakes the server's attach listener (one more thread, the same for every variant).

### Final Rss by mapping (MiB, /proc/<pid>/smaps at the last sample; "anon" is unnamed anonymous memory: the Java heap, metaspace, code cache, thread stacks and every allocator's unnamed arenas; challoc names its regions)

| allocator | [heap] | anon | file-backed | named anonymous regions |
|---|---:|---:|---:|---|
| challoc | 0.0 | 537.3 | 35.2 | challoc:classes=254.6 challoc:huge=238.3 challoc:meta=25.4 |
| challoc-hd2000 | 0.0 | 537.1 | 35.2 | challoc:classes=249.8 challoc:huge=234.6 challoc:meta=26.0 |
| challoc-hd500 | 0.0 | 537.0 | 35.2 | challoc:classes=253.0 challoc:huge=170.9 challoc:meta=26.6 |
| challoc-hd0 | 0.0 | 538.0 | 35.2 | challoc:classes=229.2 challoc:meta=26.0 challoc:huge=15.0 |
| glibc-arena2 | 245.6 | 749.3 | 34.9 | none (kernel without anonymous VMA names, or no allocator names its mappings) |
| glibc-trim | 2.0 | 772.7 | 34.9 | none (kernel without anonymous VMA names, or no allocator names its mappings) |

Series: 2026-09-20-aarch64-jvm-huge-decay-challoc.csv 2026-09-20-aarch64-jvm-huge-decay-challoc-hd2000.csv 2026-09-20-aarch64-jvm-huge-decay-challoc-hd500.csv 2026-09-20-aarch64-jvm-huge-decay-challoc-hd0.csv 2026-09-20-aarch64-jvm-huge-decay-glibc-arena2.csv 2026-09-20-aarch64-jvm-huge-decay-glibc-trim.csv (elapsed_s,phase,rss_kb,pss_kb,anon_kb,vmrss_kb,vmhwm_kb,threads, one line per 5 s; phase is idle, w<i>:high|low or final) and 2026-09-20-aarch64-jvm-huge-decay-challoc-windows.csv 2026-09-20-aarch64-jvm-huge-decay-challoc-hd2000-windows.csv 2026-09-20-aarch64-jvm-huge-decay-challoc-hd500-windows.csv 2026-09-20-aarch64-jvm-huge-decay-challoc-hd0-windows.csv 2026-09-20-aarch64-jvm-huge-decay-glibc-arena2-windows.csv 2026-09-20-aarch64-jvm-huge-decay-glibc-trim-windows.csv (one line per window: ab's req/s and p99 per endpoint, the server's CPU per request, Rss, threads and the pool's size at the window's end). Raw: /src/target/jvm/soak-out/soak.<allocator>/{samples.csv,windows.csv,w<i>-json.ab,w<i>-blob.ab,gc.log,smaps-by-name.txt,nmt-peak.txt,nmt-final.txt,stats.txt,stderr}.

## Reading (lane huge-decay, 2026-09-21)

The question: does a decay period of the huge tier's own (`huge_dirty_decay_ms`, the key this run introduces; the library is 27f2f59, whose `src/` the VM had as an uncommitted tree over f419b88, hence the header's label) close the 300..400 MiB challoc holds above glibc-arena2 and glibc-trim at the end of every load lull of this soak (`2026-09-20-aarch64-jvm-soak.md`), 222 MiB of which were the huge tier's empty extents inside the 10 s window. Same VM shape, same harness, same day; the four challoc rows are one library under `CHALLOC_CONF`, the two glibc rows are the harness's. Rss at the low windows' ends and the stats print at exit are from `windows.csv` and the `stderr` of each run (`target/jvm/soak-out/soak.<variant>/`).

| variant | huge window | peak Rss MiB | final Rss MiB | tail mean Rss MiB | slope MiB/min | CPU us/req high / low | blob req/s low | Rss at the low windows' ends, mean (min..max) MiB | `challoc:huge` at the end MiB | `Huge:` at exit | eager purges (bytes) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| challoc | follows `dirty_decay_ms` (10 s; today's default) | 1300.8 | 1090.8 | 1185.1 | -4.18 | 53.2 / 54.9 | 35281 | 1086 (1070..1094) | 238.3 | 4 empty extents (55551 pages), 0 purged | 221377 (341.8 GB) |
| challoc-hd2000 | 2000 ms | 1304.2 | 1082.8 | 1180.5 | -4.24 | 54.9 / 55.6 | 33760 | 1051 (968..1083) | 234.6 | 2 empty extents (43336 pages), 9 purged (253 pages) | 246929 (895.2 GB) |
| challoc-hd500 | 500 ms | 1309.3 | 1022.7 | 1168.8 | -4.51 | 58.1 / 57.6 | 32270 | 1022 (854..1072) | 170.9 | 2 empty extents (62736 pages), 2 purged (69 pages) | 276724 (1.78 TB) |
| challoc-hd0 | 0 (purge at the free) | 1260.4 | 843.4 | 1088.1 | -7.36 | 67.2 / 64.4 | 29687 | 917 (841..1025) | 15.0 | 0 empty extents, 2 purged extents (65985 pages) | 7486620 (4.20 TB) |
| glibc-arena2 | - | 1033.8 | 1029.8 | 980.6 | +1.81 | 49.4 / 53.3 | 36873 | - | - | - | - |
| glibc-trim | `malloc_trim(0)` every 5 s | 1186.3 | 809.7 | 968.9 | -7.13 | 50.8 / 54.4 | 36690 | - | - | - | - |
| challoc-trim | `malloc_trim(0)` every 5 s (the JVM's TrimNativeHeapInterval on challoc) | 1201.5 | 846.2 | 1116.1 | -6.30 | 51.2 / 53.6 | 36334 | 1043 (846..1063) | 81.4 | - | - |

What the numbers say:

1. The window barely matters until it is shorter than the JVM's GC cycle.
   At 2000 ms the tail mean moves -0.4% and `challoc:huge` at the end
   is the same ~235 MiB. The extents at the end of a lull are not
   idle memory waiting out a 10 s window: the blob generator at 8
   connections still moves about 4 GiB/s of direct buffers, the JVM's
   direct-memory limit (the 512 MiB heap size) forces a GC about every
   120 ms, the Cleaner frees one cycle's worth of buffers (~200 MiB of runs
   above 256 KiB) in a burst, and the next cycle takes the same sizes back
   out of the `Empty` list before an age-based curve has a step to purge
   (the step of a 10 s window is 50 ms, of a 2 s window 10 ms; the
   smootherstep weight at 120 ms of age is 0.9999 and 0.98). The empty
   extents the stats print sees at exit (4 empty extents of 217 MiB) are the last burst
   with nothing left to reuse them, which is what the previous file's
   "222 MiB inside the 10 s window" was: a snapshot of one burst, not a
   backlog.
2. Below the cycle the curve turns into refaults. At 500 ms about a tenth
   of every burst is past its first steps before it is reused (weight 0.9
   at 120 ms of 500), -1.4% tail mean for +9% CPU per
   request in the high windows and -8.5% blob req/s in the low
   ones. At 0 every freed run is `MADV_DONTNEED`ed at the free and faulted
   back in by the next allocation: 7.5 M eager purges, 4.2 TB
   over 1200 s (3.5 GB/s of purge and refault), `challoc:huge` 15 MiB
   at the end, Rss at the low windows' ends -169 MiB, the tail mean
   -8.2% (closing 47% of the gap to glibc-arena2's tail mean and
   26% of glibc-trim's), for +26% CPU per request in the high
   windows, +17% in the low, -16% blob req/s in the low windows
   and a blob p99 in the high windows +32%. The RSS bought is linear
   in the CPU spent; no value on this axis is free.
3. Why glibc-trim gets to a lower tail mean with +4.7%..+7.7% less CPU per request than challoc:
   `malloc_trim(0)` every 5 s purges whatever is free at that instant
   regardless of age, then nothing for 5 s, so each burst's pages are
   purged at most once per 5 s (tens of MB/s of refaults) instead of once
   per cycle, and its Rss at the window ends spans 716..1145 MiB in
   the two files because the sample lands at a random phase of those 5 s.
   An age-based window cannot reproduce that on a churn whose cycle is
   shorter than any window worth having; a purge on a timer regardless of
   age can. `challoc-trim` in the companion file
   `2026-09-20-aarch64-jvm-huge-decay-trim.md` is that experiment on challoc
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
   every 5 s. Tail mean -5.8% against the default and peak -7.6%,
   at CPU per request 51.2 / 53.6 against 53.2 / 54.9 (-4% / -2%) and blob req/s in the low windows +3.0%:
   it costs nothing (a purge of what is free at that instant, once per
   5 s) and buys more than `huge_dirty_decay_ms:500` at +9..10% CPU, though
   less than 0. Its named regions at the end: `challoc:classes` 166.8 +
   `challoc:huge` 81.4 + meta = 274 MiB against the default's
   518: the age-blind purge also drains the class tier's fully
   empty slabs (~90 MiB waiting in the 10 s window), which no huge key
   touches. Its window ends span 846..1175 MiB, the same sawtooth as
   glibc-trim's, and its tail mean is still above glibc-arena2's
   981 and glibc-trim's 969: what is left is the
   half-occupied slabs, which no purge can give back, and the thread
   caches. This is the mechanism to build if the JVM gap is to close:
   a purge on a timer (or at a lull's start) regardless of age, not a
   shorter window.

## Valkey guard: the soak under the strongest candidate

`make valkey-soak ALLOCS=challoc DURATION=1200` twice on the same VM after the JVM rows (`make valkey STAGE=build`, Valkey 9.1.2 with the contrib patch, `quantum8`; the acceptance protocol's phases: 600 s write, 600 s delete, 60 s settle, activedefrag on, no cgroup peak reset), the default configuration first and `CHALLOC_CONF=huge_dirty_decay_ms:0` second: the value that moved the JVM most, so if it leaves Valkey alone every milder value does. Summaries under `target/valkey/soak-hd-default/` and `soak-hd-0/` on the VM (`2026-09-21-aarch64-valkey-soak.md`, not fetched: `RESULT_DIR` outside `bench/results/` because the two challoc-only runs are a guard, not an acceptance file). Positive = the candidate higher.

| measure | challoc, default | challoc, `huge_dirty_decay_ms:0` | candidate vs default |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 932922 | 916500 | -1.8% |
| write: valkey-benchmark p99 ms | 1.047 | 1.079 | +3.1% |
| write: memtier req/s | 964751 | 934429 | -3.1% |
| write: memtier p99 ms | 0.951 | 1.015 | +6.7% |
| delete: valkey-benchmark req/s | 1430296 | 1403430 | -1.9% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.727 | +2.3% |
| delete: memtier req/s | 1255377 | 1217795 | -3.0% |
| delete: memtier p99 ms | 0.759 | 0.807 | +6.3% |
| peak RSS MiB (VmHWM) | 580.5 | 576.1 | -0.8% |
| RSS MiB at end of write phase | 580.3 | 575.9 | -0.8% |
| RSS MiB at end of delete phase | 131.5 | 128.3 | -2.4% |
| RSS MiB after settle | 131.4 | 128.3 | -2.4% |
| allocator_frag_ratio after settle | 1.81 | 1.78 | - |
| keys after settle | 105978 | 105385 | - |

RSS unchanged (VmHWM -0.8%, after settle -2.4%, inside the 4..5% two-soak spread of the RSS rows). Throughput -2..-3% and the write p99 +0.03 ms (valkey-benchmark, +3.1%) and +0.06 ms (memtier, +6.7%): at the edge of, not inside, the spread two same-day soaks of one server showed on 2026-09-17 (`2026-09-17-aarch64-gce-slab-geometry-soak{1,2}-valkey-soak.md`: the stock jemalloc column 0.8..1.0% apart on the write rows, challoc's 1.7..2.5% between two commits), in one run each, with the default soak run first (straight after the JVM chain and the quick) and the candidate three hours later after the trim soak. The key can only act where a huge run is freed, and this server frees one at a dict rehash (the 500 k-key table's old buckets on growth in the write phase, on shrink in the delete phase: a handful of runs of a few MiB per phase, each purged at the free and refaulted by the next rehash under `0`), so the mechanism has nothing to move the p99 of 900 k SET/s with; the guard reads unchanged on RSS and within a single run's spread on p99, and a second pair would settle the last 0.03..0.06 ms if the key were ever proposed as a default, which it is not (below).

## Quick tier (`make quick OLD=main`, one round each, on the VM)

| run | new vs old per row (positive = new worse) | verdict |
|---|---|---|
| the code change alone (default conf) | larson1 -0.2%, larson10 -0.2%, xmalloc +0.1%, alloc-test1 +0.1%, valkey1 +4.7%, churn compacts | go |
| `NEW_CONF=huge_dirty_decay_ms:0` | larson1 +0.2%, larson10 -0.2%, xmalloc -0.0%, alloc-test1 -0.1%, valkey1 -0.2%, churn compacts | go |
| `NEW_CONF=huge_dirty_decay_ms:500` | larson1 +0.7%, larson10 -1.9%, xmalloc +0.0%, alloc-test1 +0.3%, valkey1 +0.5%, churn compacts | go |

Three rounds of the same two libraries (old `abb5e8bab687df0b` = main, new `7ed09e4e6424620f` = 27f2f59's `src/`): valkey1 +4.7%, -0.2%, +0.5%, so the first round's +4.7% is the row's one-round noise, and the codegen compare shows no per-allocation function changed.

## Verdict

No value of `huge_dirty_decay_ms` closes most of the JVM gap without paying for it in CPU, and the shipped default stays as it is (the key follows `dirty_decay_ms` unless set): 2000 is inside the noise; 500 buys 1.4..2.5% of tail-mean Rss for 9..10% CPU per request; 0 buys 6..8% (about half the gap to glibc-arena2, a quarter of glibc-trim's) for 22..26% CPU per request and 16% of the lulls' blob req/s. The huge extents at a lull's end are one GC burst in flight, not a backlog, and any age-based window shorter than the ~120 ms GC cycle purges and refaults every burst. The mechanism that does close the gap is a purge of whatever is free at the moment regardless of age, on a timer: `challoc-trim` (companion file) took 3.6..5.8% off the tail mean and 3..8% off the peak for no CPU, and drained the class tier's empty slabs besides the huge tier's. Valkey is unchanged under the strongest value on x86_64 and within one run's spread on aarch64, with nothing for the key to act on there; the quick tier is `go` on both architectures with and without the key.
