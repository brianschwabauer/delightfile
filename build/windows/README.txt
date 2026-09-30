delightfile @VERSION@ for Windows (x86-64)
A keyboard-first file manager. https://github.com/brianschwabauer/delightfile

Running it
----------
Unzip this folder anywhere and run delightfile.exe. Everything it needs is
in the folder: FFmpeg's DLLs for video, sound and pictures, and pdfium.dll
for PDF pages. Nothing is installed. Settings live in %APPDATA%\delightfile
and the rest in %LOCALAPPDATA%\delightfile; deleting this folder and those
two removes it.

The first time
--------------
delightfile is not signed, so the first time it runs Windows SmartScreen may
say "Windows protected your PC". Choose "More info", then "Run anyway".

To skip that, unblock the zip before unzipping it: right-click it,
Properties, tick "Unblock", OK. Or in PowerShell:

    Unblock-File .\delightfile-@VERSION@-x86_64-windows.zip

Installed with scoop instead, it is not checked by SmartScreen:

    scoop bucket add brianschwabauer https://github.com/brianschwabauer/scoop-bucket
    scoop install brianschwabauer/delightfile

Licenses
--------
delightfile is GPL-3.0-or-later. The FFmpeg DLLs are an FFmpeg GPL build, and
pdfium.dll carries its own license and its libraries'. Licenses\ holds them
all, and Licenses\SOURCES.txt says exactly which FFmpeg this is and where its
source is.
