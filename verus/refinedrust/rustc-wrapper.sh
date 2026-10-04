#!/bin/sh
# RUSTC_WRAPPER for `cargo refinedrust` (run.sh): challoc's build script
# belongs to the primary package, which `refinedrust-rustc` translates and
# then stops without emitting a binary; `be_rustc` makes it compile the
# build script first (it still writes an empty translation, which run.sh
# ignores).
case " $* " in
*" --crate-name build_script_build "*) RR_BE_RUSTC=true exec "$@" ;;
esac
exec "$@"
