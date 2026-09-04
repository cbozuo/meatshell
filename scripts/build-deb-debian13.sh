#!/usr/bin/env bash
#
# 在 Debian 13 (trixie) / KDE 机器上一键构建并打包 meatshell 的 .deb 验证包。
# 与 CI (.github/workflows/release.yml 的 "Build Debian package" 步骤) 逻辑一致：
#   1. 安装 GUI 构建依赖 (Slint/rfd/arboard 需要的系统库)
#   2. 确保 Rust 工具链存在 (没有就走 rustup 装 stable)
#   3. cargo build --release
#   4. 用 dpkg-deb 打出 .deb (Depends 由 dpkg-shlibdeps 自动推导)
#
# 用法：
#   sudo apt-get install -y git
#   git clone <你的 fork> meatshell && cd meatshell
#   bash scripts/build-deb-debian13.sh
# 产物：当前目录下的  meatshell-<版本>-linux-amd64.deb
#
# 说明：本脚本只做“验证包”，不 bump 版本号、不打 tag。

set -euo pipefail

echo "==> 检查系统 (期望 Debian 13 / trixie)"
if [ -r /etc/os-release ]; then
    . /etc/os-release
    echo "    发行版: ${PRETTY_NAME:-unknown}"
    if [[ "${VERSION_CODENAME:-}" != "trixie" && "${VERSION_ID:-}" != "13" ]]; then
        echo "    警告: 当前不是 Debian 13 (trixie)，依赖版本可能不匹配，继续……" >&2
    fi
fi

echo "==> 安装构建依赖 (需要 sudo)"
sudo -v
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
    build-essential pkg-config cmake curl \
    libfontconfig1-dev libfreetype6-dev \
    libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
    libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
    libgl1-mesa-dev libegl1-mesa-dev libgtk-3-dev \
    libudev-dev dpkg-dev

echo "==> 确保 Rust 工具链"
if ! command -v cargo >/dev/null 2>&1; then
    echo "    cargo 未找到，通过 rustup 安装 stable"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
    # shellcheck disable=SC1090
    . "$HOME/.cargo/env"
fi
cargo --version

echo "==> cargo build --release"
cargo build --release --locked

VERSION_NUM="$(sed -n '/^\[package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
echo "    版本: ${VERSION_NUM}"

ROOT="deb-root"
BIN="target/release/meatshell"
install -Dm755 "$BIN" "$ROOT/usr/bin/meatshell"
install -Dm644 assets/meatshell.desktop "$ROOT/usr/share/applications/meatshell.desktop"
install -Dm644 assets/icon@512.png "$ROOT/usr/share/icons/hicolor/512x512/apps/meatshell.png"
install -Dm644 THIRD_PARTY_NOTICES.md "$ROOT/usr/share/doc/meatshell/THIRD_PARTY_NOTICES.md" 2>/dev/null || true
install -d "$ROOT/DEBIAN"

# dpkg-shlibdeps 需要源包元数据 debian/control 才能正常推导依赖
install -d debian
cat > debian/control <<'EOF'
Source: meatshell
Section: net
Priority: optional
Maintainer: MeatShell contributors
Standards-Version: 4.6.2

Package: meatshell
Architecture: any
Depends: ${shlibs:Depends}
Description: Lightweight SSH, SFTP, and terminal client
EOF

echo "==> 推导运行时依赖 (dpkg-shlibdeps)"
DEPENDS="$(dpkg-shlibdeps -O -e"$ROOT/usr/bin/meatshell" | sed -n 's/^shlibs:Depends=//p')"
test -n "$DEPENDS"
echo "    Depends: ${DEPENDS}"

cat > "$ROOT/DEBIAN/control" <<EOF
Package: meatshell
Version: ${VERSION_NUM}
Section: net
Priority: optional
Architecture: amd64
Maintainer: MeatShell contributors
Depends: ${DEPENDS}
Homepage: https://github.com/yituorou/meatshell
Description: Lightweight SSH, SFTP, and terminal client
 MeatShell is a cross-platform terminal client written in Rust and Slint.
EOF

DEB="meatshell-${VERSION_NUM}-linux-amd64.deb"
dpkg-deb --root-owner-group --build "$ROOT" "$DEB"
echo
echo "==> 完成: $(pwd)/$DEB"
dpkg-deb --info "$DEB" | sed -n '1,12p'
