# register-startup.ps1
# Registers Voice Transcriptor in HKCU Run registry so it launches automatically on user login.

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectDir = (Resolve-Path "$scriptDir\..").Path
$exeTarget = "$projectDir\src-tauri\target\release\voice-dictation.exe"

if (-not (Test-Path $exeTarget)) {
    Write-Host "Executable not found at $exeTarget" -ForegroundColor Red
    exit 1
}

$exePath = (Resolve-Path $exeTarget).Path
$regValue = "`"$exePath`""

Set-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "VoiceTranscriptor" -Value $regValue -Force

Write-Host "Successfully registered in HKCU Run:" -ForegroundColor Green
Get-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "VoiceTranscriptor"
