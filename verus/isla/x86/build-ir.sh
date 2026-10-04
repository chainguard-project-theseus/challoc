#!/bin/sh
# Build the x86 model for Isla (verus/ISLA.md, "The x86_64 model"):
#
#   verus/isla/x86/build-ir.sh SAIL_X86_CHECKOUT OUT
#
# writes OUT.ir from a sail-x86-from-acl2 checkout's model/ (the Sail
# translation of the ACL2 x86isa model) with the checkout's
# test-generation patches and this directory's Isla entry: isla_regs.sail
# first, isla_rb.sail spliced over the model's rb, isla_entry.sail last.
# Needs `sail` with the isla-sail plugin built (Isla's isla-sail/,
# `make`), found as ISLA_SAIL_PLUGIN (default
# $ISLA/isla-sail/plugin.cmxs). About 15 seconds.
set -eu
[ $# -eq 2 ] || { echo "usage: $0 SAIL_X86_CHECKOUT OUT" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
out=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
plugin=${ISLA_SAIL_PLUGIN:-${ISLA:?set ISLA or ISLA_SAIL_PLUGIN}/isla-sail/plugin.cmxs}
cd "$1/model"
splices=
for f in ../test-generation-patches/*.sail "$here/isla_rb.sail"; do
	splices="$splices -splice $f"
done
# shellcheck disable=SC2086
exec sail -plugin "$plugin" -isla $splices -o "$out" \
	"$here/isla_regs.sail" prelude.sail register_types.sail registers.sail \
	register_accessors.sail opcode_ext.sail memory_accessors.sail init.sail \
	config.sail logging.sail step.sail main.sail "$here/isla_entry.sail"
