# unregister-service.ps1
# Stops and removes Voice Transcriptor from Windows Task Scheduler.

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host "[UAC] Requesting Administrator elevation to unregister task..." -ForegroundColor Yellow
    Start-Process powershell.exe -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`"" -Verb RunAs
    exit 0
}

$taskName = "VoiceTranscriptor"

Write-Host "Stopping and removing Scheduled Task '$taskName'..." -ForegroundColor Yellow

# Stop task if running
Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue

# Also stop any running voice-dictation process
Get-Process -Name "voice-dictation" -ErrorAction SilentlyContinue | Stop-Process -Force

# Unregister scheduled task
Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue

Write-Host "Task '$taskName' has been removed." -ForegroundColor Green
