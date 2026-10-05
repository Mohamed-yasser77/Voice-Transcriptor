@echo off
:: Registers and launches Voice Transcriptor with Administrator privileges via Task Scheduler
echo Elevating to Administrator to register and start Voice Transcriptor...
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell.exe -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File \"%~dp0register-service.ps1\" -StartNow -NonInteractive' -Verb RunAs"
