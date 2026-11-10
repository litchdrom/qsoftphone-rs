# qsoftphone-rs

A Rust rewrite of **qsoftphone**, the desktop SIP softphone.

The C++/Qt implementation lives next door in `../qsoftphone` and is the
behavioural reference for everything in this repository. This crate is
not a port of its UI — it is a port of its *semantics*: the SIP
identity resolution, the call event contract, the history rules and
the transfer construction all have to behave identically, including the
bugs that have already been fixed there.

**Status: M6.2 complete.** The workspace builds, the test suite passes, the
app starts and exits cleanly, an account can be registered against a SIP
registrar with digest authentication (MD5 and SHA-256), and calls can be
placed and received over that registration — outbound INVITE, inbound
INVITE with an automatic 180 Ringing, answer, decline, hang up, and DTMF.
A call carries a voice: the RTP runs over a bound UDP port, G.711 and
G.722 are both spoken, and the mute keeps the clock running. The codecs,
the sound card and the test sound live on a settings page, and the
history is in SQLite. The sound itself is continuous now: the playout
waits for a jitter buffer to fill, a lost frame is concealed, the
speaker is fed one value per *frame* rather than per channel slot —
which is what was playing the audio at double speed — and the playout
does **not** touch the pitch to chase the peer's clock, which is what
made it wobble. The sound card's callback no longer takes a lock, and
unregistering in the middle of a call ends the call and closes the
device instead of leaving it to the next one. Every call now says what
its media did. The history is a **table** — `dir | Number | Duration |
Time | code | status`, with the legend above it — and its timestamps
carry the seconds, so a call that lasted twenty seconds no longer reads
as if it started and ended at the same instant. The status line goes
back to "Ready." when the call is over. The dialpad is in the **middle**
of the call page, and it does both jobs: it dials the number when the app
is idle and it sends the DTMF tones during a call. And a call with **no
microphone** — a remote session without a microphone redirection, a
machine with no sound card — still sends RTP silence, so the clock
keeps running and the call does not die. The window now **wakes up**:
the app asks egui to draw again ten times a second, so the registration,
the calls and the history appear when they happen instead of when the
mouse moves. The settings page has an **echo test**: the microphone,
played back through the speaker — and it now **measures** rather than
guesses: a live level meter, the level in decibels, the names of the
devices it used, and a warning when the speaker is a headphone jack.
A call says how many frames it **sent**, and how many of those were
silence the sender had to make for itself. The call page holds **only
the calls that are up**. And the history's expanded details open **under
the row they belong to**, the way the C++ opens them — they used to be
drawn below the whole table, which put them off the screen in a history
of any length. And the user's own packet capture was read against
MicroSIP's call in the same file: there was **no loss at all** in either
direction, the echo that comes back is a faithful copy of the voice that
goes out, and the one thing that was different was the **marker bit** —
set on every single packet, where MicroSIP sets it once. It now marks
the start of a talkspurt, and nothing else. The **volume knobs** are
there too: the microphone's gain is applied for the first time, it goes
above unity as a preamp, and both directions have a knob — live on the
call page, and as the default in the settings. A call can now be
**transferred**: the **Transfer** control on the call page opens a
popup that sends a `REFER` — blind, to a new target, or attended, to a
consultation that is already up — and the peer reports the progress of
the new leg over `NOTIFY`s, which the banner on the call turns into
words. When the new leg is answered the original leg is **released**, so
the user is not left talking to the old peer under a banner that says
"Transferred". The softphone now makes **sounds of its own**: a call that
comes in **rings** — the ringtone is a WAV file of the user's or the
built-in tone, and both of them are picked in the settings, with a preview
button — and a call we made **rings back**, with a generated 440 Hz tone,
whenever the far end answers with a `180 Ringing` that carries no SDP, so
there is nothing to hear. A call that is still ringing can be
**redirected**: the incoming-call screen answers the `INVITE` with a `302
Moved Temporarily` and a `Contact` naming the target, so the call is
forwarded without ever being answered. See
[Milestones](#milestones).

Two of the things that were wrong with it are right now: the **ringtone
can be per account** — the account editor has a file of its own, so two
lines can be told apart by the ring that comes for them — and the
**attended transfer puts the original leg on hold**, the way a phone
does, so the user hears the consultation and not the other party at the
same time. The panel also **follows the call that is up** now: a call
that has ended gives the panel up, and the keypad sends its tones to the
call the user is actually talking to.

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

`--call <number>` places a call **without a window** and writes the
levels of the call to the log, once a second, and the numbers of the
media when it stops — for when there is nobody at the machine to listen
to it. `--seconds N` says how long the call should last and `--codec
NAME` forces the codec. See [M4.11](#m411--where-the-silence-is).

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
    ├── qsoftphone-media/          the media plane: RTP, G.711, G.722, the sound card
    │   ├── audio.rs               the microphone, the speaker, the resampling
    │   ├── codec.rs               G.711, computed rather than tabulated
    │   ├── jitter.rs              the jitter buffer: what arrived, in order
    │   ├── lib.rs                 the session: socket, negotiation, threads
    │   └── rtp.rs                 the UDP socket and the packets on it
    ├── qsoftphone-sip/            the SIP layer: transport, registration, calls
    │   ├── call.rs                placing, answering and ending calls
    │   ├── digest.rs              RFC 7616 interop with rsipstack
    │   ├── endpoint.rs            one endpoint per account, per transport
    │   ├── incoming.rs            answering what arrives on that endpoint
    │   ├── inspector.rs           the message inspector: digest + public address
    │   ├── registrar.rs           REGISTER / refresh / unregister
    │   ├── sdp.rs                 the SDP placeholder and the DTMF encoding
    │   ├── session.rs             one account's endpoint and the dialogs on it
    │   └── state.rs               what the UI needs to know, nothing more
    └── qsoftphone-app/            the binary
        ├── main.rs                startup order, --smoke
        ├── app.rs                 the egui shell
        ├── history.rs             the call history, read on the runtime
        └── picker.rs              the native file dialog, on a thread of its own
```

The split is deliberate. Everything the brief calls out as a bug is a
pure function over strings, which is why it can be unit-tested without
a UDP socket, a GPU or a sound card. `qsoftphone-sip` is the only crate
that touches the network, and it never talks to the window: it reports
state over a channel that the UI drains at the top of each frame (§3).

---

## The stack, and what it costs

Pinned in the workspace `Cargo.toml`, locked in `Cargo.lock`.

| Crate | Version | Role |
|---|---|---|
| `rsipstack` | 0.7.9 | SIP transport, transactions, dialogs |
| `rtc` | 0.21.1 | RTP / SRTP / ICE / DTLS media plane |
| `cpal` | 0.18.2 | microphone capture and speaker playback |
| `nanoaudio-g722` | 0.1.1 | the G.722 wideband codec |
| `egui` / `eframe` | 0.37.0 | the UI (default wgpu backend) |
| `rusqlite` | 0.41 | storage, `bundled` — SQLite is compiled in |
| `tokio` | 1.54.1 | the async runtime the SIP stack runs on |
| `tracing` | 0.1 | logging |
| `rfd` | 0.17.2 | the operating system's own file dialog (M6.4) |

### The dependency budget

The brief asks for a resolved tree under ~250 crates, assuming
rsipstack would be the bulk at 134.

**Measured, the real number is 310 package names** for the Windows target
(390 for Linux, which adds X11/Wayland/ALSA). Four of those are this
workspace's own crates, so 306 and 386 are third-party packages. The gap
is not bloat — it is two subtrees the brief's estimate did not account
for:

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

The count is `cargo tree --workspace --prefix none --edges normal --target
x86_64-pc-windows-msvc`, unique package names. It moved from 303 to 304
when M1 added the `qsoftphone-sip` workspace member — the SIP crates
were already in the tree, because the app declared the whole pinned
stack from M0 so the budget would be measured against the final
dependency set rather than a moving target.

**M2 added no dependency at all.** The resolved tree for `c19bd90` (the
last M1 commit) and for the M2 tree is identical — same package names,
same versions. The call layer is built on rsipstack's dialog API, which
was already there.

**M3 added one package, on Linux only.** The history shows the wall-clock
time of a call, and the C++ shows local time, so the `time` crate's
`local-offset` feature is enabled. It needs `num_threads` — on Unix
only, because that is where the local offset can change under a running
program. The Windows tree is **unchanged**: 304 package names before,
304 after. `time-macros` was already in the tree.

**M4 added no third-party package at all.** The Windows tree went from
304 package names to 305 and the Linux tree from 384 to 385, and the
one package in each is `qsoftphone-media` — this workspace's own crate.
The third-party count is 301 on Windows and 381 on Linux, unchanged.
`bytes` is named directly by the media crate, but `rtc-rtp` already
depended on it.

**M4.1 added four packages: the G.722 codec and what it brings.**
`nanoaudio-g722` is the codec, `nanoaudio-core` holds the types it
shares, and `wide` + `safe_arch` are the SIMD helpers it uses on x86.
The lock file grew by exactly those four — 580 → 584 entries — and the
per-target tree by the same four: Windows 306 → 310, Linux 386 → 390.
(The counts in the paragraph above were taken with a command that
counts one fewer; the delta is the same.)

**M6.4 added one package: `rfd`, the native file dialog.**
`rfd` is what opens the operating system's own "pick a file" window,
the way the C++ opens Qt's `FileDialog`. The lock file grew by two
entries — 584 → 586 — and the per-target tree by **one package name**:
Windows 309 → 310, Linux 389 → 390. The extra lock entry is
`pollster 0.4.0`, which `rfd`'s Linux portal backend wants; the tree
already had a `pollster` (1.0.1), so only the name count moved.

What `rfd` brings with it is almost nothing, because it is almost
nothing: `log`, `raw-window-handle` and `windows-sys` were **already**
in the tree (eframe and winit need them), so on Windows the whole
cost is one crate that compiles against the COM `IFileOpenDialog` that
is already part of the operating system.

**The version is 0.17 and not 0.18, on purpose.** `rfd` 0.18
(published 2026-11-07) declares `rust-version = "1.92"`. Taking it
would have raised this workspace's floor from 1.85 to 1.92 — and
because clippy reads that floor, it would have turned **nine
pre-existing findings** in `qsoftphone-media` and `qsoftphone-core`
(`clippy::chunks_exact_to_as_chunks`, which asks for `as_chunks` — a
1.88 API) into build errors in a milestone that has nothing to do with
the media plane. 0.17.2 declares no floor of its own and brings the
same dependencies. The floor is still 1.85.

The features are trimmed per target:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
rfd = { version = "0.17", default-features = false }

[target.'cfg(target_os = "macos")'.dependencies]
rfd = { version = "0.17", default-features = false }

[target.'cfg(target_os = "linux")'.dependencies]
rfd = { version = "0.17" }
```

`default-features = false` on Windows and macOS leaves out the Linux
portal backend and the Wayland parent-window support — neither of
which exists on those platforms. On Linux the defaults are kept,
because they are the **portal** backend (`xdg-desktop-portal`), which
is the one that needs no system libraries; the `gtk3` backend would
need GTK installed on the machine, which is a worse thing to ask of
a user than a portal that every modern desktop already speaks.

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
| §5.2 caller display name: header → contacts → URI user-part → Display Name | `CallController.cpp` | `sip::identity::resolve_caller_display_name` |
| §5.3 history stores the *remote* number, never the local AOR | `src/core/history/CallHistory.h`, `src/app/Application.cpp` | `model::history::CallRecord` |
| §5.4 180 Ringing on every inbound INVITE | `src/core/sip/SipEndpoint.cpp` | `qsoftphone-sip::call::handle_incoming_invite` |
| §5.5 hang up without a forced status code | `src/core/sip/SipCall.cpp` (`hangup`) | `qsoftphone-sip::call::CallManager::hangup` |
| §5.6 `Refer-To` construction, blind and attended | `src/core/sip/SipEndpoint.cpp` | `sip::refer`, `qsoftphone-sip::transfer` |
| §5.8 digest auth: MD5 + SHA-256, realm vs SIP domain | `src/core/sip/SipEndpoint.cpp` | `qsoftphone-sip::digest` |
| Header capture allowlist | `src/core/call/AccountProfile.h` | `model::account::AccountProfile::filter_headers` |
| The call event contract | `src/core/call/CallEvent.h` | `model::call_event::CallEvent` |

Deliberate differences from the C++:

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
* **`Refer-To` is built by hand, in the brief's exact form.** The C++
  delegates to pjsip's `Call::xfer` / `Call::xferReplaces`. The brief
  is explicit that the attended form carries `Replaces` as a URI
  parameter inside the `Refer-To` value — not as a separate `Replaces`
  header — and that the test suite expects exactly that. The *contents*
  of the parameter are the tagged form (§5.6's example is kept in the
  core and tested, but a PBX matches a dialog by all three of its
  identifiers — see [M5](#m5--transfer)).

### One conflict in the brief, resolved in favour of the C++

§5.3 says `remote_uri` is *"our own URI on an incoming call"*. The C++
disagrees, and agrees with itself everywhere:

* `src/core/history/CallHistory.h`: *"`remoteUri` is the full SIP URI
  as pjsip saw it (caller for incoming, callee for outgoing)"*.
* `src/app/Application.cpp:314-322`:
  `otherUri = outgoing ? ev.callee : ev.caller` — the **peer** side for
  both directions, with the comment *"For the URI line, we also pick
  the peer side"*.
* `src/ui/Dialer.qml` renders `remoteUri` as
  `asterisk@193.178.169.164` for incoming calls — the peer again.

The same section's load-bearing rule — *"`remote_number` is the SIP
peer's user-part, never our own extension"* — is the one the C++
satisfies, and the one that matters for redial. This crate therefore
stores the **peer** URI in `remote_uri`.

The brief's §5.1 requirement (*"store the resolved dialled number in
history and use it for redial"*) is met by a separate `dialed_number`
column, which is not in the C++ schema: under DID routing the peer may
identify itself as `asterisk` while the customer dialled `6666`, and
those are different numbers with different redial behaviour.

If a test suite insists on the literal reading of §5.3 bullet 2, the
change is one line in `model::history::CallRecord::new`.

### The event contract

`CallEvent` serialises to exactly the C++ `toJson()` shape:
snake_case names (`account_id`, `call_id`, `dialed_number`, …), the
same `type` strings (`call_incoming`, `call_outgoing_initiated`,
`call_ringing`, `dtmf`, …), and empty fields omitted rather than
serialised as blanks. The REST API, the CRM plugins and the webhook
dispatcher are all downstream of this stream and never talk to the SIP
layer directly.

---

## M1 — Registration

### What happens

1. The user picks an account and presses **Register**.
2. `RegistrationManager::start` spawns a task on the tokio runtime. The
   UI thread does nothing else — it never waits for the network (§3).
3. The task opens one endpoint for that account — its own socket, its
   own transport, its own Via branch space — and sends a REGISTER with
   `Expires: 300`.
4. A registrar that wants credentials answers `401` with a
   `WWW-Authenticate` challenge; the client answers with the digest and
   retries with a bumped CSeq.
5. On `200 OK` the **granted** expiry is read back (`Expires` may be
   less than what was asked for) and the account is reported as
   registered.
6. The task then sleeps until the refresh is due and registers again.
   Failures back off instead of hammering the registrar.
7. **Unregister** sends a final `REGISTER` with `Expires: 0` and then
   stops the loop.

The state changes travel over an unbounded channel; the UI drains it at
the top of every frame and caches it. The window never polls the SIP
stack and never blocks on it.

### The SHA-256 spelling problem

This is the one place where rsipstack 0.7.9 and RFC 7616 disagree, and
it is worth stating plainly because it is not a cosmetic difference.

RFC 7616 §3.4 names the algorithms `MD5`, `SHA-256` and `SHA-512`.
rsipstack's `Algorithm::try_from`
(`src/sip/headers/auth.rs:33-49`) only accepts `MD5`, `SHA256` and
`SHA512` — the **unhyphenated** spelling. The rejection is not a soft
one: `WWW-Authenticate: Digest ... algorithm=SHA-256` fails to parse
*as a whole header*
(`src/sip/headers/typed/www_authenticate.rs:44-46`), so a registrar
that uses the RFC spelling could not be registered against at all.

`qsoftphone-sip::digest::DigestInterop` closes that gap from the
outside, as an endpoint-level `MessageInspector`:

* **on the way in**, the challenge's algorithm token is rewritten into
  the spelling rsipstack's parser accepts (`SHA-256` → `SHA256`);
* **on the way out**, the `Authorization` header gets the server's own
  token back (`SHA-256`), because RFC 7616 §3.4.1 has the client echo
  the algorithm the server named.

The digest itself is still computed by rsipstack — only the label on
the packet is rewritten. The rewrite is idempotent, which matters
because rsipstack applies `after_received` twice (once when the message
arrives, once when it is handed to the transaction).

**The limitation, stated plainly.** rsipstack's *typed* `Authorization`
parser has the same gap, so an rsipstack-based **server** cannot verify
a client that echoes the RFC spelling. Our client is unaffected against
real servers — Asterisk, Kamailio and OpenSIPS all accept the token
they themselves named — but this crate cannot be used as a
SHA-256 registrar that insists on `algorithm=SHA-256` being echoed
literally. Fixing it properly means patching rsipstack's `Algorithm`
parser, not patching around it here.

### The requests that arrive on the endpoint

rsipstack hands every incoming request that has no matching transaction
to the endpoint's `incoming_transactions()` channel. M1 had nothing
reading that channel, so **every OPTIONS was silently dropped**.

That matters more than it sounds. Asterisk pings a registered endpoint
with SIP OPTIONS; a UA that does not answer is marked *unreachable* by
the server, and the calls that follow fail even though the
registration itself looks perfect. The symptom is exactly
"it registers, and nothing else works".

`qsoftphone-sip::incoming` owns that channel now:

| Request | Answer | Why |
|---|---|---|
| `OPTIONS` | `200 OK` | The registrar's keepalive. |
| `INVITE` without a To tag | `100 Trying`, then `180 Ringing` | A new call — M2 takes it over. |
| anything with a To tag | routed to its dialog | An in-dialog request: the BYE, the DTMF `INFO`, a re-INVITE. |
| anything else | `501 Not Implemented` | A status code beats a timeout. |

The response copies the request's Via, From, To, Call-ID and CSeq
(RFC 3261 §8.2.6) and carries a tag on the To header (§8.1.3.7), so
the transaction matches on the other side.

The To tag is also what tells a **new call** apart from an **in-dialog
request**: an initial request has no To tag, everything that follows in
the dialog has one.

### Transports

* **UDP** binds the configured port up front and falls back to an
  ephemeral one if the port is taken — a second copy of the app should
  not refuse to start.
* **TCP** dials the registrar on demand *and* listens on the local
  port. The listener matters: the registrar may open a **new**
  connection to the Contact instead of reusing the one we dialled out
  on, and that is how the keepalive arrives. The port is resolved
  before the listener is created, because the transport layer does the
  bind and the Contact has to name the port that was really taken.
* **TLS** has no listener. An inbound TLS connection needs a server
  certificate, and the account model has nowhere to put one — that is
  M6. A TLS account therefore only works for requests that arrive on
  the connection we dialled out on.

### Other M1 decisions

* **The SIP domain is not applied to the From/To URI yet.** rsipstack's
  `Registration` derives the Request-URI and the From/To headers from
  one server URI, so the registrar host is used for all three. The
  brief itself puts "SIP domain" in M6, and on a single-tenant PBX the
  two are the same. When an account has a domain set, a warning is
  logged — the domain is not silently ignored. This is the one line to
  change when M6 lands.
* **The refresh loop is ours, not `Registration::keep_alive`.** The
  callback `keep_alive` takes cannot reach back into the registration
  it is driving, and the loop needs the *granted* expiry rather than
  the requested one.
* **A busy SIP port is not fatal.** If the configured port is taken, the
  endpoint falls back to an ephemeral port and logs it — a second copy
  of the app should not refuse to start.

### If the server still cannot reach you

The registration succeeding and the calls failing are two different
problems, and the log tells you which one you have:

1. **`the transport is open`** — the local address and port the
   endpoint is listening on. If this is a private address, the server
   cannot reach you directly.
2. **What the server actually pings** is the Contact it bound, not
   that address. On Asterisk: `pjsip show endpoint <aor>` — look at
   `Contact:` and at `Status:`. `Unreachable` there means the
   qualify timer is failing.
3. If the Contact holds the *private* address, the registrar did not
   report the public one back. rsipstack takes the Contact from the
   Via's `received`/`rport` when the registrar sends them, so this is
   a `nat=` setting on the Asterisk endpoint, not a setting here.
4. The transport has to be **udp** for a complete setup today. TCP
   listens and answers, but a TLS endpoint has no listener, because an
   inbound TLS connection needs a server certificate and the account
   model has nowhere to put one.

### Editing an account

Selecting an account loads it into an editable form in the middle
panel: label, registrar, user, password, transport, domain, outbound
proxy and the two identity headers. **Save** writes it back, **Revert**
throws the changes away.

The account keeps its id and its creation timestamp, so editing is an
update and not a second account — the registration state, the call
history and everything else that hangs off the id stay attached.

The "add account" form on the left has its own draft. They used to
share one, which meant that typing a new account silently changed what
**Save** would write to the selected one. There is a test for that.

### Logging (§6)

Every log line carries the account id, and call lines carry the call
id. This is done with spans rather than by repeating the id at every
call site: `qsoftphone_core::logging::account_span` and `call_span`
build the spans, and the `Full` formatter prints the current span scope
and its fields on each line, so a line written inside the span is
automatically attributed.

```text
2026-10-27T01:12:04.118Z  INFO qsoftphone_sip::registrar: account{account_id="acct-m1-logging" user="104" registrar="127.0.0.1:5060"}: registered expires=300
```

The registration task runs inside the account span, so every line it
writes — including the ones from rsipstack itself — is attributable.

---

## M2 — Calling

### What happens

**Outgoing.** The user types a number and presses **Call**. `CallManager`
takes the number, turns it into a routable URI (a bare number is dialled
at the registrar, the way the C++ does it), and sends an INVITE with an
SDP offer. The dialog's state channel reports `180 Ringing` as
`call_ringing` and the `200 OK` as `call_answered`.

**Incoming.** A `100 Trying` goes out, then — **immediately**, before the
user has seen anything — a `180 Ringing`. A PBX that does not get one
aborts with `503`/`480` after its ringing timeout, so this is not a
nicety (§5.4). The call is reported to the UI, the incoming-call modal
opens, and the user answers or declines.

**Hanging up** never forces a status code (§5.5). The stack picks the
right one for the state and the role:

| State | Role | What goes out | Status |
|---|---|---|---|
| established | either | `BYE` | `200 OK` |
| early | the side that called | `CANCEL` | `487 Request Terminated` |
| early | the side that was called | `603 Decline` | `603` |

Those are the same three cases `pjsua_call_hangup` picks when the status
code is left at zero — see the C++ `SipCall::hangup()` and its comment.
Forcing `603` onto a normal hangup used to make every twelve-second call
show up in the history as "declined"; the same bug would have been easy
to port along with the code.

rsipstack's own `InviteDialog::hangup()` covers two of the three cases —
it cancels an early call of ours and it says `BYE` on a confirmed one —
but it is a **no-op for an early incoming call**, which has to be
declined instead. `CallManager::hangup` makes that third case explicit.

### The two rsipstack behaviours worth knowing

Both of these cost a rewrite to get right, and both are the sort of
thing that works in a unit test and fails on the wire.

**A response only reaches the wire if somebody drives the transaction.**
`InviteDialog::ringing`, `accept` and `reject` *enqueue* the response;
it is sent while `InviteDialog::handle(&mut tx)` is running. Without that
loop the 180 Ringing of §5.4 never leaves the machine. So the sequence
for an inbound call is: `send_trying()` → `get_or_create_server_invite`
→ `ringing()` → **spawn** `handle()`. The spawned loop is also what
answers the ACK and drives the in-dialog requests.

**The Contact has to name the public address.** rsipstack's
`build_local_contact` would produce `sip:104@0.0.0.0:5060`, and every
in-dialog request the peer sends — the BYE, a re-INVITE — would go
nowhere. `qsoftphone-sip::inspector::SessionInspector` therefore watches
the Via of any response for the registrar's `received`/`rport` and
exposes it as the session's `PublicAddress`. The outbound INVITE's
Contact and the inbound `200 OK` use it.

### The answer has to name a codec the offer listed

The first version of the answer was a fixed body: one PCMU stream.
Against a real Asterisk that is **wrong**, and the symptom is brutal —
Asterisk answers the `200 OK` with a `BYE` a millisecond later.

The reason is RFC 3264 §6: the answer may only use media formats the
offer listed. Asterisk's offer is

```text
m=audio 50344 RTP/AVP 8 101
a=rtpmap:8 PCMA/8000
a=rtpmap:101 telephone-event/8000
```

— G.711 **A-law** and telephone-event. A fixed G.711 µ-law answer
(`m=audio 4000 RTP/AVP 0`) names a format the offer never mentioned,
so the answer is invalid and the peer is entitled to tear the call
down. Asterisk does.

So the answer is built from the offer now: the codecs this build
speaks are looked for in what the peer offered, and the peer's own
payload type is echoed back. A peer that put µ-law on a dynamic
payload type gets that number back. When nothing is common the stream
is rejected with `m=audio 0` — RFC 3264's way of saying "no media" —
and the call itself stays up.

The offer lists both G.711 codecs, and deliberately **not**
`telephone-event`: there is no RTP yet, so negotiating RFC 2833 would
make the peer send the digits into a stream nobody reads. Without it
the peer falls back to RFC 4733, which this build can receive.

### The UI thread is not on the runtime

`eframe` owns the main thread and the SIP stack runs on a tokio runtime
of its own, so the thread that draws the window is **not** inside that
runtime. The window is not blocked — the runtime has its own workers —
but the thread-local runtime context is missing.

That matters because rsipstack drives the INVITE from a task it spawns
with `tokio::spawn`, and `tokio::spawn` panics when the calling thread
has no runtime:

```text
thread 'main' panicked at rsipstack-0.7.9/src/platform/mod.rs:92:5:
there is no reactor running, must be called from the context of a Tokio 1.x runtime
```

The panic is on the UI thread, and it happens *before* a single byte
goes out: the INVITE is built, the dialog is registered, and only then
does the spawn fail. The window dies with that message on stderr and
nothing in the log — which is exactly what the report looked like.

The fix is to enter the runtime's context around the call:

```rust
let runtime = self.handle.clone();
let (dialog, _handle) = {
    let _runtime = runtime.enter();
    session.dialog_layer.do_invite_async(option, state_sender)?
};
```

Nothing else in the UI thread needs it: `ringing()`, `accept()` and
`reject()` only queue a message on the dialog's channel, and the hangup
and the DTMF are already spawned on the runtime.

**This is the class of bug the loopback test could not see.** Every
outgoing-call test was a `#[tokio::test]`, which runs *inside* a
runtime — the one situation that never crashed. The regression test is
deliberately a plain `#[test]` that builds its own runtime and places
the call from outside it.

### The INVITE carries the credential

The REGISTER has always answered Asterisk's challenge with the
account's digest (§5.8). The **INVITE** did not: `InviteOption.credential`
was `None`, so a PBX that challenges the INVITE — and Asterisk does,
with the same `realm="asterisk"` — was treated as if it had sent the
final response. The dialog ended with `TerminatedReason::ProxyAuthRequired`
and the call died without ringing.

The credential now comes from the same place the registrar's does, so
the two cannot drift apart.

### The SDP is otherwise a placeholder

There is no audio in M2, and the SDP says so: one G.711 stream
(`PCMU` and `PCMA`) on a port that is not bound, `a=sendrecv`. The call
connects, the SIP trace is clean, and M4 replaces the whole thing with
the session `rtc` negotiated.
The call panel says so too, under the call that is up — "Media: not
connected — the RTP stack lands in M4" — so that a connected call is
not mistaken for a call with audio.

*(M4 replaced all of it: the port is bound, the RTP runs, and the UI
says what the media is doing.)*

### DTMF is RFC 4733 over `INFO`

RFC 2833 puts the digits in the RTP stream, which needs the media
plane. The signalling-only way to send a digit is an `INFO` request with
an `application/dtmf-relay` body (RFC 4733 §3), so that is what M2 uses:

```text
INFO sip:peer@host SIP/2.0
Content-Type: application/dtmf-relay

Signal=5
Duration=100
```

The body is built and parsed by `qsoftphone-sip::sdp`, and rsipstack's
own `parse_info_dtmf` is used to recognise it. M4 may switch to RFC 2833.

### The window

The window opens on the **Call** page: the dialer is at the top, the
calls are listed under it, and the account settings are a page away
rather than in the way.

```text
┌───────────────────────────────────────────────────────────────┐
│ 104 · 104@193.178.169.164:5161   Not registered  [Register]  │
├──────────┬────────────────────────────────────────────────────┤
│ ACCOUNTS │ [Call] [Account]              M2 — calling   v0.1.0│
│          │ Ready.                                            │
│  104     │ Number [ 6666               ]  [Call]             │
│          │ CALLS                                             │
│ + Add    │  ← Alice  connected                   [Hang up]   │
│  Remove  │    [1][2][3] [4][5][6] [7][8][9] [*][0][#]        │
└──────────┴────────────────────────────────────────────────────┘
```

The **Register** button is in the header, next to the state it
changes. A softphone that needs a trip through a settings form before
it can register is a softphone nobody uses — and that was the first
version's mistake: the whole account form sat above the dialer, and
the dialer was below the fold.

The dialer is only live for a **registered** account — a call needs an
endpoint to go out from, and the call layer refuses it if there is
none. The call panel follows the newest call that is not over, and
shows the resolved peer name, the dialled number, the last DTMF digit
and a hang-up button, with the DTMF keypad once the call is
connected. A call that arrives switches the window to the call page.

The incoming-call modal is §5.7's rule: **it cannot be closed by the
user.** No close button, no click-outside-to-dismiss, no Esc — only
Answer and Decline end it. `egui::Modal` never closes itself:
`should_close()` is an opt-in the caller has to ask for, and this app
never asks. The modal is drawn from the call's state, so it disappears
when the call is answered or ends, and not before.

### The identity of an incoming call (§5.2, §5.3)

The caller's display name and the number the remote party actually
dialled are resolved when the INVITE arrives:

* the caller display name goes header → contacts → URI user-part → SIP
  Display Name, the same order as the C++;
* the dialled number goes header → Request-URI → To-URI. The
  **Request-URI** is the DID case — the PBX forwards `sip:6666@host` to
  our AOR — and it is the only way to tell `6666` from `104`;
* only the headers on the account's capture allowlist are kept.

### What is *not* in M2

* **No audio.** The media plane is M4; the SDP is a placeholder.
* **No call history.** The events are produced and shown, but nothing is
  written to SQLite yet — that is M3.
* **No transfer.** REFER is M5; `sip::refer` (the `Refer-To`
  construction) is already ported and tested, but nothing sends it.
* **The SIP domain is still not applied** to the From/To URI — M6, as
  noted above.

---

## M3 — History

### What happens

When a call ends, the app writes one row to the `calls` table, and the
history page shows it. The page is a search box, a row of direction
chips and the list. Each row shows the number, how long the call
lasted, when it happened and the SIP status code; expanding it shows
the full URI, the cause and the exact timestamps. The expanded row has
a **Redial** button.

### The semantics are §5.3's

* `remote_number` is the **peer's** user-part — the caller for an
  incoming call, the callee for an outgoing one. Never our own
  extension.
* `remote_uri` is the peer's URI on both sides. The event carries it:
  the C++'s `CallEvent` has `caller` and `callee` as **URIs**, and
  `Application.cpp` runs `sipUriUserPart()` over them to get the
  number. This tree had them as user-parts, which is enough to show a
  number but not enough for a history row — so they are URIs now, and
  the UI shows the user-part, exactly as before.
* `dialed_number` is the resolved dialled number (§5.1), and it is
  what a redial uses — for an **outgoing** call. For a DID-forwarded
  call the remote party is the PBX that forwarded it, and dialling the
  PBX back is not what the user means by "call them back".

  *(M4.1 made the redial direction-aware: an incoming call is called
  back on the **caller**, because the dialled number of an incoming
  call is usually our own extension. See
  [M4.1](#m41--codecs-the-sound-card-and-the-redial).)*

An incoming call that was never answered is stored as **missed**. The
C++ has the capability — `CallHistory::recordMissed` — but never calls
it, so its unanswered incoming calls land as plain "incoming" and
classify as "cancelled". A call **we** declined is not missed: the
user was there and said no, and the decline leaves a `603` behind.

### The result badge is the C++'s

`classify_result` is the port of `HistoryModel::classifyResult`, case
by case: a missed call is missed; an answered call that lasted is
completed; a local hangup before the answer is cancelled; and the rest
is read off the SIP status code — 486 is busy, 487 cancelled, 408 no
answer, 401/403/407/603 rejected, everything else 4xx and 5xx failed.

### The search

The C++ filters with a case-insensitive substring over the remote
number, the remote URI, the remote name and the hangup cause, and it
matches the SIP status code exactly — so "486" finds every busy call.
That is `HistoryStore::search`, and the direction chips narrow it
further. A `%` or a `_` in the search is a literal, not a wildcard.

### The database work is never on the UI thread

§6 says no blocking in the UI thread, ever, and SQLite is blocking
I/O. The UI does not touch the database for the history: it sends a
command over a channel, a task on the runtime does the work on tokio's
blocking pool, and the answer comes back over another channel and is
drained at the top of the frame (§3).

The account CRUD still runs on the UI thread. It is M0's code, and it
is one row on a button press rather than a query per keystroke — but
it is blocking I/O, and it should move to the same worker. It has not.

### What is *not* in M3

* **No recording.** The `recorded` column is there and always false.
* **No per-account filter.** Every row in the database is on the page.
  The account is stored with the row; filtering by it belongs with the
  rest of the settings, in M6.
* **No contact matching in the UI.** The rule is ported and tested in
  core (`Contacts::best_match_for_phone`, the seven-digit suffix of
  §5.2), but nothing calls it from the history yet.

---

| | Windows | Linux | macOS |
|---|---|---|---|
| config | `%APPDATA%\qsoftphone\config.json` | `$XDG_CONFIG_HOME/qsoftphone/config.json` | `~/Library/Application Support/qsoftphone/config.json` |
| data | `%LOCALAPPDATA%\qsoftphone\data\` | `$XDG_DATA_HOME/qsoftphone/data/` | `~/Library/Application Support/qsoftphone/data/` |
| logs | `%LOCALAPPDATA%\qsoftphone\logs\` | `$XDG_CACHE_HOME/qsoftphone/logs/` | `~/Library/Logs/qsoftphone/` |

Logs rotate daily: `qsoftphone.log.2026-10-26`. `RUST_LOG` overrides
the level from the settings file. Every line carries the account id,
and call lines carry the call id — see
[Logging (§6)](#logging-6).

SQLite lives in one file in the data directory: `accounts.sqlite`, with
the `accounts`, `contacts` and `calls` tables. The C++ keeps three
files (`accounts.sqlite`, `history.sqlite`, `contacts.sqlite`); there
is no reason for the split here, and `AppPaths` still knows the other
two paths for when there is.

---

## M4 — Media

### What happens

A call now carries a voice.

**Outgoing.** The media session binds a UDP port *before* the INVITE is
built, and the SDP names that port. The peer's `200 OK` carries its own
answer; the codec it chose and the address its RTP comes from are read
out of it. When the call is confirmed the microphone opens and the RTP
starts.

**Incoming.** The `200 OK` answers the peer's offer: the same codec the
offer listed, on the peer's own payload type, on our bound port. The
peer's RTP address comes from its offer. When the call is confirmed the
microphone opens and the RTP starts.

**Muting** either side does not stop the RTP. A muted microphone sends
the codec's silence code, so the RTP clock keeps moving and the peer
does not decide the stream has died and hang up. A muted speaker drops
what arrives, locally.

### The crate

`qsoftphone-media` is new, and it is the only crate that touches a
sound card.

```text
crates/qsoftphone-media/
├── codec.rs       G.711 µ-law and A-law, computed rather than tabulated;
│                  and G.722, which keeps a state
├── rtp.rs         the UDP socket and the RTP packets on it
├── audio.rs       the microphone, the speaker, the resampling, the tone
└── lib.rs         the session: the socket, the negotiation, the threads
```

### `rtc` is used for the packets, not for the whole stack

The brief pins `rtc` — "Sans-I/O WebRTC: RTP packets, codecs, SRTP. We
supply the transport." That is exactly how it is used here:
`rtc::rtp` supplies the packet format, the sequence numbers, the
timestamp and the G.711 payloader, and this crate supplies the UDP
socket.

The rest of `rtc` — ICE, DTLS, SRTP, SCTP — is **not** used. A SIP
phone has no use for a data channel, and its media is plain RTP: the
signalling already agreed the address and the codec. `rtc-sdp` is not
used either: the SDP that a SIP phone exchanges is not the SDP that
WebRTC exchanges, and the `rtc-sdp` model is built around ICE
candidates, DTLS fingerprints and media midpoints, none of which
belong in an offer to a PBX. The SDP stays in `qsoftphone-sip::sdp`,
where it already was, and it grew a parser for the peer's answer.

### G.711 is computed, not tabulated

The classic G.711 implementations carry two 256-entry lookup tables per
variant. They are generated here from the algorithm instead, which is
fewer lines and — more importantly — cannot disagree with the decoder.

The tests are exact, not approximate: **every one of the 256 µ-law bytes
and every one of the 256 A-law bytes survives a round trip** through
the encoder and the decoder, and the decoded curve is monotonic. (One
µ-law code is the exception: `0x7F` is the other spelling of zero, and
the encoder always writes `0xFF` for zero.)

### The sound card does not run at 8 kHz

G.711 is 8 kHz, always. A sound card is 44.1 or 48 kHz, almost always,
and it will not open at 8 kHz. So the audio is converted twice on every
call: 48 kHz down to 8 kHz on the way in, and 8 kHz up to 48 kHz on the
way out.

The down direction gets a crude low-pass for free: when the ratio is a
whole number — 48 kHz to 8 kHz is six to one — the samples of each
group are **averaged** rather than picked, which keeps the aliasing out
of the voice band. The up direction is a linear interpolation, because
there is nothing to alias.

The state carries across buffers, so a stream that arrives in pieces
comes out the same as one that did not. There is a test for exactly
that.

### The mute keeps the clock running

A muted microphone still sends RTP. It sends the codec's silence code —
`0xFF` in µ-law, `0xD5` in A-law — so the sequence numbers and the
timestamps keep advancing.

This is not a nicety. A peer that stops seeing RTP tears the call
down, and the symptom is a call that dies a second after the user
hits the mute button. The loopback test proves the clock keeps moving:
five muted frames arrive as five frames.

### The codec is picked, and it is remembered

The codec the next call offers is picked by the user, and it is kept in
the settings file — the `media` section, where the C++ kept the codec
priorities. The C++ let the user set a priority per codec and used the
order; here the order is the list, and the first entry is the one the
offer asks for.

*(M4.1 moved the picker off the call page and onto the settings page,
and turned it into a list with an order — see
[M4.1](#m41--codecs-the-sound-card-and-the-redial).)*

The peer has the last word: if it answers with another codec this build
speaks, that one is used, on the peer's own payload type.

### Two bugs the loopback test found

Both of these are the sort of thing that passes a unit test and fails
on the wire, and both were found by the end-to-end test rather than by
reading the code.

**The inbound queue was bounded in the wrong unit.** The bound was
written as "a hundred frames" and applied as "a hundred samples". A
160-byte frame was truncated to 100 bytes on arrival, and the peer heard
a clipped syllable. The bound is now in samples, and it is
`100 * 160` of them.

**The sending thread could not be stopped.** The sender waits on a
condition variable for a frame to send, and the wait was written to
loop until a frame arrived — the timeout was read and thrown away. The
thread therefore slept in the condvar forever, and `stop()` — which
joins it — hung with it. The hang showed up as a test binary that never
exited, which is a much better symptom than a call that never ends.

### The dependency cost: nothing

**M4 added no third-party package at all.** The Windows tree went from
304 package names to 305, and the Linux tree from 384 to 385, and the
one package in each is `qsoftphone-media` — this workspace's own
crate. The third-party count is 301 on Windows and 381 on Linux, the
same as after M3.

`bytes` is named directly by the media crate, because `rtc-rtp` packs
the RTP payload into it — but it was already in the tree, so naming it
costs nothing.

### What is *not* in M4

* **No NAT traversal on the media plane.** The signalling side already
  learned the public address from the registrar's `received`/`rport`
  echo, and the SDP advertises the public address. The RTP side does
  **not** yet: the port the SDP names is the local one, and a NAT that
  rewrites it will break the call. Symmetric RTP — sending from the
  port the peer's RTP arrived on — is not implemented either. `rtc` does
  ship ICE and STUN, and this is where they would be used.
* **No SRTP.** The RTP is in the clear, as SIP media normally is.
* **No RFC 2833.** The digits still travel as RFC 4733 over `INFO`.
  The offer deliberately does not list `telephone-event`.
* **No ringtone.** The C++ plays a WAV when a call comes in; this build
  does not yet.
* **No device picker.** The microphone and the speaker are the system
  defaults. The settings have the fields for them (`media.microphone`,
  `media.speaker`) and the media crate reads them, but the UI to pick
  them is M6.

*(M4.1 built the picker and put it on the settings page.)*

---

## M4.1 — Codecs, the sound card and the redial

Four things the user asked for, in one milestone, because they are
one thing: **the things that are not about one call.**

### The codec picker is off the call page

It was a combo box in the middle of the dialer, with two entries, and
it chose the codec the *next* call would offer. It is now a **list on
the settings page**, with an order:

```text
CODECS
  PCMU   G.711 µ-law, 8 kHz              [▲] [▼] [×]
  PCMA   G.711 A-law, 8 kHz              [▲] [▼] [×]
  G722   G.722, 16 kHz wideband          [▲] [▼] [×]
```

The order is the offer's order, and the list is kept in the settings
file — the `media` section, where the C++ kept its codec priorities.
It is **app-level**, like the C++'s: the codecs are what the softphone
speaks, not what one account speaks. (Per-account codecs are still on
the backlog; one account may only speak PCMU and another only PCMA.)

The peer still has the last word: if it answers with another codec this
build speaks, that one is used, on the peer's own payload type.

### G.722 — the wideband codec

**G.722 is 16 kHz of speech in 64 kbit/s**, and it is not a companded
codec like G.711: the signal is split into two sub-bands by a
quadrature mirror filter and each is coded with ADPCM. Two things
about it surprise everybody, and both are in RFC 3551 §4.5.2:

* the RTP clock rate is **8000 Hz**, not 16000 — the original
  recommendation had a historical error in it and the payload type kept
  the wrong number so that the two ends would agree;
* so the payload is **160 bytes** per 20 ms frame, exactly like G.711,
  and the RTP timestamp advances by 160, exactly like G.711.

**The RTP layer did not change at all.** The payloader is opaque bytes
(`rtc::rtp` even ships a `G722Payloader` alias of the G.711 one) and
the clock is the same 8 kHz. What changed is the codec: the frame is
320 samples instead of 160, the samples are 16 kHz instead of 8 kHz,
and the encoder and the decoder **keep a state** — an ADPCM decoder
that starts late, or out of step, produces noise.

So the media crate grew a per-codec description:

| | sample rate | RTP clock | frame samples | frame bytes | rtpmap |
|---|---|---|---|---|---|
| PCMU | 8000 | 8000 | 160 | 160 | `PCMU/8000` |
| PCMA | 8000 | 8000 | 160 | 160 | `PCMA/8000` |
| G722 | **16000** | 8000 | **320** | 160 | `G722/8000` |

And the encoders became objects instead of functions:

```rust
let mut encoder = Codec::G722.encoder();   // it remembers the signal
let frame = encoder.encode(&pcm);          // 320 samples in, 160 bytes out
let mut decoder = Codec::G722.decoder();   // and so does this
let samples = decoder.decode(&frame);      // 160 bytes in, 320 samples out
```

A muted microphone still sends — silence — and the silence goes
**through the encoder** like any other frame, so an encoder that keeps
a state stays in step with the decoder at the other end. G.722 has no
fixed silence code: the silence is whatever the encoder makes of a
frame of zeros.

### The codec was checked against the reference vectors

This is the part that matters. A G.722 codec that is self-consistent
but **not** the same as the one on the other end is worse than no codec
at all: the call connects, and the peer hears noise.

`nanoaudio-g722` is a Rust port of the CMU 1993 implementation — the
same code lineage as SpanDSP's `g722enc`/`g722dec`, and therefore the
same code Asterisk's `codec_g722.c` runs. It was checked against the
reference vectors that ship with `sippy/libg722`:

| vector | what it is | result |
|---|---|---|
| `pcminb.dat` → `pcminb.g722.out` | encode the reference PCM | SHA-256 **matches** |
| `pcminb.g722.out` → `pcminb.raw.16k.out` | decode it again | SHA-256 **matches** |
| `test.g722` → `test.raw.16k.out` | decode the reference stream | SHA-256 **matches** |
| `fullscale.g722` → `fullscale.raw.out` | decode a full-scale stream | SHA-256 **matches** |

Byte for byte, against the published digests. The round trip is good
too: 41–60 dB of signal-to-noise on a tone, once the 22-sample group
delay of the synthesis filter is taken into account.

Two other implementations were measured against it and **did not** match
byte for byte — one of them is a careful transcription of the ITU Blue
Book, and it disagrees with the CMU code on the first octet. That is
why the reference vectors, and not a round-trip test, decided it.

### The sound card and the test sound

The **Settings** page also holds the microphone and the speaker
pickers, and a **Test sound** button: a third of a second of 440 Hz
through the speaker, with a fade at each end so that it does not click.
It says whether the speaker works without anybody having to make a
call.

Both are **not** on the interface thread (§3). Reading the sound card's
devices opens the host and asks it what it has, and playing the tone
opens the device and waits for it; both happen on the runtime, and the
answer comes back over a channel and is drained at the top of the
frame.

The device names are persisted the way the C++ persisted them — the
friendly name — and an empty name means the system default.

### The redial calls the caller back

The bug the user reported: **a call from 6661 to 104 redialled 104**.

The dialled number of an incoming call is the number the caller dialled
to reach us, and when there is no DID in front of us that is *our own
extension*. Dialling it calls ourselves.

So the redial target is now the **peer**, and it is direction-aware:

* **outgoing** — the number that was dialled. For a DID-forwarded call
  the remote party is the PBX that forwarded it, and dialling the PBX
  back is not what the user means by "call them back" (§5.1).
* **incoming** — the **caller**, unless the resolved dialled number is
  something else than our own extension. That exception is the DID
  case: a PBX that forwards `sip:6666@host` to our AOR and puts the
  customer in a header, where the dialled number is 6666 and the peer
  is `asterisk`.

The C++ settled the same thing after the user clarified it: the history
shows the peer, and redial dials the peer.

The redial also goes out on **the account the call was on**, not on
whichever account happens to be selected now.

### The dependency cost: four packages

| package | what it is |
|---|---|
| `nanoaudio-g722` | the G.722 codec — MIT, pure Rust |
| `nanoaudio-core` | the types it shares — MIT |
| `wide` | the SIMD helpers it uses on x86 — MIT/Apache-2.0 |
| `safe_arch` | `wide`'s helper for the same |

The lock file grew by exactly those four: 580 → 584 entries. The
per-target tree grew by the same four — Windows 306 → 310, Linux 386 →
390 (`cargo tree --workspace --prefix none --edges normal --target …`,
unique names; the counts in the M4 section were taken with a command
that counts one fewer).

That is the price of not writing the codec ourselves, and it was worth
paying: the alternative was a hand-ported ADPCM encoder whose only
proof would have been its own round trip.

### What is *not* in M4.1

* **No Opus.** It needs a pure-Rust encoder (`opus-rs`, which brings
  `fearless_simd` and `libm` with it), the RFC 7587 RTP framing, and a
  **third-party `codec_opus` module on the PBX** — Asterisk does not
  ship one. It is the next codec, and it is a bigger job than G.722
  was.
* **No ILBC.** It is a third codec with the same shape of problem:
  `codec_ilbc` ships with Asterisk, but there is no pure-Rust ILBC in
  the tree to use.
* **No codec per account.** The list is app-level; the backlog item
  stands.
* **No ringtone.** The C++ plays a WAV when a call comes in, and
  synthesises a ringback tone for an outgoing one. The test sound is
  neither.

---

## M4.2 — The jitter buffer

### The bug

A call connected and the sound was broken — a "dithering" that came and
went, for G.711 and for G.722 alike. **Both codecs being wrong the
same way was the clue**: a codec fault does not look like that, and
G.711 and G.722 share no code at all below the RTP layer.

The fault was above the codecs, and it took two milestones to find it
all. **M4.2 is the first half**: there was no jitter buffer. **M4.3 is
the half that actually mattered**: the sound card's channels were
miscounted, and the audio was being played at double speed. Both had
to be fixed, and the second one is the one that made the noise.

The first half, then:

RTP is UDP. The peer sends a frame every 20 ms, and the sound card
takes what it is given, ten milliseconds at a time. Without a buffer
the speaker plays the ten milliseconds that have arrived and then goes
silent until the next packet — so every twenty milliseconds the sound
stopped and started again. The queue hovered between nothing and one
frame, and that is exactly what was reported.

The C++ never had the problem: pjmedia (pjsip 2.14) has a jitter buffer
in it, and the RTP goes through that. This build had the RTP and the
sound card, and nothing between them.

### What is in it now

Three things, and the first is the one that fixes the sound.

* **The playout waits for the buffer to fill.** The speaker stays
  silent until four frames — 80 ms — are waiting to be played, and then
  plays them steadily. That is the jitter delay: the time the audio
  spends in the machine so that a packet which arrives late does not
  leave a hole in it.

  *This is necessary, and it is not sufficient: see M4.3.*
* **A jitter buffer** (`media::jitter`) holds the frames that arrived,
  in sequence order, so a packet that took a detour is put back in its
  place and a packet that never arrives is noticed.
* **A lost frame is concealed** rather than dropped, because a decoder
  that skips a frame falls out of step with the encoder at the other
  end — and for G.722, which is ADPCM and keeps a state, that is noise
  for the rest of the call.

### Late is not lost

The distinction is the whole of a jitter buffer. A frame that has not
arrived yet may be on its way; a frame that has been given up on never
will be. So the buffer does not decide by itself — it hands out the
frame it is waiting for, and the playout comes back with "I have run
out of patience" when the audio it has left runs out.

* A frame that arrives **late but in time** is used. The playout waits
  for it as long as it has slack.
* A frame that **never arrives** — while the peer keeps sending — is
  concealed, and the decoder takes its step so that it stays in step
  with the encoder at the other end.
* A frame that never arrives **because the peer stopped sending** is
  not concealed at all. It is not a lost frame, it is the end of the
  stream, and inventing audio for it would be wrong. The playout goes
  silent, which is what silence is.

The rule is tested both ways: a peer that stops sending produces
nothing more, and a peer that skips a frame gets a concealment and
stays in step.

### The concealment, and why the obvious G.722 answer is wrong

G.711 has a silence code — 0xFF for µ-law, 0xD5 for A-law — and the
concealment is that code. G.722 has no silence code, and the two
obvious candidates are both traps. This was measured, not guessed:

| What is fed to the decoder | What happens |
|---|---|
| a frame of zeros | the high band's code 0 means "a large negative change" **and** carries the largest magnitude index, so the step size grows while it is fed — the sound is at full scale within a frame or two |
| the last frame, repeated | the step size follows the magnitude of the code, so a loud frame makes it grow with every repetition — measured peak 32,767 |
| **0xC0, repeated** | "no change" to both bands, with the *smallest* magnitude index there is, so both step sizes shrink: the decoder converges, the peak stays bounded, and the real frames bring the step sizes back |

The table is the codec's own — `LOW_MAGNITUDES`, `HIGH_MAGNITUDES` and
the level tables in `nanoaudio-g722` — and the numbers are the ones the
tests measure.

What a lost G.722 frame costs is honest: the decoder's estimate of the
signal is one frame out of date, and it catches up over the next few
frames (measured on a ramp: 13.8 dB, then 22.0 dB, then 31.7 dB of
signal-to-noise over the three frames that follow). G.722 is a
closed-loop codec, and a closed-loop codec cannot be resynchronised for
free. What it can do is stay bounded, and it does.

### The two clocks

The peer's packets are timestamped with *its* clock and the sound card
runs on *ours*, and the two drift against each other by a few hundred
parts per million — the difference between two independent sound cards.
Left alone, the buffer slowly empties and the sound breaks up again,
minutes into a long call, which is the worst possible time for it.

**M4.2 answered that by trimming the playout rate**: the playout played
very slightly faster or slower, by at most half a percent, according to
how the buffer's level was trending. The correction was slow, and it
followed the *average* level rather than the level of the moment, so one
late packet did not throw the rate about.

**That was wrong, and M4.4 removed it.** A rate change *is* a pitch
change, and the correction was not the gentle half-percent it was
meant to be: the buffer's level moves in whole frames, and a fast loop
over a signal that steps a whole frame at a time turns that into a
pitch wobble at the frame rate. Measured on a 400 Hz sine, the pitch
wandered and the sine's purity — how much of the output's energy is at
the frequency it is supposed to be at — fell from **1.000 to 0.17**.
That was the "dithering", and it was in this section, not in the
network.

What holds the clock difference now is the buffer's **slack**: four
frames, eighty milliseconds of difference between the two clocks. That
covers a peer's drift for the better part of a minute, and it does it
without touching the pitch. A clock recovery that does *not* touch the
pitch — a timestamp-driven playout, as pjmedia does it — is the next
piece of work and is on the [backlog](#backlog--low-priority).

### What the C++ does, and what this does not

The C++ goes through pjmedia's jitter buffer (`pjmedia_jbuf`), and it
is worth being precise about that one, because this build borrowed the
ideas and not the code:

* it is **frame-indexed** — a frame goes in at its own sequence
  number, so a frame that arrives out of order goes to its own slot;
* it has a **prefetch**: while the buffer is still filling up to the
  prefetch count it hands out "nothing yet", which is the same idea as
  this build's priming;
* the **prefetch is adaptive**: the buffer measures the burst level —
  the jitter it actually sees — and moves the prefetch with it, so the
  delay follows the network;
* a frame that never arrived is **reported as missing**, and the
  transport runs the concealment for it;
* when the buffer grows — which is what a difference in clocks does —
  it **discards** frames to shorten the latency again.

This build is fixed-depth (four frames) rather than adaptive, it is
level-driven rather than clock-driven, it conceals a lost frame in the
codec itself rather than in the transport, and — since M4.4 — it does
not trim the playout rate at all: the difference between the clocks is
absorbed by the buffer's slack rather than by changing the pitch. A
timestamp-driven playout, which is how pjmedia absorbs it without
audible artefacts, is still to come.

80 ms is a deliberate choice. ITU-T G.114 puts the one-way delay of a
telephone call under 150 ms, and a hole in the audio is worse than
delay. Making the depth adaptive, the way pjmedia does, is the obvious
next step — it would take the delay down to a few frames on a clean
network without giving up the margin on a bad one.

### The dependency cost: nothing

No new crate. The jitter buffer is a `VecDeque` and a sequence-number
comparison. (M4.2's rate correction was also a few lines on the
resampler; M4.4 deleted them.)

### What is *not* in M4.2

* **No RTCP.** The receiver does not hear the peer's reports, so it
  does not know the peer's idea of the jitter, and it does not send
  ours. The buffer's depth is fixed at four frames rather than
  negotiated.
* **No comfort noise.** A lost frame is concealed with the codec's
  own "no change", not with synthesised noise. On a link with real
  loss — a lossy mobile connection, say — a proper comfort-noise
  generator would sound better than silence.
* **No forward error correction.** Asterisk can be configured with
  FEC for G.722; this build does not ask for it and cannot use it.
* **No DTMF in the media plane.** RFC 4733 still goes out as SIP
  `INFO`, exactly as in M2.

---

## M4.3 — The sound card's channels

### The bug, again

The jitter buffer went in and the sound was **still** broken. The
report was the same as before: a "dithering" that came and went, for
both codecs.

And the clue that broke it open was this one: **the raw pcap plays
without distortion.** The payloads on the wire are good. So the fault
was not in the network, not in the codecs and not in the RTP — it was
in what the build did with the samples between the socket and the
speaker.

### What was wrong

The playback wrote **one resampled sample per channel slot**. The
device's buffer is *interleaved*: a frame is one sample per channel,
and the frames follow each other. A stereo card therefore wants one
value per **frame**, and it got one per **slot** — so the audio was
played at **double speed**.

The capture side had it right all along:

```rust
let frames = data.len() / channels;   // the microphone: one value per frame
for frame in 0..frames {
    let mut sum = 0.0;
    for channel in 0..channels { sum += data[frame * channels + channel]...; }
    capture.push(sum / channels as f32, &state);
}
```

The playback side did not:

```rust
for slot in data.iter_mut() {          // the speaker: one value per slot
    let value = playback.next(&state);
    *slot = T::from_normalised(value);
}
```

Which is why the capture never showed the fault, and why the *sent*
audio was clean in the pcap while the *received* audio was noise.

### Why it sounds like that

At double speed the voice is a chipmunk, and — this is the part that
made it "dithering" rather than merely fast — **the buffer can never
stay full**. Twice as much is taken out of the playout buffer as goes
into it, so it is empty almost all the time: the speaker plays a
fragment of audio, then silence, then the next fragment, as fast as
the packets arrive. That is a dither, and the fragments are the
missing bits.

The measurement is in the test, and it is not subtle: with the old
code, a stereo card starved the playout **44,876 times out of 48,000
samples — 94 % of the time.** With the fix: never.

It also explains why the jitter buffer could not save it. A buffer
cannot hold a level that is being drained twice as fast as it is
filled, and no amount of priming changes that.

And it explains why the test sound was fine: the tone is generated at
the card's own rate, so playing it at double speed is still a tone —
just the wrong one, and a warble rather than a clean 440 Hz. That is
fixed too, in the same way.

### The fix

`fill_output` walks the buffer **frame by frame** and gives every
channel of a frame the same value:

```rust
for frame in data.chunks_mut(channels) {
    let value = next();
    for slot in frame { *slot = T::from_normalised(value); }
}
```

The microphone and the speaker are now symmetric, and the audio moves
at the speed of the frames.

### The dependency cost: nothing

### What is *not* in M4.3

* **No per-device channel layouts.** Mono and stereo are handled; a
  device with more than two channels gets the mono signal in every
  channel, which is what a softphone wants anyway.
* **No loudness control.** The pcap plays at the peer's level and so
  does this; there is no gain stage between the socket and the card.
* **The decimating resampler still averages only on a whole-number
  ratio.** On a 44.1 kHz card the microphone's 44.100 → 8.000
  conversion goes through point sampling rather than an averaging
  filter, which is a weak low-pass. It is a quality wart on the
  *sent* audio, not the received, and the card here runs at 48 kHz —
  but a proper anti-aliasing filter belongs on that path.

---

## M4.4 — The pitch wobble

### The bug, a third time

M4.2 and M4.3 both went in to fix "the distorted sound" and the report
came back the same: *"Still have distorted sound. ANYHOW raw pcap plays
sound without distortion."* The pcap playing cleanly is the clue that
has held all along — **the payloads on the wire are good**, so the
fault is in what this build does with the samples between the socket
and the speaker.

M4.2 built the jitter buffer and M4.3 fixed the channel count, and
neither was enough. What was left was the **playout's own rate
correction**, which M4.2 had added and nobody had measured.

### What the correction did

M4.2 wanted the playout to follow the peer's clock, because the two
sound cards drift against each other, and it did it by trimming the
resampler's step by up to half a percent. The intent was a sixth of a
semitone — inaudible.

The problem is that the buffer's level moves in **whole frames**: it is
a count of frames waiting to be played, and it steps by one frame at a
time, at the frame rate. A control loop with a fast gain (0.02 per
callback) over a signal that steps a whole frame at a time does not
produce a small steady offset. It produces a **pitch wobble at the
frame rate** — the resampler's step is pushed up and down once per
frame, and the sound's pitch moves with it.

### The measurement

This is where M4.2 went wrong, and it is worth saying plainly: the
drift tests only checked that the buffer **did not run dry**. They
could not see a pitch that was moving. So M4.4 built the measurement
that could.

The **fidelity harness** drives the whole path — a sine wave into the
codec, through the jitter buffer, the decoder, the sample queue, the
resampler and out to a simulated 48 kHz stereo card — and measures the
output with a **Goertzel filter**, which picks out one frequency out of
a signal. The number it reports is the **purity**: how much of the
output's energy is at the frequency it is supposed to be at. A clean
sine is 1.000.

| | purity | fundamental |
|---|---|---|
| with the rate correction | **0.17** | 400.5 Hz (wandering) |
| without it | **1.000** | 0.248 of the expected 0.25 |

A wobble smears the phase, and a smeared sine has its energy spread
across a band — which is exactly what a purity of 0.17 says.

### The fix

**The rate correction is gone.** Deleted, not clamped: `Resampler::rate`
and `set_rate()`, `Playback::average_level`, and the three constants
that drove them (`MAX_RATE_TRIM`, `RATE_GAIN`, `LEVEL_SMOOTHING`).
`Playback::begin()` is now just the priming check — and, since M4.5,
the one place the callback takes the shared queue.

What holds the clock difference instead is the buffer's **slack**:
four frames, eighty milliseconds. That covers a peer's drift for the
better part of a minute, and it does it without touching the pitch.

The rule this milestone leaves behind: **any rate change is a pitch
change.** A jitter buffer that corrects a clock difference by changing
the playout rate is trading one artefact for a worse one.

### A constant shift is not a wobble

The purity metric has to distinguish the two, or it will fail a build
that is merely a fraction of a percent sharp. It does:

* the metric **searches** for the strongest frequency near the
  nominal one, so a build that sits at 400.5 Hz is still found, and is
  not a wobble;
* it takes the **worst** short window (200 ms), because a shift smears
  in a long window while a wobble smears in every window it touches.

### The tests

M4.4 replaced two drift tests that could only see "the buffer ran dry"
with seven that can see the audio:

* `the_audio_that_comes_out_is_the_audio_that_went_in` — the whole
  path, both codecs: purity above 0.98, the amplitude is right, and
  there is **no energy at twice the frequency** (which is what a
  double-speed playout looks like).
* `the_audio_survives_a_jittery_and_lossy_peer` — one frame in fifty
  lost, packets arriving early and late: less than 5 % silence, purity
  above 0.95.
* `the_pitch_does_not_move` — the dominant frequency, measured second
  by second, stays within 1 Hz of 400 Hz.
* `the_buffer_absorbs_the_difference_between_the_two_clocks` — thirty
  seconds of a peer that is 0.125 % slow: no underrun.
* `a_fast_peer_does_not_make_the_delay_run_away` — the other way
  round.
* `a_stereo_device_gets_one_value_per_frame` and
  `a_stereo_device_plays_the_audio_at_the_right_speed` — M4.3's
  regressions, kept.

The first of these was checked against the old code: **it fails** when
the rate correction is put back. That is what makes it a regression
test and not a description.

### The dependency cost: nothing

No new crate, and no new dependency of any kind. The measurement is a
Goertzel filter, which is a recurrence with two state variables.

### What is *not* in M4.4

* **No timestamp-driven playout.** The proper fix for the two clocks —
  what pjmedia does — is to play each frame at the time its RTP
  timestamp says, and to absorb a clock difference by discarding or
  repeating a sample *at the boundary* rather than by changing the rate
  of everything. That is the next piece of work and it is on the
  [backlog](#backlog--low-priority). Without it, a clock difference
  bigger than the buffer's slack — minutes into a long call, at a few
  hundred parts per million — will still empty the buffer.
* **The live call is still unverified.** The path is proved in
  simulation and the numbers are clean, but the failing call's pcap
  is not on this machine, so the real stream's loss and clock offset
  have not been replayed through it. The pcap turned out not to be
  needed: the log line that came with the report named the cause, and
  it was not in this section — see
  [M4.5](#m45--the-stale-call-and-the-real-time-callback).
* **No adaptive jitter depth.** The buffer is still a fixed four
  frames, so the delay is a flat 80 ms.

---

## M4.5 — The stale call and the real-time callback

### What the log said

The report came with a log line, and the log line is the clue:

```text
WARN qsoftphone_media::audio: the microphone reported an error
      error=A buffer underrun or overrun occurred.
```

That is `cpal`'s `ErrorKind::Xrun`. In cpal's WASAPI backend it is
emitted in exactly one place — when the capture buffer comes back with
the **`AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`** flag, which the
Windows audio engine sets when *a client on that endpoint did not
consume its buffer in time*.

It is not a codec error and it is not a network error. It says the
sound card had more than one client, or that a client's callback
missed its deadline.

And it came with a second observation, which turned out to be the
same bug:

> if user do unregister in active call state — next call became stale
> and does corrupted mic. After full restart of the app — echo test
> passed.

A restart fixes it. That is the signature of **state that outlives the
thing it belongs to**.

### The stale call

Here is the chain, and every link is in the code:

1. `AccountSession` holds the media of that account's calls, in a map
   keyed by Call-ID.
2. The media is taken off the account in exactly one place:
   `forget_media`, and it is called from exactly one place — the
   `DialogState::Terminated` arm of the call's watcher.
3. **Unregistering does not terminate the dialog.** The registration
   task is cancelled, the endpoint's token goes, the transport dies —
   and the dialog layer never pushes `Terminated`, because the thing
   that would have pushed it is the thing that just died.
4. `SessionRegistry::remove()` — which exists, and which closes a
   session properly — **had no caller in production code at all**. Only
   a test called it.
5. So the account's session stayed in the registry with its media
   attached: the **microphone and the speaker stayed open**, and the
   RTP threads kept running and kept sending.

The next call then opened a **second** capture stream and a **second**
render stream on the same device. Two clients on one WASAPI endpoint
is exactly the situation the engine flags with a discontinuity, and
exactly the situation where the microphone's audio comes back
corrupted — and where the second render client leaves the first one
starved, which is what "hear nothing" is.

### The fix

* **Unregistering ends the account's calls.** `RegistrationManager::stop`
  removes the session from the registry, and removing a session now
  **closes** it: the token is cancelled, every media session is
  stopped, and the sound card is closed.
* **A closed account ends its calls.** The account's session has a
  `CancellationToken`, and the call's watcher waits on it beside the
  dialog's state channel. When the account goes away the watcher ends
  the call: the media is released and the interface is told the call
  ended, so it does not sit on the screen as a stale call.
* **The teardown does not wait for the network.** The final
  `Expires: 0` REGISTER is a round trip, and the user does not wait
  for it: the calls end and the devices close **now**, on the
  runtime — never on the interface thread (§6).

### The real-time callback

The second defect is separate, and it is worth being precise about
what it is: the sound card's callback took a **lock**.

The playback callback pulled its samples from the shared queue one at a
time, taking the mutex every time — thousands of times a second, in a
thread with a deadline. A lock is a kernel wait, and a kernel wait in
a real-time thread is a buffer overrun. It was not proved to be *this*
call's overrun, and this README does not claim it was: the stale call
is the proven cause. But a callback that blocks is a defect on its own
account, and the fix is the one the discipline demands:

* **The callback takes the queue once per callback**, not once per
  sample, and then works on its own copy. The per-sample path has no
  lock in it at all — the per-sample accessor is gone, so there is no
  way for it to come back.
* **The test for it is a test for the discipline**, not a description:
  a whole callback's worth of samples, and exactly **one** take of the
  queue.

### The call now says what it did

Every call used to end in silence, in every sense: there was no number
anywhere saying whether the audio had arrived, been lost, or run dry.
A call now logs what its media did when it stops:

```text
INFO qsoftphone_media: the call's media stopped
      received=1832 concealed=3 underruns=0
```

* `received` — the frames the peer sent.
* `concealed` — the frames that never arrived and had to be concealed.
* `underruns` — the samples of silence the speaker had to play, which
  is the number that says the sound broke up.

A call that ended in silence now says so, instead of having to be
guessed at from the outside. That is the measurement M4.2 and M4.3
both lacked.

### The dependency cost: nothing

No new crate. The token is `rsipstack`'s own `CancellationToken`, which
the registrar was already using.

### What is *not* in M4.5

* **Two concurrent calls still open two sound cards.** The stale call
  is fixed, but a user who genuinely places a second call while the
  first is up still gets two capture streams on the device. The C++
  had one sound card for the whole application; this build has one per
  call. That is a design difference, not a bug, and it is on the
  backlog.
* **The capture callback still takes a lock** — once per 20 ms frame,
  to hand the encoded frame to the sender. It is far better than the
  old playback path, but it is still a lock in a real-time thread; the
  proper answer is a lock-free hand-off, which is a bigger change
  than this milestone.
* **The jitter buffer is still a fixed four frames**, and the two
  clocks are still absorbed by its slack rather than by a
  timestamp-driven playout. See M4.4.

---

## M4.6 — The history row, the legend, and the remote session

Three things, and the third is the one that matters.

### The redial is in the row

The redial existed, but only inside the **expanded** row: a user had to
click "Details" to find the button they came for. It is now in the row
itself, next to "Details", and the expanded panel is for reading —
the full URI, the dialled number, the cause, the exact timestamps.

The target is unchanged and still correct: the **caller** for an
incoming call, the number that was dialled for an outgoing one (§5.1).

### The legend

The rows use an arrow, a colour and a bare number, and none of those
explain themselves: a `486` in the middle of a line means nothing to
anyone who has not read the code. There is now a key above the list:

```text
→ outgoing   ← incoming   OK  Missed  Busy  No answer  Rejected  Failed
a bare number is the SIP status code
```

The colours are not written twice. `CallResult::color()` lives in the
core crate next to `CallResult::label()`, and the legend and the rows
both ask it — so the key and the rows cannot drift apart.

### The remote session

> Some softphones has issue with sound passing rdp session. Could you
> try to prevent that issues.

**What Windows actually does**, and it is worth being exact because
the two directions behave differently:

* **Playback** is redirected by default. The sound the application
  plays comes out of the **client's** speakers.
* **Recording is not.** The microphone only appears in the remote
  session when the *connection* asks for it — "Remote audio recording
  → Record from this computer" in the client's settings, or
  `audiocapturemode:i:1` in an `.rdp` file — and when the policy on the
  machine allows it. Seeing a "Remote Audio" output device is **not**
  proof that there is a microphone.

So a softphone in a remote session can hear the other end and cannot
be heard, and the reason is a switch on the client. That is not
something an application can fix. What it *can* do is stop making it
worse, and say what is going on.

**The bug it was making worse: a call with no microphone sent
nothing at all.**

The module has always claimed that "a machine with no sound card is
not a reason to refuse the call: the RTP still runs, the peer still
hears our silence". That was **not true**. The sending thread waited
for a frame to arrive from the capture side, and with no microphone no
frame ever arrives — so the wire stayed empty. A peer that sees an
empty RTP stream treats the call as having no media, and a good many
of them hang up.

The sender now makes the silence itself, on the frame clock, when
there is no microphone:

* the sender waits one frame period (20 ms — every codec's frame is
  20 ms) for a frame;
* if one arrives, it is sent;
* if none arrives and there **is** a microphone, the frame is merely
  late, and the sender waits for it rather than put a hole in the
  middle of real audio;
* if none arrives and there is **no** microphone, the sender encodes a
  frame of silence and sends it.

The silence goes **through the codec's encoder**, like any other
frame — so a stateful codec (G.722, where the encoder's state *is*
the codec) stays in step with the decoder at the other end.

### The device fallback

A default endpoint that refuses to open is not the end of the road:
the next device is tried. A remote session's redirected endpoint is
often the one that refuses, and there is another one behind it.

A device the user asked for **by name** is still the only one tried.
If the user picked a headset and it is gone, opening the laptop's
microphone instead would be worse than useless: the user would not
know which microphone the other end was hearing.

### Saying so

* The **settings page** says what a remote session is and where the
  switch for the microphone is, and that the call still works — the
  peer hears silence and this end still hears the peer.
* The **call's status line** says why the microphone is missing, when
  it is missing because the session never brought one.
* The detection is `GetSystemMetrics(SM_REMOTESESSION)` — one call to
  `user32`, read once at startup.

### The dependency cost: nothing

No new crate. The Win32 call is a three-line `extern "system"` block
with `#[link(name = "user32")]`.

### What is *not* in M4.6

* **The microphone cannot be brought into a remote session by the
  application.** The switch is on the client and the policy is on the
  machine. What this build does is keep the call alive and say where
  the switch is.
* **The remote session has not been tested against one.** This machine
  is at the console, so the detection returns false here and the
  notice is exercised by a test that forces it on. The behaviour of
  the redirected endpoint itself is the operating system's, and the
  fallback covers the case where it refuses.
* **Two concurrent calls still open two sound cards** — see M4.5.

---

## M4.7 — The history as a table, the dialpad, and the timestamps

Four things were asked for, and one more turned up on the way.

### The history is a table

The rows are read **down the columns**, and a row of marks with nothing
to say what the columns are is not a table. The history page is now a
grid with a header:

```text
dir     Number      Duration   Time    code   status
◀       6661        1:35       09:07   200    OK
        Bob
▶       559         0:22       08:50   200    OK
        Alice
✗       7777        —          08:31          Missed
        Carol
```

The **legend stays above it** — it is the key to the marks and the
colours, and the columns do not explain them.

Three things follow from being a table:

* **The header is drawn even when there is nothing under it.** A table
  that grows a header once it has rows is a list that occasionally grows
  a header; and the empty state ("No calls yet.") goes *under* the
  header, where it belongs.
* **The expanded details go under the table, not inside the row.** A
  grid's columns are sized by their contents, so a full SIP URI in the
  "dir" column would make the whole table as wide as the URI. The
  details are for reading, and they read better under the table anyway.
* **The actions have no title.** "Redial" and "Details" are not a
  column of information; they are what you do with the row.

### The start and the end of a call

> Started: 30 Oct 2026, 08:26
> Ended: 30 Oct 2026, 08:26

The **data** was never wrong: the row's `started_at` and `ended_at` are
two different instants, and the duration column (00:22) said so. The
**format** was too coarse — the timestamps were printed to the minute,
so a call that lasted twenty seconds started and ended "at the same
time" on the page. A history that cannot tell you when a call ended is
a history that looks broken.

The stamp carries the seconds now:

```text
30 Oct 2026, 09:07:06
```

which is also what the C++ does — `Qt.formatDateTime(startedAt,
"yyyy-MM-dd HH:mm:ss")` in `src/ui/HistoryPanel.qml`.

### The status line goes back to "Ready."

"Hanging up…" was written to the status line when the hangup was
*requested*, and nothing ever wrote over it: the call ended, the row
landed in the history, and the status line went on saying that
something was still being hung up. A status line that lies about the
state of the app is worse than no status line at all.

The `CallEnded` event now puts it back to "Ready." — the same thing it
says when there is nothing to say.

### The dialpad, and the milestone note that was not information

The header of the call page carried the label "**M5 — settings**": the
build's own bookkeeping, telling the user which milestone the software
was at. It is of no use to a user, and it is gone.

In its place is the **dialpad**, on the right of the page header, where
the C++ has it on the dialer page (`src/ui/components/Keypad.qml`). It
is one keypad that does both jobs, exactly as the C++ describes it: *"it
dials numbers when idle and sends DTMF tones during the call"*. When a
call is up, the same keys send the tones; when the app is idle, they
dial the number.

(The header was the wrong place for it; **M4.8** moves it into the
middle of the page, where the C++ has it. See
[M4.8](#m48--the-keypad-in-the-middle-the-window-that-did-not-wake-up-and-the-echo-test).)

The separate DTMF keypad that used to live inside the call panel is
gone — there is one keypad now, not two.

### The arrow is drawn, not typed

The direction column used to be a character — `→`, `←`, `✕` — and the
pictures showed a **little empty box** in every row.

The reason is the font: egui's bundled proportional face (Ubuntu-Light)
has no arrows in it, and a character the font does not have is drawn as
a box. The C++ gets away with the same characters because Qt goes
through the platform's fonts, which do have them.

So the marks are **painted** — a triangle for the way the call was
going, a cross for a missed call — with the painter, in the direction's
colour. The character is still in the accessibility tree (a screen
reader says "rightwards arrow"), it is only not painted.

The colours live in the core crate next to the labels, as
`Direction::color()`, so the legend and the rows cannot drift apart —
the same rule as `CallResult::color()`.

### The pictures

This machine's desktop is locked, so its screen cannot be captured: the
window is never composited and both `CopyFromScreen` and `PrintWindow`
come back black. The pictures in this repository were therefore made by
**painting egui's own tessellation** — the shapes egui produced for the
frame, rasterised by hand — which needs no window, no GPU and no
unlocked desktop. That is scaffolding, not part of the application: it
is not in the tree.

### The dependency cost: nothing

No new crate, and no new file in the tree.

### What is *not* in M4.7

* **The expanded row's details are not in the row.** They are under the
  table, because a grid's columns are sized by their contents.
* **The keypad is only on the call page**, like the C++'s. It was in the
  header of that page in M4.7; **M4.8 moved it into the middle of the
  page**, where the C++ has it.
* **Two concurrent calls still open two sound cards** — see M4.5.

---

## M4.8 — The keypad in the middle, the window that did not wake up, and the echo test

Three things were asked for:

> The keypad is nice but why its not in the middle of the window?
> Also it starts working really slow.
> No my audio in echo test.

### The keypad is in the middle

It was in the **header** of the page, on the right, next to the page
buttons — which is where a status widget belongs, not a dialpad. The C++
has it in the middle of the dialer page, between the number and the
recent-calls list (`src/ui/Dialer.qml`), and that is where it is now:

```text
  Number  [6666            ]  [ Call ]

              [ 1 ] [ 2 ] [ 3 ]
              [ 4 ] [ 5 ] [ 6 ]
              [ 7 ] [ 8 ] [ 9 ]
              [ * ] [ 0 ] [ # ]

  CALLS
```

The centring is in the **content area**, not the whole window: the window
has an accounts panel down the left, and a keypad in the middle of the
*window* would sit under that panel. It is centred in the page the user
is looking at.

The header row is now the same height on every page, and the keypad only
exists on the call page — as in the C++.

### "It starts working really slow"

The complaint was not about the start. It was about the window **not
waking up**.

egui only draws when something asks it to. The things that change in this
app all arrive from **outside** egui — the registration state, the
incoming call, the call state, the media state, the history rows — and
nothing was ever asking egui to draw again. So the window sat on the
picture it had until the user happened to move the mouse: the status
line lagged behind the app, an incoming call did not appear until the
mouse moved, and the whole thing looked slow.

The app now asks to be drawn again, ten times a second:

```rust
ui.ctx().request_repaint_after(POLL);
```

A tenth of a second is quick enough that nothing is noticed and slow
enough that an idle window costs nothing. The request is made in
`draw()`, so the headless tests exercise it too — there is a test that
asserts the app asks for a repaint.

The **start** was measured rather than guessed, because "it starts
slowly" is a complaint that needs a number behind it. The app records
when it was started and logs the time to the first frame:

```text
the first frame was drawn millis=49
```

Forty-nine milliseconds — the window is up in a twentieth of a second. The
log line is in the log file from then on, so "it feels slow" can be
checked against a number instead of argued about.

### The echo test

> No my audio in echo test.

The settings page had a **test sound**: a 440 Hz tone through the
speaker, which says that the speaker works and nothing else. There was no
way to test the **microphone** at all, and the C++ has none either — its
`SettingsWindow.qml` offers a "Loopback self-test" and that is all.

The echo test is the microphone, played back through the speaker: you
speak, and you hear yourself. It is the only test of the sound card
that checks the microphone, the speaker **and** the two together, and it
is the only one a user can do alone — nobody has to be at the other end
to answer.

How it works:

* The capture callback takes the samples, mixes the channels down to one
  and pushes them into a queue.
* The playback callback takes them out of the queue and writes them to
  the speaker, resampling if the two devices do not run at the same rate
  — the same resampler a call's audio goes through.
* **The lock is taken once per callback**, never per sample. A real-time
  callback that takes a lock waits on a kernel object, and the wait
  shows up in the audio as an overrun.
* The queue holds **two seconds**. A playback that falls behind does not
  make the echo longer and later.
* The test runs for five seconds, on the runtime — never on the
  interface thread.

The sound card is a **shared resource**, and that is the rule the test
runs under: a call holds the microphone and the speaker, and a second
client on one endpoint is exactly what makes cpal report
`A buffer underrun or overrun occurred`. So the echo test **refuses to
run during a call**, and a call **refuses to start** while the echo test
is holding the card.

What it reports:

```text
Your voice came in; the loudest moment was -18 dB (Microphone Array into Headphone).
```

The **device names** are in the answer, and they are the point. A quiet
echo test is usually a test that ran on a device the user did not mean
to use: a laptop whose default speaker is called "Headphone" while the
headphones are unplugged plays into nothing at all, and the microphone
hears the room instead of the test. On the machine this was built on,
the default output is `Headphone (Realtek(R) Audio)` and the speakers
are `Speaker (Realtek(R) Audio)` — a difference that is invisible until
the report says which one it used.

The report has three cases, and they are different problems:

| The report says | What it means |
|---|---|
| "The microphone sent nothing at all" | the capture never ran — no microphone, or it was unplugged |
| "The microphone was silent — nothing came in" | the capture ran, and the room was quiet — you did not speak, or the microphone is muted |
| "Your voice came in; the loudest moment was −18 dB" | it worked |

If the card never answers at all, the button does **not** sit at
"Listening…" for ever: the test is given up on after thirty seconds and
says so.

Wear headphones. Without them the microphone hears the speaker and the
test howls — which is the loopback of a loopback.

### What was measured, on this machine

The sound card of this machine was exercised directly, with the real
function, not a mock:

```text
echo test finished in 5.37s: peak=0.007190 samples=240128 (-42.9 dBFS)
```

240,128 samples in five seconds is exactly the microphone's rate (48 kHz),
and the peak is the room's noise floor, because nobody was talking to
it. The capture half of the test is therefore **proven on real
hardware**.

The playback half runs at the right rate too — the output callback
delivered 96,000 samples a second (48,000 frames, two channels) — but
**the acoustic loop could not be closed here**: this machine's desktop
is locked, and the microphone did not pick up the speaker even with a
full-scale tone playing into it. Whether the sound actually comes out of
the speaker is therefore the one thing in this milestone that only a
person at an unlocked machine can say.

### The dependency cost: nothing

No new crate. The echo test reuses the resampler, the device picker and
the error type the calls already use.

### What is *not* in M4.8

* **The echo test does not loop the playback back into the microphone.**
  It is the user's own ears that judge the speaker, which is what makes
  it a test a user can do alone.
* **The settings page still offers "System default" as the speaker**, and
  on this machine that means the *headphone* endpoint. Choosing the
  speaker explicitly is the fix, and the report now shows which device
  was used.
* **Two concurrent calls still open two sound cards** — see M4.5.

---

## M4.9 — The microphone tells the truth, and the calls are the ones that are up

Two things were asked for:

> 1) Mic does nort work
> 2) can we keep that size on the screenshot.
>    a) remove calls section - it just waste the space.

And the log line that came with it:

```text
WARN qsoftphone_media::audio: the microphone reported an error error=A buffer underrun or overrun occurred.
```

### What "the microphone does not work" turned out to be

The complaint was that the **echo test showed nothing and no sound came
out**. The app could not say why, because it had nothing to say: it
counted the frames the peer sent and never counted the frames it sent,
and it reported the microphone as "silent" without saying how silent.

So it was measured, on this machine, with the real function:

| What was measured | What it says |
|---|---|
| The capture endpoint's state | **active**, not muted, 92 % |
| Windows' microphone permission | **Allow** (for console apps) |
| The samples the microphone delivered | **96,000 a second** — the right rate |
| The loudest sample it ever carried | **−48 dBFS** — and **−67 dBFS** at other moments |
| A full-scale tone on `Speaker (Realtek(R) Audio)` | the microphone's peak moved by **0.4 dB** |
| A full-scale tone on `Headphone (Realtek(R) Audio)` | the microphone's peak moved by **0.1 dB** |
| The speaker the app is using | `Headphone (Realtek(R) Audio)` — **a headphone jack** |

So: nothing is blocking the microphone, the microphone is delivering
samples at the right rate, and it is hearing **nothing**. And the
speaker the app is playing into is a **headphone jack**, which is
silent unless something is plugged into it.

That is two different things, and the app said neither of them.

### The echo test now shows the level

The report was a yes/no — "the microphone was silent" — which is the
one answer that helps nobody. A quiet room and a voice two metres away
are at the same level, and "silent" says nothing about which one it was.

It is now a **number**, in decibels:

```text
The microphone heard nothing: it stayed at -67 dB (Microphone Array (AMD Audio Device) into Headphone (Realtek(R) Audio)).
If you spoke, the microphone is muted or the wrong input is picked.
```

```text
Something came in; the loudest moment was -18 dB (Microphone Array (AMD Audio Device) into Speaker (Realtek(R) Audio)).
```

And while the test runs there is a **level meter** — the live level of
the microphone, with the peak held beside it:

```text
Listening…
▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░|  -21 dB
Speak now. If the bar does not move, the microphone is muted or the wrong input is picked.
```

The meter is the actual test. The number cannot say *whose* voice it
was; the meter can, because the user speaks and watches it move. **If
the bar does not move when you talk, the microphone is not working** —
and no amount of waiting will change that.

The scale is **−60 dB to 0 dB**, because that is the range a voice
lives in: −60 dB is the floor of a quiet room, and 0 dB is as loud as
it can get without clipping.

### The headphone jack is named

A headphone endpoint with nothing plugged into it is the commonest
reason a test is silent, and the name is what says so:

```text
The speaker is 'Headphone (Realtek(R) Audio)'. If nothing is plugged into it, there is no sound.
```

It is a guess from the name, and it is only used to say something
helpful.

### The overrun is not an error

The line the user pasted:

```text
the microphone reported an error error=A buffer underrun or overrun occurred.
```

is cpal's way of saying that Windows' audio engine flagged the capture
buffer with `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`. On this machine
it arrives **once, when the stream starts, on every device** — the
engine's buffer was already full before the client attached to it — and
the audio carries on.

It was logged as a warning about a device, which is what it looks like
when something is wrong and is not what it is. It is now said as what
it is:

```text
INFO qsoftphone_media::audio: the device reported a buffer overrun; the audio carries on
```

### The frames that went out are counted

The call's summary counted what came **in** and said nothing about what
went out, so a call in which the microphone produced nothing looked
exactly like a call in which the microphone worked:

```text
the call's media stopped received=1358 concealed=12 underruns=3714
```

It now says both, and it says which of them was silence the sender had
to make for itself:

```text
the call's media stopped received=1358 sent=1358 sent_silence=0 concealed=12 underruns=3714
```

`sent_silence` is the one that matters: it is the number of frames the
sender made **because there was no microphone to make them**. A call
with `sent_silence=0` had a microphone, whatever the levels say.

### The calls section holds the calls that are up

The call page listed **every call it had ever seen**, finished ones
included, which is why it was a section that wasted space: a row of a
call that ended ten minutes ago cannot be answered or hung up, and the
history — which can be searched, expanded and redialled — has its own
page.

It now holds **only the calls that are up**, and a call leaves it when
it is hung up:

```text
CALLS
No calls up.
```

The dialpad stays in the horizontal centre of the page, and the window
size is left as it is.

### The dependency cost: nothing

No new crate.

### What is *not* in M4.9

* **The microphone is not fixed.** It is *measured* now: the app says
  what the level was, which devices were used, and whether the frames
  went out. If the machine's microphone is dead, the app says so — it
  cannot make it work.
* **The call page does not show the microphone's level during a call.**
  The meter is for the echo test; a live meter during a call is the
  same idea and is not built yet.
* **Two concurrent calls still open two sound cards** — see M4.5.

---

## M4.10 — The details are under the row they belong to

The report was:

> Thanks. Sound works. Calls shows only active. But now history details
> are missing.

with a picture of the history page in which the first row's button read
**"Hide"** — so the row *was* expanded — and the details were not on the
screen.

### What "missing" meant

The details were not missing. They were drawn **after the whole table**.

The history was an `egui::Grid`, and a grid sizes its columns by what is
in them. The details of a row are long — the full SIP URI, the exact
timestamps, the hangup cause — so they could not be a column of the
table: they would have made the table as wide as the longest URI. They
were therefore drawn below the table, and a history with twenty-five calls
put the details of the *first* row below the *twenty-fifth*, which is off
the screen.

So the button worked, the row was expanded, and the details were on the
page — just not on the part of the page anybody was looking at. Clicking
"Hide" and clicking "Details" looked like the same thing.

### The table is drawn by hand, and the details go under their own row

The C++ has the answer, and it is not a grid either
(`src/ui/HistoryPanel.qml`): the row is a `RowLayout`, and inside it there
is an **"Expanded details panel"** — a `ColumnLayout` with
`Layout.fillWidth: true` — that becomes visible when the row is
expanded. The details belong to the row and they are **inside** it.

So this build now does the same:

* the table is drawn by hand, with the cells placed at rects the code
  computes — not at rects egui sizes by their contents;
* the columns have **fixed widths** (`COL_DIR`, `COL_NUMBER`,
  `COL_DURATION`, `COL_TIME`, `COL_CODE`, `COL_STATUS`), so a long URI
  cannot widen the table;
* the details are drawn **inside the row loop**, under the row they
  belong to.

The columns line up, which is the part a grid could not promise. Measured
at 1920 points wide:

| | dir | Number | Duration | Time | code | status |
|---|---|---|---|---|---|---|
| the header | 238 | **270** | 378 | 454 | 522 | 574 |
| the row | 238 | **270** | 378 | 454 | 522 | 574 |

The number in the row is at 270, exactly under the "Number" header — not
at 259, which is where the content-sized cell put it before.

### The second bug: the table was wider than the window

Drawing the page is how the first bug was found, and it found another
one. The table was sized against `ui.available_width()`, and inside a
scroll area that number **runs past the edge of the window** — the
panel's own margin is not clipped away:

```text
the scroll area's content rect   [238.0 163.0] - [732.0 916.0]   (494 points wide)
the visible part of it            [230.0 163.0] - [716.0 916.0]   (478 points wide)
```

Sixteen points of the table were outside the window. In a 716-point
window the **"Details" button was past the edge** — the button that opens
the details could not be clicked at all, in a window the size the user
has.

The table is now measured against the **clip** rectangle, which is where
the window ends:

```text
the "Details" button ends at      707.6
the window is                     716.0
```

and when the page is too narrow for the fixed widths, the columns shrink
together, each by the same share, so they keep their proportions and
still line up.

### The dependency cost: nothing

No new crate.

### What is *not* in M4.10

* **The history is not paginated.** It draws every row it has, in a
  scroll area. A history of ten thousand calls is still ten thousand
  rows.
* **The details are not a copyable field.** The URI and the timestamps
  are labels; there is no button that puts them on the clipboard.
* **The columns cannot be re-ordered or resized.** The widths are
  constants in the code.

---

## M4.11 — Where the silence is

The report was:

> Hello, Thanks. No sound
> `INFO qsoftphone_media::audio: the device reported a buffer overrun; the
> audio carries on device="microphone"`
> echo test working fine. I hear myself.

Two things in it: no sound on a call, and a log line about an overrun. The
second one is a red herring — it is the **microphone**, it has been `INFO`
since M4.9, and the audio carries on. The first one is the report.

"No sound" is not a diagnosis. A call has two directions and three places
a thing can go wrong between the microphone and the ear, and each of them
looks exactly the same from the user's chair.

### What was measured, on the user's own PBX

Everything below was measured against the real Asterisk, on the user's
own account, with the user's own devices. The build's media path was
read end to end first — the codec framing, the negotiation order, the
jitter buffer, the resampler, the gain — and **nothing was wrong with
it**. The audio does reach the sound card.

The peer's RTP payload was then decoded and looked at, and that is where
the answer was:

```text
the first ~400 ms      -23 dBFS RMS, -18.5 dBFS peak
the rest of the call   -45 … -66 dBFS
the live level, once a second     -57 … -81 dBFS
```

There **is** audio on the line, and there is almost none of it: a burst
in the first second and then a signal that is barely above the noise
floor. The line is quiet, and the decoder was right to decode it.

Our own side, in the same call:

```text
the peer's audio, at the speaker      0.1469   (-16.7 dBFS)
the microphone picked up               0.0020 … 0.0123   (-54 … -38 dBFS)
```

The first number is exactly the peer's peak (0.1836) times the speaker
gain (0.8) — the audio arrives at the sound card at the level it should.
The second is a room with nobody in it: the measurement was taken with
no one at the machine.

### The app now says where the silence is

The call row carries a **live meter** for the peer, **three numbers** and
**one sentence**:

```text
Peer [▓░░░░░░░░░░░░░░░░░░] -60 dB   peaks: peer -15 dB · you -66 dB · speaker -17 dB
The peer is sending, but it is too quiet to hear — their microphone is muted, too far away, or turned down.
```

* the **bar** is the level *now* — the peer as they are at this moment;
* the **number** beside it is the same thing in decibels;
* the **peaks** are where each of the three got to during the call — what
  the peer sent, what the microphone picked up, and what the speaker was
  given;
* the **sentence** names the place the silence is in, in words.

The sentences are the ones that can be true:

| | |
|---|---|
| The microphone is muted | the peer hears silence |
| The speaker is muted | nothing comes out of it |
| The peer is sending silence | there is nothing on the line to hear |
| The peer is sending, but it is too quiet to hear | their microphone is muted, too far away, or turned down |
| The peer is sending audio, but the speaker is getting silence | the sound card is not playing it |
| The audio reached the speaker, but it is too quiet to hear | turn the speaker volume up |
| The peer hears nothing | nothing is leaving this machine |
| The microphone is picking up almost nothing | turn the microphone volume up, or the peer will hear silence |

**The peer is judged on the level now, and not on the peak.** A line that
carried a beep in the first second and room noise ever since is quiet,
and the peak would say the opposite — the peak was -15 dB in the call
above, and there was nothing to hear.

### The bug the picture found

The first picture of the new row showed the numbers **on top of each
other**: "peer -66 dB" printed over "-66 dB", both unreadable.

It was not the font and not the separator. The level meter painted its
own number in decibels **outside the space the layout had given it** —
the bar was allocated 140 points, the number was painted at 148 — so the
number landed on whatever came next, which was the text with the three
levels in it. Two pieces of text on top of each other are two unreadable
pieces of text.

The number is now **measured and given room in the layout**
(`layout_no_wrap` → the galley's width → the space the widget takes), so
it can never land on its neighbour. The test
`the_meters_number_does_not_land_on_the_next_thing` reads the two boxes
out of the frame and compares them, and it fails if the number is put
back where it was:

```text
the number              198.2 … 232.4
the text after it       198.2 … 287.6      <- the same x: on top of it
```

### The `--call` diagnostic

A call without a window, for when there is nobody at the machine to
listen to it:

```powershell
cargo run -p qsoftphone-app -- --call 559 --seconds 15
cargo run -p qsoftphone-app -- --call 559 --seconds 15 --codec PCMA
```

It registers, calls, and writes the levels of the call once a second and
the numbers of the media when it stops:

```text
the media of the call: codec=G722 pt=9 port=61123 remote=Some("193.178.169.164:50056")
                         mic=Some("Microphone Array (AMD Audio Device)")
                         speaker=Some("Headphone (Realtek(R) Audio)") problem=None
the levels of the call: peer=0.00119 peer_peak=0.18365 you=0.00026 speaker=0.14692
the call's media stopped received=504 sent=663 sent_silence=0 concealed=312
                          underruns=16194 incoming_peak=0.2296 outgoing_peak=0.0123
                          played_peak=0.1837
```

`--codec` forces the codec, which is how a call is checked against one
PBX codec at a time.

### The dependency cost: nothing

No new crate.

### What is *not* in M4.11

* **The sentence is not a diagnosis of the far end.** It can say that the
  peer's audio is too quiet to hear; it cannot say whether their
  microphone is muted, far away, or turned down. The user knows which
  it is.
* **The levels are not per-codec.** A G.722 call and a G.711 call are
  measured the same way, and the decibel figures are comparable because
  both are full-scale floats.
* **The peaks are never reset.** They are the peaks of the call, and a
  call that had one loud moment keeps that number until it ends.
* **The meter shows one direction.** The peer's level is the one that is
  a bar; the microphone and the speaker are numbers only.

---

## M4.12 — The codec the peer chose, and the frames that never came

### What the user reported

> Red is sent blue returning from rcho test. It's really weird. For test I
> use microsip with the same server - nice loud returning from asterisk
> echo. No issues at all.

MicroSIP, the same PBX, the same machine, the same extension: a loud,
clean echo. This app: the echo comes back, and it is broken.

### What was measured, on the user's own PBX

Everything below is a measurement. The `--call` diagnostic (M4.11) is
what made them possible: it places a call, runs for a given number of
seconds, and prints the numbers.

**Our sending is textbook-correct.** A dump of every packet that went
out, over twenty seconds: fifty frames a second, the sequence number up
by one every time, the timestamp up by 160, a hundred and sixty byte
payload, **zero** sequence gaps.

**The path carries what we send.** A 1 kHz tone injected where the
microphone's samples go in comes back out of the speaker at the same
level — correlation 0.999 — so nothing in the app is attenuating the
audio.

**And yet a quarter of the peer's frames never arrive.** Over a
twenty-second call: 1050 frames sent, 829 received, 206 lost. The
peer's own stream has holes in it — the sequence numbers jump by two
and by three — so the frames are gone before they reach us.

**It is not our receiving thread.** It spends 97 % of its time blocked
in the socket read, waiting for packets that do not come.

**It is not the codec.** A PCMA call loses the same 20–29 %.

**It is not the marker bit.** RFC 3551 §5.1 reserves it for the first
packet of a talkspurt and the packetizer sets it on every frame, which
is wrong — but clearing it changes nothing: 74 % either way.

**It is not our send timing.** A sender paced by a timer instead of by
the sound card, with a completely different shape of gaps, loses the
same.

### The bug: the peer chose the codec

`parse_answer` took **the first format in the peer's answer** that this
build speaks. Asterisk answers `m=audio 50026 RTP/AVP 9 8 0` — G.722
first — whatever the offer said, so every call landed on G.722, and the
user's own preference (`PCMA` first) was never consulted.

That matters because **G.722 is a stateful codec**. It is ADPCM, and it
keeps a state: the decoder at the other end has to see every frame, in
order, from the first one. A frame the network loses does not leave a
hole in the audio — it leaves the decoder out of step, and **everything
after it is noise** for the rest of the call.

G.711 does not have that problem. It is a lookup table: a lost frame is
a lost frame, and the next one is fine.

So the same 20 % of frames that the path drops costs a G.711 call a
dropout every half second, and costs a G.722 call a ruined conversation.
**MicroSIP asks for G.711 and gets a loud, clean echo; this app was
being pushed onto G.722 and was getting noise.**

The fix is one loop: the codec we use is the one **we** prefer among the
answer's formats.

```rust
for wanted in codecs {
    for format in &media.formats {
        ...
        if codec != *wanted { continue; }
        return Some(Answer { codec, ... });
    }
}
```

RFC 3264 §5.1 says the offer lists the formats in order of preference
and §6.1 asks the answerer to keep that order — but a peer that does
not keep it must not take the choice away from us. pjsip does the same
thing, and so does the C++: its comment says it, "the codec priority is
the single mechanism pjsip uses to decide which codec to put first in
the SDP offer/answer".

**The call now negotiates PCMA against the user's own Asterisk**, and
the echo comes back at the level we sent it.

### The bursts, and the jitter buffer

The peer's packets do not arrive one at a time every 20 ms. They arrive
in **clumps** — the median gap between two arriving packets is under a
millisecond — with gaps of up to half a second in between.

The jitter buffer held **five** frames. A clump of ten overflows it, and
the overflow drops the oldest frames *on our side*: they have to be
concealed, and a concealment is a hole in the audio that no amount of
decoding fills in.

It holds **twelve** frames now, which is enough for a clump, and the
number of concealed frames fell from 435 to 233 — the frames that are
genuinely missing, and no more. The *playout* delay went from 80 ms to
120 ms, which is the delay of the call; the holding area for a clump is
a different thing, and it costs nothing.

### The frames that never came are now counted

The holes in the peer's sequence numbers are counted, and they are told
apart from the frames we hid ourselves. The call row says so:

> The path is dropping the audio: 206 of the peer's 1035 frames never
> arrived (20 %).

And with G.722 it adds why that is worse than it sounds:

> G.722 keeps a state, so every lost frame leaves the peer's decoder
> out of step — G.711 would only drop out.

A quiet peer and a path that drops the audio sound the same to the ear.
Now they do not look the same on the screen.

### The clock, when there is no microphone

A call with no microphone has to send anyway: the RTP clock is what
tells the peer the call is alive. The sender made the silence itself,
on the frame clock — and it ran at **31 frames a second instead of 50**.

Windows' timers are coarse: a twenty millisecond wait can take
thirty-one. The sender waited twenty milliseconds between the frames,
so the RTP clock ran at half speed, and the peer heard our voice
stretched and full of holes.

The frames are now sent when they are **due**, and a frame that is
overdue is sent at once, so the clock stays in step with the wall clock
even though the waits are coarse. Measured: fifty frames a second.

### The dependency cost: nothing

No new crate.

### What is *not* in M4.12

* **The lost frames are not repaired.** They are gone, and no amount of
  buffering brings them back. The app says how many there were.
* **The peer's codec is still the peer's business.** We ask for G.711
  first and use it when the answer allows it; a PBX that answers with
  G.722 only gets G.722.
* **The diagnosis does not blame the network.** It says the frames did
  not arrive; whether the path dropped them or the PBX never sent them
  is not something this side of the call can tell.
* **The microphone is still not fixed by this.** A silent microphone is
  still a silent microphone — the call row says so in its own words.

---

## M4.13 — The two captures, side by side

### What the user asked for

> Hello. Please compare two dumps. 1st call qsoftphone - 2nd call microsip

One capture, two calls, one PBX: this app first, MicroSIP second. It is
1839 packets long and it holds four RTP streams — what each end sent,
and what each end got back.

### What is in the capture

| | **qsoftphone** | **MicroSIP** |
|---|---|---|
| Stream | `192.168.0.71:60712 ↔ 193.178.169.164:50196` | `192.168.0.71:38000 ↔ 193.178.169.164:50138` |
| What was sent | **PCMA (PT 8)**, 540 packets of 172 bytes | **G.722 (PT 9)**, 373 packets |
| The clock | 50.1 fps, timestamp +160 every time | 50 fps |
| Sequence gaps | **0** | **0** |
| **Marker bit** | **set on 540 of 540** | **set on 1 of 373** |
| Inter-arrival | p10 12.1 ms, p50 20.8 ms, p90 22.5 ms, max 23.6 ms | p10 19.7 ms, p50 20.0 ms, p90 20.3 ms, max 29.7 ms |
| What the PBX sent back | PT 8 for 525 packets, **PT 9 for the first 3** | PT 9 for all 374 packets |
| Sequence gaps (back) | **0** | **0** |

Two calls, one PBX, and **not one frame lost in either direction**. The
"a fifth of the peer's frames never arrive" of M4.12 was the environment
on the day, not the codec and not this app: the same build with the
same codec was clean at 19:43 on 2 November and broken from 20:17 that
evening, and it is clean today, with both codecs.

### The audio, decoded

Both calls were decoded with this crate's own decoders — the same G.711
and G.722 code that plays the audio:

| | sent (peak / rms) | returned (peak / rms) |
|---|---|---|
| qsoftphone | 0.1914 / 0.0144 | 0.1914 / 0.0264 |
| MicroSIP | 1.0000 / 0.0909 | 1.0000 / 0.0950 |

MicroSIP's echo is a copy: what came back is four per cent above what
was sent. Ours is the same at the peak, and the extra in the average is
the PBX's own **start beep** — a loud quarter of a second at the head of
the returned stream, louder than the speech, and part of what looked
"weird".

The app's own counters for that same call agree: `received=524 sent=540
lost=1 concealed=1 underruns=0`, with `incoming_peak=0.19140625` against
`outgoing_peak=0.19102521`.

### The answer is not the answer

The capture carries the SDP of both calls. Ours:

```
m=audio 60712 RTP/AVP 8 9 0
```

Asterisk's:

```
m=audio 50196 RTP/AVP 9 8 0
```

**G.722 first.** And yet Asterisk sent **PCMA** for 525 of its 528
packets. The answer is the peer's list of what it can do; the channel
codec is picked from the **offer's** order. That is M4.12's fix,
confirmed on the wire: this app asks for PCMA first and gets PCMA.

### The one difference: the marker bit

Everything else about the two streams is the same. This is not:

* ours: the marker is set on **every** packet — 540 out of 540;
* MicroSIP's: the marker is set on **one** packet out of 373.

RFC 3551 §5.1: the marker "is set to 1 to indicate that the packet
contains the first octet of a talkspurt". It marks where a talk
**starts** — not where every frame starts.

`rtc`'s packetizer cannot know that. It sets the marker on the **last**
payload of a frame, and a frame here is one packet long, so the marker
lands on every packet. pjsip — the media stack of the C++ reference —
does it differently and simply: it keeps a flag for "we are streaming",
clears it when nothing is transmitted, and sets the marker on the first
packet after that.

### The fix

The marker is now set by the RTP layer, which is where a talkspurt is
known:

* the **first** packet of the stream carries it — it is the start of the
  first talkspurt, and the peer may be waiting for it;
* the **first packet after a silence** carries it — that is where the
  next talk starts;
* **no other packet does.**

Silence is decided on the **samples**, before they are encoded, and it
travels with the frame into the sender (`OutboundFrame`). It cannot be
decided on the encoded frame: G.722 has no silence code at all, and
G.711's silence code is not the same thing as a quiet room. The
threshold is −54 dBFS — quiet enough for the hum of a room, loud enough
for a breath.

The tests read the marker bit off the wire (the top bit of the second
byte of the RTP header) and check all three cases: the first packet of a
stream, the first packet after a pause, and a stream that is silence
from beginning to end.

### The dependency cost: nothing

No new crate.

### What is *not* in M4.13

* **The frames the peer never sent are still gone.** M4.12 counts them
  and says so; nothing brings them back.
* **The send jitter is still ours.** Our inter-arrival spread is ±5 ms
  and MicroSIP's is ±0.3 ms: our clock is the sound card's 512-sample
  buffer against a 960-sample frame. A deadline-paced sender would
  narrow it, and that is a bigger change than this milestone.
* **The capture is not re-taken.** The marker bit is proved by the tests
  and by MicroSIP's stream in the same capture; a fresh capture of this
  app's own stream would be the confirmation, and it needs a capture
  tool on the machine.

---

## M4.14 — The echo test comes back

### What the user reported

> Some major bug with echo test in settings. After call with media —
> echo test stopped working.

The echo test is the microphone played back through the speaker: speak,
and hear yourself. It is the only test of the sound card that a user
can do alone, and after a call it stopped working — for the rest of
the session.

### The bug

The test refused to start when the sound card was busy:

```rust
if self.active_call().is_some() {
    self.echo_test = EchoTest::Failed("a call is up — the sound card is in use.".to_string());
    return;
}
```

`active_call()` is the call **the panel is showing** — the one the user
selected. It is set when a call is placed, and it is never cleared: the
call page keeps showing the call that is over, which is right. So after
the first call of the session `active_call()` was `Some(...)` for ever,
and the echo test refused for ever.

The sound card was free the whole time. The media is released *before*
the call-ended event is published — `session.forget_media(&call_id)`
runs first, and it closes the microphone and the speaker — so by the
time the app knows the call is over, the card has been given back.

### The test that kept it alive

There was a test for this: `the_echo_test_will_not_run_during_a_call`.
It used `run_a_call(&bus, "call-1", true)` — a helper that publishes a
call that starts, is answered **and ends**. The test asserted that the
echo test is refused while a call is up, and the call it built was not
up any more. It asserted the bug, and the bug survived every change
since.

The helpers now say what they mean: `run_a_call_that_stays_up` for a
call that is still there, `run_a_call` for one that is over.

### The fix

One predicate, and it asks the right question:

```rust
fn sound_card_is_busy(&self) -> bool {
    self.call_views.values().any(|view| !view.ended)
}
```

A call that has **ended** does not hold the sound card.

The test that catches it was written against the old behaviour and
failed with it: *"the sound card is still busy after the call ended"*.

### The other button

The **test sound** — the tone the settings page plays through the
speaker — had no rule at all, so it played **over a call**: a second
client on one endpoint, which is exactly what makes cpal report an
overrun and the audio break up. It now waits for the same reason.

### The dependency cost: nothing

No new crate.

### What is *not* in M4.14

* **The microphone gain is still not applied.** The config's
  `microphoneVolume` is stored and never used, so the microphone is
  played at unity whatever the setting says. That is the next
  milestone.
* **The echo test does not cancel the echo.** It is a loopback: the
  microphone is played back through the speaker, and what the
  microphone hears of the speaker is in the recording. Turning the
  volume down is the answer; cancelling it is not built.

---

## M4.15 — The volume controls, and the preamp

### What was asked for

> Can we add volume control to output and input (optional some kind of
> preamp)

And the finding that came with it: the microphone gain was **stored and
never used**.

### The knob that did nothing

The config has carried `microphoneVolume` since the beginning, and so
has the media state: `AudioState::mic_gain`, set from
`MediaConfig::microphone_gain`, read by **nobody**. The capture path
took the samples as they came and encoded them. The speaker's gain was
applied; the microphone's was not, so the setting was a number in a
file that changed nothing.

The C++ reference has the same gap — `microphone_volume` is written to
its config file and read by nobody there either. What the C++ *does*
have is a **live in-call volume slider** on the dialer's action bar
(`VolumeController`: a process-wide factor applied in the playback
callback), and that is what this build was missing.

So the input volume is applied now, and both directions have a knob.

### The preamp

A gain is a multiplier. Below 1.0 it turns the microphone down; above
1.0 it is a **preamp** — the microphone turned up, which is what a
quiet microphone needs.

The knob is in **decibels**: 0 dB is no change at all, and the scale is
the one every mixing desk and every telephone uses. A gain on its own
("×1.8") means nothing to a user. (The C++ maps its slider with x²
instead, which is a rough approximation of the same idea; decibels are
the scale itself, and they go both ways.)

| | the range | why |
|---|---|---|
| Microphone | −40 dB … **+24 dB** | a preamp: the voice louder than the room |
| Speaker | −40 dB … **+12 dB** | a headphone on a quiet machine is quiet |

The gain is applied to the **samples**, before they are encoded — where
a level is a level. G.722 has no silence code and G.711's silence code
is not the same thing as a quiet room; the same is true of a gain.

### Where the knobs are

* **The call page**: the two knobs, next to the mute buttons, where
  the C++ has its slider. They change the call that is up.
* **The settings page**: the volume every call *starts* with — and it
  takes effect at once on the call that is up, because a volume that
  has to be set again for every call is a volume nobody sets.

The settings are written to the file when the drag **ends**, not on
every step of it: the file is written on the thread that draws the
window, and a drag that writes a file sixty times a second is a drag
that stutters.

### Two numbers, not one

The microphone's own level and what we send are two numbers now:

* `outgoing_peak` — what the microphone picked up. It answers "did the
  microphone hear anything at all".
* `sent_peak` — what leaves the machine, after the gain. It answers
  "what does the peer hear".

The difference between them is the preamp, and the diagnosis asks the
right one for each question: *"The peer hears nothing: nothing is
leaving this machine"* is about the sent level, and *"The microphone is
picking up almost nothing — turn the microphone volume up"* is about
the microphone's own.

### The dependency cost: nothing

No new crate. egui has the slider, and the media crate already had the
gain — it was only never applied.

### What is *not* in M4.15

* **The ringtone and the ringback have no knob.** Their volumes are in
  the config and are not applied either.
* **The preamp turns the room up with the voice.** It is a gain, not a
  filter: it cannot separate a quiet voice from a quiet room.
* **The call page's knobs are not remembered.** The next call starts
  at the setting in the settings page.

---

## M4.16 — The call is said once

### What the log showed

The user's own log, read to see whether the sound card was free after
a call, said this:

```text
the call's media stopped received=199 sent=212 ... lost=0 underruns=0
the call's media stopped received=199 sent=212 ... lost=0 underruns=0
```

The same call, twice.

### Why

The media says what the call did when it **stops**, and the session is
**dropped** right after it stops — and the drop says it again, because
the drop is the safety net for a session that was never stopped (the
account is unregistered in the middle of a call, and the numbers still
have to be said).

Both were right on their own and wrong together: `stop()` always runs
before the drop, so every call was written twice.

### The fix

One flag on the session: the first of the two to speak says it, and
the other is silent. The safety net is still there — a session that
was never stopped still says its numbers when it goes.

The test reads the log the way the other logging test in this
repository does — a subscriber that keeps what it is given — and counts
the lines.

### The dependency cost: nothing

No new package. `tracing-subscriber` was already in the tree (the core
and the sip crate use it); naming it as a **dev**-dependency of the
media crate adds nothing to the build.

### What is *not* in M4.16

* **The log is not a report.** It is what the media saw, and it is
  said once, at the end. A call that is still up says nothing about
  itself until it is over.

---

## M5 — Transfer

### What happens

A transfer is a `REFER` (RFC 3515): the peer we are talking to is asked
to put a new leg up. There are two flavours, and they differ only in the
`Refer-To`:

```text
Blind:
  Refer-To: <sip:bob@target.example.com>

Attended (RFC 5589) — the `Replaces` names the dialog by all three of
its identifiers, so the peer can find it:
  Refer-To: <sip:bob@target.example.com;method=INVITE?Replaces=1a2b3c%40192.0.2.4%3Bto-tag%3Dpeer-tag%3Bfrom-tag%3Dour-tag>
```

The peer answers `202 Accepted` and then reports the progress of the new
leg over a `NOTIFY` (RFC 3515 §3.4). The progress arrives as a
`call_transfer_progress` event, and the call page says what is
happening.

There is a **third** method, and it is the one a hardware SIP phone
uses: the **PBX feature code**. The app types `*2<target>#` as DTMF and
the *server* does the transfer. It needs no `REFER`, no consultation and
nothing for the far side to match, so it works on a PBX that does not
honour a `REFER` at all — and it is the C++'s own third method.

### The `NOTIFY` is read by hand

rsipstack has a `ReferStatus::parse` for exactly this — and it does not
work. It looks for the `Event` and `Subscription-State` headers among the
**unknown** ones, and rsipstack's own parser knows both of them, so it
never finds them and the parse always comes back empty.

`sip::transfer::refer_progress` reads the typed headers the stack
actually produces: the `Event: refer` that says it is a transfer, the
`message/sipfrag` body that carries the referred call's status line, and
the `Subscription-State` that says whether the subscription is still
alive and why it ended.

Two rules in it are worth stating:

* **The last status line in the body is the progress.** RFC 3515 §3.4.1
  allows the body to carry several; a body with `100 Trying` and then
  `200 OK` is reporting progress, and the progress is the 200.
* **The body decides, and the reason does not.** `terminated;reason=noresource`
  does **not** mean the peer could not route to the target — it means the
  subscription is over. RFC 3515 §2.4.7 asks for exactly that on the
  final NOTIFY of a **successful** transfer, and pjsip's own state name
  for a terminated subscription is the literal string
  `terminated;reason=noresource`, so a server says it on every transfer
  that ends. The rules are:

  | | |
  |---|---|
  | `200 OK` in the body | the transfer worked |
  | `4xx` or `5xx` in the body | it failed |
  | the subscription ended and the body said anything else | it failed |
  | the subscription is still alive | it is in flight |

  Reading `noresource` as "no route to the target" — as the C++'s own
  comment does — reports **every successful transfer** as a failure,
  because that is the string every successful transfer ends with.

### The `Replaces` names the dialog by all three of its identifiers

The attended transfer names the dialog it is handing over, so the peer
can bridge to it. A dialog is named by **three** things (RFC 3891 §3.1):
its Call-ID, the tag the peer put on its `To`, and the tag we put on our
`From`.

§5.6's example is the Call-ID alone — `call-id%40host` — and it is kept
in the core and tested, but it **cannot** match a local leg. Asterisk
looks the dialog up with all three:

```c
pjsip_ua_find_dialog(&replaces->call_id, &replaces->to_tag, &replaces->from_tag, PJ_TRUE)
```

(`res_pjsip_refer.c`, `refer_incoming_attended_request`). With only a
Call-ID the lookup fails, and Asterisk falls through to the
`external_replaces` dialplan path — which is the user's error, verbatim:

```text
Received REFER for remote session on channel 'PJSIP/104-0000003e' from endpoint '104'
but 'external_replaces' extension not found in context test
```

So what goes on the wire is the tagged form:

```text
Refer-To: <sip:bob@target.example.com;method=INVITE?Replaces=1a2b3c%40192.0.2.4%3Bto-tag%3Dpeer-tag%3Bfrom-tag%3Dour-tag>
```

The `;` and the `=` are percent-encoded because the value is parsed as a
`Replaces` **header** once the escapes are undone — and pjsip sends the
same string (`pjsua_call_xfer_replaces`). The two tags look swapped
against what they are called: the **to-tag** is the *peer's* tag (our
remote tag) and the **from-tag** is *ours* (our local tag), because the
two ends of a dialog hold the same two tags the other way round.

The host is the one the Call-ID carries, not the host of our contact: the
peer matches the whole Call-ID it received.

### The Contact has to be the **public** address

Every in-dialog request — the REFER, the BYE, the DTMF `INFO` — carries a
`Contact`, and it has to name the address the peer can actually reach us
at. Behind a NAT that is not the address of our own socket.

rsipstack builds the dialog's own Contact from the transport's address
when the dialog is created without one, and on a machine with more than
one network interface that is a gamble. The user's own PBX showed the
result: `<sip:10.251.7.193:5060>` — the **VPN interface's** address,
which the peer cannot reach at all.

It is not a cosmetic difference. **Asterisk sends the transfer's NOTIFYs
to the Contact of the REFER** (RFC 3515 §3.4), so with a private
Contact the progress never arrives: the app never learns that the
transfer worked, never releases the original leg, and **the call never
ends**. The user's two reports — *"the call does not end after the
transfer"* and *"it picks some other address, it's my VPN interface"* —
were the same bug.

The registrar says where it sees us from — RFC 3581's `received` and
`rport` in the `Via` of its answer — and that is the address the
Contact has to carry. The dialog is created with it now, so every
in-dialog request carries it.

### The `Refer-To` names the **target**, not the server

The attended transfer's `Refer-To` names the third party the
consultation was placed to: the number the user dialled.

The dialog's own snapshot carries the peer's **Contact**, and when the
consultation went through a PBX that Contact is the *server's* address.
A `Refer-To` that names the server is a transfer to the server. pjsip
does not have the problem — it takes the consult call's remote URI,
which is the third party's address.

It matters for the fallback: when the peer cannot match the `Replaces`
— it does not know the dialog — it dials the `Refer-To` instead
(Asterisk's `external_replaces` path). With the target in it, that
fallback is a sensible blind transfer; with the server's address in it,
it is a transfer to the server.

### The original leg goes when the transfer works

The brief is explicit: *"Tear down the original leg on success."* The
user is otherwise still connected to the old peer while the banner says
"Transferred".

The release belongs to the **call layer**, not to the interface: the
`NOTIFY` that reports the new leg as answered is what ends the original
call, and the interface only draws what it is told.

The C++ does not do this. `CallController::onTransferStatus` composes the
banner string and nothing else, and its own comment says why it can
leave it alone — *"200 OK — new target answered; the original call will
be torn down by Asterisk imminently"*. A PBX that tears it down is a
PBX that agrees; a PBX that does not is exactly the case the user is
left connected to the old peer. This build hangs the leg up itself.

For the attended transfer the **consultation leg goes with it**: it is
our own leg, and a leg left up holds the microphone for a call the user
has already left.

### A failed transfer keeps the call up

A transfer that fails — the target cannot be reached, the peer refuses
the `REFER` — leaves the original call alone. The user may want to try
again, and a call that no longer exists cannot be retried. This is the
choice the brief offers, and it is the one this crate makes.

### The banner is the C++'s wording

`CallController::onTransferStatus` composes one string, and it is
composed the same way here:

| | |
|---|---|
| 100 | "Transferring… new target trying" |
| 180, 183 | "Transferring… new target ringing" |
| 200 | "Transferring… new target answered" |
| 400…699 | "Transfer failed: `<code>` (`<reason>`)" — and the reason is left out when it is `noresource` |
| a failure that never got an answer | "Transfer failed: no route to target" |
| anything else | "Transfer ended (`<reason>`)", or "Transferring…" |

**One thing is not the C++'s: the progress decides, and `noresource` is
not shown.** The C++ reads `noresource` as *"the subscription ended
without a resource being created (Asterisk couldn't route to the
target)"* and puts it in the banner — so a failed transfer reads
"Transfer failed: 486 (noresource)", which explains nothing, because
`noresource` is on **every** terminating subscription.

The C++ checks the reason *last*, so its successes are right. M5 checked
it **first**, and every successful transfer was reported as "no route to
target". The banner now takes the **progress** and the **code**:

* the progress says whether it failed;
* the code says how far it got;
* the reason is shown only when it is not `noresource`.

### The interface

The **Transfer** control is on the call page, under the mute buttons —
the call the `REFER` goes out from. It opens a **popup**, the way the
C++ has it (`src/ui/Dialer.qml`, a `Popup` with `modal: true`).

It is a popup because the transfer is three steps and the button that
finishes it must not be somewhere the user can lose. The first attempt
put the controls inline on the call row, and it failed twice over: the
row sits at the bottom of the page, and the moment the consultation
dialled it became the active call — so the controls followed the
*active* call and vanished. The user was left with two calls on the
screen and no way to finish the transfer.

The popup has the C++'s two phases:

* **input** — "Transfer call to", a number, and **Blind transfer** or
  **Attended transfer**;
* **consult** — "Consulting — complete transfer", a sentence saying
  where the consultation is ("Dialing…", "Ringing the target…",
  "Consultation answered — speak to the target, then complete the
  transfer.", "Consultation ended."), and **Complete transfer** /
  **Cancel consultation**.

The **Complete transfer** button is always there — greyed out until the
target **rings**, with the sentence above saying why. A button that is
not there is a button the user cannot find; a button that is there and
greyed out, under a sentence that says why, is one they can. The C++
greys it out until the target **answers**; M6.5 changed that, and
[the reason](#m65--the-transfer-completed-while-the-target-is-still-ringing)
is in RFC 3891 §3.1.

Escape closes the popup, but not while a consultation is in flight:
closing it then would leave the consultation leg up with nobody able
to reach it. This is the C++'s rule (`closePolicy: Popup.NoAutoClose`
in consult mode).

While the consultation is up the original call's microphone is muted: the
peer must not hear the conversation with the third party. That is a
**local mute, not a SIP hold** — the peer is not told, and it has no way
of knowing a hold happened.

### The third method: the PBX feature code

The popup offers a third button, **PBX feature code**, and it is the one
a hardware SIP phone uses. The phone's transfer key sends the PBX's
code, the user dials the target, and the phone sends the terminator —
the PBX does the transfer and the phone only types.

That is `*2<target>#` on Asterisk (`*3` for the attended flavour), sent
as DTMF over `INFO`, one digit at a time with a pause between them,
because the server matches the code as a dialplan pattern and needs to
see the digits arrive as a sequence rather than as one burst.

The prefix is editable in the popup, because it is the **server's**
setting and not this build's: Asterisk's `*2` is a dialplan extension,
and what it does depends on the dialplan. The default is `*2` — the
blind transfer code.

This method needs no `REFER`, no consultation and no `Replaces` for the
far side to match, so it gets a transfer through a PBX that does not
honour a `REFER` at all — and the C++ has the same third method, for
the same reason.

What it cannot do is report progress: the app types the code and the
PBX takes the call from there. There is no `NOTIFY` to read, so the
banner has nothing to say about how the transfer is going.

It is also the one the brief calls out by name (§5.9): *"A PBX
feature-code transfer (`*2<target>#`) must send the whole string as raw
DTMF and must not be re-intercepted by the configured `*2` rule."* The
string is sent whole, and it is sent from the app's DTMF path rather
than from the dialpad — so the configurable `code → action` rules of
§5.9, when they land in M6, have to be careful about where they
intercept.

### The page does not jump

The user's second report: the transfer field jumped about when the
microphone level changed.

The cause was the **diagnosis** — the sentence under the meters that
says what is wrong with the audio ("The peer is sending silence…"). It
appears and disappears as the levels move, and a line that changes the
height of the page moves every button under it. The field the user was
typing into was one of the things that moved.

The diagnosis now has a space of its own that does not change size,
and the text is clipped to it — a sentence too long for its space says
the rest on hover. The rule is general: **a control must not move
while the user is using it**, and a live reading must never decide how
much room the page takes.

### The dependency cost: nothing

No new package.

### What is *not* in M5

* **A SIP hold is not implemented.** The original call is muted locally
  during an attended transfer, not put on hold with a re-INVITE. The
  peer sees a silent call, not a call on hold.
* **A transfer we are *given* is declined.** A peer that sends us a
  `REFER` gets `501 Not Implemented`: this build does not take part in a
  transfer it is handed, and saying so is the honest answer.
* **The `Replaces` on the wire is the tagged form.** §5.6's Call-ID-only
  form is still in the core and still tested, but it is not what goes
  out: a PBX that matches a dialog by its tags — which is what Asterisk
  does — cannot find a dialog by its Call-ID alone.
* **The feature code is the server's, not this build's.** The app types
  `*2<target>#` and the PBX does what `*2` does. If the dialplan has no
  `*2` in it, nothing happens, and nothing here can tell the user why.
* **The consultation is not bridged by us.** The peer bridges to it —
  that is what the `Replaces` means.

---

## M6 — The sounds the softphone makes for itself, and the call that is redirected

### What happens

Three things, asked for together:

1. **A call that comes in rings.** The softphone plays a ringtone of its
   own while the call is ringing — the C++'s `RingtonePlayer`. The ringtone
   is picked in the settings: a WAV file of the user's, or the built-in
   tone.
2. **A call we made rings back.** When the phone we called answers with a
   `180 Ringing` that carries no SDP, there is nothing to hear, so the
   softphone makes its own ringback — the C++'s `RingbackPlayer`.
3. **A call that is still ringing can be redirected.** The incoming-call
   screen has a **Redirect** button: it answers the `INVITE` with a `302
   Moved Temporarily` and a `Contact` naming the target. The call is never
   answered — this is forwarding without answering.

### The ringtone is generated, not shipped

The C++ ships `src/ui/assets/ringtone.wav`: 8 kHz mono 16-bit, six seconds
long, two seconds of tone and four of silence. This build **makes** that
sound instead of shipping it: 440 Hz and 480 Hz together, two seconds on and
four off, with a 20 ms fade at each end so it does not click.

That is a deliberate trade. A binary asset has to be built, copied next to
the executable and found at run time, and a softphone that cannot find its
ringtone file rings silently — the one failure a user cannot forgive. The
generated tone is the same shape of sound and it is always there.

A WAV file **is** honoured when the settings name one: `audio.ringtone_file`
is read as 16-bit PCM, mono or stereo (a stereo file is mixed down to
mono), and resampled to the device's own rate. A file that cannot be read
is not a reason to be silent: the built-in tone plays instead, and the log
says why.

### The ringback plays only when there is nothing to hear

The ringback is a single 440 Hz tone, a second in every four — the C++'s
`RingbackPlayer`, which synthesises it with pjmedia's tone generator rather
than reading a file.

It plays on **one** condition: the peer answered our `INVITE` with a `180
Ringing` (or `183 Session Progress`) that carried **no SDP**. That is the
ordinary ringback from a PBX, and there is no RTP in it — nothing is being
sent to the speaker, so the softphone makes the sound itself.

If the `180` **does** carry an SDP, the peer is already sending audio, and
the softphone keeps quiet: a ringback on top of the peer's own ringtone is
two rings at once. The fact travels on the event as `has_early_media` —
the SIP layer knows what the response carried, and the app, which owns the
sound, decides what to do about it.

> **Early media itself is not implemented.** The media still starts at the
> `200 OK`, so a `180` **with** an SDP gives silence until the call is
> answered. That is a separate piece of work, and the ringback does not
> pretend otherwise: it stays quiet exactly when there *is* media to come.

### The redirect is a `302`, and it is called "Redirect"

RFC 3261 §21.4's `302 Moved Temporarily` is the forwarding answer: the
caller is told to try somewhere else, and where is in the `Contact`.

The button is called **Redirect** and not **Transfer** on purpose. The call
page already has a button called *Transfer*, and it does something else
entirely — it sends a `REFER` and asks the peer to put a new leg up, with
the far end on hold until the new leg answers. Two buttons with the same
name and different meanings are a trap, and the names are also the
difference between "I am moving this call" and "I am not taking this call".

The `302` goes out through the **cloned transaction sender**, because
rsipstack's `InviteDialog::reject` cannot add a `Contact` — and a `302`
without a `Contact` is a `302` that says nothing. The response is built by
hand from the request (the `Via`, the `From`, the `To`, the `Call-ID` and
the `CSeq` are copied) and the `Contact` is pushed onto it.

The target is written the way the app writes every other number: `200`
becomes `sip:200@<the registrar>`, so the caller is sent to the same place
the app would have dialled.

### The sound is played on a thread of its own

`SoundPlayer` lives in the media crate. It opens the speaker itself, on a
thread of its own, and the interface never waits for it (§6): `play()`
returns at once, `stop()` sets a flag and does not join, and the *next*
play joins the previous thread on its own thread.

The C++ mixes the ringtone into the same conference bridge the call uses.
This build cannot: the media crate opens a **pair** of devices per call, and
a second stream on the same Windows device is the shared-mode case — it
works, and the two streams are mixed by the audio engine.

The sound belongs to a **call**: the ringtone stops when that call is
answered, and the ringback when it is. Another call ending does not silence
it.

### The 60-second watchdog

The C++'s two players both have it, and it is worth keeping: pjsip 2.14 has
a path where the call is torn down so fast that the disconnected state
never reaches the UI, and without the cap the tone would play for ever.
This build has no such path — but a call that rings for ever is still a
fault, so the player caps every sound at a minute.

### The settings are global, as they are in the C++

The ringtone file and the two volumes live in the `audio` section of the
config file, which is **app-level**. A per-account ringtone would need a
column on the account and a migration, and the C++ does not have one
either — so the first version is the one that matches the reference.

The defaults are the ones the C++ uses, except the inbound ringtone's
volume: `ringtone_volume` is **0.9** here and 0.7 there, and the backlog
said to align it when the player landed. The ringback is 0.7 in both.

### The settings page scrolls now

The settings page grew a section, and it was already close to the bottom of
the window: the echo test — the thing that answers "is my microphone
working?" — was falling off the bottom and could not be reached. The page
is now in a scroll area, and the ringtone section was made compact enough
that everything fits in the default window anyway.

### The dependency cost: nothing

No new package. The tones are made with `f32` maths, the WAV is read with
`std::fs`, and the player is `cpal` — which M4 already brought in.

### What is *not* in M6

* **Early media is not implemented.** The media starts at the `200 OK`, so a
  `180` with an SDP is silent until the call is answered.
* **The redirect is not a `SIP 302` with a `Reason` header**, and it does
  not follow the call after it: the app marks the call as redirected and
  stops watching it.
* **The sound is not mixed into the call.** It is a second stream on the
  same device, not the same conference bridge.
* **M6's other items are not started**: the accounts UI, the transport
  picker, the SIP domain and the outbound proxy. This milestone is the
  three things that were asked for; the rest of M6 is still M6.

---

## M6.2 — The ringtone per account, the call the panel shows, and the held leg

Three things, all reported after M6.1 went out.

### The ringtone is per account now

The user asked for it: *"I meant to play some audio to inbound call.
That the user can separate account with audio."* A softphone with two
accounts wants two rings — the ring is how you know **which** line is
calling you.

So the ringtone file is a column on the account (`ringtone_file`), with
a field in the account editor. The lookup is: **the account's own file
first**, then the app's own file (`audio.ringtoneFile` in the settings),
then the built-in tone. An account with nothing set rings exactly like
the app does.

The column was added after the first release, so it comes with the
migration that brings an older database up to it — `ALTER TABLE accounts
ADD COLUMN ringtone_file TEXT NOT NULL DEFAULT ''`. The default matters:
the accounts that are already there get an **empty** ringtone, not a
null one, and they survive the migration with their rows intact.

### The panel follows the call that is up

*"The user must click to the call to have active screen of the call"* and
*"dialpad input ignored in main screen"*.

They were the same bug. The panel showed **one** call, and the app never
let go of it: the active call was only set when there was **no** active
call, and it was never cleared when a call ended. So after the first call
of the day, the panel stayed on that dead call — and the keypad, which
sends its DTMF to the active call, thought the app was idle and dialled
a number instead of sending a tone.

Three rules now, and they are the C++'s in spirit — the C++ has a window
per call, so it never has this problem:

* **A call that has ended gives the panel up.** The panel moves to the
  newest call that is still up.
* **A call that is answered takes the panel.** It is the call the user is
  talking to, and the keypad has to send its tones there.
* **A call the user picked in the list keeps the panel**, until that call
  ends. The pick is theirs, and a call being answered somewhere else
  does not overrule it.

Hanging up a call in the list no longer takes the panel to it either:
the user did not ask to look at the call they are hanging up.

### The attended transfer holds the original leg

*"I can hear the called line and the ring tone in the same time. Normal
is calling leg went hold."*

The original leg was only **locally muted**: the microphone was muted,
so the peer could not hear the conversation with the third party — but
the speaker was not, so the user heard the other party **and** the
consultation's ringback at the same time.

Now the original leg goes on a **real hold**, as the C++ does
(`CallController::holdCall` before the consultation is dialled,
`unholdCall` when the transfer is called off):

* A hold is a **re-INVITE** whose SDP says `a=sendonly` (RFC 3264 §5):
  "I will send, I will not receive". The peer answers `a=recvonly` and
  stops sending, so the far end goes quiet.
* The re-offer names **the codec that was negotiated and nothing else**.
  A hold must not change the codec under the call.
* The speaker is muted **at once**, before the re-INVITE goes out: the
  peer takes a moment to stop, and a user who is on a consultation
  should not hear the other leg in the meantime. If the peer refuses
  the hold, the speaker comes back — a silent user with a live peer is
  worse than a loud one.
* When the transfer is called off, the original leg comes off hold and
  gets its microphone back.

The transfer popup says **"Original call on hold — the other party cannot
hear you"**, because a leg that goes quiet without saying why looks like
a fault.

### The dependency cost: nothing

No new package: a column, a migration, a re-INVITE that rsipstack
already knows how to send, and a flag that was already there.

### What is *not* in M6.2

* **There is no file picker.** The ringtone file is typed, as every
  other path in this app is.
* **The held leg is not unheld when the transfer completes.** The
  original leg is released then, so there is nothing to unhold.
* **The peer is not told that the call is on hold in words.** It is
  told in the SDP, which is how a hold is told.

---

## M6.3 — The consultation that dies

*"Case atd transfer with refer — if transferee dest does cancel the call with
cancel or other error code, complete transfer and cancel consultation still
presented. If user hit complete transfer that hanging up the call."*

### What the log showed

The user's own log (`qsoftphone.log.2026-11-09`, 21:25) has the whole
story in eleven lines:

```
21:25:39.683  INVITE sip:105@193.178.169.164:5161   <- the consultation
21:25:39.935  200 OK                                 <- the target answered
21:25:43.191  REFER sip:asterisk@193.178.169.164:5161   <- "Complete transfer"
21:25:43.442  202 Accepted
21:25:43.444  NOTIFY  Subscription-State: active
21:25:43.444  NOTIFY  Subscription-State: terminated;reason=noresource
21:25:43.446  BYE  (the consultation)
21:25:43.447  BYE  (the original call)               <- ours
```

Three things were wrong, and one more turned up on the way.

### The popup did not follow the consultation

The transferee destination can end the consultation itself: it hangs up,
it cancels, or the call fails with an error code. The app noticed none of
it — the transfer popup went on offering **"Complete transfer"** and
**"Cancel consultation"** for a call that was already gone.

The popup now **follows the consultation**. When the consultation ends:

* the popup closes,
* the original leg comes **off hold** — the hold was ours, put there
  for the consultation, and the user is talking to that peer again,
* the status line says **"The consultation ended — the transfer was
  called off"**.

That is the C++'s rule, in `Dialer.qml`: the popup polls the
consultation call's state every tick, and when that call dies it calls
`_cleanupConsult()` — unhold the source, clear the consult state, close
the popup — and the transfer-status banner takes over for the user.

The popup also refuses to offer the buttons for a consultation that is
over. A button that cannot work is a button that must not be there:
pressing it would send a REFER for a call that is gone, and §5.6 takes
the original leg down with it.

### "Complete transfer" on a dead consultation hung the call up

The REFER for a consultation that is over is a transfer that cannot
work. The peer answers it with a failure, and §5.6 releases the original
leg when the NOTIFY reports the transfer as done — so the user lost the
call they were in.

Two guards now:

* the **button** is only there for a consultation that is alive and
  answered, and
* the **action** refuses a consultation that is not up:
  *"The consultation is over — there is nothing to transfer."* Nothing
  goes on the wire.

### A transfer that failed puts the original call back on the line

The policy in this README already says that a failed transfer keeps the
original call up, so the user can retry. But the leg was still **on
hold** — the hold belonged to the consultation, and the consultation is
over. The hold comes off now, and the microphone comes back with it.

### The `Replaces` was in the wrong place

The log's `noresource` is the server saying **"no route to target"**:
it could not find the consultation dialog, so it treated the transfer as
one to another machine.

The dialog was named as a **URI parameter** inside the `Refer-To`:

```
Refer-To: <sip:105@193.178.169.164:5161;method=INVITE?Replaces=5ed9369c...>
```

That is the form RFC 3515 §2.1 shows — and the form a server that
reads the URI parameter finds. Asterisk reads the **`Replaces` header**
(`ast_sip_get_replaces()`) and does not look at the URI at all, so it
never saw the dialog. The C++ reference is pjsip's `Call::xferReplaces`,
which builds a **header**.

So the header goes out as well, with the same three identifiers and
unescaped (a header is not a URI parameter):

```
Replaces: 5ed9369c7ccc4c29913befe04e089e74@restsend.com;to-tag=35583397-...;from-tag=CLDOGrFp>
```

Both forms name the same dialog, so a server that reads either one
finds it.

### The dependency cost: nothing

No new package: a header that rsipstack already knows how to carry as
an unknown header, a flag on the transfer state, and the rules that were
already in the C++.

### What is *not* in M6.3

* **The CANCEL is not answered.** rsipstack 0.7.9 drops an incoming
  CANCEL for a call that is not up yet — the client transaction is not
  confirmed, and the stack only replies to a CANCEL on a confirmed
  server dialog. The `487` that follows it is what ends the call, and
  the app hears that; answering the CANCEL would need a patch to the
  stack.
* **The consultation is not dialled again after a failure.** The user
  presses "Complete transfer" again, or calls the target themselves.
* **A blind transfer is not affected.** There is no consultation and no
  hold in that case, so there is nothing to take back.

---

## M6.4 — The file the user picks

*"Ok. Looks nice. Let's add some interface to obtain ringtone audio
files like browse."*

### What the C++ has

`src/ui/SettingsWindow.qml` has, next to the ringtone field, a
**"Browse…"** button and a **"Reset"** button, and a QML `FileDialog`:

```qml
FileDialog {
    id: fileDialog
    title: qsTr("Pick a ringtone WAV file")
    nameFilters: [ qsTr("WAV files (*.wav)"), qsTr("All files (*.*)") ]
    fileMode: FileDialog.OpenFile
}
```

Until now the ringtone file could only be **typed** into a field, and a
path that is typed wrong is a path that does not ring. The C++ lets
the user *pick* the file, and that is what is missing here.

### What there is now

**Browse…** and **Reset**, on both places a ringtone file lives:

| Where | The file | Browse… | Reset |
|---|---|---|---|
| The settings page | the app's own ringtone | yes | yes — back to the built-in tone |
| The account editor | one account's ringtone | yes | yes — back to the app's ringtone |

The dialog is the operating system's own window, with the C++'s title
(*"Pick a ringtone WAV file"*) and its two filters — **WAV files**
(`*.wav`) and **All files** (`*.*`), so a ringtone that is called
something else can still be picked.

### The dialog is on a thread of its own

A file dialog stays open until the user has answered it. Opening it
on the interface thread would freeze the window for as long as the
user takes to look at it, so the dialog is opened on a **worker
thread** and what it says arrives on a channel, which the app drains
at the top of the frame — the same pattern as the registration state,
the call events and the media (§3: the interface never waits).

That is not a workaround around `rfd`; it is what `rfd` does itself
for its own asynchronous API, and on Windows it has to be that way:
the COM apartment the dialog lives in is per **thread**, and `rfd`
initialises COM (`CoInitializeEx(COINIT_APARTMENTTHREADED)`) on
whichever thread opens the dialog.

The window keeps running while the dialog is up. The **Browse…**
button is greyed out until the dialog has been answered, so there is
never more than one of them.

### A picked file has to be a file the softphone can play

The "All files" filter is an escape hatch, and it lets the user pick
something that is not a ringtone at all. The file is therefore read
**before** it is accepted, with the same reader the ringtone itself
uses (`qsoftphone_media::read_wav` — 16-bit PCM, mono or stereo, and
a stereo file is mixed down to mono). A file that cannot be played is
refused, and the status line says why:

> That file cannot be used as a ringtone: C:\sounds\notes.txt is not
> a WAV file

The check is on the worker thread, with the reading, so a big file
does not stall the window. The point is that the ringtone setting can
never become a file that will not ring: the built-in tone is the
fallback, and a ringtone that silently does not ring is the one
failure a user cannot forgive.

### An account's ringtone is an editor field

A picked file for the **app's** ringtone is written to the settings
file at once. A picked file for an **account's** ringtone goes into the
editor, like every other field of it — the account is saved when the
user presses "Save", and the status line says so:

> Picked C:\sounds\office.wav. Save the account to keep it.

The dialog is modal to the *user*, not to the app: the window keeps
running while it is up, and the user can pick another account behind
it. So the answer carries **which account** it was for, and a file
that comes back for an account the user has since left is refused:

> The account changed while the dialog was open — pick the file again.

### The dependency cost: one package

`rfd` — the operating system's own file dialog. **One** package name
on each target (Windows 309 → 310, Linux 389 → 390), two lock
entries. The full accounting, and why it is 0.17 and not 0.18, are in
[the dependency budget](#the-dependency-budget) above.

### What is *not* in M6.4

* **The dialog is not parented to the app's window.** `eframe` keeps
  the raw window handle private (`raw_window_handle` is `pub(crate)`
  in `epi.rs`), so there is no handle to give `rfd`. The dialog is a
  window of its own: it comes to the front, because a process that
  owns the foreground may always bring its own windows forward, but it
  is not modal to the app's window — the app keeps running.
* **macOS has no "All files" filter.** The macOS panel merges every
  filter into one flat list of allowed types, and there a `*` is not a
  wildcard but a literal extension. The WAV filter stands on its own
  there.
* **The Linux and macOS builds are unverified from here.** This
  machine is Windows: the portal backend on Linux and `NSOpenPanel` on
  macOS are the upstream code paths, but they have not been run.
* **The dialog does not remember the last folder.** It opens in the
  folder the current ringtone is in, and in the system's own default
  place otherwise.

---

## M6.5 — The transfer completed while the target is still ringing

*"Can we add the user option to complete atd transfer with refer before
answer. User hears ringtone and complete transfer giving completion on pbx
logic."*

### What the C++ does

The C++ offers **Complete transfer** only once the consultation has
**answered**. `src/ui/Dialer.qml` puts the condition in the button's
`enabled`:

```qml
enabled: transferPopup._inConsultMode
    ? (transferPopup._consultState === "answered"
       && transferPopup._sourceCallId > 0)
    : (transferField.text.trim().length > 0
       && root._callId > 0)
```

So in the C++ the user **has to wait for the target to pick up** before the
button comes alive, and the transfer is a REFER that the peer executes
against a dialog that is already up.

### What there is now

The button comes alive as soon as the target **rings**, and what the user
is told is that the server will finish it:

> The transfer can be completed now — the server will connect you when
> they answer.

That is the whole of the user's request: hear the target ring, press
**Complete transfer**, and let the PBX do the rest.

This is a **deliberate difference from the C++**, and the reason is in the
RFCs rather than in taste.

### Why ringing is enough, and dialling is not

The attended transfer names the dialog it is handing over, so the peer can
bridge to it. A dialog is named by **three** things (RFC 3891 §3.1): its
Call-ID, the tag the peer put on its `To`, and the tag we put on our
`From`.

The peer's tag arrives with the **first response that carries one** — the
`180 Ringing`. So:

| The consultation is | The dialog is named | The transfer can be completed |
|---|---|---|
| dialling | no — no tag yet | **no** |
| ringing | yes | **yes** |
| answered | yes | yes |
| ended | — | no — there is nothing to transfer |

Before the target rings there is **no dialog to point at**. A `Replaces`
without the to-tag names nothing, the server cannot find the call, and it
falls through to the "no route to target" path — the fault M6.3 was
about, and the reason that milestone put the `Replaces` header on the
wire.

So the rule is: **the target has to ring, and it does not have to
answer.**

The rule is enforced twice, on purpose:

* the **app** decides by the state of the consultation — ringing or
  answered means the button is live — and says why when it is not;
* the **SIP layer** checks the dialog itself and refuses with
  `ConsultationNotNamed` ("the consultation has not been named by the
  server yet") rather than sending a REFER that cannot work.

The second check is the backstop. The app's rule is a rule about the
*state*; the SIP layer's rule is a rule about the *dialog*, and the
dialog is the thing the server has to be able to find.

### After the REFER has gone out, the server owns the transfer

This is the part that changed the app's behaviour, and it is the part
that matters most.

Before the REFER goes out, the transfer is **ours**: the consultation is
up, the original leg is on hold, and if the consultation dies the user
is talking to the old peer again.

After the REFER goes out, the transfer is the **server's**. It has been
told to take the consultation over; it holds the transfer, and it reports
how that went over the NOTIFY (RFC 3515 §3.4). The consultation dying is
then what the server does when it bridges — or what the target does when
it gives up — and either way the **server** is the one who says how it
went.

So the app now says:

> The consultation ended — the server has the transfer and will say how it
> went.

and **the hold stays on** until the server says so. Taking it off now
would put the user's voice back on the old peer's line while the server
is bridging. The hold still comes off when the transfer **fails** — that
is the policy that keeps the original call up (§5.6), and it is unchanged.

### The dependency cost: nothing

No new package: a flag on the transfer state, a check on the dialog, and
a rule the RFCs already imply.

### What is *not* in M6.5

* **The user's Asterisk never rings.** It answers `100 Trying` and then
  `200 OK` — it never sends a `180 Ringing`, so on that PBX the target
  goes from *dialling* straight to *answered* and there is no window in
  which the button could be pressed early. The feature is proved against
  the peer in this repository, which holds the call in the early state;
  it has **not** been exercised against a real PBX that rings.
* **The transfer is still the server's to complete.** Nothing here waits
  for the target to answer and then sends a second REFER — the whole
  point is that one REFER is enough, and the server bridges when the
  target picks up.
* **The C++'s rule is not changed.** The reference still waits for the
  answer; this build deliberately does not, and the difference is
  recorded above.

---

## Policy decisions

Taken from the brief, recorded here so they are not re-litigated:

* **A rejected transfer is reported, never swallowed.** If the server
  rejects the REFER, the error is shown and the call is *not* left
  running as if the transfer had succeeded.
* **A failed transfer keeps the original call up.** §5.6 offers two
  options — keep the original call so the user can retry, or release
  it. This crate **keeps it up**. Releasing on failure loses a call
  that was still good, and the user cannot retry a call that no
  longer exists. The original leg *is* released on success, because
  that is what "transferred" means (§5.6, and the bug the C++ had).
* **Session recording and the CRM webhook endpoint are opt-in per
  account.** Neither is enabled by default.
* **The REST API is bearer-token authenticated**, and the admin bind
  address defaults to localhost.

---

## Milestones

| | Scope | State |
|---|---|---|
| **M0** | Workspace, pinned dependencies, tracing, config load/save, account CRUD in SQLite, the SIP pure functions with their tests | **done** |
| **M1** | Registration: rsipstack transport, digest auth (MD5 + SHA-256), register/unregister, registration state surfaced to the UI | **done** |
| **M2** | Calling: outbound INVITE, inbound INVITE with automatic 180 Ringing (§5.4), answer, decline, hang up without a forced code (§5.5), DTMF | **done** |
| **M3** | History: SQLite persistence with the §5.3 semantics, redial using the resolved dialled number (§5.1), searchable list in the UI | **done** |
| M4 | Media: `rtc` RTP + `cpal` audio — SDP negotiation, send/receive audio, mute, codec selection | **done** |
| **M4.1** | Codecs & the sound card: G.722 (wideband), the codec list on a settings page, the device pickers, the test sound, the redial of an incoming call | **done** |
| **M4.2** | The jitter buffer: the playout waits for the buffer to fill, and a lost frame is concealed | **done** |
| **M4.3** | The sound card's channels: one value per frame, not per slot — the audio was being played at double speed | **done** |
| **M4.4** | The pitch wobble: the playout no longer trims its rate to chase the peer's clock — that *was* the dithering | **done** |
| **M4.5** | The stale call and the real-time callback: unregistering ends the call and closes the sound card, and the callback no longer takes a lock | **done** |
| **M4.6** | The history row, the legend, and the remote session: the redial is in the row, the rows have a key, and a call with no microphone still sends | **done** |
| **M4.7** | The history as a table, the dialpad, and the timestamps: the history has a header row, the timestamps carry the seconds, the status line goes back to "Ready.", and the dialpad is in the header of the call page | **done** |
| **M4.8** | The keypad in the middle, the window that wakes up, and the echo test: the dialpad is centred in the call page, the app asks egui to redraw ten times a second, and the microphone is played back through the speaker with the device names in the report | **done** |
| **M4.9** | The microphone tells the truth, and the calls are the ones that are up: the echo test has a level meter and says the level in decibels, a call says how many frames it sent, and the call page lists only the calls that are up | **done** |
| **M4.10** | The details are under the row they belong to: the history table is drawn by hand with fixed columns, an expanded row opens its details **under itself** instead of below the whole table, and the table fits the window | **done** |
| **M4.11** | Where the silence is: the call row carries a live peer meter, the three levels in decibels, and a sentence that names the place the silence is in; the media path is measured end to end against the real PBX | **done** |
| **M4.12** | The codec the peer chose, and the frames that never came: our own codec preference decides the session, the jitter buffer holds a peer's bursts, the frames that never arrived are counted and said out loud, and the clock runs at fifty frames a second even with no microphone | **done** |
| **M4.13** | The two captures, side by side: the user's own pcap, read packet by packet, against MicroSIP's call in the same file — the loss was the environment, the echo is faithful, and the one difference left was the marker bit, which is now set only where a talkspurt starts | **done** |
| **M4.14** | The echo test comes back: the sound card is busy while a call is **up**, not while one is selected — so the test works again after a call, and the test sound waits for the card instead of playing over a call | **done** |
| **M4.15** | The volume controls, and the preamp: the microphone gain is applied for the first time, it goes above unity, and both directions have a knob — live on the call page, and as the default in the settings | **done** |
| **M4.16** | The call is said once: the media logged what the call did when it stopped *and* again when the session was dropped, so every call was in the log twice | **done** |
| **M5** | Transfer: blind and attended REFER per §5.6, a `Replaces` that names the consultation by all three of its identifiers, the NOTIFY progress over RFC 3515 §3.4, the original leg released when the new leg is answered, and the PBX feature code | **done** |
| **M6.1** | The sounds and the redirect: a ringtone on an incoming call with a file and a volume in the settings, a generated ringback on a 180 without SDP, and a 302 Moved Temporarily from the incoming-call screen | **done** |
| **M6.2** | The ringtone per account, the call the panel shows, and the hold: a ringtone file on the account, the panel following the call that is up, and the attended transfer putting the original leg on hold | **done** |
| **M6.3** | The consultation that dies: the transfer popup following the consultation, a dead consultation offering no transfer, a failed transfer putting the original call back on the line, and the `Replaces` header the server reads | **done** |
| **M6.4** | The file the user picks: a native **Browse…** dialog for the ringtone, on the settings page and on the account editor, with the WAV filter, the **Reset** button, and a file that is checked before it is accepted | **done** |
| **M6.5** | The transfer completed while the target is ringing: **Complete transfer** is live as soon as the target rings, the server finishes the transfer, and a consultation that dies after the REFER is the server's business rather than the user calling it off | **done** |
| M6 | Settings & polish: accounts UI, transport selection, SIP domain, outbound proxy, ringtone, the feature codes of §5.9 | the ringtone is done in M6.1 and M6.2 |

One milestone at a time; M3 does not start before M2 works end to end.

### Backlog — low priority

Wanted, but not next. Each item says which milestone it lands in, so
none of them is lost and none of them disturbs the order above.

| Item | Lands in | Notes |
|---|---|---|
| **One sound card for the whole application** | M6 | The C++ had **one** microphone and one speaker for the application; this build opens a pair **per call**, so two concurrent calls put two clients on the same device — which is what the Windows audio engine flags with a discontinuity. M4.5 fixed the stale call (a call whose account was unregistered); what is left is the genuine two-calls-at-once case. The fix is to open the devices once and share them, which means the media plane stops belonging to a call and belongs to the application. |
| **A lock-free hand-off to the sound card** | M6 | The playback callback now takes the shared queue **once per callback** and works on its own copy — no lock in the per-sample path. The capture side still takes one lock per 20 ms frame to hand the encoded frame to the sender, which is far better and still a lock in a real-time thread. The proper answer is a single-producer/single-consumer ring buffer with no lock at all. |
| **A timestamp-driven playout** | M5 | The proper way to absorb the difference between the peer's clock and ours without touching the pitch. Each frame is played at the time its **RTP timestamp** says, and a clock difference is absorbed by dropping or repeating a sample at the **boundary** of a frame rather than by changing the rate of the whole stream. This is what pjmedia's jitter buffer does (`pjmedia_jbuf` is frame-indexed and clock-driven). M4.4 removed the rate correction that was doing it wrongly; this replaces it properly, and it is what makes a call hold up for hours rather than minutes. |
| **Codec selection per account** | M6 | The codec list is now on the settings page and is **app-level**: the order is the offer's order, and it is kept in the settings file. What is still missing is that it should be **per account** — one account may only speak PCMU, another only PCMA. `AccountProfile` has no codec field yet, and the account editor has no control for one. |
| **Media device selection, in the main settings** | **done in M4.1** | The picker is on the settings page: the microphone and the speaker, with "System default" and the devices of the machine. The device list is read on the runtime, not on the interface thread, and the name is persisted in the settings file — the fields were already there (`media.microphone`, `media.speaker`) and the media crate reads them. |
| **Opus, as a third codec** | M6 | The next codec after G.722. It needs a pure-Rust encoder (`opus-rs`, which brings `fearless_simd` and `libm` with it), the RFC 7587 RTP framing, and — the hard part — a **third-party `codec_opus` module on the PBX**: Asterisk does not ship one, so the PBX has to be built with it before a single Opus call is possible. |
| **Expandable history, with the call time and the SIP code** | **done in M3** | The **data** was already there: the `calls` table has `duration_sec`, `sip_code` and `hangup_cause`. The UI was the missing part, and M3 has it: the history page's row shows the number, the duration, the time and the SIP status code, and expands to the full URI, the cause and the exact timestamps. |
| **NAT traversal for the media plane** | M5 | The signalling side already does what it can: the public `Contact` is learned from the `received`/`rport` the registrar echoes (RFC 3581), the binding is refreshed before it expires, and Asterisk's OPTIONS keepalive is answered. M4 bound a **real** port and the SDP names it — but it is the **local** port, and a NAT that rewrites it will break the call. At the very least the SDP has to carry the NAT-mapped port and the stack has to accept the symmetric RTP a NAT sends back from a different source port; beyond that, `rtc` 0.21.1 already ships `ice`, `stun` and `turn` as unconditional dependencies, so full ICE with host/srflx/relay candidates is available without adding a crate. |
| **Configurable, playable ring tone for incoming calls, in the main settings** | **done in M6.1, M6.2 and M6.4** | The settings page has a **RINGTONE** section: the WAV file, a preview button and two levels. The player is in the media crate (`SoundPlayer`) — the C++ reference is `RingtonePlayer`. The built-in tone is generated rather than shipped as a binary. **M6.2** made the file **per account**: the account editor has one too, and the account's own file is used for its calls. **M6.4** added the **Browse…** dialog and the **Reset** button to both, so the file is picked rather than typed — the C++'s own `FileDialog`, with its WAV filter. |
| **A generated ring tone when a call is ringing and there is no media** | **done in M6.1** | A **synthesised** tone, not a file: it starts on a `180 Ringing` that carries no SDP, so an outgoing call still rings audibly. The C++ reference is `RingbackPlayer`. |

**Why NAT traversal is on this list and not in M2.** In M2 the SDP named
port 4000, and nothing bound it. Against the real Asterisk the call
comes up — `200 OK` with an SDP, then the ACK — and the PBX starts
sending RTP (G.711 A-law) at that port immediately, because our offer
lists PCMU and PCMA and Asterisk picked A-law. The audio has nowhere
to land. That was the whole of the remaining media gap, and it was
M4's job, not a NAT problem in the signalling.

M4 bound a real port and the RTP now has somewhere to land. What is
left is the NAT: the port the SDP names is the local one, and a router
that rewrites it will still break the call. That is the next thing to
try against a real PBX from behind a router.

**The two ring tones are not the same thing, and the C++ has both.**
`RingtonePlayer` — the **inbound** one — plays a **WAV file**:
`src/ui/assets/ringtone.wav`, 8 kHz mono 16-bit, 6 seconds long.
Decoded, it is 2 s of tone and then 4 s of silence — the North
American cadence, with the on/off pattern baked into that one loop. It
is copied next to the binary as `share/qsoftphone/ringtone.wav` and
overridden with `audio.ringtone_file`. `RingbackPlayer` — the
**outgoing** one — **synthesises** a 440 Hz tone, 1 s on / 3 s off, at
48 kHz mono (the bundled `ringback.wav` is the same cadence as a file:
1 s of tone, then 3 s of silence), and it takes its own volume
(`audio.ringback_volume`, a separate slider from the inbound
ringtone's).

Two rules carry over from the C++ unchanged. The tone stops when the
call is answered, declined or disconnected — **and** a 60-second
watchdog caps it, because pjsip 2.14 has a path where the call is torn
down so fast that the disconnected state never reaches the UI, and
without the cap the tone would play forever. The Rust tree's default
`ringtone_volume` is 0.9 where the C++ uses 0.7; align it when the
player lands.

---

## Verified, and not

**Verified in this repository:**

* `cargo build --workspace` succeeds (rustc 1.98.1, MSVC).
* `cargo test --workspace` — 429 tests, all passing.
* `cargo clippy --workspace --all-targets -- -D warnings` is clean.
* `cargo fmt --all --check` is clean.
* `--smoke` starts the app, creates the directories, the log file and
  the database, and exits 0.
* The GUI opens a window titled `qsoftphone` and stays up.
* Account CRUD round-trips through SQLite, including the policy block.
* The config file loads, saves and round-trips; a malformed file is an
  error, not a silent reset.
* **A real REGISTER goes over a real UDP socket.** The loopback
  registrar answers `401 Unauthorized` with a `WWW-Authenticate`
  challenge; the client answers with the digest, retries with a bumped
  CSeq, and gets `200 OK`. This runs for both `algorithm=MD5` and
  `algorithm=SHA-256`.
* **The digest is verified independently of rsipstack.** The test
  computes the RFC 7616 SHA-256 and the RFC 2617 MD5 itself and
  compares against what went on the wire, so a wrong hash function or a
  wrong set of hashed strings fails the test.
* **The account id is on the log line the registration task wrote** —
  proved end to end against the loopback registrar, not just at the
  formatter.
* **An OPTIONS keepalive is answered with `200 OK` over a real
  socket** — UDP and TCP both, on a connection opened by the "server"
  side. The response mirrors the request's Via, From, To, Call-ID and
  CSeq, and carries a To tag.
* **An unimplemented method still gets an answer** (`501`), so a peer
  never waits for a timeout.
* **An account can be edited in place** — same id, same creation
  timestamp, new settings, and still exactly one account.
* The registration state reaches the UI over the channel and is shown
  in the title bar and next to the account.
* **A call goes out and comes back, over a real UDP socket.** The
  loopback peer is a hand-written registrar and call agent: it answers
  the REGISTER, rings the call, answers it and hangs up. The test proves
  the INVITE reached it, the `180 Ringing` came back as `call_ringing`,
  the `200 OK` came back as `call_answered`, and the BYE reached the
  peer.
* **The hangup is a BYE, not a forced `603`.** The test asserts the
  status code the call ended with is `200`, and that the peer saw a
  `BYE`.
* **A call that is still ringing is declined with `603`** — the third
  case of §5.5, the one rsipstack's own `hangup()` leaves alone.
* **An inbound call rings before the user does anything.** The peer
  records the `180 Ringing` it received, and the test checks it *before*
  the answer is sent — so §5.4's ordering is proved, not assumed.
* **A DTMF digit arrives as an event** (`dtmf`, with the digit), and
  **a DTMF digit sent by the app arrives at the peer as an `INFO`**
  request.
* **A blind transfer reaches the peer and the original leg is released.**
  The loopback peer answers the `REFER` with `202 Accepted` and then
  reports the progress over `NOTIFY`s; the test reads the `Refer-To` off
  the wire, waits for the progress that says the new leg is answered, and
  checks that the original call ended and that the peer saw the `BYE`.
* **An attended transfer names the consultation.** The `Refer-To` carries
  `;method=INVITE?Replaces=`, and the `Replaces` is the consultation
  dialog's Call-ID with the `@` percent-encoded **and the dialog's two
  tags** — the peer looks the dialog up by all three. The same three
  identifiers go out in the **`Replaces` header**, unescaped, because
  that is the one Asterisk reads.
* **A transfer that cannot reach its target leaves the original call
  up.** The peer ends the subscription and the body says `486` or
  `503`; the progress comes back as a failure, and no `BYE` goes out.
* **A transfer that worked is not reported as a failure.** The final
  `NOTIFY` of a successful transfer carries
  `terminated;reason=noresource`, and that reason says nothing about
  the transfer — the body says `200 OK`, and the transfer worked.
* **A feature-code transfer arrives as DTMF.** The app types
  `*2<target>#` one digit at a time, and the peer sees every digit in
  order.
* **An in-dialog request carries the public address.** The BYE's
  `Contact` names the address the registrar says it sees us at, not the
  local socket. The test runs on a machine with a VPN interface, and
  without the fix the Contact is that interface's address — exactly
  what the user's PBX showed.
* **A BYE the peer does not know still ends the call.** The peer answers
  `481 Call/Transaction Does Not Exist`, which is what a PBX does after
  a transfer: the dialog is gone, the channel is in a bridge. The call
  still ends — the BYE went out (RFC 3261 §15.1.1).
* **The transfer popup says how the attended transfer is finished.** The
  popup has the C++'s two phases: one to name a target and pick the
  method, and one — while the consultation is dialling — that says where
  the consultation is and offers **Complete transfer**.
* **A call the far end cancels ends.** The peer cancels the `INVITE` and
  answers it with `487 Request Terminated` (RFC 3261 §9.2) — what
  Asterisk does when the party that was called gives up while the call
  is still ringing. The call ends with that code, and it was never
  reported as answered.
* **The transfer popup follows the consultation.** When the consultation
  ends, the popup closes, the original leg comes off hold and the status
  line says so; and a popup that still holds a consultation which is
  over offers **neither** "Complete transfer" nor "Cancel consultation".
* **A dead consultation cannot be transferred.** The action refuses it
  with *"The consultation is over — there is nothing to transfer"*, and
  nothing goes on the wire.
* **A transfer that failed puts the original call back on the line.** The
  leg was put on hold for the consultation, so the hold comes off when
  the transfer fails — the policy that keeps the original call up is
  worth nothing if the call is left silent.
* **The transfer can be completed while the target is still ringing.** The
  loopback peer holds the call in the early state, and the REFER goes out
  against that still-ringing dialog: the `Replaces` on the wire carries the
  consultation's Call-ID **and the to-tag that came with the `180
  Ringing`**, and the transfer goes on to the answered state.
* **A consultation that has not rung cannot be transferred.** The peer
  rings late, so the dialog has no to-tag yet; the SIP layer refuses with
  `ConsultationNotNamed`, and **nothing goes on the wire** — the peer
  never saw a REFER.
* **The consultation dying after the REFER is the server's business.** The
  app says the server has the transfer, does not say the user called it
  off, and **leaves the original leg on hold** — taking it off would put
  the user's voice back on the old peer's line while the server is
  bridging.
* **The button is not live before the target rings.** The popup says
  *"The transfer can be completed once the target rings"*, and the button
  is greyed out until then.
* **An incoming call rings.** The event starts the ringtone, and the test
  checks that the player is playing.
* **A call we made rings back — but only when there is nothing to hear.**
  The loopback peer answers the `INVITE` with a `180 Ringing` that carries
  no SDP, and the ringback starts. The peer answers the same way *with* an
  SDP — that is early media, and the peer's own audio is coming — and the
  test checks that the ringback does **not** start.
* **A ringing call can be redirected.** The peer calls us, the app answers
  the `INVITE` with a `302 Moved Temporarily`, and the `Contact` on it
  names the target that was asked for (`sip:200@…`), not the caller's own
  address.
* **A sound does not play for ever.** The watchdog caps every sound at a
  minute; the test uses a shorter cap and checks that the sound stops and
  that the player says it is not playing any more.
* **The settings page has the ringtone, and the page is reachable.** The
  file field, the preview button and the two levels are on the page, and
  the page scrolls — the echo test used to fall off the bottom of the
  window, and a setting that cannot be reached is a setting that does not
  exist.
* **An account can have its own ringtone.** The account's own file is
  used for its calls, an account with no file of its own falls back to
  the app's, and with neither it is the built-in tone. The account editor
  has the field, and an older database gains the column without losing
  the accounts that are already in it.
* **The ringtone file can be picked, not only typed.** The **Browse…**
  button is on the settings page and on the account editor, with the
  C++'s **Reset** beside it, and both are inside the window — a button
  off the right edge is a button nobody can click. The reset puts the
  built-in tone back.
* **A picked file lands where it was picked for.** The app's ringtone
  goes into the settings file; an account's goes into the editor, which
  is what "Save" then writes. A file that comes back for an account the
  user has since left is refused, and the status line says so.
* **A file that cannot be played is refused.** The picked file is read
  with the same reader the ringtone uses, and a file that is not a WAV
  the softphone can play never reaches the settings — the ringtone
  cannot become a file that will not ring.
* **The native dialog really opens.** On this machine (Windows), the
  COM `IFileOpenDialog` was opened from a worker thread and stayed up
  until the process ended — so the dialog is not just a call that
  returns nothing.
* **The panel follows the call that is up.** A call that has ended gives
  the panel up, a call that is answered takes it, and a call the user
  picked in the list keeps it — until that call ends.
* **The keypad sends its tones to the call that is up.** The tone goes to
  the call on the panel, not to the dead call the panel used to be stuck
  on, and the status line names the peer it was meant for.
* **A call can be held and taken off hold.** The hold is a re-INVITE
  whose SDP says `a=sendonly`; the peer answers `a=recvonly` and stops
  sending, and the re-offer names the codec that was negotiated and
  nothing else.
* **The transfer popup says the original call is on hold.** A leg that
  goes quiet without saying why looks like a fault.
* **The page does not jump when the levels move.** The diagnosis under
  the meters gets a space of its own that does not change size, so the
  buttons under it never move.
* **The answer names a codec the offer listed.** The loopback peer
  offers G.711 A-law and telephone-event — the offer Asterisk sends —
  and the test reads the `200 OK` off the wire and checks that it says
  PCMA and not PCMU. This is the bug that made Asterisk hang up on
  every answered call.
* **The incoming-call modal cannot be dismissed.** The UI is driven
  headlessly (egui lays the widgets out without a window): Esc and a
  click outside the modal are fed in, and the modal is still up
  afterwards. Answering or ending the call closes it.
* **The window opens on the call page, and a call brings it forward.**
  Both are tests, not intentions.
* **A call placed from the UI thread does not take the app down.** The
  test places the call from a thread that is *not* inside the tokio
  runtime — the same situation the egui UI thread is in — because a
  `#[tokio::test]` would run inside the runtime and hide the bug. See
  "The UI thread is not on the runtime" below.
* **A challenged INVITE is answered with the account's digest.** The
  loopback peer answers the first INVITE with `401 Unauthorized` and a
  `WWW-Authenticate` challenge; the client retries with an
  `Authorization` header, and the test verifies the digest against the
  account's password with rsipstack's own verifier.
* **A finished call lands in the database.** The test drives the
  events — the call comes in, it is answered, it ends — and then waits
  for the history worker to write the row and answer with it. The row
  is checked against §5.3: the peer is `6666`, the URI is
  `sip:6666@sip.example.com`, the direction is incoming, the call is
  answered and the SIP code is `200`.
* **An unanswered incoming call is stored as missed**, and a call that
  was answered is not.
* **A redial dials the dialled number.** The test builds the row the
  way a DID-forwarded call makes it — the remote party is the PBX, the
  dialled number is the customer — and asserts that the redial targets
  `6666` and not `asterisk`. (M4.1 added the incoming case: a call
  from 6661 to 104 is called back on 6661.)
* **The search finds what it should.** Case-insensitively over the
  number, the URI and the name; "486" finds the busy call by its SIP
  code; the direction chips narrow it; and a `%` in the search is a
  literal, not a wildcard.
* **A database written by the old schema still takes the account's
  UUID.** There are databases on disk with `account_id INTEGER`; the
  test opens one, writes a row and reads it back.
* **Audio goes from one session to another over a real UDP socket.** Two
  media sessions, each with a bound port, each pointed at the other: a
  G.711 frame queued on one comes out of the other's inbound queue,
  decoded, and within the quantisation error of G.711.
* **Every G.711 byte survives a round trip.** All 256 µ-law codes and
  all 256 A-law codes, through the encoder and the decoder, exactly.
* **A muted microphone still sends, and the peer hears silence.** The
  RTP clock keeps moving: five muted frames arrive as five frames.
* **The SDP names a port that is bound.** The media session's port is
  the one in the offer, and the two sessions in the loopback test are
  on different ports.
* **The peer's answer is read correctly.** The codec it chose, the
  payload type it put that codec on (including a dynamic one), the
  address and the port it listens on.
* **The media session can be stopped.** The test stops it and checks
  that nothing more arrives — and that the process still exits, which
  is how the sending thread's condvar bug was found.
* **The whole call page fits the window.** The headless frame is
  given the size the app opens at, and the test checks that the
  dialer, the call button and the call list are all on the screen. A
  row that is culled is a button the user cannot press — and the
  earlier headless tests never saw the page at all, because without a
  screen rect egui lays the central panel out with no height and drops
  everything below the first row.
* **The codec picker is not on the call page.** The headless frame of
  the call page does not contain the word "Codec" any more.
* **The settings page holds the codecs and the sound card.** The
  headless frame contains the codec list — including G.722 — the
  microphone and the speaker pickers, and the test sound button.
* **The codecs can be reordered, taken out and put back.** The order
  reaches the call layer, and the last codec cannot be turned off: a
  call with no codec has no media at all.
* **G.722 is byte-exact with the reference implementation.** Encoding
  the reference PCM and decoding the reference stream — the vectors
  that ship with `sippy/libg722`, the code lineage Asterisk's
  `codec_g722.c` runs — both reproduce the published SHA-256 digests.
  See [M4.1](#m41--codecs-the-sound-card-and-the-redial).
* **A G.722 frame goes from one session to the other** over a real UDP
  socket: 320 samples in, 160 bytes on the wire, 320 samples out on
  the other side.
* **The G.722 round trip keeps the speech.** 41–60 dB of
  signal-to-noise on a tone, once the 22-sample group delay of the
  synthesis filter is accounted for.
* **A muted wideband microphone sends silence too.** G.722 has no
  fixed silence code, so the silence is whatever the encoder makes of
  a frame of zeros — and it is quiet.
* **An incoming call redials the caller.** A call from 6661 to 104 is
  called back on 6661, and an outgoing call is still called back on
  the number that was dialled.
* **The test sound is played off the interface thread.** The button
  starts the work on the runtime, and the answer comes back on a
  channel — whether the tone played or the machine has no speaker.
* **The playout is continuous.** The peer sends a frame every 20 ms and
  the card takes 10 ms at a time; the test plays ten frames that way
  and checks that the sound never stops for more than the resampler's
  first sample. The test fails with the priming removed — the playout
  starts after 0 samples — so it is the buffer, not the test, that
  makes the sound whole.
* **The playout waits for the buffer to fill** before it starts, and
  it does not stop again when the buffer runs low.
* **The jitter buffer puts a late frame back in its place**, notices a
  frame that never arrives, throws away a frame that is too late to be
  of use, and does not grow without bound.
* **A late frame is waited for, not concealed.** The playout keeps the
  slack in the buffer while there is slack, and a frame that turns up
  late is decoded like any other.
* **A lost frame is concealed, and a peer that stops sending is not.**
  The first is tested with a frame that never arrives while the peer
  keeps sending; the second with a peer that sends one frame and then
  stops, which produces no audio at all.
* **A lost frame does not derail the G.722 decoder.** The signal-to-
  noise on the frames after the loss climbs from 13.8 dB to 31.7 dB as
  the decoder catches up.
* **The G.722 concealment cannot run away.** A frame of zeros and a
  repeated loud frame both reach full scale; the byte this build uses
  stays bounded over eight consecutive concealments.
* **The playout does not touch the pitch.** A 400 Hz sine through the
  whole path — codec, jitter buffer, decoder, queue, resampler, a
  simulated 48 kHz stereo card — comes out with a **purity of 1.000**:
  all of the energy is at the frequency it went in at. The pitch does
  not move: measured second by second, the dominant frequency stays
  within 1 Hz of 400 Hz. With M4.2's rate correction put back, the
  purity falls to **0.17** and the pitch wanders to 400.5 Hz. That
  measurement is the bug, and it is what the "dithering" was.
* **The buffer absorbs the difference between the two clocks.** A
  simulation of a peer that is 0.125 % slow — what two independent
  sound cards do to each other — runs for thirty seconds with no
  underrun, and a peer that is that fast does not make the delay run
  away either.
* **The audio survives a jittery and lossy peer.** One frame in fifty
  lost, with packets arriving early and late: less than 5 % silence
  and a purity above 0.95.
* **A stereo card gets one value per frame.** The interleaving is
  checked directly — left, right, left, right, and every channel of a
  frame gets the same value.
* **A stereo card plays the audio at the right speed.** The whole
  path, with a 48 kHz stereo device and a peer that sends a frame
  every 20 ms: the playout never runs dry. With the old code it ran
  dry **44,876 times out of 48,000 samples** — 94 % of the time.
  That measurement is the bug: it is why the sound was a dither
  rather than merely fast, and why a jitter buffer could not save it.
* **There is no energy at twice the frequency.** A playout running at
  double speed puts a second harmonic where there should be none; the
  fidelity test looks for it and does not find it.
* **The sound card's callback takes the queue once.** A whole
  callback's worth of samples, and exactly **one** take of the shared
  queue: the per-sample path has no lock in it at all. This is the
  discipline a real-time thread has to keep — a lock there is a kernel
  wait, and a kernel wait is a buffer overrun.
* **Unregistering in the middle of a call ends the call.** The
  account's session is closed: its token is cancelled, its media is
  stopped, the sound card is closed, and the interface is told the call
  ended. The call does not sit on the screen as a stale one.
* **Closing a session stops its media.** The media of a call is not
  running any more after the account is closed — checked on the
  transport, so the test never opens a microphone on the machine it
  runs on.
* **A hole in the call is counted.** A playout that runs dry is
  counted, so that the log says whether the sound was continuous or
  broken — and a call now says what its media did when it stops:
  frames received, frames concealed, samples of silence played.
* **A call with no microphone still sends.** The RTP clock is what
  tells the peer the call is alive, and a peer that sees an empty
  stream hangs up. With no microphone there is no capture side, so the
  sender makes the silence itself, on the frame clock, through the
  codec's own encoder — which keeps a stateful codec (G.722) in step
  with the decoder at the other end. The silence is µ-law's `0xFF`.
* **The clock keeps running.** Not one frame: five, in half a second,
  with nothing pushed by the test. The frames arrive on their own.
* **The default device is tried first**, and a device the user asked
  for by name is the **only** one tried — opening a different
  microphone without saying so would be worse than useless.
* **The redial is in the history row**, without expanding it, and it
  dials the caller back for an incoming call and the dialled number
  for an outgoing one.
* **The history rows have a legend.** The arrow, the colours and the
  bare number are all explained, and the colours come from the same
  place as the labels, so the key and the rows cannot drift apart.
* **A remote session is recognised and explained.** The notice says
  what a remote session is, where the switch for the microphone is, and
  that the call still works.
* **The history is a table with a header.** The headless frame of the
  history page has a `dir`, `Number`, `Duration`, `Time`, `code` and
  `status` column, side by side, in that order — and the row's number
  is under the "Number" column and below the header. The header is
  there even when the history is empty.
* **A call that lasted twenty seconds does not read as if it started
  and ended at the same time.** The stamp carries the seconds, and the
  test compares the two stamps of a call 22 seconds long.
* **A hung-up call leaves the status line at "Ready."** The status line
  used to stay on "Hanging up…" for ever.
* **The dialpad is on the call page**, it is not on the other pages, it
  dials the number when the app is idle, and the same key sends a tone
  during a call.
* **The build's own bookkeeping is not on the screen.** "M5 — settings"
  is gone from every page.
* **The dialpad is in the middle of the call page.** The headless frame
  puts the keypad between the number and the calls list, and the keypad
  is centred in the page's content area — not in the header, and not
  under the accounts panel.
* **The app asks to be drawn again.** egui only draws when something
  asks it to, and everything that changes in this app arrives from
  outside egui: the headless frame asks for a repaint, and the test
  fails if it does not. This is what made the window look slow.
* **The first frame is timed.** The app logs how long it took to get
  there — 49 ms on this machine — so "it starts slowly" can be checked
  against a number.
* **The echo test is on the settings page**, it says that it plays the
  microphone, and it **will not run during a call** — the sound card is
  a shared resource, and a second client on one endpoint is what makes
  cpal report an overrun. It **runs again as soon as the call is over**:
  a call that has ended has given the card back, and the card is free
  before the app is even told the call is gone.
* **The echo test reports what it heard and which devices it used**, and
  it distinguishes "the microphone sent nothing" from "the microphone was
  silent" from "your voice came in".
* **The echo test does not sit at "Listening…" for ever.** If the sound
  card never answers, the test is given up on and says so.
* **The echo test's queue keeps its bound and remembers its peak** — two
  seconds of samples at most, and the loudest sample that came in.
* **The echo test works on the real sound card.** The real function was
  run against this machine's microphone: 240,128 samples in five
  seconds, which is exactly the microphone's rate, and the peak was the
  room's noise floor. The capture half is proven on hardware.
* **The echo test publishes a live level.** The capture callback hands
  the level of the last callback to the caller — measured on the real
  sound card, 176 updates in four seconds — and the level meter shows
  it.
* **The echo report says the level in decibels.** "The microphone was
  silent" and "the microphone heard a whisper at -58 dB" are the same
  word and completely different problems; only the number tells them
  apart.
* **The echo test warns about a headphone speaker.** A headphone jack
  with nothing plugged into it is the commonest reason a test is
  silent.
* **The frames that went out are counted**, and the silence the sender
  had to make for itself is counted separately.
* **The call page holds only the calls that are up.** A call leaves the
  list when it is hung up.
* **The details of a history row are under that row.** The details used
  to be drawn below the **whole table**, which put them off the screen in
  a history of any length. The test expands the first row of a
  three-row history and checks that the details are above the second
  row — and the test fails if they are moved back below the table.
* **The button that opens the details is on the page.** The table used
  to be sized against `ui.available_width()`, which inside the scroll
  area runs past the edge of the window, so in a 716-point window the
  "Details" button was outside it. The table is now measured against the
  clip rectangle, and the test checks the button is on the screen.
* **The history's columns line up.** The row's number is under the
  "Number" header — the same x — and every column of the header is to
  the right of the one before it.
* **The call row says where the silence is.** The row carries the peer's
  level as it is now, the three peaks in decibels, and a sentence that
  names the place the silence is in: the microphone is muted, the speaker
  is muted, the peer is sending silence, the peer is sending something
  too quiet to hear, the sound card is not playing it, or the microphone
  picked up nothing.
* **A line that carried a beep and then went quiet is called quiet.** The
  peer is judged on the level now and not on the peak — a peak of -15 dB
  from the first second of a call does not make the rest of it audible.
* **The meter's number is given room in the layout.** It used to be
  painted outside the space the bar was given, and it landed on top of
  the text after it. The test reads both boxes out of the frame and
  checks that they do not overlap — and it fails if the number is put
  back where it was.
* **The levels are measured where it matters.** The peak of what the peer
  sent is measured on the **decoded** audio, the microphone's peak on
  the captured frame, and the speaker's peak on the samples **after** the
  gain and the resampler — which is the number that says whether the
  sound card was given anything at all.

**Verified against a real Asterisk:**

* **The REGISTER works against Asterisk PBX 22.8.0.** The registrar
  answers `401 Unauthorized` with
  `WWW-Authenticate: Digest realm="asterisk", …, algorithm=MD5, qop="auth"`,
  the client answers with the digest, and Asterisk replies `200 OK` with
  `Contact: <sip:104@181.28.198.57:…>;expires=299`.
* **An outbound INVITE reaches Asterisk, survives its challenge and is
  answered.** Asterisk challenges the INVITE with `401` exactly as it
  challenges the REGISTER; the client retries with the
  `Authorization` header, and Asterisk answers with a status code
  (`404 Not Found` for a SIP user that does not exist — the probe
  dialled a user that cannot exist, so nothing could ring).
* The INVITE carries the **public** Contact —
  `<sip:104@181.28.198.57:…>` — learned from the `received` and `rport`
  parameters the registrar echoes back, and the Request-URI keeps the
  registrar's port.
* **A call to the user's own extension is answered, and carries audio.**
  The call comes up on G.722 (`200 OK`, payload type 9), RTP flows both
  ways, and the media stops with `received=504 sent=663 concealed=312`.
  The audio is **quiet**, and that is now measured rather than guessed:
  the peer's payload decodes to -23 dBFS RMS for the first 400 ms and
  -45 … -66 dBFS after it, while our side delivers the peer's audio to
  the sound card at -16.7 dBFS — exactly the peer's peak times the 0.8
  speaker gain. See [M4.11](#m411--where-the-silence-is).
* **The codec we use is the one we prefer.** Measured against the
  user's own Asterisk: the offer says `RTP/AVP 8 9 0` (PCMA first) and
  the answer says `RTP/AVP 9 8 0` (G.722 first). Before M4.12 the
  answer won, and every call was G.722. Now the offer's preference
  wins, and the same call comes up as **PCMA, payload type 8**.
* **Our RTP on the wire is textbook.** A dump of every packet over a
  twenty-second call: fifty frames a second, the sequence number up by
  one every time, the timestamp up by 160, a hundred and sixty byte
  payload, **zero** gaps.
* **The frames the peer never sent are counted, and told apart from the
  ones we hid.** A call on the user's PBX ends with `received=829
  sent=1050 lost=206 concealed=233`: 206 frames never arrived, and the
  233 concealed are the gaps that had to be filled — the two numbers
  are not the same thing.
* **The clock runs at fifty frames a second with no microphone.** The
  test measures the rate over a second and asks for at least forty: a
  sender that waits twenty milliseconds between the frames runs at
  thirty-one on Windows, and it did.
* **The user's own capture, read packet by packet.** Both calls in it —
  this app's and MicroSIP's — have **zero** sequence gaps in both
  directions, and the audio that comes back is a faithful copy of the
  audio that was sent (`incoming_peak=0.19140625` against
  `outgoing_peak=0.19102521` for this app's call). See
  [M4.13](#m413--the-two-captures-side-by-side).
* **The marker bit marks a talkspurt, and only a talkspurt.** The test
  reads the bit off the wire: it is set on the first packet of a stream
  and on the first packet after a silence, and on no other packet.
* **The microphone gain is applied to the samples.** The test measures
  the frame the microphone produced: half the gain is half the voice,
  and four times the gain is four times the voice.
* **The knobs are where they should be.** The call page has the two
  knobs next to the mute buttons, and the settings page has the volume
  every call starts with — and the knob is in decibels, because a gain
  on its own means nothing to a user.

**Not verified:**

* **No third-party softphone has been called.** The calls in the test
  suite were made against the hand-written peer in this repository; the
  Asterisk run above ended in a `404`. The SIP messages on the wire are
  the real ones, but no call has yet been *answered* by a peer this
  repository did not write.
* **A call has now been made and listened to, and it was silent.** The
  user placed calls from this build to their own Asterisk extension,
  answered them, and reported **"no sound"**. The call is answered, RTP
  flows both ways, and the numbers say the line is quiet: the peer's
  payload is -45 … -66 dBFS after the first 400 ms, while our side hands
  the sound card -16.7 dBFS. The later capture settles the other
  direction: our own outgoing stream in that call carried real speech
  (peak 0.19) and the PBX sent it back at the same level. What is *not*
  yet proved by a human ear is what the far end hears from us. The call
  row shows all three numbers while the call is up, so the next call
  answers the question without an argument with a log. See
  [M4.11](#m411--where-the-silence-is).
* **The echo comes back clean, and the pcap says why.** The user's report
  was that the echo from their Asterisk echo extension arrives garbled,
  while MicroSIP on the same machine and the same PBX gets a loud, clean
  one. The cause was measured and fixed in
  [M4.12](#m412--the-codec-the-peer-chose-and-the-frames-that-never-came):
  this app was being pushed onto **G.722**, whose decoder cannot survive
  a lost frame. The user's own capture has now settled the rest — see
  [M4.13](#m413--the-two-captures-side-by-side): **not one frame was
  lost in either direction**, in either call, and the returned audio is
  a faithful copy of what was sent. What is left is the marker bit,
  which is fixed and proved by the tests.
* **The user's microphone is silent in every measurement this machine
  made, and loud in the user's own.** The levels say `you=0.0015`, about
  -56 dBFS — every measurement of the microphone was taken with nobody
  at the machine. The user's capture, taken with somebody at the
  machine, has our outgoing stream at peak 0.19 with speech in it, and
  the PBX sends it back at the same level. What the machine cannot see
  is the microphone's own state: whether it is muted in Windows, or the
  machine picked the wrong device, or the room was simply quiet.
* **The volume knobs have not been heard by a user.** The gain is
  proved by the tests — half the gain is half the voice — and the knobs
  are on the pages, but nobody has turned one up on a real call yet, so
  what the peer hears with the preamp at +12 dB is a question the
  numbers answer and the ear has not.
* **The stale call has been proved by reading the code, not by
  hearing it.** The chain is in [M4.5](#m45--the-stale-call-and-the-real-time-callback)
  and every link is checkable: the media was only ever released by the
  dialog's `Terminated` state, unregistering never produces that state,
  and the registry's `remove` — the one function that closes a session
  properly — had no caller outside a test. What has *not* been done is
  to watch a call be unregistered and then watch the next call open
  the device cleanly. **That is the one thing to check on the next
  call**, and the log now says what happened:
  `the call's media stopped received=… concealed=… underruns=…`.
* **The transfer has not been made to a real PBX.** The `REFER`, the
  `NOTIFY` progress and the release of the original leg are all proved
  against the hand-written peer in this repository, over a real socket.
  What has not been tried is the user's own Asterisk: the attended
  transfer failed there because the `Replaces` named the dialog by its
  Call-ID alone, and a PBX that matches a dialog by its tags cannot find
  it that way. The `Replaces` now carries the tags as well — whether
  Asterisk takes it is the open question, and it is the one to check on
  the next call with a second extension.
* **The attended transfer against the user's Asterisk said "no route to
  target" — and M6.3 says why.** The user's own log (21:25 and 21:26 on
  2026-11-09) has the whole exchange: the consultation was answered, the
  `REFER` went out, Asterisk answered `202 Accepted` and then two
  `NOTIFY`s, the last one `terminated;reason=noresource`, and it took
  the consultation down. "No route to target" is Asterisk saying it
  could not find the dialog — and it could not, because the dialog was
  named as a **URI parameter** and Asterisk reads the **`Replaces`
  header**. The header now goes out as well (M6.3). Whether Asterisk
  then bridges the way the user expects is the open question, and it
  needs the next call with a second extension.
* **The consultation dying has not been seen against the real PBX.** The
  app's reaction to it — the popup closing, the original leg coming off
  hold — is proved against the peer in this repository, which cancels the
  `INVITE` and answers it with `487` the way Asterisk does when the
  party that was called gives up while the call is still ringing. What
  the user's PBX does in that moment is not known from the log: the
  `BYE` in it came **after** the `REFER`, so the consultation died
  because the transfer failed rather than because the target hung up.
* **The attended transfer has not been seen through yet.** Against the
  user's Asterisk the `Replaces` was accepted — the dialog was found,
  and Asterisk put the transfer in motion — but the progress never came
  back: the NOTIFYs went to the private address in the Contact, so the
  app never learned the transfer had worked and never released the
  original leg. That is the Contact fix above. What the next call with
  a second extension will show is whether the far end is bridged the
  way the user expects — that is a question about the PBX's dialplan,
  not about the app.
* **The transfer completed while the target rings has not been seen
  against a real PBX.** The user's own Asterisk never sends a `180
  Ringing`: it answers `100 Trying` and then `200 OK`, so the
  consultation goes from *dialling* straight to *answered* and the
  window in which the button could be pressed early does not exist
  there. The feature is proved against the peer in this repository,
  which holds the call in the early state — but a PBX that rings and
  then **answers after the REFER has already gone out** is the case
  that has not been run, and that is the case the user asked for.
* **The ringtone and the ringback have not been heard.** The tones are
  made, the WAV is read and the decision is tested — and the sound
  itself has not been through a speaker, because no call has rung yet:
  see the next bullet. The cadences are the C++'s (440 + 480 Hz, two
  seconds on and four off; 440 Hz, one second on and three off), and the
  file reader is proved against WAVs the test writes itself. The test
  suite uses a **silent** player, so a run does not beep through every
  test; the machine's own card (a Realtek headphone output) was found by
  the media crate when it was called live.
* **The user's own Asterisk answers at once, so it cannot ring.** A live
  call to extension 104 came back `100 Trying` and then `200 OK` — no
  `180 Ringing` at all — and extension 559 (the echo extension) is the
  same. Two consequences: the **ringback** cannot be exercised against
  this PBX, because a PBX that answers at once never rings, and the
  **redirect** cannot be exercised either, because a redirect needs an
  *inbound* call and the only client on this machine is this app. Both
  need a phone that calls 104, or a second extension that rings.
* **The redirect has not been tried against the real Asterisk.** The `302`
  and its `Contact` are proved against the hand-written peer, over a real
  socket. What Asterisk does with a `302` — whether it follows the
  `Contact`, and what the caller's phone sees — is a question about the
  PBX, and it is the one to check on the next inbound call.
* **The remote session has not been tested against one.** This machine
  is at the console, so `GetSystemMetrics(SM_REMOTESESSION)` returns
  false here and the notice is exercised by a test that forces it on.
  What the redirected endpoint does is the operating system's
  business; what this build had wrong — sending *nothing* when there
  is no microphone — is fixed and proved on the loopback.
* **The failing call's pcap is not on this machine.** The report was
  that the raw pcap plays cleanly and the softphone does not, which
  proves the payloads are good; but the capture itself has not been
  replayed through this build's path, so the real stream's loss and its
  clock offset have not been measured. It is not needed to check the
  next call — the call's own numbers are now in the log — but it would
  settle the clock question: the RTP timestamps against the arrival
  times give the clock difference directly, and the sequence numbers
  give the loss.
* **G.722 has not been talked to a PBX.** The codec is proved
  byte-exact against the reference implementation Asterisk uses — the
  published SHA-256 digests of the `sippy/libg722` vectors both match
  — but no call has yet been made with `allow=g722` on the PJSIP
  endpoint and listened to. That is the one thing left to try.
* **The test sound has not been heard either.** It is played on the
  runtime and the result comes back, but a locked desktop has no
  speaker to play it on.
* **The echo test has been heard — by the user, on this machine.** After
  M4.9 the user pressed the "Echo test" button and reported **"Sound
  works"**, which is the one thing the numbers could not say: the
  capture half is proved on the real sound card (the real function was
  run against the real microphone and took 240,128 samples in five
  seconds, which is exactly the microphone's rate, with the peak at the
  room's noise floor — −48 dBFS to −67 dBFS, depending on the moment),
  and the playback half now has a human ear behind it. What it does
  **not** prove is a call: the echo test is a loopback through the
  sound card, not an RTP session to a peer.
* **The microphone has been heard — by the user, through the echo test.**
  "I hear myself" is the microphone working: the capture side picks the
  user up and the playback side plays it back. What a call cannot yet
  show is what the **far end** hears: the microphone measured
  -38 … -54 dBFS in the automated runs, which is a room with nobody in
  it.
* **The media has not been tested against a NAT.** The SDP advertises
  the public address, but the RTP port is the local one, and a NAT
  that rewrites it will break the call. This is the first thing to
  check when the app is run against a real PBX from behind a router.
* The GUI has been started and its window photographed, so the layout
  is as it is shown above. It has not been reviewed by a human at
  normal resolution — this machine's desktop was locked for most of
  the build, and the screenshots were taken when it was not. The latest
  pictures were painted from egui's own tessellation rather than
  photographed from the screen, because the desktop was locked; they
  show the real layout and the real colours, but they are a rasteriser's
  idea of the frame, not the framebuffer itself.
* The Linux build has not been compiled or run. The dependency tree is
  resolved for `x86_64-unknown-linux-gnu` (390 package names) but no
  Linux binary was produced.
* **The file dialog has only been run on Windows.** The COM backend was
  opened and waited for, on this machine. The Linux portal backend and
  the macOS `NSOpenPanel` are the upstream code paths, but neither has
  been run here, and the macOS panel has no "All files" filter (the
  filters are merged into one flat list of allowed types there, where a
  `*` is a literal extension rather than a wildcard).
* **The dialog is not parented to the app's window.** `eframe` keeps
  the raw window handle private, so there is no handle to give `rfd`.
  The dialog is a window of its own; it comes to the front, because a
  process that owns the foreground may always bring its own windows
  forward, but it is not modal to the app's window.
* The SIP domain is not applied to the From/To URI — M6, as noted
  above.
