#!/bin/bash
# FinBook 自动备份脚本
#
# 原理：走产品自己的备份接口 POST /api/backups（内部是 WAL checkpoint +
#       VACUUM/COPY 出一致快照），而不是直接 cp 账套文件——直接复制 WAL 模式下
#       正在写的 .fbk 会拿到半截事务。
#
# 流程：登录取 cookie → 读账套列表 → 逐个「选账套 + 触发备份」→ 清理过期备份
#
# 部署（宝塔面板 → 计划任务 → Shell 脚本，建议每天 01:30）：
#   1) 把本脚本放到任意路径，chmod +x
#   2) 配置环境变量 FINBOOK_ADMIN_PASS（见下方「配置」）
#   3) 任务选「Shell 脚本」，填：bash /path/to/backup.sh
#   失败时退出码非 0，宝塔会记录失败并可配告警。
#
# 依赖：curl（必需）。jq 可选，缺了自动退回 grep 解析。

set -uo pipefail

# ── 配置 ───────────────────────────────────────────────────────────────
# 服务地址：容器内/本机直连走 127.0.0.1（绕过 nginx，少一层依赖）
BASE_URL="${FINBOOK_BASE_URL:-http://127.0.0.1:8080}"
# 备份账号：需要一个有「备份」权限的账号。管理员登录不绑定设备、也不踢掉
# 其他会话，所以脚本用管理员账号不会把正在用系统的同事踢下线。
ADMIN_USER="${FINBOOK_ADMIN_USER:-admin}"
# 口令来源优先级：环境变量 > 同目录 .env 文件（推荐，chmod 600）
if [ -z "${FINBOOK_ADMIN_PASS:-}" ] && [ -f "$(dirname "$0")/.env" ]; then
    # shellcheck disable=SC1091
    . "$(dirname "$0")/.env"
fi
ADMIN_PASS="${FINBOOK_ADMIN_PASS:-}"
# 备份目录：容器部署是 /data/books/backups，非容器按实际 FINBOOK_BOOKS_DIR 改
BACKUP_DIR="${FINBOOK_BACKUP_DIR:-/data/books/backups}"
# 保留天数
KEEP_DAYS="${FINBOOK_KEEP_DAYS:-30}"
LOG_FILE="${FINBOOK_BACKUP_LOG:-/var/log/finbook-backup.log}"

log() {
    local line="[$(date '+%Y-%m-%d %H:%M:%S')] $*"
    echo "$line"
    { echo "$line"; } >> "$LOG_FILE" 2>/dev/null || true
}

die() {
    log "致命错误：$*"
    exit 1
}

[ -n "$ADMIN_PASS" ] || die "未配置 FINBOOK_ADMIN_PASS（环境变量或同目录 .env）"
command -v curl >/dev/null 2>&1 || die "缺少 curl"

# ── 登录 ───────────────────────────────────────────────────────────────
COOKIE_JAR=$(mktemp)
trap 'rm -f "$COOKIE_JAR"' EXIT

# 登录失败通常三种：口令错 / 账号被锁 / 口令处于「强制修改」状态。
# 强制改密状态下除改密接口外全部 401，脚本会表现为「读账套列表失败」。
#
# 口令经 stdin 传给 curl（--data-binary @-），**不能**写成 -d "{...}"：
# 命令行参数对同机任何用户可见（ps auxww 能看到完整 argv），
# 而计划任务常常与人在同一台机器上跑 —— 那是明文凭证泄露路径。
login_resp=$(printf '{"username":"%s","password":"%s","device_id":"backup-script","device_name":"备份脚本"}' \
                  "$ADMIN_USER" "$ADMIN_PASS" \
    | curl -sS -m 20 -c "$COOKIE_JAR" -X POST "$BASE_URL/api/login" \
        -H "Content-Type: application/json" --data-binary @- 2>&1) \
    || die "登录请求失败：$login_resp"

if ! grep -q "finbook_sid" "$COOKIE_JAR" 2>/dev/null; then
    die "登录未拿到会话 cookie。响应：${login_resp:-（空）}
  常见原因：口令错误/账号被锁；或该账号处于「必须修改口令」状态
  （此时只有改密与退出接口可用，请先在浏览器登录改完口令）"
fi
log "登录成功：$ADMIN_USER"

api_get() {
    curl -sS -m 60 -b "$COOKIE_JAR" "$BASE_URL$1"
}
api_post() {
    curl -sS -m 300 -b "$COOKIE_JAR" -X POST "$BASE_URL$1"
}

# ── 读账套列表 ─────────────────────────────────────────────────────────
books_json=$(api_get "/api/books") || die "获取账套列表失败"
case "$books_json" in
    *'"error"'*) die "获取账套列表被拒：$books_json" ;;
esac

# 提取 key：优先 jq，退回 grep（响应是紧凑 JSON，格式稳定）
if command -v jq >/dev/null 2>&1; then
    book_keys=$(printf '%s' "$books_json" | jq -r '.books[]?.key')
else
    book_keys=$(printf '%s' "$books_json" | grep -o '"key":"[^"]*"' | cut -d'"' -f4)
fi

if [ -z "$book_keys" ]; then
    log "没有可备份的账套（books 为空），正常退出"
    exit 0
fi

book_count=$(printf '%s\n' "$book_keys" | grep -c .)
log "待备份账套 $book_count 个"

# ── 逐个账套备份 ───────────────────────────────────────────────────────
ok=0
fail=0
total_bytes=0

while IFS= read -r key; do
    [ -z "$key" ] && continue

    # 备份接口作用于「当前选中的账套」，所以每个账套都要先 select
    sel=$(api_post "/api/books/$key/select") || sel="请求失败"
    case "$sel" in
        *'"error"'*) log "跳过 $key：选择账套失败 $sel"; fail=$((fail+1)); continue ;;
    esac

    resp=$(api_post "/api/backups") || resp="请求失败"
    case "$resp" in
        *'"ok":true'*)
            name=$(printf '%s' "$resp" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')
            size=$( [ -n "$name" ] && [ -f "$BACKUP_DIR/$name" ] \
                    && stat -c %s "$BACKUP_DIR/$name" 2>/dev/null \
                    || echo 0 )
            total_bytes=$(( total_bytes + size ))
            log "备份成功 $key → $name ($(numfmt --to=iec "$size" 2>/dev/null || echo "${size}B"))"
            ok=$((ok+1))
            ;;
        *) log "备份失败 $key：$resp"; fail=$((fail+1)) ;;
    esac
done <<EOF
$book_keys
EOF

# ── 清理过期备份 ───────────────────────────────────────────────────────
if [ -d "$BACKUP_DIR" ]; then
    pruned=$(find "$BACKUP_DIR" -maxdepth 1 -type f -name '*.fbk' -mtime +"$KEEP_DAYS" \
             -print -delete 2>/dev/null | wc -l)
    [ "$pruned" -gt 0 ] && log "清理：删除 $pruned 个超过 ${KEEP_DAYS} 天的备份"
    left=$(find "$BACKUP_DIR" -maxdepth 1 -type f -name '*.fbk' 2>/dev/null | wc -l)
    log "备份目录现存 $left 个文件：$BACKUP_DIR"
else
    log "警告：备份目录不存在 $BACKUP_DIR（本次备份已生成，但无法统计/清理）"
fi

log "完成：成功 $ok，失败 $fail，合计 $(numfmt --to=iec "$total_bytes" 2>/dev/null || echo "${total_bytes}B")"

if [ "$fail" -gt 0 ]; then
    exit 1
fi
exit 0
