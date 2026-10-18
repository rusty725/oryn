#!/bin/sh
# Build oryn_<version>-1_<arch>.deb for Ubuntu / Debian.
# Run from the repository root:  sh pkg/deb/build-deb.sh
# Needs: cargo, dpkg-dev (dpkg-shlibdeps), optionally lintian.
set -eu

PKG=oryn
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
REV=1
ARCH=$(dpkg --print-architecture)
MAINT="rusty725 <rusty725@users.noreply.github.com>"
OUT=${OUT:-target/deb}
ROOT=$OUT/${PKG}_${VER}-${REV}_${ARCH}

cargo build --release --locked
rm -rf "$ROOT"
install -Dm755 target/release/oryn "$ROOT/usr/bin/oryn"
install -Dm644 doc/oryn.1 "$ROOT/usr/share/man/man1/oryn.1"
gzip -9n "$ROOT/usr/share/man/man1/oryn.1"
DOC=$ROOT/usr/share/doc/$PKG
install -Dm644 README.md "$DOC/README.md"
install -Dm644 THIRD_PARTY_NOTICES.md "$DOC/THIRD_PARTY_NOTICES.md"
install -Dm644 LICENSES-dependencies.txt "$DOC/LICENSES-dependencies.txt"
gzip -9n "$DOC/README.md" "$DOC/LICENSES-dependencies.txt"

# Machine-readable copyright file (Debian policy 12.5).
{
  echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
  echo "Upstream-Name: oryn"
  echo "Upstream-Contact: rusty725"
  echo "Source: https://github.com/rusty725/oryn"
  echo "Comment: The binary also contains Rust crates under MIT, Apache-2.0, ISC,"
  echo " Zlib, BSL-1.0 and Unlicense licenses; their notices are in"
  echo " /usr/share/doc/oryn/LICENSES-dependencies.txt.gz."
  echo
  echo "Files: *"
  echo "Copyright: 2026 rusty725"
  echo "License: MIT"
  echo
  echo "Files: src/nvapi.rs"
  echo "Copyright: 2026 rusty725"
  echo "           2023 Ilya Zlobintsev"
  echo "           2026 Philip Langdale"
  echo "License: MIT"
  echo
  echo "License: MIT"
  sed -n '/^Permission is hereby granted/,$p' LICENSE | sed -e 's/^$/./' -e 's/^/ /'
} > "$DOC/copyright"

DATE=$(date -R)
printf '%s (%s-%s) unstable; urgency=medium\n\n  * Release %s.\n\n -- %s  %s\n' \
  "$PKG" "$VER" "$REV" "$VER" "$MAINT" "$DATE" | gzip -9n > "$DOC/changelog.Debian.gz"

# Library dependencies, worked out from the binary itself.
mkdir -p "$OUT/tmp/debian"
: > "$OUT/tmp/debian/control"
DEPS=$(cd "$OUT/tmp" && dpkg-shlibdeps -O "$OLDPWD/$ROOT/usr/bin/oryn" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')

SIZE=$(du -sk --exclude=DEBIAN "$ROOT" | cut -f1)
mkdir -p "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<CONTROL
Package: $PKG
Version: $VER-$REV
Architecture: $ARCH
Maintainer: $MAINT
Installed-Size: $SIZE
Depends: $DEPS
Suggests: smartmontools, pci.ids
Section: utils
Priority: optional
Homepage: https://github.com/rusty725/oryn
Description: terminal hardware monitor with every temperature sensor
 Oryn shows every temperature sensor in the machine in one table: CPU cores,
 GPU edge, hotspot and individual VRAM chips, each RAM stick, NVMe, SATA and
 USB drives, chipset and network cards, each named after its hardware and
 with min/max, trend and a history graph.
 .
 Around it sit btop-style panels for CPU (with live clock speed and per-core
 temperatures), GPU (NVIDIA and AMD), memory, disks, network, processes (with
 search, terminate, kill and nice) and system specs. 100 color schemes,
 compact mode and mouse support.
CONTROL

dpkg-deb --root-owner-group -Zxz --build "$ROOT" "$OUT/" >/dev/null
DEB=$OUT/${PKG}_${VER}-${REV}_${ARCH}.deb
echo "built $DEB"
if command -v lintian >/dev/null; then
  lintian --tag-display-limit 0 "$DEB" || true
fi
