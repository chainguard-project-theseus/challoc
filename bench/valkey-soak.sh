#!/bin/sh
# Moved to bench/drivers/valkey-soak.sh (bench/README.md, "Drivers"); this entry
# point stays for the Makefile targets and bench/gce/challoc-bench.
exec sh "$(dirname "$0")/drivers/valkey-soak.sh" "$@"
