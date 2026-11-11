#!/bin/sh
# herdr-multiplayer installer: installs or updates the fork's latest build.
#
#   curl -fsSL https://raw.githubusercontent.com/sschoedel/herdr-multiplayer/multiplayer-cursors/install.sh | bash
#
# Environment overrides:
#   HERDR_INSTALL_DIR        install directory (default: ~/.local/bin)
#   HERDR_NO_MODIFY_PATH=1   never edit shell startup files
#   HERDR_FORK_RELEASE_URL   release download base (default: the fork-latest release)
set -eu

BIN="herdr"
RELEASE_URL="${HERDR_FORK_RELEASE_URL:-https://github.com/sschoedel/herdr-multiplayer/releases/download/fork-latest}"
INSTALL_DIR="${HERDR_INSTALL_DIR:-$HOME/.local/bin}"

main() {
    echo ""
    echo "  herdr-multiplayer installer"
    echo "  a fork of herdr (herdr.dev) for shared sessions"
    echo ""

    OS="$(uname -s)"
    ARCH="$(uname -m)"
    case "${OS}/${ARCH}" in
        Darwin/arm64|Darwin/aarch64) ASSET="herdr-macos-aarch64" ;;
        Linux/x86_64|Linux/amd64)    ASSET="herdr-linux-x86_64" ;;
        *) err "no herdr-multiplayer build for ${OS}/${ARCH} yet (builds exist for Apple Silicon Macs and x86_64 Linux)" ;;
    esac
    log "detected ${OS}/${ARCH}"

    need curl
    need awk

    if command -v sha256sum >/dev/null 2>&1; then
        SHA256_TOOL="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        SHA256_TOOL="shasum"
    elif command -v openssl >/dev/null 2>&1; then
        SHA256_TOOL="openssl"
    else
        err "SHA-256 verification requires sha256sum, shasum, or openssl"
    fi

    TMP="$(mktemp -d)"
    trap 'rm -rf "$TMP"' EXIT

    log "fetching checksums..."
    curl -fsSL --retry 3 --connect-timeout 10 --max-time 30 "${RELEASE_URL}/SHA256SUMS" -o "${TMP}/SHA256SUMS" \
        || err "can't reach ${RELEASE_URL}/SHA256SUMS (a new build may be publishing; try again in a minute)"
    SHA256="$(awk -v asset="$ASSET" '$2 == asset { print tolower($1); exit }' "${TMP}/SHA256SUMS")"
    if [ "${#SHA256}" -ne 64 ]; then
        err "the release has no checksum for ${ASSET}"
    fi

    log "downloading ${ASSET}..."
    curl -fsSL --retry 3 --connect-timeout 10 --max-time 300 "${RELEASE_URL}/${ASSET}" -o "${TMP}/${BIN}" \
        || err "download failed from ${RELEASE_URL}/${ASSET}"

    case "$SHA256_TOOL" in
        sha256sum) ACTUAL="$(sha256sum < "${TMP}/${BIN}" | awk '{ print $1 }')" ;;
        shasum)    ACTUAL="$(shasum -a 256 < "${TMP}/${BIN}" | awk '{ print $1 }')" ;;
        openssl)   ACTUAL="$(openssl dgst -sha256 < "${TMP}/${BIN}" | awk '{ print $NF }')" ;;
    esac
    if [ "$ACTUAL" != "$SHA256" ]; then
        err "downloaded binary checksum did not match the release; nothing was installed"
    fi
    chmod +x "${TMP}/${BIN}"
    "${TMP}/${BIN}" --version >/dev/null 2>&1 || err "the downloaded binary does not run on this machine"

    UPDATING=0
    if [ -e "${INSTALL_DIR}/${BIN}" ]; then
        UPDATING=1
    fi
    mkdir -p "$INSTALL_DIR"
    # Stage next to the target, then rename: a running herdr keeps its old file intact.
    cp "${TMP}/${BIN}" "${INSTALL_DIR}/.${BIN}.new"
    mv -f "${INSTALL_DIR}/.${BIN}.new" "${INSTALL_DIR}/${BIN}"
    log "installed ${INSTALL_DIR}/${BIN} ($(echo "$SHA256" | cut -c1-12))"

    ensure_path

    echo ""
    if [ "$UPDATING" = 1 ]; then
        log "updated. to pick it up:"
        echo "      herdr windows:  detach with ctrl+b q, then run 'herdr' again"
        echo "      shared server:  herdr server live-handoff --import-exe ${INSTALL_DIR}/${BIN}"
        echo "                      (switches a running server over without killing panes)"
    else
        log "ready. run 'herdr' to get started."
    fi
    log "later updates: run 'herdr update' (add --handoff on a machine hosting a shared session)"
    echo ""
}

# Make sure `herdr` resolves to this install in new shells, including when another herdr
# (for example Homebrew's) would otherwise be found first.
ensure_path() {
    FOUND="$(command -v "$BIN" 2>/dev/null || true)"
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) IN_PATH=1 ;;
        *) IN_PATH=0 ;;
    esac
    if [ "$IN_PATH" = 1 ] && [ "$FOUND" = "${INSTALL_DIR}/${BIN}" ]; then
        return
    fi

    case "$INSTALL_DIR" in
        "$HOME"/*) SHOWN_DIR="\$HOME${INSTALL_DIR#"$HOME"}" ;;
        *)         SHOWN_DIR="$INSTALL_DIR" ;;
    esac
    LINE="export PATH=\"${SHOWN_DIR}:\$PATH\""
    case "$(basename "${SHELL:-}")" in
        zsh)  RC="$HOME/.zshrc" ;;
        bash) if [ "$OS" = "Darwin" ]; then RC="$HOME/.bash_profile"; else RC="$HOME/.bashrc"; fi ;;
        *)    RC="" ;;
    esac

    echo ""
    if [ -n "${HERDR_NO_MODIFY_PATH:-}" ] || [ -z "$RC" ]; then
        if [ -n "$FOUND" ] && [ "$FOUND" != "${INSTALL_DIR}/${BIN}" ]; then
            warn "'herdr' currently runs ${FOUND}, not this install"
        else
            warn "${INSTALL_DIR} is not in your PATH"
        fi
        echo "      add this to your shell config so ${INSTALL_DIR} comes first:"
        echo ""
        echo "        ${LINE}"
        return
    fi

    if [ -f "$RC" ] && grep -Fq "# added by herdr-multiplayer installer" "$RC"; then
        if [ -n "$FOUND" ] && [ "$FOUND" != "${INSTALL_DIR}/${BIN}" ]; then
            warn "'herdr' in this shell runs ${FOUND}; open a new terminal to use ${INSTALL_DIR}/${BIN}"
        fi
        return
    fi
    {
        echo ""
        echo "# added by herdr-multiplayer installer"
        echo "$LINE"
    } >> "$RC"
    if [ -n "$FOUND" ] && [ "$FOUND" != "${INSTALL_DIR}/${BIN}" ]; then
        log "put ${INSTALL_DIR} ahead of ${FOUND} in ${RC}"
    else
        log "added ${INSTALL_DIR} to your PATH in ${RC}"
    fi
    echo "      open a new terminal (or run: exec \$SHELL) before running herdr"
}

log()  { printf '  \033[32m>\033[0m %s\n' "$1"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$1"; }
err()  { printf '  \033[31mx\033[0m %s\n' "$1" >&2; exit 1; }

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        err "requires '$1'; install it first"
    fi
}

main "$@"
