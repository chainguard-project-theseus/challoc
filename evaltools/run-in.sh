# run-in.sh VARIANT THREADS: one xmalloc-cg run under ballast 92
v=$1; w=$2
case $v in
  fixed) so=head.so; conf=tcache_nslots_small_min:65536 ;;
  slow) so=head.so; conf= ;;
  pre) so=pre.so; conf= ;;
  start0) so=s0.so; conf= ;;
esac
cd /src/target/eval
echo "== variant=$v threads=$w"
MALLOC_CONF=$conf ./ballast 92 ./rus env LD_PRELOAD=/src/target/eval/$so ./xmalloc-test -w $w -t 5 -s 64 2>&1
