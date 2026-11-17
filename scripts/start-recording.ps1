# Start the screen capture.
#
# The app runs this when the agent becomes ready, and runs
# stop-recording.ps1 when they stop being ready.
#
# The state of the capture lives next to this script: recording.pid
# holds the PID of the recorder, and the recordings are written into
# the recordings folder beside it.
#
# This file is ASCII on purpose. Windows PowerShell 5.1 reads a .ps1
# that has no byte-order mark as the system's own code page, and
# anything above ASCII comes out as gibberish.

[CmdletBinding()]
param(
    # The file to write. Empty = a name of its own under recordings.
    [string] $Output = "",

    # The frames per second.
    [int] $Fps = 15,

    # The ffmpeg to use. Empty = the one on PATH, or
    # QSOFTPHONE_FFMPEG from the environment.
    [string] $Ffmpeg = ""
)

$ErrorActionPreference = 'Stop'

# Where the state of the capture lives: next to this script.
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$pidFile = Join-Path $here 'recording.pid'
$folder = Join-Path $here 'recordings'

function Get-CaptureProcess {
    # The PID of the capture, if it is still running.
    #
    # A PID file that is left behind by a crash says nothing about
    # what is running now, so the PID is looked up and the process it
    # names is checked: only ffmpeg is ours to call a capture.
    if (-not (Test-Path $pidFile)) {
        return $null
    }
    $text = [System.IO.File]::ReadAllText($pidFile).Trim()
    $capturePid = 0
    if (-not [int]::TryParse($text, [ref]$capturePid) -or $capturePid -le 0) {
        return $null
    }
    $process = Get-Process -Id $capturePid -ErrorAction SilentlyContinue
    if ($null -eq $process) {
        return $null
    }
    if ($process.ProcessName -ne 'ffmpeg') {
        return $null
    }
    return $process
}

$running = Get-CaptureProcess
if ($null -ne $running) {
    Write-Output ("The capture is already running (pid {0})." -f $running.Id)
    exit 0
}

# The ffmpeg to use.
if (-not $Ffmpeg) {
    $Ffmpeg = $env:QSOFTPHONE_FFMPEG
}
if (-not $Ffmpeg) {
    $command = Get-Command ffmpeg -ErrorAction SilentlyContinue
    if ($null -eq $command) {
        Write-Output "ffmpeg is not on the PATH, and no ffmpeg was named."
        exit 1
    }
    $Ffmpeg = $command.Source
}
if (-not (Test-Path $Ffmpeg)) {
    Write-Output ("The ffmpeg is not there: {0}" -f $Ffmpeg)
    exit 1
}

# The file to write.
if (-not $Output) {
    if (-not (Test-Path $folder)) {
        New-Item -ItemType Directory -Path $folder -Force | Out-Null
    }
    $name = 'screen-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.mp4'
    $Output = Join-Path $folder $name
}

# The command.
#
# The capture is written as a fragmented MP4: the file has to be
# playable when the recorder is killed, and an ordinary MP4 is not
# playable until it is finished. Three things make that true:
#
#   * `+frag_keyframe+empty_moov` - the file is written in pieces,
#     and the description of the track is at the front of it;
#   * `-frag_duration 2000000` - a piece is closed every two seconds.
#     Without it a piece is closed at the next keyframe, and the
#     keyframe of x264 is 250 frames away: a capture that is stopped
#     in the first half-minute has nothing in it at all;
#   * `-flush_packets 1` - the pieces are on the disk as they are
#     written, and not in the buffer of the muxer, which a killed
#     recorder takes with it.
$arguments = @(
    '-hide_banner',
    '-loglevel', 'error',
    '-f', 'gdigrab',
    '-framerate', $Fps,
    '-i', 'desktop',
    '-c:v', 'libx264',
    '-preset', 'ultrafast',
    '-pix_fmt', 'yuv420p',
    '-movflags', '+frag_keyframe+empty_moov',
    '-frag_duration', '2000000',
    '-flush_packets', '1',
    '-y',
    ('"{0}"' -f $Output)
)

# The recorder is detached: the app must not wait for a capture that
# lasts for hours, and the window of the capture must not come up on
# the user's desktop.
$process = Start-Process -FilePath $Ffmpeg -ArgumentList $arguments -WindowStyle Hidden -PassThru

# The PID file is the state of the capture: it says what is running,
# and it says it to the stop script.
[System.IO.File]::WriteAllText($pidFile, [string]$process.Id)

Write-Output ("The capture is running (pid {0}): {1}" -f $process.Id, $Output)
exit 0
