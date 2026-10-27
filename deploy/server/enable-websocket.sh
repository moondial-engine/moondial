#!/usr/bin/env bash
# Add the WebSocket session location to the live nginx config on the game server. Run
# as root, once, after the release that ships it:
#   bash /srv/moondial/current/deploy/enable-websocket.sh
# certbot rewrote /etc/nginx/sites-enabled/play after setup, so the file is edited in
# place rather than replaced: every `location / {` block gets a sibling for /v0/session.
set -uo pipefail
F=/etc/nginx/sites-enabled/play
[ -f "$F" ] || { echo "FAIL: $F missing"; exit 1; }
if grep -q "location /v0/session" "$F"; then
  echo "OK:   $F already has the session location"
else
  python3 - "$F" <<'PY'
import re, sys
path = sys.argv[1]
s = open(path).read()
block = """    location /v0/session {
        proxy_pass http://127.0.0.1:7777;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }

"""
# Insert before each `location / {` that proxies to the driver.
out = re.sub(r"(\n)(\s*location / \{)", lambda m: m.group(1) + block + m.group(2), s, count=2)
open(path, "w").write(out)
PY
  echo "OK:   added the session location to $F"
fi
nginx -t && systemctl reload nginx && echo "OK:   nginx reloaded" || { echo "FAIL: nginx config test failed"; exit 1; }
