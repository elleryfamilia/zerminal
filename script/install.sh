#!/usr/bin/env bash
set -euo pipefail

# Zerminal Linux installer.
#   curl -fsSL https://github.com/elleryfamilia/zerminal/releases/latest/download/install.sh | sh
#
# Installs Zerminal into ~/.local/zerminal.app and symlinks the launcher into
# ~/.local/bin. After this, Zerminal's built-in updater downloads and applies
# new releases in place — no package manager involvement.
#
# This is a single path for every Linux distro (Fedora, Ubuntu, Arch, etc.).
# Mac users should install via Homebrew.
#
# Overrides:
#   ZERMINAL_VERSION=v0.1.17   pin to a specific release (default: latest)
#   ZERMINAL_NONINTERACTIVE=1  skip the confirmation prompt

REPO="elleryfamilia/zerminal"
VERSION="${ZERMINAL_VERSION:-latest}"
if [ "$VERSION" = "latest" ]; then
    BASE="https://github.com/${REPO}/releases/latest/download"
else
    BASE="https://github.com/${REPO}/releases/download/${VERSION}"
fi
TARBALL_URL="${BASE}/zerminal-linux-x86_64.tar.gz"

INSTALL_ROOT="${HOME}/.local"
APP_DIR="${INSTALL_ROOT}/zerminal.app"
BIN_LINK="${INSTALL_ROOT}/bin/zerminal"
DESKTOP_LINK="${INSTALL_ROOT}/share/applications/dev.zerminal.Zerminal.desktop"
ICON_512_LINK="${INSTALL_ROOT}/share/icons/hicolor/512x512/apps/zerminal.png"
ICON_1024_LINK="${INSTALL_ROOT}/share/icons/hicolor/1024x1024/apps/zerminal.png"

TMP=""

cleanup() {
    [ -n "$TMP" ] && rm -rf "$TMP"
}
trap cleanup EXIT

main() {
    case "$(uname -s)" in
        Darwin)
            cat >&2 <<'EOF'
This script no longer installs Zerminal on macOS.

Use Homebrew instead:
    brew install --cask elleryfamilia/zerminal/zerminal

Or download the DMG manually from:
    https://github.com/elleryfamilia/zerminal/releases/latest
EOF
            exit 1
            ;;
        Linux) install_linux ;;
        *) die "Unsupported OS: $(uname -s)" ;;
    esac
}

install_linux() {
    [ "$(uname -m)" = "x86_64" ] || die \
        "Linux on $(uname -m) is not shipped. Build from source: cargo run -p zerminal"

    echo "Zerminal installer"
    echo "  repo:    ${REPO}"
    echo "  version: ${VERSION}"
    echo "  install: ${APP_DIR}"
    echo "  symlink: ${BIN_LINK}"
    echo
    echo "This installs Zerminal into your home directory (no sudo). The"
    echo "built-in updater will apply future releases automatically."
    echo

    detect_system_package_install

    confirm

    TMP="$(mktemp -d "${TMPDIR:-/tmp}/zerminal-XXXXXX")"

    echo "Downloading ${TARBALL_URL}"
    fetch "${TARBALL_URL}" > "${TMP}/zerminal.tar.gz"

    mkdir -p "${INSTALL_ROOT}"
    # --delete-ish: blow away any prior contents so we don't mix old + new files.
    if [ -d "${APP_DIR}" ]; then
        rm -rf "${APP_DIR}"
    fi
    tar -xzf "${TMP}/zerminal.tar.gz" -C "${INSTALL_ROOT}"

    [ -x "${APP_DIR}/bin/zerminal" ] || die \
        "tarball did not contain expected ${APP_DIR}/bin/zerminal"

    install_symlinks
    refresh_desktop_db

    echo
    echo "Installed Zerminal $(${APP_DIR}/bin/zerminal --version 2>/dev/null || echo "${VERSION}")"

    warn_path

    echo "Done. Run with: zerminal"
}

# If Zerminal is already installed via the distro package manager, the system
# binary at /usr/bin/zerminal will shadow the new ~/.local/bin/zerminal in PATH
# and the in-app updater will refuse to run. Detect and prompt to remove.
detect_system_package_install() {
    local found_via=""
    local remove_cmd=""

    if command -v rpm >/dev/null 2>&1 && rpm -q zerminal >/dev/null 2>&1; then
        found_via="rpm"
        remove_cmd="sudo dnf remove -y zerminal"
    elif command -v dpkg >/dev/null 2>&1 && dpkg -s zerminal >/dev/null 2>&1; then
        found_via="dpkg"
        remove_cmd="sudo apt-get remove -y zerminal"
    elif command -v pacman >/dev/null 2>&1 && pacman -Qi zerminal >/dev/null 2>&1; then
        found_via="pacman"
        remove_cmd="sudo pacman -R --noconfirm zerminal"
    fi

    [ -n "$found_via" ] || return 0

    cat <<EOF
Detected a system-package install of Zerminal (via ${found_via}).
That install lives under /usr/ and will shadow the new ~/.local install in PATH;
it also blocks Zerminal's in-app auto-updater.

Recommend removing it first:
    ${remove_cmd}

EOF
    if [ "${ZERMINAL_NONINTERACTIVE:-0}" = "1" ] || [ ! -t 0 ]; then
        echo "Continuing non-interactively. Remove the system package yourself afterwards."
        echo
        return 0
    fi
    printf "Remove it now? [y/N] "
    read -r ans
    case "$ans" in
        y|Y|yes)
            # shellcheck disable=SC2086
            eval $remove_cmd
            ;;
        *)
            echo "Skipping. You can remove it later with: ${remove_cmd}"
            ;;
    esac
    echo
}

install_symlinks() {
    mkdir -p "$(dirname "$BIN_LINK")"
    ln -sfn "${APP_DIR}/bin/zerminal" "${BIN_LINK}"

    if [ -f "${APP_DIR}/share/applications/dev.zerminal.Zerminal.desktop" ]; then
        mkdir -p "$(dirname "$DESKTOP_LINK")"
        ln -sfn "${APP_DIR}/share/applications/dev.zerminal.Zerminal.desktop" "${DESKTOP_LINK}"
    fi

    if [ -f "${APP_DIR}/share/icons/hicolor/512x512/apps/zerminal.png" ]; then
        mkdir -p "$(dirname "$ICON_512_LINK")"
        ln -sfn "${APP_DIR}/share/icons/hicolor/512x512/apps/zerminal.png" "${ICON_512_LINK}"
    fi
    if [ -f "${APP_DIR}/share/icons/hicolor/1024x1024/apps/zerminal.png" ]; then
        mkdir -p "$(dirname "$ICON_1024_LINK")"
        ln -sfn "${APP_DIR}/share/icons/hicolor/1024x1024/apps/zerminal.png" "${ICON_1024_LINK}"
    fi
}

# Best-effort: rebuild caches so the desktop entry and icon show up immediately.
# Both commands are no-ops if not installed, hence the `|| true`.
refresh_desktop_db() {
    command -v update-desktop-database >/dev/null 2>&1 && \
        update-desktop-database "${INSTALL_ROOT}/share/applications" >/dev/null 2>&1 || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && \
        gtk-update-icon-cache --force --quiet "${INSTALL_ROOT}/share/icons/hicolor" >/dev/null 2>&1 || true
}

warn_path() {
    case ":${PATH}:" in
        *:"${INSTALL_ROOT}/bin":*) return 0 ;;
    esac
    cat <<EOF

Note: ${INSTALL_ROOT}/bin is not in your PATH. Add it by appending to your
shell's rc file (one of these matches yours):

    # zsh
    echo 'export PATH="\$HOME/.local/bin:\$PATH"' >> ~/.zshrc

    # bash
    echo 'export PATH="\$HOME/.local/bin:\$PATH"' >> ~/.bashrc

    # fish
    fish_add_path \$HOME/.local/bin

Then open a new terminal (or 'source' the file) before running 'zerminal'.
EOF
}

confirm() {
    [ "${ZERMINAL_NONINTERACTIVE:-0}" = "1" ] && return 0
    [ -t 0 ] || return 0  # piped from curl: no TTY, proceed without prompt
    printf "Continue? [y/N] "
    read -r ans
    case "$ans" in y|Y|yes) ;; *) die "aborted" ;; esac
}

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -fsSL "$@"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "$@"
    else
        die "curl or wget required"
    fi
}

die() {
    printf "error: %s\n" "$1" >&2
    exit 1
}

main "$@"
