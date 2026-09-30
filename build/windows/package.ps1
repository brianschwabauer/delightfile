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

Licenses\ holds delightfile's license, FFmpeg's COPYING.GPLv3 (the BtbN
zip's LICENSE.txt, which is that file byte for byte), pdfium's licenses and
SOURCES.txt, which names the exact FFmpeg (from the BtbN folder's name) and
where its source is: FFmpeg's repository at the commit that name ends in, and
BtbN's build scripts at the release ffmpeg.lock names. Each file ends in .txt
so a double-click opens it. The BtbN zip carries no LICENSE.md; SOURCES.txt
says where it is.

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
    $licenses = Join-Path $dir 'Licenses'
    foreach ($sub in 'delightfile', 'FFmpeg', 'pdfium') {
        New-Item -ItemType Directory -Force (Join-Path $licenses $sub) | Out-Null
    }
    Copy-Item (Join-Path $repo 'LICENSE') (Join-Path $licenses 'delightfile\LICENSE.txt')
    Copy-Item (Join-Path $FfmpegDir 'LICENSE.txt') (Join-Path $licenses 'FFmpeg\COPYING.GPLv3.txt')
    $pdfium_root = Split-Path (Split-Path $Pdfium)
    Copy-Item (Join-Path $pdfium_root 'LICENSE') (Join-Path $licenses 'pdfium\LICENSE.txt')
    Copy-Item (Join-Path $pdfium_root 'licenses\*') (Join-Path $licenses 'pdfium')

    # Which FFmpeg, from BtbN's folder name (ffmpeg-n9.0.1-11-ge47273f4d9-
    # win64-gpl-shared-9.0: git describe of the FFmpeg commit), and which
    # BtbN release, from the lock's URL.
    $ffmpeg_name = Split-Path (Resolve-Path $FfmpegDir).Path -Leaf
    if ($ffmpeg_name -notmatch '^ffmpeg-(n[0-9.]+(?:-[0-9]+-g([0-9a-f]+))?)-win64-gpl-shared') {
        throw "$ffmpeg_name is not named as BtbN's GPL shared builds are, so SOURCES.txt cannot say where its source is"
    }
    $ffmpeg_version = $Matches[1]
    $ffmpeg_ref = if ($Matches[2]) { $Matches[2] } else { $ffmpeg_version }
    $lock_url = (Get-Content (Join-Path $PSScriptRoot 'ffmpeg.lock'))[0].Trim()
    if ($lock_url -notmatch '/releases/download/([^/]+)/') { throw "ffmpeg.lock's URL names no BtbN release" }
    $btbn_release = $Matches[1]
    $pdfium_lock_url = (Get-Content (Join-Path $PSScriptRoot 'pdfium.lock'))[0].Trim()
    $pdfium_release = ($pdfium_lock_url -replace '^.*/releases/download/', '') -replace '/[^/]+$', ''
    $sources = @"
Where the source is of what this folder carries

delightfile $version
  License: GPL-3.0-or-later, Licenses\delightfile\LICENSE.txt
  Source:  https://github.com/brianschwabauer/delightfile (tag v$version)

FFmpeg $ffmpeg_version, the DLLs avcodec, avformat, avutil, swresample and
swscale: BtbN/FFmpeg-Builds release $btbn_release, win64 GPL shared.
  License: GPL-3.0-or-later, Licenses\FFmpeg\COPYING.GPLv3.txt (the build's
           LICENSE.txt). FFmpeg's LICENSE.md, which says which parts are under
           which license, is in the source below.
  Source:  FFmpeg's repository at $ffmpeg_ref
           https://git.ffmpeg.org/ffmpeg.git
           https://github.com/FFmpeg/FFmpeg/tree/$ffmpeg_ref (mirror)
  Build:   the scripts that built these DLLs, which name the version of every
           library built into them
           https://github.com/BtbN/FFmpeg-Builds/tree/$btbn_release

pdfium, bblanchon/pdfium-binaries $pdfium_release, pdfium.dll
  License: Licenses\pdfium\LICENSE.txt, and beside it the licenses of what it
           builds in
  Source:  https://github.com/bblanchon/pdfium-binaries/releases/tag/$pdfium_release
"@
    [System.IO.File]::WriteAllText((Join-Path $licenses 'SOURCES.txt'), (($sources + "`n") -replace "`r?`n", "`r`n"))

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
