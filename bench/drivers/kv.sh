# shellcheck shell=sh
# shellcheck disable=SC2034 # KV_* are read by the sourcing drivers
# bench/drivers/kv.sh: the Redis and Valkey server builds the key-value
# drivers share (valkey-soak.sh, kv-ops.sh). Sourced, POSIX sh, after ROOT
# is set; reads KV_SERVER (valkey | redis, default valkey).
#
# One pinned tag per server, cloned from GitHub on first use and
# commit-checked, built at most once per target volume in up to three
# trees under target/<server>/:
#
#   src-<tag>-jemalloc  MALLOC=jemalloc: the server as upstream ships it, its
#                       bundled jemalloc 5.3.0 (lg_quantum 3, je_ prefix,
#                       static), active defrag available. The `jemalloc`
#                       allocator of these drivers, and the client binaries
#                       (cli, benchmark) of every allocator.
#   src-<tag>-libc      MALLOC=libc: zmalloc calls malloc/free and asks
#                       malloc_usable_size (glibc's HAVE_MALLOC_SIZE), no
#                       active defrag. Every other allocator runs this one
#                       binary with its library in LD_PRELOAD (glibc: none),
#                       so only the allocator changes between them.
#   src-<tag>-runpath   the server's challoc integration applied
#                       (contrib/valkey/000{1,2}, contrib/redis/000{1,2,3}),
#                       MALLOC=challoc, linked with a RUNPATH of
#                       KV_RUNPATH whose libchalloc.so.0 is pointed at the
#                       variant's library before each start (bench/README.md,
#                       "Valkey, fixed runpath"): the usable-size entry
#                       points, challoc's defrag hint, the jemalloc code
#                       paths (stats, purge, the je_malloc_conf tuning).
#
# kv_resolve ALLOC [LINK] sets, for one allocator (challoc, jemalloc,
# jemalloc53, jemalloc54, glibc, tcmalloc, mimalloc; LINK native|preload
# for challoc):
#   KV_SRV       the server binary
#   KV_PRELOAD   the library to LD_PRELOAD into the server ("" for none)
#   KV_EXPECT    INFO memory's mem_allocator for that build
#   KV_DEFRAG    yes when the build can run active defrag, no otherwise
#   KV_BUILD     jemalloc | libc | runpath (which tree)
# and builds the tree when it is missing. CB_LIB is the challoc library
# (and the stock one under cb); by hand KV_CHALLOC_LIB.

KV_SERVER=${KV_SERVER:-valkey}
case $KV_SERVER in
  valkey)
    KV_TAG=${VALKEY_TAG:-9.1.2}
    KV_COMMIT=${VALKEY_COMMIT:-7f1dffedff6de73058b2c2a389422b6ecd56c8fb}
    KV_REPO=https://github.com/valkey-io/valkey;;
  redis)
    KV_TAG=${REDIS_TAG:-8.10.3}
    KV_COMMIT=${REDIS_COMMIT:-1a2574ce1551d3de6e0b4a56756c0fdbc6659e4f}
    KV_REPO=https://github.com/redis/redis;;
  *) echo "kv.sh: KV_SERVER must be valkey or redis, got '$KV_SERVER'" >&2; exit 1;;
esac
KV_WORK=$ROOT/target/$KV_SERVER
KV_SRC=$KV_WORK/src-$KV_TAG
KV_SRC_JE=$KV_SRC-jemalloc
KV_SRC_LIBC=$KV_SRC-libc
KV_SRC_RP=$KV_SRC-runpath
KV_RUNPATH=${VALKEY_RUNPATH:-/opt/challoc/lib}
# The server's challoc integration, applied in order to the runpath tree.
case $KV_SERVER in
  valkey) KV_PATCHES="$ROOT/contrib/valkey/0001-Add-MALLOC-challoc.patch $ROOT/contrib/valkey/0002-zmalloc-use-challoc-usable-size-entry-points.patch";;
  redis)  KV_PATCHES=$(printf '%s ' "$ROOT"/contrib/redis/0*.patch);;
esac
KV_CLI=$KV_SRC_JE/src/$KV_SERVER-cli
KV_BENCH=$KV_SRC_JE/src/$KV_SERVER-benchmark
KV_BIN=${CB_BIN:-$ROOT/target/bench-bin}

kv_log() { echo "kv.sh: $*" >&2; }
kv_die() { echo "kv.sh: $*" >&2; exit 1; }

# kv_clone DIR: a pristine tree of KV_TAG in DIR, from the by-hand tree
# (target/<server>/src-<tag>, tests/consumers/valkey-build.sh) when there
# is one, else from GitHub; the commit is checked.
kv_clone() {
  [ -d "$1/.git" ] && return 0
  mkdir -p "$KV_WORK/logs"
  if [ -d "$KV_SRC/.git" ]; then
    git -c safe.directory='*' clone -q "$KV_SRC" "$1"
  else
    git -c advice.detachedHead=false clone -q --depth 1 --branch "$KV_TAG" "$KV_REPO" "$1"
  fi
  kv_head=$(git -c safe.directory='*' -C "$1" rev-parse HEAD)
  [ "$kv_head" = "$KV_COMMIT" ] || kv_die "$1 is $kv_head, expected $KV_COMMIT for $KV_SERVER $KV_TAG"
}
# kv_make DIR LOG ARGS...: make in DIR, the log in KV_WORK/logs; the tail on failure.
kv_make() {
  kv_dir=$1; kv_lf=$KV_WORK/logs/$2; shift 2
  mkdir -p "$KV_WORK/logs"
  ( cd "$kv_dir" && make -j"$(nproc)" "$@" all ) > "$kv_lf" 2>&1 \
    || { tail -30 "$kv_lf" >&2; kv_die "build in $kv_dir failed (see $kv_lf)"; }
}
kv_ensure_jemalloc() {
  [ -x "$KV_SRC_JE/src/$KV_SERVER-server" ] && return 0
  kv_log "building $KV_SERVER $KV_TAG with its bundled jemalloc in $KV_SRC_JE"
  kv_clone "$KV_SRC_JE"
  kv_make "$KV_SRC_JE" "build-jemalloc-$(uname -m).log" MALLOC=jemalloc BUILD_TLS=yes
}
kv_ensure_libc() {
  [ -x "$KV_SRC_LIBC/src/$KV_SERVER-server" ] && return 0
  kv_log "building $KV_SERVER $KV_TAG with MALLOC=libc in $KV_SRC_LIBC"
  kv_clone "$KV_SRC_LIBC"
  kv_make "$KV_SRC_LIBC" "build-libc-$(uname -m).log" MALLOC=libc BUILD_TLS=yes
}
# kv_point_runpath LIB: KV_RUNPATH/libchalloc.so.0 (and .so) -> LIB.
kv_point_runpath() {
  mkdir -p "$KV_RUNPATH"
  ln -sfn "$1" "$KV_RUNPATH/libchalloc.so.0"
  ln -sfn "$1" "$KV_RUNPATH/libchalloc.so"
}
# kv_ensure_runpath LIB: the patched server linked against KV_RUNPATH, built
# once per patch series.
kv_ensure_runpath() {
  # shellcheck disable=SC2086 # KV_PATCHES is a list of paths without spaces
  kv_stamp=$(cat $KV_PATCHES | sha256sum | cut -d' ' -f1)
  if [ -x "$KV_SRC_RP/src/$KV_SERVER-server" ] && [ "$(cat "$KV_SRC_RP.patches" 2>/dev/null)" = "$kv_stamp" ]; then
    kv_point_runpath "$1"; return 0
  fi
  kv_log "building the fixed-runpath challoc $KV_SERVER in $KV_SRC_RP ($KV_RUNPATH)"
  rm -rf "$KV_SRC_RP"
  kv_clone "$KV_SRC_RP"
  # shellcheck disable=SC2086
  git -c safe.directory='*' -C "$KV_SRC_RP" apply $KV_PATCHES
  kv_point_runpath "$1"
  kv_make "$KV_SRC_RP" "build-runpath-$(uname -m).log" MALLOC=challoc BUILD_TLS=yes CFLAGS="-I$ROOT/include" \
    LDFLAGS="-L$KV_RUNPATH -Wl,-rpath,$KV_RUNPATH -Wl,--enable-new-dtags"
  readelf -d "$KV_SRC_RP/src/$KV_SERVER-server" | grep -q "RUNPATH.*$KV_RUNPATH" \
    || kv_die "$KV_SRC_RP/src/$KV_SERVER-server has no RUNPATH $KV_RUNPATH"
  echo "$kv_stamp" > "$KV_SRC_RP.patches"
}

# kv_stock_lib ALLOC: the library of a preloaded stock allocator (by hand;
# under cb CB_LIB is the same path).
kv_stock_lib() {
  case $1 in
    glibc)      echo "";;
    jemalloc53) echo "$KV_BIN/jemalloc-5.3.0/libjemalloc.so.2";;
    jemalloc54) echo "$KV_BIN/jemalloc-5.4.0/libjemalloc.so.2";;
    tcmalloc)   echo /usr/lib/libtcmalloc_minimal.so.4;;
    mimalloc)   echo "$KV_BIN/libmimalloc.so";;
    *) return 1;;
  esac
}

kv_resolve() {
  kv_alloc=$1; kv_link=${2:-native}
  case $kv_alloc in
    jemalloc)
      kv_ensure_jemalloc
      KV_SRV=$KV_SRC_JE/src/$KV_SERVER-server; KV_PRELOAD=; KV_EXPECT=jemalloc-5.3.0; KV_DEFRAG=yes; KV_BUILD=jemalloc;;
    challoc)
      kv_lib=${CB_LIB:-${KV_CHALLOC_LIB:-}}
      [ -f "$kv_lib" ] || kv_die "challoc: no library (CB_LIB or KV_CHALLOC_LIB): '$kv_lib'"
      if [ "$kv_link" = native ]; then
        kv_ensure_runpath "$kv_lib"
        KV_SRV=$KV_SRC_RP/src/$KV_SERVER-server; KV_PRELOAD=; KV_EXPECT=jemalloc-5.3.0-challoc; KV_DEFRAG=yes; KV_BUILD=runpath
      else
        kv_ensure_libc
        KV_SRV=$KV_SRC_LIBC/src/$KV_SERVER-server; KV_PRELOAD=$kv_lib; KV_EXPECT=libc; KV_DEFRAG=no; KV_BUILD=libc
      fi;;
    glibc|jemalloc53|jemalloc54|tcmalloc|mimalloc)
      kv_ensure_libc
      if [ -n "${CB_ALLOC:-}" ]; then KV_PRELOAD=${CB_LIB:-}; else KV_PRELOAD=$(kv_stock_lib "$kv_alloc"); fi
      [ -z "$KV_PRELOAD" ] || [ -f "$KV_PRELOAD" ] || kv_die "$kv_alloc: no library $KV_PRELOAD"
      KV_SRV=$KV_SRC_LIBC/src/$KV_SERVER-server; KV_EXPECT=libc; KV_DEFRAG=no; KV_BUILD=libc;;
    *) kv_die "unknown allocator $kv_alloc (challoc, jemalloc, jemalloc53, jemalloc54, glibc, tcmalloc, mimalloc)";;
  esac
}

# kv_mapped PID: the allocator libraries PID maps, real paths, space-separated.
kv_mapped() {
  grep -oE '/[^ ]*/lib(challoc|jemalloc|mimalloc|tcmalloc)[^/ ]*\.so[.0-9]*' "/proc/$1/maps" 2>/dev/null \
    | sort -u | while read -r kv_f; do readlink -f "$kv_f"; done | sort -u | tr '\n' ' ' | sed 's/ $//'
}
# kv_check_binding PID ALLOC: the server maps exactly what KV_resolve said
# (the runpath build: the variant's challoc; bundled: nothing; preload: the
# preloaded library; glibc: nothing). Dies otherwise.
kv_check_binding() {
  case $KV_BUILD in
    runpath) kv_want=$(readlink -f "$KV_RUNPATH/libchalloc.so.0");;
    jemalloc) kv_want=;;
    libc) kv_want=; [ -n "$KV_PRELOAD" ] && kv_want=$(readlink -f "$KV_PRELOAD");;
  esac
  kv_got=$(kv_mapped "$1")
  [ "$kv_got" = "$kv_want" ] || kv_die "$2 server (pid $1) maps '${kv_got:-nothing}', expected '${kv_want:-no allocator library}'"
}
