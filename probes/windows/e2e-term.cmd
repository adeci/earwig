@echo off
rem A Windows Terminal tab running term-read.cmd, in front.
set P=C:\earwig-build\target\windows-probe\release
del C:\earwig-build\term.txt 2>nul
start "" wt.exe -w new --title "earwig term" --suppressApplicationTitle cmd /c C:\earwig-build\src\windows\probes\windows\term-read.cmd
ping -n 4 127.0.0.1 >nul
%P%\keys.exe front "earwig term" -- fg
