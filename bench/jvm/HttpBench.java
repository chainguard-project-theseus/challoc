// HttpBench: the JVM server row of bench/jvm-bench.sh (docs/plan.md Phase 3
// task 5 (a)). A plain JDK program, no build tool: bench/jvm-bench.sh runs
// `javac -d target/jvm bench/jvm/HttpBench.java` once and starts
//
//   java -Xms512m -Xmx512m -XX:+AlwaysPreTouch -cp target/jvm HttpBench serve PORT THREADS
//
// under each allocator (LD_PRELOAD). The server is the JDK's own
// com.sun.net.httpserver on a fixed-size THREADS-thread executor (200 in the
// protocol) with three endpoints:
//
//   /json   a small JSON body (about 300 bytes) built per request from a
//           StringBuilder: ordinary short-lived Java objects, which the fixed
//           Java heap absorbs; the allocator sees only the JVM's own native
//           work for the request (the socket read/write path, the response
//           headers' byte[]s are heap, the JIT's arenas early on)
//   /blob   64 KiB copied out of a per-request ByteBuffer.allocateDirect(65536):
//           every request is one malloc(65536) in the JVM (Unsafe.allocateMemory
//           -> os::malloc) and its free happens when the Cleaner of the buffer
//           runs after a GC, in bursts, so native memory churns in a pattern the
//           Java heap does not smooth; -XX:MaxDirectMemorySize is left at its
//           default (= -Xmx), so at most 512 MiB of direct buffers are live
//           before Bits.reserveMemory forces a System.gc()
//   /quit   answers, then System.exit(0) so the allocator's atexit stats print
//           (MALLOC_CONF=stats_print:true, MIMALLOC_SHOW_STATS=1) runs
//
// The soak mode (bench/jvm-bench.sh ROWS=soak, second iteration of the task,
// 2026-09-20: a shape that can separate the allocators, where a 5-minute run
// with one buffer size never lets glibc's arenas fragment)
//
//   java ... HttpBench soak PORT CORE MAX KEEPALIVE_S MIN_BYTES MAX_BYTES
//
// serves the same endpoints from a ThreadPoolExecutor that grows and shrinks
// (core CORE, maximum MAX, idle threads retired after KEEPALIVE_S seconds, a
// SynchronousQueue so a request beyond the live threads starts a new one;
// 16 / 400 / 2 s in the protocol) while the load generator alternates 60 s
// at 256 connections with 60 s at 8, so threads are created and retired all
// run long (each new thread is a fresh glibc arena candidate and a fresh
// per-thread cache in every allocator), and /blob draws its body size per
// request from a log-uniform distribution between MIN_BYTES and MAX_BYTES
// (256 bytes .. 1 MiB in the protocol: direct ByteBuffers of mixed sizes, so
// no single size class dominates and fragmentation can appear). The body
// is copied out of the direct buffer in 64 KiB heap chunks, so the Java
// heap sees no humongous arrays and the native side dominates. /stats
// answers "pool=<live> largest=<max ever> active=<busy> completed=<tasks>".
//
// `HttpBench maps` prints the allocator libraries in /proc/self/maps and
// exits (the harness's binding check).
//
// The program prints "READY <port>" on stdout once it listens; the harness
// polls for that line. The Java heap is fixed and pre-touched by the command
// line above, so the RSS differences between allocators are native memory
// only.

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpHandler;
import com.sun.net.httpserver.HttpServer;

import java.io.IOException;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.SynchronousQueue;
import java.util.concurrent.ThreadLocalRandom;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicLong;

public final class HttpBench {
    static final int BLOB = 65536;
    static final int TEMPLATE_MAX = 1 << 20;
    static final byte[] TEMPLATE = new byte[TEMPLATE_MAX];
    static final AtomicLong COUNTER = new AtomicLong();

    static {
        for (int i = 0; i < TEMPLATE_MAX; i++) TEMPLATE[i] = (byte) (i * 31 + 7);
    }

    public static void main(String[] args) throws Exception {
        if (args.length == 0) usage();
        switch (args[0]) {
            case "maps" -> maps();
            case "serve" -> serve(Integer.parseInt(args[1]), Executors.newFixedThreadPool(Integer.parseInt(args[2])), new BlobHandler());
            case "soak" -> {
                int core = Integer.parseInt(args[2]), max = Integer.parseInt(args[3]), keep = Integer.parseInt(args[4]);
                int min = Integer.parseInt(args[5]), maxBytes = Integer.parseInt(args[6]);
                if (min < 8 || maxBytes > TEMPLATE_MAX || min > maxBytes) usage();
                ThreadPoolExecutor pool = new ThreadPoolExecutor(core, max, keep, TimeUnit.SECONDS,
                    new SynchronousQueue<>(), new ThreadPoolExecutor.CallerRunsPolicy());
                serve(Integer.parseInt(args[1]), pool, new MixedBlobHandler(min, maxBytes));
            }
            default -> usage();
        }
    }

    static void usage() {
        System.err.println("usage: HttpBench serve PORT THREADS | HttpBench soak PORT CORE MAX KEEPALIVE_S MIN_BYTES MAX_BYTES | HttpBench maps");
        System.exit(2);
    }

    // The allocator libraries mapped into this process, one path per line,
    // sorted and unique: the harness compares them with what LD_PRELOAD named.
    static void maps() throws IOException {
        Files.lines(Paths.get("/proc/self/maps"))
            .filter(l -> l.matches(".*/lib(challoc|jemalloc|mimalloc|tcmalloc)[^/ ]*\\.so.*"))
            .map(l -> l.substring(l.indexOf('/')))
            .distinct()
            .sorted()
            .forEach(System.out::println);
    }

    static void serve(int port, ExecutorService pool, HttpHandler blob) throws IOException {
        HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", port), 4096);
        server.setExecutor(pool);
        server.createContext("/json", new JsonHandler());
        server.createContext("/blob", blob);
        server.createContext("/stats", ex -> {
            String s = pool instanceof ThreadPoolExecutor t
                ? "pool=" + t.getPoolSize() + " largest=" + t.getLargestPoolSize() + " active=" + t.getActiveCount() + " completed=" + t.getCompletedTaskCount() + "\n"
                : "pool=fixed\n";
            reply(ex, "text/plain", s.getBytes(StandardCharsets.US_ASCII));
        });
        server.createContext("/quit", ex -> {
            reply(ex, "text/plain", "bye\n".getBytes(StandardCharsets.US_ASCII));
            // Stop accepting, then exit through the normal path so atexit
            // handlers (the allocators' stats prints) run.
            new Thread(() -> { server.stop(0); System.exit(0); }).start();
        });
        server.start();
        System.out.println("READY " + port);
        System.out.flush();
    }

    static void reply(HttpExchange ex, String type, byte[] body) throws IOException {
        // Drain the request body so keep-alive connections stay in sync.
        ex.getRequestBody().readAllBytes();
        ex.getResponseHeaders().set("Content-Type", type);
        ex.sendResponseHeaders(200, body.length);
        try (OutputStream os = ex.getResponseBody()) {
            os.write(body);
        }
    }

    // A small JSON body assembled per request: a counter, the time, a random
    // id and a short array. About 300 bytes; every piece is a Java object.
    static final class JsonHandler implements HttpHandler {
        @Override public void handle(HttpExchange ex) throws IOException {
            long n = COUNTER.incrementAndGet();
            ThreadLocalRandom r = ThreadLocalRandom.current();
            StringBuilder sb = new StringBuilder(384);
            sb.append("{\"id\":").append(n)
              .append(",\"time\":").append(System.nanoTime())
              .append(",\"token\":\"").append(Long.toHexString(r.nextLong())).append(Long.toHexString(r.nextLong()))
              .append("\",\"thread\":\"").append(Thread.currentThread().getName())
              .append("\",\"path\":\"").append(ex.getRequestURI().getPath())
              .append("\",\"values\":[");
            for (int i = 0; i < 16; i++) {
                if (i > 0) sb.append(',');
                sb.append(r.nextInt(100000));
            }
            sb.append("],\"ok\":true}\n");
            reply(ex, "application/json", sb.toString().getBytes(StandardCharsets.UTF_8));
        }
    }

    // 64 KiB through a fresh direct ByteBuffer per request: fill it from the
    // template, copy it out into a heap array, send that. The direct
    // buffer's native memory is one malloc(65536) now and one free when its
    // Cleaner runs after a later GC.
    static final class BlobHandler implements HttpHandler {
        @Override public void handle(HttpExchange ex) throws IOException {
            ByteBuffer direct = ByteBuffer.allocateDirect(BLOB);
            direct.put(TEMPLATE, 0, BLOB);
            direct.putLong(0, COUNTER.incrementAndGet());
            direct.flip();
            byte[] out = new byte[BLOB];
            direct.get(out);
            reply(ex, "application/octet-stream", out);
        }
    }

    // The soak's /blob: a fresh direct ByteBuffer of a log-uniform size in
    // [min, max] per request (one malloc(n) in the JVM, freed when the
    // buffer's Cleaner runs after a later GC), filled from the template and
    // copied out to the socket in 64 KiB heap chunks (no humongous heap
    // arrays: the Java heap's churn stays small and constant per byte, the
    // native side is what varies). Content-Length is the drawn size.
    static final class MixedBlobHandler implements HttpHandler {
        final double lnMin, lnRange;
        final int min, max;
        MixedBlobHandler(int min, int max) {
            this.min = min; this.max = max;
            lnMin = Math.log(min); lnRange = Math.log(max) - lnMin;
        }
        int draw() {
            int n = (int) Math.round(Math.exp(lnMin + ThreadLocalRandom.current().nextDouble() * lnRange));
            return Math.max(min, Math.min(max, n));
        }
        @Override public void handle(HttpExchange ex) throws IOException {
            int n = draw();
            ByteBuffer direct = ByteBuffer.allocateDirect(n);
            direct.put(TEMPLATE, 0, n);
            if (n >= 8) direct.putLong(0, COUNTER.incrementAndGet());
            direct.flip();
            ex.getRequestBody().readAllBytes();
            ex.getResponseHeaders().set("Content-Type", "application/octet-stream");
            ex.sendResponseHeaders(200, n);
            byte[] chunk = new byte[Math.min(n, BLOB)];
            try (OutputStream os = ex.getResponseBody()) {
                while (direct.hasRemaining()) {
                    int k = Math.min(chunk.length, direct.remaining());
                    direct.get(chunk, 0, k);
                    os.write(chunk, 0, k);
                }
            }
        }
    }
}
