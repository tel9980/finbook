#!/usr/bin/env bash
# 部署 finbook 到生产机，并**从反代实际连的地址**验证。
#
# 这个脚本存在的唯一理由：DEPLOY.md §4.1 已经把那个坑写清楚了
# （端口映射错 → 站点 502 而容器 healthcheck 全绿），表格里也列了
# 「回环上游绑回环、其它地址绑 0.0.0.0」。
#
# 但 2026-10-01 我还是又踩了一次：文档就在仓库里，我按「标准做法」写了
# `-p 127.0.0.1:8080:8080`，而这台机器的 nginx `proxy_pass` 指向
# `10.66.0.1:8080`（wg0 WireGuard 网卡，不是回环也不是 docker bridge）→
# 站点 502，而 `docker inspect` 显示 healthy。
#
# **知道陷阱不等于不会踩。** 所以把「动手前先看 proxy_pass」这一步
# 从「文档里的建议」变成「脚本里的必经步骤」：绑定方式由脚本从反代配置
# 推导，推导不出来就**报错停下**，绝不让人凭印象填。
#
# 用法（在服务器上，仓库根目录）：
#   bash tools/deploy.sh <镜像标签>            # 默认 finbook:latest
#   bash tools/deploy.sh finbook:aging
#
# 环境变量可覆盖（默认值就是本机实际拓扑）：
#   VHOST_FILE        nginx vhost 路径
#   CONTAINER_NAME    容器名
#   DATA_VOLUME       数据卷
#   HOST_PORT         若已给出，直接用它并**跳过**推导（仍会验证）

set -euo pipefail

TAG="${1:-finbook:latest}"
VHOST_FILE="${VHOST_FILE:-/www/server/panel/vhost/nginx/finbook.conf}"
CONTAINER_NAME="${CONTAINER_NAME:-finbook}"
DATA_VOLUME="${DATA_VOLUME:-finbook_data}"

fail() { echo "!! $*" >&2; exit 1; }

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo /opt/finbook-src-head0923b)"
echo "仓库：$PWD"
echo "镜像：$TAG"
echo "提交：$(git rev-parse --short HEAD 2>/dev/null || echo '未知')"

# ---- 1. 从反代配置推导上游地址 -------------------------------------------------
#
# 「推导不出来就停」是重点。以前这里是留给人手填的一格，而那一格
# 正是出错的地方。
#
# ⚠️ 两个踩过的坑，都写在这段里：
#
#   ① 字符类写成 `[0-9a-zA-Z.:\[\]-]` 匹配不到任何东西。
#      POSIX 允许**在括号表达式里再嵌括号表达式**（`[[...]]` 是字符类），
#      所以 `[` 会被当成「开一个内层字符类」，集合语义完全不是我以为的那个；
#      加 `\]` 也救不回来（实测两个写法都 rc=1 无匹配）。
#      改用**否定**字符类 `[^\s;/]+`：到空白/斜杠/分号为止，不碰括号转义。
#
#   ② `set -e` 会在这条命令替换赋值那一步就把脚本杀掉，
#      于是下面那句「推导不出来就报错停下」**永远执行不到** ——
#      现象是「脚本什么都不打印就 exit 1」。比报错还糟：
#      它让「设计好的失败路径」静默失效，看起来像部署成功了。
#      所以整条管道必须显式吞掉失败（`|| true`），把判断留给 if。
UPSTREAM=""
if [ -n "${HOST_PORT:-}" ]; then
  UPSTREAM="(由 HOST_PORT 指定，跳过推导)"
  UP_HOST="(跳过)"
  UP_PORT="$HOST_PORT"
elif [ -f "$VHOST_FILE" ]; then
  UPSTREAM=$(grep -oE 'proxy_pass[[:space:]]+https?://[^\s;/]+' "$VHOST_FILE" 2>/dev/null \
             | head -1 | sed -E 's#.*://##' | sed -E 's#[/?].*$##' || true)
  UPSTREAM=${UPSTREAM%.}
  if [ -n "$UPSTREAM" ] && [[ "$UPSTREAM" == *:* ]]; then
    UP_HOST="${UPSTREAM%:*}"
    UP_PORT="${UPSTREAM##*:}"
  else
    UPSTREAM=""
    UP_HOST=""
    UP_PORT=""
  fi
else
  echo "找不到反代配置：$VHOST_FILE"
fi

if [ -z "$UPSTREAM" ]; then
  echo
  echo "--- 我在 $VHOST_FILE 里看到的 proxy_pass 行（若有）---"
  grep -n 'proxy_pass' "$VHOST_FILE" 2>/dev/null | sed 's/^/  /' || echo "  （没有）"
  fail "读不到反代上游地址。
     确认上面那几行的实际写法后，用 HOST_PORT=<端口> 重跑，或改 VHOST_FILE。
     **不要凭印象填端口** —— 那正是本脚本要消除的动作。
     （第一版这里还会静默 exit：那是因为 set -e 在管道里就砍掉了脚本，
       这段提示根本执行不到。管道现在显式 || true，判断留给这里。）"
fi

UP_HOST="${UPSTREAM%:*}"
UP_PORT="${UPSTREAM##*:}"
echo
echo "反代上游：$UP_HOST:$UP_PORT   （来源：${HOST_PORT:+HOST_PORT}${HOST_PORT:-proxy_pass}）"

if ! [[ "$UP_PORT" =~ ^[0-9]+$ ]]; then
  fail "上游端口不是数字：$UP_PORT"
fi

# ---- 2. 按上游地址决定端口绑定 --------------------------------------------------
#
# 唯一一处需要判断的地方，写成表而不是「凭印象」：
#   上游是回环            → docker-proxy 绑回环就够（更安全）
#   上游是宿主的别的地址  → 必须绑 0.0.0.0，否则那个地址上没有监听
#
# 为什么第二种特别容易踩：wg0 / docker0 / eth0 这些**非回环**地址，
# 在「绑 127.0.0.1」时全都连不上，而容器自己完全健康。
case "$UP_HOST" in
  127.0.0.1|localhost|::1|'[::1]')
    BIND="-p 127.0.0.1:${UP_PORT}:8080"
    BIND_WHY="上游是回环 → 绑回环（外部访问不到，暴露面最小）"
    ;;
  *)
    BIND="-p ${UP_PORT}:8080"
    BIND_WHY="上游是非回环地址（${UP_HOST}）→ 必须绑 0.0.0.0。
             绑 127.0.0.1 的话那个地址上没有监听，站点 502 而容器 healthy。"
    ;;
esac
echo "端口绑定：$BIND"
echo "           $BIND_WHY"

# ---- 3. 备份 -------------------------------------------------------------------
echo
echo "=== 备份数据卷 $DATA_VOLUME ==="
mkdir -p backups
STAMP=$(date +%Y%m%d-%H%M%S)
docker run --rm -v "$DATA_VOLUME:/data" -v "$PWD/backups:/out" debian:bookworm-slim \
  sh -c "tar czf /out/realm-$STAMP.tgz -C /data realm.db; \
         [ -d /data/books ] && tar czf /out/books-$STAMP.tgz -C /data books; \
         ls -la /out | grep $STAMP" \
  || fail "备份失败 —— 不继续。没有备份就没有回滚余地。"

# ---- 4. 换容器 -----------------------------------------------------------------
echo
echo "=== 换容器（保留旧镜像 tag，可直接 docker run 回滚）==="
if docker ps -a --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"; then
  docker stop "$CONTAINER_NAME" >/dev/null
  docker rm "$CONTAINER_NAME" >/dev/null
  echo "旧容器已移除"
fi
# shellcheck disable=SC2086
docker run -d --name "$CONTAINER_NAME" --restart unless-stopped \
  -v "$DATA_VOLUME:/data" \
  $BIND \
  "$TAG" | tail -1

for i in $(seq 1 20); do
  s=$(docker inspect -f '{{.State.Health.Status}}' "$CONTAINER_NAME" 2>/dev/null || echo none)
  [ "$s" = healthy ] && { echo "health: healthy（第 $i 次）"; break; }
  [ "$s" = unhealthy ] && { echo "health: unhealthy"; docker logs --tail 30 "$CONTAINER_NAME"; exit 1; }
  [ "$i" -eq 20 ] && { echo "health: 20 次仍未就绪"; docker logs --tail 30 "$CONTAINER_NAME"; exit 1; }
  sleep 5
done

# ---- 5. 从反代那侧验证 ---------------------------------------------------------
#
# 这一步不能省，也不能改成「从容器内 curl」：healthcheck 已经在容器内跑过了，
# 它证明不了宿主→容器的映射对不对。**要证明的恰恰是映射**，所以必须从外面打。
echo
echo "=== 从反代上游 $UP_HOST:$UP_PORT 验证（不是从容器内）==="
UP="http://${UP_HOST}:${UP_PORT}"
code=$(curl -s -o /dev/null -w '%{http_code}' "$UP/api/health")
echo "  /api/health -> $code"
if [ "$code" != "200" ]; then
  echo "  响应体：$(curl -s "$UP/api/health" | head -c 200)"
  fail "从反代上游访问不通。若容器 healthy 而这里不通，就是端口映射/绑定错了。
     当前绑定：$BIND
     宿主机监听：$(ss -lnt | grep ":$UP_PORT" || echo '(无)')"
fi
echo "  body: $(curl -s "$UP/api/health" | head -c 80)"

for p in / /api/customers /api/settle/aging?account=1122; do
  echo "  $p -> $(curl -s -o /dev/null -w '%{http_code}' "$UP$p")"
done

echo
echo "=== 反代那一跳（basic auth 在反代层，未带口令应是 401）==="
PUBLIC=$(grep -oE 'server_name[[:space:]]+[^;]+' "$VHOST_FILE" | head -1 | awk '{print $2}')
if [ -n "$PUBLIC" ]; then
  for scheme in https http; do
    echo "  $scheme://$PUBLIC/ -> $(curl -sk -o /dev/null -w '%{http_code}' "$scheme://$PUBLIC/" || echo 失败)"
  done
  echo "  401 = 反代在、只是缺 basic auth 口令（口令不在仓库里，需人工确认）"
fi

echo
echo "=== 完成 ==="
echo "  回滚：docker run -d --name $CONTAINER_NAME --restart unless-stopped -v $DATA_VOLUME:/data $BIND <旧镜像>"
echo "  备份：backups/*-$STAMP.tgz"