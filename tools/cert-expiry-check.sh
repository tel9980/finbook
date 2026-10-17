#!/bin/bash
# 证书到期检查：nginx 实际在用的证书剩余天数 < 阈值 就记 ERROR 日志
set -u
THRESH_DAYS="${1:-3}"
LOG=/var/log/cert-watch.log
ts() { date '+%F %T'; }
rc=0
for f in $(nginx -T 2>/dev/null | awk '/ssl_certificate /{print $2}' | tr -d ';' | sort -u); do
  [ -f "$f" ] || { echo "$(ts) ERROR missing-cert $f" >>"$LOG"; rc=1; continue; }
  end=$(date -d "$(openssl x509 -in "$f" -noout -enddate | cut -d= -f2)" +%s 2>/dev/null) || continue
  now=$(date +%s)
  days=$(( (end - now) / 86400 ))
  if [ "$days" -lt "$THRESH_DAYS" ]; then
    echo "$(ts) ERROR cert-expires-in-${days}d $f notAfter=$(date -d @$end '+%F %T')" >>"$LOG"
    rc=1
  else
    echo "$(ts) OK ${days}d $f" >>"$LOG"
  fi
done
exit $rc
