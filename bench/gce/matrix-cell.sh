#!/usr/bin/env bash
# matrix-cell.sh: the VM side of `challoc-bench matrix` (docs/plan.md Phase
# 2.5 task 3). Runs on a throwaway matrix VM from ~/challoc, detached, and
# writes everything under ~/logs: one log per step and the combined
# matrix.log the driver fetches. The last line of matrix.log is the
# machine-readable summary the driver turns into the results table:
#
#   MATRIX cell=<cell> image=<name> kernel=<uname -r> page=<bytes> machine=<type>
#          tests=<passed>/<failed> fuzz=PASS|FAIL cgroup=PASS|FAIL
#          privileged=PASS|FAIL|- wall=<s> outcome=PASS|FAIL
#   PROBE <the features: line of tests/c/features.c>
#   PROBE_PRIVILEGED <the same under DOCKER_FLAGS=--privileged>
#
# Steps, in order: make image, make test (release build, unit tests, the
# api tests with the C programs, the 512 MiB cgroup-limit tests, the churn),
# the probe line (tests/c/features.c under LD_PRELOAD in the container, plus
# the container's own page size), make fuzz, make test-cgroup (the churn
# again and the 300 s oscillation), and, when the second argument is
# `privileged`, `make test` once more with DOCKER_FLAGS=--privileged so
# Docker's default seccomp profile does not hide process_madvise and mseal
# from the library (the plan's bare-metal/nested-KVM substitute). A step
# that fails does not stop the following ones: every cell records every
# step.
#
#   bash bench/gce/matrix-cell.sh CELL IMAGE [privileged]
set -u

cell="${1:?cell name}"
image="${2:?image name}"
extra="${3:-}"
LOG="$HOME/logs"
mkdir -p "$LOG"
cd "$HOME/challoc" || { echo "no ~/challoc"; exit 1; }

t0=$(date +%s)
machine="$(curl -sf -H 'Metadata-Flavor: Google' \
  http://metadata.google.internal/computeMetadata/v1/instance/machine-type 2>/dev/null | sed 's#.*/##')"
[ -n "$machine" ] || machine=unknown
os="$(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}")"
page="$(getconf PAGESIZE)"
echo "== cell $cell on $(hostname): $os"
echo "== image $image, machine $machine, $(uname -m) $(uname -r), page size $page"
echo "== cgroup fs: $(stat -fc %T /sys/fs/cgroup 2>/dev/null || echo unknown), docker: $(docker --version 2>/dev/null || echo none)"
echo "== tree: $(git rev-parse --short HEAD 2>/dev/null) $(git status --porcelain 2>/dev/null | wc -l | tr -d ' ') dirty"
echo "== start $(date -u +%FT%TZ)"

# step NAME CMD...: run CMD with its output in $LOG/NAME.log, print the
# outcome and the tail, and record PASS/FAIL in the variable named
# RC_<NAME> (dashes become underscores).
step() {
  local name="$1"; shift
  local s e rc var
  echo
  echo "== step $name: $*  ($(date -u +%H:%M:%S))"
  # A core dumped by an aborting test lands in the tree as root's file and
  # breaks the next docker build's context; provisioning discards cores,
  # this is the belt.
  sudo rm -f "$HOME/challoc/core" "$HOME"/challoc/core.[0-9]* 2>/dev/null
  s=$(date +%s)
  "$@" > "$LOG/$name.log" 2>&1
  rc=$?
  e=$(date +%s)
  echo "== step $name: rc=$rc in $((e - s)) s"
  tail -n 12 "$LOG/$name.log" | sed 's/^/   | /'
  var="RC_${name//-/_}"
  if [ "$rc" = 0 ]; then eval "$var=PASS"; else eval "$var=FAIL"; fi
  return 0
}

step image make image
step test make test

# The probe line: the same program the api tests run, standalone, so the
# line is recorded even if `make test` failed earlier; the container's
# view of the page size next to it.
probe_cmd() {
  make run CMD='getconf PAGESIZE; cc -O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=16 -Itests/c tests/c/features.c -o target/matrix-features -ldl && LD_PRELOAD=/src/target/release/libchalloc.so ./target/matrix-features'
}
step probe probe_cmd
# The same line with Docker's seccomp profile out of the way: what the
# library sees on this kernel when nothing hides process_madvise or mseal.
probe_priv_cmd() {
  DOCKER_FLAGS='--privileged' make run CMD='cc -O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=16 -Itests/c tests/c/features.c -o target/matrix-features -ldl && LD_PRELOAD=/src/target/release/libchalloc.so ./target/matrix-features'
}
step probe-privileged probe_priv_cmd

step fuzz make fuzz
step test-cgroup make test-cgroup

RC_test_privileged=-
if [ "$extra" = privileged ]; then
  priv_cmd() { DOCKER_FLAGS='--privileged' make test; }
  step test-privileged priv_cmd
fi

# Counts from every `test result:` line of make test (unit tests, api
# tests, the cgroup-limit tests).
counts() {
  awk '/^test result:/ { for (i = 1; i <= NF; i++) { if ($(i+1) == "passed;") p += $i; if ($(i+1) == "failed;") f += $i } } END { printf "%d/%d", p, f }' "$1" 2>/dev/null
}
tests="$(counts "$LOG/test.log")"
failed_names="$(grep -E '^test .* FAILED$|^    [a-z_:]+$' "$LOG/test.log" | sed -n 's/^test \(.*\) \.\.\. FAILED$/\1/p; s/^    \([a-z_:]\+\)$/\1/p' | sort -u | tr '\n' ',' | sed 's/,$//')"
probe_line="$(grep -m1 '^features: ' "$LOG/probe.log" || echo 'features: (none)')"
probe_priv_line="$(grep -m1 '^features: ' "$LOG/probe-privileged.log" || echo 'features: (none)')"
container_page="$(grep -m1 -E '^[0-9]+$' "$LOG/probe.log" || echo '?')"

outcome=PASS
for v in "$RC_image" "$RC_test" "$RC_probe" "$RC_probe_privileged" "$RC_fuzz" "$RC_test_cgroup" "$RC_test_privileged"; do
  [ "$v" = FAIL ] && outcome=FAIL
done
[ "$container_page" = "$page" ] || echo "== NOTE: container page size $container_page differs from the host's $page"
t1=$(date +%s)
echo
echo "== end $(date -u +%FT%TZ), wall $((t1 - t0)) s"
[ -n "$failed_names" ] && echo "FAILED_TESTS $failed_names"
echo "PROBE $probe_line"
echo "PROBE_PRIVILEGED $probe_priv_line"
echo "MATRIX cell=$cell image=$image kernel=$(uname -r) page=$page machine=$machine tests=$tests fuzz=$RC_fuzz cgroup=$RC_test_cgroup privileged=$RC_test_privileged wall=$((t1 - t0)) outcome=$outcome"
