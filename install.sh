#!/bin/bash
set -euo pipefail

# ── corplink installer ─────────────────────────────────────────────────────
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/cyhhao/corplink-rs/master/install.sh | bash
#
# Installs the latest release of corplink to /usr/local/bin.

REPO="${CORPLINK_REPO:-cyhhao/corplink-rs}"
INSTALL_DIR="${CORPLINK_INSTALL_DIR:-/usr/local/bin}"
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
TARGET_BIN="${INSTALL_DIR}/${BIN_NAME}"
STAGED_BIN="${INSTALL_DIR}/.${BIN_NAME}.new.$$"
BACKUP_BIN="${INSTALL_DIR}/.${BIN_NAME}.backup.$$"

if [ -w "$INSTALL_DIR" ]; then
  USE_SUDO=0
else
  USE_SUDO=1
fi

run_privileged() {
  if [ "$USE_SUDO" -eq 1 ]; then
    sudo "$@"
  else
    "$@"
  fi
}

cleanup() {
  rm -rf "$TMP_DIR"
  if [ -e "$STAGED_BIN" ] || [ -L "$STAGED_BIN" ] || [ -e "$BACKUP_BIN" ] || [ -L "$BACKUP_BIN" ]; then
    run_privileged rm -f "$STAGED_BIN" "$BACKUP_BIN" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

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

EXPECTED_VERSION="${TAG#v}"
VERSION_OUTPUT="$("$NEW_BIN" --version 2>/dev/null || true)"
if [ "${VERSION_OUTPUT##* }" != "$EXPECTED_VERSION" ]; then
  echo "error: downloaded binary version mismatch: expected ${EXPECTED_VERSION}, got ${VERSION_OUTPUT:-unknown}"
  exit 1
fi

# ── Install ───────────────────────────────────────────────────────────────

echo "staging update next to ${TARGET_BIN} ..."
run_privileged install -m 755 "$NEW_BIN" "$STAGED_BIN"

STAGED_VERSION="$("$STAGED_BIN" --version 2>/dev/null || true)"
if [ "${STAGED_VERSION##* }" != "$EXPECTED_VERSION" ]; then
  echo "error: staged binary version mismatch"
  exit 1
fi

WAS_RUNNING=0
DAEMON_PORT=4027
RUNTIME_FILE="${HOME}/.config/corplink/daemon-runtime.json"
if [ -f "$TARGET_BIN" ]; then
  if [ -f "$RUNTIME_FILE" ]; then
    SAVED_PORT="$(sed -n 's/.*"port":[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$RUNTIME_FILE")"
    if [ -n "$SAVED_PORT" ]; then
      DAEMON_PORT="$SAVED_PORT"
    fi
  fi
  STATUS_OUTPUT="$("$TARGET_BIN" status --port "$DAEMON_PORT" 2>/dev/null || true)"
  case "$STATUS_OUTPUT" in
    "daemon is running"*)
    WAS_RUNNING=1
    ;;
  esac
fi

if [ -f "$TARGET_BIN" ]; then
  run_privileged cp -p "$TARGET_BIN" "$BACKUP_BIN"
fi
sync

if [ "$WAS_RUNNING" -eq 1 ]; then
  echo "stopping running corplink daemon ..."
  if ! "$TARGET_BIN" stop; then
    echo "error: failed to stop running daemon; existing installation was not changed"
    exit 1
  fi
fi

echo "installing to ${TARGET_BIN} ..."
if ! run_privileged mv -f "$STAGED_BIN" "$TARGET_BIN"; then
  echo "error: atomic install failed; existing binary was not changed"
  if [ "$WAS_RUNNING" -eq 1 ]; then
    "$TARGET_BIN" start --port "$DAEMON_PORT" --no-open || true
  fi
  exit 1
fi

INSTALLED_VERSION="$("$TARGET_BIN" --version 2>/dev/null || true)"
if [ "${INSTALLED_VERSION##* }" != "$EXPECTED_VERSION" ]; then
  echo "error: installed binary validation failed; rolling back"
  if [ -f "$BACKUP_BIN" ]; then
    run_privileged mv -f "$BACKUP_BIN" "$TARGET_BIN"
  fi
  if [ "$WAS_RUNNING" -eq 1 ]; then
    "$TARGET_BIN" start --port "$DAEMON_PORT" --no-open || true
  fi
  exit 1
fi

if [ "$WAS_RUNNING" -eq 1 ]; then
  echo "restarting corplink daemon on port ${DAEMON_PORT} ..."
  if ! "$TARGET_BIN" start --port "$DAEMON_PORT" --no-open; then
    echo "error: new daemon failed to start; rolling back"
    if [ -f "$BACKUP_BIN" ]; then
      "$TARGET_BIN" stop || true
      run_privileged mv -f "$BACKUP_BIN" "$TARGET_BIN"
      "$TARGET_BIN" start --port "$DAEMON_PORT" --no-open || true
    fi
    exit 1
  fi
fi

run_privileged rm -f "$BACKUP_BIN"

echo ""
echo "corplink ${TAG} installed to ${TARGET_BIN}"
echo ""
echo "get started:"
echo "  corplink serve        # start web UI"
echo "  corplink --help       # see all commands"
