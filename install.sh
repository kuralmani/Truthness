#!/bin/sh
# Truthness installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/kuralmani/Truthness/main/install.sh | sh
#
# Downloads the latest prebuilt server and the browser extension into
# ~/.truthness, puts `truthness` on your PATH, and starts it.
#
#   TRUTHNESS_HOME=dir       install somewhere other than ~/.truthness
#   TRUTHNESS_NO_START=1     install only, do not start the server
#   TRUTHNESS_FROM_SOURCE=1  build with cargo instead of downloading a binary
set -eu

REPO="kuralmani/Truthness"
DIR="${TRUTHNESS_HOME:-$HOME/.truthness}"
say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

have curl || die "curl is required"
mkdir -p "$DIR/bin"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  *) TARGET="" ;;
esac

# The latest release tag, read from where /releases/latest redirects to.
TAG="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" | sed 's|.*/tag/||')"
case "$TAG" in v*) ;; *) die "could not find the latest release of $REPO" ;; esac
BASE="https://github.com/$REPO/releases/download/$TAG"

if [ -n "$TARGET" ] && [ -z "${TRUTHNESS_FROM_SOURCE:-}" ]; then
  say "Downloading Truthness $TAG for $TARGET"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  curl -fsSL "$BASE/truthness-$TAG-$TARGET.tar.gz" | tar xz -C "$TMP"
  cp "$TMP/truthness-$TAG-$TARGET/truthness" "$DIR/bin/truthness"
  cp "$TMP/truthness-$TAG-$TARGET/truthness.example.toml" "$DIR/truthness.example.toml"
  chmod +x "$DIR/bin/truthness"
else
  [ -n "$TARGET" ] || say "No prebuilt binary for $(uname -s) $(uname -m); building from source."
  have cargo || die "Rust is required to build from source: https://rustup.rs"
  have git || die "git is required to build from source"
  if [ -d "$DIR/src/.git" ]; then git -C "$DIR/src" pull --ff-only; else git clone --depth 1 "https://github.com/$REPO.git" "$DIR/src"; fi
  cargo install --path "$DIR/src" --locked --root "$DIR"
  cp "$DIR/src/truthness.example.toml" "$DIR/truthness.example.toml"
fi

if have unzip; then
  say "Downloading the browser extension"
  curl -fsSL -o "$DIR/extension.zip" "$BASE/truthness-extension.zip"
  rm -rf "$DIR/extension"
  unzip -qo "$DIR/extension.zip" -d "$DIR/extension"
  rm -f "$DIR/extension.zip"
else
  say "unzip not found: skipping the browser extension (get it from https://github.com/$REPO/releases/latest)"
fi

# Put `truthness` on the PATH if a usual, writable place exists.
LINKED=""
for d in "$HOME/.local/bin" /usr/local/bin "$HOME/bin"; do
  case ":$PATH:" in
    *":$d:"*) if [ -d "$d" ] && [ -w "$d" ]; then ln -sf "$DIR/bin/truthness" "$d/truthness"; LINKED="$d"; break; fi ;;
  esac
done
RUN="truthness"
[ -n "$LINKED" ] || RUN="$DIR/bin/truthness"

say ""
say "Truthness $TAG is installed in $DIR"
say ""
say "AI tools found on this machine (Truthness uses these automatically):"
FOUND=""
have claude && { say "  - Claude Code CLI"; FOUND=1; }
have codex && { say "  - Codex CLI"; FOUND=1; }
have gemini && { say "  - Gemini CLI"; FOUND=1; }
[ -n "${ANTHROPIC_API_KEY:-}" ] && { say "  - Anthropic API key"; FOUND=1; }
[ -n "${GEMINI_API_KEY:-}" ] && { say "  - Gemini API key"; FOUND=1; }
if [ -z "$FOUND" ]; then
  say "  none. Install the Claude Code CLI (https://claude.com/claude-code), or copy"
  say "  $DIR/truthness.example.toml to $DIR/truthness.toml and add a model."
fi
say ""
say "Browser extension (Chrome or Edge), once:"
say "  1. Open chrome://extensions or edge://extensions"
say "  2. Turn on Developer mode"
say "  3. Click \"Load unpacked\" and choose $DIR/extension"
say ""
say "Start Truthness any time with:  $RUN"
say ""

if [ -z "${TRUTHNESS_NO_START:-}" ]; then
  say "Starting Truthness at http://localhost:8787 (Ctrl+C to stop)"
  (
    sleep 2
    if have open; then open "http://localhost:8787"; elif have xdg-open; then xdg-open "http://localhost:8787"; fi
  ) >/dev/null 2>&1 &
  exec "$DIR/bin/truthness"
fi
