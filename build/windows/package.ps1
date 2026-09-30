<#
.SYNOPSIS
The Windows release zip (plans/other-platforms/06-build-and-release.md
B6.19): delightfile-<version>-x86_64-windows.zip, one folder holding
delightfile.exe, the FFmpeg DLLs it needs, pdfium.dll and a README.

.DESCRIPTION
    $env:FFMPEG_DIR = & build\windows\fetch-ffmpeg.ps1
    & build\windows\icon.ps1
    cargo build --release --locked -p df-app
    & build\windows\fetch-pdfium.ps1
    & build\windows\package.ps1

The FFmpeg DLLs are the ones delightfile.exe imports and the ones those
import, read with dumpbin /dependents from the FFmpeg build's bin\ folder, so
avdevice, avfilter and the ffmpeg programs stay out. Everything any file in
the folder imports is then either in the folder or part of Windows, or the
script stops and names what is missing.

The zip is then checked the way a person would use it, by
check-package.ps1: unpacked into an empty folder, delightfile.exe --version
run there with every FFmpeg folder taken off PATH, and the exe's icon and
version resource (B6.20) looked for; build\windows\icon.ps1 before the build
gives it the icon.

The zip's path is the one thing written to the output.

.PARAMETER Binary
The program. Defaults to target\release\delightfile.exe.

.PARAMETER FfmpegDir
The FFmpeg build the program was linked against: the folder
fetch-ffmpeg.ps1 names. Defaults to $env:FFMPEG_DIR.

.PARAMETER Pdfium
pdfium.dll. Defaults to where fetch-pdfium.ps1 unpacks it.

.PARAMETER Out
Where the zip goes. Defaults to target\dist.
#>
param(
    [string]$Binary = (Join-Path $PSScriptRoot '..\..\target\release\delightfile.exe'),
    [string]$FfmpegDir = $env:FFMPEG_DIR,
    [string]$Pdfium = (Join-Path $PSScriptRoot '..\..\target\pdfium\win-x64\bin\pdfium.dll'),
    [string]$Out = (Join-Path $PSScriptRoot '..\..\target\dist')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$version = $null
$in_package = $false
foreach ($line in Get-Content (Join-Path $repo 'Cargo.toml')) {
    if ($line -match '^\[workspace\.package\]') { $in_package = $true; continue }
    if ($line -match '^\[') { $in_package = $false }
    if ($in_package -and $line -match '^version\s*=\s*"([^"]+)"') { $version = $Matches[1]; break }
}
if (-not $version) { throw "no [workspace.package] version in Cargo.toml" }

if (-not (Test-Path $Binary)) { throw "no release binary at $Binary; run: cargo build --release --locked -p df-app" }
if (-not $FfmpegDir -or -not (Test-Path (Join-Path $FfmpegDir 'bin'))) {
    throw "no FFmpeg build at '$FfmpegDir'; pass -FfmpegDir or set FFMPEG_DIR (build\windows\fetch-ffmpeg.ps1)"
}
if (-not (Test-Path $Pdfium)) { throw "no pdfium.dll at $Pdfium; run build\windows\fetch-pdfium.ps1" }
$ffmpeg_bin = (Resolve-Path (Join-Path $FfmpegDir 'bin')).Path

function findDumpbin {
    $on_path = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($on_path) { return $on_path.Source }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($vs) {
            $found = Get-ChildItem (Join-Path $vs 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe') -ErrorAction SilentlyContinue |
                Sort-Object FullName -Descending | Select-Object -First 1
            if ($found) { return $found.FullName }
        }
    }
    throw "dumpbin.exe was not found: it comes with Visual Studio's C++ tools, which the MSVC build needs anyway"
}
$dumpbin = findDumpbin

# The DLLs a PE file imports, load-time and delay-loaded, as dumpbin names them.
function importsOf([string]$file) {
    $lines = & $dumpbin /nologo /dependents $file
    if ($LASTEXITCODE -ne 0) { throw "dumpbin could not read $file" }
    foreach ($line in $lines) {
        if ($line -match '^\s+(\S+\.dll)\s*$') { $Matches[1] }
    }
}

$name = "delightfile-$version-x86_64-windows"
$stage = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
$dir = Join-Path $stage $name
New-Item -ItemType Directory -Force $dir | Out-Null
try {
    Copy-Item $Binary (Join-Path $dir 'delightfile.exe')
    Copy-Item $Pdfium (Join-Path $dir 'pdfium.dll')

    # The FFmpeg DLLs, walked from the program's imports.
    $queue = [System.Collections.Generic.Queue[string]]::new()
    $queue.Enqueue((Join-Path $dir 'delightfile.exe'))
    while ($queue.Count -gt 0) {
        $file = $queue.Dequeue()
        foreach ($dll in importsOf $file) {
            $from = Join-Path $ffmpeg_bin $dll
            $to = Join-Path $dir $dll
            if ((Test-Path $from) -and -not (Test-Path $to)) {
                Write-Host "  $dll  <- $from"
                Copy-Item $from $to
                $queue.Enqueue($to)
            }
        }
    }

    # Everything imported is in the folder or is Windows' own.
    $system = Join-Path $env:SystemRoot 'System32'
    $missing = @()
    foreach ($file in Get-ChildItem $dir -Include *.exe, *.dll -Recurse) {
        foreach ($dll in importsOf $file.FullName) {
            if ($dll -match '^(api|ext)-ms-') { continue }
            if (Test-Path (Join-Path $dir $dll)) { continue }
            if (Test-Path (Join-Path $system $dll)) { continue }
            $missing += "$($file.Name) imports $dll"
        }
    }
    if ($missing.Count -gt 0) { throw "not in the folder and not part of Windows: $($missing -join '; ')" }

    $readme = (Get-Content (Join-Path $PSScriptRoot 'README.txt') -Raw) -replace '@VERSION@', $version
    [System.IO.File]::WriteAllText((Join-Path $dir 'README.txt'), ($readme -replace "`r?`n", "`r`n"))
    Copy-Item (Join-Path $repo 'LICENSE') (Join-Path $dir 'LICENSE.txt')
    New-Item -ItemType Directory -Force (Join-Path $dir 'licenses\pdfium') | Out-Null
    Copy-Item (Join-Path $FfmpegDir 'LICENSE.txt') (Join-Path $dir 'licenses\ffmpeg.txt')
    $pdfium_root = Split-Path (Split-Path $Pdfium)
    Copy-Item (Join-Path $pdfium_root 'LICENSE') (Join-Path $dir 'licenses\pdfium\LICENSE.txt')
    Copy-Item (Join-Path $pdfium_root 'licenses\*') (Join-Path $dir 'licenses\pdfium')

    New-Item -ItemType Directory -Force $Out | Out-Null
    $zip = Join-Path (Resolve-Path $Out).Path "$name.zip"
    if (Test-Path $zip) { Remove-Item $zip }
    [System.IO.Compression.ZipFile]::CreateFromDirectory($dir, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $true)
} finally {
    Remove-Item -Recurse -Force $stage
}

Write-Host "packaged $name.zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"
& (Join-Path $PSScriptRoot 'check-package.ps1') -Zip $zip | Out-Host
Write-Output $zip
