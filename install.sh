#!/bin/bash
set -euo pipefail

# ── corplink installer ─────────────────────────────────────────────────────
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/cyhhao/corplink-rs/master/install.sh | bash
#
# Installs the latest release of corplink to /usr/local/bin.

REPO="cyhhao/corplink-rs"
INSTALL_DIR="/usr/local/bin"
BIN_NAME="corplink"

# ── Detect platform ────────────────────────────────────────────────────────

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin) PLATFORM="macos" ;;
  Linux)  PLATFORM="linux" ;;
  *)
    echo "error: unsupported OS: $OS"
    exit 1
    ;;
esac

case "$ARCH" in
  arm64|aarch64) ARCH="arm64" ;;
  x86_64)        ARCH="x86_64" ;;
  *)
    echo "error: unsupported architecture: $ARCH"
    exit 1
    ;;
esac

echo "detected platform: ${PLATFORM}-${ARCH}"

# ── Fetch latest release ──────────────────────────────────────────────────

echo "fetching latest release from github.com/${REPO} ..."

API_URL="https://api.github.com/repos/${REPO}/releases/latest"
RELEASE_JSON="$(curl -fsSL "$API_URL" 2>/dev/null)" || {
  echo "error: failed to fetch release info (no releases yet?)"
  exit 1
}

TAG="$(echo "$RELEASE_JSON" | grep '"tag_name"' | head -1 | sed 's/.*: *"//;s/".*//')"
if [ -z "$TAG" ]; then
  echo "error: could not determine latest release tag"
  exit 1
fi

echo "latest release: $TAG"

# ── Find matching asset ───────────────────────────────────────────────────

ASSET_PATTERN="${PLATFORM}-${ARCH}"
DOWNLOAD_URL="$(echo "$RELEASE_JSON" \
  | grep '"browser_download_url"' \
  | grep "$ASSET_PATTERN" \
  | head -1 \
  | sed 's/.*: *"//;s/".*//')"

if [ -z "$DOWNLOAD_URL" ]; then
  echo "error: no asset found for ${ASSET_PATTERN} in release ${TAG}"
  echo "available assets:"
  echo "$RELEASE_JSON" | grep '"browser_download_url"' | sed 's/.*: *"//;s/".*//'
  exit 1
fi

ASSET_NAME="$(basename "$DOWNLOAD_URL")"
echo "downloading $ASSET_NAME ..."

# ── Download and extract ──────────────────────────────────────────────────

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fSL "$DOWNLOAD_URL" -o "${TMP_DIR}/${ASSET_NAME}"

echo "extracting ..."
if echo "$ASSET_NAME" | grep -q '\.tar\.gz$'; then
  tar -xzf "${TMP_DIR}/${ASSET_NAME}" -C "$TMP_DIR"
elif echo "$ASSET_NAME" | grep -q '\.zip$'; then
  unzip -oq "${TMP_DIR}/${ASSET_NAME}" -d "$TMP_DIR"
else
  echo "error: unknown archive format: $ASSET_NAME"
  exit 1
fi

# Find the binary (could be `corplink` or legacy `corplink-rs`)
NEW_BIN=""
for name in corplink corplink-rs; do
  if [ -f "${TMP_DIR}/${name}" ]; then
    NEW_BIN="${TMP_DIR}/${name}"
    break
  fi
done

if [ -z "$NEW_BIN" ]; then
  echo "error: binary not found in archive"
  exit 1
fi

chmod +x "$NEW_BIN"

# ── Install ───────────────────────────────────────────────────────────────

if [ -w "$INSTALL_DIR" ]; then
  mv "$NEW_BIN" "${INSTALL_DIR}/${BIN_NAME}"
else
  echo "installing to ${INSTALL_DIR}/${BIN_NAME} (requires sudo) ..."
  sudo mv "$NEW_BIN" "${INSTALL_DIR}/${BIN_NAME}"
fi

echo ""
echo "corplink ${TAG} installed to ${INSTALL_DIR}/${BIN_NAME}"
echo ""
echo "get started:"
echo "  corplink serve        # start web UI"
echo "  corplink --help       # see all commands"
