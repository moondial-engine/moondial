#!/usr/bin/env bash
# One-time setup of agentscanplay.com and moondial.ai on the VPS. Run as root.
# From Windows PowerShell:
#   cmd /c 'ssh root@5.78.104.64 "CERT_EMAIL=you@example.com bash -s" < deploy/setup-server.sh > setup-server.log'
#
# - creates /srv/agentscanplay/{releases,current} owned by the deploy user (same user potesmith deploys as)
# - seeds a placeholder release so nginx has a root before the first GitHub deploy
# - installs both nginx server blocks and enables them
# - runs certbot --nginx for both domains (DNS must already point here, which it does)
# The deploy key is the one potesmith already uses; nothing to add for SSH.
set -uo pipefail
exec 2>&1

RUN_USER=${RUN_USER:-starsailor}
APP_DIR=/srv/agentscanplay
CERT_EMAIL=${CERT_EMAIL:?set CERT_EMAIL=you@example.com for certificate notices}
DOMAINS=(agentscanplay.com www.agentscanplay.com moondial.ai www.moondial.ai)
PENDING_FULL=()

ok()   { echo "OK:   $*"; }
fail() { echo "FAIL: $*"; }

[ "$(id -u)" -eq 0 ] || { echo "Run as root"; exit 1; }
id "$RUN_USER" >/dev/null 2>&1 || { fail "user $RUN_USER does not exist"; exit 1; }
command -v nginx >/dev/null || { fail "nginx not installed"; exit 1; }
command -v certbot >/dev/null || { fail "certbot not installed (apt install certbot python3-certbot-nginx)"; exit 1; }

echo "######## Directories"
install -d -o "$RUN_USER" -g "$RUN_USER" "$APP_DIR" "$APP_DIR/releases"
if [ ! -L "$APP_DIR/current" ]; then
  INIT=$APP_DIR/releases/00000000000000-initial
  install -d -o "$RUN_USER" -g "$RUN_USER" "$INIT/sites/agentscanplay" "$INIT/sites/moondial"
  for site in agentscanplay moondial; do
    printf '<!doctype html><meta charset="utf-8"><title>%s</title><p>Deploying soon.</p>\n' "$site" > "$INIT/sites/$site/index.html"
    printf 'FROM: disremembered\nTO: any-agent\nINTENT: INFORM\n\n[FACT] This site is deploying. [src:setup-server]\n' > "$INIT/sites/$site/invitation.txt"
  done
  chown -R "$RUN_USER:$RUN_USER" "$INIT"
  ln -sfn "$INIT" "$APP_DIR/current"
  chown -h "$RUN_USER:$RUN_USER" "$APP_DIR/current"
  ok "placeholder release at $INIT, current -> initial"
else
  ok "current already points at $(readlink "$APP_DIR/current")"
fi

echo; echo "######## nginx"
# The server blocks are piped in after this script? No: they live in the repo. The workflow
# rsyncs deploy/ into each release, so take them from the current release when present,
# else from /tmp where the operator scp'd them.
for name in agentscanplay moondial; do
  SRC=""
  for cand in "$APP_DIR/current/deploy/nginx-$name.conf" "/tmp/nginx-$name.conf"; do
    [ -f "$cand" ] && { SRC=$cand; break; }
  done
  if [ -z "$SRC" ]; then
    fail "nginx-$name.conf not found; scp deploy/nginx-$name.conf to /tmp/ first"; exit 1
  fi
  # A config that carries its own 443 blocks (agentscanplay) names certificate files
  # that do not exist before certbot has run. Until they do, install the port-80
  # blocks only, so nginx can start and certbot can answer the challenge.
  domain=$([ "$name" = agentscanplay ] && echo agentscanplay.com || echo moondial.ai)
  if grep -q 'listen 443' "$SRC" && [ ! -f "/etc/letsencrypt/live/$domain/fullchain.pem" ]; then
    cat > "/etc/nginx/sites-available/$name" <<STUB
server {
    listen 80;
    listen [::]:80;
    server_name $domain www.$domain;
    root $APP_DIR/current/sites/$name;
    index index.html;
}
STUB
    ok "installed a port-80 stub for $name (no certificate yet); the full config follows certbot"
    PENDING_FULL+=("$name:$SRC")
  else
    install -m 644 "$SRC" "/etc/nginx/sites-available/$name"
    ok "installed server block for $name from $SRC"
  fi
  ln -sfn "/etc/nginx/sites-available/$name" "/etc/nginx/sites-enabled/$name"
done
nginx -t && systemctl reload nginx && ok "nginx reloaded" || { fail "nginx config test failed"; exit 1; }

echo; echo "######## Certificates"
ARGS=()
for d in "${DOMAINS[@]}"; do ARGS+=(-d "$d"); done
if certbot --nginx --non-interactive --agree-tos --redirect -m "$CERT_EMAIL" "${ARGS[@]}"; then
  ok "certificates issued and HTTPS redirect added for: ${DOMAINS[*]}"
else
  fail "certbot failed; check that DNS for all four names resolves to this host, then rerun"
fi
# Now the certificate files exist: a config that carries its own 443 blocks replaces
# what certbot wrote, so the live file and the repo's are one file.
for pair in "${PENDING_FULL[@]:-}"; do
  [ -n "$pair" ] || continue
  name=${pair%%:*}; SRC=${pair#*:}
  install -m 644 "$SRC" "/etc/nginx/sites-available/$name" && ok "installed full config for $name from $SRC"
done
nginx -t && systemctl reload nginx

echo; echo "######## Done"
echo "Deploy user: $RUN_USER   App dir: $APP_DIR"
echo "GitHub secrets: reuse potesmith's VPS_HOST, VPS_SSH_KEY, VPS_KNOWN_HOSTS; var VPS_USER=$RUN_USER"
