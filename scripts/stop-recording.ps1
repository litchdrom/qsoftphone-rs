# Stop the screen capture.
#
# The app runs this when the agent stops being ready, and when the app
# closes while they are still ready.
#
# The capture is found through recording.pid: the PID file says which
# process is the capture, so this stops *that* recorder and not some
# other ffmpeg that happens to be running.
#
# This file is ASCII on purpose. Windows PowerShell 5.1 reads a .ps1
# that has no byte-order mark as the system's own code page, and
# anything above ASCII comes out as gibberish.

[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

# Where the state of the capture lives: next to this script.
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$pidFile = Join-Path $here 'recording.pid'

if (-not (Test-Path $pidFile)) {
    Write-Output "No capture is running."
    exit 0
}

$text = [System.IO.File]::ReadAllText($pidFile).Trim()
$capturePid = 0
if (-not [int]::TryParse($text, [ref]$capturePid) -or $capturePid -le 0) {
    # The file is not a PID: it is nothing to stop, and it is in the
    # way of the next start.
    [System.IO.File]::Delete($pidFile)
    Write-Output "The PID file is not a PID; it is cleared."
    exit 0
}

$process = Get-Process -Id $capturePid -ErrorAction SilentlyContinue
if ($null -eq $process) {
    # The capture is gone and the file was left behind: a stale file
    # must not make the app think that something is running.
    [System.IO.File]::Delete($pidFile)
    Write-Output "The capture is not running; the stale PID file is cleared."
    exit 0
}

if ($process.ProcessName -ne 'ffmpeg') {
    # The PID was taken by another program after the capture died: it
    # is not ours to kill.
    [System.IO.File]::Delete($pidFile)
    Write-Output ("The pid {0} is not ffmpeg; it is left alone." -f $capturePid)
    exit 0
}

# The capture is killed rather than asked to stop: ffmpeg is in the
# middle of writing a file, and the file is fragmented so that it can
# be read anyway.
Stop-Process -Id $capturePid -Force -ErrorAction SilentlyContinue
[System.IO.File]::Delete($pidFile)

Write-Output ("The capture is stopped (pid {0})." -f $capturePid)
exit 0
