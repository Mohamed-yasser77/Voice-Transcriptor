@echo off
:: Launches Voice Transcriptor into the user's active desktop session
cd /d "%~dp0.."
start "" "%~dp0..\src-tauri\target\release\voice-dictation.exe"
