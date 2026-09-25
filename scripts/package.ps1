# Builds Onset and packs everything needed to run it on another Windows 11 PC into
# dist\Onset-<version>-windows.zip: onset.exe, onset-cli.exe (calibration and tools), the
# rekordbox offsets, and a short quick start. Close any running onset.exe first.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

cargo build --release -p onset-app -p onset-cli
if ($LASTEXITCODE -ne 0) { throw "build failed (is onset.exe still running?)" }

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$name = "Onset-$version-windows"
$out = Join-Path $root "dist\$name"
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force (Join-Path $out 'offsets') | Out-Null

Copy-Item target\release\onset.exe, target\release\onset-cli.exe $out
Copy-Item offsets\*.toml (Join-Path $out 'offsets')
@"
Onset $version - visuals for rekordbox

1. Start rekordbox 7 in Performance mode and load a track.
2. Double-click onset.exe. The launcher opens.
3. Pick the screen and a style, then press Start show. Esc comes back to the launcher.

First time with a different rekordbox version? Open the Setup tab and press Calibrate
(about two minutes). The file it writes goes in the offsets folder next to onset.exe.

Keys during the show: H stats, C now-playing card, B blackout, Left/Right scene,
F fullscreen, Esc back to the launcher.
"@ | Set-Content -Encoding utf8 (Join-Path $out 'QUICKSTART.txt')

$zip = Join-Path $root "dist\$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $out -DestinationPath $zip
Write-Host "Packed $zip"
