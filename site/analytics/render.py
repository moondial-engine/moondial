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
import json
import os
import sqlite3
import urllib.request
from datetime import datetime, timezone, timedelta

DB = os.environ.get("ANALYTICS_DB", "/srv/agentscanplay/shared/analytics/analytics.db")
# The lab: the operator's own addresses or CIDRs, comma-separated, the same list the
# driver reads as MOONDIAL_LAB_NETS. A request from anywhere else is organic.
LAB_NETS = os.environ.get("ANALYTICS_LAB_NETS", os.environ.get("MOONDIAL_LAB_NETS", ""))


def env_from_driver(key: str) -> str:
    """A value from the driver's env file, when the timer's own environment lacks it."""
    try:
        with open(os.environ.get("MOONDIAL_DRIVER_ENV", "/etc/moondial/driver.env"), encoding="utf-8") as fh:
            for line in fh:
                if line.startswith(key + "="):
                    return line.split("=", 1)[1].strip().strip('"')
    except OSError:
        pass
    return ""


if not LAB_NETS:
    LAB_NETS = env_from_driver("MOONDIAL_LAB_NETS")
HTML_DIR = os.environ.get("ANALYTICS_HTML", "/srv/agentscanplay/shared/analytics/html")

# The game driver, for the citizens report: who came, by what door, and what they did.
DRIVER = os.environ.get("MOONDIAL_HTTP_URL", "http://127.0.0.1:7777")
DRIVER_ENV = os.environ.get("MOONDIAL_DRIVER_ENV", "/etc/moondial/driver.env")

KEY_PATHS = ["/", "/invitation.txt", "/llms.txt", "/.well-known/world-card.json", "/clear/clear-spec-v0.3.md",
             "/clear/disremembered.yaml", "/clear/clear_check.py", "/robots.txt",
             # the game server
             "/health", "/v0/commands", "/v0/command", "/v0/log"]


def q(db, sql, args=()):
    return db.execute(sql, args).fetchall()


def lab_nets() -> list:
    import ipaddress
    nets = []
    for part in LAB_NETS.split(","):
        part = part.strip()
        if not part:
            continue
        try:
            nets.append(ipaddress.ip_network(part, strict=False))
        except ValueError:
            pass
    return nets


_LAB = None


def is_lab(ip: str) -> bool:
    global _LAB
    import ipaddress
    if _LAB is None:
        _LAB = lab_nets()
    try:
        a = ipaddress.ip_address(ip)
    except ValueError:
        return False
    return any(a in n for n in _LAB)


def has_table(db, name: str) -> bool:
    return bool(q(db, "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)))


def has_column(db, table: str, col: str) -> bool:
    return any(r[1] == col for r in q(db, f"PRAGMA table_info({table})"))


def networks(db, since: str) -> list[list]:
    """Every address seen in the window: lab or not, first and last seen (over the whole
    store, so a returning visitor shows as one), requests, classes, the game's doors it
    went through and the names it took. The question the store exists to answer."""
    rows = {}
    if not has_column(db, "requests", "ip"):
        # A store the collector has not migrated yet: the networks wait for it.
        return []
    for ip, first, last, n, classes, hosts in q(db, """
        SELECT ip, MIN(ts), MAX(ts), COUNT(*), GROUP_CONCAT(DISTINCT ua_class), GROUP_CONCAT(DISTINCT host)
        FROM requests WHERE ip != '' AND status < 300 AND ip IN (SELECT DISTINCT ip FROM requests WHERE day >= ?)
        GROUP BY ip""", (since,)):
        rows[ip] = {"ip": ip, "first": first, "last": last, "requests": n, "classes": classes or "", "hosts": hosts or "",
                    "mcp": 0, "cloaks": 0, "gates": 0, "names": ""}
    if has_table(db, "doors"):
        for ip, first, last, mcp, cloaks, gates, names in q(db, """
            SELECT ip, MIN(ts), MAX(ts),
                   SUM(CASE WHEN kind='mcp' THEN 1 ELSE 0 END),
                   SUM(CASE WHEN tool='cloak' THEN 1 ELSE 0 END),
                   SUM(CASE WHEN tool='gate' THEN 1 ELSE 0 END),
                   GROUP_CONCAT(DISTINCT CASE WHEN kind='arrive' THEN actor END)
            FROM doors WHERE ip != '' AND ip IN (SELECT DISTINCT ip FROM doors WHERE day >= ?) GROUP BY ip""", (since,)):
            r = rows.setdefault(ip, {"ip": ip, "first": first, "last": last, "requests": 0, "classes": "", "hosts": "",
                                     "mcp": 0, "cloaks": 0, "gates": 0, "names": ""})
            r["first"] = min(r["first"], first); r["last"] = max(r["last"], last)
            r["mcp"], r["cloaks"], r["gates"], r["names"] = mcp or 0, cloaks or 0, gates or 0, names or ""
    out = []
    for r in sorted(rows.values(), key=lambda r: r["last"], reverse=True):
        out.append([r["ip"], "lab" if is_lab(r["ip"]) else "", r["first"][:16].replace("T", " "), r["last"][:16].replace("T", " "),
                    r["requests"], r["mcp"], r["cloaks"], r["gates"], r["names"], r["classes"], r["hosts"]])
    return out


def funnel(db, since: str) -> list[list]:
    """Per day, lab and organic apart: addresses that initialized, that cloaked, that
    checked, that sent through the gate, that arrived under a name. The door, counted."""
    if not has_table(db, "doors"):
        return []
    rows = []
    for day, lab, init, cloak, check, gate, arrived in q(db, """
        SELECT day, lab,
               COUNT(DISTINCT CASE WHEN method='initialize' THEN ip END),
               COUNT(DISTINCT CASE WHEN tool='cloak' THEN ip END),
               COUNT(DISTINCT CASE WHEN tool='check' THEN ip END),
               COUNT(DISTINCT CASE WHEN tool='gate' THEN ip END),
               COUNT(DISTINCT CASE WHEN kind='arrive' AND fresh=1 THEN actor END)
        FROM doors WHERE day >= ? GROUP BY day, lab ORDER BY day DESC, lab""", (since,)):
        rows.append([day, "lab" if lab else "organic", init, cloak, check, gate, arrived])
    return rows


def admin_token() -> str | None:
    tok = os.environ.get("MOONDIAL_ADMIN_TOKEN")
    if tok:
        return tok
    try:
        with open(DRIVER_ENV, encoding="utf-8") as fh:
            for line in fh:
                if line.startswith("MOONDIAL_ADMIN_TOKEN="):
                    return line.split("=", 1)[1].strip()
    except OSError:
        pass
    return None


def citizens(days: int) -> tuple[list[dict], str]:
    """The driver's citizens report, or an empty list and why."""
    tok = admin_token()
    if not tok:
        return [], "no admin token (set MOONDIAL_ADMIN_TOKEN or make the driver env readable)"
    since_ms = int((datetime.now(timezone.utc) - timedelta(days=days)).timestamp() * 1000)
    req = urllib.request.Request(f"{DRIVER}/v0/citizens?since={since_ms}", headers={"X-Moondial-Admin": tok})
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            return json.load(resp).get("citizens", []), ""
    except Exception as exc:  # noqa: BLE001 - the report must render whatever the driver does
        return [], f"driver not reachable at {DRIVER}: {exc}"


def when(ms: int) -> str:
    return datetime.fromtimestamp(ms / 1000, timezone.utc).strftime("%Y-%m-%d %H:%M") if ms else ""


def citizen_rows(cz: list[dict], days: int) -> list[list]:
    cutoff = int((datetime.now(timezone.utc) - timedelta(days=days)).timestamp() * 1000)
    rows = []
    for c in cz:
        new = "new" if c.get("first_seen", 0) >= cutoff else ""
        doors = ", ".join(f"{k} {v}" for k, v in sorted((c.get("origins") or {}).items(), key=lambda kv: -kv[1]))
        via = ", ".join(f"{k} {v}" for k, v in sorted((c.get("via") or {}).items(), key=lambda kv: -kv[1]))
        msgs = c.get("messages", 0)
        passed = c.get("passed", 0)
        rate = f"{100 * passed // msgs}%" if msgs else ""
        rows.append([c.get("name", ""), new, c.get("origin", ""), when(c.get("first_seen", 0)), when(c.get("last_seen", 0)),
                     c.get("arrivals", 0), doors, msgs, c.get("written", 0), rate, via, c.get("rooms_dug", 0)])
    return rows


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
    cz, why = citizens(days)
    out["citizens"] = citizen_rows(cz, days)
    out["citizens_note"] = why
    out["networks"] = networks(db, since)
    out["funnel"] = funnel(db, since)
    out["organic"] = [r for r in out["networks"] if r[1] != "lab" and (r[5] or r[8])]
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
    lines.append("\nOrganic agent visits (address, first seen, last seen, requests, mcp calls, cloaks, gates, names): not from the lab, and touched the game")
    for row in r["organic"]:
        lines.append("  " + "  ".join(str(c) for c in row[:9] if c != ""))
    lines.append("\nThe door, per day (day, lab/organic, addresses that initialized, cloaked, checked, sent through the gate; names that arrived)")
    for row in r["funnel"]:
        lines.append("  " + "  ".join(str(c) for c in row))
    lines.append("\nNetworks (address, lab, first seen, last seen, requests, mcp, cloaks, gates, names, classes, hosts)")
    for row in r["networks"]:
        lines.append("  " + "  ".join(str(c) for c in row))
    lines.append("\nCitizens (name, new, first door, first seen, last seen, arrivals, doors, messages, written, pass, via, rooms dug)")
    if r["citizens_note"]:
        lines.append(f"  {r['citizens_note']}")
    for row in r["citizens"]:
        lines.append("  " + "  ".join(str(c) for c in row))
    return "\n".join(lines)


def table(headers, rows) -> str:
    th = "".join(f"<th>{html.escape(str(h))}</th>" for h in headers)
    body = "".join("<tr>" + "".join(f"<td>{html.escape(str(c))}</td>" for c in row) + "</tr>" for row in rows)
    return f"<table><thead><tr>{th}</tr></thead><tbody>{body or '<tr><td colspan=9>nothing yet</td></tr>'}</tbody></table>"


def page(r: dict) -> str:
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    cz_note = f"<p class=\"sub\">{html.escape(r['citizens_note'])}</p>" if r["citizens_note"] else ""
    sections = [
        ("Organic agent visits: not from the lab, and touched the game", table(
            ["address", "", "first seen", "last seen", "requests", "mcp calls", "cloaks", "gates", "names taken", "classes", "hosts"], r["organic"])),
        ("The door, per day", table(["day", "", "initialized", "cloaked", "checked", "gate", "names arrived"], r["funnel"])),
        ("Networks: every address in the window", table(
            ["address", "", "first seen", "last seen", "requests", "mcp calls", "cloaks", "gates", "names taken", "classes", "hosts"], r["networks"])),
        ("Citizens: who came, by what door, and what they did", cz_note + table(
            ["name", "", "first door", "first seen", "last seen", "arrivals", "doors", "messages", "written", "pass", "via", "rooms dug"], r["citizens"])),
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
td:nth-child(2) {{ color: var(--dust); }}
</style></head><body><main>
<h1>Moondial analytics</h1>
<p class="sub">Since {html.escape(r['since'])} ({r['days']} days) · rendered {now} · 2xx requests unless noted · addresses kept {html.escape(os.environ.get("ANALYTICS_RETENTION_DAYS", "90"))} days, operator-only · lab = {html.escape(LAB_NETS or "no lab nets set")}</p>
{body}
</main></body></html>"""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--days", type=int, default=30)
    ap.add_argument("--html", action="store_true", help="also write the admin page")
    args = ap.parse_args()
    if not os.path.exists(DB):
        print(f"no analytics store at {DB}; set ANALYTICS_DB (on the game server: /srv/moondial/shared/analytics/analytics.db)")
        return 1
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
