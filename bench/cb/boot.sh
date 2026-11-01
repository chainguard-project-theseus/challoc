#!/bin/bash
# bench/cb/boot.sh: the boot script of a `cb launch` cell VM
# (bench/README.md, "cb launch"; bench/cb/gce.py creates the VM with this
# file as its startup-script metadata).
#
# Everything the VM needs is in its own instance metadata (gce.py,
# Launch.metadata), so the host needs bash, curl, tar, awk and sed and
# sha256sum and nothing else (a Chainguard OS host has no python3, no apt,
# no docker):
#   cb-bucket, cb-run     the bench bucket and the run id
#   cb-launch             the launch file (runs/<run>/launch.<sha>.json);
#                         this VM's entry in it is keyed by its instance
#                         name; cb-launch-sha256 its hash
#   cb-harness            the harness tree tarball (bench/cb and friends);
#                         cb-harness-sha256 its hash
#   cb-sources            the variant source tarballs, <object>=<sha256>,
#                         space-separated
# Every object fetched from the bucket is checked against its hash from
# the metadata before it is used (the bucket is writable by every cb VM;
# the instance metadata is not).
#   cb-image              the bench image for this architecture, by digest
#   cb-perf               the perf bundle (bench/cb/perf-build.sh),
#                         <object>=<sha256>: unpacked to /var/cb/perf (perf
#                         for the host itself) and mounted at /opt/perf in
#                         the container, where cb run puts it on PATH; a
#                         failed fetch only loses perf
#   cb-runtime            docker | podman | sudo-podman (root podman with
#                         --network=host: netavark needs nft, which a
#                         Chainguard OS host lacks)
#   cb-container          default | privileged | perfmon; cb-cpus and
#                         cb-memory are the container's limits, cb-cpuset
#                         its CPUs (--cpuset-cpus)
#   cb-prep               host preparation steps, space-separated:
#                           grow-var        grow /var's filesystem online
#                                           (a Chainguard OS image ships a
#                                           4.9 GiB /var on a bigger partition)
#                           thp:<f>=<v>     THP sysfs: thp:enabled=always,
#                                           thp:64kB=madvise (mTHP sizes)
#                           sysctl:<k>=<v>  one sysctl
#                           modprobe:<mod>  load a kernel module (arm_spe_pmu:
#                                           the Statistical Profiling
#                                           Extension's perf source); a
#                                           failure is logged, not fatal
#                           kernel:<pkg>    (Ubuntu) install a kernel
#                                           flavour, make it the GRUB
#                                           default and reboot into it once
#
#   1. host prep: stop the apt timers and snapd (they wake up mid-run),
#      perf_event_paranoid 1, the cb-prep steps; in parallel install Docker
#      where the runtime is docker and it is missing, and fetch the
#      harness tree and the variant sources
#   2. pull the bench image by digest; when the pull fails, build it from
#      the harness tree (Dockerfile.dev's test stage + bench/Dockerfile.cb)
#   3. `cb run --launch launch.json --vm <name>` in the container
#   4. after every block, PUT the result files to runs/<run>/res/<name>/, so
#      a preemption loses at most one block
#   5. done.json to res/<name>/ and the marker runs/<run>/done/<name>.json,
#      then idle: the launcher deletes the VM, and max-run-duration deletes
#      it if the launcher is gone. A VM never powers off: a stopped VM
#      outlives its max-run-duration (the timer stops with it).
#
# Credentials: the VM runs with the storage read/write scope only, as a
# service account that can read and write objects in the bench bucket and
# pull from the bench Artifact Registry repository. Under the docker
# runtime the benchmarked container cannot reach the metadata server (a
# DOCKER-USER reject rule), so code under test never holds that token;
# under podman (--network=host) it can. Output goes to the serial console
# (startup-script output) and /var/log/cb-boot.log.
set -u
exec > >(tee -a /var/log/cb-boot.log) 2>&1

# Wait for the launcher's delete, or max-run-duration's.
idle() { echo "cb-boot: idle until deleted"; while :; do sleep 3600; done; }

if [ -e /var/lib/cb-boot.done ]; then
  echo "cb-boot: already ran on this disk"
  idle
fi

T0=$(date +%s.%N)
BOOT_S=$(cut -d' ' -f1 /proc/uptime)
MD=http://metadata.google.internal/computeMetadata/v1
md() { curl -sf -H 'Metadata-Flavor: Google' "$MD/$1"; }
attr() { md "instance/attributes/$1" || true; }
since() { awk -v t0="$T0" -v t1="$(date +%s.%N)" 'BEGIN { printf "%.1f", t1 - t0 }'; }
NAME=$(md instance/name)
ZONE=$(md instance/zone); ZONE=${ZONE##*/}
MACHINE=$(md instance/machine-type); MACHINE=${MACHINE##*/}
BUCKET=$(attr cb-bucket)
RUN=$(attr cb-run)
LAUNCH=$(attr cb-launch)
LAUNCH_SHA=$(attr cb-launch-sha256)
HARNESS=$(attr cb-harness)
HARNESS_SHA=$(attr cb-harness-sha256)
SOURCES=$(attr cb-sources)
IMAGE=$(attr cb-image)
RUNTIME=$(attr cb-runtime); RUNTIME=${RUNTIME:-docker}
CONTAINER=$(attr cb-container); CONTAINER=${CONTAINER:-default}
CPUS=$(attr cb-cpus)
MEMORY=$(attr cb-memory)
CPUSET=$(attr cb-cpuset)
PREP=$(attr cb-prep)
PERF=$(attr cb-perf)
PROVISIONING=STANDARD; [ "$(md instance/scheduling/preemptible)" = TRUE ] && PROVISIONING=SPOT
PREFIX=runs/$RUN
RES=$PREFIX/res/$NAME
# /var: the root of a Chainguard OS host is a read-only composefs.
W=/var/cb
OUT=$W/out
mkdir -p "$W/src" "$W/sources" "$OUT/res"
TIMES="{\"boot_s\":$BOOT_S"
stamp() { TIMES="$TIMES,\"$1\":$(since)"; echo "cb-boot: $1 at +$(since) s"; }
echo "cb-boot: $NAME ($MACHINE, $ZONE, $PROVISIONING) run $RUN, uptime $BOOT_S s, $(uname -r), runtime $RUNTIME, container $CONTAINER${CPUS:+ cpus $CPUS}${CPUSET:+ cpuset $CPUSET}${MEMORY:+ memory $MEMORY}${PREP:+, prep $PREP}"

tok() { md instance/service-accounts/default/token | sed -n 's/.*"access_token" *: *"\([^"]*\)".*/\1/p'; }
get() { curl -sf --retry 3 -H "Authorization: Bearer $(tok)" -o "$2" "https://storage.googleapis.com/$BUCKET/$1"; }
# getv OBJECT FILE SHA256: get, then refuse bytes that are not the ones
# the launcher meant.
getv() {
  get "$1" "$2" || return 1
  local got
  got=$(sha256sum "$2" | cut -d' ' -f1)
  [ -n "$3" ] && [ "$got" = "$3" ] || { echo "cb-boot: $1: sha256 $got, want $3"; rm -f "$2"; return 1; }
}
put() { curl -sf --retry 3 -X PUT -H "Authorization: Bearer $(tok)" --upload-file "$1" "https://storage.googleapis.com/$BUCKET/$2" >/dev/null; }

# Docker comes from apt; a RHEL-family host (AlmaLinux, Rocky) has dnf and
# no apt, so it runs podman whatever the cell asked for.
if [ "$RUNTIME" = docker ] && ! command -v docker >/dev/null && ! command -v apt-get >/dev/null && command -v dnf >/dev/null; then
  RUNTIME=podman
fi
case $RUNTIME in
  docker) RT=docker; NET=() ;;
  podman|sudo-podman) RT=podman; NET=(--network=host) ;;
  *) RT=docker; NET=() ;;
esac

upload_res() {
  for f in samples.jsonl blocks.jsonl host.json manifest.json plan.json; do
    [ -f "$OUT/res/$f" ] && put "$OUT/res/$f" "$RES/$f"
  done
  return 0
}

finish() { # status exit-code reason
  local status=$1 code=$2 reason=$3
  echo "cb-boot: finish $status ($reason) at +$(since) s"
  [ -f /run/cb-uploader.pid ] && kill "$(cat /run/cb-uploader.pid)" 2>/dev/null
  upload_res
  [ -f "$OUT/cb.log" ] && put "$OUT/cb.log" "$RES/cb.log"
  if [ -d "$OUT/res/logs" ]; then
    tar -C "$OUT/res" -czf "$OUT/logs.tgz" logs 2>/dev/null && put "$OUT/logs.tgz" "$RES/logs.tgz"
  fi
  put /var/log/cb-boot.log "$RES/boot.log"
  # shellcheck disable=SC1010 # `done` is stamp's argument (the stage name)
  stamp done
  reason=$(printf '%s' "$reason" | tr '"\\\n' "'/ ")
  printf '{"schema":"challoc.bench.done/v1","vm":"%s","status":"%s","exit":%d,"reason":"%s","zone":"%s","machine":"%s","provisioning":"%s","kernel":"%s","times":%s}}\n' \
    "$NAME" "$status" "$code" "$reason" "$ZONE" "$MACHINE" "$PROVISIONING" "$(uname -r)" "$TIMES" > "$OUT/done.json"
  put "$OUT/done.json" "$RES/done.json"
  put "$OUT/done.json" "$PREFIX/done/$NAME.json"
  touch /var/lib/cb-boot.done
  sync
  idle
}

[ -n "$BUCKET" ] && [ -n "$RUN" ] && [ -n "$LAUNCH" ] && [ -n "$HARNESS" ] || { echo "cb-boot: no cb-bucket/cb-run/cb-launch/cb-harness metadata"; idle; }

# ---- 1. host prep, runtime, sources --------------------------------------------
systemctl stop apt-daily.timer apt-daily-upgrade.timer unattended-upgrades.service snapd.service snapd.socket 2>/dev/null
systemctl kill --kill-whom=all apt-daily.service apt-daily-upgrade.service 2>/dev/null
sysctl -qw kernel.perf_event_paranoid=1 2>/dev/null
# The negative tests and a crashing variant must not leave cores behind.
sysctl -qw 'kernel.core_pattern=|/bin/false' 2>/dev/null

(
  if [ "$RT" = docker ] && ! command -v docker >/dev/null; then
    # shellcheck disable=SC2030 # for this subshell's apt-get only
    export DEBIAN_FRONTEND=noninteractive
    apt_docker() { apt-get -o DPkg::Lock::Timeout=120 install -y -qq --no-install-recommends docker.io; }
    apt_docker >/dev/null 2>&1 \
      || { apt-get -o DPkg::Lock::Timeout=120 update -qq && apt_docker; } \
      || {
        # A Debian release past its end of life (Debian 11, the 5.10
        # tripwire) leaves deb.debian.org for archive.debian.org, and the
        # mirror drops pool files its index still lists while it moves.
        # shellcheck source=/dev/null # the host's file, sourced in this subshell only
        . /etc/os-release
        # shellcheck disable=SC2046 # one word per file: apt list names hold no spaces
        [ "$ID" = debian ] \
          && sed -i -E 's#https?://(deb|security)\.debian\.org/#http://archive.debian.org/#' \
               /etc/apt/sources.list $(ls /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources 2>/dev/null) \
          && echo "cb-boot: apt: $PRETTY_NAME from archive.debian.org" \
          && apt-get -o DPkg::Lock::Timeout=120 -o Acquire::Check-Valid-Until=false update -qq && apt_docker
      }
  fi
  if [ "$RT" = docker ]; then
    systemctl start docker && docker version --format '{{.Server.Version}}'
  else
    command -v podman >/dev/null || { command -v dnf >/dev/null && dnf -y -q install podman; }
    podman version --format '{{.Client.Version}}'
  fi
) > /var/log/cb-runtime.log 2>&1 &
RUNTIME_PID=$!

getv "$LAUNCH" "$OUT/launch.json" "$LAUNCH_SHA" || finish infra 3 "cannot fetch $LAUNCH"
{ getv "$HARNESS" "$W/harness.tar.gz" "$HARNESS_SHA" && tar -xzf "$W/harness.tar.gz" -C "$W/src"; } || finish infra 3 "cannot fetch the harness $HARNESS"
for os in $SOURCES; do
  o=${os%%=*}
  # cb run finds a source by its tree: src/<tree>.<sha16>.tar.gz -> <tree>.tar.gz
  t=${o##*/}; t=${t%%.*}
  getv "$o" "$W/sources/$t.tar.gz" "${os#*=}" || finish infra 3 "cannot fetch source $o"
done
# perf: a missing bundle costs perf, not the run (rt counts without it).
PERF_MOUNT=()
if [ -n "$PERF" ]; then
  if getv "${PERF%%=*}" "$W/perf.tar.gz" "${PERF#*=}" && mkdir -p "$W/perf" && tar -xzf "$W/perf.tar.gz" -C "$W/perf"; then
    PERF_MOUNT=(-v "$W/perf:/opt/perf:ro")
    echo "cb-boot: perf ${PERF%%=*}: host $("$W/perf/bin/perf" version 2>&1 | head -1)"
  else
    echo "cb-boot: perf ${PERF%%=*} not usable; continuing without perf"
  fi
fi
stamp fetched
wait $RUNTIME_PID || { cat /var/log/cb-runtime.log; finish infra 3 "container runtime $RUNTIME not usable"; }
echo "cb-boot: $RT $(tail -1 /var/log/cb-runtime.log)"
# The metadata server (the VM's token) is the host's, not the container's.
if [ "$RT" = docker ]; then
  iptables -I DOCKER-USER -d 169.254.169.254/32 -j REJECT \
    || finish infra 3 "cannot block the metadata server from containers (iptables DOCKER-USER)"
fi
stamp runtime

# prep `kernel:<package>` (Ubuntu): install the kernel flavour, make it the
# GRUB default by its menu entry id and reboot once; this script runs again
# at the next boot and goes on from here.
PREP_KERNEL=
for step in $PREP; do case $step in kernel:*) PREP_KERNEL=${step#kernel:} ;; esac; done
if [ -n "$PREP_KERNEL" ] && [ ! -e /var/lib/cb-prep.done ]; then
  echo "cb-boot: prep: installing $PREP_KERNEL (running $(uname -r))"
  # shellcheck disable=SC2031 # the runtime subshell above exports its own copy
  export DEBIAN_FRONTEND=noninteractive
  { apt-get -o DPkg::Lock::Timeout=120 update -qq && apt-get -o DPkg::Lock::Timeout=120 install -y -qq "$PREP_KERNEL"; } \
    || finish infra 3 "prep: apt-get install $PREP_KERNEL failed"
  FLAVOUR=${PREP_KERNEL#linux-}
  # shellcheck disable=SC2012 # kernel file names are plain; sort -V orders the versions
  KV=$(ls /boot/vmlinuz-*-"$FLAVOUR" 2>/dev/null | sed 's|/boot/vmlinuz-||' | sort -V | tail -1)
  [ -n "$KV" ] || finish infra 3 "prep: no /boot/vmlinuz-*-$FLAVOUR after installing $PREP_KERNEL"
  # Ubuntu cloud images boot initramfs-less first (GRUB_FORCE_PARTUUID);
  # a generic kernel has its disk driver (NVMe on C4A) as a module, so it
  # needs its initramfs.
  [ -e "/boot/initrd.img-$KV" ] || update-initramfs -c -k "$KV" >/dev/null 2>&1
  update-grub >/dev/null 2>&1
  SUB=$(grep -oP "submenu '[^']*'.*?\\\$menuentry_id_option '\K[^']+" /boot/grub/grub.cfg | head -1)
  ENT=$(grep -oP "menuentry '[^']*with Linux $KV'.*?\\\$menuentry_id_option '\K[^']+" /boot/grub/grub.cfg | head -1)
  [ -n "$ENT" ] || finish infra 3 "prep: no GRUB entry for $KV"
  printf 'GRUB_DEFAULT="%s"\nGRUB_FORCE_PARTUUID=\n' "${SUB:+$SUB>}$ENT" > /etc/default/grub.d/99-cb-kernel.cfg
  update-grub >/dev/null 2>&1 || finish infra 3 "prep: update-grub failed"
  echo "$KV" > /var/lib/cb-prep.done
  echo "cb-boot: prep: rebooting into $KV (GRUB_DEFAULT ${SUB:+$SUB>}$ENT)"
  put /var/log/cb-boot.log "$RES/boot-prep.log"
  sync
  reboot
  exit 0
fi
[ -e /var/lib/cb-prep.done ] && echo "cb-boot: prep: wanted $(cat /var/lib/cb-prep.done), running $(uname -r)"

thp_write() { # file value
  local f=/sys/kernel/mm/transparent_hugepage/$1
  case $1 in *kB) f=/sys/kernel/mm/transparent_hugepage/hugepages-$1/enabled ;; esac
  echo "$2" > "$f" && echo "cb-boot: prep $f = $(cat "$f")"
}
for step in $PREP; do
  case $step in
    grow-var)
      dev=$(findmnt -no SOURCE /var | sed 's/\[.*//')
      avail=$(df -BG --output=avail /var | tail -1 | tr -dc 0-9)
      if [ "${avail:-0}" -lt 20 ]; then
        echo "cb-boot: prep grow-var: $dev has ${avail} GiB free on /var"
        # resize2fs from a Wolfi container that sees only the /var mount
        # (-v /var), so it grows the mounted filesystem online. Not the
        # host's resize2fs (the 2026-10 production images ship one): it
        # finds the read-only /sysroot mount of the same device first and
        # fails with "Read-only file system While checking for on-line
        # resizing support".
        $RT run --rm --privileged "${NET[@]}" -v /dev:/dev -v /var:/mnt/var cgr.dev/chainguard/wolfi-base \
          sh -c "apk add --no-cache e2fsprogs-extra >/dev/null 2>&1; resize2fs $dev" 2>&1 | tail -1
        echo "cb-boot: prep grow-var: $(df -h --output=size,avail /var | tail -1)"
        avail=$(df -BG --output=avail /var | tail -1 | tr -dc 0-9)
        [ "${avail:-0}" -ge 20 ] || finish infra 3 "prep grow-var failed: ${avail} GiB free on /var"
      fi ;;
    thp:*=*) kv=${step#thp:}; thp_write "${kv%%=*}" "${kv#*=}" || finish infra 3 "prep $step failed" ;;
    kernel:*) ;;
    sysctl:*=*) kv=${step#sysctl:}; sysctl -w "${kv%%=*}=${kv#*=}" || finish infra 3 "prep $step failed" ;;
    # Not fatal: whether the hardware has the unit is what the run
    # records (host.json pmu_devices).
    modprobe:*)
      if modprobe "${step#modprobe:}" 2>&1; then
        echo "cb-boot: prep modprobe ${step#modprobe:} ok"
      else
        echo "cb-boot: prep modprobe ${step#modprobe:} failed: $(dmesg | grep -i "${step#modprobe:}" | tail -2 | tr '\n' ' ')"
      fi ;;
    *) finish infra 3 "unknown prep step $step" ;;
  esac
done
stamp prep

# ---- 2. the bench image ----------------------------------------------------------
IMG_ID=
if [ -n "$IMAGE" ]; then
  tok | $RT login -u oauth2accesstoken --password-stdin "https://${IMAGE%%/*}" >/dev/null 2>&1
  if $RT pull -q "$IMAGE" >/dev/null; then
    IMG_ID=$IMAGE
  else
    echo "cb-boot: pull of $IMAGE failed; building the image here"
  fi
fi
if [ -z "$IMG_ID" ]; then
  ( cd "$W/src" && $RT build "${NET[@]}" -q -f Dockerfile.dev --target test -t cb-dev-test:local . \
      && $RT build "${NET[@]}" -q -f bench/Dockerfile.cb --build-arg DEV_IMAGE=cb-dev-test:local -t cb-bench:local . ) \
    > /var/log/cb-image-build.log 2>&1 || { tail -40 /var/log/cb-image-build.log; finish infra 3 "image build failed"; }
  IMG_ID=cb-bench:local
  # Image ids of local builds differ VM to VM; the harness tree they were
  # built from does not, and it is what the comparability key needs.
  HT=${HARNESS##*/}; HT=${HT%.tar.gz}
  IMAGE="built@harness:${HT:0:12}"
fi
stamp image

# ---- 3. run the shard, 4. upload per block ----------------------------------------
(
  last=
  while sleep 10; do
    s=$(stat -c %s "$OUT/res/blocks.jsonl" 2>/dev/null) || continue
    if [ "$s" != "$last" ]; then upload_res; last=$s; fi
  done
) &
echo $! > /run/cb-uploader.pid

case "$CONTAINER" in
  # debugfs for the knobs (sched.llc_balancing, rseq.slice_ext_nsec).
  privileged) PROFILE=(--privileged -v /sys/kernel/debug:/sys/kernel/debug) ;;
  perfmon) PROFILE=(--cap-add PERFMON) ;;
  *) PROFILE=() ;;
esac
# SELinux (the RHEL family) would deny the bind mounts their labels.
[ "$RT" = podman ] && PROFILE+=(--security-opt label=disable)
[ -n "$CPUS" ] && PROFILE+=(--cpus "$CPUS")
[ -n "$CPUSET" ] && PROFILE+=(--cpuset-cpus "$CPUSET")
[ -n "$MEMORY" ] && PROFILE+=(--memory "$MEMORY" --memory-swap "$MEMORY")
DIGEST=${IMAGE##*@}
$RT run --rm --name cb "${NET[@]}" "${PROFILE[@]}" "${PERF_MOUNT[@]}" \
  -v "$W/src:/src" -v "$OUT:/out" -v "$W/sources:/cb/sources:ro" \
  -e CB_HOST="$NAME" -e CB_MACHINE="$MACHINE" -e CB_ZONE="$ZONE" \
  -e CB_SPOT="$([ "$PROVISIONING" = SPOT ] && echo 1 || echo 0)" -e CB_IMAGE="${DIGEST:0:19}" \
  -e CB_RUNTIME="$RUNTIME" -e CB_CONTAINER="$CONTAINER" \
  -w /src "$IMG_ID" \
  python3 bench/cb run --launch /out/launch.json --vm "$NAME" --sources /cb/sources --out /out/res --bin /opt/cb/bench-bin \
  > "$OUT/cb.log" 2>&1
rc=$?
stamp ran
tail -25 "$OUT/cb.log"
if [ $rc -eq 0 ]; then
  finish ok 0 "shard complete"
elif [ $rc -eq 3 ]; then
  finish infra 3 "cb run: $(grep '^cb: ' "$OUT/cb.log" | tail -1)"
else
  finish failed "$rc" "cb run exited $rc"
fi
