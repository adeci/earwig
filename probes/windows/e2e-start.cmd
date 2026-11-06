@echo off
rem Start the scratch app and the earwig daemon (through earwigw) in this session.
set P=C:\earwig-build\target\windows-probe\release
set E=C:\earwig-build\target\windows\release
set EARWIG_AUDIO_FILE=C:\earwig-build\src\windows\tests\fixtures\dictation-sentences.wav
set RUST_LOG=info,earwig_core=debug,earwig_transcribe=debug
del "%LOCALAPPDATA%\earwig\earwig.log" 2>nul
start "" %P%\scratch.exe C:\earwig-build\scratch.txt
start "" %E%\earwigw.exe
ping -n 4 127.0.0.1 >nul
%P%\keys.exe front "earwig scratch" -- fg
