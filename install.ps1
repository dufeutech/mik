# mik installer script for Windows
# Usage: irm https://raw.githubusercontent.com/dufeutech/mik/main/install.ps1 | iex
#
# Options (via env vars):
#   $env:MIK_VERSION = "v0.1.0"           Install specific version
#   $env:MIK_NO_COMPLETIONS = "1"         Skip shell completions
#   $env:MIK_INSTALL_DIR = "C:\mik"       Custom install directory

$ErrorActionPreference = "Stop"

function Write-Info { Write-Host "==> " -ForegroundColor Blue -NoNewline; Write-Host $args }
function Write-Success { Write-Host "==> " -ForegroundColor Green -NoNewline; Write-Host $args }
function Write-Warn { Write-Host "==> " -ForegroundColor Yellow -NoNewline; Write-Host $args }

function Get-LatestVersion {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/dufeutech/mik/releases/latest"
    return $release.tag_name
}

function Install-Mik {
    Write-Host ""
    Write-Host "  ┌─────────────────────────────────────┐"
    Write-Host "  │  mik - WASI HTTP Component Runtime  │"
    Write-Host "  └─────────────────────────────────────┘"
    Write-Host ""

    # Configuration
    $version = if ($env:MIK_VERSION) { $env:MIK_VERSION } else { Get-LatestVersion }
    $installDir = if ($env:MIK_INSTALL_DIR) { $env:MIK_INSTALL_DIR } else { "$env:LOCALAPPDATA\mik\bin" }
    $platform = "x86_64-pc-windows-msvc"

    Write-Info "Platform: $platform"
    Write-Info "Version: $version"
    Write-Info "Install directory: $installDir"
    Write-Host ""

    # Create install directory
    if (-not (Test-Path $installDir)) {
        New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    }

    # Download
    $url = "https://github.com/dufeutech/mik/releases/download/$version/mik-$platform.zip"
    $zipPath = "$env:TEMP\mik.zip"
    
    Write-Info "Downloading mik $version..."
    try {
        Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing
    } catch {
        Write-Host "Error: Failed to download from $url" -ForegroundColor Red
        Write-Host "Make sure the version exists: https://github.com/dufeutech/mik/releases" -ForegroundColor Red
        exit 1
    }

    # Extract
    Write-Info "Extracting..."
    Expand-Archive -Path $zipPath -DestinationPath $env:TEMP\mik-extract -Force
    Move-Item -Path "$env:TEMP\mik-extract\mik.exe" -Destination "$installDir\mik.exe" -Force
    
    # Cleanup
    Remove-Item -Path $zipPath -Force -ErrorAction SilentlyContinue
    Remove-Item -Path "$env:TEMP\mik-extract" -Recurse -Force -ErrorAction SilentlyContinue

    Write-Success "Installed mik to $installDir\mik.exe"

    # Install completions
    if (-not $env:MIK_NO_COMPLETIONS) {
        Write-Host ""
        Write-Info "Installing PowerShell completions..."
        
        $completionScript = & "$installDir\mik.exe" completions powershell
        
        # Ensure profile directory exists
        $profileDir = Split-Path -Parent $PROFILE
        if (-not (Test-Path $profileDir)) {
            New-Item -ItemType Directory -Path $profileDir -Force | Out-Null
        }
        
        # Check if already in profile
        if (Test-Path $PROFILE) {
            $profileContent = Get-Content $PROFILE -Raw -ErrorAction SilentlyContinue
            if ($profileContent -and $profileContent.Contains("Register-ArgumentCompleter -Native -CommandName 'mik'")) {
                Write-Success "PowerShell completions already configured"
            } else {
                Add-Content -Path $PROFILE -Value "`n# mik completions`n$completionScript"
                Write-Success "PowerShell completions added to $PROFILE"
            }
        } else {
            Set-Content -Path $PROFILE -Value "# mik completions`n$completionScript"
            Write-Success "PowerShell completions added to $PROFILE"
        }
        
        Write-Host "    Restart PowerShell or run: . `$PROFILE"
    }

    # Check PATH
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -notlike "*$installDir*") {
        Write-Host ""
        Write-Warn "$installDir is not in your PATH"
        
        $addToPath = Read-Host "Add to PATH? (Y/n)"
        if ($addToPath -ne "n" -and $addToPath -ne "N") {
            [Environment]::SetEnvironmentVariable("Path", "$userPath;$installDir", "User")
            $env:Path = "$env:Path;$installDir"
            Write-Success "Added to PATH (restart terminal for full effect)"
        } else {
            Write-Host "    Add manually: `$env:Path += `";$installDir`""
        }
    }

    Write-Host ""
    Write-Success "Installation complete!"
    Write-Host ""
    Write-Host "  Get started:"
    Write-Host "    mik new my-service"
    Write-Host "    cd my-service"
    Write-Host "    mik build -rc"
    Write-Host "    mik dev"
    Write-Host ""
}

Install-Mik
