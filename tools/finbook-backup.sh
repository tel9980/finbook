#!/bin/bash
# 每日账套 + realm 在线一致备份。
# 走宿主机 sqlite3 的 .backup 出一致快照（WAL 下比裸 cp 安全），打包后按
# DEPLOY.md §6.1 的三条教训做**业务语义回读**：
#   ① 0 字节文件 integrity_check 照样返回 ok —— 必须单独判非空
#   ② 「合法但空」的库也会过 —— 必须断言表数 / 行数 > 0
#   ③ 账套在 books/ 下，不在数据根目录 —— 必须按目录找且至少找到 1 个非空
# 另外把同目录 .attachments/（若存在）一起打包。
set -u
VOL=${FINBOOK_DATA_VOL:-/var/lib/docker/volumes/finbook_data/_data}
OUT=${FINBOOK_BACKUP_OUT:-/opt/finbook-src-head0923b/backups}
KEEP_DAYS=${FINBOOK_BACKUP_KEEP:-14}
LOG=${FINBOOK_BACKUP_LOG:-/var/log/finbook-backup.log}
STAMP=$(date +%Y%m%d-%H%M%S)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

log() { echo "$(date '+%F %T') $*" >>"$LOG"; }
fail() { log "ERROR $*"; log "FAILED stamp=$STAMP"; exit 1; }

mkdir -p "$OUT" "$TMP/books"
[ -d "$VOL" ] || fail "数据卷不存在 $VOL"
[ -f "$VOL/realm.db" ] || fail "realm.db 不在 $VOL"

# ---- realm.db：一致快照 + 三段回读 ----
sqlite3 "$VOL/realm.db" ".backup '$TMP/realm.db'" || fail "realm.db 快照失败"
[ -s "$TMP/realm.db" ] || fail "realm.db 快照是 0 字节"
ic=$(sqlite3 "$TMP/realm.db" 'PRAGMA integrity_check;') || fail "realm.db 校验没跑起来"
[ "$ic" = "ok" ] || fail "realm.db integrity=$ic"
tables=$(sqlite3 "$TMP/realm.db" "select count(*) from sqlite_master where type='table';")
books=$(sqlite3 "$TMP/realm.db" 'select count(*) from realm_book;')
users=$(sqlite3 "$TMP/realm.db" 'select count(*) from realm_user;')
[ "${tables:-0}" -gt 0 ] || fail "realm.db 里一张表都没有"
[ "${books:-0}" -gt 0 ] || fail "realm_book 0 行 —— 备份到的不是这套数据"
[ "${users:-0}" -gt 0 ] || fail "realm_user 0 行 —— 备份到的不是这套数据"

# ---- 账套：每个非空 .fbk 快照 + 表数/凭证数回读 ----
found=0
for f in "$VOL"/books/*.fbk; do
  [ -e "$f" ] || continue
  b=$(basename "$f")
  [ -s "$f" ] || fail "$b 源文件 0 字节，跳过它等于没备份"
  sqlite3 "$f" ".backup '$TMP/books/$b'" || fail "$b 快照失败"
  [ -s "$TMP/books/$b" ] || fail "$b 快照 0 字节"
  ic=$(sqlite3 "$TMP/books/$b" 'PRAGMA integrity_check;') || fail "$b 校验没跑起来"
  [ "$ic" = "ok" ] || fail "$b integrity=$ic"
  t=$(sqlite3 "$TMP/books/$b" "select count(*) from sqlite_master where type='table';")
  [ "${t:-0}" -gt 0 ] || fail "$b 是「合法但空」的库（0 张表）"
  v=$(sqlite3 "$TMP/books/$b" 'select count(*) from voucher;' 2>/dev/null || echo '?')
  log "  账套 $b 表=$t 凭证=$v"
  found=$((found + 1))
done
[ "$found" -gt 0 ] || fail "books/ 下一个非空账套都没找到 —— 备份等于没做"

# ---- 外置附件（若存在，两个可能位置都看）----
for a in "$VOL/.attachments" "$VOL/books/.attachments"; do
  if [ -d "$a" ]; then
    rel=${a#"$VOL"/}
    mkdir -p "$TMP/$(dirname "$rel")"
    cp -a "$a" "$TMP/$rel" || fail "附件目录拷贝失败 $a"
    log "  附件 $rel $(find "$a" -type f | wc -l) 个文件"
  fi
done

# ---- 打包 + 归档自检 ----
TAR="$OUT/finbook-auto-$STAMP.tgz"
ENTRIES=$(ls -A "$TMP" | tr '\n' ' ')
tar czf "$TAR" -C "$TMP" $ENTRIES || fail "打包失败"
tar tzf "$TAR" >/dev/null || fail "归档自检失败 $TAR"
tar tzf "$TAR" | grep -Eq '^(\./)?realm\.db$' || fail "归档里没有 realm.db"
tar tzf "$TAR" | grep -Eq '^(\./)?books/[^/]*\.fbk$' || fail "归档里没有账套"

find "$OUT" -name 'finbook-auto-*.tgz' -mtime +"$KEEP_DAYS" -delete
log "OK stamp=$STAMP size=$(stat -c%s "$TAR") 账套=$found realm_book=$books realm_user=$users kept=$(find "$OUT" -name 'finbook-auto-*.tgz' | wc -l)"
