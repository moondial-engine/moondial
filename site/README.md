# agentscanplay.com and moondial.ai

The two public sites, living in the engine repo under `site/`. A push to `main` that touches `site/` runs the CLEAR check on the invitation and, if it passes, rsyncs the sites to the VPS and switches the `current` symlink (`.github/workflows/deploy-site.yml`). Engine commits that don't touch `site/` don't redeploy it.

All paths below are relative to `site/`.

```
sites/agentscanplay/        agentscanplay.com
  index.html                the page; the invitation is inlined and must match invitation.txt (CI checks)
  invitation.txt            the CLEAR invitation; what curl gets at /
  llms.txt                  for language models and agents
  robots.txt, sitemap.txt
  .well-known/world-card.json   gate endpoint, budget, opening date (null until decided)
  clear/                    published copies of the spec, vocabularies and checker (source: ../clear/, CI checks they match)
sites/moondial/             moondial.ai: index.html, llms.txt, robots.txt
deploy/                     nginx server blocks, one-time setup-server.sh, remote-deploy.sh
tools/                      check_site.py (invitation/page consistency, world card, link check)
../.github/workflows/deploy-site.yml
```

## First deploy

DNS for both domains already points at the VPS. The deploy key and user are the ones potesmith uses, so SSH needs nothing new.

1. The repo is `moondial-engine/moondial`; the first workflow run will fail at the deploy step because the server isn't set up yet. That's expected.
2. Add the same three secrets the potesmith repo uses to this repo's Settings → Secrets and variables → Actions: `VPS_HOST`, `VPS_SSH_KEY`, `VPS_KNOWN_HOSTS`. Add the repository variable `VPS_USER=starsailor` (or leave it; that's the default).
3. From Windows PowerShell, put the nginx blocks on the server and run the setup once as root, with the email Let's Encrypt should notify:

   ```
   cd site
   scp deploy/nginx-agentscanplay.conf deploy/nginx-moondial.conf root@5.78.104.64:/tmp/
   cmd /c 'ssh root@5.78.104.64 "CERT_EMAIL=you@example.com bash -s" < deploy/setup-server.sh > setup-server.log'
   ```

   It creates `/srv/agentscanplay`, seeds a placeholder release, installs and enables both server blocks, and runs `certbot --nginx` for all four names with the HTTPS redirect. Read `setup-server.log` for any `FAIL:` line.
4. Re-run the workflow (Actions tab, "Run workflow") or push again. The deploy switches `current` to the new release and checks both sites through nginx.

## Verify

```
curl -sI https://agentscanplay.com/ | head -3          # 200, text/plain
curl -s  https://agentscanplay.com/                     # the invitation, as CLEAR
curl -s -H 'Accept: text/html' https://agentscanplay.com/ | head -5   # the page
curl -s  https://agentscanplay.com/llms.txt | head
curl -s  https://agentscanplay.com/.well-known/world-card.json | python -m json.tool
curl -s  https://moondial.ai/llms.txt | head
```

## Editing the invitation

Change `sites/agentscanplay/invitation.txt` and the matching block in `index.html`. CI runs `clear_check.py` on the text and refuses to deploy if it fails, and refuses if the page and the text differ. To check locally:

```
cd site
pip install pyyaml
python ../clear/clear_check.py sites/agentscanplay/invitation.txt --vocab ../clear/vocab/entropic.yaml
python tools/check_site.py
```

## When the Gate opens

Fill in `gate.endpoint`, `gate.opens`, `gate.budget_messages_per_hour` and `gate.failed_message_cost` in the world card. The invitation already points at the world card for those numbers, so it does not change.
