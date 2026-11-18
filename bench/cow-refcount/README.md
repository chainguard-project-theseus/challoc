# Valkey refcount pin and fork COW (session measurements)

Not harness-tier numbers. They come from one develop session: amd64, 4 vCPU,
15 GiB, kernel 6.18. Valkey 9.1.2, `io-threads 1`, `rdbcompression no`,
BGSAVE stretched to about 60 s with `rdb-key-save-delay`. One million random
keys are changed during the save. The driver is
`bench/cow-spike/cowbench.py` from the `spike/cow-epoch` branch, with its
data directory moved to `/tmp/w/m/data`.

The builds (`build.sh`):

| label | Valkey | allocator |
|---|---|---|
| je | stock 9.1.2 | bundled jemalloc 5.3.0 |
| jef | adds `contrib/valkey/0003` | bundled jemalloc 5.3.0 |
| ch | contrib 0001+0002 plus spike 0003 (the epoch hook, inert here) | challoc `spike/cow-epoch`, `cow_epoch` off |
| chf | as ch, plus `contrib/valkey/0003` | as ch |
| epx | as ch | `cow_epoch:true,exact_sized_free:true` |
| epxf | as chf | `cow_epoch:true,exact_sized_free:true` |

`rdb_last_cow_size` in MiB, two runs each (`results.jsonl`,
`results-del.jsonl`):

| workload | je | jef | ch | chf | epx | epxf |
|---|---|---|---|---|---|---|
| 10M x 64 B, 1M DEL | 1069, 1068 | 166, 165 | 1061, 1061 | 158, 158 | 1061, 1061 | 158, 158 |
| 10M x 64 B, 1M SET (embstr) | 1062, 1058 | 1062, 1062 | 1059, 1059 | 1059, 1059 | 1061, 1060 | 157, 158 |
| 2M x 1 KiB, 200k SET (raw) | 897, 896 | 897, 896 | 876, 876 | 876, 876 | 79, 79 | 79, 79 |

Peak of parent RSS plus child dirty, in MiB: DEL 2164 → 1260 (jemalloc), 2144 →
1241 (challoc); embstr SET with the epoch 2231 → 1328.

The rest of the copy after the fix is the hashtable bucket pages (about
130 MiB at 10M keys), which every delete and overwrite writes in place.
