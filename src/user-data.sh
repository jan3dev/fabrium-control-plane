#!/bin/bash
# Fabrium bootstrap stub. Cloud-init runs it once, as root, at first boot.
set -euo pipefail
exec >>/var/log/fabrium-bootstrap.log 2>&1
umask 077
URL='<PUBLIC_URL>/v1/bootstrap'
TOKEN='<TOKEN>'
PAYLOAD=/run/fabrium-bootstrap.json

deadline=$((SECONDS + 600))
while [ "$SECONDS" -lt "$deadline" ]; do
  if printf 'header = "Authorization: Bearer %s"\n' "$TOKEN" |
    curl -fsS -K - --max-time 20 -X POST -o "$PAYLOAD" "$URL"; then
    break
  fi
  sleep 10
done
[ -s "$PAYLOAD" ] || { echo "bootstrap: no payload"; exit 1; }

count="$(jq '.files | length' "$PAYLOAD")"
for i in $(seq 0 $((count - 1))); do
  path="$(jq -r ".files[$i].path" "$PAYLOAD")"
  owner="$(jq -r ".files[$i].owner" "$PAYLOAD")"
  if [ "$owner" = root ]; then
    mkdir -p "$(dirname "$path")"
  else
    sudo -u "$owner" mkdir -p -m 700 "$(dirname "$path")"
  fi
  jq -j ".files[$i].content" "$PAYLOAD" >"$path"
  chmod "$(jq -r ".files[$i].mode" "$PAYLOAD")" "$path"
  chown "$owner:" "$path"
done

jq -r '.script' "$PAYLOAD" >/run/fabrium-setup.sh
bash /run/fabrium-setup.sh
rm -f "$PAYLOAD" /run/fabrium-setup.sh
