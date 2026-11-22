//! Narrow editing windows keep the media or preview panel above a full-width timeline.
//! These rectangles belong to the view. Resizing never rewrites the saved desktop dock.
use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::state::Mode;
use egui::{Align, Layout, Rect, Ui, UiBuilder, pos2, vec2};

pub(crate) fn narrow(width: f32) -> bool {
    width < 1000.0
}

fn bar(ui: &mut Ui, rect: Rect, salt: &str) -> Ui {
    let mut child = ui.new_child(UiBuilder::new().id_salt(salt).max_rect(rect).layout(Layout::left_to_right(Align::Center)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.spacing_mut().interact_size.y = 32.0;
    child
}

fn command(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, label: &str) {
    let response = ui.button(label);
    app.auto.add_response(id, &response, label);
    if response.clicked()
        && let Err(error) = crate::menus::invoke(app, ui.ctx(), id, serde_json::json!({}))
    {
        app.ui.status = error;
    }
}

pub(crate) fn header(app: &mut FilmcraftApp, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, app.tokens.header_bg);
    let mut row = bar(ui, rect.shrink2(vec2(5.0, 1.0)), "compact-header");
    row.menu_button(tl!("Menu"), |ui| {
        for (mode, label) in [(Mode::Import, tl!("Import")), (Mode::Edit, tl!("Edit")), (Mode::Export, tl!("Export"))] {
            if ui.selectable_label(mode == app.ui.mode, label).clicked() {
                app.ui.mode = mode;
                ui.close();
            }
        }
        ui.separator();
        crate::menus::compact_menu(app, ui);
    });
    command(app, &mut row, "file.save", tl!("Save"));
    if app.ui.mode == Mode::Edit {
        let label = if app.playback.playing { tl!("Stop") } else { tl!("Play") };
        command(app, &mut row, "playback.toggle", label);
        command(app, &mut row, "view.zoomToSequence", tl!("Fit"));
    }
}

pub(crate) fn edit(app: &mut FilmcraftApp, ui: &mut Ui, body: Rect) {
    let tab = egui::Id::unique("compact-edit-panel");
    let mut panel = ui.ctx().data(|d| d.get_temp::<PanelKind>(tab)).unwrap_or(PanelKind::Project);
    if app.ui.focused != PanelKind::Timeline {
        panel = app.ui.focused;
    }
    let bar_height = 39.0_f32.min(body.height());
    let tabs = Rect::from_min_size(body.min, vec2(body.width(), bar_height));
    let mut row = bar(ui, tabs.shrink2(vec2(4.0, 1.0)), "compact-panel-choices");
    for (kind, label) in [(PanelKind::Project, tl!("Project")), (PanelKind::Program, tl!("Preview"))] {
        if row.selectable_label(panel == kind, label).clicked() {
            panel = kind;
            app.ui.focused = kind;
        }
    }
    row.menu_button(tl!("Panels"), |ui| {
        egui::ScrollArea::vertical().max_height((body.height() * 0.65).max(72.0)).show(ui, |ui| {
            for candidate in PanelKind::ALL.into_iter().filter(|p| *p != PanelKind::Timeline) {
                if ui.selectable_label(panel == candidate, crate::i18n::t(candidate.title())).clicked() {
                    panel = candidate;
                    app.ui.focused = candidate;
                    ui.close();
                }
            }
        });
    });
    ui.ctx().data_mut(|d| d.insert_temp(tab, panel));
    let top = tabs.max.y;
    let split = top + (body.max.y - top) * 0.42;
    let upper = Rect::from_min_max(pos2(body.min.x, top), pos2(body.max.x, split));
    let timeline = Rect::from_min_max(pos2(body.min.x, split + 1.0), body.max);
    let mut bodies = Vec::new();
    for (panel, rect) in [(panel, upper), (PanelKind::Timeline, timeline)] {
        if !rect.is_positive() {
            continue;
        }
        if ui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| rect.contains(p))) {
            app.ui.focused = panel;
        }
        let mut child = ui.new_child(UiBuilder::new().id_salt(("compact-panel", panel)).max_rect(rect));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        app.auto.add(&format!("panel.{}", panel.id()), rect, panel.title());
        crate::panels::show(app, &mut child, panel, rect);
        bodies.push((panel, rect, ui.layer_id()));
    }
    ui.ctx().data_mut(|data| data.insert_temp(crate::dock_ui::bodies_id(), bodies));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, Pos2, RawInput};

    #[test]
    fn phone_timeline_room_for_media_trim_without_changing_desktop_dock() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(336.0, 715.0));
        let ctx = Context::default();
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        crate::theme::install(&ctx, &app.tokens);
        app.session.execute("file.newSequence", serde_json::json!({"name":"Cut"})).expect("new sequence");
        let dock = serde_json::to_value(&app.ui.dock).expect("original dock");
        ctx.run_ui(RawInput { screen_rect: Some(rect), ..Default::default() }, |ui| {
            let top = Rect::from_min_size(rect.min, vec2(rect.width(), crate::header::HEIGHT));
            header(&mut app, ui, top);
            edit(&mut app, ui, Rect::from_min_max(top.left_bottom(), rect.max));
        })
        .textures_delta
        .clear();
        let timeline = app.tl.layout.as_ref().expect("timeline");
        assert!(timeline.ruler.width() > 0.70 * rect.width());
        let [x, y, w, h] = app.auto.find("timeline.timecode").expect("timecode").rect;
        let tc = Rect::from_min_size(pos2(x, y), vec2(w, h));
        assert!(!tc.intersects(timeline.ruler));
        assert!(app.last_timeline_width > 0.70 * rect.width());
        assert_eq!(serde_json::to_value(&app.ui.dock).expect("restored dock"), dock);
    }
}
