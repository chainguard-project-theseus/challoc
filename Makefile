# challoc build entry points. The library is Linux-only; every target except
# `test-host` runs inside the Wolfi dev container built from Dockerfile.dev.
#
#   make dev                 interactive shell in the container (native arch)
#   make dev PLATFORM=linux/amd64
#                            same, emulated x86_64 (correctness only)
#   make run CMD='...'       run one shell command in the container
#   make build               cargo build --release in the container
#   make test                cargo build --release, then cargo test --features std
#                            in the container (some tests load the release .so).
#                            The `api::` tests run in a second process: they
#                            initialise the real allocator, whose reservations
#                            plus a second full layout (the layout tests) exceed
#                            what Rosetta can track under emulation.
#   make test-host           pure-logic unit tests on the host
#   make jemalloc-compat     build with the `jemalloc-soname` feature into
#                            target/jemalloc-compat and stage libjemalloc.so.2,
#                            the libjemalloc.so link name and jemalloc.pc under
#                            target/jemalloc-compat/stage (scripts/jemalloc-compat.sh;
#                            QUANTUM=8|16, PREFIX=<dir> for a package layout)
#   make fuzz                build, then run fuzz/fuzz.c under LD_PRELOAD for
#                            FUZZ_OPS (10M) operations on FUZZ_THREADS (4)
#                            threads with FUZZ_SEED (fixed; `random` draws one)
#   make bench-build         fetch and build the mimalloc-bench programs, redis,
#                            mimalloc and the bench/ helpers into the target volume
#   make bench               bench-build, cargo build --release, then run the
#                            benchmark protocol (bench/run.sh) and write
#                            bench/results/<date>-<arch>.md (RESULT=path overrides;
#                            HOST_KIND="..." names the machine in the header).
#                            Native arch only.
#   make valkey              build challoc (quantum8) and Valkey at the tag
#                            Wolfi ships with contrib/valkey/0001-*.patch, then
#                            run unit/memefficiency and the full test suite
#                            (contrib/valkey/build.sh; STAGE=build|memefficiency|
#                            full|all, TEST_FLAGS='--clients 8', logs under
#                            target/valkey/logs)
#   make valkey-soak         soak Valkey under challoc and under its bundled
#                            jemalloc 5.3.0: write-heavy then delete-heavy
#                            phases with valkey-benchmark and memtier_benchmark,
#                            INFO memory sampled to CSV every 10 s, summary in
#                            bench/results/<date>-<arch>-valkey-soak.md
#                            (bench/valkey-soak.sh; ALLOCS, DURATION=1200,
#                            SETTLE, INTERVAL, CHUNK, VB_REQS, KEYS, DEFRAG,
#                            RESULT_DIR). Needs `make valkey STAGE=build` first.
#   make ruby                stage libjemalloc.so.2 (quantum16), build Ruby 3.4 at
#                            the tag Wolfi ships with Wolfi's configure flags
#                            (`--with-jemalloc`) against the stage, check that the
#                            binaries load it, then run `make test-all`
#                            (contrib/ruby/build.sh; STAGE=build|test|all,
#                            ALLOC=challoc|glibc for a baseline build without
#                            jemalloc, TESTS='ruby/test_gc.rb ...', TESTOPTS,
#                            JOBS; logs under target/ruby/logs)
#
# `target/` inside the container is a named Docker volume, one per
# architecture, so Linux artifacts never mix with host artifacts and builds do
# not go through the slow macOS bind mount. The cargo registry is a named
# volume too so crates are fetched once.

HOST_ARCH := $(shell uname -m | sed -e 's/x86_64/amd64/' -e 's/aarch64/arm64/')
PLATFORM  ?= linux/$(HOST_ARCH)
ARCH      := $(notdir $(PLATFORM))
IMAGE     ?= challoc-dev:$(ARCH)

DOCKER_RUN = docker run --rm --platform $(PLATFORM) \
	-v "$(CURDIR)":/src -w /src \
	-v challoc-target-$(ARCH):/src/target \
	-v challoc-cargo-registry:/root/.cargo/registry \
	$(DOCKER_FLAGS) $(IMAGE)

.PHONY: image dev run build test test-host jemalloc-compat fuzz bench-build bench valkey valkey-soak ruby clean-volumes

image:
	docker build --platform $(PLATFORM) -f Dockerfile.dev -t $(IMAGE) .

dev: DOCKER_FLAGS = -it
dev: image
	$(DOCKER_RUN) sh

run: image
	$(DOCKER_RUN) sh -c '$(CMD)'

build: image
	$(DOCKER_RUN) cargo build --release

test: image
	$(DOCKER_RUN) sh -c 'cargo build --release \
	  && sh scripts/jemalloc-compat.sh \
	  && cargo test --features std -- --nocapture --skip api:: \
	  && cargo test --features std api:: -- --nocapture'

test-host:
	cargo test --features std

jemalloc-compat: DOCKER_FLAGS = $(if $(QUANTUM),-e QUANTUM=$(QUANTUM)) $(if $(PREFIX),-e PREFIX=$(PREFIX))
jemalloc-compat: image
	$(DOCKER_RUN) sh scripts/jemalloc-compat.sh

FUZZ_OPS     ?= 10000000
FUZZ_THREADS ?= 4
FUZZ_SEED    ?= 0xC4A110C
FUZZ_TABLE   ?= 2048
fuzz: image
	$(DOCKER_RUN) sh -c 'cargo build --release \
	  && cc -O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=16 -Itests/c \
	       fuzz/fuzz.c -o target/fuzz -ldl \
	  && LD_PRELOAD=/src/target/release/libchalloc.so \
	     ./target/fuzz $(FUZZ_OPS) $(FUZZ_THREADS) $(FUZZ_SEED) $(FUZZ_TABLE)'

HOST_CPU := $(shell sysctl -n machdep.cpu.brand_string 2>/dev/null || grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' | grep . || lscpu 2>/dev/null | grep -m1 'Model name' | sed 's/.*: *//')
bench-build: image
	$(DOCKER_RUN) sh bench/build.sh

bench: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(if $(HOST_KIND),-e HOST_KIND="$(HOST_KIND)") $(if $(RESULT),-e RESULT="$(RESULT)")
bench: image
	$(DOCKER_RUN) sh -c 'sh bench/build.sh >/dev/null && cargo build --release && sh bench/run.sh'

valkey: DOCKER_FLAGS = $(if $(STAGE),-e STAGE=$(STAGE)) $(if $(TEST_FLAGS),-e TEST_FLAGS="$(TEST_FLAGS)")
valkey: image
	$(DOCKER_RUN) sh contrib/valkey/build.sh

SOAK_VARS = ALLOCS DURATION SETTLE INTERVAL CHUNK VB_REQS KEYS SIZE MT_SIZES DEL_RATIO DEFRAG RESULT_DIR
valkey-soak: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(foreach v,$(SOAK_VARS),$(if $($(v)),-e $(v)="$($(v))"))
valkey-soak: image
	$(DOCKER_RUN) sh bench/valkey-soak.sh

RUBY_VARS = STAGE ALLOC TESTS TESTOPTS JOBS RUBY_TAG RUBY_COMMIT RUBY_CHERRY
ruby: DOCKER_FLAGS = $(foreach v,$(RUBY_VARS),$(if $($(v)),-e $(v)="$($(v))"))
ruby: image
	$(DOCKER_RUN) sh contrib/ruby/build.sh

clean-volumes:
	docker volume rm -f challoc-target-$(ARCH) challoc-cargo-registry
