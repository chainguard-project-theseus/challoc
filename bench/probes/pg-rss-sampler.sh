#!/bin/sh
# pg-rss-sampler.sh: the RSS sidecar of bench/postgres-bench.sh (docs/plan.md
# Phase 3 task 5 (b)). Every INTERVAL seconds until DIR/stop exists it reads
# /proc/<pid>/smaps_rollup for every `postgres` process of OSUSER (the
# postmaster, its client backends and its auxiliary processes) and prints
# one sample to stdout:
#
#   S <n> <epoch> <postmaster_pid> <processes>
#   P <pid> <role> <pss_kb> <pss_anon_kb> <pss_shmem_kb> <rss_kb>   (one per process)
#
# role is postmaster (the pid in PGDATA/postmaster.pid), backend (a process
# whose title starts with "postgres: <DBUSER> ", the client backends pgbench
# connects to) or aux (checkpointer, background writer, walwriter, autovacuum
# launcher and workers, logical replication launcher). Pss is proportional
# set size, so the sum over every process counts each shared_buffers page
# once; Pss_Anon is the anonymous share (the allocator's heap, stacks), which
# is where the allocators differ; Pss_Shmem is the process's share of the
# shared memory segment.
#
# It runs as OSUSER (postgres-bench.sh starts it with `su`): the container's
# root has no CAP_SYS_PTRACE and cannot read another uid's smaps_rollup, the
# owner of the processes can. One `pgrep`, one `ps` and one awk per sample;
# awk reads the smaps_rollup files with getline, so a process that exits
# between the listing and the read is skipped rather than failing the sample.
#
#   sh pg-rss-sampler.sh OSUSER DBUSER PGDATA INTERVAL DIR
set -u
OSUSER=$1; DBUSER=$2; PGDATA=$3; INTERVAL=$4; DIR=$5
n=0
while [ ! -e "$DIR/stop" ]; do
  pm=$(head -1 "$PGDATA/postmaster.pid" 2>/dev/null || echo 0)
  pids=$(pgrep -d, -u "$OSUSER" -x postgres || true)
  ts=$(date +%s)
  if [ -n "$pids" ]; then
    ps -o pid=,args= -p "$pids" 2>/dev/null
  fi | awk -v user="$DBUSER" -v pm="$pm" -v n="$n" -v ts="$ts" '
    {
      pid = $1; args = substr($0, index($0, $2))
      if (pid == pm) role = "postmaster"
      else if (index(args, "postgres: " user " ") == 1) role = "backend"
      else role = "aux"
      f = "/proc/" pid "/smaps_rollup"; pss = ""; anon = ""; shm = ""; rss = ""
      while ((getline l < f) > 0) {
        split(l, a, /:? +/)
        if (a[1] == "Pss") pss = a[2]; else if (a[1] == "Pss_Anon") anon = a[2]
        else if (a[1] == "Pss_Shmem") shm = a[2]; else if (a[1] == "Rss") rss = a[2]
      }
      close(f)
      if (pss != "") out[++k] = "P " pid " " role " " pss " " anon " " shm " " rss
    }
    END { print "S " n " " ts " " pm " " k+0; for (i = 1; i <= k; i++) print out[i] }'
  n=$((n+1))
  i=0; while [ $i -lt "$INTERVAL" ] && [ ! -e "$DIR/stop" ]; do sleep 1; i=$((i+1)); done
done
