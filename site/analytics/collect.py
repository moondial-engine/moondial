#!/usr/bin/env python3
"""Collect nginx JSON access-log lines into the analytics store.

Runs on a timer (every 5 minutes) as the deploy user. Reads new lines from
/var/log/nginx/analytics.jsonl (and the rotated .1 file, so nothing is lost across a
rotation), classifies each request, and appends it to a SQLite store with a daily-salted
visitor hash instead of the IP. Idempotent: it remembers the inode and byte offset it
reached, so re-running never double counts.

Store: /srv/agentscanplay/shared/analytics/analytics.db
Tables:
  requests  one row per request: ts, host, path, status, bytes, rt, accept_kind,
            ua_class, ua_name, ua, referer_host, visitor (daily hash), method
  salts     one random salt per UTC day, used for the visitor hash; older salts are
            deleted, which makes the hashes unlinkable to IPs after the day ends
  cursor    the log file position reached (inode, offset)

The engine's admin tool reads this store (or its Postgres successor). render.py writes
the interim HTML report from it.

Standard library only. Python 3.10+.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import secrets
import sqlite3
import sys
from datetime import datetime, timezone, timedelta
from urllib.parse import urlsplit

LOG = os.environ.get("ANALYTICS_LOG", "/var/log/nginx/analytics.jsonl")
DB = os.environ.get("ANALYTICS_DB", "/srv/agentscanplay/shared/analytics/analytics.db")
RAW_RETENTION_DAYS = 2  # salts older than this are deleted; raw logs rotate separately

# User-agent classification. Order matters: AI crawlers before generic bots, tools before browsers.
AI_CRAWLERS = [
    ("ClaudeBot", "ClaudeBot"), ("Claude-User", "Claude-User"), ("Claude-SearchBot", "Claude-SearchBot"),
    ("anthropic-ai", "anthropic-ai"), ("GPTBot", "GPTBot"), ("ChatGPT-User", "ChatGPT-User"),
    ("OAI-SearchBot", "OAI-SearchBot"), ("PerplexityBot", "PerplexityBot"), ("Perplexity-User", "Perplexity-User"),
    ("Google-Extended", "Google-Extended"), ("GoogleOther", "GoogleOther"), ("Applebot-Extended", "Applebot-Extended"),
    ("CCBot", "CCBot"), ("Bytespider", "Bytespider"), ("Amazonbot", "Amazonbot"), ("cohere-ai", "cohere-ai"),
    ("meta-externalagent", "meta-externalagent"), ("Diffbot", "Diffbot"), ("YouBot", "YouBot"), ("DuckAssistBot", "DuckAssistBot"),
    ("MistralAI-User", "MistralAI-User"), ("Omgilibot", "Omgilibot"), ("Timpibot", "Timpibot"),
]
CRAWLERS = ["Googlebot", "bingbot", "Applebot", "DuckDuckBot", "YandexBot", "Baiduspider", "facebookexternalhit",
            "Twitterbot", "LinkedInBot", "Slackbot", "Discordbot", "TelegramBot", "WhatsApp", "ia_archiver", "archive.org_bot",
            "SemrushBot", "AhrefsBot", "MJ12bot", "DotBot", "PetalBot", "Sogou", "UptimeRobot", "Pingdom", "StatusCake"]
TOOLS = [("curl/", "curl"), ("Wget/", "wget"), ("python-requests", "python-requests"), ("python-httpx", "httpx"),
         ("aiohttp", "aiohttp"), ("Python-urllib", "urllib"), ("Go-http-client", "go-http"), ("node-fetch", "node-fetch"),
         ("undici", "undici"), ("axios", "axios"), ("okhttp", "okhttp"), ("Java/", "java"), ("libwww-perl", "perl"),
         ("Ruby", "ruby"), ("PostmanRuntime", "postman"), ("HTTPie", "httpie"), ("Dart/", "dart"), ("reqwest", "reqwest"),
         ("hyper/", "hyper"), ("Deno", "deno"), ("Bun/", "bun"), ("PowerShell", "powershell")]
BROWSERS = [("Edg/", "Edge"), ("OPR/", "Opera"), ("Firefox/", "Firefox"), ("Chrome/", "Chrome"), ("Safari/", "Safari")]


def classify(ua: str) -> tuple[str, str]:
    """Return (class, name). Classes: ai-crawler, crawler, tool, browser, agent, none, unknown."""
    if not ua or ua == "-":
        return "none", "none"
    for needle, name in AI_CRAWLERS:
        if needle.lower() in ua.lower():
            return "ai-crawler", name
    low = ua.lower()
    for name in CRAWLERS:
        if name.lower() in low:
            return "crawler", name
    if "bot" in low or "spider" in low or "crawl" in low:
        m = re.search(r"([A-Za-z0-9_-]+(?:bot|spider|crawler))", ua, re.IGNORECASE)
        return "crawler", (m.group(1) if m else "bot")
    for needle, name in TOOLS:
        if needle.lower() in low:
            return "tool", name
    if "mozilla/" in low:
        for needle, name in BROWSERS:
            if needle in ua:
                return "browser", name
        return "browser", "other"
    # Anything else with a name is most likely an agent that identifies itself.
    name = re.split(r"[/ (]", ua.strip(), maxsplit=1)[0][:40] or "unknown"
    return "agent", name


def accept_kind(accept: str) -> str:
    a = (accept or "").lower()
    if "text/html" in a:
        return "html"
    if a in ("", "-", "*/*"):
        return "any"
    if "text/plain" in a or "text/markdown" in a:
        return "text"
    if "application/json" in a:
        return "json"
    return "other"


def referer_host(ref: str) -> str:
    if not ref or ref == "-":
        return ""
    try:
        return urlsplit(ref).netloc.lower()
    except ValueError:
        return ""


SCHEMA = """
CREATE TABLE IF NOT EXISTS requests (
  id INTEGER PRIMARY KEY,
  ts TEXT NOT NULL, day TEXT NOT NULL, host TEXT NOT NULL, method TEXT NOT NULL, path TEXT NOT NULL,
  status INTEGER NOT NULL, bytes INTEGER NOT NULL, rt REAL NOT NULL,
  accept_kind TEXT NOT NULL, ua_class TEXT NOT NULL, ua_name TEXT NOT NULL, ua TEXT NOT NULL,
  referer_host TEXT NOT NULL, visitor TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS requests_day ON requests(day);
CREATE INDEX IF NOT EXISTS requests_host_path ON requests(host, path);
CREATE TABLE IF NOT EXISTS salts (day TEXT PRIMARY KEY, salt TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS cursor (file TEXT PRIMARY KEY, inode INTEGER NOT NULL, offset INTEGER NOT NULL);
"""


def salt_for(db: sqlite3.Connection, day: str) -> str:
    row = db.execute("SELECT salt FROM salts WHERE day = ?", (day,)).fetchone()
    if row:
        return row[0]
    salt = secrets.token_hex(16)
    db.execute("INSERT INTO salts(day, salt) VALUES (?, ?)", (day, salt))
    return salt


def visitor_hash(salt: str, ip: str, ua: str) -> str:
    return hashlib.sha256(f"{salt}|{ip}|{ua}".encode()).hexdigest()[:16]


def ingest_line(db: sqlite3.Connection, line: str) -> bool:
    try:
        r = json.loads(line)
    except json.JSONDecodeError:
        return False
    ts = r.get("ts", "")
    day = ts[:10]
    if len(day) != 10:
        return False
    ua = r.get("ua", "") or ""
    cls, name = classify(ua)
    db.execute(
        "INSERT INTO requests(ts, day, host, method, path, status, bytes, rt, accept_kind, ua_class, ua_name, ua, referer_host, visitor)"
        " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (ts, day, r.get("host", ""), r.get("method", ""), r.get("path", ""), int(r.get("status", 0)), int(r.get("bytes", 0)),
         float(r.get("rt", 0.0)), accept_kind(r.get("accept", "")), cls, name, ua[:300], referer_host(r.get("referer", "")),
         visitor_hash(salt_for(db, day), r.get("ip", ""), ua)),
    )
    return True


def read_new(db: sqlite3.Connection, path: str) -> int:
    """Read lines past the stored cursor. Handles rotation: if the inode changed, finish the
    rotated file (path + '.1') from the old offset, then start the new file from zero."""
    if not os.path.exists(path):
        return 0
    st = os.stat(path)
    row = db.execute("SELECT inode, offset FROM cursor WHERE file = ?", (path,)).fetchone()
    count = 0
    if row and row[0] != st.st_ino:
        rotated = path + ".1"
        if os.path.exists(rotated) and os.stat(rotated).st_ino == row[0]:
            n, _ = read_from(db, rotated, row[1])
            count += n
        row = None
    offset = row[1] if row else 0
    if offset > st.st_size:
        offset = 0  # truncated in place
    n, reached = read_from(db, path, offset)
    count += n
    db.execute("INSERT OR REPLACE INTO cursor(file, inode, offset) VALUES (?, ?, ?)", (path, st.st_ino, reached))
    return count


def read_from(db: sqlite3.Connection, path: str, offset: int) -> tuple[int, int]:
    """Ingest complete lines from `offset`. Returns (lines ingested, offset after the last
    complete line), so a partial line being written is re-read next time."""
    count = 0
    reached = offset
    with open(path, "rb") as fh:
        fh.seek(offset)
        for raw in fh:
            if not raw.endswith(b"\n"):
                break
            reached += len(raw)
            if ingest_line(db, raw.decode("utf-8", "replace")):
                count += 1
    return count, reached


def prune_salts(db: sqlite3.Connection) -> None:
    cutoff = (datetime.now(timezone.utc) - timedelta(days=RAW_RETENTION_DAYS)).strftime("%Y-%m-%d")
    db.execute("DELETE FROM salts WHERE day < ?", (cutoff,))


def main() -> int:
    os.makedirs(os.path.dirname(DB), exist_ok=True)
    db = sqlite3.connect(DB)
    db.executescript(SCHEMA)
    with db:
        n = read_new(db, LOG)
        prune_salts(db)
    total = db.execute("SELECT COUNT(*) FROM requests").fetchone()[0]
    print(f"ingested {n} new requests; {total} total")
    return 0


if __name__ == "__main__":
    sys.exit(main())
