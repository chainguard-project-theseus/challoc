#!/usr/bin/env bash
# devvm.sh: build and test challoc on remote Linux dev VMs on GCE.
#
#   scripts/devvm.sh up     [arm64|amd64|all]    create or start the VMs,
#                                                provision Docker, build the
#                                                dev image (idempotent)
#   scripts/devvm.sh run    ARCH NAME [--src DIR] [--base REF] -- CMD...
#                                                sync DIR to ~/w/NAME on the
#                                                VM and run CMD there
#   scripts/devvm.sh attach ARCH NAME            reattach to a run's output,
#                                                exit with its status
#   scripts/devvm.sh kill   ARCH NAME            stop a run
#   scripts/devvm.sh fetch  ARCH NAME REMOTE LOCAL
#                                                copy REMOTE back (relative
#                                                to ~/w/NAME; vol:PATH is
#                                                PATH in the run's target
#                                                volume)
#   scripts/devvm.sh ssh    ARCH [CMD...]        a shell (or CMD) on the VM
#   scripts/devvm.sh down   [arm64|amd64|all]    stop the VMs
#   scripts/devvm.sh status                      VMs, IPs and runs
#
# The VMs are challoc-dev-arm64 (c4a-standard-32) and challoc-dev-amd64
# (c4-standard-32), on-demand (not spot), Ubuntu 24.04 with Docker. The
# Makefile's Docker targets run on them unchanged, natively on both
# architectures (this replaces PLATFORM=linux/amd64 emulation).
#
# run: the source tree's HEAD is pushed to a bare mirror ~/challoc.git on
# the VM (ref refs/devvm/NAME), ~/w/NAME is a git worktree of that mirror
# reset to the commit, and the working tree (uncommitted and untracked
# files included) is rsynced over it, so `git diff HEAD` on the VM shows
# the local changes and git-based scripts (scripts/codegen-diff.sh,
# bench/quick.sh) work. Not shipped: .git and every path git ignores
# (target/, `.tmp-*` logs, .claude/ worktrees) except `.tmp-*.sh` scripts;
# files the VM has under those paths survive. --base REF also pushes REF
# (refs/devvm/NAME-base), checks it out at ~/w/.base/NAME and exports
# DEVVM_BASE (its commit) and
# DEVVM_BASE_DIR to CMD. CMD runs in ~/w/NAME with
# TARGET_VOLUME=challoc-target-NAME-ARCH exported, so runs with different
# NAMEs on one VM never share a tree, a target volume or a log; one NAME
# runs one command at a time. One CMD argument is a shell script line
# (`-- 'make a && make b'`); several are one command, quoted word by word
# (`-- make run CMD='sh scripts/codegen-diff.sh capture'`).
#
# A run is detached on the VM (setsid nohup) with its output in
# ~/runs/NAME/log; `run` streams that log and exits with CMD's status. If
# ssh drops or you press ^C the job keeps running: `attach ARCH NAME`
# streams it again from the start and returns the status once it ends.
#
# Environment: DEVVM_PROJECT (default dlorenc-dev), DEVVM_ZONE (us-east4-a),
# DEVVM_PREFIX (challoc-dev), DEVVM_SSH_KEY (~/.ssh/google_compute_engine).
# ssh and rsync go directly to the VM's external IP with gcloud's key (not
# through `gcloud compute ssh`, which hangs on detached jobs); host keys
# are pinned per VM name in ~/.cache/challoc-devvm, so recycled IPs never
# trip a "host key changed", and the last known IP is used when gcloud's
# credentials have expired.
set -euo pipefail

PROJECT="${DEVVM_PROJECT:-dlorenc-dev}"
ZONE="${DEVVM_ZONE:-us-east4-a}"
PREFIX="${DEVVM_PREFIX:-challoc-dev}"
SSH_KEY="${DEVVM_SSH_KEY:-$HOME/.ssh/google_compute_engine}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/challoc-devvm"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$CACHE"

die() { echo "devvm: $*" >&2; exit 2; }
note() { echo "devvm: $*" >&2; }
g() { gcloud --project="$PROJECT" --quiet "$@"; }

usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; }

vm_name()    { echo "$PREFIX-$1"; }
vm_machine() { case "$1" in arm64) echo c4a-standard-32 ;; amd64) echo c4-standard-32 ;; *) die "unknown arch $1" ;; esac; }
vm_image()   { echo "ubuntu-2404-lts-$1"; }
arches()     { case "${1:-all}" in all) echo "arm64 amd64" ;; arm64|amd64) echo "$1" ;; *) die "arch must be arm64, amd64 or all" ;; esac; }
check_arch() { case "${1:-}" in arm64|amd64) ;; *) die "arch must be arm64 or amd64" ;; esac; }
check_name() { [[ "${1:-}" =~ ^[A-Za-z0-9_][A-Za-z0-9._-]*$ ]] || die "bad run name '${1:-}' (letters, digits, . _ -)"; }

# vm_state ARCH: RUNNING, TERMINATED, ... or MISSING
vm_state() {
  g compute instances describe "$(vm_name "$1")" --zone="$ZONE" --format='value(status)' 2>/dev/null || echo MISSING
}

# dest ARCH: user@ip, from gcloud, cached for when gcloud's token expired.
dest() {
  local a="$1" name ip user f
  name="$(vm_name "$a")"; f="$CACHE/$name.dest"
  ip="$(g compute instances describe "$name" --zone="$ZONE" \
        --format='value(networkInterfaces[0].accessConfigs[0].natIP)' 2>/dev/null || true)"
  if [ -n "$ip" ]; then
    user="$(cat "$CACHE/user" 2>/dev/null || true)"
    if [ -z "$user" ]; then
      user="$(g compute os-login describe-profile --format='value(posixAccounts[0].username)')"
      [ -n "$user" ] || die "cannot read the OS Login username"
      echo "$user" > "$CACHE/user.$$" && mv "$CACHE/user.$$" "$CACHE/user"
    fi
    # Concurrent runs share the cache: write it atomically.
    echo "$user@$ip" > "$f.$$" && mv "$f.$$" "$f"
    echo "$user@$ip"
  elif [ -s "$f" ]; then
    note "gcloud gave no IP for $name; using the cached $(cat "$f")"
    cat "$f"
  else
    die "$name has no external IP (is it running? devvm.sh up $a)"
  fi
}

# ssh_opts ARCH: one array of options, the host key pinned to the VM name.
ssh_opts() {
  [ -f "$SSH_KEY" ] || die "no ssh key at $SSH_KEY; run 'gcloud compute ssh' to any VM once to mint it"
  local name; name="$(vm_name "$1")"
  SSH_OPTS=(-i "$SSH_KEY" -o IdentitiesOnly=yes -o BatchMode=yes
    -o HostKeyAlias="$name" -o UserKnownHostsFile="$CACHE/known_hosts"
    -o StrictHostKeyChecking=accept-new -o ConnectTimeout=15
    -o ServerAliveInterval=30 -o ServerAliveCountMax=4
    -o ControlMaster=auto -o ControlPath="$CACHE/cm-%C" -o ControlPersist=120)
}

# vssh ARCH ARGS...: ssh to the VM (DEST set by the caller or looked up).
vssh() {
  local a="$1"; shift
  ssh_opts "$a"
  # shellcheck disable=SC2029 # the arguments are the remote command line
  ssh "${SSH_OPTS[@]}" "${DEST:-$(dest "$a")}" "$@"
}

ssh_flat() { ssh_opts "$1"; printf '%q ' ssh "${SSH_OPTS[@]}"; }

# ------------------------------------------------------------------ up

provision_script() {
  cat <<'REMOTE'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
need=()
for p in docker.io docker-buildx rsync git make shellcheck; do
  dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'ok installed' || need+=("$p")
done
if [ "${#need[@]}" -gt 0 ]; then
  sudo apt-get update -qq
  sudo -E apt-get install -y -qq "${need[@]}" >/dev/null
fi
# GCE NICs carry a 1460-byte MTU; Docker's bridge defaults to 1500, so TLS
# fetches from inside a container (apk, cargo) stall on fragmented packets.
if ! grep -q '"mtu": 1460' /etc/docker/daemon.json 2>/dev/null; then
  echo '{ "mtu": 1460 }' | sudo tee /etc/docker/daemon.json >/dev/null
  sudo systemctl restart docker
fi
sudo systemctl enable --now docker >/dev/null 2>&1
id -nG | grep -qw docker || sudo usermod -aG docker "$USER"
[ -d ~/challoc.git ] || git init -q --bare ~/challoc.git
mkdir -p ~/w ~/runs
echo "$(hostname): $(uname -m) $(uname -r), $(nproc) cpus, $(sudo docker --version)"
REMOTE
}

wait_ssh() { # wait_ssh ARCH: until a command runs over direct ssh
  local a="$1" i
  for i in $(seq 1 40); do
    if DEST="$(dest "$a")" vssh "$a" true 2>/dev/null; then return 0; fi
    # OS Login may not know the key yet: gcloud registers it on first use.
    if [ "$i" = 4 ]; then g compute os-login ssh-keys add --key-file="$SSH_KEY.pub" >/dev/null 2>&1 || true; fi
    sleep 5
  done
  die "no ssh to $(vm_name "$a") after 200 s"
}

up_one() {
  local a="$1" name state
  name="$(vm_name "$a")"; state="$(vm_state "$a")"
  case "$state" in
    MISSING)
      note "creating $name ($(vm_machine "$a")) in $ZONE"
      ssh-keygen -R "$name" -f "$CACHE/known_hosts" >/dev/null 2>&1 || true
      g compute instances create "$name" --zone="$ZONE" \
        --machine-type="$(vm_machine "$a")" \
        --image-family="$(vm_image "$a")" --image-project=ubuntu-os-cloud \
        --boot-disk-size=200GB --boot-disk-type=hyperdisk-balanced \
        --provisioning-model=STANDARD \
        --metadata=enable-oslogin=TRUE \
        --labels="purpose=challoc-dev,arch=$a,owner=${USER}" >/dev/null ;;
    RUNNING) ;;
    TERMINATED|STOPPED|SUSPENDED)
      note "starting $name"
      if [ "$state" = SUSPENDED ]; then g compute instances resume "$name" --zone="$ZONE" >/dev/null
      else g compute instances start "$name" --zone="$ZONE" >/dev/null; fi ;;
    STOPPING|SUSPENDING)
      note "$name is $state; waiting"
      while [ "$(vm_state "$a")" = "$state" ]; do sleep 5; done
      up_one "$a"; return ;;
    *) note "$name is $state; waiting for it" ;;
  esac
  wait_ssh "$a"
  local d; d="$(dest "$a")"
  provision_script | DEST="$d" vssh "$a" bash -s
  # The control master logged in before the docker group was added.
  ssh_opts "$a"; ssh "${SSH_OPTS[@]}" -O exit "$d" 2>/dev/null || true
  note "$name: building the dev image (make image)"
  cmd_run "$a" _image --src "$REPO" -- make image
}

cmd_up() {
  local a pids=() names=() i failed=
  for a in $(arches "${1:-all}"); do
    ( up_one "$a" ) 2>&1 | sed -u "s/^/[$a] /" &
    pids+=("$!"); names+=("$a")
  done
  for i in "${!pids[@]}"; do wait "${pids[$i]}" || failed="$failed ${names[$i]}"; done
  # sed hides the subshell's status; check the image on each VM instead.
  for a in "${names[@]}"; do
    # shellcheck disable=SC2016 # expands on the VM
    vssh "$a" 'docker image inspect "challoc-dev:$(uname -m | sed -e s/x86_64/amd64/ -e s/aarch64/arm64/)" >/dev/null 2>&1' \
      || failed="$failed $a"
  done
  [ -z "$failed" ] || die "up FAILED for:$failed"
  cmd_status
}

# ------------------------------------------------------------------ run

# The remote half of `run`, `attach` and `kill`: bash -s -- ACTION NAME ...
remote_script() {
  cat <<'REMOTE'
set -euo pipefail
action="$1"; name="$2"; shift 2
run=~/runs/$name; w=~/w/$name
alive() { [ -s "$run/pid" ] && kill -0 "$(cat "$run/pid")" 2>/dev/null; }
stream() { # the log until the job ends, then its status
  [ -f "$run/log" ] || { echo "devvm: no run '$name' on $(hostname)" >&2; exit 2; }
  local pid; pid="$(cat "$run/pid" 2>/dev/null || echo 0)"
  if alive; then tail -n +1 -F --pid="$pid" "$run/log" 2>/dev/null; else cat "$run/log"; fi
  for _ in 1 2 3 4 5; do [ -f "$run/exit" ] && break; sleep 1; done
  [ -f "$run/exit" ] || { echo "devvm: '$name' ended without a status (killed?)" >&2; exit 255; }
  exit "$(cat "$run/exit")"
}
case "$action" in
  prep) # prep NAME: refuse a live run; own the tree again
    if alive; then echo "devvm: '$name' is still running on $(hostname); attach or kill it" >&2; exit 3; fi
    [ -d ~/challoc.git ] || git init -q --bare ~/challoc.git
    mkdir -p ~/w ~/w/.base "$run"
    # Containers run as root and may leave root-owned files in the tree.
    for d in "$w" ~/w/.base/"$name"; do
      [ -d "$d" ] && sudo chown -R "$(id -u):$(id -g)" "$d"
    done
    true ;;
  checkout) # checkout NAME SHA [BASESHA]
    sha="$1"; base="${2:-}"
    git -C ~/challoc.git worktree prune
    co() { # co DIR SHA
      if [ -e "$1/.git" ] && git -C "$1" rev-parse --git-dir >/dev/null 2>&1; then
        git -C "$1" reset -q --hard "$2"
      else
        rm -rf "$1"
        git -C ~/challoc.git worktree add -q --detach "$1" "$2"
      fi
    }
    co "$w" "$sha"
    if [ -n "$base" ]; then co ~/w/.base/"$name" "$base"; fi ;;
  start) # start NAME B64SCRIPT
    alive && { echo "devvm: '$name' is already running" >&2; exit 3; }
    rm -f "$run/exit" "$run/pid"
    echo "$1" | base64 -d > "$run/job.sh"
    date -u +%FT%TZ > "$run/started"
    # setsid: own session, so neither an ssh drop nor ^C reaches the job.
    (setsid nohup bash "$run/job.sh" > "$run/log" 2>&1 < /dev/null &)
    for _ in $(seq 1 50); do [ -s "$run/pid" ] && break; sleep 0.1; done
    stream ;;
  attach) stream ;;
  kill)
    if alive; then
      pid="$(cat "$run/pid")"
      kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid"
      echo "devvm: sent TERM to '$name' (process group $pid)"
    else echo "devvm: '$name' is not running"; fi ;;
  list)
    for r in ~/runs/*/; do
      [ -d "$r" ] || continue
      n="$(basename "$r")"; run="$r"
      if alive; then s=running; elif [ -f "$r/exit" ]; then s="exit $(cat "$r/exit")"; else s=-; fi
      printf '  %-20s %-10s %s\n' "$n" "$s" "$(cat "$r/started" 2>/dev/null)"
    done ;;
esac
REMOTE
}

remote() { # remote ARCH ACTION NAME ARGS...: the remote script over ssh
  local a="$1"; shift
  remote_script | vssh "$a" bash -s -- "$@"
}

cmd_run() {
  local a="${1:-}" name="${2:-}" src="$PWD" base=""
  check_arch "$a"; check_name "$name"; shift 2
  while [ $# -gt 0 ]; do
    case "$1" in
      --src) src="$2"; shift 2 ;;
      --base) base="$2"; shift 2 ;;
      --) shift; break ;;
      *) die "run: unexpected '$1' (the command goes after --)" ;;
    esac
  done
  [ $# -gt 0 ] || die "run: no command (devvm.sh run ARCH NAME -- CMD...)"
  src="$(git -C "$src" rev-parse --show-toplevel 2>/dev/null)" || die "run: --src must be in a git checkout"
  local sha basesha="" cmdline
  sha="$(git -C "$src" rev-parse HEAD)"
  if [ -n "$base" ]; then
    basesha="$(git -C "$src" rev-parse --verify "$base^{commit}")" || die "run: --base $base is not a commit"
  fi
  if [ $# -eq 1 ]; then cmdline="$1"; else cmdline="$(printf '%q ' "$@")"; fi

  DEST="$(dest "$a")"; export DEST
  local t0=$SECONDS
  remote "$a" prep "$name"
  # The commit (and base) into the VM's mirror.
  local refs=("+$sha:refs/devvm/$name")
  [ -z "$basesha" ] || refs+=("+$basesha:refs/devvm/$name-base")
  GIT_SSH_COMMAND="$(ssh_flat "$a")" git -C "$src" push -q --no-verify \
    "$DEST:challoc.git" "${refs[@]}"
  remote "$a" checkout "$name" "$sha" "$basesha"
  # The working tree over it: uncommitted and untracked changes too.
  # Everything git ignores stays behind (target/, logs, other agents'
  # worktrees under .claude/) except `.tmp-*.sh` scripts for `make run`.
  # Exit 24: a file vanished while rsync ran (a commit in the source tree).
  local src_rc=0 excl
  excl="$(mktemp "$CACHE/exclude.XXXXXX")"
  { echo /.git; echo /target/
    git -C "$src" ls-files -z --others --ignored --exclude-standard --directory \
      | tr '\0' '\n' | { grep -v '^\.tmp-[^/]*\.sh$' || true; } | sed 's|^|/|'
  } > "$excl"
  rsync -az --checksum --delete --exclude-from="$excl" \
    -e "$(ssh_flat "$a")" "$src/" "$DEST:w/$name/" || src_rc=$?
  rm -f "$excl"
  case "$src_rc" in 0|24) ;; *) die "rsync of $src to $(vm_name "$a") failed ($src_rc)" ;; esac
  note "$name: synced $(git -C "$src" rev-parse --short HEAD)$( [ -z "$(git -C "$src" status --porcelain --untracked-files=no)" ] || echo ' + local changes') to $(vm_name "$a"):~/w/$name in $((SECONDS - t0)) s"

  local job
  job="$(cat <<EOF
echo \$\$ > ~/runs/$name/pid
cd ~/w/$name || exit 1
export TARGET_VOLUME=challoc-target-$name-$a DEVVM_NAME=$name DEVVM_ARCH=$a
${basesha:+export DEVVM_BASE=$basesha DEVVM_BASE_DIR=\$HOME/w/.base/$name}
s=\$SECONDS
bash -c $(printf '%q' "$cmdline")
rc=\$?
echo "devvm: '$name' exited \$rc after \$((SECONDS - s)) s" >&2
echo \$rc > ~/runs/$name/exit.tmp && mv ~/runs/$name/exit.tmp ~/runs/$name/exit
EOF
)"
  local rc=0
  trap 'note "interrupted; the job keeps running on the VM: devvm.sh attach $a $name (or kill)"; exit 130' INT
  remote "$a" start "$name" "$(printf '%s\n' "$job" | base64 | tr -d '\n')" || rc=$?
  trap - INT
  if [ "$rc" = 255 ]; then
    note "ssh to $(vm_name "$a") dropped or the job died; if it still runs: devvm.sh attach $a $name"
  fi
  return "$rc"
}

cmd_attach() { check_arch "${1:-}"; check_name "${2:-}"; remote "$1" attach "$2"; }
cmd_kill()   { check_arch "${1:-}"; check_name "${2:-}"; remote "$1" kill "$2"; }

cmd_fetch() {
  local a="${1:-}" name="${2:-}" rpath="${3:-}" lpath="${4:-}"
  check_arch "$a"; check_name "$name"
  if [ -z "$rpath" ] || [ -z "$lpath" ]; then die "fetch ARCH NAME REMOTE LOCAL"; fi
  case "$rpath" in
    vol:*) rpath="/var/lib/docker/volumes/challoc-target-$name-$a/_data/${rpath#vol:}" ;;
    /*|~*) ;;
    *) rpath="w/$name/$rpath" ;;
  esac
  DEST="$(dest "$a")"
  # sudo: the target volume and files containers wrote are root's.
  rsync -az --rsync-path="sudo rsync" -e "$(ssh_flat "$a")" "$DEST:$rpath" "$lpath"
}

cmd_ssh() {
  local a="${1:-}"; check_arch "$a"; shift
  if [ $# -eq 0 ]; then vssh "$a" -t; else vssh "$a" "$@"; fi
}

cmd_down() {
  local a names=()
  for a in $(arches "${1:-all}"); do names+=("$(vm_name "$a")"); done
  note "stopping ${names[*]}"
  g compute instances stop "${names[@]}" --zone="$ZONE" >/dev/null
  cmd_status
}

cmd_status() {
  g compute instances list --filter="labels.purpose=challoc-dev" \
    --format='table(name,zone.basename(),machineType.basename(),status,networkInterfaces[0].accessConfigs[0].natIP:label=IP)'
  local a
  for a in arm64 amd64; do
    [ "$(vm_state "$a")" = RUNNING ] || continue
    echo "$(vm_name "$a") runs:"
    remote "$a" list _ 2>/dev/null || echo "  (ssh failed)"
  done
}

case "${1:-}" in
  up|run|attach|kill|fetch|ssh|down|status) c="$1"; shift; "cmd_$c" "$@" ;;
  -h|--help|help|"") usage ;;
  *) usage >&2; exit 2 ;;
esac
