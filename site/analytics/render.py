#!/usr/bin/env python3
"""Render the analytics store into a report: a text summary on stdout, and (with --html)
the interim admin page at /srv/agentscanplay/shared/analytics/html/index.html.

The questions the report answers are the ones this site exists to answer:
  - How many requests, and from what kind of visitor: browsers, AI crawlers, tools, agents?
  - Of the hits on "/", how many were served CLEAR text versus the HTML page?
  - Who is reading llms.txt, the invitation, the world card, the checker?
  - Where do the humans come from?

Standard library only. Usage: render.py [--days N] [--html]
"""

from __future__ import annotations

import argparse
import html
import os
import sqlite3
from datetime import datetime, timezone, timedelta

DB = os.environ.get("ANALYTICS_DB", "/srv/agentscanplay/shared/analytics/analytics.db")
HTML_DIR = os.environ.get("ANALYTICS_HTML", "/srv/agentscanplay/shared/analytics/html")

KEY_PATHS = ["/", "/invitation.txt", "/llms.txt", "/.well-known/world-card.json", "/clear/clear-spec-v0.3.md",
             "/clear/entropic.yaml", "/clear/clear_check.py", "/robots.txt",
             # the game server
             "/health", "/v0/commands", "/v0/command", "/v0/log"]


def q(db, sql, args=()):
    return db.execute(sql, args).fetchall()


def report(db: sqlite3.Connection, days: int) -> dict:
    since = (datetime.now(timezone.utc) - timedelta(days=days)).strftime("%Y-%m-%d")
    w = "WHERE day >= ? AND status < 300"  # redirects (http->https) are not visits
    out = {"since": since, "days": days}
    out["totals"] = q(db, f"SELECT host, COUNT(*), COUNT(DISTINCT visitor) FROM requests {w} GROUP BY host ORDER BY 2 DESC", (since,))
    out["by_class"] = q(db, f"SELECT host, ua_class, COUNT(*), COUNT(DISTINCT visitor) FROM requests {w} GROUP BY host, ua_class ORDER BY host, 3 DESC", (since,))
    out["root_split"] = q(db, f"SELECT host, accept_kind, ua_class, COUNT(*) FROM requests {w} AND path = '/' GROUP BY host, accept_kind, ua_class ORDER BY host, 4 DESC", (since,))
    out["key_paths"] = q(db, f"SELECT host, path, COUNT(*), COUNT(DISTINCT visitor) FROM requests {w} AND path IN ({','.join('?' * len(KEY_PATHS))}) GROUP BY host, path ORDER BY host, 3 DESC", (since, *KEY_PATHS))
    out["agents"] = q(db, f"SELECT ua_class, ua_name, COUNT(*), COUNT(DISTINCT visitor), MAX(ts) FROM requests {w} AND ua_class IN ('ai-crawler','tool','agent') GROUP BY ua_class, ua_name ORDER BY 3 DESC LIMIT 40", (since,))
    out["referers"] = q(db, f"SELECT referer_host, COUNT(*) FROM requests {w} AND referer_host != '' AND referer_host NOT IN ('agentscanplay.com','www.agentscanplay.com','moondial.ai','www.moondial.ai') GROUP BY referer_host ORDER BY 2 DESC LIMIT 25", (since,))
    out["daily"] = q(db, f"SELECT day, host, COUNT(*), COUNT(DISTINCT visitor) FROM requests {w} GROUP BY day, host ORDER BY day DESC, host", (since,))
    out["errors"] = q(db, "SELECT host, path, status, COUNT(*) FROM requests WHERE day >= ? AND status >= 400 GROUP BY host, path, status ORDER BY 4 DESC LIMIT 20", (since,))
    return out


def text(r: dict) -> str:
    lines = [f"Analytics since {r['since']} ({r['days']} days), 2xx requests only unless noted (redirects excluded)", ""]
    lines.append("Totals (host, requests, unique visitors)")
    for h, n, v in r["totals"]:
        lines.append(f"  {h:<22} {n:>7} {v:>7}")
    lines.append("\nBy visitor class (host, class, requests, uniques)")
    for h, c, n, v in r["by_class"]:
        lines.append(f"  {h:<22} {c:<11} {n:>7} {v:>7}")
    lines.append("\nRoot '/' served as (host, accept, class, requests): text = the CLEAR invitation, html = the page")
    for h, a, c, n in r["root_split"]:
        lines.append(f"  {h:<22} {a:<6} {c:<11} {n:>7}")
    lines.append("\nKey paths (host, path, requests, uniques)")
    for h, p, n, v in r["key_paths"]:
        lines.append(f"  {h:<22} {p:<34} {n:>7} {v:>7}")
    lines.append("\nAgents, AI crawlers and tools (class, name, requests, uniques, last seen)")
    for c, name, n, v, last in r["agents"]:
        lines.append(f"  {c:<11} {name:<24} {n:>7} {v:>7}  {last}")
    lines.append("\nReferrers")
    for ref, n in r["referers"]:
        lines.append(f"  {ref:<40} {n:>7}")
    lines.append("\nErrors (host, path, status, count)")
    for h, p, s, n in r["errors"]:
        lines.append(f"  {h:<22} {p:<34} {s:>4} {n:>6}")
    return "\n".join(lines)


def table(headers, rows) -> str:
    th = "".join(f"<th>{html.escape(str(h))}</th>" for h in headers)
    body = "".join("<tr>" + "".join(f"<td>{html.escape(str(c))}</td>" for c in row) + "</tr>" for row in rows)
    return f"<table><thead><tr>{th}</tr></thead><tbody>{body or '<tr><td colspan=9>nothing yet</td></tr>'}</tbody></table>"


def page(r: dict) -> str:
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    sections = [
        ("Totals", table(["host", "requests", "unique visitors"], r["totals"])),
        ("By visitor class", table(["host", "class", "requests", "uniques"], r["by_class"])),
        ("Root / served as", table(["host", "accept", "class", "requests"], r["root_split"])),
        ("Key paths", table(["host", "path", "requests", "uniques"], r["key_paths"])),
        ("Agents, AI crawlers, tools", table(["class", "name", "requests", "uniques", "last seen"], r["agents"])),
        ("Referrers", table(["referrer", "requests"], r["referers"])),
        ("Daily", table(["day", "host", "requests", "uniques"], r["daily"])),
        ("Errors", table(["host", "path", "status", "count"], r["errors"])),
    ]
    body = "".join(f"<h2>{html.escape(t)}</h2>{tb}" for t, tb in sections)
    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex, nofollow"><title>Moondial analytics</title>
<style>
:root {{ --night:#0b0d12; --ink:#e8e4da; --dim:#9a948a; --dust:#d9b57a; --line:#262a33; --panel:#11141b; }}
html {{ background: var(--night); color: var(--ink); font: 15px/1.5 "Iowan Old Style", Palatino, Georgia, serif; }}
body {{ margin: 0; }} main {{ max-width: 960px; margin: 0 auto; padding: 32px 20px 80px; }}
h1 {{ font-weight: 400; font-size: 32px; margin: 0 0 4px; }} .sub {{ color: var(--dim); margin: 0 0 24px; }}
h2 {{ font-weight: 400; font-size: 20px; color: var(--dust); margin: 32px 0 8px; }}
table {{ border-collapse: collapse; width: 100%; font: 13px/1.4 ui-monospace, Menlo, Consolas, monospace; }}
th, td {{ text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--line); }} th {{ color: var(--dim); font-weight: 400; }}
td:nth-child(n+3) {{ text-align: right; }} tr:hover td {{ background: var(--panel); }}
</style></head><body><main>
<h1>Moondial analytics</h1>
<p class="sub">Since {html.escape(r['since'])} ({r['days']} days) · rendered {now} · 2xx requests unless noted · uniques are daily-salted hashes, no IPs stored</p>
{body}
</main></body></html>"""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--days", type=int, default=30)
    ap.add_argument("--html", action="store_true", help="also write the admin page")
    args = ap.parse_args()
    db = sqlite3.connect(DB)
    r = report(db, args.days)
    print(text(r))
    if args.html:
        os.makedirs(HTML_DIR, exist_ok=True)
        tmp = os.path.join(HTML_DIR, "index.html.tmp")
        with open(tmp, "w", encoding="utf-8") as fh:
            fh.write(page(r))
        os.replace(tmp, os.path.join(HTML_DIR, "index.html"))
        print(f"\nwrote {HTML_DIR}/index.html")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
