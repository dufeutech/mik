#!/bin/bash
# mik installer script
# Usage: curl -fsSL https://raw.githubusercontent.com/dufeut/mik/main/install.sh | bash
#
# Options (via env vars):
#   MIK_VERSION=v0.1.0    Install specific version (default: latest)
#   MIK_NO_COMPLETIONS=1  Skip shell completions
#   MIK_INSTALL_DIR=~/.local/bin  Custom install directory

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() { echo -e "${BLUE}==>${NC} $1"; }
success() { echo -e "${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}==>${NC} $1"; }
error() { echo -e "${RED}Error:${NC} $1" >&2; exit 1; }

# Detect OS and architecture
detect_platform() {
    local os arch

    case "$(uname -s)" in
        Linux*)  os="unknown-linux" ;;
        Darwin*) os="apple-darwin" ;;
        MINGW*|MSYS*|CYGWIN*) os="pc-windows-msvc" ;;
        *) error "Unsupported OS: $(uname -s)" ;;
    esac

    case "$(uname -m)" in
        x86_64|amd64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) error "Unsupported architecture: $(uname -m)" ;;
    esac

    # Linux: prefer musl for better portability
    if [[ "$os" == "unknown-linux" ]]; then
        if ldd --version 2>&1 | grep -q musl; then
            os="unknown-linux-musl"
        else
            os="unknown-linux-gnu"
        fi
    fi

    echo "${arch}-${os}"
}

# Get latest version from GitHub
get_latest_version() {
    curl -fsSL "https://api.github.com/repos/dufeut/mik/releases/latest" \
        | grep '"tag_name"' \
        | sed -E 's/.*"([^"]+)".*/\1/'
}

# Download and install binary
install_binary() {
    local version="$1"
    local platform="$2"
    local install_dir="$3"

    local ext="tar.gz"
    [[ "$platform" == *"windows"* ]] && ext="zip"

    local url="https://github.com/dufeut/mik/releases/download/${version}/mik-${platform}.${ext}"
    local tmp_dir
    tmp_dir=$(mktemp -d)
    trap "rm -rf $tmp_dir" EXIT

    info "Downloading mik ${version} for ${platform}..."
    curl -fsSL "$url" -o "$tmp_dir/mik.${ext}" || error "Failed to download from $url"

    info "Extracting..."
    cd "$tmp_dir"
    if [[ "$ext" == "zip" ]]; then
        unzip -q "mik.${ext}"
    else
        tar -xzf "mik.${ext}"
    fi

    # Create install directory if needed
    mkdir -p "$install_dir"

    # Install binary
    local binary="mik"
    [[ "$platform" == *"windows"* ]] && binary="mik.exe"

    mv "$binary" "$install_dir/" || error "Failed to install. Try: sudo $0"
    chmod +x "$install_dir/$binary"

    success "Installed mik to $install_dir/$binary"
}

# Detect current shell
detect_shell() {
    local shell_name
    shell_name=$(basename "${SHELL:-/bin/bash}")

    case "$shell_name" in
        bash|zsh|fish) echo "$shell_name" ;;
        *) echo "bash" ;;  # Default to bash
    esac
}

# Install shell completions
install_completions() {
    local install_dir="$1"
    local mik_bin="$install_dir/mik"
    local shell_name
    shell_name=$(detect_shell)

    [[ ! -x "$mik_bin" ]] && return

    info "Installing $shell_name completions..."

    case "$shell_name" in
        bash)
            local bash_dir="${BASH_COMPLETION_USER_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions}"
            mkdir -p "$bash_dir"
            "$mik_bin" completions bash > "$bash_dir/mik"
            success "Bash completions installed to $bash_dir/mik"
            echo "    Restart your shell or run: source $bash_dir/mik"
            ;;
        zsh)
            local zsh_dir="${ZDOTDIR:-$HOME}/.zfunc"
            mkdir -p "$zsh_dir"
            "$mik_bin" completions zsh > "$zsh_dir/_mik"
            success "Zsh completions installed to $zsh_dir/_mik"
            if ! grep -q 'fpath+=.*\.zfunc' "${ZDOTDIR:-$HOME}/.zshrc" 2>/dev/null; then
                echo ""
                warn "Add this to your ~/.zshrc:"
                echo "    fpath+=~/.zfunc"
                echo "    autoload -Uz compinit && compinit"
            fi
            ;;
        fish)
            local fish_dir="${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions"
            mkdir -p "$fish_dir"
            "$mik_bin" completions fish > "$fish_dir/mik.fish"
            success "Fish completions installed to $fish_dir/mik.fish"
            ;;
    esac
}

# Check if directory is in PATH
check_path() {
    local dir="$1"
    if [[ ":$PATH:" != *":$dir:"* ]]; then
        echo ""
        warn "$dir is not in your PATH"
        echo "    Add this to your shell profile:"
        echo "    export PATH=\"$dir:\$PATH\""
    fi
}

# Main
main() {
    echo ""
    echo "  ┌─────────────────────────────────────┐"
    echo "  │  mik - WASI HTTP Component Runtime  │"
    echo "  └─────────────────────────────────────┘"
    echo ""

    # Configuration
    local version="${MIK_VERSION:-$(get_latest_version)}"
    local install_dir="${MIK_INSTALL_DIR:-$HOME/.local/bin}"
    local platform
    platform=$(detect_platform)

    info "Platform: $platform"
    info "Version: $version"
    info "Install directory: $install_dir"
    echo ""

    # Install binary
    install_binary "$version" "$platform" "$install_dir"

    # Install completions (unless disabled)
    if [[ -z "${MIK_NO_COMPLETIONS:-}" ]]; then
        echo ""
        install_completions "$install_dir"
    fi

    # Check PATH
    check_path "$install_dir"

    echo ""
    success "Installation complete!"
    echo ""
    echo "  Get started:"
    echo "    mik new my-service"
    echo "    cd my-service"
    echo "    mik build -rc"
    echo "    mik dev"
    echo ""
}

main "$@"
