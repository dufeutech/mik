#!/bin/bash
# Run cargo clippy in WSL to catch Unix-specific issues
# This script is called from pre-commit on Windows
# On Linux/macOS, Unix code is already checked by regular clippy

set -e

# Detect OS - only run on Windows
case "$(uname -s)" in
    Linux|Darwin)
        # Unix systems already compile Unix code with regular clippy
        exit 0
        ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT)
        # Windows - continue to WSL check
        ;;
    *)
        # Unknown OS, skip
        exit 0
        ;;
esac

# Check if WSL is available on Windows
if ! command -v wsl.exe &>/dev/null; then
    echo "WSL not available, skipping Unix check"
    echo "Install WSL to catch Unix-specific issues before CI"
    exit 0
fi

# Get the Windows path and convert to WSL path
WIN_PATH="$(pwd -W 2>/dev/null || pwd)"
WSL_PATH=$(wsl.exe wslpath -a "$WIN_PATH" 2>/dev/null | tr -d '\r')

if [ -z "$WSL_PATH" ]; then
    echo "Could not convert path to WSL format, skipping"
    exit 0
fi

echo "Running clippy in WSL at: $WSL_PATH"

# Check if cargo is available in WSL (source cargo env first)
if ! wsl.exe bash -c "source ~/.cargo/env 2>/dev/null; command -v cargo" &>/dev/null; then
    echo "Rust/Cargo not installed in WSL, skipping Unix check"
    echo "Install with: wsl.exe bash -c 'curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh'"
    exit 0
fi

# Run clippy in WSL
wsl.exe bash -c "source ~/.cargo/env 2>/dev/null; cd '$WSL_PATH' && cargo clippy --all-targets --all-features -- -D warnings"
