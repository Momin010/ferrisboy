#!/usr/bin/env bash
# Fetch the test ROMs and the opcode table used to build and verify ferrisboy.
# These are third-party and intentionally not committed (see .gitignore).
set -euo pipefail
cd "$(dirname "$0")/.."

mkdir -p resources test-roms

echo "==> opcode table (gbdev)"
curl -sL -o resources/Opcodes.json "https://gbdev.io/gb-opcodes/Opcodes.json"

echo "==> Blargg test ROMs"
if [ ! -d test-roms/blargg ]; then
  git clone --depth 1 https://github.com/retrio/gb-test-roms.git test-roms/blargg
fi

echo "==> dmg-acid2 (PPU conformance)"
curl -sL -o test-roms/dmg-acid2.gb \
  "https://github.com/mattcurrie/dmg-acid2/releases/download/v1.0/dmg-acid2.gb"

echo "Done. Test ROMs are in ./test-roms, opcode data in ./resources."
