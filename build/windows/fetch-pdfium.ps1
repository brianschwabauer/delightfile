<#
.SYNOPSIS
pdfium for the Windows zip: the build that pdfium.lock pins, downloaded,
checked against its sha256 and unpacked
(plans/other-platforms/06-build-and-release.md B6.12).

.DESCRIPTION
pdfium.lock is two lines, the tarball's URL and its sha256. The release is
bblanchon/pdfium-binaries' chromium/7881, the ABI df-app's `pdfium_7881`
feature binds to; nothing links it at build time, the app opens pdfium.dll
at run time from beside the executable. The tarball is kept in -Dest, so a
second run (or a CI cache of that folder) downloads nothing, and its hash is
checked on every run, cached or not.

The path of pdfium.dll is the one thing written to the output, for a caller
to take:

    $pdfium = & build\windows\fetch-pdfium.ps1

.PARAMETER Dest
Where the tarball goes and what it unpacks into. Defaults to
target\pdfium\win-x64 in the repository.
#>
param(
    [string]$Dest = (Join-Path $PSScriptRoot '..\..\target\pdfium\win-x64')
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$lock = Get-Content (Join-Path $PSScriptRoot 'pdfium.lock')
$url = $lock[0].Trim()
$sha256 = $lock[1].Trim()
$name = ($url -split '/')[-1]

New-Item -ItemType Directory -Force $Dest | Out-Null
$tgz = Join-Path $Dest $name
if (-not (Test-Path $tgz)) {
    Write-Host "downloading $url"
    Invoke-WebRequest $url -OutFile $tgz
}
$actual = (Get-FileHash $tgz -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $sha256) {
    Remove-Item $tgz
    throw "$name has sha256 $actual; pdfium.lock says $sha256"
}
Write-Host "pdfium: $(($url -split '/download/')[-1]), sha256 $sha256, verified"

foreach ($old in 'bin', 'licenses') {
    $path = Join-Path $Dest $old
    if (Test-Path $path) { Remove-Item -Recurse -Force $path }
}
# Windows' own bsdtar, named by path because Git for Windows puts a GNU tar
# on PATH that reads C: as a host name.
& "$env:SystemRoot\System32\tar.exe" -xzf $tgz -C $Dest bin/pdfium.dll LICENSE licenses | Out-Host
if ($LASTEXITCODE -ne 0) { throw "tar could not unpack $tgz" }

Write-Output (Resolve-Path (Join-Path $Dest 'bin\pdfium.dll')).Path
