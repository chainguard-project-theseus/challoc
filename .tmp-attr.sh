#!/bin/bash
# host side: build, then for each run trace the kernel side with bpftrace
set -u
rm -rf evalpre; mkdir evalpre
rsync -a --exclude target --exclude .git "$DEVVM_BASE_DIR"/ evalpre/
F='--memory=3g --memory-swap=3g'
make run EXTRA_DOCKER_FLAGS="$F" CMD='sh evaltools/build-in.sh' || exit 1
cat > /tmp/eval27.bt <<'BT'
tracepoint:syscalls:sys_enter_madvise /comm == "xmalloc-test"/ { @ms[tid] = nsecs; @madv_calls = count(); @madv_adv[args->behavior] = count(); @madv_bytes[args->behavior] = sum(args->len_in); }
tracepoint:syscalls:sys_exit_madvise /@ms[tid]/ { @madv_ns = sum(nsecs - @ms[tid]); delete(@ms[tid]); }
software:page-faults:1 /comm == "xmalloc-test"/ { @pf = count(); }
kprobe:handle_mm_fault /comm == "xmalloc-test"/ { @fs[tid] = nsecs; }
kretprobe:handle_mm_fault /@fs[tid]/ { @fault_ns = sum(nsecs - @fs[tid]); delete(@fs[tid]); }
tracepoint:syscalls:sys_enter_futex /comm == "xmalloc-test"/ { @futex = count(); }
profile:hz:499 /comm == "xmalloc-test"/ { @samples = count(); @k[kstack(4)] = count(); }
END { clear(@ms); clear(@fs); print(@k, 8); clear(@k); }
BT
for rep in 1 2 3; do
  for w in 10 16; do
    for v in ${VARIANTS:-fixed slow pre}; do
      sudo bpftrace -q /tmp/eval27.bt > /tmp/eval27.out 2>&1 &
      bp=$!
      sleep 6
      make -s run EXTRA_DOCKER_FLAGS="$F" CMD="sh evaltools/run-in.sh $v $w" 2>&1 | grep -v '^$'
      sudo kill -INT $bp; wait $bp
      echo "-- bpftrace rep=$rep v=$v w=$w"
      grep -v '^$' /tmp/eval27.out
    done
  done
done
