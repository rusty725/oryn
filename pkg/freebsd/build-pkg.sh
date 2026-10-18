#!/bin/sh
# Build oryn-<version>.pkg and oryn-<version>-amd64-freebsd.tar.gz for FreeBSD.
# Run from the repository root:  sh pkg/freebsd/build-pkg.sh
# Needs: cargo (pkg install rust, or rustup), pkg(8).
set -eu

PKG=oryn
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
ABI=$(pkg config abi)
OUT=${OUT:-target/freebsd}
STAGE=$OUT/stage
TARDIR=$OUT/$PKG-$VER-$ARCH-freebsd

cargo build --release --locked
rm -rf "$OUT"
mkdir -p "$OUT"

# Staged install tree under /usr/local, as ports do.
install -d "$STAGE/usr/local/bin" "$STAGE/usr/local/share/man/man1" "$STAGE/usr/local/share/doc/$PKG"
install -m 755 -s target/release/oryn "$STAGE/usr/local/bin/oryn"
gzip -9nc doc/oryn.1 > "$STAGE/usr/local/share/man/man1/oryn.1.gz"
for f in README.md LICENSE THIRD_PARTY_NOTICES.md LICENSES-dependencies.txt; do
  install -m 644 "$f" "$STAGE/usr/local/share/doc/$PKG/$f"
done

(cd "$STAGE" && find usr -type f | sed 's|^|/|' | sort) > "$OUT/plist"

# Shown by pkg after installing; also printed by `pkg info -D oryn`.
MESSAGE=$(cat pkg/freebsd/pkg-message)

cat > "$OUT/+MANIFEST" <<MANIFEST
name: $PKG
version: "$VER"
origin: sysutils/$PKG
comment: "Terminal hardware monitor with every temperature sensor"
maintainer: rusty725@users.noreply.github.com
www: https://github.com/rusty725/oryn
abi: "$ABI"
arch: "$(echo "$ABI" | tr '[:upper:]' '[:lower:]')"
prefix: /usr/local
licenselogic: single
licenses: [MIT]
categories: [sysutils]
desc: <<EOD
Oryn shows every temperature sensor in the machine in one table: CPU
(Tctl and each CCD), GPU core, hotspot and VRAM chips, each RAM stick,
NVMe and SATA drives, each named after its hardware and with min/max,
trend and a history graph. Around it sit panels for CPU, GPU, memory,
disks, network, processes and system specs.
EOD
messages: [ { message: <<EOM
$MESSAGE
EOM
} ]
MANIFEST

pkg create -M "$OUT/+MANIFEST" -p "$OUT/plist" -r "$STAGE" -o "$OUT"
mv "$OUT/$PKG-$VER.pkg" "$OUT/$PKG-$VER-freebsd-$ARCH.pkg"
echo "built $OUT/$PKG-$VER-freebsd-$ARCH.pkg"

# Plain tarball: binary, man page, docs and the same instructions.
mkdir -p "$TARDIR"
install -m 755 "$STAGE/usr/local/bin/oryn" "$TARDIR/oryn"
install -m 644 doc/oryn.1 "$TARDIR/oryn.1"
install -m 644 pkg/freebsd/pkg-message "$TARDIR/INSTALL-FreeBSD.txt"
for f in README.md LICENSE THIRD_PARTY_NOTICES.md LICENSES-dependencies.txt; do
  install -m 644 "$f" "$TARDIR/$f"
done
tar -czf "$TARDIR.tar.gz" -C "$OUT" "$(basename "$TARDIR")"
echo "built $TARDIR.tar.gz"

(cd "$OUT" && sha256 -r "$PKG-$VER-freebsd-$ARCH.pkg" "$(basename "$TARDIR").tar.gz" | sed 's/ /  /') > "$OUT/SHA256SUMS-freebsd"
cat "$OUT/SHA256SUMS-freebsd"
