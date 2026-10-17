# Builds a portable zip: wisp.exe, the bundled ConPTY and the license files.
#
#   . .\scripts\bootstrap.ps1
#   .\scripts\package.ps1          # -> dist\wisp-<version>-x64.zip
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
cargo build --release --manifest-path "$root\Cargo.toml"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$name = "wisp-$version-x64"
$stage = Join-Path $root "dist\$name"
Remove-Item $stage -Recurse -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "$root\target\release\wisp.exe" $stage
foreach ($file in "conpty.dll", "OpenConsole.exe") {
    if (Test-Path "$root\target\release\$file") { Copy-Item "$root\target\release\$file" $stage }
    else { Write-Warning "$file missing (run bootstrap.ps1): the package will use Windows' own ConPTY" }
}
Copy-Item "$root\LICENSE", "$root\THIRD-PARTY-NOTICES.md", "$root\README.md" $stage
New-Item -ItemType Directory -Force "$stage\docs" | Out-Null
Copy-Item "$root\docs\themes-credits.md" "$stage\docs"

$zip = Join-Path $root "dist\$name.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
$size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Output "dist\$name.zip ($size MB)"
