# register-service.ps1
# Registers Voice Transcriptor as an always-running background task in Windows Task Scheduler.
# Runs at user logon with Highest Privileges (to enable injection into elevated Admin windows).

param (
    [switch]$StartNow,
    [switch]$NonInteractive
)

$ErrorActionPreference = "Stop"

# Check for Administrator elevation. If not elevated, relaunch with UAC prompt.
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host "[UAC] Requesting Administrator elevation to register in Task Scheduler..." -ForegroundColor Yellow
    $argList = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    if ($StartNow) { $argList += " -StartNow" }
    if ($NonInteractive) { $argList += " -NonInteractive" }
    
    Start-Process powershell.exe -ArgumentList $argList -Verb RunAs
    exit 0
}

$taskName = "VoiceTranscriptor"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectDir = (Resolve-Path "$scriptDir\..").Path
$exeTarget = "$projectDir\src-tauri\target\release\voice-dictation.exe"

if (-not (Test-Path $exeTarget)) {
    Write-Host "Error: Executable not found at $exeTarget" -ForegroundColor Red
    Write-Host "Please compile the release build first using: cargo build --release" -ForegroundColor Yellow
    Pause
    exit 1
}

$exePath = (Resolve-Path $exeTarget).Path
Write-Host "Configuring Windows Task Scheduler for: $exePath" -ForegroundColor Cyan

# Action: Launch the release binary with project root as working directory
$action = New-ScheduledTaskAction -Execute $exePath -WorkingDirectory $projectDir

# Trigger: Trigger automatically when user logs on
$trigger = New-ScheduledTaskTrigger -AtLogOn

$currentUser = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
Write-Host "Registering for user identity: $currentUser" -ForegroundColor Cyan

# Principal: Run in user desktop session with Highest Privileges (elevated for UIPI / admin app typing)
$principal = New-ScheduledTaskPrincipal -UserId $currentUser -LogonType Interactive -RunLevel Highest

# Settings:
# - ExecutionTimeLimit = 0 (infinite, does not terminate after 72 hours)
# - Run on battery and don't stop if AC unplugged
# - Auto-restart up to 3 times if terminated unexpectedly
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -RestartCount 3 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -MultipleInstances IgnoreNew

$task = New-ScheduledTask -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description "Voice Dictation low-latency background tray engine with Push-to-Talk hotkey (Alt+Shift+V)"

try {
    Register-ScheduledTask -TaskName $taskName -InputObject $task -Force | Out-Null
} catch {
    Write-Host "[ERROR] Failed to register task in Task Scheduler: $_" -ForegroundColor Red
    Write-Host "Press any key to close..."
    [Console]::ReadKey()
    exit 1
}

Write-Host "==========================================================" -ForegroundColor Green
Write-Host "[SUCCESS] Task '$taskName' registered successfully!" -ForegroundColor Green
Write-Host "  - Trigger: At Windows Logon ($currentUser)" -ForegroundColor Gray
Write-Host "  - Privileges: Highest (Admin-level text injection enabled)" -ForegroundColor Gray
Write-Host "  - Execution Limit: None (24/7 always-running background task)" -ForegroundColor Gray
Write-Host "  - Binary: $exePath" -ForegroundColor Gray
Write-Host "==========================================================" -ForegroundColor Green

if ($StartNow) {
    Write-Host "Starting task '$taskName' now..." -ForegroundColor Cyan
    Start-ScheduledTask -TaskName $taskName
    Start-Sleep -Seconds 2
    $state = (Get-ScheduledTask -TaskName $taskName).State
    Write-Host "Task status: $state" -ForegroundColor Green
} elseif (-not $NonInteractive) {
    $response = Read-Host "Would you like to start the task right now? (Y/n)"
    if ($response -ne 'n' -and $response -ne 'N') {
        Start-ScheduledTask -TaskName $taskName
        Start-Sleep -Seconds 2
        $state = (Get-ScheduledTask -TaskName $taskName).State
        Write-Host "Task status: $state" -ForegroundColor Green
    }
}
