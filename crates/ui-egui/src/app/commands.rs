use super::{FilmcraftApp, menus};
#[cfg(test)]
use filmcraft_engine::Session;
#[cfg(test)]
use serde_json::json;

impl FilmcraftApp {
    // ---------------------------------------------------------------- input

    /// Sends the menu tree to the native menu bar when it changed, so its items show the same
    /// labels, shortcuts and enablement as the in-window menus (Undo with nothing to undo is
    /// disabled). The items a focused text field takes stay enabled while one has the keyboard.
    /// Without a native menu bar this does nothing.
    pub(super) fn sync_native_menu(&mut self, ctx: &egui::Context) {
        if self.hooks.menu_changed.is_none() {
            return;
        }
        let mut items = menus::menu_items(self);
        if menus::text_field_window(ctx).is_some() {
            for it in items.iter_mut().filter(|it| menus::edits_text(&it.id)) {
                it.enabled = true;
            }
        } else if self.ui.gfx_edit.is_some() {
            // a graphic's text: its Copy, Cut, Paste and Select All, whatever the project allows (#1953)
            for it in items.iter_mut().filter(|it| menus::edits_graphic_text(&it.id)) {
                it.enabled = true;
            }
        }
        if items == self.menu_sent {
            return;
        }
        if let Some(hook) = self.hooks.menu_changed.as_mut() {
            hook(&items);
        }
        self.menu_sent = items;
    }
}

impl FilmcraftApp {
    /// Drain native menu/control command ids, preserving the control-only Quit exception.
    pub(super) fn logic_command_inbox(&mut self, ctx: &egui::Context) {
        if let Some(rx) = self.command_inbox.take() {
            while let Ok(id) = rx.try_recv() {
                // control-only mode ignores native menu commands, but Quit stays available (#1943)
                if self.control_input_only && id != "app.quit" {
                    continue;
                }
                if let Err(e) = menus::activate_native(self, ctx, &id) {
                    self.ui.status = e;
                }
            }
            self.command_inbox = Some(rx);
        }
    }
}

#[cfg(test)]
mod native_menu_tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn enabled(items: &[menus::MenuItem], id: &str) -> Option<bool> {
        items.iter().find(|it| it.id == id).map(|it| it.enabled)
    }

    #[test]
    fn hosted_project_commands_wait_for_the_account_window() {
        use crate::AccountProjectRequest as Request;
        let sent = Rc::new(RefCell::new(Vec::new()));
        let sink = sent.clone();
        let mut app = FilmcraftApp::new(Session::default());
        app.hooks.account_project = Some(Box::new(move |request| {
            sink.borrow_mut().push(request);
            Ok(())
        }));
        app.session.execute("file.newSequence", json!({"name":"Keep this edit"})).unwrap();
        let ctx = egui::Context::default();
        for command in ["file.saveAll", "file.saveAs", "file.saveCopy", "file.revert", "file.closeAllProjects"] {
            menus::invoke(&mut app, &ctx, command, json!({})).unwrap();
        }
        menus::invoke_command(&mut app, &ctx, "file.open", json!({"path":"Video/Existing.fcproj"})).unwrap();
        menus::activate_native(&mut app, &ctx, "file.newProject").unwrap();
        menus::invoke(&mut app, &ctx, "file.close", json!({})).unwrap();
        assert_eq!(
            sent.borrow().as_slice(),
            [
                Request::Save,
                Request::SaveAs,
                Request::SaveCopy,
                Request::Revert,
                Request::Close,
                Request::Open(Some("Video/Existing.fcproj".into())),
                Request::New(json!({})),
            ],
        );
        assert!(app.session.is_dirty());
        assert!(app.session.path.is_none());
        assert_eq!(app.session.project.sequences().count(), 1);
        assert!(app.session.state.active_sequence.is_none());
    }

    /// The native menu bar (macOS) gets the menu tree whenever it changes, so Edit ▸ Undo and Redo
    /// follow the history instead of staying enabled (#902).
    #[test]
    fn native_menu_follows_undo_and_redo_availability() {
        let sent: Rc<RefCell<Vec<Vec<menus::MenuItem>>>> = Rc::default();
        let mut app = FilmcraftApp::new(Session::default());
        let sink = sent.clone();
        app.hooks.menu_changed = Some(Box::new(move |items: &[menus::MenuItem]| sink.borrow_mut().push(items.to_vec())));
        let ctx = egui::Context::default();

        app.sync_native_menu(&ctx);
        let last = |sent: &Rc<RefCell<Vec<Vec<menus::MenuItem>>>>, id: &str| sent.borrow().last().and_then(|items| enabled(items, id));
        assert_eq!(last(&sent, "edit.undo"), Some(false), "nothing to undo in an empty project");
        assert_eq!(last(&sent, "edit.redo"), Some(false), "nothing to redo in an empty project");
        assert_eq!(last(&sent, "file.newProject"), Some(true));

        app.sync_native_menu(&ctx);
        assert_eq!(sent.borrow().len(), 1, "an unchanged menu is not sent again");

        app.session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        app.sync_native_menu(&ctx);
        assert_eq!(last(&sent, "edit.undo"), Some(true), "an edit can be undone");
        assert_eq!(last(&sent, "edit.redo"), Some(false));

        app.session.execute("edit.undo", json!({})).unwrap();
        app.sync_native_menu(&ctx);
        assert_eq!(last(&sent, "edit.undo"), Some(false));
        assert_eq!(last(&sent, "edit.redo"), Some(true), "the undone edit can be redone");
    }

    /// #1953: AppKit does not send a disabled item's command, so while a graphic's text is edited
    /// its Copy, Cut, Paste and Select All stay enabled with no clip selected or copied; Undo stays
    /// the project's.
    #[test]
    fn native_edit_items_stay_enabled_while_a_graphic_text_is_edited() {
        let sent: Rc<RefCell<Vec<Vec<menus::MenuItem>>>> = Rc::default();
        let mut app = FilmcraftApp::new(Session::default());
        let sink = sent.clone();
        app.hooks.menu_changed = Some(Box::new(move |items: &[menus::MenuItem]| sink.borrow_mut().push(items.to_vec())));
        let ctx = egui::Context::default();
        let last = |sent: &Rc<RefCell<Vec<Vec<menus::MenuItem>>>>, id: &str| sent.borrow().last().and_then(|items| enabled(items, id));
        app.sync_native_menu(&ctx);
        assert_eq!(last(&sent, "edit.copy"), Some(false), "nothing selected to copy");
        app.ui.gfx_edit = Some(crate::state::GfxEdit::default());
        app.sync_native_menu(&ctx);
        for id in ["edit.copy", "edit.cut", "edit.paste", "edit.selectAll"] {
            assert_eq!(last(&sent, id), Some(true), "{id} while a graphic's text is edited");
        }
        assert_eq!(last(&sent, "edit.undo"), Some(false), "Undo stays the project's");
    }
}
