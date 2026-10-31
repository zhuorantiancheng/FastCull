//! Non-destructive crop draft: only Apply writes photo metadata.
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use fastcull::crop::{self, Crop, Geometry};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub width: f32,
    pub height: f32,
}
impl Preset {
    fn valid(&self) -> bool {
        !self.name.trim().is_empty()
            && !self.name.contains(['\n', '\r', '\t'])
            && [self.width, self.height]
                .iter()
                .all(|v| v.is_finite() && (0.01..=10000.).contains(v))
    }
}
pub fn settings_path() -> PathBuf {
    std::env::var_os("FASTCULL_SETTINGS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(fastcull::platform::data_directory)
        .join("crop-ratios.tsv")
}
pub fn load_presets(path: &Path) -> anyhow::Result<Vec<Preset>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    text.lines()
        .map(|line| {
            let f: Vec<_> = line.split('\t').collect();
            anyhow::ensure!(f.len() == 3, "比例预设文件格式错误");
            let p = Preset {
                name: f[0].into(),
                width: f[1].parse()?,
                height: f[2].parse()?,
            };
            anyhow::ensure!(p.valid(), "比例预设无效");
            Ok(p)
        })
        .collect()
}
pub fn save_presets(path: &Path, presets: &[Preset]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("预设目录不存在"))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    for p in presets {
        anyhow::ensure!(p.valid(), "名称不能为空或含换行；比例须在 0.01–10000 之间");
        writeln!(file, "{}\t{}\t{}", p.name.trim(), p.width, p.height)?;
    }
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[derive(Clone, Copy)]
struct Snapshot {
    geometry: Geometry,
    orientation: u16,
    ratio: Option<f32>,
}
#[derive(Clone, Copy)]
struct Drag {
    start: Pos2,
    rect: Rect,
    handle: usize,
}
pub enum Action {
    None,
    Apply,
    Cancel,
}
pub struct Editor {
    pub geometry: Geometry,
    pub orientation: u16,
    pub size: Vec2,
    pub ratio: Option<f32>,
    pub preset_name: String,
    width: f32,
    height: f32,
    name: String,
    presets: Vec<Preset>,
    presets_error: Option<String>,
    error: Option<String>,
    drag: Option<Drag>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}
impl Editor {
    pub fn new(value: Crop, size: Vec2, orientation: u16) -> Self {
        let loaded = load_presets(&settings_path());
        let presets_error = loaded
            .as_ref()
            .err()
            .map(|e| format!("无法读取比例预设：{e:#}"));
        Self {
            geometry: value.geometry(size, orientation),
            orientation,
            size,
            ratio: None,
            preset_name: "自由".into(),
            width: 3.,
            height: 2.,
            name: String::new(),
            presets: loaded.unwrap_or_default(),
            presets_error,
            error: None,
            drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
    pub fn display(&self) -> Vec2 {
        crop::oriented_size(self.size, self.orientation)
    }
    pub fn value(&self) -> Crop {
        Crop::from_geometry(self.geometry, self.size, self.orientation)
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            geometry: self.geometry,
            orientation: self.orientation,
            ratio: self.ratio,
        }
    }
    fn restore(&mut self, s: Snapshot) {
        self.geometry = s.geometry;
        self.orientation = s.orientation;
        self.ratio = s.ratio;
        self.preset_name = if let Some(r) = s.ratio {
            self.width = r;
            self.height = 1.;
            ratio_name(r)
        } else {
            "自由".into()
        };
    }
    pub fn remember(&mut self) {
        self.undo.push(self.snapshot());
        self.redo.clear();
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
    }
    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(s);
        }
    }
    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(s);
        }
    }
    pub fn rotate(&mut self, orientation: u16) {
        self.remember();
        let value = self.value();
        self.orientation = orientation;
        self.geometry = value.geometry(self.size, orientation);
        if let Some(r) = self.ratio {
            self.ratio = Some(1. / r);
            self.preset_name = ratio_name(1. / r);
        }
        std::mem::swap(&mut self.width, &mut self.height);
    }
    pub fn select_ratio(&mut self, name: String, ratio: Option<f32>) {
        self.remember();
        self.preset_name = name;
        self.ratio = ratio;
        if let Some(r) = ratio {
            self.width = r;
            self.height = 1.;
            self.geometry.set_ratio(r, self.display());
        }
    }
    pub fn controls(&mut self, ui: &mut egui::Ui) -> Action {
        ui.heading("裁切");
        ui.add_space(8.);
        ui.label("预设比例");
        let mut choice = None;
        egui::ComboBox::from_id_salt("crop-presets")
            .width(ui.available_width() - 12.)
            .selected_text(&self.preset_name)
            .show_ui(ui, |ui| {
                if ui.selectable_label(self.ratio.is_none(), "自由").clicked() {
                    choice = Some(("自由".into(), None));
                }
                if ui.selectable_label(false, "原始比例").clicked() {
                    choice = Some(("原始比例".into(), Some(self.display().x / self.display().y)));
                }
                for (w, h) in [
                    (1, 1),
                    (3, 2),
                    (2, 3),
                    (4, 3),
                    (3, 4),
                    (5, 4),
                    (4, 5),
                    (7, 5),
                    (5, 7),
                    (16, 9),
                    (9, 16),
                ] {
                    let name = ratio_name(w as f32 / h as f32);
                    if ui
                        .selectable_label(self.preset_name == name, &name)
                        .clicked()
                    {
                        choice = Some((name, Some(w as f32 / h as f32)));
                    }
                }
                if !self.presets.is_empty() {
                    ui.separator();
                }
                for p in &self.presets {
                    if ui
                        .selectable_label(
                            false,
                            format!("{}  ({} : {})", p.name, p.width, p.height),
                        )
                        .clicked()
                    {
                        choice = Some((p.name.clone(), Some(p.width / p.height)));
                    }
                }
            });
        if let Some((name, ratio)) = choice {
            self.select_ratio(name, ratio);
        }
        ui.horizontal(|ui| {
            let locked = self.ratio.is_some();
            if ratio_lock_button(ui, locked).clicked() {
                self.remember();
                self.ratio =
                    (!locked).then(|| self.geometry.rect.width() / self.geometry.rect.height());
                self.preset_name = self.ratio.map_or_else(|| "自由".into(), ratio_name);
            }
            if ui
                .add_enabled(self.ratio.is_some(), egui::Button::new("横竖互换"))
                .clicked()
            {
                let ratio = self.ratio.map(|r| 1. / r);
                self.select_ratio(ratio.map_or_else(|| "自由".into(), ratio_name), ratio);
            }
        });
        ui.add_space(8.);
        ui.label("自定义比例（宽 : 高）");
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.width)
                    .range(0.01..=10000.)
                    .speed(0.1)
                    .max_decimals(2),
            );
            ui.label(":");
            ui.add(
                egui::DragValue::new(&mut self.height)
                    .range(0.01..=10000.)
                    .speed(0.1)
                    .max_decimals(2),
            );
            if ui.button("使用").clicked() {
                self.select_ratio(
                    format!("{} : {}", self.width, self.height),
                    Some(self.width / self.height),
                );
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.name)
                .hint_text("预设名称")
                .desired_width(ui.available_width()),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.presets_error.is_none(),
                    egui::Button::new("保存比例预设"),
                )
                .clicked()
            {
                let p = Preset {
                    name: self.name.trim().into(),
                    width: self.width,
                    height: self.height,
                };
                let mut next = self.presets.clone();
                if let Some(old) = next.iter_mut().find(|v| v.name == p.name) {
                    *old = p;
                } else {
                    next.push(p);
                }
                match save_presets(&settings_path(), &next) {
                    Ok(()) => {
                        self.presets = next;
                        self.error = None;
                    }
                    Err(e) => self.error = Some(format!("保存预设失败：{e:#}")),
                }
            }
            if ui
                .add_enabled(
                    self.presets.iter().any(|p| p.name == self.preset_name),
                    egui::Button::new("移除预设"),
                )
                .clicked()
            {
                let next: Vec<_> = self
                    .presets
                    .iter()
                    .filter(|p| p.name != self.preset_name)
                    .cloned()
                    .collect();
                match save_presets(&settings_path(), &next) {
                    Ok(()) => {
                        self.presets = next;
                        self.preset_name = "自定比例".into();
                    }
                    Err(e) => self.error = Some(format!("保存预设失败：{e:#}")),
                }
            }
        });
        ui.separator();
        ui.label("角度");
        let before = self.snapshot();
        let angle = ui.add(
            egui::Slider::new(&mut self.geometry.angle, -45.0..=45.0)
                .suffix("°")
                .fixed_decimals(2)
                .step_by(0.01),
        );
        if angle.drag_started() || (angle.changed() && !angle.dragged()) {
            self.undo.push(before);
            self.redo.clear();
        }
        if angle.changed() {
            self.geometry.constrain(self.display());
        }
        ui.horizontal(|ui| {
            if ui.button("向左 90°").clicked() {
                self.rotate(counterclockwise(self.orientation));
            }
            if ui.button("向右 90°").clicked() {
                self.rotate(clockwise(self.orientation));
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.undo.is_empty(), egui::Button::new("撤销"))
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(!self.redo.is_empty(), egui::Button::new("重做"))
                .clicked()
            {
                self.redo();
            }
            if ui.button("重置裁切").clicked() {
                self.remember();
                self.geometry = Geometry::full(self.display());
                self.ratio = None;
                self.preset_name = "自由".into();
            }
        });
        ui.add_space(8.);
        let size = self.geometry.rect.size();
        ui.label(format!("约 {:.0} × {:.0} 像素", size.x, size.y));
        ui.weak(
            "拖动边框或八个手柄改变范围；框内拖动位置，框外拖动重画。旋转时自动约束到照片边缘。",
        );
        ui.weak("锁定比例时，拖动角点自动适应横拍 / 竖拍。解锁后可自由改变宽高。");
        ui.weak("Enter 应用 · Esc 取消 · Ctrl/Cmd+Z 撤销");
        if let Some(e) = self.error.as_ref().or(self.presets_error.as_ref()) {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        ui.separator();
        let mut action = Action::None;
        ui.horizontal(|ui| {
            if ui.button("应用裁切").clicked() {
                action = Action::Apply;
            }
            if ui.button("取消").clicked() {
                action = Action::Cancel;
            }
        });
        action
    }
    pub fn overlay(&mut self, ui: &mut egui::Ui, response: &egui::Response, image: Rect) {
        let display = self.display();
        let scale = image.width() / display.x;
        let to_screen = |p: Pos2| image.min + p.to_vec2() * scale;
        let crop_rect = Rect::from_min_max(
            to_screen(self.geometry.rect.min),
            to_screen(self.geometry.rect.max),
        );
        let handles = [
            crop_rect.left_top(),
            crop_rect.center_top(),
            crop_rect.right_top(),
            crop_rect.right_center(),
            crop_rect.right_bottom(),
            crop_rect.center_bottom(),
            crop_rect.left_bottom(),
            crop_rect.left_center(),
        ];
        let hit = |p: Pos2| {
            handles
                .iter()
                .position(|h| h.distance(p) < 10.)
                .unwrap_or(if crop_rect.contains(p) { 8 } else { 9 })
        };
        if let Some(pos) = response.hover_pos() {
            ui.ctx().set_cursor_icon(match hit(pos) {
                0 | 4 => egui::CursorIcon::ResizeNwSe,
                2 | 6 => egui::CursorIcon::ResizeNeSw,
                1 | 5 => egui::CursorIcon::ResizeVertical,
                3 | 7 => egui::CursorIcon::ResizeHorizontal,
                8 => egui::CursorIcon::Move,
                _ => egui::CursorIcon::Crosshair,
            });
        }
        if response.drag_started()
            && let Some(pos) = ui.input(|i| i.pointer.press_origin())
        {
            self.remember();
            self.drag = Some(Drag {
                start: ((pos - image.min) / scale).to_pos2(),
                rect: self.geometry.rect,
                handle: hit(pos),
            });
        }
        if response.dragged()
            && let (Some(d), Some(pos)) = (self.drag, response.interact_pointer_pos())
        {
            let p = ((pos - image.min) / scale).to_pos2();
            self.geometry.rect = drag_rect(d, p, self.ratio);
            if let Some(ratio) = self.ratio {
                let next = drag_ratio(d, p, ratio);
                if (ratio - next).abs() > 0.0001 {
                    self.ratio = Some(next);
                    std::mem::swap(&mut self.width, &mut self.height);
                    self.preset_name = ratio_name(next);
                }
            }
            self.geometry.constrain(display);
        }
        if response.drag_stopped() {
            self.drag = None;
        }
        // Redraw overlay from current draft, so the handles follow the pointer in this frame.
        let r = Rect::from_min_max(
            to_screen(self.geometry.rect.min),
            to_screen(self.geometry.rect.max),
        );
        let painter = ui.painter_at(response.rect);
        let shade = Color32::from_black_alpha(155);
        for mask in [
            Rect::from_min_max(response.rect.min, Pos2::new(response.rect.right(), r.top())),
            Rect::from_min_max(
                Pos2::new(response.rect.left(), r.bottom()),
                response.rect.max,
            ),
            Rect::from_min_max(Pos2::new(response.rect.left(), r.top()), r.left_bottom()),
            Rect::from_min_max(r.right_top(), Pos2::new(response.rect.right(), r.bottom())),
        ] {
            painter.rect_filled(mask, 0., shade);
        }
        painter.rect_stroke(
            r,
            0.,
            Stroke::new(1.5, Color32::WHITE),
            egui::StrokeKind::Inside,
        );
        for t in [1. / 3., 2. / 3.] {
            let x = egui::lerp(r.x_range(), t);
            let y = egui::lerp(r.y_range(), t);
            painter.line_segment(
                [Pos2::new(x, r.top()), Pos2::new(x, r.bottom())],
                Stroke::new(1., Color32::from_white_alpha(140)),
            );
            painter.line_segment(
                [Pos2::new(r.left(), y), Pos2::new(r.right(), y)],
                Stroke::new(1., Color32::from_white_alpha(140)),
            );
        }
        for p in [
            r.left_top(),
            r.center_top(),
            r.right_top(),
            r.right_center(),
            r.right_bottom(),
            r.center_bottom(),
            r.left_bottom(),
            r.left_center(),
        ] {
            painter.rect_filled(
                Rect::from_center_size(p, Vec2::splat(7.)),
                1.,
                Color32::WHITE,
            );
        }
    }
}
fn ratio_lock_button(ui: &mut egui::Ui, locked: bool) -> egui::Response {
    let response = ui
        .add_sized([30., 28.], egui::Button::new("").selected(locked))
        .on_hover_text(if locked {
            "解锁比例（当前已锁定）"
        } else {
            "锁定当前裁切比例"
        });
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            locked,
            if locked {
                "解锁比例"
            } else {
                "锁定比例"
            },
        )
    });
    let center = response.rect.center();
    let color = ui.style().interact(&response).fg_stroke.color;
    let painter = ui.painter();
    let stroke = Stroke::new(1.6, color);
    painter.rect_stroke(
        Rect::from_center_size(center + Vec2::new(0., 3.), Vec2::new(12., 10.)),
        2.,
        stroke,
        egui::StrokeKind::Inside,
    );
    let x = if locked { 0. } else { 4. };
    let points = (0..=16)
        .map(|i| {
            let angle = std::f32::consts::PI + i as f32 * std::f32::consts::PI / 16.;
            center + Vec2::new(x + angle.cos() * 4., -2. + angle.sin() * 5.)
        })
        .collect();
    painter.add(egui::Shape::line(points, stroke));
    painter.line_segment(
        [
            center + Vec2::new(4. + x, -2.),
            center + Vec2::new(4. + x, 1.),
        ],
        stroke,
    );
    if locked {
        painter.line_segment(
            [center + Vec2::new(-4., -2.), center + Vec2::new(-4., 1.)],
            stroke,
        );
    }
    painter.circle_filled(center + Vec2::new(0., 3.), 1.2, color);
    response
}
pub fn clockwise(o: u16) -> u16 {
    match o {
        1 => 6,
        6 => 3,
        3 => 8,
        8 => 1,
        2 => 7,
        7 => 4,
        4 => 5,
        5 => 2,
        _ => 1,
    }
}
fn counterclockwise(o: u16) -> u16 {
    clockwise(clockwise(clockwise(o)))
}
fn ratio_name(ratio: f32) -> String {
    let direction = if (ratio - 1.).abs() < 0.0001 {
        "方形"
    } else if ratio > 1. {
        "横向"
    } else {
        "竖向"
    };
    for (w, h) in [
        (1, 1),
        (3, 2),
        (2, 3),
        (4, 3),
        (3, 4),
        (5, 4),
        (4, 5),
        (7, 5),
        (5, 7),
        (16, 9),
        (9, 16),
    ] {
        if (ratio - w as f32 / h as f32).abs() < 0.0001 {
            return format!("{w} : {h} · {direction}");
        }
    }
    format!("{ratio:.4} : 1 · {direction}")
}
// A small dead band around the diagonal prevents orientation flicker. Only
// corner handles and drawing a new crop express both width and height intent.
fn drag_ratio(d: Drag, p: Pos2, ratio: f32) -> f32 {
    let anchor = match d.handle {
        0 => d.rect.right_bottom(),
        2 => d.rect.left_bottom(),
        4 => d.rect.left_top(),
        6 => d.rect.right_top(),
        9 => d.start,
        _ => return ratio,
    };
    let delta = (p - anchor).abs();
    let landscape = ratio.max(1. / ratio);
    if delta.x > delta.y * 1.08 {
        landscape
    } else if delta.y > delta.x * 1.08 {
        1. / landscape
    } else {
        ratio
    }
}
fn drag_rect(d: Drag, p: Pos2, ratio: Option<f32>) -> Rect {
    if d.handle == 8 {
        return d.rect.translate(p - d.start);
    }
    let (anchor, point) = match d.handle {
        0 => (d.rect.right_bottom(), p),
        2 => (d.rect.left_bottom(), p),
        4 => (d.rect.left_top(), p),
        6 => (d.rect.right_top(), p),
        1 => (d.rect.center_bottom(), Pos2::new(d.rect.center().x, p.y)),
        3 => (d.rect.left_center(), Pos2::new(p.x, d.rect.center().y)),
        5 => (d.rect.center_top(), Pos2::new(d.rect.center().x, p.y)),
        7 => (d.rect.right_center(), Pos2::new(p.x, d.rect.center().y)),
        _ => (d.start, p),
    };
    let delta = point - anchor;
    let mut size = delta.abs().max(Vec2::splat(1.));
    let edge = matches!(d.handle, 1 | 3 | 5 | 7);
    if let Some(r) = ratio.map(|r| drag_ratio(d, p, r)) {
        if matches!(d.handle, 1 | 5) {
            size.x = size.y * r;
        } else if matches!(d.handle, 3 | 7) || size.x / size.y > r {
            size.y = size.x / r;
        } else {
            size.x = size.y * r;
        }
    } else if edge {
        if matches!(d.handle, 1 | 5) {
            size.x = d.rect.width();
        } else {
            size.y = d.rect.height();
        }
    }
    let center = if edge {
        if matches!(d.handle, 1 | 5) {
            anchor + Vec2::new(0., delta.y.signum() * size.y * 0.5)
        } else {
            anchor + Vec2::new(delta.x.signum() * size.x * 0.5, 0.)
        }
    } else {
        anchor
            + Vec2::new(
                if delta.x < 0. { -1. } else { 1. },
                if delta.y < 0. { -1. } else { 1. },
            ) * size
                * 0.5
    };
    Rect::from_center_size(center, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locked_corner_drag_switches_orientation_and_edge_move_stays_stable() {
        let rect = Rect::from_min_max(Pos2::new(100., 100.), Pos2::new(400., 300.));
        for (handle, anchor) in [
            (0, rect.right_bottom()),
            (2, rect.left_bottom()),
            (4, rect.left_top()),
            (6, rect.right_top()),
            (9, rect.left_top()),
        ] {
            let d = Drag {
                start: rect.left_top(),
                rect,
                handle,
            };
            for (offset, ratio) in [
                (Vec2::new(100., 150.), 2. / 3.),
                (Vec2::new(180., 120.), 1.5),
            ] {
                let p = anchor + offset;
                let result = drag_rect(d, p, Some(1.5));
                assert!((result.width() / result.height() - ratio).abs() < 0.0001);
                assert_eq!(result.min, anchor);
            }
            let near_diagonal = anchor + Vec2::new(100., 101.);
            assert_eq!(drag_ratio(d, near_diagonal, 1.5), 1.5);
            assert_eq!(drag_ratio(d, near_diagonal, 2. / 3.), 2. / 3.);
            let free = drag_rect(d, anchor + Vec2::new(100., 170.), None);
            assert!((free.width() / free.height() - 100. / 170.).abs() < 0.0001);
        }
        for handle in [1, 3, 5, 7, 8] {
            let d = Drag {
                start: rect.center(),
                rect,
                handle,
            };
            assert_eq!(drag_ratio(d, Pos2::ZERO, 1.5), 1.5);
        }
    }
    #[test]
    fn locked_orientation_switch_is_undoable_through_pointer_events() {
        let ctx = egui::Context::default();
        let mut e = Editor::new(Crop::default(), Vec2::new(600., 600.), 1);
        e.geometry.rect = Rect::from_min_max(Pos2::new(100., 100.), Pos2::new(400., 300.));
        e.ratio = Some(1.5);
        let before = e.geometry.rect;
        let mut frame = |events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(700.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| {
                            let (_, response) = ui.allocate_exact_size(
                                Vec2::splat(600.),
                                egui::Sense::click_and_drag(),
                            );
                            e.overlay(
                                ui,
                                &response,
                                Rect::from_min_size(Pos2::ZERO, Vec2::splat(600.)),
                            );
                        });
                },
            );
        };
        frame(vec![]);
        frame(vec![
            egui::Event::PointerMoved(before.max),
            egui::Event::PointerButton {
                pos: before.max,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ]);
        let target = Pos2::new(300., 400.);
        frame(vec![egui::Event::PointerMoved(target)]);
        frame(vec![egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }]);
        assert_eq!(e.ratio, Some(2. / 3.));
        assert_eq!(e.geometry.rect.min, before.min);
        assert!((e.geometry.rect.width() / e.geometry.rect.height() - 2. / 3.).abs() < 0.0001);
        e.undo();
        assert_eq!(e.geometry.rect, before);
        assert_eq!(e.ratio, Some(1.5));
        e.redo();
        assert_eq!(e.ratio, Some(2. / 3.));
    }
    #[test]
    fn all_handles_resize_about_opposite_anchor_and_keep_locked_ratio() {
        let original = Rect::from_min_max(Pos2::new(100., 100.), Pos2::new(500., 300.));
        let pts = [
            original.left_top(),
            original.center_top(),
            original.right_top(),
            original.right_center(),
            original.right_bottom(),
            original.center_bottom(),
            original.left_bottom(),
            original.left_center(),
        ];
        for (handle, start) in pts.into_iter().enumerate() {
            let drag = Drag {
                start,
                rect: original,
                handle,
            };
            let next = drag_rect(drag, start + (start - original.center()) * 0.2, Some(2.));
            assert!(next.width() > original.width());
            assert!((next.width() / next.height() - 2.).abs() < 0.0001);
            let free = drag_rect(drag, start + Vec2::new(20., 10.), None);
            assert!(free.is_positive());
        }
        let moved = drag_rect(
            Drag {
                start: original.center(),
                rect: original,
                handle: 8,
            },
            original.center() + Vec2::new(20., -15.),
            Some(2.),
        );
        assert_eq!(moved.size(), original.size());
        assert_eq!(moved.center(), original.center() + Vec2::new(20., -15.));
    }
    #[test]
    fn preset_unicode_fractional_values_survive_restart_and_invalid_write_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ratios.tsv");
        let p = Preset {
            name: "宽银幕 电影".into(),
            width: 2.39,
            height: 1.,
        };
        save_presets(&path, std::slice::from_ref(&p)).unwrap();
        assert_eq!(load_presets(&path).unwrap(), vec![p.clone()]);
        assert!(
            save_presets(
                &path,
                &[Preset {
                    height: 0.,
                    ..p.clone()
                }]
            )
            .is_err()
        );
        assert_eq!(load_presets(&path).unwrap(), vec![p]);
    }
    #[test]
    fn undo_redo_and_quarter_turn_preserve_the_same_source_crop() {
        let size = Vec2::new(6000., 4000.);
        let mut e = Editor::new(Crop::default(), size, 1);
        e.select_ratio("16:9".into(), Some(16. / 9.));
        let value = e.value();
        let previous = e.geometry.rect;
        e.rotate(6);
        assert_eq!(e.value(), value);
        assert!((e.geometry.rect.width() / e.geometry.rect.height() - 9. / 16.).abs() < 0.0001);
        e.undo();
        assert_eq!(e.orientation, 1);
        assert_eq!(e.geometry.rect, previous);
        e.redo();
        assert_eq!(e.orientation, 6);
        assert_eq!(e.value(), value);
    }
    #[test]
    fn pointer_drag_changes_crop_through_actual_egui_response() {
        let ctx = egui::Context::default();
        let mut e = Editor::new(Crop::default(), Vec2::new(600., 400.), 1);
        e.geometry.rect = Rect::from_min_max(Pos2::new(100., 80.), Pos2::new(500., 320.));
        let before = e.geometry.rect;
        let frame = |e: &mut Editor, events: Vec<egui::Event>| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| {
                            let (_, r) = ui.allocate_exact_size(
                                Vec2::new(600., 400.),
                                egui::Sense::click_and_drag(),
                            );
                            e.overlay(
                                ui,
                                &r,
                                Rect::from_min_size(Pos2::ZERO, Vec2::new(600., 400.)),
                            );
                        });
                },
            );
        };
        frame(&mut e, vec![]);
        frame(
            &mut e,
            vec![
                egui::Event::PointerMoved(Pos2::new(100., 80.)),
                egui::Event::PointerButton {
                    pos: Pos2::new(100., 80.),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            &mut e,
            vec![egui::Event::PointerMoved(Pos2::new(140., 110.))],
        );
        frame(
            &mut e,
            vec![egui::Event::PointerButton {
                pos: Pos2::new(140., 110.),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        assert_eq!(e.geometry.rect.min, Pos2::new(140., 110.));
        assert_eq!(e.geometry.rect.max, before.max);
        e.undo();
        assert_eq!(e.geometry.rect, before);
    }
}
