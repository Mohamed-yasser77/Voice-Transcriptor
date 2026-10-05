# service-status.ps1
# Checks the background service status, process health, and socket readiness.

$taskName = "VoiceTranscriptor"

Write-Host "=================== Voice Dictation Status ===================" -ForegroundColor Cyan

# 1. Scheduled Task Status
try {
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction Stop
    $taskInfo = Get-ScheduledTaskInfo -TaskName $taskName -ErrorAction SilentlyContinue
    Write-Host "[Task Scheduler]" -ForegroundColor Yellow
    Write-Host "  Registered : Yes"
    Write-Host "  State      : $($task.State)"
    if ($taskInfo) {
        Write-Host "  Last Run   : $($taskInfo.LastRunTime)"
        Write-Host "  Last Result: $($taskInfo.LastTaskResult)"
    }
} catch {
    Write-Host "[Task Scheduler] Not registered" -ForegroundColor Gray
}

# 2. Process Status
$dictationProc = Get-Process -Name "voice-dictation" -ErrorAction SilentlyContinue
if ($dictationProc) {
    Write-Host "[App Process]" -ForegroundColor Green
    Write-Host "  Running    : Yes (PID: $($dictationProc.Id))"
    Write-Host "  Memory     : $([math]::Round($dictationProc.WorkingSet64 / 1MB, 1)) MB"
} else {
    Write-Host "[App Process] Not currently running" -ForegroundColor Red
}

# 3. Whisper Server Socket Check (TCP 9877)
$tcpConnection = Test-NetConnection -ComputerName 127.0.0.1 -Port 9877 -InformationLevel Quiet -WarningAction SilentlyContinue
if ($tcpConnection) {
    Write-Host "[Whisper Engine] Listening on 127.0.0.1:9877 (Ready)" -ForegroundColor Green
} else {
    Write-Host "[Whisper Engine] 127.0.0.1:9877 not reachable (Idle or starting)" -ForegroundColor Gray
}

Write-Host "==============================================================" -ForegroundColor Cyan
