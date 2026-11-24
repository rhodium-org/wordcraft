//! Host modes: a page that runs WordCraft (as an `<iframe>`, or in its own document) opens
//! documents in it and stores what it saves instead of the browser downloading. The messages and
//! the two transports are `craft-web-host`'s (storytold/craft-libs), the same contract as the
//! other Craft apps' bridges, with the `wordcraft:` prefix. This module is WordCraft's part of it.
//!
//! - `?host=parent` (an `<iframe>`, such as a Nextcloud app) talks to the same-origin parent over
//!   `postMessage`; `data-host="page"` on the canvas (the page's own document, no iframe) uses
//!   DOM `CustomEvent`s on the canvas instead. `?host=parent` wins when both are present. With no
//!   parent, or a parent on another origin, there is no bridge: the normal app with downloads.
//! - `wordcraft:ready` once the app is running and listening; send a file now. WordCraft reads
//!   bytes, so it says `file: false` and a host sends `bytes`.
//! - `wordcraft:open` (from the host): `{ name, bytes }`. It opens like a document picked with
//!   File › Open or dropped on the window; a file over [`MAX_OPEN_BYTES`] is refused before it is
//!   copied.
//! - `wordcraft:save` (to the host): `{ name, saveAs, bytes }` for every file WordCraft would
//!   otherwise download. The host stores it and reports failures itself. `saveAs` says whether
//!   the file becomes the document's file:
//!   - Save: `false`. It writes the opened file under its own name and format, as the desktop
//!     app does. A name the parent never sent (an untitled document, an imported .doc saved as
//!     .docx) is a new file: `true`, as for Save As.
//!   - Save As, and File › Export to another document format (which runs Save As): `true` for a
//!     format that keeps everything (.docx and the other Word packages, .odt, .rtf). The document
//!     is now that file; the next Save sends its name with `false`.
//!   - Save As or Export to a format that doesn't (PDF, Markdown, plain text), Export as PDF and
//!     Export Page as PNG: `false`. They are copies; the document's file stays the same.
//! - `wordcraft:dirty` whenever unsaved changes appear or go away, `wordcraft:failed` when the
//!   app couldn't start.
//!
//! - Beyond the crate's contract ([`crate::host_ext`]): `wordcraft:places` lists the host's places
//!   in backstage Open and Save As, choosing one sends `wordcraft:open-request`, and a Save As
//!   into one carries `place`; `wordcraft:document-place` says where a document lives, for File ›
//!   Info and Recent; `wordcraft:autosave {available}` lets AutoSave run through a host that
//!   writes in place; after `wordcraft:confirm-saves {value: true}` each Save carries an `id` and
//!   counts once the host answers `wordcraft:saved {id}`, while `wordcraft:save-failed {id,
//!   message}` leaves the document unsaved and shows why. A save AutoSave sent carries
//!   `auto: true`, so the host can store it without asking anything.
//!
//! In both host modes the host sends the document to edit, so no sample opens, saves are handed
//! to the host rather than downloaded, the host guards leaving the page, and `?author=` names the
//! user for comments and tracked changes.

use std::cell::Cell;
use std::rc::Rc;

use craft_web_host::web::{FileData, Host, Message};
use wordcraft_ui_egui::async_saves::{AsyncSaves, SaveOutcome};
use wordcraft_ui_egui::host_places::{self, HostPlaces};
use wordcraft_ui_egui::{Inbox, Services, WordApp};

use crate::host_ext::Ext;

/// The app id in the host messages (`wordcraft:ready`).
const APP: &str = "wordcraft";

/// The name a document gets when the host sent none (the crate says `document` for a blank name).
const DEFAULT_NAME: &str = "Document.docx";

/// The largest document taken from the host page, refused before it is copied.
const MAX_OPEN_BYTES: u64 = 512 << 20;

/// The longest user name taken from `?author=`.
const MAX_AUTHOR_CHARS: usize = 200;

#[derive(Clone)]
pub struct Bridge {
    host: Rc<Host>,
    ext: Ext,
}

impl Bridge {
    /// The bridge for this page, or `None` when WordCraft isn't hosted: no `?host=parent` and no
    /// `data-host="page"` on the canvas, an unframed `?host=parent` page (it stays the normal app
    /// with downloads), or a parent on another origin.
    pub fn detect(canvas: &web_sys::HtmlCanvasElement) -> Option<Self> {
        let host = Host::connect(APP, canvas)?;
        let ext = Ext::new(APP, host.mode(), canvas)?;
        Some(Self { host: Rc::new(host), ext })
    }

    /// Hand saved and exported files to the host instead of downloading them, report unsaved
    /// changes to it, and hear the files it sends.
    ///
    /// The host's places, AutoSave and confirmed saves ([`crate::host_ext`]) are filled in too;
    /// each stays as it was before until the host declares it.
    pub fn connect(&self, services: &mut Services, inbox: Inbox, ctx: egui::Context) {
        let saves = AsyncSaves::default();
        let places = HostPlaces::default();
        let confirms = Rc::new(Cell::new(false));
        let rewrites = Rc::new(Cell::new(false));
        self.listen(inbox, ctx.clone(), saves.clone());
        self.listen_ext(&places, &saves, &confirms, &rewrites, ctx);
        let (bridge, tickets, place) = (self.clone(), saves.clone(), places.clone());
        services.host_save = Some(Box::new(move |name: &str, bytes: &[u8], save_as: bool| {
            // Numbered only for a host that confirms saves: until it answers, the save isn't done.
            let ticket = confirms.get().then(|| tickets.start());
            let posted = bridge.post_save(
                name,
                bytes,
                ticket.map(|t| t.to_string()).as_deref(),
                save_as,
                place.take_save_place().as_deref(),
                place.take_auto_save(),
            );
            if let (Err(e), Some(ticket)) = (&posted, ticket) {
                tickets.finish(ticket, SaveOutcome::Failed(e.clone()));
            }
            posted
        }));
        // Anything else that would download goes to the host too: copies, never waited for.
        let bridge = self.clone();
        services.download = Some(Box::new(move |name: &str, bytes: &[u8]| bridge.post_save(name, bytes, None, false, None, false)));
        // The browser's save picker is never used; the saves that finish later are the host's.
        services.async_saves = Some(saves);
        services.host_places = Some(places);
        let ext = self.ext.clone();
        services.host_open = Some(Box::new(move |place: &str, key: Option<&str>| ext.post_open_request(place, key)));
        services.host_rewrites = Some(Box::new(move || rewrites.get()));
        // The crate does not repeat a state it already reported.
        let host = self.host.clone();
        services.on_dirty = Some(Box::new(move |dirty: bool| host.post_dirty(dirty)));
    }

    /// `?author=`: the host's name for the user signs comments and tracked changes.
    pub fn set_author(app: &mut WordApp, author: &str) {
        let author: String = author.trim().chars().take(MAX_AUTHOR_CHARS).collect();
        if !author.is_empty() {
            app.session.author = author;
        }
    }

    pub fn post_ready(&self) {
        let _ = self.host.post_ready(env!("CARGO_PKG_VERSION"), false);
    }

    /// The app couldn't start (no WebGPU or WebGL2, for example): let the host say so.
    pub fn post_failed(&self, error: &str) {
        let _ = self.host.post_failed(error, None);
    }

    fn post_save(&self, path: &str, bytes: &[u8], id: Option<&str>, save_as: bool, place: Option<&str>, auto: bool) -> Result<(), String> {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        self.ext.post_save(&name, bytes, id, save_as, place, auto)
    }

    /// Hear `wordcraft:open` and queue the file like a dropped one, and `wordcraft:saved` for a
    /// numbered save.
    fn listen(&self, inbox: Inbox, ctx: egui::Context, saves: AsyncSaves) {
        self.host.listen(move |message| match message {
            Message::Open { name, data } => deliver_open(&inbox, &ctx, &name, &data),
            Message::Saved { id } => {
                if let Ok(ticket) = id.parse::<u64>() {
                    saves.finish(ticket, SaveOutcome::Saved);
                    ctx.request_repaint();
                }
            }
            _ => {}
        });
    }

    /// Hear the extension messages ([`crate::host_ext`]).
    fn listen_ext(&self, places: &HostPlaces, saves: &AsyncSaves, confirms: &Rc<Cell<bool>>, rewrites: &Rc<Cell<bool>>, ctx: egui::Context) {
        let (places, saves, confirms, rewrites) = (places.clone(), saves.clone(), confirms.clone(), rewrites.clone());
        self.ext.listen(move |kind, message| {
            match kind {
                "places" => places.set(host_places::places_from(&message)),
                "document-place" => {
                    if let Some(place) = host_places::document_place_from(&message) {
                        places.arrived(place);
                    }
                }
                "autosave" => rewrites.set(host_places::flag(&message, "available")),
                "confirm-saves" => confirms.set(host_places::flag(&message, "value")),
                "save-failed" => {
                    if let Some((ticket, outcome)) = host_places::save_failed_from(&message) {
                        saves.finish(ticket, outcome);
                    }
                }
                _ => return,
            }
            ctx.request_repaint();
        });
    }
}

/// The file an open message carries, pushed onto the inbox for the next frame, like a dropped
/// file. A host's file is untrusted input: an oversized one is refused before it is copied into
/// the app's memory (the data's own length, not the host's claim). The crate has cleaned the name
/// (and cut it to 255 characters); a blank one comes as `document` and becomes `Document.docx`.
fn deliver_open(inbox: &Inbox, ctx: &egui::Context, name: &str, data: &FileData) {
    let name = if name == "document" { DEFAULT_NAME } else { name }.to_string();
    let FileData::Bytes(array) = data else {
        log::warn!("host file {name} came as a File; WordCraft reads bytes only; not opened");
        return;
    };
    if data.size() > MAX_OPEN_BYTES as f64 {
        log::warn!("host file is {} bytes, over the {} MiB host open limit; not opened", data.size(), MAX_OPEN_BYTES >> 20);
        return;
    }
    inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, array.to_vec()));
    ctx.request_repaint();
}
