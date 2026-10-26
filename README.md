# qsoftphone-rs

A Rust rewrite of **qsoftphone**, the desktop SIP softphone.

The C++/Qt implementation lives next door in `../qsoftphone` and is the
behavioural reference for everything in this repository. This crate is
not a port of its UI — it is a port of its *semantics*: the SIP
identity resolution, the call event contract, the history rules and
the transfer construction all have to behave identically, including the
bugs that have already been fixed there.

**Status: M0 complete.** The workspace builds, the test suite passes,
the app starts and exits cleanly. The SIP stack is not wired up yet —
see [Milestones](#milestones).

---

## Quick start

```powershell
cargo build --workspace
cargo test  --workspace
cargo run   -p qsoftphone-app -- --smoke   # startup path only, then exit
cargo run   -p qsoftphone-app               # the GUI
```

`--smoke` runs the whole startup path (paths, settings, logging,
database) and exits 0 without opening a window. It is what CI and the
"does it start and exit cleanly" check use.

Set `QSOFTPHONE_HOME` to put every file under one directory — handy
for a portable install, and for testing without touching your real
profile:

```powershell
$env:QSOFTPHONE_HOME = "C:\tmp\qsoftphone"
```

---

## Layout

```text
qsoftphone-rs/
├── Cargo.toml                     workspace + the pinned dependency set
├── Cargo.lock                     committed; the exact resolved tree
└── crates/
    ├── qsoftphone-core/           no window, no sound card, no network
    │   ├── config.rs              the JSON settings file
    │   ├── error.rs               one error type for the crate
    │   ├── logging.rs             tracing -> rotating file + stderr
    │   ├── model/                 account, call event, history record
    │   ├── paths.rs               per-OS config / data / log dirs
    │   ├── sip/                   pure SIP functions (uri, identity, refer)
    │   └── storage/               SQLite: accounts, contacts, history
    └── qsoftphone-app/            the binary
        ├── main.rs                startup order, --smoke
        └── app.rs                 the egui shell
```

The split is deliberate. Everything the brief calls out as a bug is a
pure function over strings, which is why it can be unit-tested without
a UDP socket, a GPU or a sound card.

---

## The stack, and what it costs

Pinned in the workspace `Cargo.toml`, locked in `Cargo.lock`.

| Crate | Version | Role |
|---|---|---|
| `rsipstack` | 0.7.9 | SIP transport, transactions, dialogs |
| `rtc` | 0.21.1 | RTP / SRTP / ICE / DTLS media plane |
| `cpal` | 0.18.2 | microphone capture and speaker playback |
| `egui` / `eframe` | 0.37.0 | the UI (default wgpu backend) |
| `rusqlite` | 0.41 | storage, `bundled` — SQLite is compiled in |
| `tokio` | 1.54.1 | the async runtime the SIP stack runs on |
| `tracing` | 0.1 | logging |

### The dependency budget

The brief asks for a resolved tree under ~250 crates, assuming
rsipstack would be the bulk at 134.

**Measured, the real number is 303 crates** for the Windows target
(382 for Linux, which adds X11/Wayland/ALSA). The gap is not
bloat — it is two subtrees the brief's estimate did not account for:

| Subtree | Crates | Trimmable? |
|---|---|---|
| `rtc` | 141 | No — every sub-crate (dtls, ice, srtp, stun, turn, mdns, media, datachannel) is an unconditional dependency |
| `eframe` | 171 | No — `wgpu` alone is 69 and the brief asks for the default wgpu backend |
| `rsipstack` | 64 | Yes — see below |
| `cpal` | 17 | No |
| `tokio` | 18 | No |
| `tracing-subscriber` | 23 | No |
| `rusqlite` | 9 | No |

`rtc` and `wgpu` together are 210 crates of pinned, non-negotiable
stack. There is no trim that reaches 250 without substituting the
stack, which the brief forbids.

**What was trimmed.** `rsipstack` is built with `default-features = false`
and only `platform-tokio`, `std` and `rustls` enabled:

```toml
rsipstack = { version = "0.7", default-features = false, features = [
    "platform-tokio", "std", "rustls",
] }
```

Measured effect: **132 crates → 64, saving 68.** The brief's transport
list is udp/tcp/tls only, so the `websocket` feature
(`tokio-tungstenite`) and the `srv_lookup` feature
(`hickory-resolver` and its `idna` / `icu_*` tail) are dead weight.
Re-enable either in one place — the workspace `Cargo.toml` — and pay
the cost back.

### Deviations from the brief's pins

* `rusqlite = "0.41"` — the brief said "0.3x"; 0.41 is the current
  release.
* `rsipstack` default features off — as above, with the cost measured.

---

## What is ported, and from where

Every rule below has a unit test. The C++ file it came from is named
so the two can be diffed by hand.

| Rule | C++ reference | Rust |
|---|---|---|
| URI user-part extraction, angle-bracket stripping | `src/core/sip/SipUri.cpp` | `sip::uri::sip_uri_user_part` |
| Bare number → routable URI against the registrar | `src/core/call/CallController.cpp` (`normaliseSipTarget`) | `sip::uri::normalise_sip_target` |
| §5.1 dialled number: header → Request-URI → To-URI | `CallController.cpp`, `src/core/call/AccountProfile.h` | `sip::identity::resolve_dialed_number` |
| §5.2 caller display name: header → contacts → URI user-part | `CallController.cpp` | `sip::identity::resolve_caller_display_name` |
| §5.3 history stores the *remote* number, never the local AOR | `src/core/history/CallHistory.h` | `model::history::CallRecord` |
| §5.4 header capture allowlist | `src/core/call/AccountProfile.h` | `model::account::AccountProfile::filter_headers` |
| §5.5 180 Ringing stops local ringback | `src/core/sip/SipEndpoint.cpp` | M1 |
| §5.6 `Refer-To` / `Replaces` construction | `src/core/sip/SipEndpoint.cpp` | `sip::refer` |
| The call event contract | `src/core/call/CallEvent.h` | `model::call_event::CallEvent` |

Two deliberate differences from the C++:

* **The dialled-number header value is passed through the user-part
  extractor.** The C++ stores the raw header value; if the header
  carries `sip:7777@host` rather than `7777`, the C++ would show the
  whole URI. Extracting the user-part is a no-op for a bare number and
  correct for a URI.
* **Contact matching requires a 7-digit suffix.** The C++
  `ContactsService::bestMatchForPhone()` is still a TODO stub — it
  returns nothing — so there was no rule to copy. Seven digits is the
  shortest national significant number (ITU-T E.123); anything shorter
  matches far too many people.

### The event contract

`CallEvent` serialises to exactly the C++ `toJson()` shape:
snake_case names (`account_id`, `call_id`, `dialed_number`, …), the
same `type` strings (`call_incoming`, `call_outgoing_initiated`,
`call_ringing`, `dtmf`, …), and empty fields omitted rather than
serialised as blanks. The REST API, the CRM plugins and the webhook
dispatcher are all downstream of this stream and never talk to the SIP
layer directly.

---

## Where the files live

| | Windows | Linux | macOS |
|---|---|---|---|
| config | `%APPDATA%\qsoftphone\config.json` | `$XDG_CONFIG_HOME/qsoftphone/config.json` | `~/Library/Application Support/qsoftphone/config.json` |
| data | `%LOCALAPPDATA%\qsoftphone\data\` | `$XDG_DATA_HOME/qsoftphone/data/` | `~/Library/Application Support/qsoftphone/data/` |
| logs | `%LOCALAPPDATA%\qsoftphone\logs\` | `$XDG_CACHE_HOME/qsoftphone/logs/` | `~/Library/Logs/qsoftphone/` |

Logs rotate daily: `qsoftphone.log.2026-10-26`. `RUST_LOG` overrides
the level from the settings file.

SQLite databases live in the data directory: `accounts.sqlite`,
`history.sqlite`, `contacts.sqlite`.

---

## Policy decisions

Taken from the brief, recorded here so they are not re-litigated:

* **A rejected transfer is reported, never swallowed.** If the server
  rejects the REFER, the error is shown and the call is *not* left
  running as if the transfer had succeeded.
* **Session recording and the CRM webhook endpoint are opt-in per
  account.** Neither is enabled by default.
* **The REST API is bearer-token authenticated**, and the admin bind
  address defaults to localhost.

---

## Milestones

| | Scope | State |
|---|---|---|
| **M0** | Workspace, pinned dependencies, tracing, config load/save, account CRUD in SQLite, the SIP pure functions with their tests | **done** |
| M1 | rsipstack endpoint, registration, the inbound INVITE path, Request-URI capture, the 180 Ringing rule | next |
| M2 | rtc media, cpal capture/playback, the dialer, DTMF, hold/mute | |
| M3 | The REST API and the CRM plugin boundary | |
| M4 | The call window, the history list, contacts | |
| M5 | Recording, webhooks, the admin surface | |

One milestone at a time; M1 does not start before M0 works end to end.

---

## Verified, and not

**Verified in this repository:**

* `cargo build --workspace` succeeds (rustc 1.98.1, MSVC).
* `cargo test --workspace` — 65 tests, all passing.
* `cargo clippy --workspace --all-targets -- -D warnings` is clean.
* `--smoke` starts the app, creates the directories, the log file and
  the database, and exits 0.
* The GUI opens a window titled `qsoftphone` and stays up.
* Account CRUD round-trips through SQLite, including the policy block.
* The config file loads, saves and round-trips; a malformed file is an
  error, not a silent reset.

**Not verified:**

* Nothing has been put on the wire. No SIP registration, no INVITE, no
  RTP, no audio device. That is M1–M2.
* The GUI has been started and its window inspected by title, but the
  layout has not been reviewed by a human at normal resolution.
* The Linux build has not been compiled or run. The dependency tree is
  resolved for `x86_64-unknown-linux-gnu` (382 crates) but no Linux
  binary was produced.
