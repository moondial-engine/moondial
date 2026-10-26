#!/usr/bin/env bash
# Runs ON moondial-1 as the deploy user; the GitHub workflow pipes it over SSH:
#   ssh game "bash -s -- <release>" < deploy/server/remote-deploy.sh
# The release folder holds the static `moondial-driver` binary and deploy/. Switch the
# `current` symlink, restart the service, wait for /health, roll back if it does not come up.
set -euo pipefail

APP_DIR=/srv/moondial
KEEP_RELEASES=5
REL_NAME="${1:?release name required}"
REL="$APP_DIR/releases/$REL_NAME"

log() { printf '\n==> %s\n' "$*"; }

[ -x "$REL/moondial-driver" ] || { echo "No executable moondial-driver in release, aborting"; exit 1; }
"$REL/moondial-driver" --version >/dev/null 2>&1 || echo "(binary does not answer --version; continuing)"

PREV=$(readlink -f "$APP_DIR/current" 2>/dev/null || true)
point_current() {
  ln -sfn "$1" "$APP_DIR/current.tmp"
  mv -Tf "$APP_DIR/current.tmp" "$APP_DIR/current"
}

healthy() {
  for _ in $(seq 1 20); do
    if out=$(curl -fsS --max-time 2 http://127.0.0.1:7777/health 2>/dev/null) && printf '%s' "$out" | grep -q '"ok":true'; then
      echo "health: $out"
      return 0
    fi
    sleep 0.5
  done
  return 1
}

log "Switching current -> $REL_NAME"
point_current "$REL"
sudo systemctl restart moondial

if healthy; then
  echo "driver is up"
else
  echo "driver did not answer /health; last log lines:"
  journalctl -u moondial -n 30 --no-pager || true
  if [ -n "$PREV" ] && [ -d "$PREV" ]; then
    echo "rolling back to $(basename "$PREV")"
    point_current "$PREV"
    sudo systemctl restart moondial
    healthy && echo "rollback is up" || echo "rollback is ALSO down; look at journalctl -u moondial"
  fi
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
