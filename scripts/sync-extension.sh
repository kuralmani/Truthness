#!/bin/sh
# The verdict renderer lives in web/ and is copied into the extension.
# Run this after editing web/render.js or web/verdict.css.
set -e
cd "$(dirname "$0")/.."
cp web/render.js web/verdict.css extension/
echo "extension/ is in sync with web/"
