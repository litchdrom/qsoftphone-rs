# qsoftphone-rs

A Rust rewrite of **qsoftphone**, the desktop SIP softphone.

The C++/Qt implementation lives next door in `../qsoftphone` and is the
behavioural reference for everything in this repository. This crate is
not a port of its UI — it is a port of its *semantics*: the SIP
identity resolution, the call event contract, the history rules and
the transfer construction all have to behave identically, including the
bugs that have already been fixed there.

**Status: M2 complete.** The workspace builds, the test suite passes, the
app starts and exits cleanly, an account can be registered against a SIP
registrar with digest authentication (MD5 and SHA-256), and calls can be
placed and received over that registration — outbound INVITE, inbound
INVITE with an automatic 180 Ringing, answer, decline, hang up, and DTMF.
There is no audio yet: the media plane is M4. See
[Milestones](#milestones).

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
        └── app.rs                 the egui shell
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
| `egui` / `eframe` | 0.37.0 | the UI (default wgpu backend) |
| `rusqlite` | 0.41 | storage, `bundled` — SQLite is compiled in |
| `tokio` | 1.54.1 | the async runtime the SIP stack runs on |
| `tracing` | 0.1 | logging |

### The dependency budget

The brief asks for a resolved tree under ~250 crates, assuming
rsipstack would be the bulk at 134.

**Measured, the real number is 305 package names** for the Windows target
(384 for Linux, which adds X11/Wayland/ALSA). Three of those are this
workspace's own crates, so 302 and 381 are third-party packages. The gap
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
| §5.6 `Refer-To` construction, blind and attended | `src/core/sip/SipEndpoint.cpp` | `sip::refer` |
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
  header — and that the test suite expects exactly that.

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

### The SDP is a placeholder

There is no audio in M2, and the SDP says so: one PCMU stream on a port
that is not bound, `a=sendrecv`. The call connects, the SIP trace is
clean, and M4 replaces the whole thing with the session `rtc` negotiated.

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

### The UI

The dialer is only live for a **registered** account — a call needs an
endpoint to go out from, and the call layer refuses it if there is
none. The call panel follows the newest call that is not over, and shows
the resolved peer name, the dialled number, the last DTMF digit and a
hang-up button, with the DTMF keypad once the call is connected.

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

| | Windows | Linux | macOS |
|---|---|---|---|
| config | `%APPDATA%\qsoftphone\config.json` | `$XDG_CONFIG_HOME/qsoftphone/config.json` | `~/Library/Application Support/qsoftphone/config.json` |
| data | `%LOCALAPPDATA%\qsoftphone\data\` | `$XDG_DATA_HOME/qsoftphone/data/` | `~/Library/Application Support/qsoftphone/data/` |
| logs | `%LOCALAPPDATA%\qsoftphone\logs\` | `$XDG_CACHE_HOME/qsoftphone/logs/` | `~/Library/Logs/qsoftphone/` |

Logs rotate daily: `qsoftphone.log.2026-10-26`. `RUST_LOG` overrides
the level from the settings file. Every line carries the account id,
and call lines carry the call id — see
[Logging (§6)](#logging-6).

SQLite databases live in the data directory: `accounts.sqlite`,
`history.sqlite`, `contacts.sqlite`.

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
| M3 | History: SQLite persistence with the §5.3 semantics, redial using the resolved dialled number (§5.1), searchable list in the UI | next |
| M4 | Media: `rtc` RTP + `cpal` audio — SDP negotiation, send/receive audio, mute, codec selection | |
| M5 | Transfer: blind and attended REFER per §5.6, NOTIFY progress, original leg released on success | |
| M6 | Settings & polish: accounts UI, transport selection, SIP domain, outbound proxy, ringtone, the feature codes of §5.9 | |

One milestone at a time; M2 does not start before M1 works end to end.

---

## Verified, and not

**Verified in this repository:**

* `cargo build --workspace` succeeds (rustc 1.98.1, MSVC).
* `cargo test --workspace` — 146 tests, all passing.
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
* **The incoming-call modal cannot be dismissed.** The UI is driven
  headlessly (egui lays the widgets out without a window): Esc and a
  click outside the modal are fed in, and the modal is still up
  afterwards. Answering or ending the call closes it.

**Not verified:**

* **No third-party registrar has been used.** Everything above was
  proven against a registrar written by hand, inside this repository.
  The digest, the CSeq bump, the `Expires` handling and the
  `algorithm=SHA-256` echo are all correct against the spec — but an
  Asterisk, Kamailio or OpenSIPS instance has not been tried, and that
  is the real test of §5.8. In particular, the OPTIONS keepalive has
  been proven against a hand-written registrar, not against Asterisk's
  own `qualify` timer.
* **No third-party softphone has been called.** The calls above were
  made against the hand-written peer in this repository. The SIP
  messages on the wire are the real ones, but no Asterisk or Kamailio
  instance has placed or received one.
* **No audio.** The SDP is a placeholder and no RTP has been sent. That
  is M4.
* The GUI has been started and its window inspected by title, but the
  layout has not been reviewed by a human at normal resolution — the
  desktop this was built on was locked, so the window could not be
  photographed. What *is* proved is that the UI lays out and draws
  without panicking, and that the M2 behaviours above hold, in a
  headless egui run.
* The Linux build has not been compiled or run. The dependency tree is
  resolved for `x86_64-unknown-linux-gnu` (384 package names) but no
  Linux binary was produced.
* The SIP domain is not applied to the From/To URI — M6, as noted
  above.
