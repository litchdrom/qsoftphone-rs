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

## How to wire them to a status

In the softphone's **Settings → STATUSES**, find the status **Ready**
and give it two hooks:

| Field | Value |
|---|---|
| **When set** → Run a script | `powershell -NoProfile -ExecutionPolicy Bypass -File "<repo>\scripts\start-recording.ps1"` |
| **When left** → Run a script | `powershell -NoProfile -ExecutionPolicy Bypass -File "<repo>\scripts\stop-recording.ps1"` |
| **Trigger** | *Only when it becomes active* |

The **trigger** is what makes it a capture and not a button: with
*Every time it is set* the capture starts each time the status is
picked — including the one the account is already in. With *Only when
it becomes active* it starts when the state changes into Ready, and
when the app is opened with the account already in Ready.

The app waits for the stop script when it closes: a capture that is
left running with nobody watching it is a capture nobody wanted.

> The statuses are **global** — they are a state of the user, not of
> one account. With two accounts on the same list, a status that is
> changed in one of them is changed for both, and the leave hook runs
> for the one that left it.

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
