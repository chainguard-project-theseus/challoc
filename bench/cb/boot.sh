#!/bin/bash
# bench/cb/boot.sh: the startup script of a `cb launch` cell VM
# (bench/README.md, "cb launch"; bench/cb/gce.py creates the VM with this
# file as its startup-script metadata).
#
# The VM finds its work from its own metadata: cb-bucket and cb-run name
# gs://<bucket>/runs/<run>/launch.json, and its instance name is its key
# in that file's `vms` table (cell, shard, replicate, rows, seed). Then:
#
#   1. host prep: stop the apt timers and snapd (they wake up mid-run),
#      perf_event_paranoid 1; in parallel install Docker (docker.io) and
#      fetch the harness tree and the variant source tarballs
#   2. pull the bench image by digest (bench/image.lock, recorded in the
#      launch file); when the pull fails, build it from the harness tree
#      (Dockerfile.dev's test stage + bench/Dockerfile.cb), which is slow
#   3. `cb run --launch launch.json --vm <name>` in the container
#   4. after every block, PUT the result files to runs/<run>/res/<name>/, so
#      a preemption loses at most one block
#   5. done.json (status, exit code, timings) to res/<name>/ and the marker
#      runs/<run>/done/<name>.json, then power off (the launcher deletes
#      the VM; max-run-duration deletes it if the launcher is gone)
#
# Credentials: the VM's service account can read and write objects in the
# bench bucket and pull from the bench Artifact Registry repository, and
# nothing else. Output goes to the serial console (the guest agent copies
# startup-script output there) and /var/log/cb-boot.log.
set -u
exec > >(tee -a /var/log/cb-boot.log) 2>&1

if [ -e /var/lib/cb-boot.done ]; then
  echo "cb-boot: already ran on this disk; powering off"
  poweroff
  exit 0
fi

T0=$(date +%s.%N)
BOOT_S=$(cut -d' ' -f1 /proc/uptime)
MD=http://metadata.google.internal/computeMetadata/v1
md() { curl -sf -H 'Metadata-Flavor: Google' "$MD/$1"; }
since() { python3 -c "import sys,time; print(round(time.time()-float(sys.argv[1]),1))" "$T0"; }
NAME=$(md instance/name)
ZONE=$(md instance/zone); ZONE=${ZONE##*/}
MACHINE=$(md instance/machine-type); MACHINE=${MACHINE##*/}
BUCKET=$(md instance/attributes/cb-bucket)
RUN=$(md instance/attributes/cb-run)
PROVISIONING=STANDARD; [ "$(md instance/scheduling/preemptible)" = TRUE ] && PROVISIONING=SPOT
PREFIX=runs/$RUN
RES=$PREFIX/res/$NAME
W=/cb
OUT=$W/out
mkdir -p "$W/src" "$W/sources" "$OUT/res"
TIMES="{\"boot_s\":$BOOT_S"
stamp() { TIMES="$TIMES,\"$1\":$(since)"; echo "cb-boot: $1 at +$(since) s"; }
echo "cb-boot: $NAME ($MACHINE, $ZONE, $PROVISIONING) run $RUN, uptime $BOOT_S s"

tok() { md instance/service-accounts/default/token | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])'; }
get() { curl -sf --retry 3 -H "Authorization: Bearer $(tok)" -o "$2" "https://storage.googleapis.com/$BUCKET/$1"; }
put() { curl -sf --retry 3 -X PUT -H "Authorization: Bearer $(tok)" --upload-file "$1" "https://storage.googleapis.com/$BUCKET/$2" >/dev/null; }

upload_res() {
  for f in samples.jsonl blocks.jsonl host.json manifest.json plan.json; do
    [ -f "$OUT/res/$f" ] && put "$OUT/res/$f" "$RES/$f"
  done
  return 0
}

finish() { # status exit-code reason
  local status=$1 code=$2 reason=$3
  echo "cb-boot: finish $status ($reason) at +$(since) s"
  [ -f /tmp/cb-uploader.pid ] && kill "$(cat /tmp/cb-uploader.pid)" 2>/dev/null
  upload_res
  [ -f "$OUT/cb.log" ] && put "$OUT/cb.log" "$RES/cb.log"
  if [ -d "$OUT/res/logs" ]; then
    tar -C "$OUT/res" -czf "$OUT/logs.tgz" logs 2>/dev/null && put "$OUT/logs.tgz" "$RES/logs.tgz"
  fi
  put /var/log/cb-boot.log "$RES/boot.log"
  stamp done
  python3 - "$OUT/done.json" "$status" "$code" "$reason" "$NAME" "$ZONE" "$MACHINE" "$PROVISIONING" "$TIMES}" <<'EOF'
import json, sys
path, status, code, reason, name, zone, machine, prov, times = sys.argv[1:]
json.dump({"schema": "challoc.bench.done/v1", "vm": name, "status": status, "exit": int(code), "reason": reason,
           "zone": zone, "machine": machine, "provisioning": prov, "times": json.loads(times)}, open(path, "w"))
EOF
  put "$OUT/done.json" "$RES/done.json"
  put "$OUT/done.json" "$PREFIX/done/$NAME.json"
  touch /var/lib/cb-boot.done
  sync
  poweroff
  exit 0
}

# ---- 1. host prep, Docker, sources ------------------------------------------
systemctl stop apt-daily.timer apt-daily-upgrade.timer unattended-upgrades.service snapd.service snapd.socket 2>/dev/null
systemctl kill --kill-whom=all apt-daily.service apt-daily-upgrade.service 2>/dev/null
sysctl -qw kernel.perf_event_paranoid=1 2>/dev/null

# The container runtime: Docker from apt on Debian and Ubuntu; podman from
# dnf on the RHEL family (AlmaLinux, Rocky), driven through the same
# `docker` command line.
RUNTIME=docker
if ! command -v apt-get >/dev/null && command -v dnf >/dev/null; then RUNTIME=podman; fi
(
  export DEBIAN_FRONTEND=noninteractive
  if [ "$RUNTIME" = podman ]; then
    command -v podman >/dev/null || dnf -y -q install podman
    podman version --format '{{.Client.Version}}'
  else
    if ! command -v docker >/dev/null; then
      apt-get -o DPkg::Lock::Timeout=120 install -y -qq --no-install-recommends docker.io >/dev/null 2>&1 \
        || { apt-get -o DPkg::Lock::Timeout=120 update -qq && apt-get -o DPkg::Lock::Timeout=120 install -y -qq --no-install-recommends docker.io; }
    fi
    systemctl start docker && docker version --format '{{.Server.Version}}'
  fi
) > /var/log/cb-docker.log 2>&1 &
DOCKER_PID=$!
docker() { if [ "$RUNTIME" = podman ]; then podman "$@"; else command docker "$@"; fi; }

get "$PREFIX/launch.json" "$OUT/launch.json" || finish infra 3 "cannot fetch $PREFIX/launch.json"
eval "$(python3 - "$OUT/launch.json" "$NAME" "$(uname -m)" <<'EOF'
import json, shlex, sys
l = json.load(open(sys.argv[1])); name = sys.argv[2]
arch = {"x86_64": "amd64", "aarch64": "arm64"}.get(sys.argv[3], sys.argv[3])
vm = l["vms"].get(name)
if vm is None:
    print("VM_OK=0"); sys.exit()
cell = l["cells"][vm["cell"]]
print("VM_OK=1")
print("ARCH=" + shlex.quote(arch))
print("HARNESS=" + shlex.quote(l["harness"]["object"]))
print("SOURCES=" + shlex.quote(" ".join(s["object"] for s in l["sources"].values())))
print("IMAGE=" + shlex.quote(l.get("image", {}).get(arch, "")))
print("CONTAINER=" + shlex.quote(cell.get("container", "default")))
kern = [p.split(":", 1)[1] for p in cell.get("prep") or [] if p.startswith("kernel:")]
print("PREP_KERNEL=" + shlex.quote(kern[0] if kern else ""))
EOF
)"
[ "$VM_OK" = 1 ] || finish infra 3 "$NAME is not in the launch file"

# prep `kernel:<package>` (Ubuntu): install the kernel flavour, make it the
# GRUB default by its menu entry id and reboot once; this script runs again
# at the next boot and goes on from here.
if [ -n "$PREP_KERNEL" ] && [ ! -e /var/lib/cb-prep.done ]; then
  wait $DOCKER_PID
  echo "cb-boot: prep: installing $PREP_KERNEL (running $(uname -r))"
  export DEBIAN_FRONTEND=noninteractive
  apt-get -o DPkg::Lock::Timeout=120 update -qq && apt-get -o DPkg::Lock::Timeout=120 install -y -qq "$PREP_KERNEL" \
    || finish infra 3 "prep: apt-get install $PREP_KERNEL failed"
  FLAVOUR=${PREP_KERNEL#linux-}
  KV=$(ls /boot/vmlinuz-*-"$FLAVOUR" 2>/dev/null | sed 's|/boot/vmlinuz-||' | sort -V | tail -1)
  [ -n "$KV" ] || finish infra 3 "prep: no /boot/vmlinuz-*-$FLAVOUR after installing $PREP_KERNEL"
  update-grub >/dev/null 2>&1
  SUB=$(grep -oP "submenu '[^']*'.*?\\\$menuentry_id_option '\K[^']+" /boot/grub/grub.cfg | head -1)
  ENT=$(grep -oP "menuentry '[^']*with Linux $KV'.*?\\\$menuentry_id_option '\K[^']+" /boot/grub/grub.cfg | head -1)
  [ -n "$ENT" ] || finish infra 3 "prep: no GRUB entry for $KV"
  echo "GRUB_DEFAULT=\"${SUB:+$SUB>}$ENT\"" > /etc/default/grub.d/99-cb-kernel.cfg
  update-grub >/dev/null 2>&1 || finish infra 3 "prep: update-grub failed"
  echo "$KV" > /var/lib/cb-prep.done
  echo "cb-boot: prep: rebooting into $KV (GRUB_DEFAULT ${SUB:+$SUB>}$ENT)"
  put /var/log/cb-boot.log "$RES/boot-prep.log"
  sync
  reboot
  exit 0
fi
[ -e /var/lib/cb-prep.done ] && echo "cb-boot: prep: wanted $(cat /var/lib/cb-prep.done), running $(uname -r)"
get "$HARNESS" "$W/harness.tar.gz" && tar -xzf "$W/harness.tar.gz" -C "$W/src" || finish infra 3 "cannot fetch the harness $HARNESS"
for o in $SOURCES; do
  get "$o" "$W/sources/${o##*/}" || finish infra 3 "cannot fetch source $o"
done
stamp fetched
wait $DOCKER_PID || { cat /var/log/cb-docker.log; finish infra 3 "docker install failed"; }
stamp docker

# ---- 2. the bench image ----------------------------------------------------------
IMG_ID=
if [ -n "$IMAGE" ]; then
  tok | docker login -u oauth2accesstoken --password-stdin "${IMAGE%%/*}" >/dev/null 2>&1
  if docker pull -q "$IMAGE" >/dev/null; then
    IMG_ID=$IMAGE
  else
    echo "cb-boot: pull of $IMAGE failed; building the image here"
  fi
fi
if [ -z "$IMG_ID" ]; then
  ( cd "$W/src" && docker build -q -f Dockerfile.dev --target test -t cb-dev-test:local . \
      && docker build -q -f bench/Dockerfile.cb --build-arg DEV_IMAGE=cb-dev-test:local -t cb-bench:local . ) \
    > /var/log/cb-image-build.log 2>&1 || { tail -40 /var/log/cb-image-build.log; finish infra 3 "image build failed"; }
  IMG_ID=cb-bench:local
  IMAGE="built:$(docker image inspect -f '{{.Id}}' cb-bench:local | cut -c1-19)"
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
echo $! > /tmp/cb-uploader.pid

# Profiles: default, privileged, perfmon (CAP_PERFMON), and cpusN: a CPU
# quota of N CPUs (cgroup cpu.max, no cpuset: the affinity mask stays the
# host's) plus CAP_PERFMON.
case "$CONTAINER" in
  privileged) PROFILE=(--privileged) ;;
  perfmon) PROFILE=(--cap-add PERFMON) ;;
  cpus[0-9]*) PROFILE=(--cpus="${CONTAINER#cpus}" --cap-add PERFMON) ;;
  *) PROFILE=() ;;
esac
# SELinux (the RHEL family) would deny the bind mounts their labels.
[ "$RUNTIME" = podman ] && PROFILE+=(--security-opt label=disable)
DIGEST=${IMAGE##*@}
docker run --rm --name cb "${PROFILE[@]}" \
  -v "$W/src:/src" -v "$OUT:/out" -v "$W/sources:/cb/sources:ro" \
  -e CB_HOST="$NAME" -e CB_MACHINE="$MACHINE" -e CB_ZONE="$ZONE" \
  -e CB_SPOT="$([ "$PROVISIONING" = SPOT ] && echo 1 || echo 0)" -e CB_IMAGE="${DIGEST:0:19}" \
  -w /src "$IMG_ID" \
  python3 bench/cb run --launch /out/launch.json --vm "$NAME" --sources /cb/sources --out /out/res --bin /opt/cb/bench-bin \
  > "$OUT/cb.log" 2>&1
rc=$?
stamp ran
tail -25 "$OUT/cb.log"
if [ $rc -eq 0 ]; then
  finish ok 0 "shard complete"
else
  finish failed "$rc" "cb run exited $rc"
fi
