// bench/node/server.js: the HTTP server of the node-bench `server` row
// (docs/plan.md Phase 3 task 5 (c)). Plain `http`, no dependencies. Two
// endpoints, so the load generator exercises two allocation shapes:
//
//   GET /json   a small JSON body built per request (an object with a
//               counter, a timestamp, a few strings and a 16-element array,
//               stringified: about 400 bytes). Every request allocates the
//               object graph in the V8 heap and the response string; the
//               allocator sees the socket buffers and the http parser's
//               allocations, not the JS objects.
//   GET /blob   a 1 MiB body from a per-request Buffer.allocUnsafe(1 << 20),
//               filled (so the pages are touched), written in 64 KiB chunks
//               (subarray views of the same backing store) with an explicit
//               Content-Length. allocUnsafe above the 4 KiB pool threshold
//               takes a fresh ArrayBuffer whose backing store is one
//               malloc(1 MiB) through V8's array buffer allocator and one
//               free when the GC collects the Buffer, so 1 MiB backing
//               stores churn through the allocator under test at the
//               request rate.
//
// Usage: node --max-old-space-size=256 server.js PORT [PIDFILE]
// Prints "listening PORT pid PID" on stdout once bound. SIGTERM closes the
// listener and exits 0. Nothing is logged per request.
'use strict';
const http = require('http');
const fs = require('fs');

const port = Number(process.argv[2] || 8080);
const pidfile = process.argv[3];
const CHUNK = 64 * 1024;
const BLOB = 1 << 20;
let counter = 0;

function json(req, res) {
  counter++;
  const obj = {
    id: counter,
    ts: Date.now(),
    method: req.method,
    url: req.url,
    agent: req.headers['user-agent'] || '',
    tags: ['alpha', 'beta', 'gamma', 'delta'],
    values: Array.from({ length: 16 }, (_, i) => (counter + i) * 7919 % 10007),
    nested: { ok: true, depth: 2, label: 'request-' + counter },
  };
  const body = JSON.stringify(obj);
  res.writeHead(200, {
    'Content-Type': 'application/json',
    'Content-Length': Buffer.byteLength(body),
  });
  res.end(body);
}

function blob(req, res) {
  counter++;
  const buf = Buffer.allocUnsafe(BLOB);
  buf.fill(counter & 0xff);
  res.writeHead(200, {
    'Content-Type': 'application/octet-stream',
    'Content-Length': BLOB,
  });
  let off = 0;
  const pump = () => {
    // Write 64 KiB views until the socket asks us to wait (write returned
    // false), then resume on 'drain'; the last chunk goes through end().
    while (off < BLOB) {
      const end = off + CHUNK;
      const view = buf.subarray(off, end);
      off = end;
      if (off >= BLOB) { res.end(view); return; }
      if (!res.write(view)) { res.once('drain', pump); return; }
    }
  };
  pump();
}

const server = http.createServer((req, res) => {
  if (req.url === '/json') return json(req, res);
  if (req.url === '/blob') return blob(req, res);
  res.writeHead(404, { 'Content-Length': 0 });
  res.end();
});
server.keepAliveTimeout = 60000;
server.listen(port, '127.0.0.1', () => {
  if (pidfile) fs.writeFileSync(pidfile, String(process.pid));
  console.log(`listening ${port} pid ${process.pid}`);
});
process.on('SIGTERM', () => { server.close(() => process.exit(0)); setTimeout(() => process.exit(0), 2000).unref(); });
