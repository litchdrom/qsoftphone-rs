# A dummy status script, for checking that the softphone runs them.
#
# The app runs a status's start script and stop script for real, and
# the answer is on the status line: "the command ran", or the exit
# code it failed with. The output of the script itself is not shown -
# the app throws it away - so a script that wants to be checked has to
# leave something behind.
#
# This one writes a line into script-runs.log next to this script:
#
#   2026-11-19 00:31:07  start  pid=12345  exit=0
#
# It does nothing else: no capture, no ffmpeg, no sound card, and it
# finishes at once unless it is told to take its time.
#
# This file is ASCII on purpose. Windows PowerShell 5.1 reads a .ps1
# that has no byte-order mark as the system's own code page, and
# anything above ASCII comes out as gibberish.

[CmdletBinding()]
param(
    # What the line in the log says. "start" and "stop" are the two
    # that the softphone uses, but anything is allowed.
    [string] $Label = "run",

    # The code to exit with. 0 is success; anything else is a failure,
    # and the status line says so.
    [int] $ExitCode = 0,

    # How long the script takes, in seconds.
    #
    # The app does not wait for a script that takes its time - the
    # window carries on - so this is how a capture that lasts for
    # hours is imitated. The closing of the window is the one place
    # where the app waits, and it waits for two seconds.
    [int] $Seconds = 0
)

$ErrorActionPreference = 'Stop'

# Where the log lives: next to this script, so that it is found
# without being told where it is.
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$log = Join-Path $here 'script-runs.log'

$line = '{0}  {1}  pid={2}  exit={3}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $Label, $PID, $ExitCode
[System.IO.File]::AppendAllText($log, $line + [Environment]::NewLine)

if ($Seconds -gt 0) {
    Start-Sleep -Seconds $Seconds
}

Write-Output ("The dummy script ran: {0}" -f $Label)
exit $ExitCode
