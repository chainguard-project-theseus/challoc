# bench/cow: fork copy-on-write under BGSAVE

The measurement behind fork epochs (docs/design.md section 5, "Fork
epochs"; `challoc_cow_begin`, `challoc_cow_end`): how much memory a
Valkey BGSAVE copies while the parent keeps writing, and what the epoch
changes.

`cowbench.py` runs one cell: it starts `valkey-server` (`--save ""`,
`rdbcompression no`, `io-threads 1`), loads N keys of V-byte values,
stretches a BGSAVE with `rdb-key-save-delay`, overwrites W random keys at
a constant rate while the child runs, and samples the child's
`Private_Dirty` per VMA span (`/proc/<child>/smaps`) and the parent's RSS.
It prints one JSON line: `rdb_last_cow_size` (the child's peak
Private_Dirty, which the sampler agrees with), the peak of parent RSS plus
child dirty, and, with `--bench`, `valkey-benchmark` SET throughput
outside and inside the save, or, with `--recover S --defrag`, the parent's
RSS every few seconds for S seconds after the save with `activedefrag
yes` (and the defragmenter's hits). `run.sh` is the matrix (64 B embedded
and 1 KiB raw values, 1% and 10% of the keys overwritten during the save,
three repetitions); `summarize.py` prints medians.

The builds: Valkey 9.1.2 with `contrib/valkey/0003` (no write to an
overwritten or deleted value when no module observes it; without it the
epoch saves nothing on embedded strings, since every overwrite writes the
old value's refcount), under the bundled jemalloc; with 0001 and 0002
under challoc; and with 0004 too (the epoch around `serverFork`, and the
drain), against a quantum-8 challoc built with `--no-default-features
--features quantum8`, each linked with an rpath to its library.

The harness tier is the `cow` suite (`bench/suites/cow.toml`; `make
bench-launch SUITE=cow`): its rows (`valkey-cow`, `valkey-cow-d1k`,
`valkey-cow-tput`, `valkey-cow-rec`, bench/rows.toml) run
`bench/drivers/valkey-cow.sh`, which builds Valkey per variant (the
runpath build with 0001 to 0004 for challoc base and head, the bundled
jemalloc as the control) and drives `cowbench.py` with its child sampler,
one variant per VM on the gate cells. `run.sh` is the session matrix
above: numbers from it are session measurements, not harness-tier;
`RESULTS.md` keeps the two apart.
