//! Web, hosted: the places a host page keeps documents in, beside this computer (rhodium-org
//! fork; offered upstream as host-provided places).
//!
//! A host declares them once with `wordcraft:places`, and backstage Open and Save As list them
//! beside Browse…. Choosing one asks the host (`wordcraft:open-request`), or saves through it with
//! the place named (`wordcraft:save` with `place`). After an open or a save the host may say where
//! the document lives (`wordcraft:document-place`): File › Info shows it, and the Recent entry
//! remembers it, so a document from the host reopens through the host instead of failing to open
//! as a file of this computer.
//!
//! The web shell's bridge writes into a [`HostPlaces`] from the host's messages and reads the
//! place a Save As goes to; the app reads it every frame. Plain `Rc<RefCell<…>>` state, like
//! [`crate::async_saves`]: everything runs on the page's one thread.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most places a host can declare; the rest are ignored.
pub const MAX_PLACES: usize = 16;
/// The longest place label, document label or key taken from a host, in characters.
pub const MAX_LABEL_CHARS: usize = 200;
/// The longest place id taken from a host, in characters.
pub const MAX_ID_CHARS: usize = 64;

/// A place the host keeps documents in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPlace {
    /// The host's own name for it, sent back in `open-request` and `save`.
    pub id: String,
    /// Shown as given.
    pub label: String,
    /// A data URI the host offered. Kept and reported, not drawn yet.
    pub icon: Option<String>,
    /// Backstage Open lists it.
    pub open: bool,
    /// Backstage Save As lists it.
    pub save_as: bool,
}

/// Where a document lives in a host's place: the place, the host's key for the document there
/// (sent back to reopen it) and the label File › Info and Recent show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentPlace {
    pub place: String,
    pub key: String,
    pub label: String,
}

#[derive(Default)]
struct State {
    places: Vec<HostPlace>,
    /// `document-place` messages not yet given to the document shown.
    arrived: Vec<DocumentPlace>,
    /// Where each open document lives ([`crate::Session::document_id`]).
    documents: BTreeMap<u64, DocumentPlace>,
    /// The place the next save goes to (a Save As from backstage), taken once by the bridge.
    save_place: Option<String>,
}

/// The host's places and where its documents live, shared between the bridge and the app (a cheap
/// clone).
#[derive(Clone, Default)]
pub struct HostPlaces(Rc<RefCell<State>>);

impl HostPlaces {
    /// `wordcraft:places`: replace the list (an empty list removes them).
    pub fn set(&self, places: Vec<HostPlace>) {
        self.0.borrow_mut().places = places;
    }

    pub fn list(&self) -> Vec<HostPlace> {
        self.0.borrow().places.clone()
    }

    pub fn get(&self, id: &str) -> Option<HostPlace> {
        self.0.borrow().places.iter().find(|p| p.id == id).cloned()
    }

    /// `wordcraft:document-place`: the document just opened or saved lives here. It goes to the
    /// document shown at the next frame ([`crate::WordApp::settle_host_places`]), after the file
    /// the host sent with it has opened.
    pub fn arrived(&self, place: DocumentPlace) {
        self.0.borrow_mut().arrived.push(place);
    }

    pub(crate) fn take_arrived(&self) -> Vec<DocumentPlace> {
        std::mem::take(&mut self.0.borrow_mut().arrived)
    }

    pub(crate) fn set_document(&self, document: u64, place: DocumentPlace) {
        self.0.borrow_mut().documents.insert(document, place);
    }

    /// Where the document lives, if the host said.
    pub fn document(&self, document: u64) -> Option<DocumentPlace> {
        self.0.borrow().documents.get(&document).cloned()
    }

    pub(crate) fn set_save_place(&self, place: Option<String>) {
        self.0.borrow_mut().save_place = place;
    }

    /// The bridge: the place this save goes to, if one was chosen (claimed once).
    pub fn take_save_place(&self) -> Option<String> {
        self.0.borrow_mut().save_place.take()
    }
}

/// Text from a host: control characters dropped, surrounding space trimmed, cut to `max`
/// characters; `None` when nothing is left.
fn clean(raw: Option<&Value>, max: usize) -> Option<String> {
    let s: String = raw?.as_str()?.chars().filter(|c| !c.is_control()).collect();
    let s = s.trim();
    (!s.is_empty()).then(|| s.chars().take(max).collect())
}

/// The places a `wordcraft:places` message declares. A host's message is untrusted input: an
/// entry without an id or a label, or that offers neither Open nor Save As, is skipped, a second
/// entry with the same id too, and only the first [`MAX_PLACES`] count. An icon must be an image
/// data URI.
pub fn places_from(message: &Value) -> Vec<HostPlace> {
    let mut out: Vec<HostPlace> = Vec::new();
    let Some(entries) = message.get("places").and_then(Value::as_array) else { return out };
    for entry in entries {
        if out.len() == MAX_PLACES {
            break;
        }
        let (Some(id), Some(label)) = (clean(entry.get("id"), MAX_ID_CHARS), clean(entry.get("label"), MAX_LABEL_CHARS)) else { continue };
        let open = entry.get("open").and_then(Value::as_bool) == Some(true);
        let save_as = entry.get("saveAs").and_then(Value::as_bool) == Some(true);
        if !(open || save_as) || out.iter().any(|p| p.id == id) {
            continue;
        }
        let icon = entry.get("icon").and_then(Value::as_str).filter(|s| s.starts_with("data:image/")).map(str::to_string);
        out.push(HostPlace { id, label, icon, open, save_as });
    }
    out
}

/// The place a `wordcraft:document-place` message names, or `None` without a place. The key
/// defaults to the label, and the label to the key.
pub fn document_place_from(message: &Value) -> Option<DocumentPlace> {
    let place = clean(message.get("place"), MAX_ID_CHARS)?;
    let key = clean(message.get("key"), MAX_LABEL_CHARS);
    let label = clean(message.get("label"), MAX_LABEL_CHARS);
    let (key, label) = match (key, label) {
        (Some(k), Some(l)) => (k, l),
        (Some(k), None) => (k.clone(), k),
        (None, Some(l)) => (l.clone(), l),
        (None, None) => return None,
    };
    Some(DocumentPlace { place, key, label })
}

/// A host's on/off switch (`autosave`'s `available`, `confirm-saves`' `value`): on only for `true`.
pub fn flag(message: &Value, field: &str) -> bool {
    message.get(field).and_then(Value::as_bool) == Some(true)
}

/// The save a `wordcraft:save-failed` message reports, as its ticket and outcome, or `None`
/// without the `id` of a numbered save. The host's message, cut to [`MAX_LABEL_CHARS`], says why;
/// without one the outcome says the host didn't store it.
pub fn save_failed_from(message: &Value) -> Option<(u64, crate::async_saves::SaveOutcome)> {
    let id = message.get("id")?;
    let ticket = id.as_u64().or_else(|| id.as_str().and_then(|s| s.parse().ok()))?;
    let why = clean(message.get("message"), MAX_LABEL_CHARS).unwrap_or_else(|| tl!("the page didn't store it").to_string());
    Some((ticket, crate::async_saves::SaveOutcome::Failed(why)))
}

#[cfg(test)]
mod tests;
