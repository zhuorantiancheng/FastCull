use eframe::egui::{self, Color32, Vec2};
use fastcull::{
    organizing::{Node, Plan},
    session::Session,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct Editor {
    pub open: bool,
    pub selected: Option<PathBuf>,
    pub recursive: bool,
    pub plan: Plan,
    pub photos: BTreeSet<usize>,
    anchor: Option<usize>,
    file: Option<PathBuf>,
    query: String,
    tree: Arc<Vec<Node>>,
    pub blocked: bool,
    dialog: Option<Dialog>,
    pub error: Option<String>,
    pub direct: bool,
    pub move_busy: usize,
    pub queued: BTreeSet<usize>,
    pub drop_cursor: Option<egui::CursorIcon>,
    pub move_status: String,
    pub last_target: Option<PathBuf>,
    pub advance: bool,
    requests: Vec<MoveRequest>,
}

pub struct MoveRequest {
    pub indices: Vec<usize>,
    pub target: PathBuf,
    pub advance: bool,
}

enum Dialog {
    New {
        parent: PathBuf,
        name: String,
    },
    Move {
        indices: Vec<usize>,
        target: PathBuf,
    },
    Pending,
}
impl Editor {
    pub fn new(paths: &[PathBuf], session: Option<&Arc<Session>>) -> Self {
        let binding = session.and_then(|s| s.organization_file());
        let mut error = None;
        let plan = if let Some((file, id)) = &binding {
            if file.exists() {
                match Plan::read(file, id, paths.to_vec()) {
                    Ok(plan) => plan,
                    Err(e) => {
                        error = Some(format!("整理记录读取失败：{e:#}"));
                        Plan::new(id.clone(), paths.to_vec())
                    }
                }
            } else {
                Plan::new(id.clone(), paths.to_vec())
            }
        } else {
            Plan::new("standalone".into(), paths.to_vec())
        };
        let tree = Arc::new(plan.tree());
        let blocked = error.is_some();
        Self {
            open: paths
                .iter()
                .filter_map(|p| p.parent())
                .collect::<BTreeSet<_>>()
                .len()
                > 1,
            selected: None,
            recursive: true,
            plan,
            tree,
            blocked,
            photos: BTreeSet::new(),
            anchor: None,
            file: if blocked { None } else { binding.map(|b| b.0) },
            query: String::new(),
            dialog: None,
            error,
            direct: false,
            move_busy: 0,
            queued: BTreeSet::new(),
            drop_cursor: None,
            move_status: String::new(),
            last_target: None,
            advance: true,
            requests: Vec::new(),
        }
    }
    pub fn persist(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.blocked,
            "整理记录读取失败，原文件已保留；可以直接关闭，修复记录后再同步"
        );
        if let Some(file) = &self.file {
            self.plan.write(file)?;
        }
        Ok(())
    }
    pub fn create_folder(&mut self, parent: &Path, name: &str) -> anyhow::Result<PathBuf> {
        let result = self.plan.create_folder(parent, name, self.file.as_deref());
        self.tree = Arc::new(self.plan.tree());
        result
    }
    pub fn new_dialog(&mut self, parent: PathBuf) {
        self.dialog = Some(Dialog::New {
            parent,
            name: String::new(),
        });
    }
    pub fn move_dialog(&mut self, indices: Vec<usize>, target: PathBuf) {
        self.dialog = Some(Dialog::Move { indices, target });
    }
    pub fn pending_dialog(&mut self) {
        self.dialog = Some(Dialog::Pending);
    }
    pub fn dismiss_dialog(&mut self) {
        self.dialog = None;
    }
    pub fn allows(&self, i: usize) -> bool {
        self.selected
            .as_ref()
            .is_none_or(|p| self.plan.in_folder(i, p, self.recursive))
    }
    pub fn editing(&self, ctx: &egui::Context) -> bool {
        self.dialog.is_some() || ctx.memory(|m| m.focused()) == Some(egui::Id::new("folder-search"))
    }
    pub fn select(&mut self, i: usize, visible: &[usize], modifiers: egui::Modifiers) {
        if modifiers.shift
            && let Some(anchor) = self.anchor
            && let (Some(a), Some(b)) = (
                visible.iter().position(|j| *j == anchor),
                visible.iter().position(|j| *j == i),
            )
        {
            if !modifiers.command {
                self.photos.clear();
            }
            self.photos
                .extend(visible[a.min(b)..=a.max(b)].iter().copied());
        } else if modifiers.command {
            if !self.photos.remove(&i) {
                self.photos.insert(i);
            }
            self.anchor = Some(i);
        } else {
            self.photos.clear();
            self.photos.insert(i);
            self.anchor = Some(i);
        }
    }
    pub fn selection(&self, current: usize, visible: &[usize]) -> Vec<usize> {
        if self.direct {
            let selected: Vec<_> = self
                .photos
                .iter()
                .filter(|i| visible.contains(i))
                .copied()
                .collect();
            if !selected.is_empty() {
                return selected;
            }
        }
        if !self.direct && !self.photos.is_empty() {
            self.photos.iter().copied().collect()
        } else if visible.contains(&current) {
            vec![current]
        } else {
            Vec::new()
        }
    }
    pub fn stage(&mut self, indices: &[usize], target: &Path) -> bool {
        if self.direct && !self.blocked {
            self.requests.push(MoveRequest {
                indices: indices.to_vec(),
                target: target.to_owned(),
                advance: self.advance,
            });
            return true;
        }
        let result = self
            .file
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("请从 LR 打开选片器后安排移动"))
            .and_then(|file| self.plan.stage(indices, target, file));
        match result {
            Ok(()) => {
                self.tree = Arc::new(self.plan.tree());
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(format!("无法保存整理计划：{e:#}"));
                false
            }
        }
    }
    pub fn take_requests(&mut self) -> Vec<MoveRequest> {
        std::mem::take(&mut self.requests)
    }
    pub fn relocated(&mut self, changes: Vec<(usize, PathBuf)>) {
        if changes.is_empty() {
            return;
        }
        for (index, path) in changes {
            if let Some(parent) = self.plan.sources[index].parent() {
                self.plan.folders.insert(parent.to_owned());
            }
            self.plan.sources[index] = path;
        }
        self.tree = Arc::new(self.plan.tree());
    }
    pub fn repeat_move(&mut self, current: usize, visible: &[usize]) {
        if let Some(target) = self.last_target.clone() {
            let indices = self.selection(current, visible);
            self.stage(&indices, &target);
        } else {
            let target = self.plan.sources[current].parent().unwrap().to_owned();
            self.move_dialog(self.selection(current, visible), target);
        }
    }
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        current: usize,
        visible: &[usize],
        height: f32,
    ) -> bool {
        let mut changed = false;
        let selection = self.selection(current, visible);
        ui.horizontal(|ui| {
            ui.strong("文件夹");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.blocked && (self.selected.is_some() || !visible.is_empty()),
                        egui::Button::new("＋"),
                    )
                    .on_hover_text("新建子文件夹")
                    .clicked()
                {
                    let parent = self.selected.clone().or_else(|| {
                        self.plan
                            .effective_path(current)
                            .parent()
                            .map(Path::to_owned)
                    });
                    if let Some(parent) = parent {
                        self.dialog = Some(Dialog::New {
                            parent,
                            name: String::new(),
                        });
                    }
                }
            });
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .id(egui::Id::new("folder-search"))
                .hint_text("筛选文件夹")
                .desired_width(f32::INFINITY),
        );
        changed |= ui.checkbox(&mut self.recursive, "包含子文件夹").changed();
        if ui
            .selectable_label(
                self.selected.is_none(),
                format!("全部照片   {}", self.plan.sources.len()),
            )
            .clicked()
        {
            self.selected = None;
            changed = true;
        }
        let tree = self.tree.clone();
        ui.scope(|ui| {
            // A solid scrollbar reserves its own lane instead of covering counts.
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            ui.spacing_mut().scroll.bar_width = 8.;
            ui.spacing_mut().scroll.bar_inner_margin = 6.;
            egui::ScrollArea::vertical()
                .id_salt("folder-tree")
                .max_height(height.max(100.))
                .scroll_source(egui::scroll_area::ScrollSource {
                    drag: !egui::DragAndDrop::has_payload_of_type::<Vec<usize>>(ui.ctx()),
                    ..Default::default()
                })
                .show(ui, |ui| {
                    for node in tree.iter() {
                        changed |= self.node(ui, node, &selection);
                    }
                });
        });
        ui.small("数量为本次加载的照片");
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.blocked && (self.direct || self.file.is_some()) && !selection.is_empty(),
                    egui::Button::new(format!(
                        "{} · {} 张",
                        if self.direct {
                            "移动"
                        } else {
                            "安排移动"
                        },
                        selection.len()
                    )),
                )
                .on_hover_text(if self.direct {
                    "移动照片和旁车；Ctrl/Cmd / Shift 多选，拖到文件夹也可移动"
                } else if self.file.is_some() {
                    "先保存整理计划，退出后在 LR 中完成实际移动；Ctrl/Cmd / Shift 多选照片"
                } else {
                    "Bridge 中请使用原生文件移动；LR 整理计划需要从 LR 打开选片器"
                })
                .clicked()
            {
                let target = self
                    .last_target
                    .clone()
                    .or_else(|| self.selected.clone())
                    .or_else(|| {
                        self.plan
                            .effective_path(selection[0])
                            .parent()
                            .map(Path::to_owned)
                    })
                    .unwrap();
                self.dialog = Some(Dialog::Move {
                    indices: selection.clone(),
                    target,
                });
            }
            if ui.small_button("清除选择").clicked() {
                self.photos.clear();
                self.anchor = None;
            }
        });
        if self.direct {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.blocked && self.last_target.is_some() && !selection.is_empty(),
                        egui::Button::new("移到上次位置"),
                    )
                    .on_hover_text("M：移动所选照片到上次目标；未设目标时先选择文件夹")
                    .clicked()
                {
                    self.repeat_move(current, visible);
                }
                if let Some(target) = &self.last_target {
                    ui.add(
                        egui::Label::new(target.file_name().unwrap_or_default().to_string_lossy())
                            .truncate(),
                    )
                    .on_hover_text(target.display().to_string());
                }
            });
            ui.checkbox(&mut self.advance, "移动后下一张");
            if self.move_busy > 0 {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("移动队列：{} 张", self.move_busy));
                });
            } else if !self.move_status.is_empty() {
                ui.small(&self.move_status);
            }
        }
        if !self.direct {
            if ui
                .button(format!("待整理 {} 张", self.plan.moves.len()))
                .clicked()
            {
                self.dialog = Some(Dialog::Pending);
            }
            if !self.plan.moves.is_empty() {
                ui.small("文件尚未移动；退出后到 LR 完成");
            }
        }
        if let Some(error) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        changed
    }
    fn node(&mut self, ui: &mut egui::Ui, node: &Node, selection: &[usize]) -> bool {
        let mut changed = false;
        let query = self.query.to_lowercase();
        fn matches(n: &Node, query: &str) -> bool {
            n.path.to_string_lossy().to_lowercase().contains(query)
                || n.children.iter().any(|n| matches(n, query))
        }
        if !matches(node, &query) {
            return false;
        }
        if node.children.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(18.);
                changed |= self.row(ui, node, selection);
            });
        } else {
            let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                egui::Id::new(("folder-node", &node.path)),
                true,
            );
            state
                .show_header(ui, |ui| {
                    changed |= self.row(ui, node, selection);
                })
                .body(|ui| {
                    for child in &node.children {
                        let mut compact = child;
                        while compact.direct == 0
                            && compact.children.len() == 1
                            && !self.plan.folders.contains(&compact.path)
                        {
                            compact = &compact.children[0];
                        }
                        changed |= self.node(ui, compact, selection);
                    }
                });
        }
        changed
    }
    fn can_drop(&self, indices: &[usize], target: &Path) -> bool {
        !self.blocked
            && (self.direct || self.file.is_some())
            && !indices.is_empty()
            && indices
                .iter()
                .all(|i| *i < self.plan.sources.len() && !self.queued.contains(i))
            && indices.iter().any(|i| {
                self.plan.effective_path(*i).parent().is_some_and(|p| {
                    fastcull::organizing::path_key(p) != fastcull::organizing::path_key(target)
                })
            })
    }
    fn row(&mut self, ui: &mut egui::Ui, node: &Node, selection: &[usize]) -> bool {
        let label = node.path.file_name().map_or_else(
            || node.path.display().to_string(),
            |v| v.to_string_lossy().into_owned(),
        );
        let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width().max(1.), 24.));
        let response = ui.interact(
            rect,
            egui::Id::new(("folder-row", &node.path)),
            egui::Sense::click(),
        );
        let selected = self.selected.as_ref() == Some(&node.path);
        let visuals = ui.style().interact_selectable(&response, selected);
        let painter = ui.painter_at(rect);
        let count = painter.layout_no_wrap(
            node.count.to_string(),
            egui::FontId::proportional(12.),
            visuals.text_color(),
        );
        let count_pos = egui::pos2(
            rect.right() - 4. - count.size().x,
            rect.center().y - count.size().y / 2.,
        );
        let name_right = rect.right() - (count.size().x + 8.).max(32.) - 6.;
        if selected || response.hovered() || response.has_focus() {
            painter.rect_filled(rect, 3., visuals.weak_bg_fill);
        }
        let icon = rect.left_center() + Vec2::new(4., -4.);
        painter.rect_filled(
            egui::Rect::from_min_size(icon - Vec2::new(0., 3.), Vec2::new(7., 4.)),
            1.,
            Color32::from_gray(150),
        );
        painter.rect_filled(
            egui::Rect::from_min_size(icon, Vec2::new(14., 9.)),
            1.,
            Color32::from_gray(175),
        );
        let mut job = egui::text::LayoutJob::simple(
            label,
            egui::FontId::proportional(14.),
            visuals.text_color(),
            (name_right - rect.left() - 24.).max(1.),
        );
        job.wrap.max_rows = 1;
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        let pos = rect.left_center() + Vec2::new(24., -galley.size().y * 0.5);
        painter
            .with_clip_rect(rect.intersect(egui::Rect::from_min_max(
                rect.min,
                egui::pos2(name_right.max(rect.left()), rect.bottom()),
            )))
            .galley(pos, galley, visuals.text_color());
        painter.galley(count_pos, count, visuals.text_color());
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                ui.is_enabled(),
                selected,
                node.path.display().to_string(),
            )
        });
        let response = response.on_hover_text(node.path.display().to_string());
        let mut changed = false;
        if response.clicked() {
            self.selected = Some(node.path.clone());
            changed = true;
        }
        if let Some(payload) = response.dnd_hover_payload::<Vec<usize>>() {
            let allowed = self.can_drop(&payload, &node.path);
            let cursor = if allowed {
                egui::CursorIcon::Move
            } else {
                egui::CursorIcon::NotAllowed
            };
            self.drop_cursor = Some(cursor);
            ui.ctx().set_cursor_icon(cursor);
            ui.painter().rect_stroke(
                response.rect,
                2.,
                egui::Stroke::new(
                    1.,
                    if allowed {
                        Color32::LIGHT_BLUE
                    } else {
                        Color32::LIGHT_RED
                    },
                ),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(payload) = response.dnd_release_payload::<Vec<usize>>()
            && self.can_drop(&payload, &node.path)
        {
            changed |= self.stage(&payload, &node.path);
        }
        response.context_menu(|ui| {
            if ui
                .add_enabled(!self.blocked, egui::Button::new("新建子文件夹"))
                .clicked()
            {
                self.dialog = Some(Dialog::New {
                    parent: node.path.clone(),
                    name: String::new(),
                });
                ui.close();
            }
            if ui
                .add_enabled(
                    !self.blocked && (self.direct || self.file.is_some()) && !selection.is_empty(),
                    egui::Button::new(if self.direct {
                        "将已选照片移动到此处"
                    } else {
                        "将已选照片安排到此处"
                    }),
                )
                .clicked()
            {
                self.dialog = Some(Dialog::Move {
                    indices: selection.to_vec(),
                    target: node.path.clone(),
                });
                ui.close();
            }
        });
        changed
    }
    pub fn dialogs(&mut self, ctx: &egui::Context) -> bool {
        let Some(mut dialog) = self.dialog.take() else {
            return false;
        };
        let mut close = false;
        let mut changed = false;
        egui::Modal::new(egui::Id::new("folder-dialog")).frame(egui::Frame::popup(&ctx.style()).inner_margin(18.)).show(ctx, |ui| {
            ui.set_width(440_f32.min((ctx.content_rect().width() - 80.).max(240.)));
            ui.set_max_width(ui.available_width());
            ui.spacing_mut().item_spacing = Vec2::new(10., 10.);
            ui.spacing_mut().interact_size.y = 34.;
            for (style, size) in [(egui::TextStyle::Heading, 22.), (egui::TextStyle::Body, 16.), (egui::TextStyle::Button, 16.)] {
                ui.style_mut().text_styles.insert(style, egui::FontId::proportional(size));
            }
            match &mut dialog {
                Dialog::New { parent, name } => {
                    ui.heading("新建文件夹"); ui.label(parent.display().to_string());
                    ui.add(egui::TextEdit::singleline(name).id(egui::Id::new("new-folder-name")).hint_text("文件夹名称").desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        if ui.button("创建").clicked() {
                            match self.create_folder(parent, name) {
                                Ok(path) => { self.tree = Arc::new(self.plan.tree()); if !self.direct { self.selected = Some(path); } changed = true; self.error = None; close = true; }
                                Err(e) => { self.tree = Arc::new(self.plan.tree()); self.error = Some(format!("{e:#}")); }
                            }
                        }
                        if ui.button("取消").clicked() { close = true; }
                    });
                }
                Dialog::Move { indices, target } => {
                    ui.heading(format!("{} {} 张照片", if self.direct { "移动" } else { "安排移动" }, indices.len()));
                    ui.label(if self.direct { "ARW 和对应旁车一起移动，后台执行；已有同名文件时不覆盖。" } else { "保存整理计划，文件暂留原位置。退出后在 LR 中完成移动，保留原照片的全部编辑记录。" });
                    ui.label("目标文件夹");
                    let mut folders = Vec::new();
                    fn collect(nodes: &[Node], out: &mut Vec<PathBuf>) { for n in nodes { out.push(n.path.clone()); collect(&n.children, out); } }
                    collect(&self.tree, &mut folders);
                    let target_name = target.file_name().map_or_else(|| target.display().to_string(), |s| s.to_string_lossy().into_owned());
                    egui::ComboBox::from_id_salt("move-destination").width(ui.available_width()).truncate().selected_text(target_name)
                        .show_ui(ui, |ui| {
                            ui.set_width(420.);
                            for p in folders {
                                let response = ui.add(egui::Button::new(p.display().to_string()).truncate().selected(&p==target)).on_hover_text(p.display().to_string());
                                if response.clicked() { *target = p; ui.close(); }
                            }
                        }).response.on_hover_text(target.display().to_string());
                    if self.direct {
                        let mut text = target.display().to_string();
                        if ui.add(egui::TextEdit::singleline(&mut text).id(egui::Id::new("move-target-path")).hint_text("也可粘贴其他目标文件夹路径").desired_width(f32::INFINITY)).changed() {
                            *target = PathBuf::from(text.trim_matches('"'));
                        }
                    }
                    egui::ScrollArea::vertical().id_salt("move-sources").max_height(100.).show(ui, |ui| {
                        for &i in indices.iter() {
                            let p = &self.plan.sources[i];
                            let parent = p.parent().and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy();
                            ui.add(egui::Label::new(format!("{} · {}", p.file_name().unwrap_or_default().to_string_lossy(), parent)).truncate()).on_hover_text(p.display().to_string());
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button(if self.direct { "开始移动" } else { "保存计划" }).clicked() && self.stage(indices, target) { changed = true; close = true; }
                        if ui.button("取消").clicked() { close = true; }
                    });
                }
                Dialog::Pending => {
                    ui.heading(format!("待整理 {} 张照片", self.plan.moves.len()));
                    ui.label("取消计划不会移动或删除文件。");
                    let moves: Vec<_> = self.plan.moves.iter().map(|(a,b)| (a.clone(),b.clone())).collect();
                    egui::ScrollArea::vertical().id_salt("pending-moves").max_height(220.).show(ui, |ui| {
                        for (from, to) in moves {
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(Vec2::new((ui.available_width()-90.).max(60.), 48.), egui::Layout::top_down(egui::Align::Min), |ui| {
                                    ui.add(egui::Label::new(from.file_name().unwrap_or_default().to_string_lossy()).truncate()).on_hover_text(from.display().to_string());
                                    ui.add(egui::Label::new(format!("→ {}", to.parent().and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy())).truncate()).on_hover_text(to.parent().unwrap().display().to_string());
                                });
                                if ui.add_sized(Vec2::new(80., 34.),egui::Button::new("取消此项")).clicked() && let Some(file) = &self.file {
                                    match self.plan.cancel(&from, file) {
                                        Ok(()) => { self.tree = Arc::new(self.plan.tree()); self.error = None; changed = true; }
                                        Err(e) => self.error = Some(format!("{e:#}")),
                                    }
                                }
                            });
                            ui.separator();
                        }
                    });
                    if ui.button("返回").clicked() { close = true; }
                }
            }
            if let Some(e) = &self.error { ui.colored_label(Color32::LIGHT_RED, e); }
        });
        if !close {
            self.dialog = Some(dialog);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overflowing_tree_reserves_scrollbar_lane_and_aligns_counts() {
        for width in [220., 360.] {
            let ctx = egui::Context::default();
            ctx.style_mut(|style| style.animation_time = 0.);
            let paths: Vec<_> = (0..300)
                .map(|i| {
                    PathBuf::from(format!(
                        "E:/photos/long-folder-name-{:02}/photo-{i}.ARW",
                        i / 10
                    ))
                })
                .collect();
            let mut editor = Editor::new(&paths, None);
            let mut boundary = 0.;
            for frame in 0..3 {
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(640., 480.),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::SidePanel::left("test-tree")
                            .exact_width(width)
                            .show(ctx, |ui| {
                                boundary = ui.available_rect_before_wrap().right();
                                editor.show(ui, 0, &[0], 100.);
                            });
                    },
                );
                let counts: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|shape| {
                        if let egui::Shape::Text(text) = &shape.shape
                            && matches!(text.galley.job.text.as_str(), "300" | "10")
                        {
                            Some(text.pos.x + text.galley.size().x)
                        } else {
                            None
                        }
                    })
                    .collect();
                if frame == 2 {
                    assert!(counts.len() > 3);
                    for right in &counts {
                        assert!(
                            *right <= boundary - 17.,
                            "count={right}, boundary={boundary}"
                        );
                    }
                    assert!(counts.iter().all(|right| (*right - counts[0]).abs() < 1.));
                }
            }
        }
    }
    #[test]
    fn drag_cursor_and_drop_follow_target_and_pending_photo_state() {
        let ctx = egui::Context::default();
        let paths = [
            PathBuf::from("E:/source/one.ARW"),
            PathBuf::from("E:/other/two.ARW"),
        ];
        let mut editor = Editor::new(&paths, None);
        editor.direct = true;
        let mut node = Node {
            path: PathBuf::from("E:/target"),
            count: 0,
            direct: 0,
            children: vec![],
        };
        let render = |ctx: &egui::Context, editor: &mut Editor, node: &Node, events| {
            ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        editor.row(ui, node, &[0]);
                    });
                },
            )
        };
        render(&ctx, &mut editor, &node, vec![]);
        let center = ctx
            .read_response(egui::Id::new(("folder-row", &node.path)))
            .unwrap()
            .rect
            .center();
        for mode in 0..5 {
            node.path = if mode == 1 {
                PathBuf::from("E:/source")
            } else {
                PathBuf::from("E:/target")
            };
            editor.queued = if mode == 2 {
                [0].into()
            } else {
                BTreeSet::new()
            };
            editor.blocked = mode == 3;
            editor.direct = mode != 4;
            egui::DragAndDrop::clear_payload(&ctx);
            render(&ctx, &mut editor, &node, vec![]);
            egui::DragAndDrop::set_payload(&ctx, vec![0usize]);
            let output = render(
                &ctx,
                &mut editor,
                &node,
                vec![egui::Event::PointerMoved(center)],
            );
            assert_eq!(
                output.platform_output.cursor_icon,
                if mode == 0 {
                    egui::CursorIcon::Move
                } else {
                    egui::CursorIcon::NotAllowed
                }
            );
        }
        // Releasing on an invalid row must not queue a move or raise an error.
        let output = render(
            &ctx,
            &mut editor,
            &node,
            vec![egui::Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(
            output.platform_output.cursor_icon,
            egui::CursorIcon::NotAllowed
        );
        assert!(editor.take_requests().is_empty() && editor.error.is_none());
    }
    #[test]
    fn long_paths_in_move_and_pending_dialogs_stay_inside_minimum_window() {
        let ctx = egui::Context::default();
        let path = PathBuf::from(format!(
            "E:/{}/original.ARW",
            "long-parent-name/".repeat(20)
        ));
        let mut editor = Editor::new(std::slice::from_ref(&path), None);
        let target = PathBuf::from(format!("E:/{}/selected", "another-parent/".repeat(20)));
        editor.plan.moves.insert(path, target.join("original.ARW"));
        for mode in [0, 1, 2] {
            let pending = mode == 1;
            editor.direct = mode == 2;
            if pending {
                editor.pending_dialog();
            } else {
                editor.move_dialog(vec![0], target.clone());
            }
            for _ in 0..2 {
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(640., 480.),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        editor.dialogs(ctx);
                    },
                );
                let fill = egui::Frame::popup(&ctx.style()).fill;
                for shape in output.shapes {
                    if let egui::Shape::Rect(rect) = shape.shape
                        && rect.fill == fill
                        && rect.rect.height() > 100.
                    {
                        assert!(rect.rect.width() <= 520., "{:?}", rect.rect);
                        assert!(rect.rect.height() <= 440., "{:?}", rect.rect);
                    }
                }
            }
        }
    }
    #[test]
    fn multi_selection_uses_filtered_order_and_text_dialogs_keep_keys() {
        let paths: Vec<_> = (0..6).map(|i| PathBuf::from(format!("{i}.ARW"))).collect();
        let mut editor = Editor::new(&paths, None);
        let visible = [0, 2, 4, 5];
        editor.select(2, &visible, egui::Modifiers::NONE);
        editor.select(5, &visible, egui::Modifiers::SHIFT);
        assert_eq!(editor.selection(0, &visible), [2, 4, 5]);
        editor.select(4, &visible, egui::Modifiers::COMMAND);
        assert_eq!(editor.selection(0, &visible), [2, 5]);
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("folder-search")));
        assert!(editor.editing(&ctx));
        ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("folder-search")));
        editor.new_dialog(PathBuf::from("C:/example"));
        assert!(editor.editing(&ctx));
        editor.dismiss_dialog();
        assert!(!editor.editing(&ctx));
        let _ = ctx.end_pass();
    }
    #[test]
    fn corrupt_plan_is_preserved_and_cannot_be_replaced_on_exit() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![dir.path().join("source.ARW")];
        let manifest = dir.path().join("session.txt");
        fastcull::handoff::Manifest::new("session-test".into(), &paths)
            .unwrap()
            .write(&manifest)
            .unwrap();
        let session = Arc::new(Session::new(vec![fastcull::state::Item {
            path: paths[0].clone(),
            updates: Default::default(),
        }]));
        session.attach_manifest(manifest.clone()).unwrap();
        let file = manifest.with_extension("folders");
        std::fs::write(&file, "damaged but preserved").unwrap();
        let editor = Editor::new(&paths, Some(&session));
        assert!(editor.blocked);
        assert!(editor.persist().is_err());
        assert_eq!(
            std::fs::read_to_string(file).unwrap(),
            "damaged but preserved"
        );
    }
}
