#!/bin/sh
# $cell and $A split into arguments on purpose.
# shellcheck disable=SC2086
cd /tmp/w/m || exit 1
OUT=/tmp/w/m/results-del.jsonl
L=/tmp/w/m/lib
EC="cow_epoch:true,exact_sized_free:true"
for rep in 1 2; do
  A="--vsize 64 --keys 10000000 --writes 1000000 --save-delay -10 --seed $rep --op DEL"
  python3 cowbench.py --server vje/src/valkey-server --label je $A
  python3 cowbench.py --server vjef/src/valkey-server --label jef $A
  python3 cowbench.py --server vep/src/valkey-server --lib "$L" --label ch $A
  python3 cowbench.py --server vepf/src/valkey-server --lib "$L" --label chf $A
  python3 cowbench.py --server vep/src/valkey-server --lib "$L" --malloc-conf "$EC" --label epx $A
  python3 cowbench.py --server vepf/src/valkey-server --lib "$L" --malloc-conf "$EC" --label epxf $A
done >>"$OUT"
echo "done"
