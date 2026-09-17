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
#   make quick               fast tier (bench/quick.sh, ~2 min on a VM): build the
#                            working tree and OLD (default HEAD) into target/quick/,
#                            one round of larson 1/10, xmalloc-test, alloc-test 1
#                            and the churn probe under old, new and jemalloc; one
#                            table; exit 1 when new trails old by more than
#                            QUICK_TOLERANCE (5) percent or does not compact
#   make ab                  protocol tier A/B (bench/ab.sh): VARIANTS='old=HEAD~1
#                            new=HEAD narenas16=HEAD~1:narenas:16 jemalloc', ROUNDS=5
#                            plus warm-up, interleaved, sorted scores and median RSS
#                            in the results-file table shape (RESULT=path, ADAPTIVE=1,
#                            BASELINES=cached)
#   make baseline            cache jemalloc/glibc/mimalloc protocol rows for this
#                            machine under target/baselines/ (bench/baseline.sh)
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
#                            RESULT_DIR, OCCUPANCY=1 for MEMORY MALLOC-STATS dumps
#                            every MS_INTERVAL s). Needs `make valkey STAGE=build` first.
#   make soak-quick          challoc-only 600 s soak with OCCUPANCY=1, compared
#                            against the cached jemalloc soak from `make
#                            soak-baseline` (1200 s protocol under jemalloc, stored
#                            under target/baselines/); results under
#                            target/valkey/soak-quick. Not an acceptance run.
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

# In a git worktree `.git` is a file pointing at the main repository's
# `.git/worktrees/<name>`; mount that common dir at the same path so `git
# archive`/`rev-parse` work inside the container (bench/quick.sh, bench/ab.sh
# build variants from git revs). A plain checkout needs no extra mount.
GIT_COMMON := $(shell git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
ifneq ($(GIT_COMMON),)
ifneq ($(GIT_COMMON),$(CURDIR)/.git)
GIT_MOUNT = -v "$(GIT_COMMON)":"$(GIT_COMMON)":ro
endif
endif

DOCKER_RUN = docker run --rm --platform $(PLATFORM) \
	-v "$(CURDIR)":/src -w /src \
	-v challoc-target-$(ARCH):/src/target \
	-v challoc-cargo-registry:/root/.cargo/registry \
	$(GIT_MOUNT) $(DOCKER_FLAGS) $(IMAGE)

.PHONY: image dev run build test test-host jemalloc-compat fuzz bench-build bench quick ab baseline valkey valkey-soak soak-quick soak-baseline ruby clean-volumes

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

# Two-tier harness (bench/README.md). quick: ~2 min go/no-go for an
# iteration; ab: the protocol-tier A/B that results files are written from;
# baseline: cache the stock allocators' protocol rows for BASELINES=cached.
QUICK_VARS = OLD NEW QUICK_TOLERANCE QUICK_BENCHES CHURN_SECS CHURN_CLASS CHURN_MAX_FRACTION BASELINES BASELINE_TOLERANCE QUICK_SANITY
quick: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(foreach v,$(QUICK_VARS),$(if $($(v)),-e $(v)="$($(v))"))
quick: image
	$(DOCKER_RUN) sh bench/quick.sh

AB_VARS = VARIANTS ROUNDS BENCHES ADAPTIVE BASELINES BASELINE_TOLERANCE RESULT HOST_KIND TITLE
ab: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(foreach v,$(AB_VARS),$(if $($(v)),-e $(v)="$($(v))"))
ab: image
	$(DOCKER_RUN) sh bench/ab.sh

baseline: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(if $(ROUNDS),-e ROUNDS="$(ROUNDS)") $(if $(BENCHES),-e BENCHES="$(BENCHES)") $(if $(ALLOCS),-e ALLOCS="$(ALLOCS)") $(if $(HOST_KIND),-e HOST_KIND="$(HOST_KIND)")
baseline: image
	$(DOCKER_RUN) sh bench/baseline.sh

valkey: DOCKER_FLAGS = $(if $(STAGE),-e STAGE=$(STAGE)) $(if $(TEST_FLAGS),-e TEST_FLAGS="$(TEST_FLAGS)")
valkey: image
	$(DOCKER_RUN) sh contrib/valkey/build.sh

SOAK_VARS = ALLOCS DURATION SETTLE INTERVAL CHUNK VB_REQS KEYS SIZE MT_SIZES DEL_RATIO DEFRAG RESULT_DIR OCCUPANCY MS_INTERVAL BASELINES SAVE_BASELINE PORT
valkey-soak: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(foreach v,$(SOAK_VARS),$(if $($(v)),-e $(v)="$($(v))"))
valkey-soak: image
	$(DOCKER_RUN) sh bench/valkey-soak.sh

# Quick soak: challoc only, 10 minutes of load, occupancy sampler on, compared
# against the cached jemalloc soak from `make soak-baseline` (the full 1200 s
# protocol under jemalloc, stored under target/baselines/). Results under
# target/valkey/soak-quick, never bench/results/. Acceptance stays `make valkey-soak`.
soak-quick: ALLOCS = challoc
soak-quick: DURATION ?= 600
soak-quick: SETTLE ?= 60
soak-quick: OCCUPANCY ?= 1
soak-quick: BASELINES = cached
soak-quick: RESULT_DIR ?= target/valkey/soak-quick
soak-quick: valkey-soak

soak-baseline: ALLOCS = jemalloc
soak-baseline: DURATION ?= 1200
soak-baseline: SETTLE ?= 60
soak-baseline: SAVE_BASELINE = 1
soak-baseline: RESULT_DIR ?= target/valkey/soak-baseline
soak-baseline: valkey-soak

RUBY_VARS = STAGE ALLOC TESTS TESTOPTS JOBS RUBY_TAG RUBY_COMMIT RUBY_CHERRY
ruby: DOCKER_FLAGS = $(foreach v,$(RUBY_VARS),$(if $($(v)),-e $(v)="$($(v))"))
ruby: image
	$(DOCKER_RUN) sh contrib/ruby/build.sh

clean-volumes:
	docker volume rm -f challoc-target-$(ARCH) challoc-cargo-registry
