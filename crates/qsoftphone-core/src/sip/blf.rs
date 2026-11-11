//! The Busy Lamp Field: what the other extensions are doing.
//!
//! A BLF is a lamp on a desk that says whether a colleague is free,
//! busy or on a call. On Asterisk it is built on RFC 3903: the watcher
//! **subscribes** to the `dialog` event for an address of record,
//! and the server answers with a `dialog-info+xml` document that lists
//! that address's dialogs.
//!
//! Everything here is a pure function over strings — the document is
//! parsed, and the lamp is worked out from the dialogs in it. §6.

use std::collections::BTreeMap;

/// What one lamp says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlfState {
    /// Nothing is known: the subscription has not been established, or
    /// the server has not said anything yet.
    #[default]
    Unavailable,
    /// The extension is free.
    Available,
    /// The extension is ringing — a call is coming in, or going out.
    Ringing,
    /// The extension is on a call.
    Busy,
}

impl BlfState {
    /// The word the panel shows.
    pub fn label(self) -> &'static str {
        match self {
            BlfState::Unavailable => "unknown",
            BlfState::Available => "free",
            BlfState::Ringing => "ringing",
            BlfState::Busy => "busy",
        }
    }

    /// The colour the lamp is.
    ///
    /// The lamp is what the eye reads: green for free, red for busy,
    /// amber for ringing, and grey for "we do not know" — a lamp that
    /// shows a colour it has not earned is worse than a lamp that
    /// shows none.
    pub fn colour(self) -> (u8, u8, u8) {
        match self {
            BlfState::Unavailable => (140, 140, 140),
            BlfState::Available => (120, 200, 120),
            BlfState::Ringing => (231, 176, 60),
            BlfState::Busy => (231, 76, 60),
        }
    }
}

/// The state of one dialog in the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogState {
    /// The call is being set up and the far end has not answered.
    Trying,
    /// Early media: the far end is ringing.
    Proceeding,
    /// Early media with a local alert.
    Early,
    /// The call is answered.
    Confirmed,
    /// The call is over.
    Terminated,
    /// A state this build does not know.
    ///
    /// It is kept rather than thrown away: a server that grows a new
    /// state should not make the lamp go dark.
    Other(String),
}

impl DialogState {
    /// The state the document named.
    pub fn parse(text: &str) -> DialogState {
        match text.trim() {
            "trying" => DialogState::Trying,
            "proceeding" => DialogState::Proceeding,
            "early" => DialogState::Early,
            "confirmed" => DialogState::Confirmed,
            "terminated" => DialogState::Terminated,
            other => DialogState::Other(other.to_string()),
        }
    }

    /// Is this a call that is still up?
    fn is_live(&self) -> bool {
        matches!(
            self,
            DialogState::Trying
                | DialogState::Proceeding
                | DialogState::Early
                | DialogState::Confirmed
        )
    }

    /// A state that says nothing at all.
    ///
    /// A dialog the server named but did not say anything about is not
    /// a dialog that is over: it is a dialog whose state is unknown,
    /// and the lamp says so.
    fn is_unknown(&self) -> bool {
        matches!(self, DialogState::Other(state) if state.is_empty())
    }
}

/// One dialog in the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    /// The dialog's own id. It is how a **partial** update knows which
    /// dialog it is talking about.
    pub id: String,
    /// What the call is doing.
    pub state: DialogState,
}

/// A `dialog-info` document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DialogInfo {
    /// The address of record the document is about.
    pub entity: String,
    /// `full` or `partial`.
    ///
    /// A **full** document replaces everything that is known; a
    /// **partial** one only changes the dialogs it mentions.
    pub state: String,
    /// The dialogs in it.
    pub dialogs: Vec<Dialog>,
}

/// Parse a `dialog-info+xml` document.
///
/// The document is read for two things: the state of every `<dialog>`,
/// and the `id` that names it. Nothing else matters, and nothing else
/// is read — the `<local>` and `<remote>` blocks are what the *peer*
/// cares about, not what a lamp does.
///
/// **The state comes in two shapes, and both are read.** RFC 3903 puts
/// it in an attribute of the `<dialog>`:
///
/// ```xml
/// <dialog id="105" state="confirmed"> … </dialog>
/// ```
///
/// and Asterisk's `res_pjsip_dialog_info_body_generator` puts it in the
/// **text** of a `<state>` child:
///
/// ```xml
/// <dialog id="105">
///  <state>terminated</state>
/// </dialog>
/// ```
///
/// A reader that only knows the first one sees a lamp that never
/// changes, and that is exactly what happened.
///
/// `None` when the text is not a `dialog-info` document at all. A
/// document that **is** one but has no dialogs in it is a document that
/// says the extension is free, and that is not an error.
pub fn parse(xml: &str) -> Option<DialogInfo> {
    let tags = tags(xml);

    let root = tags.iter().find(|tag| tag.name == "dialog-info")?;
    let entity = attribute(root.attrs, "entity").unwrap_or_default();
    let state = attribute(root.attrs, "state").unwrap_or_default();

    // The dialogs, in order. A `<state>` element belongs to the dialog
    // that is open around it, which is the one before it.
    let mut dialogs: Vec<Dialog> = Vec::new();
    let mut open: Option<(String, Option<String>)> = None;
    for tag in &tags {
        match tag.name {
            "dialog" => {
                if let Some((id, state)) = open.take() {
                    dialogs.push(dialog(id, state));
                }
                open = Some((
                    attribute(tag.attrs, "id").unwrap_or_default(),
                    attribute(tag.attrs, "state"),
                ));
            }
            "state" => {
                // The child element's text, when the dialog has no
                // attribute of its own.
                if let Some((_, state)) = open.as_mut()
                    && state.is_none()
                {
                    let text = tag.text.trim();
                    if !text.is_empty() {
                        *state = Some(text.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    if let Some((id, state)) = open.take() {
        dialogs.push(dialog(id, state));
    }

    Some(DialogInfo {
        entity,
        state,
        dialogs,
    })
}

/// One dialog, with the state it named.
fn dialog(id: String, state: Option<String>) -> Dialog {
    Dialog {
        id,
        state: DialogState::parse(&state.unwrap_or_default()),
    }
}

/// What the lamps of one watched extension say.
///
/// The server sends the state of the **whole** extension with a `full`
/// document and the state of **one** dialog with a `partial` one, so
/// the dialogs that are not in a partial document are still true — they
/// are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tracker {
    /// The dialogs that are known, by id.
    dialogs: BTreeMap<String, DialogState>,
    /// Whether anything has been said at all.
    ///
    /// An extension nobody has heard from is **unavailable**, not
    /// free: a lamp that says "free" when the truth is "we do not
    /// know" is a lamp that lies.
    heard: bool,
}

impl Tracker {
    /// A tracker that has heard nothing.
    pub fn new() -> Tracker {
        Tracker::default()
    }

    /// Take a document and work out what the lamp says.
    pub fn apply(&mut self, info: &DialogInfo) -> BlfState {
        self.heard = true;

        // A full document replaces everything; a partial one only
        // changes the dialogs it mentions.
        if info.state.eq_ignore_ascii_case("full") || self.dialogs.is_empty() {
            self.dialogs.clear();
        }
        for dialog in &info.dialogs {
            // A dialog with no id cannot be told apart from another,
            // so it is the only one of its kind.
            let id = if dialog.id.is_empty() {
                format!("\u{0}{}", self.dialogs.len())
            } else {
                dialog.id.clone()
            };
            self.dialogs.insert(id, dialog.state.clone());
        }

        self.state()
    }

    /// What the lamp says now.
    pub fn state(&self) -> BlfState {
        if !self.heard {
            return BlfState::Unavailable;
        }
        if self
            .dialogs
            .values()
            .any(|state| matches!(state, DialogState::Confirmed))
        {
            return BlfState::Busy;
        }
        if self.dialogs.values().any(|state| state.is_live()) {
            return BlfState::Ringing;
        }
        // A dialog the server named but said nothing about: the state
        // is unknown, and "unknown" is not "free".
        if self.dialogs.values().any(|state| state.is_unknown()) {
            return BlfState::Unavailable;
        }
        // Every dialog in it is over, or there is none: the extension
        // is free.
        BlfState::Available
    }
}

/// One tag in the document.
struct Tag<'a> {
    name: &'a str,
    attrs: &'a str,
    /// The character data that follows the open tag, up to the next tag.
    text: &'a str,
}

/// Every element in the text, in order.
///
/// Comments, CDATA sections, processing instructions and the doctype
/// are skipped: a comment that contains the text `<dialog
/// state="confirmed">` must not be read as a dialog, and a document
/// with a comment in it is a document.
fn tags(text: &str) -> Vec<Tag<'_>> {
    let mut out = Vec::new();
    let mut i = 0;

    while i < text.len() {
        let Some(start) = text[i..].find('<').map(|at| i + at) else {
            break;
        };
        let rest = &text[start + 1..];

        if rest.starts_with("!--") {
            // A comment: everything up to "-->" is not markup.
            match rest.find("-->") {
                Some(at) => {
                    i = start + 4 + at + 3;
                    continue;
                }
                None => break,
            }
        }
        if rest.starts_with("![CDATA[") {
            match rest.find("]]>") {
                Some(at) => {
                    i = start + 9 + at + 3;
                    continue;
                }
                None => break,
            }
        }
        if rest.starts_with('?') || rest.starts_with('!') {
            // A processing instruction or the doctype.
            match rest.find('>') {
                Some(at) => {
                    i = start + 2 + at;
                    continue;
                }
                None => break,
            }
        }

        // A real element: up to the ">".
        let Some(end) = rest.find('>') else {
            break;
        };
        let inner = rest[..end].trim_end().trim_end_matches('/');
        let name_end = inner
            .find(|c: char| c.is_whitespace())
            .unwrap_or(inner.len());
        let name = &inner[..name_end];
        let attrs = inner[name_end..].trim();

        // The character data of the element: everything up to the next
        // tag. This is where the **text** of an element lives, and
        // Asterisk puts the state of a dialog in the text of its
        // `<state>` child rather than in an attribute.
        let text_start = start + 1 + end + 1;
        let text_end = text[text_start..]
            .find('<')
            .map(|at| text_start + at)
            .unwrap_or(text.len());
        let content = &text[text_start..text_end];

        out.push(Tag {
            name,
            attrs,
            text: content,
        });
        i = text_end;
    }

    out
}

/// The value of an attribute.
///
/// The name has to match **exactly**: `state` is not `stateful`, and an
/// attribute that is not a name/value pair is not an attribute.
fn attribute(attrs: &str, name: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0;

    while i < attrs.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        let key = &attrs[start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            // Not a name/value pair: skip whatever this was.
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let quote = bytes[i];
        if quote != b'"' && quote != b'\'' {
            // An unquoted value: not something this parser reads.
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }
        i += 1;
        let value_start = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        let value = &attrs[value_start..i.min(attrs.len())];
        i += 1;
        if key == name {
            return Some(unescape(value));
        }
    }

    None
}

/// The five XML entities, and the numeric ones.
fn unescape(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let replacement = match entity {
            "amp" => Some("&".to_string()),
            "lt" => Some("<".to_string()),
            "gt" => Some(">".to_string()),
            "quot" => Some("\"".to_string()),
            "apos" => Some("'".to_string()),
            other => other
                .strip_prefix('#')
                .and_then(
                    |number| match number.strip_prefix('x').or(number.strip_prefix('X')) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => number.parse::<u32>().ok(),
                    },
                )
                .and_then(char::from_u32)
                .map(|c| c.to_string()),
        };
        match replacement {
            Some(text) => {
                out.push_str(&text);
                rest = &rest[end + 1..];
            }
            None => {
                // Not an entity we know: it stays as it was.
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document RFC 3903 describes: the state is an **attribute**
    /// of the `<dialog>`.
    const FREE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<dialog-info xmlns="urn:ietf:params:xml:ns:dialog-info" version="0" state="full" entity="sip:105@193.178.169.164">
  <dialog id="h6hhv876a3dsdjf" state="terminated">
    <local>
      <identity display="Bob"><uri>sip:105@193.178.169.164</uri></identity>
    </local>
    <remote>
      <identity display="Alice"><uri>sip:104@193.178.169.164</uri></identity>
    </remote>
  </dialog>
</dialog-info>"#;

    /// The document RFC 3903 describes: the state is an **attribute**
    /// of the `<dialog>`.
    const BUSY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<dialog-info xmlns="urn:ietf:params:xml:ns:dialog-info" version="0" state="full" entity="sip:105@193.178.169.164">
  <dialog id="h6hhv876a3dsdjf" state="confirmed">
    <local>
      <identity display="Bob"><uri>sip:105@193.178.169.164</uri></identity>
    </local>
    <remote>
      <identity display="Alice"><uri>sip:104@193.178.169.164</uri></identity>
    </remote>
  </dialog>
</dialog-info>"#;

    /// The document **Asterisk** sends for an extension that is free.
    ///
    /// Asterisk puts the state in the **text** of a `<state>` child, not
    /// in an attribute of the `<dialog>` — this is the document that
    /// comes out of `res_pjsip_dialog_info_body_generator`, and the one
    /// the user's own Asterisk sent.
    const ASTERISK_FREE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<dialog-info xmlns="urn:ietf:params:xml:ns:dialog-info" version="3" state="full" entity="sip:105@193.178.169.164:5161">
 <dialog id="105">
  <state>terminated</state>
 </dialog>
</dialog-info>"#;

    /// The document Asterisk sends for an extension that is on a call.
    const ASTERISK_BUSY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<dialog-info xmlns="urn:ietf:params:xml:ns:dialog-info" version="3" state="full" entity="sip:105@193.178.169.164:5161">
 <dialog id="105">
  <state>confirmed</state>
 </dialog>
</dialog-info>"#;

    /// The document Asterisk sends for an extension that is ringing.
    const ASTERISK_RINGING: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<dialog-info xmlns="urn:ietf:params:xml:ns:dialog-info" version="3" state="full" entity="sip:105@193.178.169.164:5161">
 <dialog id="105">
  <state>early</state>
 </dialog>
</dialog-info>"#;

    #[test]
    fn the_state_is_read_out_of_the_element_asterisk_sends() {
        // The state is in the **text** of a `<state>` child, not in an
        // attribute of the `<dialog>`.
        let info = parse(ASTERISK_FREE).expect("the document was not read");
        assert_eq!(info.dialogs.len(), 1);
        assert_eq!(info.dialogs[0].id, "105");
        assert_eq!(
            info.dialogs[0].state,
            DialogState::Terminated,
            "the state in the element was not read"
        );
    }

    #[test]
    fn the_lamp_follows_the_state_asterisk_sends() {
        let mut tracker = Tracker::new();
        assert_eq!(
            tracker.apply(&parse(ASTERISK_BUSY).expect("not read")),
            BlfState::Busy,
            "an extension that is on a call is busy"
        );

        let mut tracker = Tracker::new();
        assert_eq!(
            tracker.apply(&parse(ASTERISK_RINGING).expect("not read")),
            BlfState::Ringing,
            "an extension that is ringing is ringing"
        );

        let mut tracker = Tracker::new();
        assert_eq!(
            tracker.apply(&parse(ASTERISK_FREE).expect("not read")),
            BlfState::Available,
            "an extension with no call is free"
        );
    }

    #[test]
    fn a_dialog_with_no_state_at_all_says_unknown() {
        // The server named a dialog and said nothing about it: that is
        // not a dialog that is over. It is a dialog whose state is
        // unknown, and the lamp says so.
        let xml = r#"<dialog-info state="full" entity="sip:105@example.com">
            <dialog id="x"/>
          </dialog-info>"#;
        let mut tracker = Tracker::new();
        assert_eq!(
            tracker.apply(&parse(xml).expect("the document was not read")),
            BlfState::Unavailable,
            "a dialog with no state is not a dialog that is over"
        );
    }

    #[test]
    fn a_document_is_read_for_the_entity_and_the_dialogs() {
        let info = parse(BUSY).expect("the document was not read");
        assert_eq!(info.entity, "sip:105@193.178.169.164");
        assert_eq!(info.state, "full");
        assert_eq!(info.dialogs.len(), 1);
        assert_eq!(info.dialogs[0].id, "h6hhv876a3dsdjf");
        assert_eq!(info.dialogs[0].state, DialogState::Confirmed);
    }

    #[test]
    fn a_dialog_that_is_over_is_terminated() {
        let info = parse(FREE).expect("the document was not read");
        assert_eq!(info.dialogs[0].state, DialogState::Terminated);
    }

    #[test]
    fn the_text_that_is_not_a_document_is_not_read() {
        assert!(parse("not xml at all").is_none());
        assert!(parse("").is_none());
        assert!(parse("<other-info/>").is_none());
    }

    #[test]
    fn a_document_with_no_dialogs_is_a_document() {
        // An extension with no calls at all is free — and the document
        // that says so is not an error.
        let info = parse(
            r#"<dialog-info xmlns="urn:ietf:params:xml:ns:dialog-info" version="0" state="full" entity="sip:105@example.com"/>"#,
        )
        .expect("the document was not read");
        assert!(info.dialogs.is_empty());
        assert_eq!(info.entity, "sip:105@example.com");
    }

    #[test]
    fn a_comment_that_looks_like_a_dialog_is_not_one() {
        let xml = r#"<dialog-info state="full" entity="sip:105@example.com">
            <!-- <dialog id="x" state="confirmed"/> was here once -->
            <dialog id="real" state="terminated"/>
          </dialog-info>"#;
        let info = parse(xml).expect("the document was not read");
        assert_eq!(info.dialogs.len(), 1, "a comment was read as a dialog");
        assert_eq!(info.dialogs[0].id, "real");
    }

    #[test]
    fn a_state_that_is_not_a_state_is_kept() {
        let xml = r#"<dialog-info state="full" entity="sip:105@example.com">
            <dialog id="x" state="something-new"/>
          </dialog-info>"#;
        let info = parse(xml).expect("the document was not read");
        assert_eq!(
            info.dialogs[0].state,
            DialogState::Other("something-new".to_string())
        );
    }

    #[test]
    fn an_attribute_that_merely_starts_the_same_is_not_it() {
        let xml = r#"<dialog-info state="full" stateful="yes" entity="sip:105@example.com">
            <dialog id="x" state="confirmed" stateful="yes"/>
          </dialog-info>"#;
        let info = parse(xml).expect("the document was not read");
        assert_eq!(info.state, "full");
        assert_eq!(info.dialogs[0].state, DialogState::Confirmed);
    }

    #[test]
    fn an_entity_is_unescaped() {
        let xml = r#"<dialog-info state="full" entity="sip:105@example.com?a=1&amp;b=2"/>"#;
        let info = parse(xml).expect("the document was not read");
        assert_eq!(info.entity, "sip:105@example.com?a=1&b=2");
    }

    #[test]
    fn a_lamp_that_has_heard_nothing_is_unknown() {
        let tracker = Tracker::new();
        assert_eq!(tracker.state(), BlfState::Unavailable);
    }

    #[test]
    fn a_free_extension_is_free() {
        let mut tracker = Tracker::new();
        let info = parse(FREE).expect("the document was not read");
        assert_eq!(tracker.apply(&info), BlfState::Available);
    }

    #[test]
    fn an_extension_that_is_on_a_call_is_busy() {
        let mut tracker = Tracker::new();
        let info = parse(BUSY).expect("the document was not read");
        assert_eq!(tracker.apply(&info), BlfState::Busy);
    }

    #[test]
    fn an_extension_that_is_ringing_is_ringing() {
        let mut tracker = Tracker::new();
        let xml = r#"<dialog-info state="full" entity="sip:105@example.com">
            <dialog id="x" state="trying"/>
          </dialog-info>"#;
        let info = parse(xml).expect("the document was not read");
        assert_eq!(tracker.apply(&info), BlfState::Ringing);
    }

    #[test]
    fn an_extension_with_no_dialogs_is_free() {
        let mut tracker = Tracker::new();
        let xml = r#"<dialog-info state="full" entity="sip:105@example.com"/>"#;
        let info = parse(xml).expect("the document was not read");
        assert_eq!(tracker.apply(&info), BlfState::Available);
    }

    #[test]
    fn a_full_document_replaces_what_was_known() {
        let mut tracker = Tracker::new();
        tracker.apply(&parse(BUSY).expect("the document was not read"));
        assert_eq!(tracker.state(), BlfState::Busy);

        // The call ended: the server sends the whole state again.
        tracker.apply(&parse(FREE).expect("the document was not read"));
        assert_eq!(tracker.state(), BlfState::Available);
    }

    #[test]
    fn a_partial_document_only_changes_the_dialog_it_names() {
        // A partial document carries one dialog. The others are still
        // true, and the lamp has to keep saying so.
        let mut tracker = Tracker::new();
        let xml = r#"<dialog-info state="full" entity="sip:105@example.com">
            <dialog id="a" state="confirmed"/>
            <dialog id="b" state="confirmed"/>
          </dialog-info>"#;
        tracker.apply(&parse(xml).expect("the document was not read"));
        assert_eq!(tracker.state(), BlfState::Busy);

        let partial = r#"<dialog-info state="partial" entity="sip:105@example.com">
            <dialog id="a" state="terminated"/>
          </dialog-info>"#;
        tracker.apply(&parse(partial).expect("the document was not read"));

        assert_eq!(
            tracker.state(),
            BlfState::Busy,
            "the other dialog is still up"
        );
    }

    #[test]
    fn a_partial_document_can_free_the_lamp() {
        let mut tracker = Tracker::new();
        let xml = r#"<dialog-info state="full" entity="sip:105@example.com">
            <dialog id="a" state="confirmed"/>
          </dialog-info>"#;
        tracker.apply(&parse(xml).expect("the document was not read"));

        let partial = r#"<dialog-info state="partial" entity="sip:105@example.com">
            <dialog id="a" state="terminated"/>
          </dialog-info>"#;
        tracker.apply(&parse(partial).expect("the document was not read"));
        assert_eq!(tracker.state(), BlfState::Available);
    }

    #[test]
    fn the_lamp_has_a_word_and_a_colour() {
        assert_eq!(BlfState::Busy.label(), "busy");
        assert_eq!(BlfState::Available.label(), "free");
        assert_eq!(BlfState::Ringing.label(), "ringing");
        assert_eq!(BlfState::Unavailable.label(), "unknown");

        // Every lamp has a colour, and "free" is not the same colour
        // as "we do not know".
        assert_ne!(BlfState::Available.colour(), BlfState::Unavailable.colour());
        assert_ne!(BlfState::Busy.colour(), BlfState::Ringing.colour());
    }
}
