# Fetches what building Wisp needs into .tools/ (gitignored):
#   - Zig 0.15.2, which compiles libghostty-vt
#   - Ghostty's source at the commit libghostty-vt-sys pins (update both together)
#
# Dot-source it so Zig also lands on PATH for the current session:
#   . .\scripts\bootstrap.ps1
#   cargo run --release
$ErrorActionPreference = "Stop"
$tools = Join-Path (Split-Path $PSScriptRoot) ".tools"
$zigVersion = "0.15.2"
$zigSha256 = "3a0ed1e8799a2f8ce2a6e6290a9ff22e6906f8227865911fb7ddedc3cc14cb0c"
$ghosttyCommit = "a887df42c56f6de86c0fe6da9c4eeca37931e083"

New-Item -ItemType Directory -Force $tools | Out-Null
$zig = Join-Path $tools "zig"
if (-not (Test-Path "$zig\zig.exe")) {
    $zip = Join-Path $tools "zig.zip"
    Write-Output "Downloading Zig $zigVersion..."
    Invoke-WebRequest "https://ziglang.org/download/$zigVersion/zig-x86_64-windows-$zigVersion.zip" -OutFile $zip
    if ((Get-FileHash $zip -Algorithm SHA256).Hash -ne $zigSha256) {
        Remove-Item $zip
        throw "Zig download failed its checksum"
    }
    Expand-Archive $zip $tools
    Rename-Item (Join-Path $tools "zig-x86_64-windows-$zigVersion") "zig"
    Remove-Item $zip
}

# libghostty-vt-sys would clone this itself, but under target/ the path is too
# long for Windows. .cargo/config.toml points GHOSTTY_SOURCE_DIR here instead.
$ghostty = Join-Path $tools "ghostty"
if (-not (Test-Path "$ghostty\build.zig")) {
    Write-Output "Fetching Ghostty $ghosttyCommit..."
    git init -q $ghostty
    git -C $ghostty remote add origin https://github.com/ghostty-org/ghostty.git
    git -C $ghostty fetch -q --depth 1 origin $ghosttyCommit
    git -C $ghostty checkout -q FETCH_HEAD
}

$env:PATH = "$zig;$env:PATH"
Write-Output "Ready: Zig $(& "$zig\zig.exe" version) is on PATH for this session."
