#!/usr/bin/env bash
set -euo pipefail

# Zerminal Linux installer.
#   curl -fsSL https://github.com/elleryfamilia/zerminal/releases/latest/download/install.sh | sh
#
# Detects Debian/Ubuntu, Fedora/RHEL, or Arch families and installs the
# matching signed package from the latest GitHub release.
#
# Overrides:
#   ZERMINAL_VERSION=v0.1.10  pin to a specific release (default: latest)
#   ZERMINAL_NONINTERACTIVE=1 skip the confirmation prompt

REPO="elleryfamilia/zerminal"
VERSION="${ZERMINAL_VERSION:-latest}"
if [ "$VERSION" = "latest" ]; then
    BASE="https://github.com/${REPO}/releases/latest/download"
else
    BASE="https://github.com/${REPO}/releases/download/${VERSION}"
fi
KEY_URL="${BASE}/zerminal-rpm-signing-key.asc"

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

    local family
    family="$(detect_family)"
    [ "$family" != "unknown" ] || die \
        "Unsupported distro. See https://github.com/${REPO}/releases/${VERSION}"

    SUDO="$(sudo_cmd)"
    local tmp
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/zerminal-XXXXXX")"
    trap 'rm -rf "$tmp"' EXIT

    echo "Zerminal installer"
    echo "  repo:    ${REPO}"
    echo "  version: ${VERSION}"
    echo "  family:  ${family}"
    echo
    "preview_${family}"
    echo
    confirm

    "install_${family}" "$tmp"

    echo
    echo "Done. Run with: zerminal"
}

preview_debian() {
    cat <<EOF
This will:
  1. Download zerminal-amd64.deb from ${BASE}
  2. Install it with: ${SUDO:+sudo }dpkg -i zerminal.deb || ${SUDO:+sudo }apt-get install -f -y
EOF
}

preview_rhel() {
    cat <<EOF
This will:
  1. Import the GPG signing key from ${KEY_URL}
  2. Install zerminal-x86_64.rpm with: ${SUDO:+sudo }dnf install --setopt=localpkg_gpgcheck=1 -y <url>
     dnf will reject the package if its signature does not match the imported key.
EOF
}

preview_arch() {
    cat <<EOF
This will:
  1. Verify base-devel is installed (required by makepkg)
  2. Download PKGBUILD from ${BASE}
  3. Build and install with: makepkg -si --noconfirm
EOF
}

install_debian() {
    local tmp="$1"
    need apt-get
    fetch "${BASE}/zerminal-amd64.deb" > "$tmp/zerminal.deb"
    $SUDO dpkg -i "$tmp/zerminal.deb" || $SUDO apt-get install -f -y
}

install_rhel() {
    local tmp="$1"
    need rpm
    if ! command -v dnf >/dev/null 2>&1; then
        die "dnf required (yum-only systems are unsupported)"
    fi

    fetch "$KEY_URL" > "$tmp/zerminal.asc"
    $SUDO rpm --import "$tmp/zerminal.asc"

    echo "Imported GPG key:"
    gpg --show-keys --with-fingerprint "$tmp/zerminal.asc" 2>/dev/null \
        | grep -E '^\s+[0-9A-F ]{40,}' || true
    echo "Cross-check this fingerprint against:"
    echo "  - https://github.com/${REPO}/blob/main/SECURITY.md"
    echo "  - https://github.com/elleryfamilia/homebrew-zerminal/blob/main/README.md"
    echo "  - keys.openpgp.org (search ellery@familia.me)"
    echo

    # localpkg_gpgcheck=1 makes dnf enforce signature verification at install time.
    $SUDO dnf install --setopt=localpkg_gpgcheck=1 -y "${BASE}/zerminal-x86_64.rpm"
}

install_arch() {
    local tmp="$1"
    need makepkg
    if ! pacman -Qg base-devel >/dev/null 2>&1; then
        die "base-devel required: sudo pacman -S --needed base-devel"
    fi
    fetch "${BASE}/PKGBUILD" > "$tmp/PKGBUILD"
    ( cd "$tmp" && makepkg -si --noconfirm )
}

detect_family() {
    [ -r /etc/os-release ] || { echo unknown; return; }
    # shellcheck disable=SC1091
    . /etc/os-release
    for id in "${ID:-}" ${ID_LIKE:-}; do
        case "$id" in
            debian|ubuntu|linuxmint|pop|elementary|zorin|kali) echo debian; return ;;
            fedora|rhel|centos|rocky|almalinux|nobara)         echo rhel;   return ;;
            arch|manjaro|endeavouros|garuda|cachyos)           echo arch;   return ;;
        esac
    done
    echo unknown
}

sudo_cmd() {
    if [ "$(id -u)" -eq 0 ]; then
        echo ""
    elif command -v sudo >/dev/null 2>&1; then
        echo "sudo"
    else
        die "root or sudo required"
    fi
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

need() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

die() {
    printf "error: %s\n" "$1" >&2
    exit 1
}

main "$@"
