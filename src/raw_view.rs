//! View coordinates use original, cropped image pixels, independent of preview
//! resolution. A full-resolution texture may arrive without changing the view.
use eframe::egui::{self, Pos2, Rect, Vec2};

pub struct View {
    /// None follows the viewport; Some(1) maps one source pixel to one screen pixel.
    scale: Option<f32>,
    pan: Vec2,
    fit: f32,
}
impl Default for View {
    fn default() -> Self {
        Self {
            scale: None,
            pan: Vec2::ZERO,
            fit: 1.,
        }
    }
}
impl View {
    pub fn fit(&mut self) {
        self.scale = None;
        self.pan = Vec2::ZERO;
    }
    pub fn is_fit(&self) -> bool {
        self.scale.is_none()
    }
    pub fn scale(&self) -> f32 {
        self.scale.unwrap_or(self.fit.max(0.0001))
    }
    pub fn set_scale(&mut self, scale: f32) {
        let next = scale.clamp(self.fit.clamp(0.0001, 0.05), 8.);
        self.pan *= next / self.scale();
        self.scale = Some(next);
    }
    pub fn needs_full(&self) -> bool {
        !self.is_fit() && self.scale() > self.fit * 1.01
    }
    fn layout(&mut self, area: Rect, pixels: Vec2, ppp: f32) -> Rect {
        let native = pixels / ppp;
        self.fit = (area.width() / native.x)
            .min(area.height() / native.y)
            .min(1.);
        let size = native * self.scale();
        let limit = ((size - area.size()) * 0.5).max(Vec2::ZERO);
        self.pan = self.pan.clamp(-limit, limit);
        Rect::from_center_size(area.center() + self.pan, size)
    }
    fn zoom_at(&mut self, factor: f32, anchor: Pos2, area: Rect) {
        let old = self.scale();
        let old_pan = self.pan;
        let offset = anchor - area.center();
        self.set_scale(old * factor);
        self.pan = offset - (offset - old_pan) * self.scale() / old;
    }
    pub fn interact(
        &mut self,
        ui: &egui::Ui,
        area: Rect,
        response: &egui::Response,
        pixels: Vec2,
        picking: bool,
    ) -> Rect {
        self.layout(area, pixels, ui.ctx().pixels_per_point());
        if !picking {
            if response.dragged() {
                self.pan += response.drag_delta();
            }
            if response.double_clicked() {
                if self.is_fit() {
                    let anchor = response.interact_pointer_pos().unwrap_or(area.center());
                    self.zoom_at(1. / self.scale(), anchor, area);
                } else {
                    self.fit();
                }
            }
        }
        if response.hovered() {
            let (pinch, wheel, anchor) = ui.input(|i| {
                (
                    i.zoom_delta(),
                    i.raw_scroll_delta.y,
                    i.pointer.hover_pos().unwrap_or(area.center()),
                )
            });
            // egui converts Ctrl-wheel to zoom_delta. Do not apply it twice.
            let factor = if pinch != 1. {
                pinch
            } else {
                (wheel * 0.0025).exp()
            };
            if factor.is_finite() && factor != 1. {
                self.zoom_at(factor, anchor, area);
            }
            ui.ctx().set_cursor_icon(if picking {
                egui::CursorIcon::Crosshair
            } else if response.dragged() {
                egui::CursorIcon::Grabbing
            } else if self.is_fit() {
                egui::CursorIcon::ZoomIn
            } else {
                egui::CursorIcon::Grab
            });
        }
        self.layout(area, pixels, ui.ctx().pixels_per_point())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointer_anchor_survives_zoom_and_preview_replacement() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 600.));
        let pixels = Vec2::new(6000., 4000.);
        let mut view = View::default();
        let before = view.layout(area, pixels, 1.5);
        let anchor = egui::pos2(600., 380.);
        let uv = (anchor - before.min) / before.size();
        view.zoom_at(2., anchor, area);
        let after = view.layout(area, pixels, 1.5);
        assert!(((anchor - after.min) / after.size() - uv).length() < 0.00001);
        assert_eq!(after, view.layout(area, pixels, 1.5));
        assert!(view.needs_full());
    }
    #[test]
    fn hundred_percent_is_physical_pixels_and_pan_stays_on_image() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 600.));
        let pixels = Vec2::new(6000., 4000.);
        let mut view = View::default();
        view.layout(area, pixels, 2.);
        view.set_scale(1.);
        view.pan = Vec2::splat(100_000.);
        let rect = view.layout(area, pixels, 2.);
        assert_eq!(rect.size() * 2., pixels);
        assert!(rect.contains_rect(area));
        view.fit();
        assert_eq!(view.layout(area, pixels, 2.).center(), area.center());
        assert!(!view.needs_full());
    }
    #[test]
    fn real_input_supports_pan_pinch_and_blocks_pan_while_picking() {
        let ctx = egui::Context::default();
        let mut view = View::default();
        let mut time = 0.;
        let mut run = |events, picking| {
            time += 0.05;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 600.))),
                    events,
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let (area, response) = ui.allocate_exact_size(
                            ui.available_size(),
                            egui::Sense::click_and_drag(),
                        );
                        view.interact(ui, area, &response, Vec2::new(6000., 4000.), picking);
                    });
                },
            );
            (view.scale(), view.pan)
        };
        let start = egui::pos2(400., 300.);
        let button = |pressed| egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        run(vec![egui::Event::PointerMoved(start)], false);
        let (fit, _) = run(vec![egui::Event::Zoom(3.)], false);
        run(vec![button(true)], false);
        let (_, pan) = run(
            vec![egui::Event::PointerMoved(start + Vec2::new(60., 30.))],
            false,
        );
        assert!(pan.length() > 30.);
        let (_, frozen) = run(
            vec![egui::Event::PointerMoved(start + Vec2::new(90., 60.))],
            true,
        );
        assert_eq!(pan, frozen);
        let (scale, _) = run(vec![egui::Event::Zoom(2.)], false);
        assert!((scale - fit * 2.).abs() < 0.001);
    }
}
