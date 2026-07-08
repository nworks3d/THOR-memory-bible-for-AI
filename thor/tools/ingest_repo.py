#!/usr/bin/env python3
"""Ingest a git repo's TRACKED text files into a THOR import JSONL.

The repo is the source of truth for artifact content (code + docs); THOR
indexes it directly rather than deriving from any other tool's index. Only
`git ls-files` (tracked) files are read, so gitignored secrets (.env, keys)
are never ingested. Binary/asset files are skipped by extension. Each text file
is split into ~1800-char chunks on line boundaries; every chunk becomes one
THOR fact with a stable entity_id "<repo>:<path>#<n>".

Usage:
  python ingest_repo.py <repo_path> [<repo_path> ...] [--out <file.jsonl>] [--append]

Output defaults to %LOCALAPPDATA%/thor/seed/repo_ingest.jsonl (OUTSIDE any repo;
*.jsonl is gitignored as a second line of defence).
"""
import argparse
import json
import os
import subprocess
import sys

SKIP_EXT = {
    "png", "jpg", "jpeg", "gif", "ico", "svg", "webp", "bmp", "tif", "tiff",
    "woff", "woff2", "ttf", "eot", "otf", "pdf", "zip", "gz", "tgz", "7z", "rar",
    "mp3", "mp4", "mov", "avi", "webm", "wav", "db", "sqlite", "bin", "exe",
    "dll", "so", "dylib", "class", "jar", "wasm", "lock",
}
MAX_CHUNK_CHARS = 1800
MAX_FILE_CHARS = 200_000  # cap per-file ingestion (skip giant minified bundles cheaply)

DEFAULT_OUT = os.path.join(
    os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
    "thor", "seed", "repo_ingest.jsonl",
)


def tracked_files(repo):
    out = subprocess.run(
        ["git", "-C", repo, "ls-files"], capture_output=True, text=True, encoding="utf-8"
    )
    return [f for f in out.stdout.splitlines() if f.strip()]


def chunk_text(text):
    """Split into <= MAX_CHUNK_CHARS chunks on line boundaries; hard-split any
    single line longer than the cap (minified code)."""
    chunks, buf, size = [], [], 0
    for line in text.splitlines(keepends=True):
        while len(line) > MAX_CHUNK_CHARS:
            if buf:
                chunks.append("".join(buf)); buf, size = [], 0
            chunks.append(line[:MAX_CHUNK_CHARS]); line = line[MAX_CHUNK_CHARS:]
        if size + len(line) > MAX_CHUNK_CHARS and buf:
            chunks.append("".join(buf)); buf, size = [], 0
        buf.append(line); size += len(line)
    if buf:
        chunks.append("".join(buf))
    return [c for c in chunks if c.strip()]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("repos", nargs="+")
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--append", action="store_true")
    args = ap.parse_args()

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    mode = "a" if args.append else "w"
    written = files_done = skipped_bin = skipped_big = 0
    with open(args.out, mode, encoding="utf-8") as fout:
        for repo in args.repos:
            repo = repo.rstrip("/\\")
            name = os.path.basename(repo)
            if not os.path.isdir(os.path.join(repo, ".git")):
                print(f"  skip (not a git repo): {repo}", file=sys.stderr); continue
            for rel in tracked_files(repo):
                ext = rel.rsplit(".", 1)[-1].lower() if "." in rel else ""
                if ext in SKIP_EXT:
                    skipped_bin += 1; continue
                full = os.path.join(repo, rel)
                try:
                    with open(full, "r", encoding="utf-8", errors="strict") as f:
                        text = f.read()
                except (UnicodeDecodeError, OSError):
                    skipped_bin += 1; continue  # not utf-8 text
                if len(text) > MAX_FILE_CHARS:
                    text = text[:MAX_FILE_CHARS]; skipped_big += 1
                chunks = chunk_text(text)
                for i, ch in enumerate(chunks):
                    body = f"{ch.rstrip()}\n\n[repo file | {name}/{rel} | chunk {i + 1}/{len(chunks)}]"
                    rec = {"entity_id": f"{name}:{rel}#{i}", "body": body, "actor": "repo-ingest"}
                    fout.write(json.dumps(rec, ensure_ascii=False) + "\n")
                    written += 1
                files_done += 1
    print(f"ingested {written} chunks from {files_done} files -> {args.out} "
          f"(skipped {skipped_bin} binary/non-utf8, truncated {skipped_big} large)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
