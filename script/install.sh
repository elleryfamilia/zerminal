#!/usr/bin/env sh
set -eu

# Installs Zerminal from GitHub Releases.
# Usage: curl -fsSL https://raw.githubusercontent.com/elleryfamilia/zerminal/main/script/install.sh | sh

REPO="elleryfamilia/zerminal"

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"

    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/zerminal-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/zerminal-XXXXXX")"
    fi
    trap 'rm -rf "$temp"' EXIT

    case "$platform" in
        Darwin) platform="macos" ;;
        Linux)  platform="linux" ;;
        *)
            echo "Unsupported platform: $platform"
            exit 1
            ;;
    esac

    case "$arch" in
        arm64*|aarch64) arch="aarch64" ;;
        x86_64|x86*|i686*) arch="x86_64" ;;
        *)
            echo "Unsupported architecture: $arch"
            exit 1
            ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        fetch() { command curl -fsSL "$@"; }
    elif command -v wget >/dev/null 2>&1; then
        fetch() { wget -qO- "$@"; }
    else
        echo "Error: curl or wget is required"
        exit 1
    fi

    if [ "$platform" = "macos" ]; then
        install_macos
    else
        install_linux
    fi
}

install_macos() {
    local asset="Zerminal-${arch}.dmg"
    local url="https://github.com/${REPO}/releases/latest/download/${asset}"

    echo "Downloading Zerminal for macOS ($arch)..."
    fetch "$url" > "$temp/$asset"

    echo "Mounting DMG..."
    hdiutil attach -quiet "$temp/$asset" -mountpoint "$temp/mount"
    app="$(cd "$temp/mount/"; echo *.app)"

    echo "Installing $app to /Applications..."
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi
    ditto "$temp/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp/mount"

    mkdir -p "$HOME/.local/bin"
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/zerminal"

    echo ""
    echo "Zerminal has been installed to /Applications/$app"
    if [ "$(command -v zerminal)" = "$HOME/.local/bin/zerminal" ]; then
        echo "Run with 'zerminal'"
    else
        echo "To run from your terminal, add ~/.local/bin to your PATH:"
        case "${SHELL:-}" in
            *zsh)  echo "  echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc && source ~/.zshrc" ;;
            *fish) echo "  fish_add_path -U $HOME/.local/bin" ;;
            *)     echo "  echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc && source ~/.bashrc" ;;
        esac
    fi
}

install_linux() {
    local distro=""
    distro="$(detect_distro)"

    case "$distro" in
        debian)  install_linux_deb ;;
        fedora)  install_linux_rpm ;;
        arch)    install_linux_arch ;;
        *)       install_linux_tarball ;;
    esac
}

detect_distro() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        case "$ID" in
            ubuntu|debian|linuxmint|pop|elementary|zorin|kali) echo "debian" ;;
            fedora|rhel|centos|rocky|alma|nobara) echo "fedora" ;;
            arch|manjaro|endeavouros|garuda|cachyos) echo "arch" ;;
            *) echo "unknown" ;;
        esac
    else
        echo "unknown"
    fi
}

install_linux_deb() {
    echo "Detected Debian/Ubuntu-based system"
    echo "Finding latest .deb package..."

    local url
    url="$(find_asset '\.deb$')"
    if [ -z "$url" ]; then
        echo "No .deb asset found, falling back to tarball..."
        install_linux_tarball
        return
    fi

    echo "Downloading .deb package..."
    fetch "$url" > "$temp/zerminal.deb"

    echo "Installing (requires sudo)..."
    sudo dpkg -i "$temp/zerminal.deb" || sudo apt-get install -f -y
    echo ""
    echo "Zerminal has been installed. Run with 'zerminal'"
}

install_linux_rpm() {
    echo "Detected Fedora/RHEL-based system"
    echo "Finding latest .rpm package..."

    local url
    url="$(find_asset '\.rpm$')"
    if [ -z "$url" ]; then
        echo "No .rpm asset found, falling back to tarball..."
        install_linux_tarball
        return
    fi

    echo "Downloading .rpm package..."
    fetch "$url" > "$temp/zerminal.rpm"

    echo "Installing (requires sudo)..."
    if command -v dnf >/dev/null 2>&1; then
        sudo dnf install -y "$temp/zerminal.rpm"
    else
        sudo rpm -U "$temp/zerminal.rpm"
    fi
    echo ""
    echo "Zerminal has been installed. Run with 'zerminal'"
}

install_linux_arch() {
    echo "Detected Arch-based system"
    echo "Finding latest PKGBUILD..."

    local url
    url="$(find_asset 'PKGBUILD$')"
    if [ -z "$url" ]; then
        echo "No PKGBUILD found, falling back to tarball..."
        install_linux_tarball
        return
    fi

    echo "Downloading PKGBUILD..."
    mkdir -p "$temp/pkg"
    fetch "$url" > "$temp/pkg/PKGBUILD"

    echo "Building and installing with makepkg (requires sudo for dependencies)..."
    cd "$temp/pkg"
    makepkg -si --noconfirm
    echo ""
    echo "Zerminal has been installed. Run with 'zerminal'"
}

install_linux_tarball() {
    echo "Installing from tarball..."
    local asset="zerminal-linux-${arch}.tar.gz"
    local url="https://github.com/${REPO}/releases/latest/download/${asset}"

    echo "Downloading Zerminal for Linux ($arch)..."
    fetch "$url" > "$temp/$asset"

    rm -rf "$HOME/.local/zerminal.app"
    mkdir -p "$HOME/.local"
    tar -xzf "$temp/$asset" -C "$HOME/.local/"

    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"
    ln -sf "$HOME/.local/zerminal.app/bin/zerminal" "$HOME/.local/bin/zerminal"

    # Install desktop file with absolute paths
    local desktop_src="$HOME/.local/zerminal.app/share/applications"
    local desktop_file
    desktop_file="$(ls "$desktop_src"/*.desktop 2>/dev/null | head -1)"
    if [ -n "$desktop_file" ]; then
        local dest="$HOME/.local/share/applications/dev.zerminal.Zerminal.desktop"
        cp "$desktop_file" "$dest"
        sed -i "s|Icon=zerminal|Icon=$HOME/.local/zerminal.app/share/icons/hicolor/512x512/apps/zerminal.png|g" "$dest"
        sed -i "s|Exec=zerminal|Exec=$HOME/.local/zerminal.app/bin/zerminal|g" "$dest"
    fi

    echo ""
    echo "Zerminal has been installed to ~/.local/zerminal.app"
    if [ "$(command -v zerminal)" = "$HOME/.local/bin/zerminal" ]; then
        echo "Run with 'zerminal'"
    else
        echo "To run from your terminal, add ~/.local/bin to your PATH:"
        case "${SHELL:-}" in
            *zsh)  echo "  echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc && source ~/.zshrc" ;;
            *fish) echo "  fish_add_path -U $HOME/.local/bin" ;;
            *)     echo "  echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc && source ~/.bashrc" ;;
        esac
    fi
}

find_asset() {
    local pattern="$1"
    fetch "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
        | grep -o '"browser_download_url": *"[^"]*"' \
        | grep -E "$pattern" \
        | head -1 \
        | sed 's/"browser_download_url": *"//;s/"$//'
}

main "$@"
