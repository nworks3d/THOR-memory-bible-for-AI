# Install THOR on Windows, in one command. Open PowerShell and paste:
#
#   irm https://raw.githubusercontent.com/nworks3d/THOR-memory-bible-for-AI/main/install.ps1 | iex
#
# It downloads the latest release, checks the download against the checksum
# published beside it, unpacks it into your home folder, and then runs THOR's
# own setup - which creates the memory and points Claude Code at it.
#
# Two files of yours are touched by that last step, and both are backed up to
# <name>.bak first: Claude Code's settings.json (the hooks) and .claude.json
# (the memory server). Set $env:THOR_NO_SETUP = "1" beforehand to unpack only
# and run the setup yourself later.
#
# Nothing here needs administrator rights, and nothing is installed outside
# your own user folder.

$ErrorActionPreference = 'Stop'
# Without this, Invoke-WebRequest spends most of its time drawing a progress
# bar rather than downloading, which on a file this size is minutes wasted.
$ProgressPreference = 'SilentlyContinue'

$repo = 'nworks3d/THOR-memory-bible-for-AI'
$dest = $env:THOR_HOME
if (-not $dest) { $dest = Join-Path $HOME 'thor2' }

$asset = 'thor2-windows-x86_64.zip'
$base = "https://github.com/$repo/releases/latest/download"

if ([Environment]::Is64BitOperatingSystem -ne $true) {
    throw "Only 64-bit Windows is published. Build from source for anything else."
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("thor-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    $zip = Join-Path $tmp $asset
    $sha = "$zip.sha256"

    Write-Host "Downloading THOR (about 60 MB)..."
    try {
        Invoke-WebRequest -Uri "$base/$asset" -OutFile $zip
        Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile $sha
    } catch {
        throw "Download failed: $($_.Exception.Message). You can also grab the file by hand from https://github.com/$repo/releases/latest"
    }

    # The checksum file is "<hash>  <filename>", so only the first field counts.
    $want = ((Get-Content $sha -First 1) -split '\s+')[0]
    $got = (Get-FileHash -Path $zip -Algorithm SHA256).Hash
    if (-not $want) { throw "The checksum file was empty." }
    if ($want -ne $got) {
        throw "The download does not match its published checksum. Expected $want, got $got. Nothing was installed."
    }
    Write-Host "Checksum matches."

    # The archive is flat, so it goes straight into its own directory. An
    # earlier install is overwritten in place; the memory itself lives
    # elsewhere and is never touched by this.
    if (-not (Test-Path $dest)) { New-Item -ItemType Directory -Path $dest | Out-Null }
    Expand-Archive -Path $zip -DestinationPath $dest -Force

    # Files that came out of a downloaded archive are marked as coming from the
    # internet, and Windows then refuses to run them without a warning. Clear
    # that mark on what we just unpacked.
    Get-ChildItem -Path $dest -File | ForEach-Object {
        try { Unblock-File -Path $_.FullName -ErrorAction Stop } catch {}
    }
    Write-Host "Unpacked into $dest"

    if ($env:THOR_NO_SETUP -eq '1') {
        Write-Host ""
        Write-Host "Skipped the setup because THOR_NO_SETUP is 1. Run it yourself with:"
        Write-Host "  & '$dest\install.exe'"
        return
    }

    Write-Host "Setting up..."
    & (Join-Path $dest 'install.exe')

    Write-Host ""
    Write-Host "Done. Restart Claude Code so it picks up the memory server."
    Write-Host "Check on it any time with: & '$dest\doctor.exe' --db <your store>"
}
finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
