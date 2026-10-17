#!/bin/bash
set -u
S=${1:-/usr/local/bin/finbook-backup.sh}
[ -f "$S" ] || { echo "找不到被检脚本：$S（用法：$0 [脚本路径]）"; exit 2; }
LOG=/tmp/bkselftest.log
: > "$LOG"
export FINBOOK_BACKUP_LOG="$LOG"
pass=0; bad=0

# 期望「失败」的探针：跑变异脚本，必须 rc!=0 且日志出现指定文案
expect_fail() {
  label="$1"; want="$2"; shift 2
  out=$(FINBOOK_DATA_VOL="$1" FINBOOK_BACKUP_OUT="$2" "$S" 2>&1); rc=$?
  last=$(tail -2 "$LOG" | tr '\n' ' ')
  if [ "$rc" -ne 0 ] && echo "$last" | grep -q "$want"; then
    echo "PASS 拦下：$label"; pass=$((pass+1))
  else
    echo "FAIL 未拦下：$label rc=$rc log=$last"; bad=$((bad+1))
  fi
}

# 期望「成功」
expect_ok() {
  label="$1"
  out=$(FINBOOK_DATA_VOL="$2" FINBOOK_BACKUP_OUT="$3" "$S" 2>&1); rc=$?
  if [ "$rc" -eq 0 ]; then echo "PASS 通过：$label"; pass=$((pass+1));
  else echo "FAIL 应通过却失败：$label rc=$rc"; bad=$((bad+1)); fi
}

FIX=/tmp/bkfix; rm -rf "$FIX" /tmp/bkout; mkdir -p "$FIX/books" /tmp/bkout

# 探针 1：数据卷不存在
expect_fail "数据卷不存在" "数据卷不存在" /nonexistent/vol /tmp/bkout

# 探针 2：账套源文件 0 字节
: > "$FIX/books/zero.fbk"
sqlite3 "$FIX/realm.db" "create table realm_book(x); create table realm_user(x); insert into realm_book values('b'); insert into realm_user values('u');"
expect_fail "0 字节账套" "0 字节" "$FIX" /tmp/bkout

# 探针 3：「合法但空」的库（0 张表）
rm -f "$FIX/books/zero.fbk"
sqlite3 "$FIX/books/empty.fbk" "create table t(a); drop table t;"
expect_fail "0 张表的账套" "合法但空" "$FIX" /tmp/bkout

# 探针 4：realm_book 0 行（合法但不是这套数据）
rm -f "$FIX/books/empty.fbk"
sqlite3 "$FIX/good.db" "create table realm_book(x); create table realm_user(x);"
FIX2=/tmp/bkfix2; rm -rf "$FIX2"; mkdir -p "$FIX2/books"
cp "$FIX/good.db" "$FIX2/realm.db"
sqlite3 "$FIX2/books/g.fbk" "create table voucher(id integer); insert into voucher values(1);"
expect_fail "realm_book 0 行" "realm_book 0 行" "$FIX2" /tmp/bkout

# 正例：一份像样的夹具应当通过
FIX3=/tmp/bkfix3; rm -rf "$FIX3"; mkdir -p "$FIX3/books"
sqlite3 "$FIX3/realm.db" "create table realm_book(k); create table realm_user(u); insert into realm_book values('g'); insert into realm_user values('admin');"
sqlite3 "$FIX3/books/g.fbk" "create table voucher(id integer); insert into voucher values(1);"
expect_ok "正常夹具通过" "$FIX3" /tmp/bkout
T=$(ls -t /tmp/bkout/finbook-auto-*.tgz | head -1)
tar tzf "$T" | tr '\n' ' '; echo

# 真实数据卷
echo '=== 真实卷 ==='
"$S"; echo "rc=$?"
tail -2 "$LOG"

rm -rf "$FIX" "$FIX2" "$FIX3" /tmp/bkout
echo "=== 自检结果：PASS=$pass FAIL=$bad ==="
[ "$bad" -eq 0 ]
