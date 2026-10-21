$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $Root
npm --prefix web ci
if ($LASTEXITCODE -ne 0) { throw "npm ci failed" }
& "$Root\web\node_modules\.bin\tauri.cmd" build --features gui --bundles nsis
if ($LASTEXITCODE -ne 0) { throw "GUI build failed" }
$Installer = Get-ChildItem "$Root\target\release\bundle\nsis\*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $Installer) { throw "CrossDesk NSIS installer is missing" }
python scripts/package_gui.py --artifact $Installer.FullName --platform Windows-x64 --output dist/gui
if ($LASTEXITCODE -ne 0) { throw "Packaging failed" }
