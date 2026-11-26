#!/bin/sh
# Builds truthness-extension.zip: the folder to load as an unpacked extension.
set -e
cd "$(dirname "$0")/.."
./scripts/sync-extension.sh
rm -f truthness-extension.zip
(cd extension && zip -qr ../truthness-extension.zip . -x '.*')
echo "Wrote truthness-extension.zip"
