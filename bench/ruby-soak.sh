#!/bin/sh
# Moved to bench/drivers/ruby-soak.sh (bench/README.md, "Drivers"); this entry
# point stays for the Makefile targets and bench/gce/challoc-bench.
exec sh "$(dirname "$0")/drivers/ruby-soak.sh" "$@"
