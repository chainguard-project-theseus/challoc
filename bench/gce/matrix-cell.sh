#!/usr/bin/env bash
# matrix-cell.sh: the VM side of `challoc-bench matrix` (docs/plan.md Phase
# 2.5 tasks 3 and 4). Runs on a throwaway matrix VM from ~/challoc, detached,
# and writes everything under ~/logs: one log per step and the combined
# matrix.log the driver fetches. The last lines of matrix.log are the
# machine-readable summary the driver turns into the results table:
#
#   CPU <model name (lscpu / cpuinfo)>
#   PROBE <the features: line of tests/c/features.c>
#   PROBE_PRIVILEGED <the same under DOCKER_FLAGS=--privileged>
#   SCORECARD <the line make scorecard appended> (scorecard cells only)
#   MATRIX cell=<cell> image=<name> kernel=<uname -r> page=<bytes> machine=<type>
#          zone=<zone> vcpus=<nproc> tests=<passed>/<failed> fuzz=PASS|FAIL|-
#          cgroup=PASS|FAIL|- privileged=PASS|FAIL|- scorecard=PASS|FAIL|-
#          wall=<s> outcome=PASS|FAIL
#
# Steps, in order: make image, make test (release build, unit tests, the
# api tests with the C programs, the 512 MiB cgroup-limit tests, the churn),
# the probe line (tests/c/features.c under LD_PRELOAD in the container, plus
# the container's own page size), the same under --privileged, the CPU
# capture (model name, the cpuinfo flags line, lscpu, and the raw registers
# of bench/gce/cpuid-dump.c the host fixtures come from), then:
#
#   - a kernel cell (third argument empty or `privileged`): make fuzz, make
#     test-cgroup (the churn again and the 300 s oscillation) and, with
#     `privileged`, `make test` once more with DOCKER_FLAGS=--privileged so
#     Docker's default seccomp profile does not hide process_madvise and
#     mseal from the library (the plan's bare-metal/nested-KVM substitute);
#   - a CPU cell (third argument `scorecard`, task 4): one scorecard run,
#     `make scorecard ROUNDS=3` (challoc versus jemalloc, the fixed row
#     set) appended to bench/results/scorecard-<family>.md, where <family>
#     is the machine type's family (c4a, t2a, c4, c3, c3d, n2d, c2, n4), a
#     file per CPU next to the two reference files, which are not touched;
#     the note column names the machine, CPU model, zone and vCPU count.
#     fuzz and test-cgroup are skipped (the kernel matrix covers them).
#
# A step that fails does not stop the following ones: every cell records
# every step.
#
#   bash bench/gce/matrix-cell.sh CELL IMAGE [privileged|scorecard]
set -u

cell="${1:?cell name}"
image="${2:?image name}"
extra="${3:-}"
LOG="$HOME/logs"
mkdir -p "$LOG"
cd "$HOME/challoc" || { echo "no ~/challoc"; exit 1; }

t0=$(date +%s)
meta() { curl -sf -H 'Metadata-Flavor: Google' "http://metadata.google.internal/computeMetadata/v1/instance/$1" 2>/dev/null | sed 's#.*/##'; }
machine="$(meta machine-type)"; [ -n "$machine" ] || machine=unknown
zone="$(meta zone)"; [ -n "$zone" ] || zone=unknown
family="${machine%%-*}"
vcpus="$(nproc)"
os="$(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}")"
page="$(getconf PAGESIZE)"
# The CPU model: x86 has `model name` in cpuinfo; arm64 has only implementer
# and part numbers there, which lscpu decodes (Neoverse-V2, Neoverse-N1).
model="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: *//')"
[ -n "$model" ] || model="$(lscpu 2>/dev/null | grep -m1 'Model name' | sed 's/.*: *//')"
[ -n "$model" ] || model="$(awk -F: '/CPU implementer|CPU part/ { gsub(/ /, "", $2); printf "%s ", $2 }' /proc/cpuinfo | head -c 64)"
[ -n "$model" ] || model=unknown
echo "== cell $cell on $(hostname): $os"
echo "== image $image, machine $machine ($family) in $zone, $vcpus vCPU, $(uname -m) $(uname -r), page size $page"
echo "== cpu: $model"
echo "== cgroup fs: $(stat -fc %T /sys/fs/cgroup 2>/dev/null || echo unknown), docker: $(docker --version 2>/dev/null || echo none)"
echo "== tree: $(git rev-parse --short HEAD 2>/dev/null) $(git status --porcelain 2>/dev/null | wc -l | tr -d ' ') dirty"
echo "== mode: ${extra:-kernel}"
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

# The CPU capture: what the host sees (cpuinfo, lscpu) and the raw
# registers the feature report decodes (cpuid-dump.c in the container, so
# the same toolchain everywhere), the source of the fixtures in
# src/sys/cpu.rs.
cpuinfo_cmd() {
  echo "model: $model"
  echo "vcpus: $vcpus"
  echo "machine: $machine zone: $zone"
  echo "-- /proc/cpuinfo (first processor)"
  awk '/^processor/ && n++ { exit } { print }' /proc/cpuinfo
  echo "-- lscpu"
  lscpu 2>&1
  echo "-- cpuid-dump (in the container)"
  make run CMD='cc -O2 -Wall -Wextra -Werror bench/gce/cpuid-dump.c -o target/cpuid-dump && ./target/cpuid-dump'
}
step cpuinfo cpuinfo_cmd

RC_fuzz=-; RC_test_cgroup=-; RC_test_privileged=-; RC_scorecard=-
case "$extra" in
  scorecard)
    scorecard_cmd() {
      make scorecard ROUNDS=3 SCORECARD_FILE="/src/bench/results/scorecard-$family.md" \
        HOST_KIND="GCE $machine ($model), zone $zone, $vcpus vCPU"
    }
    step scorecard scorecard_cmd
    ;;
  *)
    step fuzz make fuzz
    step test-cgroup make test-cgroup
    if [ "$extra" = privileged ]; then
      priv_cmd() { DOCKER_FLAGS='--privileged' make test; }
      step test-privileged priv_cmd
    fi
    ;;
esac

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
scorecard_line="$(grep -E '^\| 20[0-9][0-9]-' "$LOG/scorecard.log" 2>/dev/null | tail -1)"

outcome=PASS
for v in "$RC_image" "$RC_test" "$RC_probe" "$RC_probe_privileged" "$RC_cpuinfo" "$RC_fuzz" "$RC_test_cgroup" "$RC_test_privileged" "$RC_scorecard"; do
  [ "$v" = FAIL ] && outcome=FAIL
done
[ "$container_page" = "$page" ] || echo "== NOTE: container page size $container_page differs from the host's $page"
t1=$(date +%s)
echo
echo "== end $(date -u +%FT%TZ), wall $((t1 - t0)) s"
[ -n "$failed_names" ] && echo "FAILED_TESTS $failed_names"
echo "CPU $model"
echo "PROBE $probe_line"
echo "PROBE_PRIVILEGED $probe_priv_line"
[ -z "$scorecard_line" ] || echo "SCORECARD $scorecard_line"
echo "MATRIX cell=$cell image=$image kernel=$(uname -r) page=$page machine=$machine zone=$zone vcpus=$vcpus tests=$tests fuzz=$RC_fuzz cgroup=$RC_test_cgroup privileged=$RC_test_privileged scorecard=$RC_scorecard wall=$((t1 - t0)) outcome=$outcome"
