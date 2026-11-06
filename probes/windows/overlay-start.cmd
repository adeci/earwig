@echo off
set P=C:\earwig-build\target\windows-probe\release
del C:\earwig-build\overlay-probe.log 2>nul
start "" %P%\scratch.exe C:\earwig-build\scratch.txt
ping -n 3 127.0.0.1 >nul
%P%\keys.exe front "earwig scratch" -- fg
start "" %P%\overlay.exe 90
ping -n 4 127.0.0.1 >nul
%P%\keys.exe fg -- tap a -- tap b -- tap c -- fg
type C:\earwig-build\overlay-probe.log
