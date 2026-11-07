#!/bin/sh
# Install the deepseek-web-mcp binary from GitHub releases (Linux/macOS).
#
#   curl -fsSL https://raw.githubusercontent.com/zhuzhda/deepseek-web-mcp/main/install.sh | sh
#
# Environment:
#   DEEPSEEK_INSTALL_DIR  target directory (default: $HOME/.local/bin)
#   VERSION               version to install without the leading "v" (default: latest)
set -eu

REPO="zhuzhda/deepseek-web-mcp"
BIN="deepseek-web-mcp"
INSTALL_DIR="${DEEPSEEK_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "required command not found: $1"; }

usage() {
    cat <<EOF
Install $BIN from GitHub releases.

Usage: install.sh [-h|--help]

Environment:
  DEEPSEEK_INSTALL_DIR  install directory (default: \$HOME/.local/bin)
  VERSION               release to install, without the leading "v" (default: latest)

Windows is not supported by this script: use
  cargo binstall $BIN
or download the .zip archive from
  https://github.com/$REPO/releases/latest
EOF
}

case "${1:-}" in
    -h | --help) usage; exit 0 ;;
    "") ;;
    *) err "unknown argument: $1 (see --help)" ;;
esac

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux) os_part="unknown-linux-musl" ;;
        Darwin) os_part="apple-darwin" ;;
        *) err "unsupported OS '$os': use 'cargo binstall $BIN' or download an archive from https://github.com/$REPO/releases/latest" ;;
    esac
    case "$arch" in
        x86_64 | amd64) arch_part="x86_64" ;;
        arm64 | aarch64) arch_part="aarch64" ;;
        *) err "unsupported architecture '$arch': use 'cargo binstall $BIN' or download an archive from https://github.com/$REPO/releases/latest" ;;
    esac
    printf '%s-%s\n' "$arch_part" "$os_part"
}

resolve_version() {
    if [ -n "${VERSION:-}" ]; then
        printf '%s\n' "${VERSION#v}"
        return
    fi
    latest_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO/releases/latest") ||
        err "cannot query the latest release (set VERSION to skip this step)"
    case "$latest_url" in
        */releases/tag/v*) printf '%s\n' "${latest_url##*/v}" ;;
        *) err "cannot determine the latest version from '$latest_url'" ;;
    esac
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

need curl
need tar
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 ||
    err "need sha256sum or shasum to verify the download"

target=$(detect_target)
version=$(resolve_version)
archive="$BIN-$target.tar.gz"
base="https://github.com/$REPO/releases/download/v$version"

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT INT TERM

say "Downloading $archive (v$version)..."
curl -fsSL "$base/$archive" -o "$workdir/$archive" ||
    err "download failed: $base/$archive"
# Release tooling names the checksum after the archive stem; fall back to
# "<archive>.sha256" for other layouts.
checksum="$BIN-$target.sha256"
if ! curl -fsSL "$base/$checksum" -o "$workdir/$checksum" 2>/dev/null; then
    checksum="$archive.sha256"
    curl -fsSL "$base/$checksum" -o "$workdir/$checksum" ||
        err "checksum download failed: $base/$checksum"
fi

expected=$(awk 'NR == 1 { print $1 }' "$workdir/$checksum")
[ -n "$expected" ] || err "empty checksum file for $archive"
actual=$(sha256_of "$workdir/$archive")
[ "$expected" = "$actual" ] ||
    err "checksum mismatch for $archive (expected $expected, got $actual)"

tar -xzf "$workdir/$archive" -C "$workdir"
[ -f "$workdir/$BIN" ] || err "archive $archive did not contain $BIN"

mkdir -p "$INSTALL_DIR"
if command -v install >/dev/null 2>&1; then
    install -m 0755 "$workdir/$BIN" "$INSTALL_DIR/$BIN"
else
    cp "$workdir/$BIN" "$INSTALL_DIR/$BIN"
    chmod 0755 "$INSTALL_DIR/$BIN"
fi

say "Installed $("$INSTALL_DIR/$BIN" --version) to $INSTALL_DIR/$BIN"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        say ""
        say "$INSTALL_DIR is not in your PATH. Add it with:"
        say "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac
