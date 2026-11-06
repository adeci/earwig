@echo off
set P=C:\earwig-build\target\windows-probe\release
start "" %P%\scratch.exe C:\earwig-build\scratch.txt
ping -n 3 127.0.0.1 >nul
start /b "" %P%\hook.exe 6 > C:\earwig-build\hook.log
ping -n 2 127.0.0.1 >nul
%P%\keys.exe front "earwig scratch" -- fg -- hold rctrl 500 -- tap rctrl -- combo rctrl c
%P%\paste.exe "hello from earwig"
%P%\keys.exe fg
ping -n 6 127.0.0.1 >nul
type C:\earwig-build\hook.log
echo ---
type C:\earwig-build\scratch.txt
echo.
taskkill /im scratch.exe >nul
