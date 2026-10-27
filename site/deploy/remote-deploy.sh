#!/usr/bin/env bash
# Runs ON the VPS as the deploy user; the GitHub workflow pipes it over SSH:
#   ssh vps "bash -s -- <release>" < deploy/remote-deploy.sh
# The release folder already holds sites/ and deploy/. Static files only: switch the
# `current` symlink, check both sites answer through nginx, roll back if not, prune old releases.
set -euo pipefail

APP_DIR=/srv/agentscanplay
KEEP_RELEASES=5
REL_NAME="${1:?release name required}"
REL="$APP_DIR/releases/$REL_NAME"

log() { printf '\n==> %s\n' "$*"; }

[ -f "$REL/sites/agentscanplay/index.html" ] || { echo "No agentscanplay site in release, aborting"; exit 1; }
[ -f "$REL/sites/agentscanplay/invitation.txt" ] || { echo "No invitation in release, aborting"; exit 1; }
[ -f "$REL/sites/moondial/index.html" ] || { echo "No moondial site in release, aborting"; exit 1; }

PREV=$(readlink -f "$APP_DIR/current" || true)
point_current() {
  ln -sfn "$1" "$APP_DIR/current.tmp"
  mv -Tf "$APP_DIR/current.tmp" "$APP_DIR/current"
}

check() {
  # Through nginx on the local interface, by host name; scheme http is fine, the
  # redirect to https is a 301 and we follow it to the TLS listener.
  curl -fsSL -o /dev/null --resolve agentscanplay.com:443:127.0.0.1 --resolve agentscanplay.com:80:127.0.0.1 \
       -H 'Accept: text/html' "http://agentscanplay.com/" || return 1
  curl -fsSL --resolve agentscanplay.com:443:127.0.0.1 --resolve agentscanplay.com:80:127.0.0.1 \
       "http://agentscanplay.com/" | grep -q '^FROM: disremembered' || return 1
  curl -fsSL -o /dev/null --resolve moondial.ai:443:127.0.0.1 --resolve moondial.ai:80:127.0.0.1 \
       "http://moondial.ai/" || return 1
}

log "Switching current -> $REL_NAME"
point_current "$REL"

if check; then
  echo "both sites answer; the root serves CLEAR to non-browsers"
else
  echo "health check failed; rolling back"
  if [ -n "$PREV" ] && [ -d "$PREV" ]; then point_current "$PREV"; fi
  exit 1
fi

log "Pruning old releases (keeping $KEEP_RELEASES)"
cd "$APP_DIR/releases"
ls -1dt -- */ 2>/dev/null | tail -n +$((KEEP_RELEASES + 1)) | while read -r old; do
  old=${old%/}
  [ "$APP_DIR/releases/$old" = "$(readlink -f "$APP_DIR/current")" ] && continue
  rm -rf -- "$old" && echo "removed $old"
done

log "Deployed $REL_NAME"
