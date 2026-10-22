$ErrorActionPreference = 'Stop'
$taskPackage = Split-Path -Parent $PSScriptRoot
$taskExecutable = Join-Path $taskPackage 'crossdesk.exe'
if (-not (Test-Path -LiteralPath $taskExecutable)) {
    $taskExecutable = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\crossdesk.exe'
}
if (-not (Test-Path -LiteralPath $taskExecutable)) { throw 'Build crossdesk.exe first.' }
$taskKey = Join-Path $taskPackage 'crossdesk.key'
if (-not (Test-Path -LiteralPath $taskKey)) {
    & $taskExecutable keygen --out $taskKey
    if ($LASTEXITCODE -ne 0) { throw 'Pairing key generation failed.' }
}
Write-Host ''
Write-Host 'CrossDesk: Windows keyboard and mouse -> Mac; both screen edges enabled.'
Write-Host "Copy this pairing file to the Mac package folder: $taskKey"
Write-Host 'Both ends need CrossDesk 0.5.0 / protocol v7. Image and file/folder clipboard sync are on by default.'
Write-Host 'Windows LAN address candidates:'
Get-NetIPConfiguration -ErrorAction SilentlyContinue |
    Where-Object { $_.IPv4DefaultGateway } |
    ForEach-Object { $_.IPv4Address.IPAddress } |
    ForEach-Object { Write-Host "  $_`:24920" }
Write-Host 'Emergency release: Ctrl+Alt+Esc or Scroll Lock. Ctrl+C stops the service.'
Write-Host 'When Windows Firewall prompts, allow the private network used by your Mac.'
Write-Host ''
& $taskExecutable server --edge both --key-file $taskKey
exit $LASTEXITCODE
