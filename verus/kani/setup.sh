#!/bin/sh
# Install the Kani version the harnesses were checked with (0.68.0) and its
# toolchain. `cargo kani setup` downloads the release bundle with curl; on
# hosts without curl, or behind a TLS proxy curl does not trust, pass a
# bundle downloaded by other means: `sh verus/kani/setup.sh <bundle.tar.gz>`
# (https://github.com/model-checking/kani/releases/download/kani-0.68.0/
# kani-0.68.0-x86_64-unknown-linux-gnu.tar.gz).
set -eu
cargo install --locked kani-verifier --version 0.68.0
if [ $# -gt 0 ]; then
	cargo kani setup --use-local-bundle "$1"
else
	cargo kani setup
fi
cargo kani --version
