# Builds the release and packs it with the FFmpeg DLLs into
# target\windows\Venturi-x86_64-windows.zip. Needs scripts\install-windows-deps.ps1.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'windows-env.ps1')

Push-Location $root
try {
    # Windows PowerShell makes an error of a native command's stderr once it
    # is redirected, and cargo reports its progress there.
    $ErrorActionPreference = 'Continue'
    cargo build -p vv-app --release
    $ErrorActionPreference = 'Stop'
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }

    $out = Join-Path $root 'target\windows'
    $stage = Join-Path $out 'Venturi'
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force $stage | Out-Null

    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
    Copy-Item (Join-Path $target 'release\vv-app.exe') (Join-Path $stage 'venturi.exe')
    Copy-Item (Join-Path $env:FFMPEG_DIR 'bin\*.dll') $stage
    Copy-Item 'LICENSE' (Join-Path $stage 'LICENSE.txt')
    Copy-Item (Join-Path $env:FFMPEG_DIR 'LICENSE.txt') (Join-Path $stage 'FFMPEG-LICENSE.txt')

    $zip = Join-Path $out 'Venturi-x86_64-windows.zip'
    if (Test-Path $zip) { Remove-Item $zip }
    Compress-Archive -Path $stage -DestinationPath $zip
    Write-Host "Built $zip"
}
finally {
    Pop-Location
}
