#!/bin/sh
cd /tmp/w/m
OUT=/tmp/w/m/results.jsonl
L="--lib /tmp/w/m/lib"
EC="cow_epoch:true,exact_sized_free:true"
for rep in 1 2; do
 for cell in "64 10000000 1000000 -10" "1024 2000000 200000 -2"; do
  set -- $cell; v=$1 k=$2 w=$3 d=$4
  A="--vsize $v --keys $k --writes $w --save-delay $d --seed $rep"
  python3 cowbench.py --server vje/src/valkey-server --label je $A >> $OUT
  python3 cowbench.py --server vjef/src/valkey-server --label jef $A >> $OUT
  python3 cowbench.py --server vep/src/valkey-server $L --label ch $A >> $OUT
  python3 cowbench.py --server vepf/src/valkey-server $L --label chf $A >> $OUT
  python3 cowbench.py --server vep/src/valkey-server $L --malloc-conf $EC --label epx $A >> $OUT
  python3 cowbench.py --server vepf/src/valkey-server $L --malloc-conf $EC --label epxf $A >> $OUT
 done
done
echo done
