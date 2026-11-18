#!/bin/sh
cd /tmp/w/m
OUT=/tmp/w/m/results-del.jsonl
L="--lib /tmp/w/m/lib"
EC="cow_epoch:true,exact_sized_free:true"
for rep in 1 2; do
  A="--vsize 64 --keys 10000000 --writes 1000000 --save-delay -10 --seed $rep --op DEL"
  python3 cowbench.py --server vje/src/valkey-server --label je $A >> $OUT
  python3 cowbench.py --server vjef/src/valkey-server --label jef $A >> $OUT
  python3 cowbench.py --server vep/src/valkey-server $L --label ch $A >> $OUT
  python3 cowbench.py --server vepf/src/valkey-server $L --label chf $A >> $OUT
  python3 cowbench.py --server vep/src/valkey-server $L --malloc-conf $EC --label epx $A >> $OUT
  python3 cowbench.py --server vepf/src/valkey-server $L --malloc-conf $EC --label epxf $A >> $OUT
done
echo done
