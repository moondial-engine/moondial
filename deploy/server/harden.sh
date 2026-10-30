#!/usr/bin/env bash
# Harden the game server. Run as root, once; safe to re-run:
#   sudo bash /srv/moondial/current/deploy/harden.sh
#
# - nginx rate limits on the endpoints that cost CPU or guess passphrases
#   (/v0/login, /v0/gate, /v0/check, /mcp), and a connection cap per address
# - the raw text port bound to localhost: passphrases and the admin token travel
#   over it in the clear, so it is not for the internet (`nc play... 7778` stops
#   working; the HTTP and WebSocket ports carry everything it did, over TLS)
# - unattended security upgrades
# - fail2ban on sshd
# - a nightly pg_dump into $APP_DIR/shared/backups, kept 14 days
set -euo pipefail

APP_DIR=${APP_DIR:-/srv/moondial}
ENV_FILE=/etc/moondial/driver.env
NGINX_SITE=${NGINX_SITE:-/etc/nginx/sites-available/play}
RUN_USER=${RUN_USER:-starsailor}

ok() { printf '  OK  %s\n' "$*"; }
die() { printf '  !!  %s\n' "$*" >&2; exit 1; }
[ "$(id -u)" = 0 ] || die "run as root"
[ -f "$ENV_FILE" ] || die "$ENV_FILE is missing; run setup-moondial-1.sh first"
[ -f "$NGINX_SITE" ] || die "$NGINX_SITE not found; pass NGINX_SITE=/etc/nginx/sites-available/<site>"

echo "######## nginx rate limits"
# Zones live in the http context; conf.d is included there.
cat > /etc/nginx/conf.d/moondial-limits.conf <<'CONF'
# Written by deploy/server/harden.sh. Per client address.
limit_req_zone  $binary_remote_addr zone=moondial_login:1m  rate=10r/m;
limit_req_zone  $binary_remote_addr zone=moondial_gate:1m   rate=60r/m;
limit_req_zone  $binary_remote_addr zone=moondial_check:1m  rate=120r/m;
limit_conn_zone $binary_remote_addr zone=moondial_conn:1m;
limit_req_status 429;
limit_conn_status 429;
CONF
install -d /etc/nginx/snippets
cat > /etc/nginx/snippets/moondial-ratelimit.conf <<'CONF'
# Written by deploy/server/harden.sh; included from the play server block.
limit_conn moondial_conn 40;

location = /v0/login {
    limit_req zone=moondial_login burst=5 nodelay;
    proxy_pass http://127.0.0.1:7777;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
}
location ~ ^/(v0/(gate|command)|mcp)$ {
    limit_req zone=moondial_gate burst=20 nodelay;
    proxy_pass http://127.0.0.1:7777;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_read_timeout 60s;
}
location = /v0/check {
    limit_req zone=moondial_check burst=30 nodelay;
    proxy_pass http://127.0.0.1:7777;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
}
CONF
# Include it in every server block of the site (certbot made two: 80 and 443).
if grep -q "snippets/moondial-ratelimit.conf" "$NGINX_SITE"; then
  ok "$NGINX_SITE already includes the rate limits"
else
  sed -i 's|^\(\s*\)server_name play.agentscanplay.com;|&\n\1include snippets/moondial-ratelimit.conf;|' "$NGINX_SITE"
  ok "added the include to $NGINX_SITE"
fi
nginx -t && systemctl reload nginx
ok "nginx reloaded"

echo; echo "######## Text port on localhost only"
if grep -q '^MOONDIAL_TCP=0.0.0.0:' "$ENV_FILE"; then
  sed -i 's|^MOONDIAL_TCP=0.0.0.0:|MOONDIAL_TCP=127.0.0.1:|' "$ENV_FILE"
  systemctl restart moondial
  ok "MOONDIAL_TCP now 127.0.0.1:7778; driver restarted (close 7778 in the Hetzner firewall too)"
else
  ok "text port already $(grep '^MOONDIAL_TCP=' "$ENV_FILE" | cut -d= -f2)"
fi

echo; echo "######## Unattended security upgrades"
export DEBIAN_FRONTEND=noninteractive
apt-get install -y -q unattended-upgrades >/dev/null
cat > /etc/apt/apt.conf.d/20auto-upgrades <<'CONF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
APT::Periodic::AutocleanInterval "7";
CONF
systemctl enable --now unattended-upgrades >/dev/null
ok "unattended-upgrades on (security pocket, Debian/Ubuntu default origins)"

echo; echo "######## fail2ban (sshd)"
apt-get install -y -q fail2ban >/dev/null
cat > /etc/fail2ban/jail.d/moondial.conf <<'CONF'
[sshd]
enabled = true
maxretry = 5
findtime = 10m
bantime = 1h
CONF
systemctl enable --now fail2ban >/dev/null
systemctl restart fail2ban
ok "fail2ban watching sshd"

echo; echo "######## Nightly pg_dump"
if grep -q '^MOONDIAL_LOG_URL=' "$ENV_FILE"; then
  BK="$APP_DIR/shared/backups"
  install -d -m 750 -o postgres -g "$RUN_USER" "$BK"
  cat > /etc/cron.d/moondial-backup <<CRON
# Written by deploy/server/harden.sh: the event log, nightly, kept 14 days.
17 3 * * * postgres pg_dump -Fc moondial > $BK/moondial-\$(date +\\%F).dump && find $BK -name 'moondial-*.dump' -mtime +14 -delete
CRON
  chmod 644 /etc/cron.d/moondial-backup
  su postgres -c "pg_dump -Fc moondial > $BK/moondial-$(date +%F).dump"
  ok "first dump written to $BK; restore with: pg_restore -d moondial <file>"
else
  ok "no MOONDIAL_LOG_URL in $ENV_FILE (file log); nothing to dump"
fi

echo; echo "######## Secrets"
chmod 640 "$ENV_FILE"; chown root:"$RUN_USER" "$ENV_FILE"
ok "$ENV_FILE is root:$RUN_USER 640"
echo
echo "Done. Still yours to do: close 7778 in the Hetzner firewall; rotate the analytics"
echo "password if it was ever pasted anywhere; keep the MCP publishing key at mode 600."
