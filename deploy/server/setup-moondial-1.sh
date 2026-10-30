#!/usr/bin/env bash
# One-time setup of the game server (moondial-1). Run as root, once. Re-running is safe.
# From Windows PowerShell, with the deploy key's PUBLIC half on the clipboard or in a file:
#   cmd /c 'ssh root@5.78.210.32 "CERT_EMAIL=you@example.com DEPLOY_PUBKEY=\"ssh-ed25519 AAAA... deploy\" bash -s" < deploy/server/setup-moondial-1.sh > setup-moondial-1.log'
#
# What it does:
# - creates the deploy user (same name as on the web server) and authorizes the deploy key
# - creates /srv/moondial/{releases,current,shared/data}; the event log lives in shared/data
#   and survives every release
# - installs the systemd unit and an env file with a fresh admin token
# - installs nginx + certbot, the play.agentscanplay.com server block (proxy to the driver's
#   HTTP port), and requests the certificate (DNS for play.agentscanplay.com must point here)
# - installs a nightly backup timer for the event log
# - does NOT open the firewall: do that in the Hetzner console (22, 80, 443 only; the raw
#   text port carries passphrases in the clear and stays on localhost, see harden.sh)
set -uo pipefail
exec 2>&1

RUN_USER=${RUN_USER:-starsailor}
APP_DIR=/srv/moondial
DOMAIN=${DOMAIN:-play.agentscanplay.com}
CERT_EMAIL=${CERT_EMAIL:?set CERT_EMAIL=you@example.com for certificate notices}
DEPLOY_PUBKEY=${DEPLOY_PUBKEY:-}

ok()   { echo "OK:   $*"; }
fail() { echo "FAIL: $*"; }
[ "$(id -u)" -eq 0 ] || { echo "Run as root"; exit 1; }

echo "######## Packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq && apt-get install -y -qq nginx certbot python3-certbot-nginx rsync curl >/dev/null && ok "nginx, certbot, rsync, curl installed"

echo; echo "######## Deploy user"
if ! id "$RUN_USER" >/dev/null 2>&1; then
  useradd --create-home --shell /bin/bash "$RUN_USER" && ok "created $RUN_USER"
else
  ok "$RUN_USER exists"
fi
install -d -m 700 -o "$RUN_USER" -g "$RUN_USER" "/home/$RUN_USER/.ssh"
AK="/home/$RUN_USER/.ssh/authorized_keys"
touch "$AK"; chmod 600 "$AK"; chown "$RUN_USER:$RUN_USER" "$AK"
if [ -n "$DEPLOY_PUBKEY" ]; then
  grep -qF "$DEPLOY_PUBKEY" "$AK" || echo "$DEPLOY_PUBKEY" >> "$AK"
  ok "deploy key authorized for $RUN_USER"
elif [ -s "$AK" ]; then
  ok "no DEPLOY_PUBKEY given; $AK already has $(wc -l < "$AK") key(s)"
else
  fail "no DEPLOY_PUBKEY given and $AK is empty: GitHub cannot deploy until the deploy key's public half is added"
fi
# The deploy user may restart the driver and nothing else.
cat > /etc/sudoers.d/moondial <<SUDO
$RUN_USER ALL=(root) NOPASSWD: /bin/systemctl restart moondial, /bin/systemctl stop moondial, /bin/systemctl start moondial, /bin/systemctl status moondial, /bin/systemctl is-active moondial
SUDO
chmod 440 /etc/sudoers.d/moondial && visudo -cf /etc/sudoers.d/moondial >/dev/null && ok "sudoers: $RUN_USER may restart moondial"

echo; echo "######## Directories"
install -d -o "$RUN_USER" -g "$RUN_USER" "$APP_DIR" "$APP_DIR/releases" "$APP_DIR/shared" "$APP_DIR/shared/data" "$APP_DIR/backups"
ok "$APP_DIR ready; event log will live in $APP_DIR/shared/data"

echo; echo "######## Driver environment"
install -d -m 750 -o root -g "$RUN_USER" /etc/moondial
if [ ! -f /etc/moondial/driver.env ]; then
  TOKEN=$(head -c 32 /dev/urandom | base64 | tr -d '/+=' | head -c 40)
  cat > /etc/moondial/driver.env <<ENV
# Read by moondial.service. Edit, then: sudo systemctl restart moondial
MOONDIAL_WORLD=disremembered
MOONDIAL_DATA=$APP_DIR/shared/data
MOONDIAL_HTTP=127.0.0.1:7777
MOONDIAL_TCP=127.0.0.1:7778
MOONDIAL_ADMIN_TOKEN=$TOKEN
MOONDIAL_ACTIVE_HALF_LIFE_SECONDS=3600
RUST_BACKTRACE=1
ENV
  chmod 640 /etc/moondial/driver.env; chown root:"$RUN_USER" /etc/moondial/driver.env
  ok "wrote /etc/moondial/driver.env (admin token inside; read it with: sudo cat /etc/moondial/driver.env)"
else
  ok "/etc/moondial/driver.env exists, left alone"
fi

echo; echo "######## systemd"
cat > /etc/systemd/system/moondial.service <<UNIT
[Unit]
Description=Moondial driver (Disremembered)
After=network-online.target
Wants=network-online.target

[Service]
User=$RUN_USER
Group=$RUN_USER
EnvironmentFile=/etc/moondial/driver.env
WorkingDirectory=$APP_DIR/current
ExecStart=$APP_DIR/current/moondial-driver
Restart=always
RestartSec=2
# Hardening: the driver may write only its data directory.
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=$APP_DIR/shared/data
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload && systemctl enable moondial >/dev/null 2>&1 && ok "moondial.service installed and enabled (starts on first deploy)"

echo; echo "######## Backups"
cat > /etc/systemd/system/moondial-backup.service <<UNIT
[Unit]
Description=Nightly copy of the Moondial event log

[Service]
Type=oneshot
User=$RUN_USER
ExecStart=/bin/bash -c 'cd $APP_DIR/shared && tar -czf $APP_DIR/backups/data-\$(date -u +%%Y%%m%%d).tar.gz data && ls -1t $APP_DIR/backups/data-*.tar.gz | tail -n +15 | xargs -r rm --'
UNIT
cat > /etc/systemd/system/moondial-backup.timer <<UNIT
[Unit]
Description=Nightly Moondial backup

[Timer]
OnCalendar=*-*-* 04:17:00 UTC
Persistent=true

[Install]
WantedBy=timers.target
UNIT
systemctl daemon-reload && systemctl enable --now moondial-backup.timer >/dev/null 2>&1 && ok "nightly backup timer enabled (keeps 14 days in $APP_DIR/backups)"

echo; echo "######## nginx"
SRC=""
for cand in "$APP_DIR/current/deploy/nginx-play.conf" "/tmp/nginx-play.conf"; do
  [ -f "$cand" ] && { SRC=$cand; break; }
done
if [ -z "$SRC" ]; then
  # Not deployed yet: write the block inline so certbot has something to attach to.
  SRC=/tmp/nginx-play.conf
  cat > "$SRC" <<NGX
server {
    listen 80;
    listen [::]:80;
    server_name $DOMAIN;
    access_log /var/log/nginx/play.access.log;
    error_log  /var/log/nginx/play.error.log;
    add_header X-Content-Type-Options nosniff always;
    location / {
        proxy_pass http://127.0.0.1:7777;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_read_timeout 60s;
    }
}
NGX
fi
install -m 644 "$SRC" /etc/nginx/sites-available/play
ln -sfn /etc/nginx/sites-available/play /etc/nginx/sites-enabled/play
rm -f /etc/nginx/sites-enabled/default
nginx -t && systemctl reload nginx && ok "nginx serves $DOMAIN -> 127.0.0.1:7777" || { fail "nginx config test failed"; exit 1; }

echo; echo "######## Certificate"
if [ -d "/etc/letsencrypt/live/$DOMAIN" ]; then
  ok "certificate for $DOMAIN exists"
elif getent hosts "$DOMAIN" >/dev/null; then
  certbot --nginx --non-interactive --agree-tos -m "$CERT_EMAIL" --redirect -d "$DOMAIN" && ok "certificate issued for $DOMAIN" || fail "certbot failed; check DNS for $DOMAIN points here, then rerun"
else
  fail "$DOMAIN does not resolve yet; add the A record, then rerun this script"
fi

echo; echo "######## Done"
echo "Next: add GAME_HOST / GAME_KNOWN_HOSTS secrets in GitHub, open ports 22/80/443 in the Hetzner firewall, run deploy/harden.sh,"
echo "then push to main or run the 'Deploy driver' workflow. First deploy starts the service."
echo "Admin token: sudo cat /etc/moondial/driver.env"
