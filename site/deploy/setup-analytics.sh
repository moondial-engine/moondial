#!/usr/bin/env bash
# One-time analytics setup on the VPS. Run as root, after setup-server.sh and one deploy.
# From Windows PowerShell, in site/:
#   cmd /c 'ssh root@5.78.104.64 "tr -d \"\r\" | ADMIN_USER=david ADMIN_PASS=... bash -s" < deploy\setup-analytics.sh > setup-analytics.log'
#
# - installs the JSON log format (http context) and makes both public server blocks
#   include the analytics snippet (access_log + the /_admin/ page)
# - rotates /var/log/nginx/analytics.jsonl daily and deletes it after 30 days (raw IPs)
# - lets the deploy user read nginx logs (adm group) and owns the analytics store dir
# - installs a systemd timer that runs collect.py every 5 minutes and render.py hourly,
#   from the current release, so a deploy updates the scripts
# - creates the basic-auth file for /_admin/ from ADMIN_USER / ADMIN_PASS
set -uo pipefail
exec 2>&1

RUN_USER=${RUN_USER:-starsailor}
APP_DIR=/srv/agentscanplay
STORE=$APP_DIR/shared/analytics
ADMIN_USER=${ADMIN_USER:?set ADMIN_USER}
ADMIN_PASS=${ADMIN_PASS:?set ADMIN_PASS}
ok()   { echo "OK:   $*"; }
fail() { echo "FAIL: $*"; }
[ "$(id -u)" -eq 0 ] || { echo "Run as root"; exit 1; }

REL=$APP_DIR/current
[ -d "$REL/deploy/nginx-snippets" ] || { fail "no deployed release with deploy/nginx-snippets; deploy first"; exit 1; }

echo "######## nginx log format and snippet"
install -m 644 "$REL/deploy/nginx-snippets/analytics-format.conf" /etc/nginx/conf.d/analytics-format.conf
install -d /etc/nginx/snippets
install -m 644 "$REL/deploy/nginx-snippets/analytics.conf" /etc/nginx/snippets/analytics.conf
for site in agentscanplay moondial; do
  f=/etc/nginx/sites-available/$site
  [ -f "$f" ] || { fail "$f missing"; exit 1; }
  if grep -q "snippets/analytics.conf" "$f"; then
    ok "$site already includes the analytics snippet"
  else
    # Insert after the apex server_name line(s) only (the www blocks are redirects).
    sed -i -E "/^\s*server_name (agentscanplay\.com|moondial\.ai);/a\    include snippets/analytics.conf;" "$f"
    ok "added include to $site ($(grep -c 'snippets/analytics.conf' "$f") blocks)"
  fi
done

echo; echo "######## basic auth for /_admin/"
HASH=$(openssl passwd -apr1 "$ADMIN_PASS")
touch /etc/nginx/.htpasswd-moondial
if grep -q "^$ADMIN_USER:" /etc/nginx/.htpasswd-moondial; then
  sed -i "s|^$ADMIN_USER:.*|$ADMIN_USER:$HASH|" /etc/nginx/.htpasswd-moondial
else
  echo "$ADMIN_USER:$HASH" >> /etc/nginx/.htpasswd-moondial
fi
chmod 640 /etc/nginx/.htpasswd-moondial; chown root:www-data /etc/nginx/.htpasswd-moondial
ok "admin user $ADMIN_USER set"

nginx -t && systemctl reload nginx && ok "nginx reloaded" || { fail "nginx config test failed"; exit 1; }

echo; echo "######## log rotation and permissions"
cat > /etc/logrotate.d/analytics-jsonl <<'ROT'
/var/log/nginx/analytics.jsonl {
    daily
    rotate 30
    missingok
    notifempty
    compress
    delaycompress
    create 0640 www-data adm
    sharedscripts
    postrotate
        [ -f /var/run/nginx.pid ] && kill -USR1 "$(cat /var/run/nginx.pid)"
    endscript
}
ROT
usermod -aG adm "$RUN_USER"
install -d -o "$RUN_USER" -g "$RUN_USER" "$STORE" "$STORE/html"
touch /var/log/nginx/analytics.jsonl; chown www-data:adm /var/log/nginx/analytics.jsonl; chmod 640 /var/log/nginx/analytics.jsonl
ok "raw log rotates daily, kept 30 days; $RUN_USER can read it; store at $STORE"

echo; echo "######## systemd timers"
cat > /etc/systemd/system/analytics-collect.service <<UNIT
[Unit]
Description=Collect nginx analytics log into the store
[Service]
Type=oneshot
User=$RUN_USER
ExecStart=/usr/bin/python3 $APP_DIR/current/analytics/collect.py
UNIT
cat > /etc/systemd/system/analytics-collect.timer <<'UNIT'
[Unit]
Description=Run analytics collection every 5 minutes
[Timer]
OnBootSec=2min
OnUnitActiveSec=5min
[Install]
WantedBy=timers.target
UNIT
cat > /etc/systemd/system/analytics-render.service <<UNIT
[Unit]
Description=Render the analytics admin page
[Service]
Type=oneshot
User=$RUN_USER
ExecStart=/usr/bin/python3 $APP_DIR/current/analytics/render.py --days 30 --html
UNIT
cat > /etc/systemd/system/analytics-render.timer <<'UNIT'
[Unit]
Description=Render the analytics page hourly
[Timer]
OnBootSec=5min
OnUnitActiveSec=1h
[Install]
WantedBy=timers.target
UNIT
systemctl daemon-reload
systemctl enable --now analytics-collect.timer analytics-render.timer
systemctl start analytics-collect.service && systemctl start analytics-render.service && ok "first collection and render ran"
systemctl list-timers analytics-* --no-pager | head -5

echo; echo "######## Done"
echo "Admin page: https://agentscanplay.com/_admin/  (user $ADMIN_USER)"
echo "Text report any time: sudo -u $RUN_USER python3 $APP_DIR/current/analytics/render.py --days 7"
