#!/bin/sh
# Truthness installer: builds the server and puts `truthness` on your PATH.
#
#   curl -fsSL https://raw.githubusercontent.com/kuralmani/Truthness/main/install.sh | sh
#
# or, from a clone of the repository:  ./install.sh
set -eu

REPO="https://github.com/kuralmani/Truthness.git"
HOME_DIR="${TRUTHNESS_HOME:-$HOME/.truthness}"

if [ -f Cargo.toml ] && grep -q '^name = "truthness"' Cargo.toml; then
  SRC="$(pwd)"
else
  command -v git >/dev/null 2>&1 || { echo "git is required: https://git-scm.com/downloads" >&2; exit 1; }
  SRC="$HOME_DIR/src"
  if [ -d "$SRC/.git" ]; then
    echo "Updating $SRC"
    git -C "$SRC" pull --ff-only
  else
    echo "Downloading Truthness to $SRC"
    mkdir -p "$HOME_DIR"
    git clone --depth 1 "$REPO" "$SRC"
  fi
fi

if ! command -v cargo >/dev/null 2>&1; then
  if [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
  else
    echo "Rust is required to build Truthness. Install it from https://rustup.rs and run this again." >&2
    exit 1
  fi
fi

echo "Building (the first build takes a few minutes)"
cargo install --path "$SRC" --locked

cat <<DONE

Truthness is installed.

  1. Start it:            truthness
  2. Open the website:    http://localhost:8787
  3. Add the extension:   open chrome://extensions (or edge://extensions),
                          turn on Developer mode, click "Load unpacked" and choose
                          $SRC/extension

Models: with the Claude Code CLI installed and logged in, nothing else is needed.
For other models, copy $SRC/truthness.example.toml
to truthness.toml in the folder you start truthness from, and edit it.
DONE

case ":$PATH:" in
  *":$HOME/.cargo/bin:"*) ;;
  *) echo; echo "Note: add $HOME/.cargo/bin to your PATH to run 'truthness' from anywhere." ;;
esac
