#!/bin/sh
# mstats-classes.sh FILE [LO [HI]]: per-class totals over every arena of a
# challoc `malloc_stats_print` report (the `-mstats.txt` a soak saves, or
# one file of the Valkey soak's OCCUPANCY=1 sidecar): for each class with
# LO <= size <= HI (default 1280..4096, the classes the geometry variants
# move) the summed curregs and curslabs, the occupancy `curregs / (curslabs
# * nregs)`, live bytes and slab bytes in MiB, and the summed compaction
# counters (c_on, c_off, gated), as one markdown table. The report's per-
# arena bins tables are summed; nregs and slab_size are the class's, the
# same in every arena. Used by bench/results/2026-09-21-*-policy-cost-*.md.
set -eu
F=${1:?mstats file}; LO=${2:-1280}; HI=${3:-4096}
awk -v lo="$LO" -v hi="$HI" '
  /^  bins:/ { inbins = 1; next }
  /^  slab occupancy/ || /^arenas\[/ || /^$/ { inbins = 0 }
  inbins && NF == 15 && $1 ~ /^[0-9]+$/ {
    sz = $1 + 0
    if (sz < lo || sz > hi) next
    nregs[sz] = $3; slab[sz] = $4
    regs[sz] += $5; slabs[sz] += $6; nonfull[sz] += $7; empty[sz] += $8; purged[sz] += $9
    refilled[sz] += $10; compacted[sz] += $11; con[sz] += $12; coff[sz] += $13; gated[sz] += $14
    seen[sz] = 1
  }
  END {
    n = 0; for (s in seen) sizes[++n] = s + 0
    # insertion sort of the sizes
    for (i = 2; i <= n; i++) { t = sizes[i]; for (j = i - 1; j >= 1 && sizes[j] > t; j--) sizes[j+1] = sizes[j]; sizes[j+1] = t }
    print "| class | nregs | slab KiB | curregs | curslabs | nonfull | occupancy | live MiB | slab MiB | pinned MiB | c_on | c_off | gated |"
    print "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    for (i = 1; i <= n; i++) {
      s = sizes[i]
      cap = slabs[s] * nregs[s]
      occ = cap ? 100 * regs[s] / cap : 0
      live = regs[s] * s / 1048576; sb = slabs[s] * slab[s] / 1048576
      printf "| %d | %d | %d | %d | %d | %d | %.1f%% | %.1f | %.1f | %.1f | %d | %d | %d |\n", s, nregs[s], slab[s] / 1024, regs[s], slabs[s], nonfull[s], occ, live, sb, sb - live, con[s], coff[s], gated[s]
      tl += live; ts += sb
    }
    printf "| total | | | | | | %s | %.1f | %.1f | %.1f | | | |\n", ts ? sprintf("%.1f%%", 100 * tl / ts) : "-", tl, ts, ts - tl
  }' "$F"
