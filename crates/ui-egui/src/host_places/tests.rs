use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;

use super::{DocumentPlace, HostPlace, HostPlaces, MAX_PLACES, document_place_from, flag, places_from, save_failed_from};
use crate::async_saves::{AsyncSaves, SaveOutcome};
use crate::{AutoSaveBlock, Services, Session, WordApp};

fn github() -> HostPlace {
    HostPlace { id: "github".into(), label: "GitHub".into(), icon: None, open: true, save_as: true }
}

/// What a hosted app handed its host: saves as (name, save_as, place) and open requests as (place, key).
#[derive(Default)]
struct Seen {
    saves: Vec<(String, bool, Option<String>)>,
    /// The documents the host was told were shown, each pass.
    shown: Vec<crate::ShownDocument>,
    /// Whether each save was AutoSave's.
    autos: Vec<bool>,
    opens: Vec<(String, Option<String>)>,
}

/// An app hosted by a page, as the web shell's bridge sets it up: saves go to the host (numbered
/// when `confirms`), places and open requests through it, AutoSave while `rewrites` says so.
fn hosted(confirms: bool, rewrites: Rc<std::cell::Cell<bool>>) -> (WordApp, HostPlaces, AsyncSaves, Rc<RefCell<Seen>>) {
    let places = HostPlaces::default();
    let saves = AsyncSaves::default();
    let seen = Rc::new(RefCell::new(Seen::default()));
    let (p, s, log) = (places.clone(), saves.clone(), seen.clone());
    let host_save: crate::HostSaveFn = Box::new(move |name: &str, _: &[u8], save_as: bool| {
        if confirms {
            s.start();
        }
        log.borrow_mut().saves.push((name.to_string(), save_as, p.take_save_place()));
        log.borrow_mut().autos.push(p.take_auto_save());
        Ok(())
    });
    let log = seen.clone();
    let shown_log = seen.clone();
    let services = Services {
        download: Some(Box::new(|_: &str, _: &[u8]| Ok(()))),
        host_save: Some(host_save),
        async_saves: Some(saves.clone()),
        host_places: Some(places.clone()),
        host_open: Some(Box::new(move |place: &str, key: Option<&str>| {
            log.borrow_mut().opens.push((place.to_string(), key.map(str::to_string)));
            Ok(())
        })),
        host_rewrites: Some(Box::new(move || rewrites.get())),
        on_document: Some(Box::new(move |shown: &crate::ShownDocument| shown_log.borrow_mut().shown.push(shown.clone()))),
        ..Default::default()
    };
    (WordApp::new(Session::new(wordcraft_doc::Document::new()), services), places, saves, seen)
}

#[test]
fn a_hosts_places_are_read_and_junk_is_skipped() {
    let message = json!({"places": [
        {"id": "github", "label": "GitHub", "open": true, "saveAs": true, "icon": "data:image/svg+xml;base64,AAAA"},
        {"id": "github", "label": "Again", "open": true},
        {"id": "drive", "label": " Drive\n ", "saveAs": true, "icon": "javascript:alert(1)"},
        {"id": "nowhere", "label": "Neither"},
        {"label": "No id", "open": true},
        {"id": "x", "open": true},
        {"id": 3, "label": "Number id", "open": true},
        "a string",
        null
    ]});
    let got = places_from(&message);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].id, "github");
    assert!(got[0].icon.is_some());
    assert_eq!(got[1], HostPlace { id: "drive".into(), label: "Drive".into(), icon: None, open: false, save_as: true });

    let many = json!({"places": (0..40).map(|i| json!({"id": format!("p{i}"), "label": "P", "open": true})).collect::<Vec<_>>()});
    assert_eq!(places_from(&many).len(), MAX_PLACES);
    for junk in [json!(null), json!({}), json!({"places": "github"}), json!([1, 2])] {
        assert!(places_from(&junk).is_empty());
    }
}

#[test]
fn a_document_place_needs_a_place_and_a_key_or_a_label() {
    let both = document_place_from(
        &json!({"place": "github", "key": "acme/papers@main:docs/Report.docx", "label": "acme/papers · main · docs/Report.docx"}),
    );
    assert_eq!(both.map(|d| d.key), Some("acme/papers@main:docs/Report.docx".into()));
    let label_only = document_place_from(&json!({"place": "github", "label": "Report"})).map(|d| (d.key, d.label));
    assert_eq!(label_only, Some(("Report".into(), "Report".into())));
    for junk in [json!({"place": "github"}), json!({"key": "k", "label": "l"}), json!({"place": "", "key": "k"}), json!(null)] {
        assert_eq!(document_place_from(&junk), None, "{junk}");
    }
}

/// A Recent document that came from a host's place goes back to the host to reopen, instead of
/// the browser being asked for a file it never had.
#[test]
fn a_recent_document_from_a_place_reopens_through_the_host() {
    let (mut a, places, _, seen) = hosted(false, Rc::default());
    places.set(vec![github()]);
    a.session.path = Some("Report.docx".into());
    let place =
        DocumentPlace { place: "github".into(), key: "acme/papers@main:Report.docx".into(), label: "acme/papers · main · Report.docx".into() };
    places.arrived(place.clone());
    a.settle_host_places();
    assert_eq!(a.ui.recent.first().map(String::as_str), Some("Report.docx"));
    assert_eq!(a.ui.recent_places.get("Report.docx"), Some(&place));
    assert_eq!(places.document(a.session.document_id()), Some(place.clone()));
    assert_eq!(a.run("ui.hostPlaces", json!({})).unwrap()["document"]["label"], json!(place.label));

    crate::backstage::open_recent(&mut a, "Report.docx").unwrap();
    assert_eq!(seen.borrow().opens, vec![("github".to_string(), Some(place.key.clone()))]);

    // Taken off the list, it forgets where it lived.
    a.run("ui.recentRemove", json!({"path": "Report.docx"})).unwrap();
    assert!(a.ui.recent_places.is_empty());
}

#[test]
fn open_and_save_as_name_a_place_the_host_offers() {
    let (mut a, places, _, seen) = hosted(false, Rc::default());
    places.set(vec![github(), HostPlace { id: "inbox".into(), label: "Inbox".into(), icon: None, open: true, save_as: false }]);

    assert_eq!(a.run("ui.hostOpen", json!({"place": "github"})).unwrap(), json!({"asked": "github"}));
    assert_eq!(seen.borrow().opens, vec![("github".to_string(), None)]);
    assert!(a.run("ui.hostOpen", json!({"place": "elsewhere"})).is_err());

    a.run("text.insert", json!({"text": "Hello"})).unwrap();
    a.run("ui.hostSaveAs", json!({"place": "github"})).unwrap();
    let saves = std::mem::take(&mut seen.borrow_mut().saves);
    assert_eq!(saves.len(), 1);
    assert_eq!(saves[0].2.as_deref(), Some("github"), "the save names its place");
    assert!(saves[0].1, "a Save As");
    assert!(a.run("ui.hostSaveAs", json!({"place": "inbox"})).is_err(), "a place that takes no saves");

    // A later save without a place chosen leaves it to the host.
    a.run("text.insert", json!({"text": "!"})).unwrap();
    a.run("file.save", json!({})).unwrap();
    assert_eq!(seen.borrow().saves.last().and_then(|s| s.2.clone()), None);

    for junk in [json!(null), json!({}), json!({"place": 3}), json!([1])] {
        for id in ["ui.hostOpen", "ui.hostSaveAs"] {
            assert!(a.run(id, junk.clone()).is_err(), "{id} {junk}");
        }
    }
    assert!(a.run("ui.hostOpen", json!({"place": "github", "key": 7})).is_err());
}

/// Without a host the place commands say so; a plain web build lists no places.
#[test]
fn without_a_host_there_are_no_places() {
    let mut a = WordApp::new(Session::new(wordcraft_doc::Document::new()), Services::default());
    assert_eq!(a.run("ui.hostPlaces", json!({})).unwrap()["places"], json!([]));
    assert!(a.run("ui.hostOpen", json!({"place": "github"})).is_err());
    assert!(a.run("ui.hostSaveAs", json!({"place": "github"})).is_err());
}

/// The browser block on AutoSave stays for a plain web build and for a host that doesn't write in
/// place; a host that does lifts it, and AutoSave's writes go to the host.
#[test]
fn autosave_follows_whether_the_host_writes_in_place() {
    let plain = Services { download: Some(Box::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let plain = WordApp::new(Session::new(wordcraft_doc::Document::new()), plain);
    assert_eq!(plain.autosave_block(), Some(AutoSaveBlock::Browser));

    let rewrites = Rc::new(std::cell::Cell::new(false));
    let (mut a, places, _, seen) = hosted(false, rewrites.clone());
    a.session.path = Some("Report.docx".into());
    assert_eq!(a.autosave_block(), Some(AutoSaveBlock::Browser));
    assert!(a.run("file.autosave", json!({"value": true})).is_err());

    rewrites.set(true);
    assert_eq!(a.autosave_block(), None, "a document the host sent is saved where it came from");
    a.session.path = Some("notes.txt".into());
    assert!(matches!(a.autosave_block(), Some(AutoSaveBlock::Format { .. })), "a format that doesn't keep everything still blocks");
    a.session.path = Some("Report.docx".into());
    a.run("file.autosave", json!({"value": true})).unwrap();
    assert!(a.autosaves());

    a.run("text.insert", json!({"text": "Hello"})).unwrap();
    a.autosave_tick(crate::now_ms() + 60_000.0);
    assert_eq!(seen.borrow().saves.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["Report.docx"]);
    assert!(!a.session.dirty);
    assert_eq!(seen.borrow().autos, [true], "the host hears that AutoSave sent it");

    a.run("text.insert", json!({"text": " again"})).unwrap();
    a.run("file.save", json!({})).unwrap();
    assert_eq!(seen.borrow().autos, [true, false], "a Save the user asked for is not marked");
    assert!(!places.take_auto_save(), "nothing is left marked for a later save");

    rewrites.set(false);
    assert!(!a.autosaves(), "the host can take it back");
}

/// A host that confirms saves: the document counts as saved only once the host says it stored
/// it; a refusal leaves it unsaved and says why.
#[test]
fn a_save_the_host_refuses_stays_unsaved_and_says_why() {
    let (mut a, _, saves, _) = hosted(true, Rc::default());
    a.session.path = Some("Report.docx".into());
    a.run("text.insert", json!({"text": "Hello"})).unwrap();
    let r = a.run("file.save", json!({})).unwrap();
    let ticket = r["pendingSave"].as_u64().unwrap();
    assert!(a.save_unconfirmed(), "posted, not yet stored");
    assert_eq!(saves.pending(), 1);

    saves.finish(ticket, SaveOutcome::Failed("the branch has moved on".into()));
    a.settle_async_saves();
    assert!(a.session.dirty, "a refused save keeps the changes unsaved");
    let status = a.status_msg.clone().map(|s| s.0).unwrap_or_default();
    assert!(status.contains("the branch has moved on") && status.contains("Report.docx"), "{status}");

    let r = a.run("file.save", json!({})).unwrap();
    saves.finish(r["pendingSave"].as_u64().unwrap(), SaveOutcome::Saved);
    a.settle_async_saves();
    assert!(!a.session.dirty && !a.save_unconfirmed());
}

#[test]
fn a_save_failed_message_names_a_numbered_save() {
    assert_eq!(save_failed_from(&json!({"id": "7", "message": "conflict"})), Some((7, SaveOutcome::Failed("conflict".into()))));
    assert!(matches!(save_failed_from(&json!({"id": 8})), Some((8, SaveOutcome::Failed(why))) if !why.is_empty()));
    for junk in [json!({}), json!({"id": "x"}), json!({"id": -1}), json!({"id": null}), json!(null)] {
        assert_eq!(save_failed_from(&junk), None, "{junk}");
    }
    assert!(flag(&json!({"available": true}), "available"));
    assert!(!flag(&json!({"available": "yes"}), "available") && !flag(&json!(null), "available"));
}

/// A host that declares places to open from owns opening: WordCraft's Open (Ctrl+O, File ›
/// Open's Browse…) shows its places instead of the browser's picker, so no document arrives
/// behind the host's back. A host with no such place leaves the picker as it was.
#[test]
fn a_host_with_open_places_owns_opening() {
    let picked = Rc::new(std::cell::Cell::new(0));
    let (mut a, places, _, _) = hosted(false, Rc::default());
    let count = picked.clone();
    a.services.open_async = Some(Box::new(move |_: &str| count.set(count.get() + 1)));

    a.run("ui.openFileDialog", json!({})).unwrap();
    assert_eq!(picked.get(), 1, "no places: the browser's picker");

    places.set(vec![HostPlace { id: "computer".into(), label: "This computer".into(), icon: None, open: true, save_as: true }, github()]);
    a.ui.backstage = false;
    a.run("ui.openFileDialog", json!({})).unwrap();
    assert_eq!(picked.get(), 1, "the picker is not used");
    assert!(a.ui.backstage && a.ui.backstage_page == "open", "Open shows the host's places");
    assert!(a.host_owns("open") && a.host_owns("saveAs"));

    places.set(vec![HostPlace { id: "drop".into(), label: "Drop box".into(), icon: None, open: false, save_as: true }]);
    assert!(!a.host_owns("open") && a.host_owns("saveAs"));
}

/// A document the host sends without saying where it lives forgets the place its name had, so
/// its Recent entry opens it as a plain file rather than from the wrong place.
#[test]
fn a_host_file_without_a_place_forgets_the_old_one() {
    let (mut a, places, _, _) = hosted(false, Rc::default());
    places.set(vec![github()]);
    let place = DocumentPlace { place: "github".into(), key: "k".into(), label: "l".into() };
    a.ui.recent_places.insert("Report.docx".into(), place.clone());
    let inbox: crate::Inbox = Default::default();
    a.services.inbox = Some(inbox.clone());
    let doc = wordcraft_engine::io::save_bytes_with("Report.docx", &wordcraft_doc::Document::new(), None, &Default::default()).unwrap();
    inbox.lock().unwrap().push(("Report.docx".into(), doc.clone()));
    a.drain_inbox();
    a.settle_host_places();
    assert!(!a.ui.recent_places.contains_key("Report.docx"));

    // Sent with its place, it keeps one.
    inbox.lock().unwrap().push(("Report.docx".into(), doc));
    places.arrived(place.clone());
    a.drain_inbox();
    a.settle_host_places();
    assert_eq!(a.ui.recent_places.get("Report.docx"), Some(&place));
}

/// While a save waits for the host's answer the host is told the document is unsaved, so it
/// keeps guarding the tab; the answer clears it, and an edit made meanwhile keeps it.
#[test]
fn the_host_hears_unsaved_until_the_save_is_confirmed() {
    let (mut a, _, saves, _) = hosted(true, Rc::default());
    let heard = Rc::new(std::cell::Cell::new(None));
    let h = heard.clone();
    a.services.on_dirty = Some(Box::new(move |d: bool| h.set(Some(d))));
    a.session.path = Some("Report.docx".into());
    a.run("text.insert", json!({"text": "Hello"})).unwrap();
    let ticket = a.run("file.save", json!({})).unwrap()["pendingSave"].as_u64().unwrap();
    a.report_dirty();
    assert_eq!(heard.get(), Some(true), "handed over, not yet written");
    saves.finish(ticket, SaveOutcome::Saved);
    a.settle_async_saves();
    a.report_dirty();
    assert_eq!(heard.get(), Some(false));

    let ticket = a.run("text.insert", json!({"text": "!"})).and_then(|_| a.run("file.save", json!({}))).unwrap()["pendingSave"].as_u64().unwrap();
    a.run("text.insert", json!({"text": " more"})).unwrap();
    saves.finish(ticket, SaveOutcome::Saved);
    a.settle_async_saves();
    a.report_dirty();
    assert_eq!(heard.get(), Some(true), "typed while the save was out");
}

/// A host page hears which document the window shows: a new one, and the first again after
/// Switch Windows, so its own controls follow the document the person is working on.
#[test]
fn the_host_hears_which_document_is_shown() {
    let (mut a, _, _, seen) = hosted(true, Rc::new(std::cell::Cell::new(true)));
    a.session.path = Some("Report.docx".into());
    a.report_document();
    let first = seen.borrow().shown.last().cloned().expect("told at once");
    assert_eq!(first.name.as_deref(), Some("Report.docx"));

    a.run("file.new", json!({})).unwrap();
    a.report_document();
    let new = seen.borrow().shown.last().cloned().expect("told of the new document");
    assert_ne!(new.id, first.id, "a new document is another one");
    assert_eq!(new.name, None, "it has no file yet");
    assert!(new.open && !new.title.is_empty(), "{new:?}");

    a.run("view.switchWindows", json!({"id": first.id})).unwrap();
    a.report_document();
    let back = seen.borrow().shown.last().cloned().expect("told of the switch");
    assert_eq!((back.id, back.name.as_deref()), (first.id, Some("Report.docx")));
    assert!(back.documents.contains(&first.id) && back.documents.contains(&new.id), "both are open: {back:?}");
}
