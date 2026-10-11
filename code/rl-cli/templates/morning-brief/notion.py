#!/usr/bin/env python3
"""Notion pages edited in the brief's days (yesterday; on a Monday, Friday through Sunday), as the
account ntn is signed in to: who, page, url.

Notion's API does not name users for a personal token, so names come from the people fields
(assignee, attendees, …) of the pages it returns; an editor named nowhere stays an id. The
signed-in person is "(me)".
"""
import datetime as dt, json, subprocess, sys

def api(path, body):
    out = subprocess.run(["ntn", "api", path, "-d", json.dumps(body)], capture_output=True, text=True, stdin=subprocess.DEVNULL)
    if out.returncode:
        sys.exit(out.stderr.strip() or "ntn failed")
    return json.loads(out.stdout)

me = subprocess.run(["ntn", "whoami"], capture_output=True, text=True, check=True, stdin=subprocess.DEVNULL).stdout.split("\t")
me_id = me[6] if len(me) > 6 else ""

today = dt.datetime.now().astimezone().replace(hour=0, minute=0, second=0, microsecond=0)
start, end = today - dt.timedelta(days=3 if today.isoweekday() == 1 else 1), today

def title(p):
    for v in (p.get("properties") or {}).values():
        if v.get("type") == "title":
            return "".join(t["plain_text"] for t in v["title"])
    return "".join(t["plain_text"] for t in p.get("title", []))

pages, names, cursor = [], {}, None
while True:
    body = {"sort": {"timestamp": "last_edited_time", "direction": "descending"},
            "filter": {"property": "object", "value": "page"}, "page_size": 100}
    if cursor:
        body["start_cursor"] = cursor
    r = api("/v1/search", body)
    done = False
    for p in r["results"]:
        for v in (p.get("properties") or {}).values():
            for u in v.get("people", []) if v.get("type") == "people" else []:
                if u.get("name"):
                    names[u["id"]] = u["name"]
        at = dt.datetime.fromisoformat(p["last_edited_time"].replace("Z", "+00:00"))
        if at < start:
            done = True
            break
        if at < end:
            pages.append(p)
    if done or not r.get("has_more"):
        break
    cursor = r["next_cursor"]

for p in pages:
    who = p["last_edited_by"]["id"]
    who = "(me)" if who == me_id else names.get(who, "someone " + who[:8])
    print(f"{who}\t{title(p)}\t{p['url']}")
