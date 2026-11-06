@echo off
rem The daemon on the real microphone; levels of one 4 s dictation.
set P=C:\earwig-build\target\windows-probe\release
set E=C:\earwig-build\target\windows\release
start "" %E%\earwigw.exe
ping -n 15 127.0.0.1 >nul
start /b "" %E%\earwig.exe events > C:\earwig-build\events.jsonl
%P%\keys.exe front "earwig scratch" -- hold rctrl 4000 -- sleep 2000
findstr /c:"level" C:\earwig-build\events.jsonl | find /c "level"
powershell -c "Get-Content C:\earwig-build\events.jsonl | Select-String level | Select -Last 3"
