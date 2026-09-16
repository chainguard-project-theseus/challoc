# challoc build entry points. The library is Linux-only; every target except
# `test-host` runs inside the Wolfi dev container built from Dockerfile.dev.
#
#   make dev                 interactive shell in the container (native arch)
#   make dev PLATFORM=linux/amd64
#                            same, emulated x86_64 (correctness only)
#   make run CMD='...'       run one shell command in the container
#   make build               cargo build --release in the container
#   make test-host           pure-logic unit tests on the host
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

.PHONY: image dev run build test-host clean-volumes

image:
	docker build --platform $(PLATFORM) -f Dockerfile.dev -t $(IMAGE) .

dev: DOCKER_FLAGS = -it
dev: image
	$(DOCKER_RUN) sh

run: image
	$(DOCKER_RUN) sh -c '$(CMD)'

build: image
	$(DOCKER_RUN) cargo build --release

test-host:
	cargo test --features std

clean-volumes:
	docker volume rm -f challoc-target-$(ARCH) challoc-cargo-registry
