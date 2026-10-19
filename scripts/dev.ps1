param([Parameter(ValueFromRemainingArguments = $true)][string[]]$CargoArguments)
$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
$taskTools = Join-Path (Split-Path -Parent $taskRoot) 'crossdesk-toolchain'
if (Test-Path -LiteralPath "$taskTools\cargo\bin\cargo.exe") {
    $env:RUSTUP_HOME = "$taskTools\rustup"
    $env:CARGO_HOME = "$taskTools\cargo"
    $taskNative = "$taskTools\rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained"
    $env:PATH = "$taskTools\cargo\bin;$taskNative;$env:PATH"
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "$taskNative\x86_64-w64-mingw32-gcc.exe"
    $taskLlvmRoot = Get-ChildItem -LiteralPath "$taskTools\llvm-mingw" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($taskLlvmRoot) {
        $taskLlvm = Join-Path $taskLlvmRoot.FullName 'bin'
        $env:PATH = "$taskLlvm;$env:PATH"
        $env:RUSTFLAGS = "-C dlltool=$taskLlvm\llvm-dlltool.exe"
    }
}
Push-Location $taskRoot
try {
    & cargo @CargoArguments
    $taskExit = $LASTEXITCODE
} finally { Pop-Location }
exit $taskExit
