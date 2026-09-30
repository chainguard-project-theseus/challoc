# challoc build entry points. The library is Linux-only; every target except
# `test-host`, `help` and the `clean-*` targets runs inside the Wolfi dev
# container built from Dockerfile.dev. `make help` lists the targets from the
# `##` comments below; the benchmark harness, its two tiers and every
# harness's variables are described in bench/README.md and in each script's
# header, the GCE side in bench/gce/README.md, the environment (lanes, target
# volumes) in CLAUDE.md.
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
#
# Variables named on the command line (`make quick OLD=HEAD~1`) reach the
# harness scripts as environment variables: each harness target forwards the
# list of variables its script reads (the *_VARS lists below, `env_flags`).

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

# CHALLOC_CONF, when set on the make command line or in the environment, is
# forwarded into every container: challoc reads it after MALLOC_CONF and the
# stock allocators ignore it, so `make <target> CHALLOC_CONF=percpu_arena:percpu`
# puts challoc alone under a key in any suite.
# CGROUP_PEAK=1 (plan Phase 2.5 task 17): the container joins the host's
# cgroup namespace and gets /sys/fs/cgroup mounted read-write, so the soak
# harnesses can reset the cgroup's memory.peak at their phase boundaries
# (bench/lib.sh cgroup_peak_*; 6.12+ accepts the write, older kernels
# refuse it with EINVAL and the harness says so). Docker's default mounts
# the tree read-only and the nsdelegate boundary refuses the write from
# inside the namespace, hence both flags. Memory limits still apply.
DOCKER_BASE = docker run --rm --platform $(PLATFORM) \
	-v "$(CURDIR)":/src -w /src \
	-v $(TARGET_VOLUME):/src/target \
	-v $(CARGO_VOLUME):/root/.cargo/registry \
	$(GIT_MOUNT) $(if $(CHALLOC_CONF),-e CHALLOC_CONF="$(CHALLOC_CONF)") \
	$(if $(CGROUP_PEAK),--cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw)
DOCKER_RUN = $(DOCKER_BASE) $(DOCKER_FLAGS) $(IMAGE)
# cgroup_run LIMIT,ARGS: one memory-limited container running
# scripts/cgroup-tests.sh ARGS (Phase 2 tasks 6 and 7). Swap is disabled
# (`--memory-swap` equal to `--memory`): Docker Desktop's VM has 1 GiB of
# swap and `--memory` alone lets a churn spill into it instead of being
# charged against the limit.
cgroup_docker = $(DOCKER_BASE) --memory=$(1) --memory-swap=$(1) $(if $(CGROUP_WATCHDOG),-e WATCHDOG=$(CGROUP_WATCHDOG)) $(IMAGE) sh scripts/cgroup-tests.sh
cgroup_run = $(call cgroup_docker,$(1)) $(2)

# env_flags VARS: `-e VAR="value"` for every variable of VARS that is set (on
# the make command line or in the environment); the unset ones contribute
# nothing.
env_flags = $(foreach v,$(1),$(if $($(v)),-e $(v)="$($(v))"))
# bench_target NAME,SCRIPT,VARS[,EXTRA]: a harness target. HOST_CPU (the
# container only sees an implementer id), EXTRA and the set variables of
# VARS are forwarded and `sh SCRIPT` runs in the container. Instantiated
# with `$(eval $(call bench_target,...))`; the `##` comment on that line is
# the target's `make help` text.
define bench_target
$(1): DOCKER_FLAGS = -e HOST_CPU="$$(HOST_CPU)" $(if $(4),$(4) )$$(call env_flags,$(3))
$(1): image
	$$(DOCKER_RUN) sh $(2)
endef
# The host CPU for the results files' headers, probed only where a target
# reads it (a recursively expanded variable: `make build` runs no probe).
HOST_CPU = $(shell sysctl -n machdep.cpu.brand_string 2>/dev/null || grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' | grep . || lscpu 2>/dev/null | grep -m1 'Model name' | sed 's/.*: *//')

.PHONY: image dev run build test test-cgroup-limit test-cgroup-churn test-cgroup-lazy test-cgroup-oscillate test-cgroup test-cgroup-compare test-pagemap-guard test-host jemalloc-compat fuzz bench-build quick ab baseline scorecard valkey valkey-soak soak-quick soak-baseline ruby ruby-bench ruby-soak python-bench node-bench check-needed clean-volumes jvm-bench postgres-bench jvm-soak help print-docker-run test-all

help: ## list the targets (this text; the `##` comments in the Makefile)
	@awk -F' ## ' '/ ## / { n = $$1; if (n ~ /^\$$\(eval/) { sub(/^[^,]*,/, "", n); sub(/,.*/, "", n) } else { sub(/:.*/, "", n) } if (n !~ /^[a-zA-Z0-9_-]+$$/) next; printf "  %-22s %s\n", n, $$2 }' $(MAKEFILE_LIST)
	@echo
	@echo "PLATFORM=linux/amd64 on any target runs the emulated x86_64 container (correctness only)."
	@echo "Harness variables: bench/README.md and the *_VARS lists in the Makefile."

image: ## build the dev container image (Dockerfile.dev; IMAGE, PLATFORM)
	docker build --platform $(PLATFORM) -f Dockerfile.dev -t $(IMAGE) .

dev: DOCKER_FLAGS = -it
dev: image ## interactive shell in the container (native arch)
	$(DOCKER_RUN) sh

run: image ## one shell command in the container: CMD='...' (escape $ as $$)
	$(DOCKER_RUN) sh -c '$(CMD)'

build: image ## cargo build --release in the container
	$(DOCKER_RUN) cargo build --release

print-docker-run: ## print the docker run line the targets use (challoc-bench perf-stat takes one)
	@echo '$(DOCKER_RUN)'

# The `api::` tests run in a second process: they initialise the real
# allocator, whose reservations plus a second full layout (the layout tests)
# exceed what Rosetta can track under emulation.
# CHALLOC_TEST_ALLOW_SKIP=1 lets a kernel- or privilege-gated test print its
# `SKIP <test> <reason>` line and pass (docs/testing.md). `make test` sets it
# everywhere, the reference VMs included (checked 2026-09-20: their
# containers still skip mseal (Docker's seccomp), the cgroup writes, the
# rseq slice extension and mTHP); `make test TEST_ALLOW_SKIP=` runs strict,
# and `grep '^SKIP '` on a log lists what a run did not check.
TEST_ALLOW_SKIP ?= 1
test: DOCKER_FLAGS = $(if $(TEST_ALLOW_SKIP),-e CHALLOC_TEST_ALLOW_SKIP=$(TEST_ALLOW_SKIP))
test: image ## release build, the jemalloc-compat stage, both cargo test groups, then the cgroup and pagemap-guard steps
	$(DOCKER_RUN) sh -c 'cargo build --release \
	  && sh scripts/jemalloc-compat.sh \
	  && cargo test --features std -- --nocapture --skip api:: \
	  && cargo test --features std api:: -- --nocapture'
	$(MAKE) --no-print-directory test-cgroup-limit
	$(MAKE) --no-print-directory test-cgroup-churn
	$(MAKE) --no-print-directory test-cgroup-lazy
	$(MAKE) --no-print-directory test-pagemap-guard

# Phase 2 task 4: the same tests that read the cgroup, in a container with
# a 512 MiB limit; the effective `memory.max` must read as that number.
# Single-threaded since task 5: tests/c/cgroup_policy.c fills the cgroup to
# 94% of the limit, which a concurrent test's 64 MiB would push over it.
test-cgroup-limit: DOCKER_FLAGS = --memory=512m -e CHALLOC_EXPECT_CGROUP_LIMIT=536870912 $(if $(TEST_ALLOW_SKIP),-e CHALLOC_TEST_ALLOW_SKIP=$(TEST_ALLOW_SKIP))
test-cgroup-limit: image ## the cgroup-reading tests under --memory=512m (in `make test`)
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
test-cgroup-churn: image ## Phase 2 task 6 (a): the 400 MiB churn under --memory=512m and 256m, policy on and off (in `make test`)
	$(DOCKER_RUN) cargo build --release
	$(call cgroup_run,512m,churn challoc purge 400 100)
	$(call cgroup_run,512m,churn challoc-off keep 400 100)
	$(call cgroup_run,256m,churn challoc purge 200 50)
	$(call cgroup_run,256m,churn challoc-off keep 200 50)

test-cgroup-oscillate: image ## Phase 2 task 6 (b), opt-in: 200..480 MiB live in 512 MiB for OSC_SECS (300) s; CONTROL=1 adds the cgroup:false run
	$(DOCKER_RUN) cargo build --release
	$(call cgroup_run,512m,oscillate challoc $(OSC_SECS))
ifneq ($(CONTROL),)
	-$(call cgroup_run,512m,oscillate challoc-off $(OSC_SECS))
endif

# Phase 2.5 task 10 (about 20 s, in `make test`): tests/c/cgroup_lazy.c
# allocates 200 MiB, frees it and idles under --memory=512m, about 50% of
# the limit, the policy's headroom band: the purge must be MADV_FREE
# (`stats.cgroup.lazy_purges` rises, RSS and memory.current stay,
# memory.stat pglazyfree grows) and the pages must be reclaimable (a 380 MiB
# set of a fresh class fits, pglazyfreed rises); with MALLOC_CONF=
# lazy_band:false (`eager`, the control) the same run purges with
# MADV_DONTNEED and RSS falls under a quarter of the churn within 2 s.
test-cgroup-lazy: image ## Phase 2.5 task 10: the two-advice purge under --memory=512m at ~50% usage and its lazy_band:false control (in `make test`)
	$(DOCKER_RUN) cargo build --release
	$(call cgroup_run,512m,lazy challoc lazy 200)
	$(call cgroup_run,512m,lazy challoc-eager eager 200)

test-cgroup: test-cgroup-churn test-cgroup-lazy test-cgroup-oscillate ## the three cgroup tests above

test-all: test fuzz test-cgroup-oscillate check-needed ## everything that runs locally: test, the 10M fuzz, the oscillation soak and the DT_NEEDED check (docs/testing.md)

# Phase 2.5 task 17 (a few seconds, in `make test`): tests/c/pagemap_guard.c
# through scripts/pagemap-guard-test.sh, both ways. Under the default
# configuration the guard slabs of four classes are located as the gaps no
# allocation came from and checked through ioctl(PAGEMAP_SCAN) on
# /proc/self/pagemap without touching them: PAGE_IS_GUARD on the guard page
# and nowhere else (6.16+), or the fallback the program names (6.13..6.15:
# not present, not swapped; before 6.13: no guard slab exists; before 6.7:
# gap check only). Under MALLOC_CONF=guards:false no gap and no guard page.
# A separate target rather than a src/api/ctests.rs driver because lane D
# edits nothing under src/; the coordinator may fold it into the cargo test
# set with one `run_c("pagemap_guard", &["on"])` driver.
test-pagemap-guard: image ## Phase 2.5 task 17: the guard slabs seen through PAGEMAP_SCAN, both ways, never touched (in `make test`)
	$(DOCKER_RUN) sh -c 'cargo build --release && sh scripts/pagemap-guard-test.sh'

# Phase 2 task 7: the same runs under other allocators (CGROUP_ALLOCS,
# default jemalloc, mimalloc and tcmalloc; needs `make bench-build` for mimalloc),
# `report` mode for the churn (no criterion) and the oscillation for
# OSC_SECS; every outcome is printed (OUTCOME lines) and none fails the
# target, the results file records them.
CGROUP_ALLOCS ?= jemalloc mimalloc tcmalloc
test-cgroup-compare: image ## Phase 2 task 7: the cgroup runs under CGROUP_ALLOCS (jemalloc mimalloc tcmalloc), recorded, no criterion
	@for a in $(CGROUP_ALLOCS); do \
	  $(call cgroup_docker,512m) churn $$a report 400 100; \
	  $(call cgroup_docker,256m) churn $$a report 200 50; \
	  $(call cgroup_docker,512m) oscillate $$a $(OSC_SECS); \
	done; true

test-host: ## pure-logic unit tests on the host (cargo test --features std)
	cargo test --features std

jemalloc-compat: DOCKER_FLAGS = $(if $(QUANTUM),-e QUANTUM=$(QUANTUM)) $(if $(PREFIX),-e PREFIX=$(PREFIX))
jemalloc-compat: image ## build with the jemalloc-soname feature and stage libjemalloc.so.2 under target/jemalloc-compat/stage (QUANTUM=8|16, PREFIX=<dir>)
	$(DOCKER_RUN) sh scripts/jemalloc-compat.sh

FUZZ_OPS     ?= 10000000
FUZZ_THREADS ?= 4
FUZZ_SEED    ?= 0xC4A110C
FUZZ_TABLE   ?= 2048
fuzz: image ## fuzz/fuzz.c under LD_PRELOAD for FUZZ_OPS (10M) operations on FUZZ_THREADS (4) threads (FUZZ_SEED; `random` draws one)
	$(DOCKER_RUN) sh -c 'cargo build --release \
	  && sh scripts/ctest-cc.sh fuzz/fuzz.c -o target/fuzz \
	  && LD_PRELOAD=/src/target/release/libchalloc.so \
	     ./target/fuzz $(FUZZ_OPS) $(FUZZ_THREADS) $(FUZZ_SEED) $(FUZZ_TABLE)'

bench-build: image ## fetch and build the mimalloc-bench programs, redis, mimalloc and the bench/ helpers into the target volume
	$(DOCKER_RUN) sh bench/build.sh

# Two-tier harness (bench/README.md). quick: ~2 min go/no-go for an
# iteration; ab: the protocol-tier A/B that results files are written from;
# baseline: cache the stock allocators' protocol rows for BASELINES=cached.
QUICK_VARS = OLD NEW OLD_CONF NEW_CONF QUICK_TOLERANCE QUICK_BENCHES CHURN_SECS CHURN_CLASS CHURN_MAX_FRACTION BASELINES BASELINE_TOLERANCE QUICK_SANITY PERF PERF_BENCHES QUICK_BUILD_ONLY VALKEY_MODEL_SECS VALKEY_MODEL_N PIN CHALLOC_THP_COUNTERS
$(eval $(call bench_target,quick,bench/quick.sh,$$(QUICK_VARS))) ## fast tier (bench/quick.sh, ~2.5 min): the working tree vs OLD (HEAD), one round, exit 1 past QUICK_TOLERANCE (5)%; PERF=1 adds per-op counters where perf exists

AB_VARS = VARIANTS ROUNDS BENCHES ADAPTIVE BASELINES BASELINE_TOLERANCE RESULT HOST_KIND TITLE VALKEY_MODEL_SECS VALKEY_MODEL_N PIN CHALLOC_THP_COUNTERS
$(eval $(call bench_target,ab,bench/ab.sh,$$(AB_VARS))) ## protocol tier A/B (bench/ab.sh): VARIANTS='old=HEAD~1 new=HEAD jemalloc' ROUNDS=5 plus warm-up, interleaved; RESULT=path, ADAPTIVE=1, BASELINES=cached

BASELINE_VARS = ROUNDS BENCHES ALLOCS HOST_KIND VALKEY_MODEL_SECS
$(eval $(call bench_target,baseline,bench/baseline.sh,$$(BASELINE_VARS))) ## cache the stock allocators' protocol rows for this machine under target/baselines/ (bench/baseline.sh)

# Scorecard (bench/scorecard.sh): the fixed row set under challoc (REV, default
# the working tree) and jemalloc, 3 interleaved rounds through bench/ab.sh,
# one line appended to bench/results/scorecard-<arch>.md; `make scorecard
# RENDER=1` prints the table sorted by date.
SCORECARD_VARS = REV ROUNDS ROWS BASELINES BASELINE_TOLERANCE HOST_KIND VALKEY_MODEL_SECS VALKEY_MODEL_N PIN SCORECARD_FILE
scorecard: DOCKER_FLAGS = -e HOST_CPU="$(HOST_CPU)" $(call env_flags,$(SCORECARD_VARS))
scorecard: image ## the fixed row set vs jemalloc (REV=tree), one line appended to bench/results/scorecard-<arch>.md; RENDER=1 prints the trend on the host
	$(if $(RENDER),sh bench/scorecard.sh --render,$(DOCKER_RUN) sh bench/scorecard.sh)

valkey: DOCKER_FLAGS = $(if $(STAGE),-e STAGE=$(STAGE)) $(if $(TEST_FLAGS),-e TEST_FLAGS="$(TEST_FLAGS)")
valkey: image ## build challoc (quantum8) and Valkey at Wolfi's tag with contrib/valkey/0001-*.patch, then its tests (STAGE=build|memefficiency|full|all, TEST_FLAGS)
	$(DOCKER_RUN) sh contrib/valkey/build.sh

# CHALLOC_CONF is not in the list: DOCKER_BASE forwards it into every container.
SOAK_VARS = ALLOCS DURATION SETTLE INTERVAL CHUNK VB_REQS KEYS SIZE MT_SIZES DEL_RATIO KEY_PATTERN DEFRAG IO_THREADS RESULT_DIR OCCUPANCY MS_INTERVAL BASELINES SAVE_BASELINE PORT COMMIT CHALLOC_LIB
$(eval $(call bench_target,valkey-soak,bench/valkey-soak.sh,$$(SOAK_VARS))) ## Valkey soak under challoc and its bundled jemalloc (bench/valkey-soak.sh; DURATION=1200, SETTLE, ALLOCS, DEFRAG, IO_THREADS, OCCUPANCY=1); needs `make valkey STAGE=build`

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
soak-quick: valkey-soak ## challoc-only 600 s soak with OCCUPANCY=1 against the cached jemalloc soak; results under target/valkey/soak-quick, not an acceptance run

soak-baseline: ALLOCS = jemalloc
soak-baseline: DURATION ?= 1200
soak-baseline: SETTLE ?= 60
soak-baseline: SAVE_BASELINE = 1
soak-baseline: RESULT_DIR ?= target/valkey/soak-baseline
soak-baseline: valkey-soak ## the 1200 s jemalloc soak that soak-quick compares against, cached under target/baselines/

RUBY_VARS = STAGE ALLOC TESTS TESTOPTS JOBS RUBY_TAG RUBY_COMMIT RUBY_CHERRY
ruby: DOCKER_FLAGS = $(call env_flags,$(RUBY_VARS))
ruby: image ## stage libjemalloc.so.2 (quantum16), build Ruby 3.4 --with-jemalloc against it and run test-all (contrib/ruby/build.sh; STAGE=build|test|all, ALLOC=glibc for the baseline build, TESTS, JOBS)
	$(DOCKER_RUN) sh contrib/ruby/build.sh

RUBY_BENCH_VARS = ALLOCS ROUNDS ROWS RESULT OUT HOST_KIND GC_ROUNDS GC_FORCED
$(eval $(call bench_target,ruby-bench,bench/ruby-bench.sh,$$(RUBY_BENCH_VARS))) ## Ruby rows under challoc, jemalloc, glibc, mimalloc, tcmalloc (bench/ruby-bench.sh; ROUNDS=5); needs both `make ruby STAGE=build` builds

RUBY_SOAK_VARS = ALLOCS DURATION INTERVAL GC_EVERY COMPACT_AT THREADS IDLE_THREADS SEED RESULT OUT HOST_KIND COMMIT CHALLOC_SRC VARIANTS TRIM
$(eval $(call bench_target,ruby-soak,bench/ruby-soak.sh,$$(RUBY_SOAK_VARS))) ## Ruby soak, a Sidekiq-shaped process for DURATION (600) s per allocator (bench/ruby-soak.sh); needs `make ruby STAGE=build`

# Python rows (bench/python-bench.sh, Phase 2.5 task 1): the stdlib workloads
# under bench/probes/python/ on the image's python3, each with PYTHONMALLOC
# unset and =malloc, under challoc (the working tree) and the stock allocators.
PYTHON_BENCH_VARS = ALLOCS ROUNDS WORKLOADS MODES RESULT OUT HOST_KIND REV
$(eval $(call bench_target,python-bench,bench/python-bench.sh,$$(PYTHON_BENCH_VARS))) ## Python 3.13 rows (bench/python-bench.sh): four stdlib workloads, PYTHONMALLOC unset and =malloc, five allocators

# Node.js rows (bench/node-bench.sh, Phase 3 task 5 (c)): node -e 0 startup,
# bench/node/json_churn.js and the bench/node/server.js http server under ab
# (both endpoints, DURATION s per allocator), under challoc (the working
# tree) and the stock allocators via LD_PRELOAD, the image's nodejs-22.
NODE_BENCH_VARS = ALLOCS ROUNDS ROWS RESULT OUT HOST_KIND REV DURATION CONNS PORT JSON_ITERS SAMPLE WARM_N OLD_SPACE
$(eval $(call bench_target,node-bench,bench/node-bench.sh,$$(NODE_BENCH_VARS))) ## Node.js 22 rows (bench/node-bench.sh): startup, JSON churn and an http server under ab for DURATION (300) s per allocator

# JVM rows (bench/jvm-bench.sh, Phase 3 task 5 (a)): `java -version` startup
# and the com.sun.net.httpserver server of bench/jvm/HttpBench.java under ab,
# on the image's openjdk-21, under challoc (the working tree), glibc, glibc
# with MALLOC_ARENA_MAX=2, glibc with the JVM's periodic trim and the stock
# allocators. jvm-soak is the same script with ROWS=soak (the mixed-size
# soak: a growing and shrinking pool, log-uniform direct-buffer sizes,
# alternating load, 20 minutes per allocator), written to
# bench/results/<date>-<arch>-jvm-soak.md with the Rss series CSVs beside it.
JVM_BENCH_VARS = ALLOCS ROUNDS ROWS DURATION WARMUP INTERVAL CONNECTIONS THREADS HEAP PORT RESULT OUT HOST_KIND REV CHALLOC_VARIANTS JAVA_OPTS IDLE_SECS NMT NMT_GAP TRIM_MS SOAK_DURATION SOAK_WINDOW SOAK_HIGH SOAK_LOW SOAK_CORE SOAK_MAX SOAK_KEEPALIVE SOAK_MIN SOAK_MAXB SOAK_TAIL
JVM_SOAK_VARS = $(filter-out ROWS,$(JVM_BENCH_VARS))
$(eval $(call bench_target,jvm-bench,bench/jvm-bench.sh,$$(JVM_BENCH_VARS))) ## JVM rows (bench/jvm-bench.sh): `java -version` startup and a 200-thread http server under ab for DURATION (300) s with NMT, seven variants
$(eval $(call bench_target,jvm-soak,bench/jvm-bench.sh,$$(JVM_SOAK_VARS),-e ROWS=soak)) ## the JVM mixed-size soak (bench/jvm-bench.sh ROWS=soak): 16..400 threads, 256 B..1 MiB direct buffers, SOAK_DURATION (1200) s per allocator

# PostgreSQL rows (bench/postgres-bench.sh, Phase 3 task 5b): Wolfi's
# postgresql-17 from the image, the server run as a non-root user the script
# creates, the allocator in the postmaster's environment, pgbench rows.
POSTGRES_BENCH_VARS = ALLOCS ROUNDS ROWS RESULT OUT HOST_KIND REV SCALE DURATION CONN_DURATION INTERVAL SAMPLING_RATE JOBS CLIENTS_A CLIENTS_B CONN_CLIENTS CONN_JOBS SHARED_BUFFERS MAX_CONNECTIONS REINIT REPORT_ONLY
$(eval $(call bench_target,postgres-bench,bench/postgres-bench.sh,$$(POSTGRES_BENCH_VARS))) ## PostgreSQL 17 rows (bench/postgres-bench.sh): pgbench select-only at 100 and 500 clients, TPC-B, connection setup; RSS over every postgres process

# Phase 3 prerequisite (e): DT_NEEDED must name the allocator library
# (design 13.6); Wolfi's --as-needed drops it from a consumer that
# references no symbol of it.
check-needed: image ## Phase 3 prerequisite (e): DT_NEEDED must name the allocator library (tests/as-needed.sh, scripts/check-needed.sh)
	$(DOCKER_RUN) sh -c 'cargo build --release && sh tests/as-needed.sh'

clean-volumes: ## remove the target and cargo-registry volumes TARGET_VOLUME and CARGO_VOLUME name
	docker volume rm -f $(TARGET_VOLUME) $(CARGO_VOLUME)
