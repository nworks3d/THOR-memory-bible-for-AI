#!/usr/bin/env python3
"""Export useful mimir memory content to a JSONL snapshot for THOR import.

READ-ONLY: opens the mimir SQLite DB with `mode=ro`, so it can never write to
the live store. THOR itself never opens the mimir DB - this exporter is the
only thing that reads it, and it only reads. The JSONL it produces contains the
user's private memories, so it must NOT be committed (write it outside the repo,
e.g. a scratch/data dir).

Usage:
  python export_mimir.py [--mimir-db <path>] [--out <snapshot.jsonl>]

Both default to the standard Windows locations; --out defaults to
%LOCALAPPDATA%\\thor\\seed\\mimir_export.jsonl (OUTSIDE any repo).
"""
import argparse
import json
import os
import sqlite3
import sys

DEFAULT_MIMIR_DB = os.path.join(
    os.environ.get("APPDATA", os.path.expanduser("~")), "mimir", "data", "mimir.db"
)

# Default the snapshot OUTSIDE any repo (alongside the THOR store), so following
# the documented usage can never drop private memories into a git work tree. The
# repo also gitignores *.jsonl as a second line of defence.
DEFAULT_OUT = os.path.join(
    os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
    "thor",
    "seed",
    "mimir_export.jsonl",
)


def compose_memory(row):
    subkind = row["subkind"] or "note"
    tags = (row["tags_text"] or "").strip()
    proj = row["proj"] or "global"
    body = (row["body"] or row["title"] or "").strip()
    footer = f"[memory/{subkind} | tags: {tags} | project: {proj} | mimir:{row['uid']}]"
    return f"{body}\n\n{footer}"


def compose_chunk(row):
    title = (row["title"] or "").strip()
    body = (row["body"] or "").strip()
    proj = row["proj"] or "global"
    path = row["path"] or ""
    head = f"{title}\n{body}".strip()
    footer = f"[doc chunk | {path} | project: {proj} | mimir:{row['uid']}]"
    return f"{head}\n\n{footer}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mimir-db", default=DEFAULT_MIMIR_DB)
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument(
        "--kinds",
        default="memory,chunk",
        help="comma-separated node kinds to export (default: memory,chunk)",
    )
    args = ap.parse_args()

    if not os.path.exists(args.mimir_db):
        print(f"mimir DB not found: {args.mimir_db}", file=sys.stderr)
        return 2

    kinds = [k.strip() for k in args.kinds.split(",") if k.strip()]
    placeholders = ",".join("?" for _ in kinds)

    uri = "file:" + args.mimir_db.replace("\\", "/") + "?mode=ro"
    con = sqlite3.connect(uri, uri=True)
    con.row_factory = sqlite3.Row
    cur = con.cursor()

    # LEFT JOIN the project node so we can label each fact with its project name.
    # Live only: not deleted, not superseded (mimir keeps history; we want heads).
    sql = f"""
        SELECT n.uid, n.kind, n.subkind, n.title, n.body, n.tags_text, n.path,
               p.title AS proj
        FROM node n
        LEFT JOIN node p ON p.id = n.project_id AND p.kind = 'project'
        WHERE n.kind IN ({placeholders})
          AND n.deleted_at IS NULL
          AND n.superseded_by IS NULL
        ORDER BY n.created_at
    """
    rows = cur.execute(sql, kinds).fetchall()

    out_dir = os.path.dirname(os.path.abspath(args.out))
    if out_dir:
        os.makedirs(out_dir, exist_ok=True)

    written = 0
    skipped = 0
    with open(args.out, "w", encoding="utf-8") as f:
        for row in rows:
            uid = row["uid"]
            if not uid:
                skipped += 1
                continue
            if row["kind"] == "memory":
                body = compose_memory(row)
            elif row["kind"] == "chunk":
                body = compose_chunk(row)
            else:
                skipped += 1
                continue
            if not body.strip():
                skipped += 1
                continue
            rec = {"entity_id": uid, "body": body, "actor": "mimir-import"}
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            written += 1

    con.close()
    by_kind = {}
    for row in rows:
        by_kind[row["kind"]] = by_kind.get(row["kind"], 0) + 1
    print(f"exported {written} records to {args.out} (skipped {skipped}); by kind: {by_kind}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
