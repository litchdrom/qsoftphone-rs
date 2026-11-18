# The screen capture scripts

Three scripts for a screen capture that the softphone starts and stops
by itself: the capture starts when the agent becomes **Ready** and
stops when they stop being Ready.

| Script | What it does |
|---|---|
| `start-recording.ps1` | starts the capture, unless it is already running |
| `stop-recording.ps1` | stops the capture, if it is running |
| `recording-status.ps1` | says whether the capture is running (exit code 0 = running, 1 = not) |

They need [ffmpeg](https://ffmpeg.org/) with `gdigrab` and `libx264`
— the Windows build from [gyan.dev](https://gyan.dev/ffmpeg/builds/)
has both. The recorder is found on the `PATH`, or named in the
`QSOFTPHONE_FFMPEG` environment variable, or passed with `-Ffmpeg`.

There is a fourth script, `dummy-run.ps1`, which starts nothing and
records that it ran — it is for checking that the wiring works, and it
is described [below](#checking-that-the-scripts-run).

## How to wire them to a status

In the softphone's **Settings → STATUSES**, find the status **Ready**
and give it three things:

| Field | Value |
|---|---|
| **When set** | the action of the status — the dial that tells the PBX that the agent is ready, or the query that tells the CRM. It runs at the same time as the capture. |
| **Start script** | `powershell -NoProfile -ExecutionPolicy Bypass -File "<repo>\scripts\start-recording.ps1"` |
| **Stop script** | `powershell -NoProfile -ExecutionPolicy Bypass -File "<repo>\scripts\stop-recording.ps1"` |

Setting the status then does both things at once: the softphone tells
the PBX that the agent is ready, and the capture starts. Setting
**Pause** runs Pause's stop script — and the capture that was started
by Ready stops, because Ready was left.

Any status can carry the pair: the start and the stop are attached to
a status of the user's own as well as to the three that come with the
app.

The **trigger** says when the capture starts:

| Trigger | The capture starts |
|---|---|
| **Every time it is set** (the default) | each time the status is picked — *including* the one the account is already in |
| **Only when it becomes active** | on a change into the status, and when the app is opened with the account already in it |

The start script is **idempotent**: it says "the capture is already
running" and does nothing, so pressing the same status twice does not
start two captures.

The app waits for the stop script when it closes: a capture that is
left running with nobody watching it is a capture nobody wanted. The
wait is short — two seconds — so a script that hangs cannot keep the
window from closing.

> The statuses are **global** — they are a state of the user, not of
> one account. With two accounts on the same list, a status that is
> changed in one of them is changed for both, and the leave hook runs
> for the one that left it.

## Checking that the scripts run

The app runs a script for real, and the answer is on the **status
line** at the bottom of the window. The line is prefixed with the
**label of the account** the status was set on:

| What the status line says | What it means |
|---|---|
| `104: the command ran.` | the script ran and exited with 0 |
| `104: the command failed: it exited with 3.` | the script ran and exited with 3 |
| `104: the command could not be started: …` | the shell could not start it at all |

The **output of the script is thrown away** — the app does not show it
— so a script that wants to be checked has to leave something behind.
`dummy-run.ps1` is that script: it writes a line into
`script-runs.log` next to it, and does nothing else.

### The dummy script

| | |
|---|---|
| `-Label` | what the line says — `start`, `stop`, anything |
| `-ExitCode` | the code to exit with (0 = success) |
| `-Seconds` | how long it takes, to imitate a capture that lasts for hours |

### The steps

1. Put the dummy in a status of its own, so the real capture is not
   touched while it is being checked. In **Settings → STATUSES**, add a
   status (or use **Pause**), and give it:

   | Field | Value |
   |---|---|
   | **Start script** | `powershell -NoProfile -ExecutionPolicy Bypass -File "<repo>\scripts\dummy-run.ps1" -Label start` |
   | **Stop script** | `powershell -NoProfile -ExecutionPolicy Bypass -File "<repo>\scripts\dummy-run.ps1" -Label stop` |

2. Save. The status is global, so it is on every account.

3. Put the account into that status, from the **status button** in the
   accounts list. The status line says that the command ran.

4. Look at the log:

   ```powershell
   Get-Content "<repo>\scripts\script-runs.log" -Tail 10
   ```

   ```text
   2026-11-19 00:31:07  start  pid=17736  exit=0
   2026-11-19 00:31:09  stop   pid=22352  exit=0
   ```

5. Put the account into **another** status: the `stop` line appears —
   the stop script of the status that was left runs then, and not
   before.

### What each thing proves

| What you see | What it proves |
|---|---|
| a `start` line, and the status line says *the command ran* | the status's start script runs when the status is set |
| a `stop` line when the account leaves the status | the stop script runs when the status is **left** |
| a `stop` line when the app is closed | the app kills the capture on its way out |
| a second `start` line for the same status | the trigger is **every time it is set**; with **only when it becomes active** there is no second line |
| `exit=3` on the line, and *the command failed: it exited with 3* | the exit code of the script reaches the app — a script that cannot do its job is not hidden |

The log is next to the scripts, and it is not in the repository: it is
the state of the last run, like `recording.pid`.

## Where the state of the capture lives

Next to the scripts, so that the scripts find it without being told
where it is:

| File | What it is |
|---|---|
| `recording.pid` | the PID of the recorder that is running |
| `recordings/screen-<date>-<time>.mp4` | the captures themselves |

The recordings are written as **fragmented MP4**
(`-movflags +frag_keyframe+empty_moov -frag_duration 2000000
-flush_packets 1`): the capture is killed rather than asked to stop,
and a file that is killed is only playable if it was written in
pieces.

* `+frag_keyframe+empty_moov` — the file is written in pieces, and the
  description of the track is at the front of it.
* `-frag_duration 2000000` — a piece is closed every two seconds.
  Without it a piece is closed at the next keyframe, and the keyframe
  of x264 is 250 frames away: a capture that is stopped in the first
  half-minute has nothing in it at all.
* `-flush_packets 1` — the pieces are on the disk as they are written
  and not in the buffer of the muxer, which a killed recorder takes
  with it.

## The ways to say "it is running"

The user asked for a simple marker — a file in the directory — and
there are five ways to do it. This is what each one is, and what it
costs.

### A — a marker file (`recording.lock`)

The file's existence **is** the state: it is there while the capture
runs, and it is deleted when the capture stops.

* **For:** the simplest thing there is, and any language can look at
  it.
* **Against:** a file left behind by a crash says the capture is
  running when it is not. Nothing notices but the user.

### B — a PID file (`recording.pid`) — **the one these scripts use**

The file holds the PID of the recorder. "Running" means that PID is
alive and is still ffmpeg.

* **For:** it says **which** process is the capture, so the stop
  script stops *that* recorder and not some other ffmpeg that happens
  to be running. A stale file is noticed — the PID is looked up, and
  a file whose PID is dead is cleared.
* **Against:** one more file, and the PID of a process that has died
  and been given to another one has to be recognised — which is why
  the name of the process is checked as well.

### C — the process table

`tasklist` to look, `taskkill /IM ffmpeg.exe` to stop. No files at
all.

* **For:** nothing to keep, and nothing to go stale.
* **Against:** it cannot tell *whose* ffmpeg it is, so it kills every
  ffmpeg on the machine — including a conversion the user started.

### D — the window title

`taskkill /FI "WINDOWTITLE eq screen-rec"`: the recorder is given a
window title of its own, and the window title is what it is found by.

* **For:** it targets one process, and it needs no file.
* **Against:** Windows only, and a window that is hidden has no title
  to match — which is exactly how a recorder should run.

### E — a lock directory

`mkdir` is atomic: the first one to make the directory owns the
capture, and the others are told that it is running.

* **For:** the classic lock, and it cannot be half-made.
* **Against:** the same staleness as the marker file, and a directory
  has to be cleared by hand after a crash.

The PID file is the one that is worth the extra file: it is the only
one of the five that both says *which* process is the capture and
notices when that process is gone.
