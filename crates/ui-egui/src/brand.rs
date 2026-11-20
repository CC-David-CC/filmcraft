//! Text attribution for the original ArtCraft project.

/// Draw the attribution with its left edge at the vertically centered point.
pub fn paint_wordmark(
    ui: &egui::Ui,
    left_center: egui::Pos2,
    height: f32,
    dark: bool,
) -> Option<egui::Rect> {
    let ink = if dark { egui::Color32::WHITE } else { egui::Color32::BLACK };
    Some(ui.painter().text(
        left_center,
        egui::Align2::LEFT_CENTER,
        "ArtCraft",
        egui::FontId::proportional(height),
        ink,
    ))
}
