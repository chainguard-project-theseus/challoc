#!/bin/sh
set -ex
M=/tmp/w/m
cd /work
[ -d $M/cs ] || git worktree add -q --detach $M/cs dlorenc/spike/cow-epoch
cd $M/cs
CARGO_TARGET_DIR=$M/target cargo build --release --locked --no-default-features --features quantum8 -q
mkdir -p $M/lib && cp $M/target/release/libchalloc.so $M/lib/libchalloc.so.0
cd /tmp/w/valkey
for v in vje:9.1.2 vjef:cow-refcount-9.1.2 vep:9.1.2 vepf:cow-refcount-9.1.2; do
  n=${v%%:*}; ref=${v#*:}
  [ -d $M/$n ] || git worktree add -q --detach $M/$n $ref
done
for n in vep vepf; do
  cd $M/$n
  git apply $M/cs/contrib/valkey/0001-Add-MALLOC-challoc.patch $M/cs/contrib/valkey/0002-zmalloc-use-challoc-usable-size-entry-points.patch $M/cs/contrib/valkey/spike/0003-serverFork-challoc-cow-epoch.patch
done
for n in vje vjef; do (cd $M/$n && make -j4 >/dev/null); done
for n in vep vepf; do (cd $M/$n && CFLAGS="-DUSE_MALLOC_USABLE_SIZE -I$M/cs/include" LDFLAGS="-L$M/target/release" make MALLOC=challoc -j4 >/dev/null); done
for n in vje vjef vep vepf; do LD_LIBRARY_PATH=$M/lib $M/$n/src/valkey-server --version; done
