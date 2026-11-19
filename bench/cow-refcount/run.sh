#!/bin/sh
# $cell and $A split into arguments on purpose.
# shellcheck disable=SC2086
cd /tmp/w/m || exit 1
OUT=/tmp/w/m/results.jsonl
L=/tmp/w/m/lib
EC="cow_epoch:true,exact_sized_free:true"
for rep in 1 2; do
 for cell in "64 10000000 1000000 -10" "1024 2000000 200000 -2"; do
  set -- $cell; v=$1 k=$2 w=$3 d=$4
  A="--vsize $v --keys $k --writes $w --save-delay $d --seed $rep"
  python3 cowbench.py --server vje/src/valkey-server --label je $A
  python3 cowbench.py --server vjef/src/valkey-server --label jef $A
  python3 cowbench.py --server vep/src/valkey-server --lib "$L" --label ch $A
  python3 cowbench.py --server vepf/src/valkey-server --lib "$L" --label chf $A
  python3 cowbench.py --server vep/src/valkey-server --lib "$L" --malloc-conf "$EC" --label epx $A
  python3 cowbench.py --server vepf/src/valkey-server --lib "$L" --malloc-conf "$EC" --label epxf $A
 done
done >>"$OUT"
echo "done"
