#!/usr/bin/env python3
"""Pre-deploy checks for the sites. Fails the build if the site contradicts itself.

1. The invitation shown in index.html is byte-for-byte the invitation served as text.
2. The world card is valid JSON and names the same CLEAR version as the invitation.
3. Every local link in each llms.txt, sitemap and index.html points at a file in the repo.
"""

import html
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SITES = os.path.join(ROOT, "sites")
problems: list[str] = []


def read(*parts: str) -> str:
    with open(os.path.join(*parts), encoding="utf-8") as fh:
        return fh.read()


# 1. invitation in the page == invitation.txt
site = os.path.join(SITES, "agentscanplay")
page = read(site, "index.html")
m = re.search(r'<pre class="clear" id="invitation">(.*?)</pre>', page, re.S)
if not m:
    problems.append("index.html has no <pre id=\"invitation\"> block")
else:
    shown = re.sub(r"<[^>]+>", "", m.group(1))
    shown = html.unescape(shown).strip() + "\n"
    served = read(site, "invitation.txt")
    if shown != served:
        problems.append("the invitation in index.html differs from invitation.txt")
        for i, (a, b) in enumerate(zip(shown.splitlines(), served.splitlines()), 1):
            if a != b:
                problems.append(f"  first difference at line {i}:\n    page: {a}\n    text: {b}")
                break

# 2. world card
try:
    card = json.loads(read(site, ".well-known", "world-card.json"))
    ver = card.get("gate", {}).get("clear_version")
    inv = read(site, "invitation.txt")
    if ver and f"CLEAR v{ver}" not in inv:
        problems.append(f"world card says CLEAR {ver} but the invitation does not mention CLEAR v{ver}")
except (OSError, ValueError) as exc:
    problems.append(f"world-card.json: {exc}")

# 3. local links resolve
LINK_RE = re.compile(r"https?://(agentscanplay\.com|moondial\.ai)(/[^\s)\"'<>]*)")
HREF_RE = re.compile(r'href="(/[^"]*)"')
for name in ("agentscanplay", "moondial"):
    base = os.path.join(SITES, name)
    for fname in ("llms.txt", "robots.txt", "sitemap.txt", "index.html"):
        path = os.path.join(base, fname)
        if not os.path.exists(path):
            continue
        text = read(path)
        targets = [(host, p) for host, p in LINK_RE.findall(text)]
        targets += [(f"{name}.com" if name == "agentscanplay" else "moondial.ai", p) for p in HREF_RE.findall(text)]
        for host, p in targets:
            site_dir = "agentscanplay" if host.startswith("agentscanplay") else "moondial"
            rel = p.split("#")[0].split("?")[0]
            if rel in ("", "/"):
                continue
            local = os.path.join(SITES, site_dir, rel.lstrip("/"))
            if not (os.path.isfile(local) or os.path.isfile(os.path.join(local, "index.html"))):
                problems.append(f"{name}/{fname}: link {host}{p} has no file at sites/{site_dir}{rel}")

if problems:
    print("site check FAILED")
    for p in problems:
        print(" -", p)
    sys.exit(1)
print("site check OK: invitation matches, world card valid, all local links resolve")
