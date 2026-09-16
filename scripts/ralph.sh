#!/usr/bin/env bash
# Unattended implementation loop. Each iteration is a fresh `claude -p`
# process with a tiny prompt; all state lives in git and PROGRESS.md, so no
# context accumulates anywhere. Stop conditions: PROGRESS.md says DONE or
# BLOCKED, or MAX_ITERS is reached.
#
#   MAX_ITERS=40 ITER_TIMEOUT=90m MODEL= BUDGET_USD_PER_ITER= scripts/ralph.sh
set -uo pipefail
cd "$(dirname "$0")/.."

MAX_ITERS="${MAX_ITERS:-40}"
ITER_TIMEOUT="${ITER_TIMEOUT:-90m}"
MODEL="${MODEL:-}"
BUDGET="${BUDGET_USD_PER_ITER:-}"
PROMPT="$(cat scripts/ralph-prompt.md)"

mkdir -p .ralph/logs

for i in $(seq 1 "$MAX_ITERS"); do
  status="$(sed -n 's/^STATUS: *//p' PROGRESS.md | head -1)"
  case "$status" in
    DONE)    echo "PROGRESS.md reports DONE; stopping."; exit 0 ;;
    BLOCKED) echo "PROGRESS.md reports BLOCKED; stopping. See PROGRESS.md."; exit 2 ;;
  esac

  log=".ralph/logs/$(date +%Y%m%d-%H%M%S)-iter$i.log"
  echo "[$(date '+%H:%M:%S')] iteration $i -> $log"

  timeout "$ITER_TIMEOUT" claude -p "$PROMPT" \
    --dangerously-skip-permissions \
    ${MODEL:+--model "$MODEL"} \
    ${BUDGET:+--max-budget-usd "$BUDGET"} \
    >"$log" 2>&1
  rc=$?
  [ "$rc" -ne 0 ] && echo "  iteration $i exited $rc (timeout is 124)"

  # Never lose work if the agent forgot to commit.
  if [ -n "$(git status --porcelain)" ]; then
    git add -A && git commit -qm "ralph iteration $i: sweep uncommitted work" && echo "  swept uncommitted work into a commit"
  fi
done
echo "Reached MAX_ITERS=$MAX_ITERS; stopping."
