<#
.SYNOPSIS
B6.19's acceptance check for the Windows zip: unpacked into an empty folder,
delightfile.exe --version runs from there with every FFmpeg folder taken off
PATH, so only the zip's own DLLs can answer; and the exe carries the icon and
the version resource of B6.20, the version being the one --version says.

.DESCRIPTION
    & build\windows\check-package.ps1 target\dist\delightfile-<version>-x86_64-windows.zip

package.ps1 runs it on what it made; the release workflow runs it again on a
fresh runner that never had FFmpeg.
#>
param(
    [Parameter(Mandatory)]
    [string]$Zip
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$zip_path = (Resolve-Path $Zip).Path
$name = [System.IO.Path]::GetFileNameWithoutExtension($zip_path)
if ($name -notmatch '^delightfile-(.+)-x86_64-windows$') { throw "$zip_path is not named delightfile-<version>-x86_64-windows.zip" }
$version = $Matches[1]

$trial = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
$saved_path = $env:PATH
try {
    [System.IO.Compression.ZipFile]::ExtractToDirectory($zip_path, $trial)
    $exe = Join-Path $trial "$name\delightfile.exe"
    if (-not (Test-Path $exe)) { throw "the zip has no $name\delightfile.exe" }
    Get-ChildItem (Join-Path $trial $name) -Recurse -File |
        ForEach-Object { Write-Host ("  {0,12:N0}  {1}" -f $_.Length, $_.FullName.Substring($trial.Length + 1)) }

    $env:PATH = ($env:PATH -split ';' | Where-Object { $_ -and -not (Test-Path (Join-Path $_ 'avcodec-63.dll')) }) -join ';'
    # The program is a windows-subsystem executable that keeps the handle it
    # is given for standard output (04-windows.md W4.1), so a file catches
    # what it prints and -Wait sees it finish.
    $version_file = Join-Path $trial 'version.txt'
    $run = Start-Process -FilePath $exe -ArgumentList '--version' -NoNewWindow -Wait -PassThru -RedirectStandardOutput $version_file
    $said = "$(Get-Content $version_file -Raw)".Trim()
    Write-Host "--version: $said (exit $($run.ExitCode))"
    if ($run.ExitCode -ne 0 -or $said -ne "delightfile $version") {
        throw "the unpacked delightfile.exe --version said '$said' (exit $($run.ExitCode)), not 'delightfile $version'"
    }

    $info = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($exe)
    Write-Host "version resource: FileVersion $($info.FileVersion), ProductVersion $($info.ProductVersion), FileDescription $($info.FileDescription)"
    if ($info.ProductVersion -ne $version -or $info.FileVersion -ne $version) {
        throw "the version resource says $($info.FileVersion)/$($info.ProductVersion), not $version (crates\df-app\build.rs)"
    }
    Add-Type -Namespace DelightfileCheck -Name Shell -MemberDefinition @'
[DllImport("shell32.dll", CharSet = CharSet.Unicode)]
public static extern uint ExtractIconExW(string file, int index, IntPtr[] large, IntPtr[] small, uint count);
'@
    $icons = [DelightfileCheck.Shell]::ExtractIconExW($exe, -1, $null, $null, 0)
    Write-Host "icons in delightfile.exe: $icons"
    if ($icons -lt 1) { throw "delightfile.exe has no icon; run build\windows\icon.ps1 before cargo build --release" }
} finally {
    $env:PATH = $saved_path
    Remove-Item -Recurse -Force $trial -ErrorAction SilentlyContinue
}
Write-Host "check-package: $name.zip unpacks, runs with no FFmpeg on PATH ($said), and carries its icon and version"
