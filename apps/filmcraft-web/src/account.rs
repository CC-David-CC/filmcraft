//! WebOS account project startup and the browser's immutable media table.

use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

use filmcraft_engine::{Session, commands::PreparedAccountSave, project::MediaRef};
use filmcraft_ui_egui::{AccountProjectFn, AccountProjectRequest, FilmcraftApp};
use serde_json::{Value, json};
use wasm_bindgen::JsValue;

use crate::{
    account_port::{AccountPort, field, string},
    fs, hostmode,
    import::{on_ui, prewarm},
};

pub(crate) struct AccountView {
    port: AccountPort,
    opened: bool,
    action: Rc<RefCell<Option<AccountProjectRequest>>>,
    last_state: Value,
    pending: Option<Save>,
    dialog: Option<(String, Option<u64>)>,
    importing_media: bool,
    sequence: u64,
    lease: Option<String>,
    finalized: bool,
}

struct Save {
    ticket: String,
    document: String,
    revision: String,
    path: String,
    project: PreparedAccountSave,
}

impl AccountView {
    pub(crate) fn new(port: AccountPort) -> Self {
        Self {
            port,
            opened: false,
            action: Rc::default(),
            last_state: Value::Null,
            pending: None,
            dialog: None,
            importing_media: false,
            sequence: 0,
            lease: None,
            finalized: false,
        }
    }

    pub(crate) fn project_service(&self) -> AccountProjectFn {
        let action = self.action.clone();
        Box::new(move |request| {
            let mut next = action.try_borrow_mut().map_err(|_| "FilmCraft is processing a project request.")?;
            if next.is_some() {
                return Err("FilmCraft is processing a project request.".into());
            }
            *next = Some(request);
            Ok(())
        })
    }

    pub(crate) fn suspended(&self) -> bool {
        !self.opened || self.lease.is_some() || self.finalized
    }

    pub(crate) fn begin_media_import(&mut self) -> Result<AccountPort, String> {
        if self.suspended() || self.importing_media {
            return Err("Finish the current FilmCraft operation before importing another original.".into());
        }
        self.importing_media = true;
        Ok(self.port.clone())
    }

    pub(crate) fn finish_media_import(&mut self) {
        self.importing_media = false;
    }

    fn busy(&self) -> bool {
        self.pending.is_some() || self.dialog.is_some() || self.importing_media
    }

    fn unfinished(app: &FilmcraftApp) -> bool {
        app.ui.gfx_edit.is_some() || app.session.voiceover.recording()
    }

    fn state(&self, app: &FilmcraftApp) -> Value {
        let project = &app.session;
        let unfinished = Self::unfinished(app);
        json!({"available":self.opened,"documentId":project.document_epoch.to_string(),"revision":project.revision.to_string(),
            "recoveryRevision":project.revision.to_string(),"dirty":project.is_dirty(),"capturable":!unfinished,
            "unfinishedEdit":unfinished,"autosaveEligible":false,"operationPending":self.busy()})
    }

    fn targets(app: &FilmcraftApp, packet: &JsValue) -> bool {
        number(packet, "documentId") == Some(app.session.document_epoch) && number(packet, "revision") == Some(app.session.revision)
    }

    pub(crate) fn poll(&mut self, app: &mut FilmcraftApp, ctx: &egui::Context) {
        if !self.opened {
            return;
        }
        for (kind, packet) in self.port.take_messages() {
            if let Err(error) = self.receive(app, ctx, &kind, &packet) {
                app.status(error);
            }
        }
        let action = self.action.borrow_mut().take();
        if let Some(action) = action
            && let Err(error) = self.project_action(app, action)
        {
            app.status(error);
        }
    }

    pub(crate) fn publish_state(&mut self, app: &FilmcraftApp) -> Result<(), String> {
        let state = self.state(app);
        if state != self.last_state {
            self.port.publish("project-state", state.clone(), None)?;
            self.last_state = state;
        }
        Ok(())
    }

    fn receive(&mut self, app: &mut FilmcraftApp, ctx: &egui::Context, kind: &str, packet: &JsValue) -> Result<(), String> {
        if kind == "save-result" {
            return self.settle_save(app, packet);
        }
        let Some(id) = bounded(packet, "requestId", 36) else { return Ok(()) };
        match kind {
            "recovery-capture" => {
                let mut reply = json!({"requestId":id,"documentId":bounded(packet,"documentId",20),"revision":bounded(packet,"revision",20)});
                if Self::targets(app, packet) && !Self::unfinished(app) && !self.suspended() {
                    match app.session.capture_account_draft() {
                        Ok(bytes) => return self.port.publish("recovery-captured", reply, Some(bytes)),
                        Err(error) => reply["message"] = json!(error.to_string()),
                    }
                } else {
                    reply["message"] = json!("The FilmCraft project changed before recovery capture.");
                }
                self.port.publish("recovery-captured", reply, None)
            }
            "session-save" => self.capture_session_save(app, packet, id),
            "session" => self.session(app, ctx, packet, id),
            "dialog-result" => self.dialog_result(app, packet, id),
            "operation" => self.operation(app, ctx, packet, id),
            _ => Ok(()),
        }
    }

    fn next_id(&mut self) -> Result<String, String> {
        self.sequence = self.sequence.checked_add(1).ok_or("FilmCraft exhausted its request identifiers.")?;
        Ok(self.sequence.to_string())
    }

    fn request_dialog(&mut self, mut packet: Value, revision: Option<u64>) -> Result<(), String> {
        let id = self.next_id()?;
        packet["requestId"] = json!(id);
        self.port.publish("dialog", packet, None)?;
        self.dialog = Some((id, revision));
        Ok(())
    }

    fn project_action(&mut self, app: &mut FilmcraftApp, action: AccountProjectRequest) -> Result<(), String> {
        if self.suspended() || self.busy() {
            return Err("Finish the pending FilmCraft project operation first.".into());
        }
        let session = &mut app.session;
        match action {
            AccountProjectRequest::Close => self.port.publish("close-intent", json!({}), None),
            AccountProjectRequest::New(settings) => self.request_dialog(json!({"operation":"new","settings":settings.to_string()}), None),
            AccountProjectRequest::Open(path) => self.request_dialog(json!({"operation":"open","path":path}), None),
            AccountProjectRequest::Revert => Err("Close and reopen the account project to return to its saved revision.".into()),
            AccountProjectRequest::SaveCopy => {
                let suggested = format!("/exports/{} copy.fcproj", session.project.name.replace(['/', '\\'], "_"));
                session.execute("file.saveCopy", json!({"path":suggested})).map(|_| ()).map_err(|error| error.to_string())
            }
            AccountProjectRequest::Save | AccountProjectRequest::SaveAs => {
                if Self::unfinished(app) {
                    return Err("Finish the current FilmCraft edit or recording before saving.".into());
                }
                if matches!(action, AccountProjectRequest::Save)
                    && let Some(path) = app.session.path.clone()
                {
                    return self.send_save(app, path, "save", json!({}));
                }
                let suggested = app.session.path.clone().unwrap_or_else(|| format!("{}.fcproj", app.session.project.name));
                self.request_dialog(json!({"operation":"save","suggested":suggested}), Some(app.session.revision))
            }
        }
    }

    fn dialog_result(&mut self, app: &FilmcraftApp, packet: &JsValue, id: String) -> Result<(), String> {
        if !self.dialog.as_ref().is_some_and(|(expected, _)| expected == &id) {
            return Ok(());
        }
        let Some((_, revision)) = self.dialog.take() else { return Ok(()) };
        match bounded(packet, "outcome", 16).as_deref() {
            Some("failed") => Err(bounded(packet, "message", 250).unwrap_or_else(|| "The FilmCraft dialog failed.".into())),
            Some("destination") if revision.is_some() => {
                if revision != Some(app.session.revision) || Self::unfinished(app) || self.suspended() {
                    return Err("The project changed while choosing its save destination.".into());
                }
                let path = bounded(packet, "path", 2048).ok_or("Invalid FilmCraft save destination.")?;
                self.send_save(app, path, "save", json!({}))
            }
            _ => Ok(()),
        }
    }

    fn capture_session_save(&mut self, app: &FilmcraftApp, packet: &JsValue, id: String) -> Result<(), String> {
        let path = bounded(packet, "path", 2048).ok_or("Invalid FilmCraft save destination.")?;
        let mut reply = json!({"requestId":id,"path":path,"documentId":bounded(packet,"documentId",20),
            "reviewedRevision":bounded(packet,"revision",20)});
        if self.busy() || self.suspended() || Self::unfinished(app) || !Self::targets(app, packet) {
            reply["outcome"] = json!("busy");
            return self.port.publish("session-save-result", reply, None);
        }
        if let Err(error) = self.send_save(app, path, "session-save-result", reply.clone()) {
            reply["outcome"] = json!("failed");
            reply["message"] = json!(error);
            self.port.publish("session-save-result", reply, None)?;
        }
        Ok(())
    }

    fn send_save(&mut self, app: &FilmcraftApp, path: String, kind: &str, mut reply: Value) -> Result<(), String> {
        let (project, bytes) = app.session.prepare_account_save(&path).map_err(|error| error.to_string())?;
        let ticket = self.next_id()?;
        let document = app.session.document_epoch.to_string();
        let revision = app.session.revision.to_string();
        reply["outcome"] = json!("captured");
        reply["ticket"] = json!(ticket);
        reply["documentId"] = json!(document);
        reply["revision"] = json!(revision);
        reply["path"] = json!(path);
        self.port.publish(kind, reply, Some(bytes))?;
        self.pending = Some(Save { ticket, document, revision, path, project });
        Ok(())
    }

    fn settle_save(&mut self, app: &mut FilmcraftApp, packet: &JsValue) -> Result<(), String> {
        let matching = self.pending.as_ref().is_some_and(|save| {
            bounded(packet, "ticket", 20).as_deref() == Some(save.ticket.as_str())
                && bounded(packet, "documentId", 20).as_deref() == Some(save.document.as_str())
                && bounded(packet, "revision", 20).as_deref() == Some(save.revision.as_str())
                && bounded(packet, "path", 2048).as_deref() == Some(save.path.as_str())
        });
        let outcome = bounded(packet, "outcome", 16);
        if !matching || !matches!(outcome.as_deref(), Some("written" | "failed" | "unknown")) {
            return Ok(());
        }
        let Some(save) = self.pending.take() else { return Ok(()) };
        let adopted = outcome.as_deref() == Some("written") && app.session.adopt_account_save(save.project).is_ok();
        self.port.publish(
            "save-settled",
            json!({"ticket":save.ticket,"documentId":save.document,"revision":save.revision,
            "path":save.path,"adopted":adopted,"projectState":self.state(app)}),
            None,
        )
    }

    fn session(&mut self, app: &mut FilmcraftApp, ctx: &egui::Context, packet: &JsValue, id: String) -> Result<(), String> {
        let operation = bounded(packet, "operation", 16).ok_or("Invalid FilmCraft session operation.")?;
        let lease = bounded(packet, "lease", 64).filter(|value| !value.is_empty()).ok_or("Invalid FilmCraft session lease.")?;
        let accepted = match operation.as_str() {
            "pause"
                if !self.suspended()
                    && !self.busy()
                    && !Self::unfinished(app)
                    && Self::targets(app, packet)
                    && field(packet, "dirty").as_bool() == Some(app.session.is_dirty())
                    && field(packet, "unfinishedEdit").as_bool() == Some(false) =>
            {
                if app.transport_running() {
                    filmcraft_ui_egui::menus::invoke_command(app, ctx, "playback.stop", json!({}))?;
                }
                self.lease = Some(lease.clone());
                true
            }
            "release" | "finalize" if self.lease.as_deref() == Some(lease.as_str()) => {
                self.lease = None;
                self.finalized = operation == "finalize";
                true
            }
            _ => false,
        };
        self.port.publish(
            "session-result",
            json!({"requestId":id,"operation":operation,"lease":lease,"ok":accepted,
            "message":if accepted {""} else {"The FilmCraft project changed after close review."}}),
            None,
        )
    }

    fn operation(&self, app: &mut FilmcraftApp, ctx: &egui::Context, packet: &JsValue, id: String) -> Result<(), String> {
        let document = bounded(packet, "documentId", 20).ok_or("Invalid FilmCraft project target.")?;
        let mut reply = json!({"requestId":id,"documentId":document});
        let path = field(packet, "path");
        let path = if path.is_null() { None } else { Some(path.as_string().ok_or("Invalid FilmCraft project binding.")?) };
        let result = if self.suspended() || document != app.session.document_epoch.to_string() || path != app.session.path {
            Err("The FilmCraft operation does not match the current project.".into())
        } else {
            let command = bounded(packet, "command", 64).ok_or("Invalid FilmCraft operation.")?;
            let args = bounded(packet, "args", 4096).ok_or("Invalid FilmCraft operation arguments.")?;
            let args: Value = serde_json::from_str(&args).map_err(|_| "Invalid FilmCraft operation arguments.")?;
            let fields = args.as_object().ok_or("FilmCraft operation arguments must be an object.")?;
            match command.as_str() {
                "document.inspect" if fields.is_empty() => Ok(json!({"name":app.session.project.name,
                    "sequenceCount":app.session.project.sequences().count(),"mediaCount":project_media(&app.session).len(),"dirty":app.session.is_dirty()})),
                _ if self.busy() || Self::unfinished(app) => Err("Finish the pending FilmCraft edit first.".into()),
                "file.newSequence"
                    if fields.len() == 1 && fields.get("name").and_then(Value::as_str).is_some_and(|name| !name.trim().is_empty() && name.len() <= 128) =>
                {
                    filmcraft_ui_egui::menus::invoke_command(app, ctx, &command, args)
                }
                "edit.undo" if fields.is_empty() => filmcraft_ui_egui::menus::invoke_command(app, ctx, &command, args),
                _ => Err("Unsupported FilmCraft project operation.".into()),
            }
        };
        match result {
            Ok(value) => reply["result"] = json!(value.to_string()),
            Err(error) => reply["message"] = json!(error),
        }
        self.port.publish("operation-result", reply, None)
    }
}

fn bounded(packet: &JsValue, field: &str, maximum: usize) -> Option<String> {
    string(packet, field).filter(|text| text.len() <= maximum)
}

fn number(packet: &JsValue, field: &str) -> Option<u64> {
    bounded(packet, field, 20)?.parse().ok()
}

pub(crate) async fn start(port: &AccountPort) -> Result<(), String> {
    let request = port.start().await?;
    let request_id = request.request_id;
    let opened = on_ui(move |view, _| {
        let session = &mut view.app.session;
        match request.bytes {
            Some(bytes) => session.open_account_project(request.path.as_deref(), &bytes, request.recover),
            None if request.settings["demo"] == true => session.execute("file.openDemoProject", json!({})).map(|_| ()),
            None => session.execute("file.newProject", request.settings).map(|_| ()),
        }
        .map_err(|error| error.to_string())?;
        Ok::<_, String>(project_media(session))
    })
    .await;
    let locations = match opened {
        Ok(locations) => locations,
        Err(message) => {
            port.open_failed(&request_id, &message);
            return Err(message);
        }
    };

    let mut available = Vec::new();
    let mut missing = 0;
    for location in locations {
        match restore_original(port, &location).await {
            Ok(()) => available.push(location),
            Err(_) => missing += 1,
        }
    }
    let document = on_ui(move |view, ctx| {
        let session = &view.app.session;
        for path in available {
            session.media.allow(&path);
        }
        let state = json!({
            "documentId":session.document_epoch.to_string(),
            "revision":session.revision.to_string(),
            "path":session.path,
        });
        if let Some(account) = &mut view.account {
            account.opened = true;
        }
        if missing > 0 {
            view.app.status(format!("{missing} media items are unavailable. Use Link Media to select originals."));
        }
        ctx.request_repaint();
        state
    })
    .await;
    let mut reply = document;
    reply["requestId"] = json!(request_id);
    reply["ok"] = json!(true);
    port.publish("started", reply, None)
}

fn project_media(session: &Session) -> BTreeSet<String> {
    session
        .project
        .items
        .values()
        .filter_map(|item| item.as_media())
        .flat_map(|media| [Some(&media.media), media.proxy.as_ref()])
        .filter_map(|reference| match reference {
            Some(MediaRef::File { path }) => Some(path.clone()),
            _ => None,
        })
        .collect()
}

async fn restore_original(port: &AccountPort, path: &str) -> Result<(), String> {
    let (digest, _) = hostmode::account_media_parts(path).ok_or("This media link needs an account original.")?;
    let blob = port.read_original(digest).await?;
    let size = blob.size() as u64;
    fs::register_account_blob(path, blob)?;
    prewarm(path, size).await;
    Ok(())
}

pub(crate) async fn store_original(port: &AccountPort, file: &web_sys::File) -> Result<String, String> {
    let name = file.name();
    let digest = port.store_original(file).await?;
    let path = hostmode::account_media_location(&digest, &name).ok_or("Invalid FilmCraft original filename.")?;
    fs::register_account_blob(&path, file.clone().into())?;
    Ok(path)
}
