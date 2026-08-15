#!/bin/sh
# Install THOR on Linux, in one command:
#
#   curl -fsSL https://raw.githubusercontent.com/nworks3d/THOR-memory-bible-for-AI/main/install.sh | sh
#
# It downloads the latest release, checks the download against the checksum
# published beside it, unpacks it into ~/.thor2, and then runs THOR's own
# setup - which creates the memory and points Claude Code at it.
#
# Two files of yours are touched by that last step, and both are backed up to
# <name>.bak first: Claude Code's settings.json (the hooks) and .claude.json
# (the memory server). Set THOR_NO_SETUP=1 to unpack only and run the setup
# yourself later.
#
# Nothing here needs root, and nothing is installed outside your home folder.

set -eu

REPO="nworks3d/THOR-memory-bible-for-AI"
DEST="${THOR_HOME:-$HOME/.thor2}"

say() { printf '%s\n' "$*"; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }

# ---- what are we running on -------------------------------------------------
# Only one build is published per platform, so an unsupported machine is told
# so plainly instead of being handed a binary that cannot run on it.
os="$(uname -s 2>/dev/null || echo unknown)"
arch="$(uname -m 2>/dev/null || echo unknown)"

case "$os" in
  Linux) ;;
  Darwin)
    die "there is no macOS build yet. Build it from source instead: clone the repository, then 'cd thor2 && cargo build --release --features semantic'." ;;
  *)
    die "unsupported system '$os'. On Windows use install.ps1 instead." ;;
esac

case "$arch" in
  x86_64|amd64) ;;
  *)
    die "unsupported processor '$arch'. Only 64-bit Intel and AMD are published; build from source for anything else." ;;
esac

ASSET="thor2-linux-x86_64.tar.gz"
BASE="https://github.com/$REPO/releases/latest/download"

# ---- tools ------------------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
else
  die "neither curl nor wget is installed, so nothing can be downloaded."
fi

if command -v sha256sum >/dev/null 2>&1; then
  sum() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sum() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "neither sha256sum nor shasum is installed, so the download cannot be verified. Refusing to install something unchecked."
fi

# ---- download ---------------------------------------------------------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading THOR (about 80 MB)..."
fetch "$BASE/$ASSET" "$tmp/$ASSET" || die "download failed. Check your connection, or grab the file by hand from https://github.com/$REPO/releases/latest"
fetch "$BASE/$ASSET.sha256" "$tmp/$ASSET.sha256" || die "the checksum file could not be downloaded, so the archive cannot be verified."

want="$(cut -d' ' -f1 < "$tmp/$ASSET.sha256")"
got="$(sum "$tmp/$ASSET")"
[ -n "$want" ] || die "the checksum file was empty."
if [ "$want" != "$got" ]; then
  die "the download does not match its published checksum. Expected $want, got $got. Nothing was installed."
fi
say "Checksum matches."

# ---- unpack -----------------------------------------------------------------
# The archive is flat, so it goes straight into its own directory. An earlier
# install is overwritten in place; the memory itself lives elsewhere (see the
# setup's --db) and is never touched by this.
mkdir -p "$DEST"
tar xzf "$tmp/$ASSET" -C "$DEST"
chmod +x "$DEST"/* 2>/dev/null || true
say "Unpacked into $DEST"

# ---- set up -----------------------------------------------------------------
if [ "${THOR_NO_SETUP:-}" = "1" ]; then
  say ""
  say "Skipped the setup because THOR_NO_SETUP=1. Run it yourself with:"
  say "  $DEST/install"
  exit 0
fi

say "Setting up..."
"$DEST/install"

say ""
say "Done. Restart Claude Code so it picks up the memory server."
say "Check on it any time with: $DEST/doctor --db <your store>"
