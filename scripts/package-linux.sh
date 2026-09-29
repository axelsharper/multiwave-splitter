#!/usr/bin/env bash
# Build a Linux tarball: dist/MultiwaveSplitter-Linux-x86_64.tar.gz
set -euo pipefail
cd "$(dirname "$0")/.."
ARCH=$(uname -m)
NAME="MultiwaveSplitter-Linux-$ARCH"
cargo build --release
rm -rf "dist/$NAME" && mkdir -p "dist/$NAME"
cp target/release/multiwave-splitter "dist/$NAME/"
cp -R assets/linux "dist/$NAME/icons"
cp installer/multiwave-splitter.desktop installer/install-linux.sh LICENSE README.md "dist/$NAME/"
chmod +x "dist/$NAME/install-linux.sh" "dist/$NAME/multiwave-splitter"
tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
echo "Built dist/$NAME.tar.gz"
