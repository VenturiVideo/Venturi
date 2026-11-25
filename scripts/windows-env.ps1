# Build environment for this PowerShell session. Dot-source it:
#   . scripts\windows-env.ps1
$deps = if ($env:VENTURI_DEPS) { $env:VENTURI_DEPS } else { Join-Path $env:LOCALAPPDATA 'venturi-deps' }
$ffmpeg = Join-Path $deps 'ffmpeg'
if (-not (Test-Path (Join-Path $ffmpeg 'bin\avcodec-*.dll'))) {
    throw "FFmpeg not found in ${ffmpeg}: run scripts\install-windows-deps.ps1"
}
$env:FFMPEG_DIR = $ffmpeg

if (-not $env:LIBCLANG_PATH) {
    $env:LIBCLANG_PATH = Join-Path $env:ProgramFiles 'LLVM\bin'
}
if (-not (Test-Path (Join-Path $env:LIBCLANG_PATH 'libclang.dll'))) {
    throw "libclang.dll not found in $env:LIBCLANG_PATH: run scripts\install-windows-deps.ps1"
}

# The FFmpeg DLLs are loaded from PATH, by the app and by the tests' ffmpeg.exe.
foreach ($dir in @((Join-Path $ffmpeg 'bin'), (Join-Path $env:USERPROFILE '.cargo\bin'))) {
    if (($env:PATH -split ';') -notcontains $dir) {
        $env:PATH = "$dir;$env:PATH"
    }
}

# libclang takes the C headers from the newest Visual Studio, which may be one
# without the C++ tools: INCLUDE names the headers of the one that has them.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) {
    throw 'Visual Studio Build Tools not found: run scripts\install-windows-deps.ps1'
}
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) {
    throw 'No Visual Studio with the C++ tools: run scripts\install-windows-deps.ps1'
}
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
$include = cmd /c "`"$vcvars`" >nul 2>&1 && set INCLUDE" | Where-Object { $_ -like 'INCLUDE=*' }
if (-not $include) {
    throw "vcvars64.bat set no INCLUDE: $vcvars"
}
$env:INCLUDE = $include.Substring('INCLUDE='.Length)
