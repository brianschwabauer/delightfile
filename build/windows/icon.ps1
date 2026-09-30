<#
.SYNOPSIS
build\windows\delightfile.ico, drawn from build\delightfile.svg with
ImageMagick (plans/other-platforms/06-build-and-release.md B6.20).

.DESCRIPTION
The icon crates\df-app\build.rs puts in delightfile.exe, beside the version
resource, when this file exists at build time; run this before
`cargo build --release`. It is generated, not committed, and git ignores it.

The SVG is drawn once at 768 px (48 units at 1536 dpi) and scaled down to
each size Explorer and the taskbar ask for, 16 to 256, which keeps the small
sizes from being drawn on a 48-unit grid at a fraction of a unit. The
windows-latest runner has ImageMagick; on another machine,
`winget install ImageMagick.ImageMagick` or `scoop install imagemagick`
provides `magick`.

.PARAMETER Out
Where the icon goes. Defaults to delightfile.ico beside this script, which is
where build.rs looks.
#>
param(
    [string]$Out = (Join-Path $PSScriptRoot 'delightfile.ico')
)

$ErrorActionPreference = 'Stop'

$svg = Join-Path $PSScriptRoot '..\delightfile.svg'
if (-not (Get-Command magick -ErrorAction SilentlyContinue)) {
    throw "ImageMagick's magick is not on PATH; install ImageMagick, or build without an icon"
}
& magick -background none -density 1536 $svg -define icon:auto-resize=256,128,64,48,40,32,24,20,16 $Out
if ($LASTEXITCODE -ne 0) { throw "magick could not draw $svg" }
& magick identify $Out | Out-Host
Write-Output (Resolve-Path $Out).Path
