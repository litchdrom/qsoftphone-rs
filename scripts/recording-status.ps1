# Say whether the capture is running.
#
# The state of the capture lives in recording.pid, next to this script.
# The PID in it is looked up: a PID file that is left behind by a crash
# says nothing about what is running now.
#
# The exit code says it too: 0 when the capture is running, 1 when it
# is not.
#
# This file is ASCII on purpose. Windows PowerShell 5.1 reads a .ps1
# that has no byte-order mark as the system's own code page, and
# anything above ASCII comes out as gibberish.

[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$pidFile = Join-Path $here 'recording.pid'

if (-not (Test-Path $pidFile)) {
    Write-Output "The capture is not running."
    exit 1
}

$text = [System.IO.File]::ReadAllText($pidFile).Trim()
$capturePid = 0
if (-not [int]::TryParse($text, [ref]$capturePid) -or $capturePid -le 0) {
    Write-Output "The capture is not running; the PID file is not a PID."
    exit 1
}

$process = Get-Process -Id $capturePid -ErrorAction SilentlyContinue
if ($null -eq $process -or $process.ProcessName -ne 'ffmpeg') {
    Write-Output "The capture is not running; the PID file is stale."
    exit 1
}

Write-Output ("The capture is running (pid {0})." -f $capturePid)
exit 0
