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
#   make test-cgroup-churn   Phase 2 task 6 (a), in `make test`: the 400 MiB churn
#                            under --memory=512m and 256m, policy on and off
#   make test-cgroup-oscillate
#                            Phase 2 task 6 (b), opt-in (5 min): 200..480 MiB live
#                            in 512 MiB for OSC_SECS (300); CONTROL=1 adds the
#                            cgroup:false run
#   make test-cgroup         both of the above
#   make test-cgroup-compare Phase 2 task 7: the same runs under CGROUP_ALLOCS
#                            (jemalloc mimalloc tcmalloc), recorded, no criterion
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
#   make quick               fast tier (bench/quick.sh, ~2.5 min on a VM): build the
#                            working tree and OLD (default HEAD) into target/quick/,
#                            one round of larson 1/10, xmalloc-test, alloc-test 1,
#                            the Valkey model (bench/probes/valkey_model.c, 5 s) and
#                            the churn probe under old, new and jemalloc; one
#                            table; exit 1 when new trails old by more than
#                            QUICK_TOLERANCE (5) percent or does not compact.
#                            PERF=1 adds per-operation counters where perf exists
#                            in the container (on the VMs: challoc-bench quick-perf)
#   make scorecard           the fixed row set (larson 1/10/16, xmalloc, alloc-test 1/N,
#                            valkey1, valkey1-free, cfrac, espresso, mstress,
#                            glibc-simple, redis, python1, python1-malloc) under challoc (REV=tree) and
#                            jemalloc, 3 interleaved rounds, one line appended to
#                            bench/results/scorecard-<arch>.md as percentages vs
#                            jemalloc plus the latest soak for the commit
#                            (bench/scorecard.sh; RENDER=1 prints the trend table)
#   make ab                  protocol tier A/B (bench/ab.sh): VARIANTS='old=HEAD~1
#                            new=HEAD narenas16=HEAD~1:narenas:16 jemalloc', ROUNDS=5
#                            plus warm-up, interleaved, sorted scores and median RSS
#                            in the results-file table shape (RESULT=path, ADAPTIVE=1,
#                            BASELINES=cached)
#   make baseline            cache jemalloc/glibc/mimalloc/tcmalloc protocol rows for this
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
#   make ruby-bench          Ruby benchmark rows (bench/ruby-bench.sh): a fixed
#                            subset of ruby/benchmark through benchmark-driver,
#                            `gem list` and bench/ruby/gc_churn.rb under challoc's
#                            stage, Wolfi's jemalloc (LD_PRELOAD), the glibc build
#                            mimalloc and tcmalloc; wall time and max RSS from bench/rt,
#                            GC.stat and RSS around forced GCs; ROUNDS=5 plus
#                            warm-up, interleaved; writes
#                            bench/results/<date>-<arch>-ruby.md (RESULT=path,
#                            ALLOCS, ROWS, HOST_KIND). Needs both `make ruby
#                            STAGE=build` builds.
#   make python-bench        Python 3.13 rows (bench/python-bench.sh): the stdlib
#                            workloads under bench/probes/python/ (JSON, dict and
#                            list churn, regex, object graph), each with
#                            PYTHONMALLOC unset and =malloc, under challoc,
#                            jemalloc, glibc, mimalloc and tcmalloc via LD_PRELOAD;
#                            wall time and max RSS from bench/rt, ROUNDS=5 plus
#                            warm-up, interleaved; writes
#                            bench/results/<date>-<arch>-python.md (RESULT=path,
#                            ALLOCS, WORKLOADS, MODES, HOST_KIND)
#
# `target/` inside the container is a named Docker volume, one per
# architecture, so Linux artifacts never mix with host artifacts and builds do
# not go through the slow macOS bind mount. The cargo registry is a named
# volume too so crates are fetched once. `TARGET_VOLUME` (default
# `challoc-target-$(ARCH)`) names the target volume and `CARGO_VOLUME`
# (default `challoc-cargo-registry`) the registry volume; a lane worktree
# (CLAUDE.md, Environment) exports `TARGET_VOLUME=challoc-target-<lane>-$(ARCH)`
# so two worktrees never share a `target/`. `make clean-volumes` removes the
# two volumes the variables name.

HOST_ARCH := $(shell uname -m | sed -e 's/x86_64/amd64/' -e 's/aarch64/arm64/')
PLATFORM  ?= linux/$(HOST_ARCH)
ARCH      := $(notdir $(PLATFORM))
IMAGE     ?= challoc-dev:$(ARCH)
TARGET_VOLUME ?= challoc-target-$(ARCH)
CARGO_VOLUME  ?= challoc-cargo-registry

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

DOCKER_BASE = docker run --rm --platform $(PLATFORM) \
	-v "$(CURDIR)":/src -w /src \
	-v $(TARGET_VOLUME):/src/target \
	-v $(CARGO_VOLUME):/root/.cargo/registry \
	$(GIT_MOUNT)
DOCKER_RUN = $(DOCKER_BASE) $(DOCKER_FLAGS) $(IMAGE)
# cgroup_run LIMIT,ARGS: one memory-limited container running
# scripts/cgroup-tests.sh ARGS (Phase 2 tasks 6 and 7). Swap is disabled
# (`--memory-swap` equal to `--memory`): Docker Desktop's VM has 1 GiB of
# swap and `--memory` alone lets a churn spill into it instead of being
# charged against the limit.
cgroup_docker = $(DOCKER_BASE) --memory=$(1) --memory-swap=$(1) $(if $(CGROUP_WATCHDOG),-e WATCHDOG=$(CGROUP_WATCHDOG)) $(IMAGE) sh scripts/cgroup-tests.sh
cgroup_run = $(call cgroup_docker,$(1)) $(2)

.PHONY: image dev run build test test-cgroup-limit test-cgroup-churn test-cgroup-oscillate test-cgroup test-cgroup-compare test-host jemalloc-compat fuzz bench-build bench quick ab baseline scorecard valkey valkey-soak soak-quick soak-baseline ruby ruby-bench python-bench clean-volumes

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
	$(MAKE) --no-print-directory test-cgroup-limit
	$(MAKE) --no-print-directory test-cgroup-churn

# Phase 2 task 4: the same tests that read the cgroup, in a container with
# a 512 MiB limit; the effective `memory.max` must read as that number.
# Single-threaded since task 5: tests/c/cgroup_policy.c fills the cgroup to
# 94% of the limit, which a concurrent test's 64 MiB would push over it.
test-cgroup-limit: DOCKER_FLAGS = --memory=512m -e CHALLOC_EXPECT_CGROUP_LIMIT=536870912
test-cgroup-limit: image
	$(DOCKER_RUN) sh -c 'cargo build --release && cargo test --features std cgroup -- --nocapture --test-threads=1'

# Phase 2 task 6: the container tests of plan Phase 2, one container per
# run (scripts/cgroup-tests.sh; the exit status is the program's, 137 is an
# OOM kill). test-cgroup-churn (about 40 s, in `make test`): tests/c/
# cgroup_churn.c allocates 400 MiB, frees it and idles under --memory=512m;
# RSS must be under 100 MiB within 2 s with the policy on (`purge`) and
# must not be with MALLOC_CONF=cgroup:false (`keep`, the control); the same
# at --memory=256m with the sizes scaled to the limit (200 MiB, 50 MiB).
# test-cgroup-oscillate (OSC_SECS, default 300 s, plus 300 s more with
# CONTROL=1: the same run under cgroup:false, recorded, not a criterion):
# tests/c/cgroup_oscillate.c swings between 200 and 480 MiB live in 512 MiB
# and must not be OOM-killed. Opt-in because of its length: `make
# test-cgroup` runs both; `make test` runs only the churn.
OSC_SECS ?= 300
test-cgroup-churn: image
	$(DOCKER_RUN) cargo build --release
	$(call cgroup_run,512m,churn challoc purge 400 100)
	$(call cgroup_run,512m,churn challoc-off keep 400 100)
	$(call cgroup_run,256m,churn challoc purge 200 50)
	$(call cgroup_run,256m,churn challoc-off keep 200 50)

test-cgroup-oscillate: image
	$(DOCKER_RUN) cargo build --release
	$(call cgroup_run,512m,oscillate challoc $(OSC_SECS))
ifneq ($(CONTROL),)
	-$(call cgroup_run,512m,oscillate challoc-off $(OSC_SECS))
endif

test-cgroup: test-cgroup-churn test-cgroup-oscillate

# Phase 2 task 7: the same runs under other allocators (CGROUP_ALLOCS,
# default jemalloc, mimalloc and tcmalloc; needs `make bench-build` for mimalloc),
# `report` mode for the churn (no criterion) and the oscillation for
# OSC_SECS; every outcome is printed (OUTCOME lines) and none fails the
# target, the results file records them.
CGROUP_ALLOCS ?= jemalloc mimalloc tcmalloc
test-cgroup-compare: image
	@for a in $(CGROUP_ALLOCS); do \
	  $(call cgroup_docker,512m) churn $$a report 400 100; \
	  $(call cgroup_docker,256m) churn $$a report 200 50; \
	  $(call cgroup_docker,512m) oscillate $$a $(OSC_SECS); \
	done; true

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
QUICK_VARS = OLD NEW QUICK_TOLERANCE QUICK_BENCHES CHURN_SECS CHURN_CLASS CHURN_MAX_FRACTION BASELINES BASELINE_TOLERANCE QUICK_SANITY PERF PERF_BENCHES QUICK_BUILD_ONLY VALKEY_MODEL_SECS VALKEY_MODEL_N PIN
quick: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(foreach v,$(QUICK_VARS),$(if $($(v)),-e $(v)="$($(v))"))
quick: image
	$(DOCKER_RUN) sh bench/quick.sh

AB_VARS = VARIANTS ROUNDS BENCHES ADAPTIVE BASELINES BASELINE_TOLERANCE RESULT HOST_KIND TITLE VALKEY_MODEL_SECS VALKEY_MODEL_N PIN
ab: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(foreach v,$(AB_VARS),$(if $($(v)),-e $(v)="$($(v))"))
ab: image
	$(DOCKER_RUN) sh bench/ab.sh

baseline: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(if $(ROUNDS),-e ROUNDS="$(ROUNDS)") $(if $(BENCHES),-e BENCHES="$(BENCHES)") $(if $(ALLOCS),-e ALLOCS="$(ALLOCS)") $(if $(HOST_KIND),-e HOST_KIND="$(HOST_KIND)") $(if $(VALKEY_MODEL_SECS),-e VALKEY_MODEL_SECS="$(VALKEY_MODEL_SECS)")
baseline: image
	$(DOCKER_RUN) sh bench/baseline.sh

# Scorecard (bench/scorecard.sh): the fixed row set under challoc (REV, default
# the working tree) and jemalloc, 3 interleaved rounds through bench/ab.sh,
# one line appended to bench/results/scorecard-<arch>.md; `make scorecard
# RENDER=1` prints the table sorted by date.
SCORECARD_VARS = REV ROUNDS ROWS BASELINES BASELINE_TOLERANCE HOST_KIND VALKEY_MODEL_SECS VALKEY_MODEL_N PIN SCORECARD_FILE
scorecard: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(foreach v,$(SCORECARD_VARS),$(if $($(v)),-e $(v)="$($(v))"))
scorecard: image
	$(if $(RENDER),sh bench/scorecard.sh --render,$(DOCKER_RUN) sh bench/scorecard.sh)

valkey: DOCKER_FLAGS = $(if $(STAGE),-e STAGE=$(STAGE)) $(if $(TEST_FLAGS),-e TEST_FLAGS="$(TEST_FLAGS)")
valkey: image
	$(DOCKER_RUN) sh contrib/valkey/build.sh

SOAK_VARS = ALLOCS DURATION SETTLE INTERVAL CHUNK VB_REQS KEYS SIZE MT_SIZES DEL_RATIO KEY_PATTERN DEFRAG RESULT_DIR OCCUPANCY MS_INTERVAL BASELINES SAVE_BASELINE PORT
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

RUBY_BENCH_VARS = ALLOCS ROUNDS ROWS RESULT OUT HOST_KIND GC_ROUNDS GC_FORCED
ruby-bench: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(foreach v,$(RUBY_BENCH_VARS),$(if $($(v)),-e $(v)="$($(v))"))
ruby-bench: image
	$(DOCKER_RUN) sh bench/ruby-bench.sh

# Python rows (bench/python-bench.sh, Phase 2.5 task 1): the stdlib workloads
# under bench/probes/python/ on the image's python3, each with PYTHONMALLOC
# unset and =malloc, under challoc (the working tree) and the stock allocators.
PYTHON_BENCH_VARS = ALLOCS ROUNDS WORKLOADS MODES RESULT OUT HOST_KIND REV
python-bench: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(foreach v,$(PYTHON_BENCH_VARS),$(if $($(v)),-e $(v)="$($(v))"))
python-bench: image
	$(DOCKER_RUN) sh bench/python-bench.sh

clean-volumes:
	docker volume rm -f $(TARGET_VOLUME) $(CARGO_VOLUME)
