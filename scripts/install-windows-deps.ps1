# Installs what building Venturi on Windows x64 needs: Rust, the MSVC build
# tools, LLVM (libclang, for the FFmpeg bindings) and a shared FFmpeg build.
# Safe to run again: what is already there is skipped.
param(
    # The FFmpeg branch scripts/build-ffmpeg.sh builds on the other platforms.
    [string]$FfmpegBranch = '9.0'
)
$ErrorActionPreference = 'Stop'

function Install-WingetPackage($id, $override) {
    $arguments = @('install', '--id', $id, '-e', '--accept-source-agreements', '--accept-package-agreements')
    if ($override) { $arguments += @('--override', $override) }
    & winget @arguments
    # -1978335189: already installed and up to date.
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) {
        throw "winget install $id failed ($LASTEXITCODE)"
    }
}

if (-not (Test-Path (Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'))) {
    Install-WingetPackage 'Rustlang.Rustup'
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$hasVc = (Test-Path $vswhere) -and
    (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
if (-not $hasVc) {
    Install-WingetPackage 'Microsoft.VisualStudio.2022.BuildTools' `
        '--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
}

if (-not (Test-Path (Join-Path $env:ProgramFiles 'LLVM\bin\libclang.dll'))) {
    Install-WingetPackage 'LLVM.LLVM'
}

$deps = if ($env:VENTURI_DEPS) { $env:VENTURI_DEPS } else { Join-Path $env:LOCALAPPDATA 'venturi-deps' }
$ffmpeg = Join-Path $deps 'ffmpeg'
if (-not (Test-Path (Join-Path $ffmpeg 'bin\avcodec-*.dll'))) {
    # GPL shared build: libx264, librubberband, the voiceover encoders, NVENC,
    # Vulkan and D3D11VA, as the other platforms' own builds.
    $name = "ffmpeg-n$FfmpegBranch-latest-win64-gpl-shared-$FfmpegBranch"
    $url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$name.zip"
    New-Item -ItemType Directory -Force $deps | Out-Null
    $zip = Join-Path $deps 'ffmpeg.zip'
    Write-Host "Downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    Expand-Archive -Path $zip -DestinationPath $deps -Force
    Rename-Item (Join-Path $deps $name) $ffmpeg
    Remove-Item $zip
}

Write-Host "Done. In a new terminal: . scripts\windows-env.ps1; cargo run -p vv-app"
