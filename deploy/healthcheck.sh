#!/bin/bash
# FinBook 存活探针：健康时 exit 0，异常时 exit 1 并打印原因。
#
# 用法（宝塔计划任务，每 5 分钟）：
#   bash healthcheck.sh
# 宝塔「计划任务」可开启「失败告警」，或自行接企业微信/钉钉 webhook：
#   bash healthcheck.sh || curl -s -X POST "$WEBHOOK" -d '{"msgtext":"finbook 探针失败"}'
#
# 为什么不用容器 HEALTHCHECK：它只在容器内生效，宿主机层面看不到
# 「容器在跑但应用已卡死」（进程活着、端口通、但探针不返 ok）这种半死状态。
#
# 为什么用 /api/health/ready 而不是 /api/health：
#   /api/health       = liveness，故意**不碰任何依赖**，只回答「进程还在收请求吗」。
#                       它返回常量 ok，所以磁盘写满、账套文件损坏、账号库被锁
#                       —— 这些系统已经不能用了，它一律报「健康」。
#   /api/health/ready = readiness，真去查账号库 / 账套库 / 数据目录可否读写。
# 探针的价值全在能不能抓到故障上，只查一个常量的探针是虚假的安全感：
# 它让人以为有人在看，于是不去查别的地方。

set -uo pipefail

BASE_URL="${FINBOOK_BASE_URL:-http://127.0.0.1:8080}"
# 连续失败几次才告警，避免网络抖动误报
MAX_FAIL="${FINBOOK_HEALTH_MAX_FAIL:-2}"
STATE_FILE="${FINBOOK_HEALTH_STATE:-/tmp/finbook-health.failcount}"
# 已告警标记：持续故障时**只告警一次**。没有它的话 MAX_FAIL 只是把告警推迟到
# 第 2 次，之后每 5 分钟再发一条 —— 故障持续一晚就是上百条 webhook，
# 真正要人处理的那条会被淹掉（告警疲劳），webhook 端还可能被限流封禁。
ALERTED_FILE="${FINBOOK_HEALTH_ALERTED:-/tmp/finbook-health.alerted}"
# 可选：告警 webhook（企业微信/钉钉通用格式）
WEBHOOK="${FINBOOK_ALERT_WEBHOOK:-}"

fail() {
    echo "FAIL: $*" >&2
    n=$(cat "$STATE_FILE" 2>/dev/null || echo 0)
    # 计数文件可能被人工编辑成非数字；不校验的话 `[ -ge ]` 会刷一堆
    # "integer expression expected" 并把真正的失败原因淹掉
    case "$n" in ''|*[!0-9]*) n=0 ;; esac
    n=$(( n + 1 ))
    echo "$n" > "$STATE_FILE"
    if [ "$n" -ge "$MAX_FAIL" ] && [ ! -f "$ALERTED_FILE" ]; then
        echo "$n" > "$ALERTED_FILE"
        echo "FAIL: 连续 $n 次探测失败（已告警，恢复前不重复告警）" >&2
        if [ -n "$WEBHOOK" ]; then
            curl -sS -m 10 -X POST "$WEBHOOK" \
                -H 'Content-Type: application/json' \
                -d "{\"msgtype\":\"text\",\"text\":{\"content\":\"[finbook] 健康检查连续失败 $n 次：$*\"}}" \
                >/dev/null 2>&1 || true
        fi
    fi
    exit 1
}

# 1) 深度探针：/api/health/ready 免登录，且过 api_auth_gate 白名单。
#    用 -f 让 HTTP 非 2xx 直接失败：readiness 在依赖不可用时返回 **503**，
#    那正是要抓的信号，不能当成 200 放过。
body=$(curl -sS -m 15 -f "$BASE_URL/api/health/ready" 2>&1) \
    || fail "深度探测失败（HTTP 非 2xx 或连不上）：$body"
# 只认 "ok":true 这个字段，不要 grep 整个串 —— 错误信息里也可能出现 ok
# 字样（例如某个文件名带 ok），那就永远探不出问题。
case "$body" in
    *'"ok":true'*) ;;
    *) fail "readiness 报告不可用：$body" ;;
esac

# 2) 数据卷可写性（**宿主机视角**）：与 readiness 里的第 ④ 项不重复 ——
#    那项是 finbook 进程用自己的 uid 试写，这里是跑 cron 的用户试写。
#    两者可能不一致：服务以 finbook 用户跑、cron 以 root 跑时，
#    这里说「可写」而服务实际写不进去。少了任一视角都会漏掉一种部署事故。
books_dir="${FINBOOK_BOOKS_DIR:-/data/books}"
if [ -d "$books_dir" ] && [ ! -w "$books_dir" ]; then
    fail "账套目录不可写：$books_dir（磁盘满 / 权限被改 / 只读挂载）"
fi

# 3) 磁盘水位：账套与备份同盘时，磁盘满是最常见的静默故障。
#    阈值取 85% 而非 90%：SQLite 写满时业务直接报错，但 WAL 与 checkpoint
#    还要额外空间，90% 往往已经太晚 —— 备份（.fbk）与账套同盘，要占两份。
used=$(df -P "$books_dir" 2>/dev/null | awk 'NR==2 {gsub(/%/,"",$5); print $5}')
case "${used:-}" in
    ''|*[!0-9]*) used="" ;;   # df 输出异常时不要拿它做数值比较
esac
if [ -n "$used" ] && [ "$used" -ge 85 ]; then
    fail "账套目录磁盘使用率 ${used}%，已接近写满（账套与备份同盘，备份还要再占一份）"
fi

# 成功：清零失败计数，并清掉「已告警」标记。
# 必须清标记：否则恢复后的**下一次**故障因为标记还在，永远不会再告警 ——
# 那是「防重复告警」机制的典型自伤：防住了噪声，也防住了真正要看的告警。
echo 0 > "$STATE_FILE"
rm -f "$ALERTED_FILE"
echo "OK: $BASE_URL/api/health/ready = ready"
exit 0
