#!/bin/bash
# CI 监控脚本 — 每 60 秒查一次状态，CI 完成时通知
# 用法: ./scripts/ci-watch.sh
# 依赖: gh CLI (已认证)

set -euo pipefail

REPO="HyperSynapseNetwork/Phira-mp-plus"
POLL_SEC=60

echo "=== CI 监控启动 ==="
echo "仓库: $REPO"
echo "轮询间隔: ${POLL_SEC}s"
echo "按 Ctrl+C 停止"
echo ""

last_status=""

while true; do
  # 取最近 2 个 workflow 的 status/conclusion，一次调用
  data=$(gh -R "$REPO" run list --limit 4 --json workflowName,status,conclusion,displayTitle,databaseId 2>/dev/null)

  # 紧凑输出
  echo "$(date '+%H:%M:%S') ───────────────────────────────"
  echo "$data" | python3 -c "
import json,sys
rows = json.load(sys.stdin)
for r in rows:
    wf = r['workflowName']
    s = r['status']
    c = r.get('conclusion','')
    t = r['displayTitle'][:50]
    m = '✅' if c=='success' else '❌' if c=='failure' else '⏳' if s=='in_progress' else '⬜'
    print(f'{m} {wf:20s} {s:12s} {c or chr(46)*4:10s} {t}')
" 2>/dev/null

  # 检查是否有 running 的
  running=$(echo "$data" | python3 -c "
import json,sys
rows = json.load(sys.stdin)
# 找最近一次 push 触发的 Build
for r in rows:
    if r['workflowName'] == 'Build' and r['status'] in ('in_progress','queued','pending'):
        print('running')
        sys.exit(0)
print('done')
" 2>/dev/null)

  if [ "$running" = "done" ]; then
    echo ""
    echo "=== CI 全部完成 ==="
    echo "$data" | python3 -c "
import json,sys
rows = json.load(sys.stdin)
for r in rows:
    wf = r['workflowName']
    c = r.get('conclusion','')
    t = r['displayTitle'][:50]
    m = '✅' if c=='success' else '❌' if c=='failure' else '⬜'
    print(f'{m} {wf:20s} {c:10s} {t}')
" 2>/dev/null
    echo ""
    # 失败时提示
    fails=$(echo "$data" | python3 -c "
import json,sys
rows = json.load(sys.stdin)
for r in rows:
    if r.get('conclusion') == 'failure':
        print(r['displayTitle'])
" 2>/dev/null)
    if [ -n "$fails" ]; then
      echo "❌ 失败的 workflow:"
      echo "$fails"
      echo ""
      echo "查看详情: gh -R $REPO run view --log-failed"
    fi
    break
  fi

  sleep "$POLL_SEC"
done
