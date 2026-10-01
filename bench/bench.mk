# bench/bench.mk: the bench/cb targets (bench/README.md, "cb"), included
# from the Makefile. They reuse its DOCKER_RUN (the dev container, the
# per-architecture target volume, target/results bind-mounted from the
# host) and `image`.
#
#   make bench-quick [BASE=main] [HEAD=HEAD] [CB_ARGS='--blocks 4']
#   make bench-aa    [BASE=HEAD]
#   make bench-smoke
#   make bench-local SUITE=<suite> [CB_ARGS=...]
#   make bench-compare DIRS='target/results/cb/<run> ...'   (on the host)
#   make bench-unit                                          (on the host)
#
# The GCE fan-out (`cb launch`, on the host: python3 >= 3.11 and gcloud
# authenticated for the project in bench/cells [defaults]):
#
#   make quick | bench-launch-quick [BASE=main] [HEAD=HEAD] [CB_ARGS=...]  cb launch quick on the gate cells
#   make bench-launch SUITE=<suite> [BASE=] [HEAD=] [CB_ARGS=...]          full, apps, soak, aa, sidecar, exp-*
#   make bench-image  [ARCHES='arm64 amd64']   build, push, pin bench/internal-image.lock
#   make bench-gc     [CB_ARGS='--run <run>']   delete one run's VMs, or stale cb- VMs
#
# Results land in target/results/cb/<run>/ (readable on the host). Local
# numbers are `indicative`: plumbing and a first look, never a verdict of
# record (those come from the GCE cells, `cb launch`, M1b).

BASE ?= main
HEAD ?= HEAD
SUITE ?= quick
CB_ARGS ?=
CB = python3 bench/cb

# The image id goes into the comparability key; the host-side preflight
# (load average, foreign containers) only warns.
cb_preflight = @$(CB) preflight 2>/dev/null || true
cb_run = $(DOCKER_RUN) $(CB) run $(1) --base "$(BASE)" --head "$(HEAD)" $(CB_ARGS)
bench-quick bench-aa bench-smoke bench-local: DOCKER_FLAGS = -e CB_IMAGE="$$(docker image inspect -f '{{.Id}}' $(IMAGE) 2>/dev/null | cut -c1-19)" -e HOST_CPU="$(HOST_CPU)" -e CB_HOST="$(shell hostname -s)"

.PHONY: quick bench-quick bench-aa bench-smoke bench-local bench-compare bench-unit bench-launch-quick bench-launch bench-image bench-gc

bench-quick: image ## cb quick suite in the container: HEAD vs BASE (main), ABBA blocks, CI-backed verdicts (bench/README.md, "cb")
	$(cb_preflight)
	$(call cb_run,quick)

bench-aa: image ## cb A/A: BASE against itself (calibration; every row should be same)
	$(cb_preflight)
	$(call cb_run,aa)

bench-smoke: image ## cb smoke: every quick row once under HEAD, parse only
	$(cb_preflight)
	$(call cb_run,smoke)

bench-local: image ## cb run SUITE (quick) in the container with CB_ARGS
	$(cb_preflight)
	$(call cb_run,$(SUITE))

bench-compare: ## cb compare DIRS on the host: verdicts from existing run directories
	$(CB) compare $(DIRS) $(CB_ARGS)

bench-unit: ## the bench/cb unit tests (stats, planner, parsers) on the host
	cd bench && python3 -m unittest discover -s cb/tests -t .

bench-launch-quick: ## cb launch quick: HEAD vs BASE (main) on the gate cells (spot VMs, one per shard and replicate), about 5-7 min from cold
	$(CB) launch quick --base "$(BASE)" --head "$(HEAD)" $(CB_ARGS)

quick: bench-launch-quick ## the go/no-go for a change: cb launch quick (bench-launch-quick)

bench-launch: ## cb launch SUITE on GCE with CB_ARGS (e.g. SUITE=aa CB_ARGS='--replicas 8')
	$(CB) launch $(SUITE) --base "$(BASE)" --head "$(HEAD)" $(CB_ARGS)

ARCHES ?= arm64 amd64
bench-image: ## build the cb bench image (bench/Dockerfile.cb) for ARCHES, push it, pin the digests in bench/internal-image.lock
	sh bench/cb/image.sh $(ARCHES)

bench-gc: ## delete cb VMs: CB_ARGS='--run <run>' for one run's; without it only stopped ones and ones past their max-run-duration
	$(CB) gc $(CB_ARGS)
