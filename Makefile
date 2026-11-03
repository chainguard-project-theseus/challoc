# challoc build entry points. The library is Linux-only; every target except
# `help`, `verus`, `isla`, `emptyasm` and the `clean-*` targets runs inside the
# Wolfi dev container built from Dockerfile.dev. `make help` lists the targets
# from the `##` comments below; the benchmark harness (bench/cb, its targets in
# bench/bench.mk) is described in bench/README.md, the environment (target
# volumes) in CONTRIBUTING.md.
#
# `target/` inside the container is a named Docker volume, one per
# architecture, so Linux artifacts never mix with host artifacts and builds do
# not go through the bind mount (slow where Docker runs in a VM). The cargo
# registry is a named volume too so crates are fetched once. `TARGET_VOLUME`
# (default `challoc-target-$(ARCH)`) names the target volume and `CARGO_VOLUME`
# (default `challoc-cargo-registry`) the registry volume; a second git
# worktree sets `TARGET_VOLUME=challoc-target-<name>-$(ARCH)` so two
# worktrees never share a `target/`. `make clean-volumes` removes the
# two volumes the variables name. The one exception is `target/results/`,
# where the harnesses write their results: it is the host's
# `target/results` bind-mounted over the volume, so a `cb run` in the
# container is readable on the host.

HOST_ARCH := $(shell uname -m | sed -e 's/x86_64/amd64/' -e 's/aarch64/arm64/')
PLATFORM  ?= linux/$(HOST_ARCH)
ARCH      := $(notdir $(PLATFORM))
# IMAGE_TARGET names a Dockerfile.dev stage to stop at (base, test,
# consumers; empty: the last stage, bench, which has everything) and goes
# into the default IMAGE tag, so a partial image never replaces the full one.
IMAGE_TARGET ?=
IMAGE     ?= challoc-dev$(if $(IMAGE_TARGET),-$(IMAGE_TARGET)):$(ARCH)
TARGET_VOLUME ?= challoc-target-$(ARCH)
CARGO_VOLUME  ?= challoc-cargo-registry
# IMAGE_BUILD empty: `image` does nothing and the targets use IMAGE as it is
# (CI with a prebuilt image).
IMAGE_BUILD   ?= 1

# QUANTUM (16 or 8) picks the size-class quantum feature of every cargo
# build and test below and of the jemalloc-compat stage.
QUANTUM       ?= 16
FEATURES       = --no-default-features --features quantum$(QUANTUM)

# In a git worktree `.git` is a file pointing at the main repository's
# `.git/worktrees/<name>`; mount that common dir at the same path so `git
# archive`/`rev-parse` work inside the container (`cb run` builds variants
# from git revs). A plain checkout needs no extra mount.
GIT_COMMON := $(shell git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
ifneq ($(GIT_COMMON),)
ifneq ($(GIT_COMMON),$(CURDIR)/.git)
GIT_MOUNT = -v "$(GIT_COMMON)":"$(GIT_COMMON)":ro
endif
endif

# CHALLOC_CONF, when set on the make command line or in the environment, is
# forwarded into every container: challoc reads it after MALLOC_CONF and the
# stock allocators ignore it, so `make <target> CHALLOC_CONF=zero_on_free:true`
# puts challoc alone under a key in any suite.
# CGROUP_PEAK=1: the container joins the host's
# cgroup namespace and gets /sys/fs/cgroup mounted read-write, so a soak
# driver run by hand (`make run CMD='sh bench/drivers/valkey-soak.sh'`) can
# reset the cgroup's memory.peak at its phase boundaries
# (bench/drivers/lib.sh cgroup_peak_*; 6.12+ accepts the write, older kernels
# refuse it with EINVAL and the driver says so). Docker's default mounts
# the tree read-only and the nsdelegate boundary refuses the write from
# inside the namespace, hence both flags. Memory limits still apply.
# EXTRA_DOCKER_FLAGS reaches every container, the memory-limited cgroup
# ones included, on top of a target's own DOCKER_FLAGS (which replace any
# DOCKER_FLAGS from the environment): `make test
# EXTRA_DOCKER_FLAGS=--privileged` runs the privilege-gated tests.
DOCKER_BASE = docker run --rm --platform $(PLATFORM) $(EXTRA_DOCKER_FLAGS) \
	-v "$(CURDIR)":/src -w /src \
	-v $(TARGET_VOLUME):/src/target \
	$(shell mkdir -p "$(CURDIR)/target/results")-v "$(CURDIR)/target/results":/src/target/results \
	-v $(CARGO_VOLUME):/root/.cargo/registry \
	$(GIT_MOUNT) $(if $(CHALLOC_CONF),-e CHALLOC_CONF="$(CHALLOC_CONF)") \
	$(if $(CGROUP_PEAK),--cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw)
DOCKER_RUN = $(DOCKER_BASE) $(DOCKER_FLAGS) $(IMAGE)
# cgroup_run LIMIT,ARGS: one memory-limited container running
# scripts/cgroup-tests.sh ARGS. Swap is disabled
# (`--memory-swap` equal to `--memory`): on a host with swap `--memory`
# alone lets a churn spill into it instead of being charged against the
# limit.
cgroup_docker = $(DOCKER_BASE) --memory=$(1) --memory-swap=$(1) $(if $(CGROUP_WATCHDOG),-e WATCHDOG=$(CGROUP_WATCHDOG)) $(IMAGE) sh scripts/cgroup-tests.sh
cgroup_run = $(call cgroup_docker,$(1)) $(2)

# env_flags VARS: `-e VAR="value"` for every variable of VARS that is set (on
# the make command line or in the environment); the unset ones contribute
# nothing.
env_flags = $(foreach v,$(1),$(if $($(v)),-e $(v)="$($(v))"))
# The host CPU for the cb host capture (the container only sees an
# implementer id), probed only where a target reads it (a recursively
# expanded variable: `make build` runs no probe).
HOST_CPU = $(shell sysctl -n machdep.cpu.brand_string 2>/dev/null || grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' | grep . || lscpu 2>/dev/null | grep -m1 'Model name' | sed 's/.*: *//')

.PHONY: image dev run build test test-cgroup-limit test-cgroup-churn test-cgroup-lazy test-cgroup-oscillate test-cgroup test-cgroup-compare jemalloc-compat install install-jemalloc-compat fuzz bench-build valkey ruby check-needed clean-volumes help print-docker-run test-all verus isla emptyasm

help: ## list the targets (this text; the `##` comments in the Makefile)
	@awk -F' ## ' '/ ## / { n = $$1; if (n ~ /^\$$\(eval/) { sub(/^[^,]*,/, "", n); sub(/,.*/, "", n) } else { sub(/:.*/, "", n) } if (n !~ /^[a-zA-Z0-9_-]+$$/) next; printf "  %-22s %s\n", n, $$2 }' $(MAKEFILE_LIST)
	@echo
	@echo "PLATFORM=linux/amd64 on any target runs the emulated x86_64 container (correctness only)."
	@echo "Benchmarks: bench/README.md (cb; make quick is cb launch quick)."

image: ## build the dev container image (Dockerfile.dev; IMAGE, IMAGE_TARGET, PLATFORM; IMAGE_BUILD= skips it)
	$(if $(IMAGE_BUILD),,@true )docker build --platform $(PLATFORM) -f Dockerfile.dev $(if $(IMAGE_TARGET),--target $(IMAGE_TARGET)) -t $(IMAGE) .

dev: DOCKER_FLAGS = -it
dev: image ## interactive shell in the container (native arch)
	$(DOCKER_RUN) sh

run: image ## one shell command in the container: CMD='...' (escape $ as $$)
	$(DOCKER_RUN) sh -c '$(CMD)'

build: image ## cargo build --release in the container (QUANTUM)
	$(DOCKER_RUN) cargo build --release $(FEATURES)

print-docker-run: ## print the docker run line the targets use
	@echo '$(DOCKER_RUN)'

# The `api::` tests run in a second process: they initialise the real
# allocator, whose reservations plus a second full layout (the layout tests)
# exceed what an x86_64 emulator can track (PLATFORM=linux/amd64 on arm64).
# CHALLOC_TEST_ALLOW_SKIP=1 lets a kernel- or privilege-gated test print its
# `SKIP <test> <reason>` line and pass (docs/testing.md). `make test` sets it
# everywhere, the reference VMs included (their containers skip mseal
# (Docker's seccomp), the cgroup writes and mTHP); `make test TEST_ALLOW_SKIP=` runs strict,
# and `grep '^SKIP '` on a log lists what a run did not check.
TEST_ALLOW_SKIP ?= 1
test: DOCKER_FLAGS = -e QUANTUM=$(QUANTUM) $(if $(TEST_ALLOW_SKIP),-e CHALLOC_TEST_ALLOW_SKIP=$(TEST_ALLOW_SKIP))
test: image ## release build, the jemalloc-compat stage, both cargo test groups, then the cgroup steps
	$(DOCKER_RUN) sh -c 'cargo build --release $(FEATURES) \
	  && sh scripts/jemalloc-compat.sh \
	  && cargo test $(FEATURES) -- --nocapture --skip api:: \
	  && cargo test $(FEATURES) api:: -- --nocapture'
	$(MAKE) --no-print-directory test-cgroup-limit
	$(MAKE) --no-print-directory test-cgroup-churn
	$(MAKE) --no-print-directory test-cgroup-lazy

# The same tests that read the cgroup, in a container with
# a 512 MiB limit; the effective `memory.max` must read as that number.
# Single-threaded: tests/c/cgroup_policy.c fills the cgroup to
# 94% of the limit, which a concurrent test's 64 MiB would push over it.
test-cgroup-limit: DOCKER_FLAGS = --memory=512m -e CHALLOC_EXPECT_CGROUP_LIMIT=536870912 $(if $(TEST_ALLOW_SKIP),-e CHALLOC_TEST_ALLOW_SKIP=$(TEST_ALLOW_SKIP))
test-cgroup-limit: image ## the cgroup-reading tests under --memory=512m (in `make test`)
	$(DOCKER_RUN) sh -c 'cargo build --release $(FEATURES) && cargo test $(FEATURES) cgroup -- --nocapture --test-threads=1'

# The memory-limited container tests, one container per
# run (scripts/cgroup-tests.sh; the exit status is the program's, 137 is an
# OOM kill). test-cgroup-churn (about 40 s, in `make test`): tests/c/
# cgroup_churn.c allocates 400 MiB, frees it and idles under --memory=512m;
# with the policy on (`purge`), held memory (RSS less LazyFree) must be
# under 100 MiB within 2 s and stats.cgroup.eager_purges must rise; with
# MALLOC_CONF=cgroup:false (`keep`, the control) RSS must stay at 100 MiB or
# more at 2 s; the same
# at --memory=256m with the sizes scaled to the limit (200 MiB, 50 MiB).
# test-cgroup-oscillate (OSC_SECS, default 300 s, plus 300 s more with
# CONTROL=1: the same run under cgroup:false, recorded, not a criterion):
# tests/c/cgroup_oscillate.c swings between 200 and 480 MiB live in 512 MiB
# and must not be OOM-killed. Opt-in because of its length: `make
# test-cgroup` runs both; `make test` runs only the churn.
OSC_SECS ?= 300
test-cgroup-churn: image ## the 400 MiB churn under --memory=512m and 256m, policy on and off (in `make test`)
	$(DOCKER_RUN) cargo build --release $(FEATURES)
	$(call cgroup_run,512m,churn challoc purge 400 100)
	$(call cgroup_run,512m,churn challoc-off keep 400 100)
	$(call cgroup_run,256m,churn challoc purge 200 50)
	$(call cgroup_run,256m,churn challoc-off keep 200 50)

test-cgroup-oscillate: image ## opt-in: 200..480 MiB live in 512 MiB for OSC_SECS (300) s; CONTROL=1 adds the cgroup:false run
	$(DOCKER_RUN) cargo build --release $(FEATURES)
	$(call cgroup_run,512m,oscillate challoc $(OSC_SECS))
ifneq ($(CONTROL),)
	-$(call cgroup_run,512m,oscillate challoc-off $(OSC_SECS))
endif

# test-cgroup-lazy (about 20 s, in `make test`): tests/c/cgroup_lazy.c
# allocates 200 MiB, frees it and idles under --memory=512m, about 50% of
# the limit, the policy's headroom band: the purge must be MADV_FREE
# (`stats.cgroup.lazy_purges` rises, RSS and memory.current stay,
# memory.stat pglazyfree grows) and the pages must be reclaimable (a 380 MiB
# set of a fresh class fits, pglazyfreed rises); with MALLOC_CONF=
# lazy_band:false (`eager`, the control) the same run purges with
# MADV_DONTNEED and RSS falls under a quarter of the churn within 2 s.
test-cgroup-lazy: image ## the two-advice purge under --memory=512m at ~50% usage and its lazy_band:false control (in `make test`)
	$(DOCKER_RUN) cargo build --release $(FEATURES)
	$(call cgroup_run,512m,lazy challoc lazy 200)
	$(call cgroup_run,512m,lazy challoc-eager eager 200)

test-cgroup: test-cgroup-churn test-cgroup-lazy test-cgroup-oscillate ## the three cgroup tests above

test-all: test fuzz test-cgroup-oscillate check-needed ## everything that runs locally: test, the 10M fuzz, the oscillation soak and the DT_NEEDED check (docs/testing.md)

# The same runs under other allocators (CGROUP_ALLOCS,
# default jemalloc, mimalloc and tcmalloc; needs `make bench-build` for mimalloc),
# `report` mode for the churn (no criterion) and the oscillation for
# OSC_SECS; every outcome is printed (OUTCOME lines) and none fails the
# target; the log is the record.
CGROUP_ALLOCS ?= jemalloc mimalloc tcmalloc
test-cgroup-compare: image ## the cgroup runs under CGROUP_ALLOCS (jemalloc mimalloc tcmalloc), recorded, no criterion
	@for a in $(CGROUP_ALLOCS); do \
	  $(call cgroup_docker,512m) churn $$a report 400 100; \
	  $(call cgroup_docker,256m) churn $$a report 200 50; \
	  $(call cgroup_docker,512m) oscillate $$a $(OSC_SECS); \
	done; true

jemalloc-compat: DOCKER_FLAGS = -e QUANTUM=$(QUANTUM) $(if $(PREFIX),-e PREFIX=$(PREFIX))
jemalloc-compat: image ## build with the jemalloc-soname feature and stage libjemalloc.so.2 under target/jemalloc-compat/stage (QUANTUM=8|16, PREFIX=<dir>)
	$(DOCKER_RUN) sh scripts/jemalloc-compat.sh

# install, install-jemalloc-compat: build in the container, then
# scripts/install.sh writes the file set under DESTDIR/PREFIX (README.md,
# "Installing"). The paths are the container's: /src is the repository, so
# the default DESTDIR (dist, relative to /src) lands in ./dist on the host;
# an absolute DESTDIR outside /src disappears with the container, and the
# script's listing is then the only record of what it wrote.
INSTALL_VARS = PREFIX LIBDIR INCLUDEDIR
DESTDIR ?= dist
install: DOCKER_FLAGS = -e DESTDIR="$(DESTDIR)" $(call env_flags,$(INSTALL_VARS))
install: image ## build, then install libchalloc.so.0, its headers and challoc.pc under DESTDIR (dist) + PREFIX (/usr/local); LIBDIR, INCLUDEDIR, QUANTUM
	$(DOCKER_RUN) sh -c 'cargo build --release $(FEATURES) && sh scripts/install.sh challoc'

install-jemalloc-compat: DOCKER_FLAGS = -e QUANTUM=$(QUANTUM) -e DESTDIR="$(DESTDIR)" $(call env_flags,$(INSTALL_VARS))
install-jemalloc-compat: image ## build with the jemalloc-soname feature, then install libjemalloc.so.2, jemalloc.h and jemalloc.pc as install does
	$(DOCKER_RUN) sh -c 'sh scripts/jemalloc-compat.sh && sh scripts/install.sh jemalloc-compat'

FUZZ_OPS     ?= 10000000
FUZZ_THREADS ?= 4
FUZZ_SEED    ?= 0xC4A110C
FUZZ_TABLE   ?= 2048
fuzz: image ## fuzz/fuzz.c under LD_PRELOAD for FUZZ_OPS (10M) operations on FUZZ_THREADS (4) threads (FUZZ_SEED; `random` draws one)
	$(DOCKER_RUN) sh -c 'cargo build --release $(FEATURES) \
	  && sh scripts/ctest-cc.sh fuzz/fuzz.c -o target/fuzz \
	  && LD_PRELOAD=/src/target/release/libchalloc.so \
	     ./target/fuzz $(FUZZ_OPS) $(FUZZ_THREADS) $(FUZZ_SEED) $(FUZZ_TABLE)'

bench-build: image ## build the bench programs (bench/build.sh) into the target volume; the bench image has them prebuilt
	$(DOCKER_RUN) sh bench/build.sh

valkey: DOCKER_FLAGS = $(if $(STAGE),-e STAGE=$(STAGE)) $(if $(TEST_FLAGS),-e TEST_FLAGS="$(TEST_FLAGS)")
valkey: image ## build challoc (quantum8) and Valkey at Wolfi's tag with contrib/valkey/000{1,2}-*.patch, then its tests (STAGE=build|memefficiency|full|all, TEST_FLAGS)
	$(DOCKER_RUN) sh tests/consumers/valkey-build.sh

RUBY_VARS = STAGE ALLOC TESTS TESTOPTS JOBS RUBY_TAG RUBY_COMMIT RUBY_CHERRY OPENSSL_TAG OPENSSL_COMMIT RUBY_GEMS_CA RUBY_TEST_EXCLUDE RUBY_TEST_EXCLUDE_EXTRA
ruby: DOCKER_FLAGS = $(call env_flags,$(RUBY_VARS))
ruby: image ## stage libjemalloc.so.2 (quantum16), build Ruby 3.4 --with-jemalloc against it and run test-all (tests/consumers/ruby-build.sh; STAGE=build|test|all, ALLOC=glibc for the baseline build, TESTS, JOBS)
	$(DOCKER_RUN) sh tests/consumers/ruby-build.sh

# DT_NEEDED must name the allocator library (design 13.6); Wolfi's --as-needed drops it from a consumer that
# references no symbol of it.
check-needed: image ## DT_NEEDED must name the allocator library (tests/as-needed.sh, scripts/check-needed.sh)
	$(DOCKER_RUN) sh -c 'cargo build --release $(FEATURES) && sh tests/as-needed.sh'

# verus runs where it is invoked, not in the dev container: it needs the
# Verus release and the rust toolchain verus/README.md names, which the
# verus check gets from the `verus` package (.chainguard/ci/verus.yaml).
# VERUS_BASE=<rev> also fails a verus/FLOOR figure lowered below that
# revision's without a reason (verus/README.md). The ledger under
# verus/ledger/ must match what `sh verus/seams.sh --write` generates
# from the tree.
verus: ## the seam ledger, the Verus proofs, then the coverage floor (verus/README.md, verus/FLOOR, verus/ledger; VERUS_BASE=<rev>)
	sh verus/seams.sh --check
	cargo verus verify --locked
	cd verus/count && cargo run --locked --release -- --floor ../FLOOR $(if $(VERUS_BASE),--base-rev $(VERUS_BASE)) ../..

# isla runs where it is invoked, not in the dev container: it needs the
# isla, isla-snapshots, sail, isla-sail, z3 and llvm packages
# (verus/ISLA.md). ISLA_TARGETS picks the targets (default both); a target
# other than the host's needs its rust standard library.
isla: ## Isla over the inline-assembly seams verus/isla/seams.md lists (verus/ISLA.md; ISLA_TARGETS, ISLA_X86_IR)
	sh verus/isla/run.sh

# emptyasm runs where it is invoked, not in the dev container: it needs
# cargo and rustc (rust-1.98), a C++ compiler for build.rs (build-base),
# llvm-objdump (llvm-21), and git for the negative test's copy of the
# tree. It checks the host's architecture (EMPTYASM_TARGETS; a target other
# than the host's needs its rust standard library) at QUANTUM, then runs
# the negative test.
emptyasm: ## the empty-template asm! blocks of verus/emptyasm/blocks.txt add no instruction, then its negative test (verus/emptyasm/run.sh; QUANTUM, EMPTYASM_TARGETS)
	EMPTYASM_CARGO_FLAGS='$(FEATURES)' sh verus/emptyasm/run.sh
	EMPTYASM_CARGO_FLAGS='$(FEATURES)' sh verus/emptyasm/run.sh -n

clean-volumes: ## remove the target and cargo-registry volumes TARGET_VOLUME and CARGO_VOLUME name
	docker volume rm -f $(TARGET_VOLUME) $(CARGO_VOLUME)

include bench/bench.mk
