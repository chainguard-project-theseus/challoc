#!/bin/sh
# Moved to bench/drivers/jvm-bench.sh (bench/README.md, "Drivers"); this entry
# point stays for the Makefile targets and bench/gce/challoc-bench.
exec sh "$(dirname "$0")/drivers/jvm-bench.sh" "$@"
