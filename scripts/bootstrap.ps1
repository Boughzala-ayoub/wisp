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

# Microsoft's ConPTY package: a newer conhost (OpenConsole) than the one in
# Windows. build.rs copies it next to wisp.exe; Wisp falls back to Windows'
# own if it's missing.
$conptyVersion = "1.25.260930003"
$conptySha512 = "LuqMk9r5oTzeRne35IOlUdSQ7DkjorOIbcnODGZ81MvmUpTr/1VBQz3fAdCNCwkfuR0Mluq+H7rS3Hes/A1fOw=="
$conpty = Join-Path $tools "conpty"
if (-not (Test-Path "$conpty\OpenConsole.exe")) {
    Write-Output "Downloading Microsoft.Windows.Console.ConPTY $conptyVersion..."
    $nupkg = Join-Path $tools "conpty.zip"
    Invoke-WebRequest "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$conptyVersion/microsoft.windows.console.conpty.$conptyVersion.nupkg" -OutFile $nupkg
    $hash = [Convert]::ToBase64String([Security.Cryptography.SHA512]::Create().ComputeHash([IO.File]::ReadAllBytes($nupkg)))
    if ($hash -ne $conptySha512) {
        Remove-Item $nupkg
        throw "ConPTY package failed its checksum"
    }
    $unpacked = Join-Path $tools "conpty-package"
    Expand-Archive $nupkg $unpacked -Force
    New-Item -ItemType Directory -Force $conpty | Out-Null
    Copy-Item "$unpacked\runtimes\win-x64\native\conpty.dll" $conpty
    Copy-Item "$unpacked\build\native\runtimes\x64\OpenConsole.exe" $conpty
    Remove-Item $nupkg
    Remove-Item $unpacked -Recurse
}

$env:PATH = "$zig;$env:PATH"
Write-Output "Ready: Zig $(& "$zig\zig.exe" version) is on PATH for this session."
