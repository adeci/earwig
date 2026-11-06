@echo off
set P=C:\earwig-build\target\windows-probe\release
del C:\earwig-build\term.txt 2>nul
start "" wt.exe -w new --title "earwig term" --suppressApplicationTitle cmd /c C:\earwig-build\src\windows\probes\windows\term-read.cmd
ping -n 4 127.0.0.1 >nul
%P%\keys.exe front "earwig term" -- fg
%P%\paste.exe "hello terminal from earwig"
%P%\keys.exe tap enter
ping -n 2 127.0.0.1 >nul
echo --- wt:
type C:\earwig-build\term.txt
del C:\earwig-build\term.txt 2>nul
start "" conhost.exe cmd /c C:\earwig-build\src\windows\probes\windows\term-read.cmd
ping -n 3 127.0.0.1 >nul
%P%\keys.exe front "earwig term" -- fg
%P%\paste.exe "hello conhost from earwig"
%P%\keys.exe tap enter
ping -n 2 127.0.0.1 >nul
echo --- conhost:
type C:\earwig-build\term.txt
