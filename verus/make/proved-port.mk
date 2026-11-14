# verus/make/proved-port.mk: the proof targets every proved port shares.
# kit/sync.sh vendors it, with kani.sh, headers.sh and templates/, from
# the proved-port kit's kit/make into a port's verus/make/, beside the
# counter it vendors into verus/count/; verus/count/KIT records the kit
# commit of both, and kit/sync.sh --check covers both. Do not edit the
# copy: change kit/make there, then re-sync.
#
# A port's Makefile sets the variables below that differ from their
# defaults, then includes this file, and runs make from its root:
#
#   VERUS_CRATES = pcre3-unicode pcre3-syntax pcre3 pcre3-capi
#   KANI_CRATES  = $(VERUS_CRATES)
#   include verus/make/proved-port.mk
#
# The targets run where make is invoked: the checks run them in the
# guest, and a development session runs them with the port's own
# wrapper (`make run CMD='make verus'`). A step the port runs before one
# of them is a prerequisite it adds in its Makefile, with no recipe
# (`verus: verus-scan`; challoc's `sh verus/seams.sh --check`).
#
# Targets:
#   verus             verus-proofs, then verus-count: the verus checks'
#                     command (.chainguard/ci/verus.yaml).
#   verus-proofs      cargo verus over VERUS_CRATES, Verus's statistics
#                     (--time-expanded --output-json) into VERUS_STATS.
#   verus-count       build the vendored counter, then run it over the
#                     port: the coverage floor (VERUS_FLOOR), with the
#                     rlimit headroom (verus/RLIMIT) from VERUS_STATS, and
#                     the comparison with VERUS_BASE, which verus and
#                     verus-count require (VERUS_BASE=none for none).
#   verus-count-test  the vendored counter's own tests.
#   verus-headers     headers.sh: the kit's Verus files against the kit
#                     templates of the commit verus/count/KIT records:
#                     the headers of verus/FLOOR, RLIMIT, REACH, MINTS and
#                     count.toml, of .chainguard/ci/verus.yaml, miri.yaml
#                     and .chainguard/reviewers.yaml, and the vendored-kit
#                     rule of .chainguard/review-prompt.md; VERUS_FLOOR's
#                     header is the verus/FLOOR template's.
#   kani              kani.sh: the Kani harnesses, all of them or a shard,
#                     failing on any failure and when none run, unless
#                     KANI_NONE = yes declares the port has none yet.
#   kani-list         kani.sh --list: the harnesses, one `CRATE NAME FILE`
#                     a line (or this shard's, with KANI_SHARDS).

# The directory this file is in (verus/make/), for its scripts.
PROVED_PORT_MAKE := $(dir $(lastword $(MAKEFILE_LIST)))

# --- Verus ---------------------------------------------------------------
# VERUS_CRATES: the packages cargo verus checks, each passed as -p. Empty
# checks cargo's default package selection (a one-crate port).
VERUS_CRATES ?=
# VERUS_MODE: `focus` checks VERUS_CRATES and not their dependencies;
# `verify` (challoc) checks every package cargo selects.
VERUS_MODE ?= focus
# VERUS_CARGO_FLAGS: further cargo flags (expat-rs: `--features verus`;
# challoc's second quantum: `--no-default-features --features quantum8`).
VERUS_CARGO_FLAGS ?=
# VERUS_THREADS: Verus's --num-threads; empty leaves Verus's default (the
# host's available parallelism). The verus checks pass CG_CPUS.
VERUS_THREADS ?=
# VERUS_FLAGS: further Verus flags, after the statistics flags.
VERUS_FLAGS ?=
# VERUS_STATS: where verus-proofs writes Verus's statistics and
# verus-count reads them.
VERUS_STATS ?= target/verus-stats.json
# VERUS_FLOOR: the ratchet file, the counter's --floor. It is the
# `[floor] path` of verus/count.toml too (chainz: verus/SHARES).
VERUS_FLOOR ?= verus/FLOOR
# VERUS_RLIMIT: non-empty passes --rlimit VERUS_STATS, which holds each
# verified function to verus/RLIMIT; empty leaves it out, for a port
# with no verus/RLIMIT yet.
VERUS_RLIMIT ?= yes
# VERUS_BASE: the revision verus/FLOOR, verus/RLIMIT and the counter's
# other ratchets compare with (--base-rev). The verus check passes the
# pull request's base and verus-main HEAD^. Empty fails verus and
# verus-count before they run anything, so a check that loses its base
# fails rather than skip the comparison. `none` runs the counter with
# --no-base, no comparison: for a local or development run
# (`make verus VERUS_BASE=none`) and the proof PR that adds the first
# FLOOR, never for a check once FLOOR is on the base.
VERUS_BASE ?=
# VERUS_COUNT_FLAGS: further counter flags (--list, --config FILE).
VERUS_COUNT_FLAGS ?=
# VERUS_COUNT_DIR: the vendored counter. VERUS_COUNT_TARGET: its cargo
# target directory, outside the copy so the copy stays byte-identical to
# the kit's.
VERUS_COUNT_DIR ?= verus/count
VERUS_COUNT_TARGET ?= target/verus-count

verus_proofs_cmd = cargo verus $(VERUS_MODE) --locked \
  $(addprefix -p ,$(VERUS_CRATES)) $(VERUS_CARGO_FLAGS) \
  -- --time-expanded --output-json \
  $(if $(VERUS_THREADS),--num-threads $(VERUS_THREADS)) $(VERUS_FLAGS) \
  > $(VERUS_STATS)
verus_count_build_cmd = cargo build --locked --release \
  --manifest-path $(VERUS_COUNT_DIR)/Cargo.toml --target-dir $(VERUS_COUNT_TARGET)
verus_count_cmd = $(VERUS_COUNT_TARGET)/release/verus-count --floor $(VERUS_FLOOR) \
  $(if $(VERUS_RLIMIT),--rlimit $(VERUS_STATS)) \
  $(if $(filter none,$(VERUS_BASE)),--no-base,--base-rev $(VERUS_BASE)) $(VERUS_COUNT_FLAGS) .
# Expanded with the recipe, so it fails `make verus` (and `make -n
# verus`) before the proofs run, and leaves other targets alone.
verus_base_missing = no VERUS_BASE to compare $(VERUS_FLOOR) with: give the revision (the verus check passes the pull request's base, verus-main HEAD^), or VERUS_BASE=none for a local run or the proof PR that adds the first FLOOR
verus_base_check = $(if $(VERUS_BASE),,$(error $(verus_base_missing)))

define verus_proofs_recipe
mkdir -p $(dir $(VERUS_STATS))
$(verus_proofs_cmd)
endef

define verus_count_recipe
$(verus_base_check)
$(verus_count_build_cmd)
$(verus_count_cmd)
endef

# --- Kani ----------------------------------------------------------------
# kani.sh reads these from its environment; its header says what each
# does. KANI_ARGS go to every cargo kani call. A port with no harnesses
# yet sets `KANI_NONE = yes` before the include, so `make kani` (and the
# kani checks) pass until its first harness, which must delete the line:
# kani.sh fails a run that finds harnesses under it, and an empty run
# without it.
KANI_NONE ?=
KANI_CRATES ?=
KANI_FLAGS ?=
KANI_LIST_FLAGS ?=
KANI_JOBS ?=
KANI_FORMAT ?=
KANI_CBMC_ARGS ?=
KANI_ONLY ?=
KANI_EXCLUDE ?=
KANI_HARNESS ?=
KANI_SHARD ?=
KANI_SHARDS ?=
KANI_WEIGHTS ?=
KANI_FAIL_PATTERN ?=
KANI_VERSION ?=
KANI_ARGS ?=
export KANI_CRATES KANI_FLAGS KANI_LIST_FLAGS KANI_JOBS KANI_FORMAT KANI_CBMC_ARGS \
  KANI_ONLY KANI_EXCLUDE KANI_HARNESS KANI_SHARD KANI_SHARDS KANI_WEIGHTS \
  KANI_FAIL_PATTERN KANI_VERSION KANI_NONE

.PHONY: verus verus-proofs verus-count verus-count-test verus-headers kani kani-list

verus: ## the Verus proofs, then the coverage floor and rlimit headroom (verus/make/proved-port.mk; VERUS_BASE=<rev> or none)
	$(verus_proofs_recipe)
	$(verus_count_recipe)

verus-proofs: ## cargo verus over VERUS_CRATES, statistics to VERUS_STATS
	$(verus_proofs_recipe)

verus-count: ## the vendored counter over the port, from VERUS_STATS (VERUS_BASE=<rev> or none)
	$(verus_count_recipe)

verus-count-test: ## the vendored counter's tests
	cargo test --locked --manifest-path $(VERUS_COUNT_DIR)/Cargo.toml --target-dir $(VERUS_COUNT_TARGET)

verus-headers: ## the kit's Verus files against the kit's templates (verus/make/headers.sh)
	VERUS_FLOOR=$(VERUS_FLOOR) sh $(PROVED_PORT_MAKE)headers.sh .

kani: ## the Kani harnesses, all or a shard (verus/make/kani.sh; KANI_SHARD/KANI_SHARDS; KANI_NONE=yes for none yet)
	sh $(PROVED_PORT_MAKE)kani.sh $(KANI_ARGS)

kani-list: ## the Kani harnesses, one CRATE NAME FILE a line
	sh $(PROVED_PORT_MAKE)kani.sh --list $(KANI_ARGS)
