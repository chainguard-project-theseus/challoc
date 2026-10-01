set -eux
cd /src
cargo build --release 2>&1 | tail -2
( cd evalpre && CARGO_TARGET_DIR=/src/target/pre cargo build --release 2>&1 | tail -2 )
rm -rf evals0 && mkdir evals0 && cp -r Cargo.toml Cargo.lock src evals0/
[ -f build.rs ] && cp build.rs evals0/; [ -d include ] && cp -r include evals0/
sed -i 's/^pub const START_LG: u32 = 3;/pub const START_LG: u32 = 0;/' evals0/src/heap/mod.rs
grep -n "pub const START_LG" evals0/src/heap/mod.rs
( cd evals0 && CARGO_TARGET_DIR=/src/target/s0 cargo build --release 2>&1 | tail -2 )
mkdir -p target/eval
cp target/s0/release/libchalloc.so target/eval/s0.so
cp target/release/libchalloc.so target/eval/head.so
cp target/pre/release/libchalloc.so target/eval/pre.so
cc -O2 -o target/eval/ballast evaltools/ballast.c
cc -O2 -o target/eval/rus evaltools/rus.c
[ -d target/eval/mb ] || git clone -q https://github.com/daanx/mimalloc-bench target/eval/mb
( cd target/eval/mb && git checkout -q 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7 )
cc -O2 -pthread -DNDEBUG -I target/eval/mb/bench/xmalloc-test -o target/eval/xmalloc-test target/eval/mb/bench/xmalloc-test/xmalloc-test.c
ls -la target/eval
