<#
.SYNOPSIS
FFmpeg 9 for building and packaging delightfile on Windows: the build
ffmpeg.lock pins, downloaded, checked against its sha256 and unpacked.

.DESCRIPTION
ffmpeg.lock is two lines, the zip's URL and its sha256
(plans/other-platforms/06-build-and-release.md B6.10, where the choice of
BtbN's month-end GPL shared build is argued). The zip is kept in -Dest, so a
second run (or a CI cache of that folder) downloads nothing, and its hash is
checked on every run, cached or not. The zip unpacks to a folder named after
itself holding include\, lib\ (the MSVC import libraries) and bin\ (the
DLLs), which is what ffmpeg-sys-next wants in FFMPEG_DIR.

The folder's path is the one thing written to the output, so a caller can
take it:

    $env:FFMPEG_DIR = & build\windows\fetch-ffmpeg.ps1
    $env:PATH = "$env:FFMPEG_DIR\bin;$env:PATH"   # for cargo test and --version

.PARAMETER Dest
Where the zip and the unpacked folder go. Defaults to target\ffmpeg in the
repository.
#>
param(
    [string]$Dest = (Join-Path $PSScriptRoot '..\..\target\ffmpeg')
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$lock = Get-Content (Join-Path $PSScriptRoot 'ffmpeg.lock')
$url = $lock[0].Trim()
$sha256 = $lock[1].Trim()
$name = ($url -split '/')[-1] -replace '\.zip$', ''

New-Item -ItemType Directory -Force $Dest | Out-Null
$zip = Join-Path $Dest "$name.zip"
if (-not (Test-Path $zip)) {
    Write-Host "downloading $url"
    Invoke-WebRequest $url -OutFile $zip
}
$actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $sha256) {
    Remove-Item $zip
    throw "$name.zip has sha256 $actual; ffmpeg.lock says $sha256"
}

$dir = Join-Path $Dest $name
$header = Join-Path $dir 'include\libavcodec\version_major.h'
if (-not (Test-Path $header)) {
    # Windows' own bsdtar reads zip; named by path because Git for Windows
    # puts a GNU tar on PATH that does not.
    & "$env:SystemRoot\System32\tar.exe" -xf $zip -C $Dest | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "tar could not unpack $zip" }
}
$major = (Select-String -Path $header -Pattern 'define LIBAVCODEC_VERSION_MAJOR\s+(\d+)').Matches[0].Groups[1].Value
Write-Host "FFmpeg: B6.10, $name, libavcodec major $major"
if ($major -ne '63') { throw "libavcodec major is $major; ffmpeg-next 9 needs 63" }

Write-Output (Resolve-Path $dir).Path
