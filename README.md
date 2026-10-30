# qsoftphone-rs

A Rust rewrite of **qsoftphone**, the desktop SIP softphone.

The C++/Qt implementation lives next door in `../qsoftphone` and is the
behavioural reference for everything in this repository. This crate is
not a port of its UI — it is a port of its *semantics*: the SIP
identity resolution, the call event contract, the history rules and
the transfer construction all have to behave identically, including the
bugs that have already been fixed there.

**Status: M4.4 complete.** The workspace builds, the test suite passes, the
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
made it wobble. See [Milestones](#milestones).

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
| `nanoaudio-g722` | 0.1.1 | the G.722 wideband codec |
| `egui` / `eframe` | 0.37.0 | the UI (default wgpu backend) |
| `rusqlite` | 0.41 | storage, `bundled` — SQLite is compiled in |
| `tokio` | 1.54.1 | the async runtime the SIP stack runs on |
| `tracing` | 0.1 | logging |

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
`Playback::begin()` is now just the priming check.

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
  have not been replayed through it. See
  [Not verified](#verified-and-not).
* **No adaptive jitter depth.** The buffer is still a fixed four
  frames, so the delay is a flat 80 ms.

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
| M5 | Transfer: blind and attended REFER per §5.6, NOTIFY progress, original leg released on success | |
| M6 | Settings & polish: accounts UI, transport selection, SIP domain, outbound proxy, ringtone, the feature codes of §5.9 | |

One milestone at a time; M3 does not start before M2 works end to end.

### Backlog — low priority

Wanted, but not next. Each item says which milestone it lands in, so
none of them is lost and none of them disturbs the order above.

| Item | Lands in | Notes |
|---|---|---|
| **A timestamp-driven playout** | M5 | The proper way to absorb the difference between the peer's clock and ours without touching the pitch. Each frame is played at the time its **RTP timestamp** says, and a clock difference is absorbed by dropping or repeating a sample at the **boundary** of a frame rather than by changing the rate of the whole stream. This is what pjmedia's jitter buffer does (`pjmedia_jbuf` is frame-indexed and clock-driven). M4.4 removed the rate correction that was doing it wrongly; this replaces it properly, and it is what makes a call hold up for hours rather than minutes. |
| **Codec selection per account** | M6 | The codec list is now on the settings page and is **app-level**: the order is the offer's order, and it is kept in the settings file. What is still missing is that it should be **per account** — one account may only speak PCMU, another only PCMA. `AccountProfile` has no codec field yet, and the account editor has no control for one. |
| **Media device selection, in the main settings** | **done in M4.1** | The picker is on the settings page: the microphone and the speaker, with "System default" and the devices of the machine. The device list is read on the runtime, not on the interface thread, and the name is persisted in the settings file — the fields were already there (`media.microphone`, `media.speaker`) and the media crate reads them. |
| **Opus, as a third codec** | M6 | The next codec after G.722. It needs a pure-Rust encoder (`opus-rs`, which brings `fearless_simd` and `libm` with it), the RFC 7587 RTP framing, and — the hard part — a **third-party `codec_opus` module on the PBX**: Asterisk does not ship one, so the PBX has to be built with it before a single Opus call is possible. |
| **Expandable history, with the call time and the SIP code** | **done in M3** | The **data** was already there: the `calls` table has `duration_sec`, `sip_code` and `hangup_cause`. The UI was the missing part, and M3 has it: the history page's row shows the number, the duration, the time and the SIP status code, and expands to the full URI, the cause and the exact timestamps. |
| **NAT traversal for the media plane** | M5 | The signalling side already does what it can: the public `Contact` is learned from the `received`/`rport` the registrar echoes (RFC 3581), the binding is refreshed before it expires, and Asterisk's OPTIONS keepalive is answered. M4 bound a **real** port and the SDP names it — but it is the **local** port, and a NAT that rewrites it will break the call. At the very least the SDP has to carry the NAT-mapped port and the stack has to accept the symmetric RTP a NAT sends back from a different source port; beyond that, `rtc` 0.21.1 already ships `ice`, `stun` and `turn` as unconditional dependencies, so full ICE with host/srflx/relay candidates is available without adding a crate. |
| **Configurable, playable ring tone for incoming calls, in the main settings** | M6 | The config keys are already there: `audio.ringtone_file` (empty = the bundled default) and `audio.ringtone_volume`. What is missing is the **player** — and the **Settings → Audio** page: a file picker, a volume slider and a preview. The C++ reference is `RingtonePlayer`. M4 opened a sound card, so the player is now possible. |
| **A generated ring tone when a call is ringing and there is no media** | M5 | A **synthesised** tone, not a file: it starts on 180 Ringing when there is no RTP to carry audio, so an incoming call still rings audibly. It needs a sound card, not RTP — and M4 opened one, so the player is now possible. |

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
* `cargo test --workspace` — 271 tests, all passing.
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

**Not verified:**

* **No third-party softphone has been called.** The calls in the test
  suite were made against the hand-written peer in this repository; the
  Asterisk run above ended in a `404`. The SIP messages on the wire are
  the real ones, but no call has yet been *answered* by a peer this
  repository did not write.
* **No audio has been heard by a human.** The RTP path is proved
  end to end between two sessions in this repository, the G.711 codec
  is proved against its own tables, the jitter buffer and the channel
  handling are proved in simulation, and the audio's **fidelity** is
  measured (a purity of 1.000 through the whole path) — but no call
  has yet been made to Asterisk and listened to. The desktop was locked
  for most of the M4 build, so the sound card could not be exercised by
  a person. **The dithering that M4.3 and M4.4 fix has not been heard
  fixed** — it has been proved wrong on the numbers, which is not the
  same thing, and the next call is the real test.
* **The failing call's pcap is not on this machine.** The report was
  that the raw pcap plays cleanly and the softphone does not, which
  proves the payloads are good; but the capture itself has not been
  replayed through this build's path, so the real stream's loss and its
  clock offset have not been measured. Dropping that pcap into the
  workspace would settle it: the RTP timestamps against the arrival
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
* **The media has not been tested against a NAT.** The SDP advertises
  the public address, but the RTP port is the local one, and a NAT
  that rewrites it will break the call. This is the first thing to
  check when the app is run against a real PBX from behind a router.
* The GUI has been started and its window photographed, so the layout
  is as it is shown above. It has not been reviewed by a human at
  normal resolution — this machine's desktop was locked for most of
  the build, and the screenshots were taken when it was not.
* The Linux build has not been compiled or run. The dependency tree is
  resolved for `x86_64-unknown-linux-gnu` (385 package names) but no
  Linux binary was produced.
* The SIP domain is not applied to the From/To URI — M6, as noted
  above.
