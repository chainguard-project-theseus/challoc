// bench/node/json_churn.js: the node-bench `json_churn` row:
// JSON.parse(JSON.stringify(obj)) over a 177 KiB
// object N times (default 2000) in one process, the shape of the Python
// rows' json_bench.py. The object is 400 records of mixed strings, numbers,
// arrays and nested objects; JSON.stringify gives 180906 bytes (the
// harness prints the byte count in the results header); the stringify result is a fresh string of that size per iteration
// (V8 keeps strings this large off the young generation's fast path and
// the external string/backing store path exercises the allocator), and
// the parse rebuilds the object graph in the V8 heap. Prints
// "iterations=N bytes=B wall_s=S" on stdout; the harness takes wall time
// and peak RSS from bench/rt around the process.
'use strict';
const N = Number(process.argv[2] || 2000);
const records = [];
for (let i = 0; i < 400; i++) {
  records.push({
    id: i,
    name: 'record-' + i + '-' + 'x'.repeat(24),
    email: 'user' + i + '@example.invalid',
    score: i * 1.5,
    active: i % 3 === 0,
    tags: ['t' + (i % 7), 't' + (i % 11), 't' + (i % 13)],
    history: Array.from({ length: 8 }, (_, j) => ({ at: 1700000000 + i * 60 + j, delta: (i * j) % 97 - 48 })),
    note: 'lorem ipsum dolor sit amet '.repeat(2),
  });
}
const obj = { version: 1, generated: 'bench', records };
const bytes = JSON.stringify(obj).length;
let sink = 0;
const t0 = process.hrtime.bigint();
for (let k = 0; k < N; k++) {
  const copy = JSON.parse(JSON.stringify(obj));
  sink += copy.records[k % 400].history[k % 8].delta;
}
const wall = Number(process.hrtime.bigint() - t0) / 1e9;
console.log(`iterations=${N} bytes=${bytes} wall_s=${wall.toFixed(3)} sink=${sink}`);
