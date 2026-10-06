param(
    [switch]$Cuda,
    [switch]$Test,
    [switch]$Check,
    [switch]$Lint,
    [switch]$Package,
    [string]$CudaArchitectures = 'native'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Push-Location (Split-Path -Parent $PSScriptRoot)
# vcvars appends to PATH; restore the caller's environment so repeated builds in one session work.
$savedEnvironment = [Environment]::GetEnvironmentVariables('Process')
try {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (!(Test-Path -LiteralPath $vswhere)) { throw 'Install Visual Studio Desktop development with C++ (MSVC and Windows SDK).' }
    $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (!$vs) { throw 'MSVC C++ build tools were not found.' }
    $vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
    # CUDA 13 supports MSVC 14.44. Prefer that toolset if VS 2026 also has it installed.
    $toolset = if (Test-Path -LiteralPath (Join-Path $vs 'VC\Tools\MSVC\14.44.35207')) { '-vcvars_ver=14.44' } else { '' }
    $devEnvironment = & cmd.exe /d /c "call `"$vcvars`" $toolset >nul && set"
    if ($LASTEXITCODE -ne 0) { throw 'Could not initialize the MSVC environment.' }
    foreach ($line in $devEnvironment) {
        if ($line -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process') }
    }
    $ninja = Join-Path $vs 'Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja\ninja.exe'
    if (!(Test-Path -LiteralPath $ninja)) { throw 'Install the Visual Studio CMake tools (Ninja).' }
    $env:CMAKE_GENERATOR = 'Ninja'
    $env:CMAKE_MAKE_PROGRAM = $ninja
    Remove-Item Env:WHISPER_DONT_GENERATE_BINDINGS -ErrorAction SilentlyContinue
    if (!$env:LIBCLANG_PATH) {
        foreach ($candidate in @((Join-Path $PWD '.tools\libclang\clang\native'), (Join-Path $env:ProgramFiles 'LLVM\bin'))) {
            if (Test-Path -LiteralPath (Join-Path $candidate 'libclang.dll')) { $env:LIBCLANG_PATH = $candidate; break }
        }
    }
    if (!$env:LIBCLANG_PATH) { throw 'Install LLVM and set LIBCLANG_PATH to the directory containing libclang.dll. See README.md.' }
    $env:GGML_NATIVE = 'OFF'
    $cargoArgs = @('--locked')
    if ($Cuda) {
        if (!$env:CUDA_PATH) {
            $nvcc = (Get-Command nvcc.exe -ErrorAction Stop).Source
            $env:CUDA_PATH = Split-Path -Parent (Split-Path -Parent $nvcc)
        }
        $env:CMAKE_CUDA_ARCHITECTURES = $CudaArchitectures
        $cargoArgs += @('--features', 'cuda', '--target-dir', 'target/cuda')
    }
    if ($Lint) { & cargo clippy @cargoArgs --all-targets -- -D warnings }
    elseif ($Check) { & cargo check @cargoArgs }
    elseif ($Test) { & cargo test @cargoArgs }
    else { & cargo build --release @cargoArgs }
    if ($LASTEXITCODE -ne 0) { throw 'Cargo failed.' }
    if ($Package) {
        if ($Check -or $Test -or $Lint) { throw '-Package requires a release build.' }
        New-Item -ItemType Directory -Force -Path 'dist' | Out-Null
        if ($Cuda) {
            Copy-Item -LiteralPath 'target/cuda/release/whisper-pause-break.exe' -Destination 'dist/whisper-pause-break-cuda.exe'
            foreach ($pattern in @('cublas64_*.dll', 'cublasLt64_*.dll', 'cudart64_*.dll')) {
                $dlls = @(Get-ChildItem -LiteralPath (Join-Path $env:CUDA_PATH 'bin') -Recurse -Filter $pattern)
                if ($dlls.Count -eq 0) { throw "Missing CUDA runtime: $pattern" }
                foreach ($dll in $dlls) { Copy-Item -LiteralPath $dll.FullName -Destination 'dist' }
            }
        } else {
            Copy-Item -LiteralPath 'target/release/whisper-pause-break.exe' -Destination 'dist'
        }
        Copy-Item -LiteralPath 'README.md','LICENSE','config.example.toml' -Destination 'dist'
        Write-Host 'Release files are in dist/.'
    }
} finally {
    foreach ($name in @([Environment]::GetEnvironmentVariables('Process').Keys)) {
        if (!$savedEnvironment.Contains($name)) { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
    }
    foreach ($entry in $savedEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
    Pop-Location
}
