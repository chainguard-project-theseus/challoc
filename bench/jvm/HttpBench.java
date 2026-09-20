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
import java.util.concurrent.ThreadLocalRandom;
import java.util.concurrent.atomic.AtomicLong;

public final class HttpBench {
    static final int BLOB = 65536;
    static final byte[] TEMPLATE = new byte[BLOB];
    static final AtomicLong COUNTER = new AtomicLong();

    static {
        for (int i = 0; i < BLOB; i++) TEMPLATE[i] = (byte) (i * 31 + 7);
    }

    public static void main(String[] args) throws Exception {
        if (args.length == 0) usage();
        switch (args[0]) {
            case "maps" -> maps();
            case "serve" -> serve(Integer.parseInt(args[1]), Integer.parseInt(args[2]));
            default -> usage();
        }
    }

    static void usage() {
        System.err.println("usage: HttpBench serve PORT THREADS | HttpBench maps");
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

    static void serve(int port, int threads) throws IOException {
        HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", port), 4096);
        ExecutorService pool = Executors.newFixedThreadPool(threads);
        server.setExecutor(pool);
        server.createContext("/json", new JsonHandler());
        server.createContext("/blob", new BlobHandler());
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
            direct.put(TEMPLATE);
            direct.putLong(0, COUNTER.incrementAndGet());
            direct.flip();
            byte[] out = new byte[BLOB];
            direct.get(out);
            reply(ex, "application/octet-stream", out);
        }
    }
}
