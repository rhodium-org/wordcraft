//! The host messages beyond `craft-web-host`'s contract (rhodium-org fork, offered upstream): a
//! host's places, AutoSave through a host that writes in place, and saves a host can refuse. They
//! travel the same way as the crate's (`postMessage` to and from the same-origin parent, or
//! `CustomEvent`s on the canvas), with the same `wordcraft:` prefix, so they belong in the crate
//! once upstream takes them; until then this module carries them for WordCraft alone.
//!
//! | Kind | Direction | Fields |
//! |---|---|---|
//! | `places` | host → app | `places: [{ id, label, icon?, open, saveAs }]`, replacing the list |
//! | `open-request` | app → host | `place`, `key?` (a Recent document the host said lived there) |
//! | `document-place` | host → app | `place`, `key?`, `label?`: where the document just opened or saved lives |
//! | `autosave` | host → app | `available`: the host writes Save back in place, so AutoSave may run |
//! | `confirm-saves` | host → app | `value`: from now on each Save carries an `id` and counts once the host answers |
//! | `save` | app → host | the crate's fields, `place?` (a Save As into one of the host's places) and `auto?` (AutoSave sent it) |
//! | `document` | app → host | `id`, `name?`, `title`, `open`, `dirty`, `documents` (every open one's id): the document shown, sent when it, its file's name or the open documents change |
//! | `save-failed` | host → app | `id`, `message?`: the save was not stored; the document stays unsaved |
//!
//! The crate's `saved {id}` confirms a save. A host that never sends `confirm-saves` sees saves
//! without an `id`, as before.

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use web_sys::js_sys;

use craft_web_host::{Mode, file_name, message_type};

/// The kinds this module hears from the host.
pub const INCOMING: [&str; 5] = ["places", "document-place", "autosave", "confirm-saves", "save-failed"];

#[derive(Clone)]
enum Target {
    /// The same-origin parent window; only it is heard and answered.
    Parent { window: web_sys::Window, parent: web_sys::Window, origin: String },
    /// The app's canvas; messages are `CustomEvent`s on it.
    Page { canvas: web_sys::EventTarget },
}

/// The extension messages' side of the connection to the hosting page.
#[derive(Clone)]
pub struct Ext {
    app: &'static str,
    target: Target,
}

impl Ext {
    /// The same transport `craft_web_host::web::Host::connect` chose (`mode`): it has already
    /// checked that a parent is framing the app from the page's own origin.
    pub fn new(app: &'static str, mode: Mode, canvas: &web_sys::Element) -> Option<Self> {
        let target = match mode {
            Mode::Parent => {
                let window = web_sys::window()?;
                let parent = window.parent().ok().flatten()?;
                let origin = window.location().origin().ok()?;
                Target::Parent { window, parent, origin }
            }
            Mode::Page => Target::Page { canvas: canvas.clone().into() },
        };
        Some(Self { app, target })
    }

    /// Hear the host's extension messages: `(kind, fields as JSON)`. A message that isn't plain
    /// data (no JSON form) arrives as `null`.
    pub fn listen(&self, handler: impl Fn(&str, serde_json::Value) + 'static) {
        let app = self.app;
        let handler = std::rc::Rc::new(handler);
        match &self.target {
            Target::Parent { window, parent, origin } => {
                let (parent, origin) = (parent.clone(), origin.clone());
                let on_message = Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                    if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                        return;
                    }
                    let data = event.data();
                    let Some(kind) = kind_of(app, &data) else { return };
                    handler(kind, json(&data));
                });
                if window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
                    on_message.forget();
                }
            }
            Target::Page { canvas } => {
                for kind in INCOMING {
                    let handler = handler.clone();
                    let on_event = Closure::<dyn Fn(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
                        handler(kind, json(&event.detail()));
                    });
                    let name = message_type(app, kind);
                    if canvas.add_event_listener_with_callback(&name, on_event.as_ref().unchecked_ref()).is_ok() {
                        on_event.forget();
                    }
                }
            }
        }
    }

    /// `save`: the crate's fields (`name`, `bytes`, `saveAs`, `id?`), `place?` and `auto?`. The bytes go as
    /// a transferred `ArrayBuffer`.
    pub fn post_save(&self, name: &str, bytes: &[u8], id: Option<&str>, save_as: bool, place: Option<&str>, auto: bool) -> Result<(), String> {
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let mut fields =
            vec![("name", JsValue::from(file_name(Some(name), "document"))), ("bytes", buffer.clone().into()), ("saveAs", save_as.into())];
        if let Some(id) = id {
            fields.push(("id", id.into()));
        }
        if let Some(place) = place {
            fields.push(("place", place.into()));
        }
        if auto {
            fields.push(("auto", true.into()));
        }
        self.post("save", &fields, Some(&buffer))
    }

    /// `document`: the document the window shows now ([`wordcraft_ui_egui::ShownDocument`]).
    pub fn post_document(&self, shown: &wordcraft_ui_egui::ShownDocument) -> Result<(), String> {
        let mut fields = vec![
            ("id", JsValue::from(shown.id.to_string())),
            ("title", JsValue::from(shown.title.as_str())),
            ("open", shown.open.into()),
            ("dirty", shown.dirty.into()),
            ("documents", shown.documents.iter().map(|id| JsValue::from(id.to_string())).collect::<js_sys::Array>().into()),
        ];
        if let Some(name) = &shown.name {
            fields.push(("name", name.as_str().into()));
        }
        self.post("document", &fields, None)
    }

    /// `open-request`: ask the host for a document from `place` (`key`: one it said lived there).
    pub fn post_open_request(&self, place: &str, key: Option<&str>) -> Result<(), String> {
        let mut fields = vec![("place", JsValue::from(place))];
        if let Some(key) = key {
            fields.push(("key", key.into()));
        }
        self.post("open-request", &fields, None)
    }

    fn post(&self, kind: &str, fields: &[(&str, JsValue)], transfer: Option<&js_sys::ArrayBuffer>) -> Result<(), String> {
        let message_type = message_type(self.app, kind);
        let error = |e: JsValue| e.as_string().unwrap_or_else(|| format!("{e:?}"));
        match &self.target {
            Target::Parent { parent, origin, .. } => {
                let message = object(&[("type", message_type.as_str().into())], fields)?;
                match transfer {
                    Some(buffer) => parent.post_message_with_transfer(&message, origin, &js_sys::Array::of1(buffer)),
                    None => parent.post_message(&message, origin),
                }
                .map_err(error)
            }
            Target::Page { canvas } => {
                let init = web_sys::CustomEventInit::new();
                init.set_detail(&object(&[], fields)?.into());
                let event = web_sys::CustomEvent::new_with_event_init_dict(&message_type, &init).map_err(error)?;
                canvas.dispatch_event(&event).map(|_| ()).map_err(error)
            }
        }
    }
}

/// The extension kind a parent's message names for `app`, or `None` for anything else (the
/// crate's own kinds, another app's messages).
fn kind_of(app: &str, data: &JsValue) -> Option<&'static str> {
    let message_type = js_sys::Reflect::get(data, &JsValue::from_str("type")).ok()?.as_string()?;
    let kind = message_type.strip_prefix(app)?.strip_prefix(':')?;
    INCOMING.into_iter().find(|k| *k == kind)
}

/// A host's message as JSON, read through the browser's own `JSON.stringify`; `null` when it has
/// no JSON form (a cycle, a function).
fn json(data: &JsValue) -> serde_json::Value {
    js_sys::JSON::stringify(data).ok().and_then(|s| s.as_string()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(serde_json::Value::Null)
}

fn object(head: &[(&str, JsValue)], fields: &[(&str, JsValue)]) -> Result<js_sys::Object, String> {
    let m = js_sys::Object::new();
    for (key, value) in head.iter().chain(fields) {
        js_sys::Reflect::set(&m, &JsValue::from_str(key), value).map_err(|_| "couldn't build the message".to_string())?;
    }
    Ok(m)
}
