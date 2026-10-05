@echo off
:: Runs unregister-service.ps1 with Administrator privileges to remove Voice Transcriptor from Task Scheduler
echo Elevating to Administrator...
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell.exe -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File \"%~dp0unregister-service.ps1\"' -Verb RunAs"
