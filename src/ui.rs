use crate::cache::Pool;
use anyhow::Result;
use eframe::egui::{self, Color32, Key, Pos2, Rect, Stroke, TextureHandle, TextureOptions, Vec2};
#[cfg(test)]
use fastcull::xmp;
use fastcull::{
    arw::ArwInfo,
    imaging::Level,
    session::Session,
    state,
    xmp::{Updates, Values},
};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Instant};

const PANEL: Color32 = Color32::from_rgb(32, 35, 40);
const BG: Color32 = Color32::from_rgb(20, 22, 26);
const RED: Color32 = Color32::from_rgb(201, 79, 79);
const COLORS: [(&str, Color32); 5] = [
    ("Red", Color32::from_rgb(206, 75, 66)),
    ("Yellow", Color32::from_rgb(226, 185, 55)),
    ("Green", Color32::from_rgb(119, 171, 79)),
    ("Blue", Color32::from_rgb(90, 121, 165)),
    ("Purple", Color32::from_rgb(162, 127, 165)),
];

struct Texture {
    handle: TextureHandle,
    bytes: usize,
    used: u64,
}
struct TextureCache {
    pinned: Option<usize>,
    items: HashMap<usize, Texture>,
    budget: usize,
    limit: usize,
    clock: u64,
}
impl TextureCache {
    fn new(budget: usize, limit: usize) -> Self {
        Self {
            pinned: None,
            items: HashMap::new(),
            budget,
            limit,
            clock: 0,
        }
    }
    fn get(&mut self, i: usize) -> Option<TextureHandle> {
        self.clock += 1;
        let t = self.items.get_mut(&i)?;
        t.used = self.clock;
        Some(t.handle.clone())
    }
    fn insert(&mut self, i: usize, t: TextureHandle, protected: usize) {
        self.insert_with_priority(i, t, protected, false);
    }
    fn insert_near(&mut self, i: usize, t: TextureHandle, protected: usize) {
        self.insert_with_priority(i, t, protected, true);
    }
    fn insert_with_priority(&mut self, i: usize, t: TextureHandle, protected: usize, near: bool) {
        self.clock += 1;
        let bytes = t.size()[0] * t.size()[1] * 4;
        self.items.insert(
            i,
            Texture {
                handle: t,
                bytes,
                used: self.clock,
            },
        );
        while self.items.len() > self.limit
            || self.items.values().map(|t| t.bytes).sum::<usize>() > self.budget
        {
            let victim = self
                .items
                .iter()
                .filter(|(k, _)| **k != protected && Some(**k) != self.pinned)
                .max_by_key(|(k, t)| {
                    (
                        if near { k.abs_diff(protected) } else { 0 },
                        u64::MAX - t.used,
                    )
                })
                .map(|(k, _)| *k);
            if let Some(k) = victim {
                self.items.remove(&k);
            } else {
                break;
            }
        }
    }
}
#[derive(Default)]
struct Photo {
    develop_loaded: bool,
    crop_edited: bool,
    orientation_edited: bool,
    full_size: Option<Vec2>,
    exposure_base: Option<i32>,
    exposure_delta: f64,
    info: Option<ArwInfo>,
    values: Values,
    loaded: bool,
    image_errors: HashMap<Level, String>,
    metadata_error: Option<String>,
    capture_indexed: bool,
    capture_error: Option<String>,
    fit_edge_loaded: u32,
    ms: f64,
}
impl Photo {
    fn apply_develop_baseline(&mut self, baseline: crate::exposure::Baseline) {
        if self.exposure_base.is_none() {
            self.exposure_base = baseline.exposure;
            if let Some(e) = baseline.exposure {
                self.values.exposure = Some(e);
            }
        }
        if !self.crop_edited && let Some(crop) = baseline.crop {
            self.values.crop = Some(crop);
        }
        if !self.orientation_edited && let Some(o) = baseline.orientation {
            self.values.orientation = Some(o);
        }
        self.develop_loaded = true;
    }
    fn orientation(&self) -> u16 {
        self.values
            .orientation
            .unwrap_or_else(|| self.info.as_ref().map_or(1, |i| i.orientation))
    }
}
pub struct App {
    crop_editor: Option<crate::crop_ui::Editor>,
    crop_requested: bool,
    baselines: crate::exposure::Baselines,
    exposure_preview: Option<Arc<crate::exposure_preview::Preview>>,
    compare: Option<Compare>,
    compare_sizes: [Vec2; 2],
    compare_ppp: f32,
    close_dialog: bool,
    allow_close: bool,
    paths: Arc<Vec<PathBuf>>,
    photos: Vec<Photo>,
    pool: Pool,
    full: TextureCache,
    fit: TextureCache,
    fit_edge: u32,
    thumbs: TextureCache,
    current: usize,
    visible: Vec<usize>,
    filters: fastcull::filtering::Filters,
    filter_editor: crate::filter_ui::Editor,
    folder_editor: crate::folder_ui::Editor,
    mover: Option<crate::move_jobs::Mover>,
    close_after_moves: bool,
    filter_dirty: bool,
    grid: bool,
    zoom: bool,
    // Fraction of the oriented image dimensions, independent of resolution/DPI.
    pan: Vec2,
    canvas_size: Vec2,
    error: Option<String>,
    started: Instant,
    first_draw: bool,
    grid_scroll: bool,
    smoke: Option<Smoke>,
    prefetched: std::collections::HashSet<(usize, Level)>,
    direction: isize,
    film_wheel: f32,
    stress_remaining: usize,
    session: Option<Arc<Session>>,
    first_frame: bool,
    navigation_at: Instant,
    startup_focus: crate::focus::StartupFocus,
    zoom_started: Option<Instant>,
}
#[derive(Clone, Copy, Default)]
struct View {
    zoom: f32,
    pan: Vec2,
}
struct Compare {
    indices: [usize; 2],
    views: [View; 2],
    active: usize,
    linked: bool,
}
impl Compare {
    fn scale(&mut self, side: usize, target: f32, fit: [f32; 2]) {
        if target <= 0. {
            self.views[side].zoom = 0.;
            if self.linked {
                self.views[1 - side].zoom = 0.;
            }
            return;
        }
        let effective = self.views.map(|v| v.zoom);
        let effective = std::array::from_fn::<_, 2, _>(|i| {
            if effective[i] > 0. {
                effective[i]
            } else {
                fit[i].clamp(0.02, 8.)
            }
        });
        let mut lower: f32 = 0.;
        let mut upper: f32 = f32::INFINITY;
        for (i, scale) in effective.iter().enumerate() {
            if i == side || self.linked {
                lower = lower.max(0.02 / scale);
                upper = upper.min(8. / scale);
            }
        }
        let ratio = (target / effective[side]).clamp(lower, upper);
        for (i, scale) in effective.iter().enumerate() {
            if i == side || self.linked {
                // Pan is an image-relative anchor, so scaling must not replace it.
                self.views[i].zoom = scale * ratio;
            }
        }
    }
    fn translate(&mut self, side: usize, delta: Vec2, limits: [Vec2; 2]) {
        let mut lower = Vec2::splat(f32::NEG_INFINITY);
        let mut upper = Vec2::splat(f32::INFINITY);
        for (i, limit) in limits.iter().enumerate() {
            if i == side || self.linked {
                // Stop BOTH at the first edge. Independent clamping would lose alignment.
                lower = lower.max((-*limit - self.views[i].pan).min(Vec2::ZERO));
                upper = upper.min((*limit - self.views[i].pan).max(Vec2::ZERO));
            }
        }
        let delta = delta.max(lower).min(upper);
        for (i, v) in self.views.iter_mut().enumerate() {
            if i == side || self.linked {
                v.pan += delta;
            }
        }
    }
}
struct Smoke {
    dir: PathBuf,
    step: usize,
    waiting: bool,
    last: Instant,
    prepared: bool,
    cached_zoom_tested: bool,
    relative_link_tested: bool,
}
fn clockwise(o: u16) -> u16 {
    match o {
        1 => 6,
        2 => 7,
        3 => 8,
        4 => 5,
        5 => 2,
        6 => 3,
        7 => 4,
        8 => 1,
        _ => 1,
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> Self {
        Self::new_session(cc, paths, None)
    }
    fn new_session(
        cc: &eframe::CreationContext<'_>,
        paths: Vec<PathBuf>,
        session: Option<Arc<Session>>,
    ) -> Self {
        configure(&cc.egui_ctx);
        Self::from_context(&cc.egui_ctx, cc.gl.as_ref(), paths, session)
    }
    fn from_context(
        ctx: &egui::Context,
        gl: Option<&Arc<eframe::glow::Context>>,
        paths: Vec<PathBuf>,
        session: Option<Arc<Session>>,
    ) -> Self {
        let app_started = Instant::now();
        log::info!("startup app-builder begin");
        log::info!(
            "startup font-config elapsed_ms={:.1}",
            app_started.elapsed().as_secs_f64() * 1000.
        );
        let paths = Arc::new(paths);
        let baselines = crate::exposure::Baselines::new(
            session.as_ref().and_then(|s| s.exposure_file()),
            ctx.clone(),
        );
        let pool = Pool::new(paths.clone(), ctx.clone(), session.clone());
        log::info!(
            "startup worker-pool elapsed_ms={:.1}",
            app_started.elapsed().as_secs_f64() * 1000.
        );
        let count = paths.len();
        let mut folder_editor = crate::folder_ui::Editor::new(&paths, session.as_ref());
        folder_editor.open = false;
        let smoke = std::env::var_os("FASTCULL_SMOKE_DIR").map(|d| Smoke {
            dir: PathBuf::from(d),
            step: 0,
            waiting: false,
            last: Instant::now(),
            prepared: false,
            cached_zoom_tested: false,
            relative_link_tested: false,
        });
        let stress_remaining = if std::env::var_os("FASTCULL_STRESS").is_some() {
            180
        } else {
            0
        };
        Self {
            folder_editor,
            crop_editor: None,
            crop_requested: false,
            mover: None,
            close_after_moves: false,
            baselines,
            exposure_preview: gl.map(|g| crate::exposure_preview::Preview::new(g.clone())),
            compare: None,
            compare_sizes: [Vec2::ZERO; 2],
            compare_ppp: 1.,
            close_dialog: false,
            allow_close: false,
            paths,
            photos: (0..count)
                .map(|i| Photo {
                    values: session
                        .as_ref()
                        .map_or_else(Values::default, |s| s.initial_values(i)),
                    ..Default::default()
                })
                .collect(),
            pool,
            full: TextureCache::new(300 * 1024 * 1024, 4),
            fit: TextureCache::new(96 * 1024 * 1024, 8),
            fit_edge: 2560,
            thumbs: TextureCache::new(64 * 1024 * 1024, 4096),
            current: 0,
            visible: (0..count).collect(),
            filters: Default::default(),
            filter_editor: crate::filter_ui::Editor::opened(),
            filter_dirty: false,
            grid: false,
            zoom: false,
            pan: Vec2::ZERO,
            canvas_size: Vec2::ZERO,
            error: None,
            started: Instant::now(),
            first_draw: false,
            grid_scroll: false,
            smoke,
            prefetched: Default::default(),
            direction: 1,
            film_wheel: 0.,
            stress_remaining,
            session,
            first_frame: false,
            navigation_at: Instant::now(),
            startup_focus: crate::focus::StartupFocus::new(),
            zoom_started: None,
        }
    }
    fn enable_bridge_moves(&mut self, ctx: &egui::Context, directory: PathBuf) {
        self.folder_editor.direct = true;
        self.mover = Some(crate::move_jobs::Mover::new(
            self.pool.locations(),
            ctx.clone(),
            directory,
        ));
    }
    fn moving(&self, index: usize) -> bool {
        self.mover
            .as_ref()
            .is_some_and(|m| m.pending.contains(&index))
    }
    fn submit_moves(&mut self) {
        let requests = self.folder_editor.take_requests();
        let has_requests = !requests.is_empty();
        for request in requests {
            let result = (|| -> Result<()> {
                anyhow::ensure!(!self.close_after_moves, "已请求完成移动后关闭");
                let mover = self
                    .mover
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("实际移动仅用于 Bridge 或独立模式"))?;
                anyhow::ensure!(
                    request.indices.iter().all(|i| !mover.pending.contains(i)),
                    "所选照片已在移动队列中"
                );
                let items =
                    fastcull::relocating::prepare(&self.paths, &request.indices, &request.target)?;
                mover.enqueue(items)?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    self.folder_editor.last_target = Some(request.target);
                    self.folder_editor.photos.clear();
                    self.error = None;
                    if request.advance && request.indices.contains(&self.current) {
                        let position = self
                            .visible
                            .iter()
                            .position(|&i| i == self.current)
                            .unwrap_or(0);
                        let next = self
                            .visible
                            .iter()
                            .skip(position + 1)
                            .chain(self.visible.iter().take(position))
                            .find(|&&i| {
                                !self.moving(i)
                                    && self
                                        .compare
                                        .as_ref()
                                        .is_none_or(|c| c.indices[1 - c.active] != i)
                            })
                            .copied();
                        if let Some(i) = next {
                            self.go(i);
                            self.folder_editor
                                .select(i, &self.visible, egui::Modifiers::NONE);
                        }
                    }
                }
                Err(e) => {
                    self.error = Some(format!("无法移动：{e:#}"));
                }
            }
        }
        if let Some(mover) = &self.mover {
            self.folder_editor.move_busy = mover.pending.len();
            if has_requests {
                self.folder_editor.queued.clone_from(&mover.pending);
            }
        }
    }
    fn receive_moves(&mut self, ctx: &egui::Context) {
        let mut changed_paths = Vec::new();
        let events = self.mover.as_mut().map(|m| m.take()).unwrap_or_default();
        let has_events = !events.is_empty();
        for event in events {
            match event {
                crate::move_jobs::Event::Photo { index, path } => {
                    Arc::make_mut(&mut self.paths)[index] = path.clone();
                    changed_paths.push((index, path));
                    self.filter_dirty = true;
                }
                crate::move_jobs::Event::Finished { error: Some(e), .. } => {
                    log::error!("Bridge move: {e}");
                    self.error = Some(format!("移动未全部完成：{e}"));
                    self.close_after_moves = false;
                }
                _ => {}
            }
        }
        self.folder_editor.relocated(changed_paths);
        if let Some(mover) = &self.mover {
            self.folder_editor.move_busy = mover.pending.len();
            if has_events {
                self.folder_editor.queued.clone_from(&mover.pending);
            }
            self.folder_editor.move_status.clone_from(&mover.status);
        }
        if self.close_after_moves && self.mover.as_ref().is_none_or(|m| m.jobs == 0) {
            self.close_after_moves = false;
            self.finish_close(ctx, false);
        }
    }
    fn go(&mut self, i: usize) {
        if self.crop_editor.is_some() {
            return;
        }
        self.crop_requested = false;
        if !self.visible.contains(&i) {
            return;
        }
        let i = i.min(self.paths.len() - 1);
        if i == self.current {
            return;
        }
        if let Some(c) = &self.compare
            && c.indices[1 - c.active] == i
        {
            self.activate_compare(1 - c.active);
            return;
        }
        if let Some(c) = &mut self.compare {
            c.indices[c.active] = i;
        }
        if i != self.current {
            self.direction = if i > self.current { 1 } else { -1 };
            self.current = i;
            self.navigation_at = Instant::now();
            self.pool.navigate(i);
            self.prefetched.clear();
            if !self.zoom {
                self.pan = Vec2::ZERO;
            }
            self.grid_scroll = true;
            self.zoom_started = self.zoom.then(Instant::now);
        }
    }
    fn next(&mut self, d: isize) {
        if let Some(i) = fastcull::filtering::step_visible(&self.visible, self.current, d) {
            if self
                .compare
                .as_ref()
                .is_some_and(|c| c.indices[1 - c.active] == i)
            {
                if let Some(j) = fastcull::filtering::step_visible(&self.visible, i, d) {
                    self.folder_editor
                        .select(j, &self.visible, egui::Modifiers::NONE);
                    self.go(j);
                }
                return;
            }
            self.folder_editor
                .select(i, &self.visible, egui::Modifiers::NONE);
            self.go(i);
        }
    }
    fn refilter(&mut self) {
        self.filter_editor.invalidate();
        let visible: Vec<_> = self
            .photos
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                (self.folder_editor.allows(i)
                    && self.filters.matches(
                        &fastcull::filtering::Row {
                            path: &self.paths[i],
                            values: &p.values,
                            capture: p.info.as_ref().map(|v| &v.capture),
                            indexed: p.capture_indexed || p.info.is_some(),
                        },
                        None,
                    ))
                .then_some(i)
            })
            .collect();
        let changed = visible != self.visible;
        self.visible = visible;
        if self
            .compare
            .as_ref()
            .is_some_and(|c| c.indices.iter().any(|i| !self.visible.contains(i)))
        {
            self.compare = None;
        }
        if let Some(i) = fastcull::filtering::reconcile_visible(&self.visible, self.current) {
            self.go(i);
        }
        self.filter_dirty = false;
        self.grid_scroll |= changed;
    }
    fn set_zoom(&mut self, zoom: bool) {
        if let Some(c) = &self.compare {
            self.zoom_compare(c.active, if zoom { 1. } else { 0. });
        }
        self.zoom = zoom;
        self.pan = Vec2::ZERO;
        self.zoom_started = zoom.then(Instant::now);
        if zoom {
            log::info!(
                "100% requested image={} cached={}",
                self.current,
                self.full.items.contains_key(&self.current)
            );
        }
    }
    fn save(&mut self, u: Updates) {
        if self.visible.is_empty() || !self.photos[self.current].loaded || self.moving(self.current)
        {
            return;
        }
        let saved_updates = u.clone();
        match self.pool.write_xmp(self.current, &u) {
            Ok(_) => {
                self.photos[self.current].crop_edited |= u.crop.is_some();
                self.photos[self.current].orientation_edited |= u.orientation.is_some();
                let v = &mut self.photos[self.current].values;
                if let Some(r) = u.rating {
                    v.rating = Some(r);
                }
                if let Some(l) = u.label {
                    v.label = Some(l);
                }
                if let Some(f) = u.flag {
                    v.flag = Some(f);
                }
                if let Some(o) = u.orientation {
                    v.orientation = Some(o);
                }
                if let Some(e) = u.exposure {
                    v.exposure = Some(e);
                }
                if let Some(crop) = u.crop {
                    v.crop = Some(crop);
                }
                self.error = None;
                self.filter_dirty = true;
                if let Some(session) = &self.session
                    && let Err(e) = session.record(self.current, &saved_updates)
                {
                    self.error = Some(format!(
                        "旁车已保存，但会话同步记录保存失败：{e:#}。请重试保存，详情见日志。"
                    ));
                    log::error!("session record: {e:#}");
                }
            }
            Err(e) => {
                let msg = format!("无法保存 {}：{e:#}", self.paths[self.current].display());
                log::error!("{msg}");
                self.error = Some(msg);
            }
        }
    }
    fn rating(&mut self, r: u8) {
        self.save(Updates {
            rating: Some(r),
            ..Default::default()
        });
    }
    fn flag(&mut self, f: &str) {
        self.save(Updates {
            flag: Some(f.to_owned()),
            ..Default::default()
        });
    }
    fn label(&mut self, l: &str) {
        let value = if self.photos[self.current]
            .values
            .label
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case(l))
        {
            ""
        } else {
            l
        };
        self.save(Updates {
            label: Some(value.to_owned()),
            ..Default::default()
        });
    }
    fn rotate(&mut self, d: i32) {
        let p = &self.photos[self.current];
        if p.info.is_some() {
            let mut o = p.orientation();
            for _ in 0..d.rem_euclid(4) {
                o = clockwise(o);
            }
            self.save(Updates {
                orientation: Some(o),
                ..Default::default()
            });
            if self.error.is_none() {
                self.pan = Vec2::ZERO;
            }
        }
    }
    fn keyboard(&mut self, ctx: &egui::Context) {
        if self.crop_editor.is_some() {
            if ctx.wants_keyboard_input() || egui::Popup::is_any_open(ctx) {
                return;
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
                self.crop_editor = None;
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter)) {
                self.apply_crop();
            } else if ctx.input_mut(|i| {
                i.consume_key(
                    egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
                    Key::Z,
                )
            }) {
                self.crop_editor.as_mut().unwrap().redo();
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Z)) {
                self.crop_editor.as_mut().unwrap().undo();
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Y)) {
                self.crop_editor.as_mut().unwrap().redo();
            }
            return;
        }
        if self.close_dialog || egui::Popup::is_any_open(ctx) {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::F)) {
            self.filter_editor.open = !self.filter_editor.open;
            if self.filter_editor.open {
                self.folder_editor.open = false;
            }
        }
        if self.filter_editor.editing(ctx) || self.folder_editor.editing(ctx) {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::A)) {
            self.folder_editor.photos = self.visible.iter().copied().collect();
            return;
        }
        // Preserve physical event order, including repeated keys in one frame.
        // Otherwise "5, right" grades the next photo instead of the original.
        let keys = ctx.input_mut(|input| {
            let mut keys = Vec::new();
            input.events.retain(|event| {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                    && *modifiers == egui::Modifiers::NONE
                    && matches!(
                        key,
                        Key::Escape
                            | Key::Q
                            | Key::G
                            | Key::C
                            | Key::D
                            | Key::E
                            | Key::Enter
                            | Key::F
                            | Key::R
                            | Key::Home
                            | Key::End
                            | Key::PageDown
                            | Key::Space
                            | Key::PageUp
                            | Key::ArrowRight
                            | Key::ArrowLeft
                            | Key::ArrowDown
                            | Key::ArrowUp
                            | Key::P
                            | Key::X
                            | Key::U
                            | Key::Num0
                            | Key::Num1
                            | Key::Num2
                            | Key::Num3
                            | Key::Num4
                            | Key::Num5
                            | Key::Num6
                            | Key::Num7
                            | Key::Num8
                            | Key::Num9
                            | Key::OpenBracket
                            | Key::CloseBracket
                            | Key::L
                            | Key::M
                            | Key::T
                    )
                {
                    keys.push(*key);
                    false
                } else {
                    true
                }
            });
            keys
        });
        for key in keys {
            if matches!(key, Key::Escape | Key::Q) {
                self.close_dialog = true;
                break;
            }
            if self.visible.is_empty() {
                continue;
            }
            match key {
                Key::G => {
                    self.grid = true;
                    self.grid_scroll = true;
                }
                Key::D | Key::E | Key::Enter => {
                    self.grid = false;
                    self.compare = None;
                }
                Key::C => self.enter_compare(),
                Key::F => self.set_zoom(!self.zoom),
                Key::R => self.retry_current(),
                Key::T => self.begin_crop(),
                Key::M if self.folder_editor.direct => {
                    self.folder_editor.repeat_move(self.current, &self.visible);
                    self.submit_moves();
                }
                Key::Home => {
                    if let Some(&i) = self.visible.first() {
                        self.go(i);
                    }
                }
                Key::End => {
                    if let Some(&i) = self.visible.last() {
                        self.go(i);
                    }
                }
                Key::PageDown | Key::Space | Key::ArrowRight => self.next(1),
                Key::PageUp | Key::ArrowLeft => self.next(-1),
                Key::ArrowDown if self.grid => self.next(6),
                Key::ArrowUp if self.grid => self.next(-6),
                Key::ArrowDown if self.zoom => {
                    self.move_view(Vec2::new(0., -100.), ctx.pixels_per_point())
                }
                Key::ArrowUp if self.zoom => {
                    self.move_view(Vec2::new(0., 100.), ctx.pixels_per_point())
                }
                Key::P => self.flag("pick"),
                Key::X => self.flag("reject"),
                Key::U => self.flag(""),
                Key::Num0 => self.rating(0),
                Key::Num1 => self.rating(1),
                Key::Num2 => self.rating(2),
                Key::Num3 => self.rating(3),
                Key::Num4 => self.rating(4),
                Key::Num5 => self.rating(5),
                Key::Num6 => self.label("Red"),
                Key::Num7 => self.label("Yellow"),
                Key::Num8 => self.label("Green"),
                Key::Num9 => self.label("Blue"),
                Key::OpenBracket => self.rotate(-1),
                Key::CloseBracket => self.rotate(1),
                Key::L => {
                    let _ = fastcull::platform::open_directory(&crate::logger::directory());
                }
                _ => {}
            }
        }
    }
    fn move_view(&mut self, delta: Vec2, ppp: f32) {
        let Some(c) = &self.compare else {
            if let Some(size) = self.native_size(self.current) {
                let shown = self.displayed_size(self.current, size) / ppp;
                let limits = (shown - self.canvas_size).max(Vec2::ZERO) * 0.5 / shown;
                self.pan = (self.pan + delta / shown).clamp(-limits, limits);
            }
            return;
        };
        let side = c.active;
        let geometry = self.compare_geometry(ppp);
        let Some(shown) = geometry[side] else {
            return;
        };
        if c.linked && geometry[1 - side].is_none() {
            return;
        }
        let limits = std::array::from_fn(|i| {
            geometry[i].map_or(Vec2::ZERO, |shown| {
                ((shown - self.compare_sizes[i]).max(Vec2::ZERO) * 0.5) / shown
            })
        });
        self.compare
            .as_mut()
            .unwrap()
            .translate(side, delta / shown, limits);
    }
    fn compare_geometry(&self, ppp: f32) -> [Option<Vec2>; 2] {
        let c = self.compare.as_ref().unwrap();
        std::array::from_fn(|side| {
            let i = c.indices[side];
            let size = self.displayed_size(i, self.native_size(i)?);
            (c.views[side].zoom > 0.).then_some(size * c.views[side].zoom / ppp)
        })
    }
    fn zoom_compare(&mut self, side: usize, target: f32) {
        let c = self.compare.as_ref().unwrap();
        let fit = std::array::from_fn(|side| {
            let i = c.indices[side];
            self.native_size(i).map_or(1., |size| {
                let size = self.displayed_size(i, size);
                let area = self.compare_sizes[side];
                if area.x <= 0. || area.y <= 0. {
                    return 1.;
                }
                (area.x / size.x).min(area.y / size.y) * self.compare_ppp
            })
        });
        self.compare.as_mut().unwrap().scale(side, target, fit);
        self.zoom = self.compare.as_ref().unwrap().views[side].zoom > 0.;
    }
    fn receive_metadata(&mut self) {
        for message in self.pool.take_metadata() {
            self.filter_dirty = true;
            let photo = &mut self.photos[message.index];
            match message.result {
                Ok(values) => {
                    if !photo.loaded {
                        photo.values = values;
                        photo.develop_loaded = false;
                    }
                    photo.loaded = true;
                    photo.metadata_error = None;
                }
                Err(e) => {
                    log::error!("metadata {}: {e}", self.paths[message.index].display());
                    photo.metadata_error = Some(e);
                    photo.loaded = false;
                }
            }
        }
    }
    fn receive_index(&mut self) {
        for message in self.pool.take_indexed() {
            let photo = &mut self.photos[message.index];
            photo.capture_indexed = true;
            match message.result {
                Ok(info) => {
                    photo.info = Some(info);
                    photo.capture_error = None;
                }
                Err(error) => photo.capture_error = Some(error),
            }
            self.filter_dirty = true;
        }
    }
    fn receive(&mut self, ctx: &egui::Context) {
        for (i, size) in self.pool.take_full_sizes() {
            self.photos[i].full_size = Some(Vec2::new(size[0] as f32, size[1] as f32));
        }
        // Limit expensive full-size uploads to one per frame.
        for _ in 0..4 {
            let Some(message) = self.pool.take_decoded() else {
                break;
            };
            let i = message.task.index;
            if let Level::Fit(edge) = message.task.level
                && self.fit.items.contains_key(&i)
                && self.photos[i].fit_edge_loaded > edge
            {
                continue;
            }
            match message.result {
                Ok((info, p)) => {
                    let photo = &mut self.photos[i];
                    self.filter_dirty |= photo.info.is_none();
                    photo.info = Some(info);
                    photo.ms = message.ms;
                    photo.image_errors.remove(&message.task.level);
                    let t = ctx.load_texture(
                        format!("{}-{:?}", i, message.task.level),
                        p,
                        TextureOptions::LINEAR,
                    );
                    if message.task.level == Level::Full {
                        self.photos[i].full_size = Some(t.size_vec2());
                        self.full.insert_near(i, t, self.current);
                        ctx.request_repaint();
                        break;
                    } else if let Level::Fit(edge) = message.task.level {
                        self.photos[i].fit_edge_loaded = edge;
                        self.fit.insert_near(i, t, self.current);
                    } else {
                        self.thumbs.insert(i, t, self.current);
                    }
                }
                Err(e) => {
                    log::error!("decode {}: {e}", self.paths[i].display());
                    self.photos[i].image_errors.insert(message.task.level, e);
                }
            }
        }
    }
    fn request(&mut self, i: usize, level: Level) {
        if self.photos[i].image_errors.contains_key(&level) {
            return;
        }
        let cached = match level {
            Level::Full => self.full.items.contains_key(&i),
            Level::Fit(edge) => {
                self.full.items.contains_key(&i)
                    || (self.fit.items.contains_key(&i) && self.photos[i].fit_edge_loaded >= edge)
            }
            Level::Thumb => self.thumbs.items.contains_key(&i),
        };
        if !cached {
            self.pool.request(i, level);
        }
    }
    fn retry_current(&mut self) {
        self.photos[self.current].image_errors.clear();
        self.photos[self.current].metadata_error = None;
        self.photos[self.current].loaded = false;
        self.pool.retry_metadata(self.current);
        self.pool.cancel(self.current);
        self.pool.navigate(self.current);
        self.prefetched.clear();
        self.error = None;
    }
    fn preview_for(&self, i: usize) -> Option<(Arc<crate::exposure_preview::Preview>, f32)> {
        let p = &self.photos[i];
        p.exposure_base?;
        let delta = p.values.exposure?;
        let preview = self.exposure_preview.as_ref()?.clone();
        (delta != 0).then_some((preview, delta as f32 / 1_000_000.))
    }
    fn finish_close(&mut self, ctx: &egui::Context, sync: bool) {
        if self.mover.as_ref().is_some_and(|m| m.jobs > 0) {
            self.close_after_moves = true;
            self.close_dialog = false;
            self.folder_editor.move_status = "完成移动后自动关闭".into();
            return;
        }
        let result = (|| -> Result<()> {
            if sync || !self.folder_editor.blocked {
                self.folder_editor.persist()?;
            }
            if let Some(s) = &self.session {
                s.record(self.current, &Updates::default())?;
                s.sync_on_exit
                    .store(sync, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.allow_close = true;
                self.close_dialog = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => self.error = Some(format!("会话保存失败，请重试：{e:#}")),
        }
    }
    fn activate_compare(&mut self, side: usize) {
        let c = self.compare.as_mut().unwrap();
        if c.active == side && self.current == c.indices[side] {
            return;
        }
        c.active = side;
        self.current = c.indices[side];
        self.navigation_at = Instant::now();
        self.zoom = c.views[side].zoom > 0.;
        self.pool.navigate(self.current);
        self.prefetched.clear();
    }
    fn adjust_exposure(&mut self, thirds: i32) {
        let p = &self.photos[self.current];
        let Some(base) = p.exposure_base else {
            return;
        };
        let delta = p.exposure_delta + thirds as f64 / 3.;
        let target =
            ((base as f64 / 1_000_000. + delta).clamp(-5., 5.) * 1_000_000.).round() as i32;
        self.save(Updates {
            exposure: Some(target),
            ..Default::default()
        });
        if self.photos[self.current].values.exposure == Some(target) {
            self.photos[self.current].exposure_delta = if target.abs() == 5_000_000 {
                (target - base) as f64 / 1_000_000.
            } else {
                delta
            };
        }
    }
    fn compare_canvas(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.compare_ppp = ctx.pixels_per_point();
        ui.columns(2, |columns| {
            for (side, ui) in columns.iter_mut().enumerate() {
                let i = self.compare.as_ref().unwrap().indices[side];
                let active = self.compare.as_ref().unwrap().active == side;
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(
                            active,
                            if side == 0 {
                                "A · 参考"
                            } else {
                                "B · 候选"
                            },
                        )
                        .on_hover_text("点击选中当前操作对象；滚轮缩放，拖动平移；双击适配 / 100%")
                        .clicked()
                    {
                        self.activate_compare(side);
                    }
                    ui.add(
                        egui::Label::new(
                            self.paths[i]
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                        )
                        .truncate(),
                    );
                });
                ui.horizontal(|ui| {
                    for (label, zoom) in [("适配", 0.), ("100%", 1.)] {
                        let selected =
                            (self.compare.as_ref().unwrap().views[side].zoom - zoom).abs() < 0.0001;
                        if ui
                            .add(egui::Button::new(label).small().selected(selected))
                            .clicked()
                        {
                            self.activate_compare(side);
                            self.zoom_compare(side, zoom);
                        }
                    }
                    let zoom = self.compare.as_ref().unwrap().views[side].zoom;
                    if zoom > 0. && (zoom - 1.).abs() > 0.0001 {
                        ui.label(format!("{:.0}%", zoom * 100.));
                    }
                });
                let (area, response) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
                self.compare_sizes[side] = area.size();
                let painter = ui.painter_at(area);
                painter.rect_filled(area, 0., BG);
                self.request(i, Level::Fit(self.fit_edge));
                let mut view = self.compare.as_ref().unwrap().views[side];
                if view.zoom > 0. {
                    self.request(i, Level::Full);
                }
                let full = self.full.get(i);
                let loaded_full = full.is_some();
                let native = self.native_size(i);
                let source = self.source_size(i, view.zoom > 0.);
                let texture = full
                    .or_else(|| self.fit.get(i))
                    .or_else(|| self.thumbs.get(i))
                    .filter(|_| view.zoom <= 0. || native.is_some());
                if let Some(t) = texture {
                    let size = self.displayed_size(i, source.unwrap_or_else(|| t.size_vec2()));
                    let native_size = size;
                    let fit_zoom = (area.width() / native_size.x)
                        .min(area.height() / native_size.y)
                        * ctx.pixels_per_point();
                    let wheel = if response.hovered() {
                        ctx.input(|input| input.smooth_scroll_delta.y)
                    } else {
                        0.
                    };
                    if response.clicked() || response.drag_started() || wheel != 0. {
                        self.activate_compare(side);
                    }
                    if response.double_clicked() {
                        self.zoom_compare(side, if view.zoom > 0. { 0. } else { 1. });
                        if view.zoom == 0.
                            && let Some(pos) = response.interact_pointer_pos()
                        {
                            let screen = Rect::from_center_size(
                                area.center(),
                                native_size * fit_zoom / ctx.pixels_per_point(),
                            );
                            if screen.contains(pos) {
                                let anchor = (screen.center() - pos) / screen.size();
                                let c = self.compare.as_mut().unwrap();
                                let delta = anchor - c.views[side].pan;
                                c.views[side].pan = anchor;
                                if c.linked {
                                    c.views[1 - side].pan += delta;
                                }
                            }
                        }
                    }
                    if wheel != 0. {
                        let target = if native.is_some() {
                            (if view.zoom > 0. { view.zoom } else { fit_zoom }
                                * (wheel * 0.002).exp())
                            .clamp(0.02, 8.)
                        } else {
                            1.
                        };
                        self.zoom_compare(side, target);
                    }
                    if native.is_some() && response.dragged() {
                        self.move_view(response.drag_delta(), ctx.pixels_per_point());
                    }
                    if response.double_clicked() || response.dragged() || wheel != 0. {
                        ctx.request_repaint();
                    }
                    view = self.compare.as_ref().unwrap().views[side];
                    let actual_zoom = view.zoom;
                    paint_adjusted(
                        &painter,
                        &t,
                        area,
                        self.photos[i].orientation(),
                        actual_zoom,
                        view.pan * native_size * actual_zoom / ctx.pixels_per_point(),
                        ctx.pixels_per_point(),
                        self.source_size(i, actual_zoom > 0.),
                        self.preview_for(i),
                        self.photos[i].values.crop,
                    );
                    if view.zoom > 0. && !loaded_full {
                        painter.text(
                            area.left_top() + Vec2::splat(12.),
                            egui::Align2::LEFT_TOP,
                            "正在加载大图…",
                            egui::FontId::proportional(14.),
                            Color32::WHITE,
                        );
                    }
                } else {
                    let error = self.photos[i].image_errors.get(&Level::Fit(self.fit_edge));
                    painter.text(
                        area.center(),
                        egui::Align2::CENTER_CENTER,
                        error.map_or("正在加载预览…", String::as_str),
                        egui::FontId::proportional(16.),
                        Color32::WHITE,
                    );
                }
                if self.compare.as_ref().unwrap().active == side {
                    painter.rect_stroke(
                        area,
                        0.,
                        Stroke::new(2., Color32::LIGHT_BLUE),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        });
    }
    fn enter_compare(&mut self) {
        if self.visible.len() < 2 {
            return;
        }
        if self.compare.is_none() {
            let other = self
                .visible
                .iter()
                .copied()
                .find(|i| *i > self.current)
                .or_else(|| self.visible.iter().copied().find(|i| *i != self.current))
                .unwrap();
            self.compare = Some(Compare {
                indices: [self.current, other],
                views: [View::default(); 2],
                active: 1,
                linked: true,
            });
            self.go(other);
            self.zoom = false;
        }
        self.grid = false;
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        // Keep the photo canvas usable when the sidebar leaves a narrow viewport.
        if ui.available_width() < 600. {
            ui.spacing_mut().button_padding = Vec2::new(4., 2.);
            ui.spacing_mut().item_spacing = Vec2::new(4., 4.);
            ui.spacing_mut().interact_size.y = 20.;
        }
        // Separate selection controls from viewing/develop controls. Each row
        // allocates only its own height and wraps naturally on narrow windows.
        let enabled = !self.visible.is_empty()
            && self.photos[self.current].loaded
            && !self.moving(self.current);
        ui.horizontal_wrapped(|ui| {
            for (label, value, hint) in [
                ("拾取", "pick", "拾取（P）"),
                ("拒绝", "reject", "拒绝（X）"),
                ("未标记", "", "清除旗标（U）"),
            ] {
                let selected = self.photos[self.current]
                    .values
                    .flag
                    .as_deref()
                    .unwrap_or("")
                    == value;
                if ui
                    .add_enabled(enabled, egui::Button::new(label).selected(selected))
                    .on_hover_text(hint)
                    .clicked()
                {
                    self.flag(value);
                }
            }
            ui.separator();
            control_group(ui, 126., |ui| {
                let rating = self.photos[self.current].values.rating.unwrap_or(0);
                for r in 1..=5 {
                    if ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(if r <= rating { "★" } else { "☆" }).frame(false),
                        )
                        .on_hover_text(format!("{r} 星（{r}），再次点击清星"))
                        .clicked()
                    {
                        self.rating(if r == rating { 0 } else { r });
                    }
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("0").small())
                    .on_hover_text("清除星级（0）")
                    .clicked()
                {
                    self.rating(0);
                }
            });
            ui.separator();
            control_group(ui, 120., |ui| {
                for ((label, color), name) in COLORS.into_iter().zip([
                    "红色（6）",
                    "黄色（7）",
                    "绿色（8）",
                    "蓝色（9）",
                    "紫色",
                ]) {
                    let selected = self.photos[self.current]
                        .values
                        .label
                        .as_deref()
                        .is_some_and(|v| v.eq_ignore_ascii_case(label));
                    if ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(if selected { "■" } else { " " })
                                .fill(color)
                                .min_size(Vec2::splat(18.)),
                        )
                        .on_hover_text(name)
                        .clicked()
                    {
                        self.label(label);
                    }
                }
            });
            ui.separator();
            ui.menu_button("更多", |ui| {
                if ui
                    .add_enabled(enabled, egui::Button::new("向左旋转  ["))
                    .clicked()
                {
                    self.rotate(-1);
                    ui.close();
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("向右旋转  ]"))
                    .clicked()
                {
                    self.rotate(1);
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        !self.visible.is_empty(),
                        egui::Button::new("重试当前照片  R"),
                    )
                    .clicked()
                {
                    self.retry_current();
                    ui.close();
                }
                if ui.button("打开日志  L").clicked() {
                    let _ = fastcull::platform::open_directory(&crate::logger::directory());
                    ui.close();
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(!self.grid && self.compare.is_none(), "单图 E")
                .on_hover_text("单图浏览（E）")
                .clicked()
            {
                self.grid = false;
                self.compare = None;
            }
            if ui
                .selectable_label(self.grid, "网格 G")
                .on_hover_text("网格浏览（G）")
                .clicked()
            {
                self.grid = true;
                self.grid_scroll = true;
            }
            if ui
                .add_enabled(
                    self.visible.len() >= 2,
                    egui::Button::new("对比 C").selected(!self.grid && self.compare.is_some()),
                )
                .on_hover_text("选择当前照片和另一张进行对比（C）")
                .clicked()
            {
                self.enter_compare();
            }
            ui.separator();
            if ui
                .add_enabled(!self.visible.is_empty(), egui::Button::new("◀"))
                .on_hover_text("上一张（←）")
                .clicked()
            {
                self.next(-1);
            }
            if ui
                .add_enabled(!self.visible.is_empty(), egui::Button::new("▶"))
                .on_hover_text("下一张（→）")
                .clicked()
            {
                self.next(1);
            }
            if !self.grid {
                if let Some(c) = &mut self.compare {
                    ui.checkbox(&mut c.linked, "联动").on_hover_text(
                        "保留各自对准的位置，按相同比例缩放和移动；取消后可分别对准主体",
                    );
                } else if ui
                    .button(if self.zoom { "100%" } else { "适配" })
                    .on_hover_text("适配 / 100%（F）")
                    .clicked()
                {
                    self.set_zoom(!self.zoom);
                }
            }
            ui.separator();
            control_group(ui, 200., |ui| {
                ui.label("曝光");
                let baseline = self.photos[self.current].exposure_base;
                let available = baseline.is_some() && enabled;
                for (label, thirds) in [("−1", -3), ("−⅓", -1), ("+⅓", 1), ("+1", 3)] {
                    if ui
                        .add_enabled(available, egui::Button::new(label).small())
                        .on_hover_text("曝光快调；JPEG 为近似预览，写回 LR 请同步选片会话")
                        .clicked()
                    {
                        self.adjust_exposure(thirds);
                    }
                }
                if self.visible.is_empty() {
                    ui.weak("—");
                } else if let Some(e) = self.photos[self.current]
                    .values
                    .exposure
                    .or(baseline)
                    .filter(|_| baseline.is_some())
                {
                    ui.monospace(format!("{:+.2}", e as f64 / 1_000_000.))
                        .on_hover_text("曝光值（EV）");
                } else {
                    ui.weak("等待参数").on_hover_text(
                        "等待 LR 的有效曝光参数；旧处理版本或未解析的自动曝光可能不可用",
                    );
                }
            });
        });
    }
    fn cell(&mut self, ui: &mut egui::Ui, i: usize, size: Vec2, film: bool) {
        self.request(i, Level::Thumb);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(
            rect,
            5.,
            if i == self.current || self.folder_editor.photos.contains(&i) {
                Color32::from_rgb(43, 53, 67)
            } else {
                PANEL
            },
        );
        if i == self.current {
            painter.rect_stroke(
                rect.shrink(1.),
                5.,
                Stroke::new(1.5, Color32::from_rgb(151, 190, 240)),
                egui::StrokeKind::Inside,
            );
        }
        let image_rect = Rect::from_min_max(
            rect.min + Vec2::splat(5.),
            Pos2::new(rect.max.x - 5., rect.max.y - if film { 18. } else { 48. }),
        );
        if let Some(t) = self.thumbs.get(i) {
            paint_image(
                &painter,
                &t,
                image_rect,
                self.photos[i].orientation(),
                self.photos[i].values.crop,
                // The thumbnail's image plane must stay stable as full images
                // arrive/leave the cache (their aspect ratios may differ).
                None,
            );
        } else {
            painter.text(
                image_rect.center(),
                egui::Align2::CENTER_CENTER,
                if self.photos[i].image_errors.contains_key(&Level::Thumb) {
                    "错误"
                } else {
                    "…"
                },
                egui::FontId::proportional(13.),
                Color32::GRAY,
            );
        }
        if self.photos[i].values.flag.as_deref() == Some("reject") {
            painter.line_segment(
                [image_rect.left_top(), image_rect.right_bottom()],
                Stroke::new(2., RED),
            );
            painter.line_segment(
                [image_rect.right_top(), image_rect.left_bottom()],
                Stroke::new(2., RED),
            );
        }
        let photo = &self.photos[i];
        let flag = match photo.values.flag.as_deref() {
            Some("pick") => "P ",
            Some("reject") => "X ",
            _ => "",
        };
        let stars = "★".repeat(photo.values.rating.unwrap_or(0) as usize);
        painter.text(
            Pos2::new(rect.min.x + 5., rect.max.y - if film { 14. } else { 39. }),
            egui::Align2::LEFT_TOP,
            format!("{flag}{stars}"),
            egui::FontId::proportional(11.),
            Color32::LIGHT_GRAY,
        );
        if let Some((_, color)) = COLORS.iter().find(|(l, _)| {
            photo
                .values
                .label
                .as_deref()
                .is_some_and(|v| v.eq_ignore_ascii_case(l))
        }) {
            painter.rect_filled(
                Rect::from_min_size(
                    Pos2::new(rect.max.x - 10., rect.max.y - 13.),
                    Vec2::splat(6.),
                ),
                0.,
                *color,
            );
        }
        if !film {
            let name = self.paths[i]
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            painter.text(
                Pos2::new(rect.min.x + 5., rect.max.y - 20.),
                egui::Align2::LEFT_TOP,
                name,
                egui::FontId::proportional(12.),
                Color32::LIGHT_GRAY,
            );
        }
        if response.clicked() {
            self.folder_editor
                .select(i, &self.visible, ui.input(|input| input.modifiers));
            self.go(i);
        }
        if response.drag_started() {
            if !self.folder_editor.photos.contains(&i) {
                self.folder_editor
                    .select(i, &self.visible, egui::Modifiers::NONE);
                self.go(i);
            }
            response.dnd_set_drag_payload(self.folder_editor.selection(i, &self.visible));
        }
        if response.double_clicked() {
            self.go(i);
            self.grid = false;
            self.compare = None;
        }
        if self.moving(i) {
            painter.text(
                rect.center_top() + Vec2::new(0., 5.),
                egui::Align2::CENTER_TOP,
                "移动中",
                egui::FontId::proportional(12.),
                Color32::LIGHT_BLUE,
            );
        }
    }
    fn film_panel(&mut self, ctx: &egui::Context) -> Rect {
        let initial_height = self
            .smoke
            .as_ref()
            .and_then(|_| std::env::var("FASTCULL_SMOKE_FILM_HEIGHT").ok())
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(94.);
        egui::TopBottomPanel::bottom("film")
            .resizable(true)
            .default_height(initial_height)
            .height_range(76. ..=(ctx.content_rect().height() * 0.35).clamp(94., 260.))
            .show(ctx, |ui| self.film(ui))
            .response
            .rect
    }
    fn film(&mut self, ui: &mut egui::Ui) {
        let active = ui.rect_contains_pointer(ui.max_rect())
            && !self.close_dialog
            && !self.folder_editor.editing(ui.ctx())
            && !egui::Popup::is_any_open(ui.ctx())
            && !ui.input(|i| i.pointer.any_down());
        if active {
            let delta = ui.input_mut(|input| {
                let mut steps = 0.;
                input.events.retain(|event| {
                    if let egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                    } = event
                    {
                        if modifiers.ctrl || modifiers.command || modifiers.alt {
                            return true;
                        }
                        let amount = if delta.y != 0. { delta.y } else { delta.x };
                        steps -= amount
                            / match unit {
                                egui::MouseWheelUnit::Point => 40.,
                                egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => 1.,
                            };
                        return false;
                    }
                    true
                });
                if steps != 0. {
                    input.raw_scroll_delta = Vec2::ZERO;
                    input.smooth_scroll_delta = Vec2::ZERO;
                }
                steps
            });
            if delta != 0. && self.film_wheel.signum() != delta.signum() {
                self.film_wheel = 0.;
            }
            self.film_wheel += delta;
            let steps = self.film_wheel.trunc().clamp(-8., 8.) as isize;
            if steps != 0 {
                self.film_wheel = self.film_wheel.fract();
                for _ in 0..steps.unsigned_abs() {
                    self.next(steps.signum());
                }
            }
        } else {
            self.film_wheel = 0.;
        }
        let height = ui.available_height().clamp(64., 248.);
        let width = (height * 1.15)
            .max(76.)
            .min((ui.available_width() - 64.).max(40.));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.;
            if ui
                .add_sized([24., height], egui::Button::new("◀").frame(false))
                .clicked()
            {
                self.next(-4);
            }
            let count = ((ui.available_width() - 28.) / (width + 4.))
                .floor()
                .max(1.) as usize;
            let position = self
                .visible
                .iter()
                .position(|i| *i == self.current)
                .unwrap_or(0);
            let start = position
                .saturating_sub(count / 2)
                .min(self.visible.len().saturating_sub(count));
            for position in start..(start + count).min(self.visible.len()) {
                let i = self.visible[position];
                self.cell(ui, i, Vec2::new(width, height), true);
            }
            if ui
                .add_sized([24., height], egui::Button::new("▶").frame(false))
                .clicked()
            {
                self.next(4);
            }
        });
    }
    fn zoom_at(&mut self, screen: Rect, pos: Option<Pos2>) {
        let anchor = pos
            .filter(|p| screen.contains(*p))
            .map(|p| (screen.center() - p) / screen.size());
        let entering = !self.zoom;
        self.set_zoom(!self.zoom);
        if entering && let Some(anchor) = anchor {
            self.pan = anchor;
        }
    }
    fn native_size(&self, i: usize) -> Option<Vec2> {
        self.full
            .items
            .get(&i)
            .map(|t| t.handle.size_vec2())
            .or(self.photos[i].full_size)
    }
    fn source_size(&self, i: usize, zoomed: bool) -> Option<Vec2> {
        if zoomed {
            // At 100%, actual decoded pixels remain authoritative.
            return self.native_size(i);
        }
        self.photos[i]
            .info
            .as_ref()
            .and_then(|info| info.image_size)
            .map(|(w, h)| Vec2::new(w as f32, h as f32))
            .or_else(|| self.native_size(i))
    }
    fn displayed_size(&self, i: usize, size: Vec2) -> Vec2 {
        self.photos[i]
            .values
            .crop
            .unwrap_or_default()
            .geometry(size, self.photos[i].orientation())
            .rect
            .size()
    }
    fn begin_crop(&mut self) {
        if self.visible.is_empty() || !self.photos[self.current].loaded || self.moving(self.current)
        {
            return;
        }
        if self.develop_pending(self.current) {
            self.crop_requested = true;
            return;
        }
        if let Some(size) = self.native_size(self.current) {
            let geometry = self.photos[self.current]
                .values
                .crop
                .unwrap_or_default()
                .geometry(size, self.photos[self.current].orientation());
            if geometry.rect.width() <= 0. || geometry.rect.height() <= 0. {
                self.error =
                    Some("现有裁切坐标无效，已保留原 XMP。请先在 Camera Raw 中检查。".into());
                self.crop_requested = false;
                return;
            }
            self.crop_editor = Some(crate::crop_ui::Editor::new(
                self.photos[self.current].values.crop.unwrap_or_default(),
                size,
                self.photos[self.current].orientation(),
            ));
            self.crop_requested = false;
            self.close_dialog = false;
            self.grid = false;
            self.compare = None;
            self.zoom = false;
            self.pan = Vec2::ZERO;
        } else {
            self.crop_requested = true;
            self.request(self.current, Level::Full);
        }
    }
    fn apply_crop(&mut self) {
        let Some(editor) = &self.crop_editor else {
            return;
        };
        let value = editor.value();
        let full = fastcull::crop::Geometry::full(editor.display());
        let reset = editor.geometry.angle == 0. && editor.geometry.rect == full.rect;
        self.save(Updates {
            crop: Some(if reset { Default::default() } else { value }),
            orientation: Some(editor.orientation),
            ..Default::default()
        });
        if self.error.is_none() {
            self.crop_editor = None;
            self.pan = Vec2::ZERO;
        }
    }
    fn crop_workspace(&mut self, ctx: &egui::Context) {
        use crate::crop_ui::Action;
        let mut editor = self.crop_editor.take().unwrap();
        let mut action = Action::None;
        egui::TopBottomPanel::top("crop-title").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("裁切照片");
                ui.label(
                    self.paths[self.current]
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                );
                ui.weak("应用后保存到 XMP，可随时重新裁切");
            });
            if let Some(e) = &self.error {
                ui.colored_label(RED, e);
            }
        });
        egui::SidePanel::right("crop-controls")
            .exact_width(280.)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    action = editor.controls(ui);
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(16.))
            .show(ctx, |ui| {
                let (area, response) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
                self.fit_edge = preview_edge(area.size(), ctx.pixels_per_point());
                self.request(self.current, Level::Fit(self.fit_edge));
                if let Some(t) = self
                    .full
                    .get(self.current)
                    .or_else(|| self.fit.get(self.current))
                {
                    let display = editor.display();
                    let (s, c) = editor.geometry.angle.to_radians().sin_cos();
                    let bounds = Vec2::new(
                        c.abs() * display.x + s.abs() * display.y,
                        s.abs() * display.x + c.abs() * display.y,
                    );
                    let scale = (area.width() / bounds.x).min(area.height() / bounds.y) * 0.94;
                    let rect = Rect::from_center_size(area.center(), display * scale);
                    let g = fastcull::crop::Geometry {
                        rect: Rect::from_center_size((display * 0.5).to_pos2(), bounds),
                        angle: editor.geometry.angle,
                    };
                    let backdrop = Rect::from_center_size(area.center(), bounds * scale);
                    let uv = g.uvs(editor.size, editor.orientation);
                    let painter = ui.painter_at(area);
                    if let Some(preview) = &self.exposure_preview {
                        let delta = self.preview_for(self.current).map_or(0., |(_, d)| d);
                        preview.paint(&painter, area, backdrop, t.id(), uv, delta);
                    } else {
                        paint_uv(&painter, &t, backdrop, uv);
                    }
                    editor.overlay(ui, &response, rect);
                }
            });
        self.crop_editor = Some(editor);
        match action {
            Action::Apply => self.apply_crop(),
            Action::Cancel => self.crop_editor = None,
            Action::None => {}
        }
    }
    fn central(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.compare.is_some() && !self.visible.is_empty() && !self.grid {
            self.compare_canvas(ui, ctx);
            return;
        }
        if self.visible.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("没有符合当前条件的照片。可以清除或调整筛选条件。");
            });
            return;
        }
        if self.grid {
            let cols = 6;
            let width = (ui.available_width() / cols as f32 - 4.).max(32.);
            let mut area = egui::ScrollArea::vertical().id_salt("grid");
            if self.grid_scroll {
                let position = self
                    .visible
                    .iter()
                    .position(|i| *i == self.current)
                    .unwrap_or(0);
                area = area.vertical_scroll_offset((position / cols) as f32 * 224.);
                self.grid_scroll = false;
            }
            area.show_rows(ui, 220., self.visible.len().div_ceil(cols), |ui, rows| {
                for row in rows {
                    ui.horizontal(|ui| {
                        for position in row * cols..((row + 1) * cols).min(self.visible.len()) {
                            let i = self.visible[position];
                            self.cell(ui, i, Vec2::new(width, 220.), false);
                        }
                    });
                }
            });
            return;
        }
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        self.canvas_size = rect.size();
        self.fit_edge = preview_edge(rect.size(), ctx.pixels_per_point());
        self.request(self.current, Level::Fit(self.fit_edge));
        if self.zoom {
            self.request(self.current, Level::Full);
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0., BG);
        let full = self.full.get(self.current);
        let actual_full = full.is_some();
        let native = self.source_size(self.current, self.zoom);
        let texture = full
            .or_else(|| self.fit.get(self.current))
            .or_else(|| self.thumbs.get(self.current))
            .filter(|_| !self.zoom || native.is_some());
        if let Some(t) = texture {
            if self.zoom && response.dragged() {
                self.move_view(response.drag_delta(), ctx.pixels_per_point());
            }
            let shown = self.displayed_size(self.current, native.unwrap_or_else(|| t.size_vec2()))
                / ctx.pixels_per_point();
            let screen = paint_adjusted(
                &painter,
                &t,
                rect,
                self.photos[self.current].orientation(),
                if self.zoom { 1. } else { 0. },
                self.pan * shown,
                ctx.pixels_per_point(),
                native,
                self.preview_for(self.current),
                self.photos[self.current].values.crop,
            );
            if !self.first_draw {
                log::info!(
                    "first loupe {:.1}ms canvas={}x{} image={}x{}",
                    self.started.elapsed().as_secs_f64() * 1000.,
                    rect.width(),
                    rect.height(),
                    screen.width(),
                    screen.height()
                );
                self.first_draw = rect.width() > 100.
                    && rect.height() > 100.
                    && screen.width() > 100.
                    && screen.height() > 100.;
            }
            if response.clicked() {
                self.zoom_at(screen, response.interact_pointer_pos());
            }
            if self.zoom
                && actual_full
                && let Some(start) = self.zoom_started.take()
            {
                log::info!(
                    "100% ready image={} latency_ms={:.1} texture={}x{}",
                    self.current,
                    start.elapsed().as_secs_f64() * 1000.,
                    t.size()[0],
                    t.size()[1]
                );
            }
        } else if let Some(e) = self.photos[self.current]
            .image_errors
            .get(&Level::Fit(self.fit_edge))
            .cloned()
        {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("预览加载失败：{e}\n按 R 重试，或按右箭头继续选片"),
                egui::FontId::proportional(16.),
                RED,
            );
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "正在加载内嵌 JPEG…",
                egui::FontId::proportional(16.),
                Color32::LIGHT_GRAY,
            );
        }
        if (self.zoom || self.crop_requested) && !actual_full {
            let message = self.photos[self.current]
                .image_errors
                .get(&Level::Full)
                .map_or(
                    if self.crop_requested {
                        "正在准备裁切…"
                    } else {
                        "正在加载 100% 清晰图…"
                    }
                    .to_owned(),
                    |e| format!("100% 加载失败：{e}；按 R 重试"),
                );
            painter.text(
                rect.left_top() + Vec2::splat(12.),
                egui::Align2::LEFT_TOP,
                message,
                egui::FontId::proportional(14.),
                Color32::YELLOW,
            );
        }
        if !actual_full
            && !self.fit.items.contains_key(&self.current)
            && let Some(e) = self.photos[self.current]
                .image_errors
                .get(&Level::Fit(self.fit_edge))
        {
            painter.text(
                rect.left_bottom() - Vec2::new(-12., 24.),
                egui::Align2::LEFT_BOTTOM,
                format!("适屏预览失败：{e}；按 R 重试"),
                egui::FontId::proportional(14.),
                Color32::YELLOW,
            );
        }
    }
    fn smoke(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            if let egui::Event::Screenshot { image, .. } = ev
                && let Some(s) = &mut self.smoke
            {
                let path = s.dir.join(format!("{:02}.png", s.step));
                let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                if let Err(e) = save_png(&path, image.size, &bytes) {
                    log::error!("screenshot: {e}");
                }
                s.step += 1;
                s.waiting = false;
                s.prepared = false;
                s.last = Instant::now();
            }
        }
        let Some(s) = &self.smoke else {
            return;
        };
        if std::env::var_os("FASTCULL_CROP_SMOKE").is_some() {
            let step = s.step;
            let waiting = s.waiting;
            let prepared = s.prepared;
            let elapsed = s.last.elapsed();
            if self.started.elapsed().as_secs() > 60 {
                panic!("crop smoke timeout at {step}");
            }
            if step >= 8 {
                assert!(self.error.is_none(), "{:?}", self.error);
                std::fs::write(
                    s.dir.join("crop-ok.txt"),
                    "Crop editor, rotation, XMP roundtrip and preview passed",
                )
                .unwrap();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if waiting || elapsed.as_millis() < 500 {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            if self.native_size(self.current).is_none() || !self.photos[self.current].loaded {
                self.request(self.current, Level::Full);
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            if !prepared {
                match step {
                    0 => {}
                    1 => self.begin_crop(),
                    2 => {
                        let e = self.crop_editor.as_mut().unwrap();
                        e.select_ratio("16 : 9".into(), Some(16. / 9.));
                        e.geometry.angle = 12.;
                        e.geometry.constrain(e.display());
                    }
                    3 => {
                        let e = self.crop_editor.as_mut().unwrap();
                        e.rotate(crate::crop_ui::clockwise(e.orientation));
                    }
                    4 => {
                        self.apply_crop();
                        assert!(self.crop_editor.is_none(), "{:?}", self.error);
                        assert_eq!(
                            fastcull::xmp::read(&self.paths[self.current]).unwrap().crop,
                            self.photos[self.current].values.crop
                        );
                    }
                    5 => self.begin_crop(),
                    6 => {
                        self.crop_editor = None;
                        self.grid = true;
                    }
                    7 => {
                        self.grid = false;
                        self.set_zoom(true);
                    }
                    _ => unreachable!(),
                }
                let s = self.smoke.as_mut().unwrap();
                s.prepared = true;
                s.last = Instant::now();
                ctx.request_repaint();
                return;
            }
            self.smoke.as_mut().unwrap().waiting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            ctx.request_repaint();
            return;
        }
        if self.started.elapsed().as_secs() > 60 {
            log::error!("smoke timeout");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if s.step == 5 && self.stress_remaining > 0 {
            self.stress_remaining -= 1;
            self.zoom = self.stress_remaining > 90;
            self.go((self.stress_remaining * 37) % self.paths.len());
            self.grid = self.stress_remaining.is_multiple_of(9);
            log::info!(
                "stress remaining={} current={}",
                self.stress_remaining,
                self.current
            );
            ctx.request_repaint();
            return;
        }
        if s.waiting
            || s.last.elapsed().as_millis() < 500
            || !(if self.zoom {
                self.full.items.contains_key(&self.current)
            } else {
                self.fit.items.contains_key(&self.current)
                    || self.full.items.contains_key(&self.current)
            })
            || !(self.photos[self.current].loaded
                || self.photos[self.current].metadata_error.is_some())
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        let step = s.step;
        let prepared = s.prepared;
        let features = std::env::var_os("FASTCULL_FEATURE_SMOKE").is_some();
        let folders = std::env::var_os("FASTCULL_FOLDER_SMOKE").is_some();
        let bridge_moves = std::env::var_os("FASTCULL_BRIDGE_MOVE_SMOKE").is_some();
        if bridge_moves && step >= 14 && self.mover.as_ref().is_some_and(|m| m.jobs > 0) {
            ctx.request_repaint_after(std::time::Duration::from_millis(40));
            return;
        }
        if features && prepared {
            let ready = self.compare.as_ref().is_none_or(|c| {
                c.indices.iter().enumerate().all(|(side, i)| {
                    if c.views[side].zoom > 0. {
                        self.full.items.contains_key(i)
                    } else {
                        self.fit.items.contains_key(i) || self.full.items.contains_key(i)
                    }
                })
            });
            if !ready || (step == 10 && self.photos.iter().any(|p| !p.capture_indexed)) {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            if step == 9 && !self.smoke.as_ref().unwrap().relative_link_tested {
                let c = self.compare.as_ref().unwrap();
                let offset = c.views[0].pan - c.views[1].pan;
                let ratio = c.views[0].zoom / c.views[1].zoom;
                self.zoom_compare(1, 1.25);
                self.move_view(Vec2::new(40., 20.), ctx.pixels_per_point());
                let c = self.compare.as_ref().unwrap();
                assert!((c.views[0].pan - c.views[1].pan - offset).length() < 0.00001);
                assert!((c.views[0].zoom / c.views[1].zoom - ratio).abs() < 0.00001);
                let views = c.views;
                self.compare.as_mut().unwrap().linked = false;
                self.move_view(Vec2::new(-20., 0.), ctx.pixels_per_point());
                assert_eq!(self.compare.as_ref().unwrap().views[0].pan, views[0].pan);
                self.compare.as_mut().unwrap().views = views;
                self.compare.as_mut().unwrap().linked = true;
                let s = self.smoke.as_mut().unwrap();
                std::fs::write(s.dir.join("relative-link.json"), format!(
                    "{{\"verified\":true,\"a\":{{\"zoom\":{},\"pan\":[{},{}]}},\"b\":{{\"zoom\":{},\"pan\":[{},{}]}},\"offset\":[{},{}]}}",
                    views[0].zoom, views[0].pan.x, views[0].pan.y,
                    views[1].zoom, views[1].pan.x, views[1].pan.y, offset.x, offset.y,
                )).unwrap();
                s.relative_link_tested = true;
                ctx.request_repaint();
                return;
            }
        }
        if step == 1 && self.zoom && self.zoom_started.is_none() && !s.cached_zoom_tested {
            self.set_zoom(false);
            self.set_zoom(true);
            self.smoke.as_mut().unwrap().cached_zoom_tested = true;
            ctx.request_repaint();
            return;
        }
        if step
            >= if bridge_moves {
                21
            } else if folders {
                19
            } else if features {
                13
            } else {
                5
            }
        {
            let dir = s.dir.clone();
            if bridge_moves {
                std::fs::write(
                    dir.join("moved-paths.txt"),
                    self.paths
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            format!("{i}\t{}\n", fastcull::handoff::encode(&p.to_string_lossy()))
                        })
                        .collect::<String>(),
                )
                .unwrap();
                assert!(self.error.is_none(), "{:?}", self.error);
                assert!(self.folder_editor.plan.moves.is_empty());
            }
            let _ = std::fs::write(
                dir.join("result.txt"),
                format!(
                    "first_draw={} current={} photos={} full={} fit={} thumbs={} error={:?} stress_remaining={} elapsed_ms={:.0}",
                    self.first_draw,
                    self.current,
                    self.paths.len(),
                    self.full.items.len(),
                    self.fit.items.len(),
                    self.thumbs.items.len(),
                    self.error,
                    self.stress_remaining,
                    self.started.elapsed().as_secs_f64() * 1000.
                ),
            );
            if features {
                self.finish_close(ctx, self.session.is_some());
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if !prepared {
            match step {
                1 => {
                    self.set_zoom(true);
                    self.pan = Vec2::new(0.09, 0.03);
                    if self.paths.len() > 1 {
                        let before = self.current;
                        for (key, expected) in [
                            (Key::ArrowRight, (before + 1).min(self.paths.len() - 1)),
                            (Key::ArrowLeft, before),
                        ] {
                            ctx.input_mut(|i| {
                                i.events.push(egui::Event::Key {
                                    key,
                                    physical_key: None,
                                    pressed: true,
                                    repeat: false,
                                    modifiers: egui::Modifiers::NONE,
                                })
                            });
                            self.keyboard(ctx);
                            assert_eq!(self.current, expected, "native 100% arrow navigation");
                            assert!(self.zoom, "navigation must preserve 100% zoom");
                            assert_eq!(
                                self.pan,
                                Vec2::new(0.09, 0.03),
                                "navigation must preserve relative anchor"
                            );
                        }
                        log::info!("native 100% arrow navigation verified");
                    }
                }
                2 => {
                    self.zoom = false;
                    self.grid = true;
                }
                3 => {
                    self.grid = false;
                    self.go(self.paths.len() - 1);
                }
                4 => {
                    self.rating(4);
                    self.label("Purple");
                    self.flag("reject");
                    self.rotate(1);
                }
                5 if features => {
                    self.zoom = false;
                    self.compare = None;
                }
                6 if features => {
                    let p = &self.photos[self.current];
                    let target = p.exposure_base.expect("feature smoke needs baseline") + 1_000_000;
                    self.save(Updates {
                        exposure: Some(target),
                        ..Default::default()
                    });
                }
                7 if features => {
                    let current = self.current;
                    self.compare = Some(Compare {
                        indices: [0, current],
                        views: [View::default(); 2],
                        active: 1,
                        linked: false,
                    });
                    self.grid = false;
                    self.zoom = false;
                }
                8 if features => {
                    let c = self.compare.as_mut().unwrap();
                    c.views[0] = View {
                        zoom: 1.,
                        pan: Vec2::new(0.05, 0.02),
                    };
                }
                9 if features => {
                    let c = self.compare.as_mut().unwrap();
                    c.views[1] = View {
                        zoom: 1.,
                        pan: Vec2::new(-0.06, 0.08),
                    };
                    // Alignment belongs to each photo. Enabling linking leaves it intact.
                    c.linked = true;
                    self.zoom = true;
                }
                10 if features => {
                    self.compare = None;
                    self.zoom = false;
                    self.filter_editor.open = true;
                    self.pool.start_index();
                    self.filters.selected.insert(
                        fastcull::filtering::Field::Rating,
                        [Some("4".into())].into(),
                    );
                    self.filter_dirty = true;
                }
                11 if features => {
                    self.filters.selected.insert(
                        fastcull::filtering::Field::Rating,
                        [Some("5".into())].into(),
                    );
                    self.filter_dirty = true;
                }
                12 if features => {
                    self.close_dialog = true;
                }
                13 if bridge_moves => {
                    assert!(self.folder_editor.direct && self.paths.len() >= 4);
                    self.close_dialog = false;
                    self.filter_editor.clear(&mut self.filters);
                    self.filter_editor.open = false;
                    self.folder_editor.open = true;
                    self.folder_editor.selected = None;
                    self.refilter();
                    self.go(0);
                    let root = self.paths[0].parent().unwrap().parent().unwrap().to_owned();
                    let target = self.folder_editor.create_folder(&root, "精选").unwrap();
                    self.folder_editor.stage(&[0], &target);
                    self.submit_moves();
                    assert_eq!(self.current, 1);
                }
                14 if bridge_moves => {
                    assert_eq!(
                        self.paths[0].parent(),
                        self.folder_editor.last_target.as_deref()
                    );
                    self.folder_editor.repeat_move(self.current, &self.visible);
                    self.submit_moves();
                    assert_eq!(self.current, 2);
                }
                15 if bridge_moves => {
                    let root = self.paths[0].parent().unwrap().parent().unwrap().to_owned();
                    let target = self.folder_editor.create_folder(&root, "批量").unwrap();
                    self.folder_editor.photos = [2, 3].into();
                    self.folder_editor.stage(&[2, 3], &target);
                    self.submit_moves();
                }
                16 if bridge_moves => {
                    self.go(0);
                    self.folder_editor.advance = false;
                    self.compare = Some(Compare {
                        indices: [0, 3],
                        views: [
                            View {
                                zoom: 1.,
                                pan: Vec2::new(0.05, 0.02),
                            },
                            View {
                                zoom: 1.,
                                pan: Vec2::new(-0.06, 0.08),
                            },
                        ],
                        active: 0,
                        linked: true,
                    });
                    let root = self.paths[0].parent().unwrap().parent().unwrap().to_owned();
                    let target = self.folder_editor.create_folder(&root, "再次移动").unwrap();
                    self.folder_editor.stage(&[0], &target);
                    self.submit_moves();
                }
                17 if bridge_moves => {
                    assert_eq!(self.compare.as_ref().unwrap().indices, [0, 3]);
                    assert_eq!(
                        self.compare.as_ref().unwrap().views[0].pan,
                        Vec2::new(0.05, 0.02)
                    );
                    self.rating(5);
                    self.adjust_exposure(1);
                    assert_eq!(fastcull::xmp::read(&self.paths[0]).unwrap().rating, Some(5));
                    assert!(self.paths[0].with_extension("xmp").is_file());
                    self.folder_editor.selected = self.paths[0].parent().map(PathBuf::from);
                    self.refilter();
                    assert_eq!(self.visible, [0]);
                }
                18 if bridge_moves => {
                    self.folder_editor.selected = None;
                    self.refilter();
                    self.folder_editor
                        .move_dialog(vec![2, 3], self.folder_editor.last_target.clone().unwrap());
                }
                19 if bridge_moves => {
                    self.folder_editor.dismiss_dialog();
                    if let Some(target) = std::env::var_os("FASTCULL_MOVE_SMOKE_CROSS") {
                        self.compare = None;
                        self.go(1);
                        self.rating(3);
                        self.go(0);
                        let target = PathBuf::from(target);
                        std::fs::create_dir_all(&target).unwrap();
                        self.folder_editor.stage(&[1], &target);
                        self.submit_moves();
                    }
                }
                20 if bridge_moves => {
                    self.compare = None;
                    self.go(1);
                    self.zoom = false;
                    if let Some(target) = std::env::var_os("FASTCULL_MOVE_SMOKE_CROSS") {
                        assert_eq!(
                            self.paths[1].parent(),
                            Some(PathBuf::from(target).as_path())
                        );
                    }
                    self.close_dialog = true;
                }
                13 if folders => {
                    self.close_dialog = false;
                    self.filter_editor.clear(&mut self.filters);
                    self.folder_editor.open = true;
                    self.folder_editor.selected = None;
                    let root = self.paths[0].parent().unwrap().parent().unwrap().to_owned();
                    let target = self.folder_editor.create_folder(&root, "精选").unwrap();
                    let indices = vec![0, self.paths.len() - 1];
                    self.folder_editor.photos = indices.iter().copied().collect();
                    assert!(self.folder_editor.stage(&indices, &target));
                    self.filter_dirty = true;
                    self.zoom = false;
                }
                14 if folders => {
                    let parent = self.paths[0].parent().unwrap().parent().unwrap().to_owned();
                    self.folder_editor.new_dialog(parent);
                }
                15 if folders => {
                    self.folder_editor.dismiss_dialog();
                    let target = self
                        .folder_editor
                        .plan
                        .moves
                        .values()
                        .next()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .to_owned();
                    self.folder_editor.move_dialog(
                        self.folder_editor.selection(self.current, &self.visible),
                        target,
                    );
                }
                16 if folders => {
                    self.folder_editor.dismiss_dialog();
                    self.folder_editor.pending_dialog();
                }
                17 if folders => {
                    self.folder_editor.dismiss_dialog();
                    self.folder_editor.selected = Some(
                        self.folder_editor
                            .plan
                            .moves
                            .values()
                            .next()
                            .unwrap()
                            .parent()
                            .unwrap()
                            .to_owned(),
                    );
                    self.filter_dirty = true;
                }
                18 if folders => {
                    assert_eq!(self.visible.len(), 2);
                    self.close_dialog = true;
                }
                _ => {}
            }
            let s = self.smoke.as_mut().unwrap();
            s.prepared = true;
            s.last = Instant::now();
            ctx.request_repaint();
            return;
        }
        // Capture the next rendered frame after these state changes.
        if std::env::var_os("FASTCULL_TREE_SMOKE").is_some() && (step == 1 || step == 2) {
            let cursor = ctx.output(|o| o.cursor_icon);
            assert_eq!(
                cursor,
                if step == 1 {
                    egui::CursorIcon::Move
                } else {
                    egui::CursorIcon::NotAllowed
                }
            );
            assert!(self.folder_editor.take_requests().is_empty());
            log::info!("native folder-tree drag step={step} cursor={cursor:?}; no move submitted");
        }
        let s = self.smoke.as_mut().unwrap();
        s.waiting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        ctx.request_repaint();
    }
}
fn control_group(ui: &mut egui::Ui, width: f32, controls: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 4.;
            controls(ui);
        },
    );
}
#[derive(Clone, Copy)]
enum CloseChoice {
    Sync,
    Direct,
    Cancel,
}
fn close_confirmation(
    ctx: &egui::Context,
    session: bool,
    organizing: bool,
    error: Option<&str>,
) -> Option<CloseChoice> {
    let mut choice = None;
    egui::Modal::new(egui::Id::new("close-confirmation"))
        .frame(egui::Frame::popup(&ctx.style()).inner_margin(22.))
        .show(ctx, |ui| {
            ui.set_width(440_f32.min((ctx.content_rect().width() - 88.).max(240.)));
            ui.spacing_mut().item_spacing = Vec2::new(12., 10.);
            ui.label(egui::RichText::new("关闭选片器").size(24.).strong());
            ui.add_space(4.);
            ui.label(egui::RichText::new(if !session {
                "星级、旗标、颜色、曝光、裁切与旋转已保存到照片元数据。"
            } else if organizing {
                "是否同步本次修改及文件整理计划到 Lightroom？"
            } else { "退出后自动同步本次全部修改到 Lightroom。" }).size(18.));
            ui.label(
                egui::RichText::new(if !session { "关闭后修改仍会保留。\n在其他软件中查看时，请刷新或重新读取照片元数据。" }
                    else if organizing { "同步星级、旗标、颜色、曝光、裁切与旋转，并建立待整理分组。\n实际移动请在 LR 文件夹面板完成，保留原照片记录。" }
                    else { "包含星级、旗标、颜色、曝光、裁切与旋转。\n只同步本次修改的字段，保留其他 LR 显影设置。" })
                    .size(14.)
                    .color(Color32::from_gray(185)),
            );
            ui.add_space(10.);
            ui.horizontal_wrapped(|ui| {
                for (text, action, width, enabled) in [
                    (if organizing { "同步并整理后关闭" } else { "同步全部并关闭" }, CloseChoice::Sync, 176., session),
                    (if session { "仅关闭，稍后同步" } else { "直接关闭" }, CloseChoice::Direct, 152., true),
                    ("取消", CloseChoice::Cancel, 88., true),
                ] {
                    if matches!(action, CloseChoice::Sync) && !session { continue; }
                    let button = egui::Button::new(egui::RichText::new(text).size(16.))
                        .min_size(Vec2::new(width, 40.));
                    if ui.add_enabled(enabled, button).clicked() {
                        choice = Some(action);
                    }
                }
            });
            if let Some(e) = error {
                ui.colored_label(RED, egui::RichText::new(e).size(14.));
            }
        });
    choice
}
#[cfg(test)]
fn shortcut(ctx: &egui::Context, key: Key) -> bool {
    ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key))
}
impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        // Native visual regression on private fixtures only; no actual drop.
        if std::env::var_os("FASTCULL_TREE_SMOKE").is_none() {
            return;
        }
        let Some(smoke) = &self.smoke else {
            return;
        };
        let step = smoke.step;
        let Some(parent) = self
            .paths
            .get(if step == 1 { 10 } else { 0 })
            .and_then(|p| p.parent())
        else {
            return;
        };
        let Some(row) = ctx.read_response(egui::Id::new(("folder-row", &parent.to_path_buf())))
        else {
            return;
        };
        if step == 1 || step == 2 {
            if !egui::DragAndDrop::has_payload_of_type::<Vec<usize>>(ctx) {
                egui::DragAndDrop::set_payload(ctx, vec![0usize]);
                input.events.push(egui::Event::PointerButton {
                    pos: row.rect.center(),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            input
                .events
                .push(egui::Event::PointerMoved(row.rect.center()));
        } else {
            egui::DragAndDrop::clear_payload(ctx);
            input.events.push(egui::Event::PointerMoved(if step == 0 {
                row.rect.right_center() + Vec2::new(10., 0.)
            } else {
                egui::pos2(500., 30.)
            }));
            if step == 3 {
                input.events.push(egui::Event::PointerButton {
                    pos: egui::pos2(500., 30.),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(p) = &self.exposure_preview {
            p.destroy();
        }
    }
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.folder_editor.drop_cursor = None;
        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && self.smoke.is_none()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_dialog = true;
        }
        self.full.pinned = self.compare.as_ref().map(|c| c.indices[1 - c.active]);
        self.pool.reference(self.full.pinned);
        self.fit.pinned = self.full.pinned;
        self.startup_focus.update(ctx, frame);
        if !self.first_frame {
            log::info!(
                "first UI frame {:.1}ms",
                self.started.elapsed().as_secs_f64() * 1000.
            );
            self.first_frame = true;
            ctx.request_repaint();
        }
        self.receive_moves(ctx);
        self.receive_metadata();
        self.receive_develop();
        self.receive_index();
        if self.filter_dirty {
            self.refilter();
        }
        self.keyboard(ctx);
        self.receive(ctx);
        if self.crop_requested {
            self.begin_crop();
        }
        if self.crop_editor.is_some() {
            self.crop_workspace(ctx);
            self.smoke(ctx);
            return;
        }
        if self.filter_dirty {
            self.refilter();
        }
        egui::SidePanel::left("activity-rail")
            .exact_width(68.)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(25, 28, 33))
                    .inner_margin(6.),
            )
            .show(ctx, |ui| {
                ui.add_space(8.);
                if navigation_button(
                    ui,
                    "文件夹",
                    self.folder_editor.open,
                    NavigationIcon::Folder,
                )
                .on_hover_text("文件夹树与照片整理")
                .clicked()
                {
                    self.folder_editor.open = !self.folder_editor.open;
                    if self.folder_editor.open {
                        self.filter_editor.open = false;
                    }
                }
                ui.add_space(6.);
                if navigation_button(ui, "筛选", self.filter_editor.open, NavigationIcon::Filter)
                    .on_hover_text("照片筛选 · Ctrl/Cmd+F")
                    .clicked()
                {
                    self.filter_editor.open = !self.filter_editor.open;
                    if self.filter_editor.open {
                        self.folder_editor.open = false;
                    }
                }
                ui.add_space(6.);
                let crop_enabled = !self.visible.is_empty()
                    && self.photos[self.current].loaded
                    && self.photos[self.current].metadata_error.is_none()
                    && !self.moving(self.current);
                if ui
                    .add_enabled_ui(crop_enabled, |ui| {
                        navigation_button(ui, "裁切", self.crop_requested, NavigationIcon::Crop)
                            .on_hover_text("裁切 · T\n自由裁切、锁定比例与旋转")
                    })
                    .inner
                    .clicked()
                {
                    self.begin_crop();
                }
                if self.filters.active() {
                    ui.add_space(8.);
                    ui.colored_label(Color32::LIGHT_BLUE, "已筛选");
                }
            });
        egui::TopBottomPanel::top("hud")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.session.is_none()
                        && ui
                            .button("新窗口")
                            .on_hover_text("打开另一批照片或文件夹")
                            .clicked()
                    {
                        match std::env::current_exe().and_then(|exe| {
                            std::process::Command::new(exe)
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .spawn()
                        }) {
                            Ok(_) => {}
                            Err(e) => self.error = Some(format!("打开新窗口失败：{e}")),
                        }
                    }
                    ui.label(format!(
                        "{} / {}（共 {}）",
                        self.visible
                            .iter()
                            .position(|i| *i == self.current)
                            .map_or(0, |i| i + 1),
                        self.visible.len(),
                        self.paths.len()
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.allocate_ui_with_layout(
                            Vec2::new(ui.available_width(), 48.),
                            egui::Layout::top_down(egui::Align::RIGHT),
                            |ui| {
                                if !self.visible.is_empty() {
                                    let name = self.paths[self.current]
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy();
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(name.as_ref()).size(17.).strong(),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(self.paths[self.current].display().to_string());
                                    if let Some(info) = &self.photos[self.current].info {
                                        let capture = &info.capture;
                                        let mut details = Vec::new();
                                        if let Some(iso) = capture.iso {
                                            details.push(format!("ISO {iso}"));
                                        }
                                        if let Some(aperture) = capture.aperture {
                                            details.push(format!("f/{aperture:.1}"));
                                        }
                                        if let Some(shutter) = capture.shutter.filter(|v| *v > 0.) {
                                            details.push(if shutter < 1. {
                                                format!("1/{:.0} s", 1. / shutter)
                                            } else {
                                                format!("{shutter:.1} s")
                                            });
                                        }
                                        if let Some(focal) = capture.focal {
                                            details.push(format!("{focal:.0} mm"));
                                        }
                                        let summary = if details.is_empty() {
                                            "未记录拍摄参数".to_owned()
                                        } else {
                                            details.join("  ·  ")
                                        };
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(summary)
                                                    .size(14.)
                                                    .color(Color32::from_gray(205)),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(
                                            format!(
                                                "{}\n{}\n{}",
                                                capture.camera.as_deref().unwrap_or(""),
                                                capture.lens.as_deref().unwrap_or(""),
                                                capture.captured_at.as_deref().unwrap_or("")
                                            ),
                                        );
                                    } else {
                                        ui.weak("正在读取拍摄信息…");
                                    }
                                }
                            },
                        );
                    });
                });
                if let Some(e) = &self.error {
                    ui.colored_label(RED, e);
                }
                if self.close_after_moves {
                    ui.label("正在完成移动，完成后自动关闭选片器");
                }
                if let Some(e) = &self.photos[self.current].metadata_error {
                    ui.colored_label(
                        RED,
                        format!(
                            "元数据读取/初始化失败，当前图片仍可浏览；评分暂不可用。按 R 重试：{e}"
                        ),
                    );
                }
                if let Some(session) = &self.session {
                    let stats = session.stats();
                    let done = stats.wrote + stats.kept + stats.error;
                    if stats.error > 0 {
                        ui.colored_label(
                            RED,
                            format!("{} 张照片未能读入 LR 评分，详情见日志", stats.error),
                        );
                    } else if done < session.len() {
                        ui.label(format!("后台读取 LR 评分：{done}/{}", session.len()));
                    }
                }
            });
        if self.filter_editor.open || self.folder_editor.open {
            if self.filter_editor.open {
                self.pool.start_index();
            }
            let rows: Vec<_> = self
                .paths
                .iter()
                .zip(&self.photos)
                .map(|(path, p)| fastcull::filtering::Row {
                    path,
                    values: &p.values,
                    capture: p.info.as_ref().map(|v| &v.capture),
                    indexed: p.capture_indexed || p.info.is_some(),
                })
                .collect();
            egui::SidePanel::left("filters")
                .default_width(260.)
                .width_range(220.0..=360.0)
                .frame(egui::Frame::new().fill(PANEL).inner_margin(12.))
                .show(ctx, |ui| {
                    if self.folder_editor.open {
                        let height = if self.filter_editor.open {
                            ui.available_height() * 0.35
                        } else {
                            ui.available_height() - 220.
                        };
                        if self
                            .folder_editor
                            .show(ui, self.current, &self.visible, height)
                        {
                            self.filter_dirty = true;
                        }
                        if self.filter_editor.open {
                            ui.separator();
                        }
                    }
                    if self.filter_editor.open {
                        egui::ScrollArea::vertical()
                            .id_salt("metadata-filters")
                            .show(ui, |ui| {
                                if self.filter_editor.show(ui, &mut self.filters, &rows) {
                                    self.filter_dirty = true;
                                }
                            });
                    }
                });
        }
        if self.filter_dirty {
            self.refilter();
        }
        self.film_panel(ctx);
        egui::TopBottomPanel::bottom("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(10, 7)),
            )
            .show(ctx, |ui| self.toolbar(ui));
        if self.filter_dirty {
            self.refilter();
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(4.))
            .show(ctx, |ui| self.central(ui, ctx));
        if let Some(e) = self.exposure_preview.as_ref().and_then(|p| p.error()) {
            self.error = Some(format!("曝光预览不可用，参数仍可保存：{e}"));
        }
        if self.folder_editor.dialogs(ctx) {
            self.filter_dirty = true;
        }
        self.submit_moves();
        if self.close_dialog {
            match close_confirmation(
                ctx,
                self.session.is_some(),
                !self.folder_editor.plan.moves.is_empty(),
                self.error.as_deref(),
            ) {
                Some(CloseChoice::Sync) => self.finish_close(ctx, true),
                Some(CloseChoice::Direct) => self.finish_close(ctx, false),
                Some(CloseChoice::Cancel) => self.close_dialog = false,
                None => {}
            }
        }
        // Foreground first; small preview work for visible cells; full prefetch only while loupe is active.
        if !self.grid && !self.visible.is_empty() {
            self.request(self.current, Level::Fit(self.fit_edge));
            if self.zoom {
                self.request(self.current, Level::Full);
            }
            // A brief pause on an already visible photo warms 100% on the
            // background worker. The foreground decoder remains available for
            // the next photo, even when this RAW needs sensor rendering.
            if !self.zoom
                && self.compare.is_none()
                && self.fit.items.contains_key(&self.current)
                && !self.full.items.contains_key(&self.current)
                && !self.photos[self.current]
                    .image_errors
                    .contains_key(&Level::Full)
            {
                let idle = self.navigation_at.elapsed();
                let delay = std::time::Duration::from_millis(300);
                if idle >= delay {
                    self.pool.warm_full(self.current);
                } else {
                    ctx.request_repaint_after(delay - idle);
                }
            }
            let level = if self.zoom {
                Level::Full
            } else {
                Level::Fit(self.fit_edge)
            };
            for step in 1..=if self.compare.is_some() {
                0
            } else if self.zoom {
                1
            } else {
                2
            } {
                if let Some(i) = fastcull::filtering::step_visible(
                    &self.visible,
                    self.current,
                    self.direction * step,
                ) && i != self.current
                    && self.prefetched.insert((i, level))
                {
                    self.request(i, level);
                }
            }
        }
        if egui::DragAndDrop::has_payload_of_type::<Vec<usize>>(ctx) {
            ctx.set_cursor_icon(
                self.folder_editor
                    .drop_cursor
                    .unwrap_or(egui::CursorIcon::NotAllowed),
            );
        }
        self.smoke(ctx);
    }
}
fn paint_image(
    painter: &egui::Painter,
    t: &TextureHandle,
    area: Rect,
    orientation: u16,
    crop: Option<fastcull::crop::Crop>,
    native_size: Option<Vec2>,
) -> Rect {
    paint_adjusted(
        painter,
        t,
        area,
        orientation,
        0.,
        Vec2::ZERO,
        1.,
        native_size,
        None,
        crop,
    )
}
fn preview_edge(size: Vec2, pixels_per_point: f32) -> u32 {
    // Keep 25% headroom over physical display pixels, with stable buckets so
    // resizing does not schedule a new decode for every single pixel change.
    let pixels = size.x.max(size.y) * pixels_per_point * 1.25;
    ((pixels / 256.).ceil() as u32 * 256).clamp(1024, 4096)
}
#[allow(clippy::too_many_arguments)]
fn paint_adjusted(
    painter: &egui::Painter,
    t: &TextureHandle,
    area: Rect,
    orientation: u16,
    zoom: f32,
    pan: Vec2,
    ppp: f32,
    native_size: Option<Vec2>,
    preview: Option<(Arc<crate::exposure_preview::Preview>, f32)>,
    crop: Option<fastcull::crop::Crop>,
) -> Rect {
    let original = native_size.unwrap_or_else(|| t.size_vec2());
    let crop = crop.unwrap_or_default();
    let size = crop.geometry(original, orientation).rect.size();
    let scale = if zoom > 0. {
        zoom / ppp
    } else {
        (area.width() / size.x).min(area.height() / size.y)
    };
    let extent = (size * scale - area.size()).max(Vec2::ZERO) * 0.5;
    let pan = Vec2::new(
        pan.x.clamp(-extent.x, extent.x),
        pan.y.clamp(-extent.y, extent.y),
    );
    let rect = Rect::from_center_size(area.center() + pan, size * scale);
    let source = crop.raw_uvs(original);
    let uvs = fastcull::crop::order(orientation).map(|i| source[i]);
    paint_uv(painter, t, rect, uvs);
    if let Some((preview, delta)) = preview {
        preview.paint(painter, area, rect, t.id(), uvs, delta);
    }
    rect
}
fn paint_uv(painter: &egui::Painter, t: &TextureHandle, rect: Rect, uvs: [Pos2; 4]) {
    let mut mesh = egui::Mesh::with_texture(t.id());
    for (pos, uv) in fastcull::crop::corners(rect).into_iter().zip(uvs) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos,
            uv,
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}
fn save_png(path: &std::path::Path, size: [usize; 2], rgba: &[u8]) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(file, size[0] as u32, size[1] as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}
enum NavigationIcon {
    Folder,
    Filter,
    Crop,
}
fn navigation_button(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    icon: NavigationIcon,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(56., 66.), egui::Sense::click());
    let painter = ui.painter();
    let accent = Color32::from_rgb(131, 183, 255);
    let color = if selected {
        accent
    } else {
        Color32::from_gray(165)
    };
    if selected || response.hovered() {
        painter.rect_filled(
            rect.shrink(2.),
            8.,
            if selected {
                Color32::from_rgb(39, 55, 76)
            } else {
                Color32::from_rgb(42, 46, 53)
            },
        );
    }
    if selected {
        painter.rect_filled(
            Rect::from_min_size(rect.left_top() + Vec2::new(0., 12.), Vec2::new(2., 34.)),
            1.,
            accent,
        );
    }
    let origin = rect.center_top() + Vec2::new(-11., 13.);
    let points = match icon {
        NavigationIcon::Filter => vec![
            origin,
            origin + Vec2::new(22., 0.),
            origin + Vec2::new(13., 10.),
            origin + Vec2::new(13., 19.),
            origin + Vec2::new(9., 21.),
            origin + Vec2::new(9., 10.),
            origin,
        ],
        NavigationIcon::Folder => vec![
            origin + Vec2::new(0., 3.),
            origin + Vec2::new(8., 3.),
            origin + Vec2::new(11., 7.),
            origin + Vec2::new(23., 7.),
            origin + Vec2::new(23., 21.),
            origin + Vec2::new(0., 21.),
            origin + Vec2::new(0., 3.),
        ],
        NavigationIcon::Crop => {
            painter.add(egui::Shape::line(
                vec![
                    origin + Vec2::new(0., 5.),
                    origin + Vec2::new(17., 5.),
                    origin + Vec2::new(17., 24.),
                ],
                Stroke::new(1.6, color),
            ));
            vec![
                origin + Vec2::new(5., 0.),
                origin + Vec2::new(5., 19.),
                origin + Vec2::new(24., 19.),
            ]
        }
    };
    painter.add(egui::Shape::line(points, Stroke::new(1.6, color)));
    painter.text(
        rect.center_bottom() - Vec2::new(0., 10.),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::proportional(12.),
        color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
fn configure(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.override_text_color = Some(Color32::from_rgb(218, 223, 231));
    visuals.extreme_bg_color = Color32::from_rgb(25, 28, 33);
    visuals.selection.bg_fill = Color32::from_rgb(48, 73, 105);
    visuals.selection.stroke = Stroke::new(1., Color32::from_rgb(169, 205, 255));
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1., Color32::from_rgb(49, 54, 62));
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(42, 47, 55);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(42, 47, 55);
    visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(56, 64, 76);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(56, 64, 76);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1., Color32::from_rgb(96, 124, 164));
    visuals.widgets.active.bg_fill = Color32::from_rgb(48, 73, 105);
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.noninteractive,
    ] {
        widget.corner_radius = egui::CornerRadius::same(5);
    }
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(13.));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(12.));
        style.spacing.item_spacing = Vec2::new(7., 7.);
        style.spacing.button_padding = Vec2::new(7., 4.);
        style.spacing.interact_size.y = 26.;
        style.spacing.indent = 14.;
    });
    if let Some(font) = cjk_font() {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert("cjk".into(), font.clone());
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .push("cjk".into());
        ctx.set_fonts(fonts);
    }
}
fn cjk_font() -> &'static Option<Arc<egui::FontData>> {
    static FONT: std::sync::OnceLock<Option<Arc<egui::FontData>>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        fastcull::platform::cjk_font_paths()
            .iter()
            .find_map(|path| std::fs::read(path).ok())
            .map(|bytes| Arc::new(egui::FontData::from_owned(bytes)))
    })
}
fn preload_fonts() {
    // Overlap font I/O with native window and graphics-driver initialization.
    std::thread::spawn(cjk_font);
}

// Create the window before filesystem enumeration. The same shell is also the
// standalone entry point, so a double click does not require command-line input.
struct Shell {
    app: Option<App>,
    gl: Option<Arc<eframe::glow::Context>>,
    pending: Option<(Option<PathBuf>, Vec<PathBuf>)>,
    receiver: Option<std::sync::mpsc::Receiver<Result<Option<Vec<PathBuf>>>>>,
    error: Option<String>,
    frames: usize,
    focus: crate::focus::StartupFocus,
}
impl Shell {
    fn load(
        &mut self,
        ctx: &egui::Context,
        request: Option<PathBuf>,
        roots: Vec<PathBuf>,
        picker: Option<bool>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.receiver = Some(rx);
        self.error = None;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = (|| -> Result<Option<Vec<PathBuf>>> {
                let roots = if let Some(file) = request {
                    let roots = crate::bridge_list::read(&file)?;
                    let _ = std::fs::remove_file(file);
                    roots
                } else if let Some(folder) = picker {
                    #[cfg(any(windows, target_os = "macos"))]
                    {
                        let dialog =
                            rfd::FileDialog::new().set_title("FastCull · 选择照片或文件夹");
                        let selected = if folder {
                            dialog.pick_folders()
                        } else {
                            dialog
                                .add_filter("RAW / JPEG", fastcull::formats::EXTENSIONS)
                                .pick_files()
                        };
                        let Some(paths) = selected else {
                            return Ok(None);
                        };
                        paths
                    }
                    #[cfg(not(any(windows, target_os = "macos")))]
                    {
                        let _ = folder;
                        anyhow::bail!("请将照片或文件夹拖入窗口");
                    }
                } else {
                    roots
                };
                crate::bridge_list::expand(roots).map(Some)
            })();
            log::info!(
                "startup discovery elapsed_ms={:.1}",
                started.elapsed().as_secs_f64() * 1000.
            );
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
}
impl eframe::App for Shell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if let Some(app) = &mut self.app {
            app.raw_input_hook(ctx, input);
        }
    }
    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let Some(app) = &mut self.app {
            app.on_exit(gl);
        }
    }
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(app) = &mut self.app {
            app.update(ctx, frame);
            return;
        }
        self.focus.update(ctx, frame);
        self.frames += 1;
        if self.frames == 1 {
            log::info!("startup shell-first-frame");
        }
        if let Some((request, roots)) = self.pending.take() {
            self.load(ctx, request, roots, None);
        }
        if let Some(result) = self.receiver.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.receiver = None;
            match result {
                Ok(Some(paths)) => {
                    let mut app = App::from_context(ctx, self.gl.as_ref(), paths, None);
                    // Loading must not start another foreground-stealing window.
                    std::mem::swap(&mut app.startup_focus, &mut self.focus);
                    app.enable_bridge_moves(ctx, crate::logger::directory().join("moves"));
                    self.app = Some(app);
                    ctx.request_repaint();
                }
                Ok(None) => {}
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.30).max(40.));
                ui.label(egui::RichText::new("FastCull").size(36.).strong());
                ui.add_space(14.);
                ui.label(egui::RichText::new("快速浏览，专注选片").size(18.));
                ui.add_space(30.);
                if self.receiver.is_some() || self.pending.is_some() || self.app.is_some() {
                    ui.spinner();
                    ui.label("正在整理照片列表…");
                } else {
                    let size = egui::vec2(180., 44.);
                    if ui.add_sized(size, egui::Button::new("打开照片")).clicked() {
                        self.load(ctx, None, Vec::new(), Some(false));
                    }
                    ui.add_space(12.);
                    if ui
                        .add_sized(size, egui::Button::new("打开文件夹"))
                        .clicked()
                    {
                        self.load(ctx, None, Vec::new(), Some(true));
                    }
                    ui.add_space(22.);
                    ui.weak("也可以将照片或文件夹拖到这里");
                    ui.weak("ARW · CR2 · NEF · JPG / JPEG");
                    if let Some(error) = &self.error {
                        ui.add_space(16.);
                        ui.colored_label(RED, error);
                    }
                }
            });
        });
        if self.receiver.is_none() && self.app.is_none() {
            let dropped = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect::<Vec<_>>()
            });
            if !dropped.is_empty() {
                self.load(ctx, None, dropped, None);
            }
        }
        if self.frames == 1 {
            ctx.request_repaint();
        }
        if std::env::var_os("FASTCULL_LAUNCHER_SMOKE").is_some() {
            for event in ctx.input(|i| i.raw.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    if let Some(dir) = std::env::var_os("FASTCULL_SMOKE_DIR") {
                        let bytes: Vec<u8> =
                            image.pixels.iter().flat_map(|p| p.to_array()).collect();
                        let _ =
                            save_png(&PathBuf::from(dir).join("launcher.png"), image.size, &bytes);
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            if self.frames == 2 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
        }
    }
}
fn initial_window_size() -> [f32; 2] {
    if std::env::var_os("FASTCULL_SMOKE_DIR").is_some() {
        std::env::var("FASTCULL_SMOKE_SIZE")
            .ok()
            .and_then(|s| {
                s.split_once('x')
                    .and_then(|(w, h)| Some([w.parse::<f32>().ok()?, h.parse::<f32>().ok()?]))
            })
            .filter(|s| s.iter().all(|v| v.is_finite() && *v >= 480. && *v <= 4096.))
            .unwrap_or([1280., 850.])
    } else {
        [1280., 850.]
    }
}
fn run_shell(request: Option<PathBuf>, paths: Vec<PathBuf>) -> Result<()> {
    preload_fonts();
    if let Some(dir) = std::env::var_os("FASTCULL_SMOKE_DIR") {
        std::fs::create_dir_all(dir)?;
    }
    let pending = (request.is_some() || !paths.is_empty()).then_some((request, paths));
    log::info!("startup native-window begin");
    eframe::run_native(
        "FastCull",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Glow,
            viewport: egui::ViewportBuilder::default()
                .with_active(true)
                .with_inner_size(initial_window_size())
                .with_min_inner_size([640., 480.]),
            ..Default::default()
        },
        Box::new(move |cc| {
            configure(&cc.egui_ctx);
            let mut shell = Shell {
                app: None,
                gl: cc.gl.clone(),
                pending,
                receiver: None,
                error: None,
                frames: 0,
                focus: crate::focus::StartupFocus::new(),
            };
            if let Some((request, roots)) = shell.pending.take() {
                shell.load(&cc.egui_ctx, request, roots, None);
            }
            Ok(Box::new(shell))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI: {e}"))
}

pub fn run(paths: Vec<PathBuf>) -> Result<()> {
    run_standalone(paths)
}
pub fn run_bridge_request(file: PathBuf) -> Result<()> {
    run_shell(Some(file), Vec::new())
}
pub fn run_standalone(paths: Vec<PathBuf>) -> Result<()> {
    run_shell(None, paths)
}
pub fn run_state(items: Vec<state::Item>, manifest: Option<PathBuf>) -> Result<()> {
    preload_fonts();
    let started = Instant::now();
    let paths = items.iter().map(|i| i.path.clone()).collect();
    let session = Arc::new(Session::new(items));
    if let Some(path) = manifest {
        session.attach_manifest(path)?;
    }
    log::info!(
        "startup session-attached elapsed_ms={:.1}",
        started.elapsed().as_secs_f64() * 1000.
    );
    let result = run_session(paths, Some(session.clone()), false);
    session.close()?;
    if result.is_ok()
        && session
            .sync_on_exit
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        session.request_sync()?;
    }
    result
}
fn run_session(paths: Vec<PathBuf>, session: Option<Arc<Session>>, bridge: bool) -> Result<()> {
    anyhow::ensure!(!paths.is_empty(), "empty photo list");
    if let Some(dir) = std::env::var_os("FASTCULL_SMOKE_DIR") {
        std::fs::create_dir_all(dir)?;
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_active(true)
            .with_inner_size(initial_window_size())
            .with_min_inner_size([640., 480.]),
        ..Default::default()
    };
    log::info!("startup native-window begin");
    eframe::run_native(
        "FastCull · RAW/JPEG",
        options,
        Box::new(move |cc| {
            let mut app = match session {
                Some(session) => App::new_session(cc, paths, Some(session)),
                None => App::new(cc, paths),
            };
            if bridge {
                app.enable_bridge_moves(&cc.egui_ctx, crate::logger::directory().join("moves"));
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnail_bounds_do_not_change_when_full_geometry_arrives() {
        for film in [false, true] {
            for orientation in [1, 6] {
                for crop in [
                    None,
                    Some(fastcull::crop::Crop {
                        enabled: true,
                        left: 200_000,
                        top: 100_000,
                        right: 800_000,
                        bottom: 900_000,
                        angle: 12_000_000,
                    }),
                ] {
                    let ctx = egui::Context::default();
                    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
                    let mut app = App::new(&cc, vec![PathBuf::from("fixture.ARW")]);
                    app.photos[0].values.orientation = Some(orientation);
                    app.photos[0].values.crop = crop;
                    let thumb = ctx.load_texture(
                        "thumb",
                        egui::ColorImage::filled([160, 120], Color32::WHITE),
                        TextureOptions::LINEAR,
                    );
                    let id = thumb.id();
                    app.thumbs.insert(0, thumb, 0);
                    let mut vertices = Vec::new();
                    for full_size in [None, Some(Vec2::new(6192., 4128.))] {
                        app.photos[0].full_size = full_size;
                        let output = ctx.run(egui::RawInput::default(), |ctx| {
                            egui::CentralPanel::default()
                                .show(ctx, |ui| app.cell(ui, 0, Vec2::new(200., 180.), film));
                        });
                        let mesh = output
                            .shapes
                            .iter()
                            .find_map(|s| match &s.shape {
                                egui::Shape::Mesh(m) if m.texture_id == id => Some(m),
                                _ => None,
                            })
                            .unwrap();
                        vertices.push(
                            mesh.vertices
                                .iter()
                                .map(|v| (v.pos, v.uv))
                                .collect::<Vec<_>>(),
                        );
                    }
                    assert_eq!(
                        vertices[0], vertices[1],
                        "film={film}, orientation={orientation}, crop={crop:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn fit_preview_tracks_physical_canvas_with_headroom_and_stable_buckets() {
        assert_eq!(preview_edge(Vec2::new(944., 612.), 1.), 1280);
        assert_eq!(preview_edge(Vec2::new(945., 613.), 1.), 1280);
        assert_eq!(preview_edge(Vec2::new(944., 612.), 2.), 2560);
        assert_eq!(preview_edge(Vec2::new(3800., 2100.), 1.), 4096);
    }
    fn wait_moves(app: &mut App, ctx: &egui::Context) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while app.mover.as_ref().unwrap().jobs > 0 {
            app.receive_moves(ctx);
            assert!(Instant::now() < deadline, "move queue stalled");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        app.refilter();
        assert!(app.error.is_none(), "{:?}", app.error);
    }
    #[test]
    fn crop_draft_cancel_save_failure_and_relocated_edit_keep_correct_identity() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("a.ARW");
        std::fs::write(&raw, b"original RAW").unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![raw.clone()]);
        app.enable_bridge_moves(&ctx, dir.path().join("journal"));
        app.photos[0].loaded = true;
        app.photos[0].full_size = Some(Vec2::new(6000., 4000.));
        app.begin_crop();
        app.crop_editor
            .as_mut()
            .unwrap()
            .select_ratio("1:1".into(), Some(1.));
        assert!(!raw.with_extension("xmp").exists());
        app.crop_editor = None; // Cancel discards the draft, preserving original bytes.
        assert!(!raw.with_extension("xmp").exists());
        app.begin_crop();
        app.crop_editor
            .as_mut()
            .unwrap()
            .select_ratio("16:9".into(), Some(16. / 9.));
        let expected = app.crop_editor.as_ref().unwrap().value();
        app.apply_crop();
        assert!(app.crop_editor.is_none());
        assert_eq!(xmp::read(&raw).unwrap().crop, Some(expected));
        let dest = dir.path().join("精选");
        std::fs::create_dir(&dest).unwrap();
        app.folder_editor.stage(&[0], &dest);
        app.submit_moves();
        wait_moves(&mut app, &ctx);
        let moved = dest.join("a.ARW");
        assert_eq!(app.paths[0], moved);
        app.begin_crop();
        assert_eq!(app.crop_editor.as_ref().unwrap().value(), expected);
        app.crop_editor
            .as_mut()
            .unwrap()
            .select_ratio("1:1".into(), Some(1.));
        app.apply_crop();
        let saved = xmp::read(&moved).unwrap().crop;
        assert_ne!(saved, Some(expected));
        assert!(!raw.with_extension("xmp").exists());
        assert_eq!(app.photos[0].values.crop, saved);
        std::fs::write(moved.with_extension("xmp"), "<broken>").unwrap();
        app.begin_crop();
        app.apply_crop();
        assert!(app.crop_editor.is_some());
        assert!(app.error.is_some());
        assert_eq!(app.photos[0].values.crop, saved);
        assert_eq!(
            std::fs::read_to_string(moved.with_extension("xmp")).unwrap(),
            "<broken>"
        );
    }
    #[test]
    fn bridge_continuous_batch_and_second_move_keep_photo_identity_and_new_xmp_path() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let paths: Vec<_> = (0..4).map(|i| source.join(format!("{i}.ARW"))).collect();
        for p in &paths {
            std::fs::write(p, "unchanged raw").unwrap();
        }
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, paths.clone());
        app.enable_bridge_moves(&ctx, dir.path().join("journal"));
        for p in &mut app.photos {
            p.loaded = true;
            p.exposure_base = Some(0);
        }
        app.rating(2);
        let dest = app.folder_editor.create_folder(dir.path(), "精选").unwrap();
        app.folder_editor.stage(&[0], &dest);
        app.submit_moves();
        assert_eq!(app.current, 1);
        app.folder_editor.repeat_move(1, &app.visible);
        app.submit_moves();
        assert_eq!(app.current, 2);
        app.go(0);
        app.rating(5); // queued photos cannot write at their old path.
        {
            let locations = app.pool.locations();
            let location = locations[0].read().unwrap();
            assert_eq!(xmp::read(&location).unwrap().rating, Some(2));
        }
        wait_moves(&mut app, &ctx);
        assert_eq!(app.paths[0], dest.join("0.ARW"));
        assert_eq!(app.paths[1], dest.join("1.ARW"));
        app.go(0);
        app.rating(4);
        assert_eq!(xmp::read(&app.paths[0]).unwrap().rating, Some(4));
        assert!(!paths[0].with_extension("xmp").exists());
        let batch = app.folder_editor.create_folder(dir.path(), "批量").unwrap();
        app.folder_editor.stage(&[2, 3], &batch);
        app.submit_moves();
        wait_moves(&mut app, &ctx);
        app.folder_editor.selected = Some(dest.clone());
        app.refilter();
        assert_eq!(app.visible, [0, 1]);
        app.folder_editor.selected = None;
        app.refilter();
        app.go(0);
        app.compare = Some(Compare {
            indices: [0, 3],
            views: [
                View {
                    zoom: 1.,
                    pan: Vec2::new(30., 40.),
                },
                View {
                    zoom: 1.,
                    pan: Vec2::new(10., 20.),
                },
            ],
            active: 0,
            linked: true,
        });
        let again = app
            .folder_editor
            .create_folder(dir.path(), "再移动")
            .unwrap();
        app.folder_editor.advance = false;
        app.folder_editor.stage(&[0], &again);
        app.submit_moves();
        wait_moves(&mut app, &ctx);
        assert_eq!(app.compare.as_ref().unwrap().indices, [0, 3]);
        assert_eq!(
            app.compare.as_ref().unwrap().views[0].pan,
            Vec2::new(30., 40.)
        );
        app.rating(5);
        assert_eq!(xmp::read(&app.paths[0]).unwrap().rating, Some(5));
        assert!(!dest.join("0.ARW").exists());
        for p in app.paths.iter() {
            assert_eq!(std::fs::read(p).unwrap(), b"unchanged raw");
        }
    }
    #[test]
    fn bridge_close_waits_for_pending_jobs_and_text_focus_keeps_m_for_typing() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("one.ARW");
        std::fs::write(&raw, "raw").unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![raw]);
        app.enable_bridge_moves(&ctx, dir.path().join("journal"));
        let target = app
            .folder_editor
            .create_folder(dir.path(), "target")
            .unwrap();
        let locations = app.pool.locations();
        let held = locations[0].read().unwrap(); // deterministic in-flight job.
        ctx.begin_pass(egui::RawInput::default());
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("folder-search")));
        app.folder_editor.last_target = Some(target.clone());
        ctx.input_mut(|i| {
            i.events.push(egui::Event::Key {
                key: Key::M,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
        });
        app.keyboard(&ctx);
        assert_eq!(app.mover.as_ref().unwrap().jobs, 0);
        ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("folder-search")));
        app.folder_editor.stage(&[0], &target);
        app.submit_moves();
        app.finish_close(&ctx, false);
        assert!(!app.allow_close);
        assert!(app.close_after_moves);
        drop(held);
        wait_moves(&mut app, &ctx);
        assert!(app.allow_close);
        let _ = ctx.end_pass();
    }
    #[test]
    fn folder_plan_filtering_and_grading_keep_original_photo_identity() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("上午");
        let b = dir.path().join("下午");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let paths = vec![a.join("a.ARW"), b.join("b.ARW")];
        for p in &paths {
            std::fs::write(p, "unchanged raw").unwrap();
        }
        let manifest = dir.path().join("session.txt");
        fastcull::handoff::Manifest::new("folder-test".into(), &paths)
            .unwrap()
            .write(&manifest)
            .unwrap();
        let session = Arc::new(Session::new(
            paths
                .iter()
                .map(|p| state::Item {
                    path: p.clone(),
                    updates: Updates {
                        rating: Some(4),
                        ..Default::default()
                    },
                })
                .collect(),
        ));
        session.attach_manifest(manifest.clone()).unwrap();
        for i in 0..2 {
            session.initialize(i).unwrap();
        }
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx);
        let mut app = App::new_session(&cc, paths.clone(), Some(session));
        assert!(app.filter_editor.open);
        assert!(!app.folder_editor.open);
        for p in &mut app.photos {
            p.loaded = true;
            p.exposure_base = Some(100000);
        }
        let dest = app.folder_editor.create_folder(dir.path(), "精选").unwrap();
        assert!(app.folder_editor.stage(&[1], &dest));
        app.folder_editor.selected = Some(dest.clone());
        app.filters.selected.insert(
            fastcull::filtering::Field::Rating,
            [Some("4".into())].into(),
        );
        app.refilter();
        assert_eq!(app.visible, [1]);
        assert_eq!(app.current, 1);
        app.rating(5);
        app.adjust_exposure(1);
        assert_eq!(xmp::read(&paths[0]).unwrap().rating, Some(4));
        assert_eq!(xmp::read(&paths[1]).unwrap().rating, Some(5));
        assert_eq!(xmp::read(&paths[1]).unwrap().exposure, Some(433333));
        assert!(!dest.join("b.ARW").exists());
        let rows = fastcull::handoff::Manifest::read(&manifest)
            .unwrap()
            .rows_since(0);
        assert_eq!(rows[0].0, paths[1]);
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"unchanged raw");
        app.folder_editor.selected = None;
        app.filters = Default::default();
        app.refilter();
        assert_eq!(app.visible, [0, 1]);
    }
    #[test]
    fn folder_search_and_multiple_selection_do_not_grade_or_close_photos() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("a.ARW"), PathBuf::from("b.ARW")]);
        ctx.begin_pass(key_input(&[Key::Q, Key::Num5]));
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("folder-search")));
        app.keyboard(&ctx);
        assert!(!app.close_dialog);
        assert_eq!(app.current, 0);
        let _ = ctx.end_pass();
        app.visible = vec![1];
        ctx.begin_pass(egui::RawInput::default());
        ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("folder-search")));
        ctx.input_mut(|i| {
            i.events.push(egui::Event::Key {
                key: Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            })
        });
        app.keyboard(&ctx);
        assert_eq!(
            app.folder_editor.photos.iter().copied().collect::<Vec<_>>(),
            [1]
        );
        let _ = ctx.end_pass();
    }
    #[test]
    fn minimum_window_with_filters_keeps_toolbar_bounded() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("photo.ARW")]);
        app.photos[0].loaded = true;
        app.photos[0].exposure_base = Some(100000);
        let mut height = 0.;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640., 480.))),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::SidePanel::left("review-filters")
                .exact_width(242.)
                .show(ctx, |_| {});
            height = egui::TopBottomPanel::bottom("review-controls")
                .show(ctx, |ui| app.toolbar(ui))
                .response
                .rect
                .height();
        });
        assert!(
            height <= 112.,
            "Toolbar takes {height} px and crowds the image"
        );
    }
    #[test]
    fn filmstrip_wheel_respects_hover_drag_dialog_filter_and_comparison() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(
            &cc,
            (0..8)
                .map(|i| PathBuf::from(format!("film-{i}.ARW")))
                .collect(),
        );
        app.visible = vec![0, 2, 4, 6];
        let frame = |app: &mut App, pos: Pos2, extra: Vec<egui::Event>| {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                ..Default::default()
            };
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.extend(extra);
            let _ = ctx.run(input, |ctx| {
                app.film_panel(ctx);
            });
        };
        let wheel = |delta| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0., delta),
            modifiers: egui::Modifiers::NONE,
        };
        let inside = Pos2::new(400., 560.);
        frame(&mut app, inside, vec![]);
        frame(&mut app, inside, vec![wheel(-1.)]);
        assert_eq!(app.current, 2);
        frame(&mut app, Pos2::new(400., 100.), vec![wheel(-1.)]);
        assert_eq!(app.current, 2);
        app.close_dialog = true;
        frame(&mut app, inside, vec![wheel(-1.)]);
        assert_eq!(app.current, 2);
        app.close_dialog = false;
        frame(
            &mut app,
            inside,
            vec![
                egui::Event::PointerButton {
                    pos: inside,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                wheel(-1.),
            ],
        );
        assert_eq!(app.current, 2);
        frame(
            &mut app,
            inside,
            vec![egui::Event::PointerButton {
                pos: inside,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        app.go(2);
        app.compare = Some(Compare {
            indices: [2, 4],
            views: [View::default(); 2],
            active: 0,
            linked: true,
        });
        frame(&mut app, inside, vec![wheel(-1.)]);
        assert_eq!(app.current, 6);
        assert_eq!(app.compare.as_ref().unwrap().indices, [6, 4]);
        frame(&mut app, inside, vec![wheel(-1.)]);
        assert_eq!(app.current, 6);
    }

    #[test]
    fn filmstrip_border_drag_resizes_and_keeps_current_photo() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("resize.ARW")]);
        let mut frame = |events| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                events,
                ..Default::default()
            };
            let mut rect = Rect::NOTHING;
            let _ = ctx.run(input, |ctx| {
                rect = app.film_panel(ctx);
            });
            rect
        };
        let original = frame(vec![]);
        let edge = Pos2::new(400., original.top());
        frame(vec![egui::Event::PointerMoved(edge)]);
        frame(vec![egui::Event::PointerButton {
            pos: edge,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let moved = edge - Vec2::new(0., 80.);
        let resized = frame(vec![egui::Event::PointerMoved(moved)]);
        frame(vec![egui::Event::PointerButton {
            pos: moved,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(
            resized.height() > original.height() + 50.,
            "{original:?} -> {resized:?}"
        );
        assert_eq!(app.current, 0);
    }

    #[test]
    fn click_zoom_centers_clicked_image_point_and_modes_are_consistent() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("a.jpg"), PathBuf::from("b.jpg")]);
        assert!(app.filter_editor.open);
        let image = Rect::from_min_size(Pos2::new(200., 50.), Vec2::new(600., 400.));
        app.zoom_at(image, Some(Pos2::new(650., 150.)));
        assert!(app.zoom);
        assert_eq!(app.pan, Vec2::new(-0.25, 0.25));
        app.next(1);
        assert_eq!(app.pan, Vec2::new(-0.25, 0.25));
        app.zoom_at(image, Some(image.center()));
        assert!(!app.zoom);
        app.zoom_at(image, Some(Pos2::ZERO));
        assert_eq!(app.pan, Vec2::ZERO); // Letterbox click has no image coordinate.
        for key in [Key::C, Key::G, Key::E] {
            ctx.begin_pass(key_input(&[key]));
            app.keyboard(&ctx);
            match key {
                Key::C => {
                    assert!(app.compare.is_some());
                    assert!(!app.grid);
                }
                Key::G => assert!(app.grid),
                Key::E => {
                    assert!(app.compare.is_none());
                    assert!(!app.grid);
                }
                _ => unreachable!(),
            }
            let _ = ctx.end_pass();
        }
    }

    #[test]
    fn zoom_navigation_never_flashes_fit_before_full_geometry_is_known() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("a.jpg"), PathBuf::from("b.jpg")]);
        let preview = ctx.load_texture(
            "pending-preview",
            egui::ColorImage::filled([600, 400], Color32::WHITE),
            TextureOptions::LINEAR,
        );
        let id = preview.id();
        app.fit.insert(1, preview, 1);
        app.zoom = true;
        app.pan = Vec2::new(0.1, -0.1);
        app.next(1);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.central(ui, ctx));
            },
        );
        assert!(app.zoom);
        assert_eq!(app.pan, Vec2::new(0.1, -0.1));
        assert!(
            !output.shapes.iter().any(
                |shape| matches!(&shape.shape, egui::Shape::Mesh(mesh) if mesh.texture_id == id)
            ),
            "100% navigation must not briefly draw the next photo fitted to the window"
        );
    }

    #[test]
    fn fit_navigation_geometry_stays_fixed_across_thumb_fit_and_full() {
        for orientation in [1, 6] {
            for compare in [false, true] {
                for cropped in [false, true] {
                    for ppp in [1., 1.5, 2.] {
                        let ctx = egui::Context::default();
                        ctx.set_pixels_per_point(ppp);
                        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
                        let mut app =
                            App::new(&cc, vec![PathBuf::from("a.ARW"), PathBuf::from("b.ARW")]);
                        app.next(1);
                        // Decoded previews carry source metadata before their first frame.
                        let mut info = fastcull::arw::ArwInfo::read(std::path::Path::new(
                            "tests/fixtures/preview-3x2.jpg",
                        ))
                        .unwrap();
                        info.image_size = Some((1200, 800));
                        app.photos[1].info = Some(info);
                        app.photos[1].values.orientation = Some(orientation);
                        if cropped {
                            app.photos[1].values.crop = Some(fastcull::crop::Crop {
                                enabled: true,
                                left: 200_000,
                                top: 100_000,
                                right: 800_000,
                                bottom: 900_000,
                                angle: 12_000_000,
                            });
                        }
                        if compare {
                            app.compare = Some(Compare {
                                indices: [0, 1],
                                active: 1,
                                linked: false,
                                views: [View {
                                    zoom: 0.,
                                    pan: Vec2::ZERO,
                                }; 2],
                            });
                        }
                        let mut vertices = Vec::new();
                        // Real Sony preview proportions, including resize rounding.
                        for (stage, size) in
                            [[200, 134], [808, 540], [808, 540], [1200, 800], [808, 540]]
                                .into_iter()
                                .enumerate()
                        {
                            let t = ctx.load_texture(
                                format!("stage-{stage}"),
                                egui::ColorImage::filled(size, Color32::WHITE),
                                TextureOptions::LINEAR,
                            );
                            let id = t.id();
                            match stage {
                                0 => app.thumbs.insert(1, t, 1),
                                1 => app.fit.insert(1, t, 1),
                                2 => {
                                    app.photos[1].full_size = Some(Vec2::new(1200., 800.));
                                    app.fit.insert(1, t, 1);
                                }
                                3 => app.full.insert(1, t, 1),
                                _ => {
                                    app.full.items.remove(&1);
                                    app.fit.insert(1, t, 1);
                                }
                            }
                            let input = egui::RawInput {
                                screen_rect: Some(Rect::from_min_size(
                                    Pos2::ZERO,
                                    Vec2::new(1600., 900.),
                                )),
                                ..Default::default()
                            };
                            // Apply pending DPI/layout changes before measuring.
                            let _ = ctx.run(input.clone(), |ctx| {
                                egui::CentralPanel::default().show(ctx, |ui| app.central(ui, ctx));
                            });
                            let output = ctx.run(input, |ctx| {
                                egui::CentralPanel::default().show(ctx, |ui| app.central(ui, ctx));
                            });
                            let mesh = output
                                .shapes
                                .iter()
                                .find_map(|shape| match &shape.shape {
                                    egui::Shape::Mesh(mesh) if mesh.texture_id == id => Some(mesh),
                                    _ => None,
                                })
                                .expect("current photo is rendered");
                            vertices.push(
                                mesh.vertices
                                    .iter()
                                    .map(|v| (v.pos, v.uv))
                                    .collect::<Vec<_>>(),
                            );
                        }
                        assert!(
                            vertices.iter().all(|v| *v == vertices[0]),
                            "main photo shifts: orientation={orientation} compare={compare} cropped={cropped} ppp={ppp}: {vertices:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn zoom_preview_to_full_keeps_crop_for_each_resolution_rotation_and_dpi() {
        for size in [[1200, 800], [1800, 1200]] {
            for orientation in [1, 6] {
                for ppp in [1., 1.5, 2.] {
                    for compare in [false, true] {
                        let ctx = egui::Context::default();
                        ctx.set_pixels_per_point(ppp);
                        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
                        let mut app =
                            App::new(&cc, vec![PathBuf::from("a.jpg"), PathBuf::from("b.jpg")]);
                        app.zoom = true;
                        app.next(1);
                        app.pan = Vec2::new(0.13, -0.09);
                        app.photos[1].full_size = Some(Vec2::new(size[0] as f32, size[1] as f32));
                        app.photos[1].values.orientation = Some(orientation);
                        if compare {
                            app.compare = Some(Compare {
                                indices: [0, 1],
                                active: 1,
                                linked: false,
                                views: [View {
                                    zoom: 1.,
                                    pan: app.pan,
                                }; 2],
                            });
                        }
                        let frame = |app: &mut App, id| {
                            // Apply the pending DPI/viewport change before measuring.
                            let _ = ctx.run(
                                egui::RawInput {
                                    screen_rect: Some(Rect::from_min_size(
                                        Pos2::ZERO,
                                        Vec2::new(800., 600.),
                                    )),
                                    ..Default::default()
                                },
                                |ctx| {
                                    egui::CentralPanel::default()
                                        .show(ctx, |ui| app.central(ui, ctx));
                                },
                            );
                            let output = ctx.run(
                                egui::RawInput {
                                    screen_rect: Some(Rect::from_min_size(
                                        Pos2::ZERO,
                                        Vec2::new(800., 600.),
                                    )),
                                    ..Default::default()
                                },
                                |ctx| {
                                    egui::CentralPanel::default()
                                        .show(ctx, |ui| app.central(ui, ctx));
                                },
                            );
                            output
                                .shapes
                                .iter()
                                .find_map(|shape| match &shape.shape {
                                    egui::Shape::Mesh(mesh) if mesh.texture_id == id => {
                                        Some(Rect::from_min_max(
                                            mesh.vertices[0].pos,
                                            mesh.vertices[2].pos,
                                        ))
                                    }
                                    _ => None,
                                })
                                .expect("current photo is rendered")
                        };
                        let thumb = ctx.load_texture(
                            "small",
                            egui::ColorImage::filled([120, 80], Color32::WHITE),
                            TextureOptions::LINEAR,
                        );
                        let id = thumb.id();
                        app.thumbs.insert(1, thumb, 1);
                        let initial = frame(&mut app, id);
                        let fit = ctx.load_texture(
                            "fit",
                            egui::ColorImage::filled([600, 400], Color32::WHITE),
                            TextureOptions::LINEAR,
                        );
                        let id = fit.id();
                        app.fit.insert(1, fit, 1);
                        let middle = frame(&mut app, id);
                        let full = ctx.load_texture(
                            "full",
                            egui::ColorImage::filled(size, Color32::WHITE),
                            TextureOptions::LINEAR,
                        );
                        let id = full.id();
                        app.full.insert(1, full, 1);
                        let final_rect = frame(&mut app, id);
                        assert_eq!(
                            initial, middle,
                            "thumbnail and fit must use full-image geometry"
                        );
                        assert_eq!(
                            middle, final_rect,
                            "full upload must only change sharpness: {size:?}, orientation={orientation}, ppp={ppp}, compare={compare}"
                        );
                        let expected = if orientation >= 5 {
                            Vec2::new(size[1] as f32, size[0] as f32)
                        } else {
                            Vec2::new(size[0] as f32, size[1] as f32)
                        } / ppp;
                        assert!((final_rect.size() - expected).length() < 0.01);
                    }
                }
            }
        }
    }
    fn key_input(keys: &[Key]) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        for &key in keys {
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        input
    }
    #[test]
    fn rapid_rating_and_navigation_keep_original_event_order() {
        for (keys, ratings) in [
            ([Key::Num5, Key::ArrowRight], [5, 2]),
            ([Key::ArrowRight, Key::Num5], [1, 5]),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let paths: Vec<_> = (0..2)
                .map(|i| dir.path().join(format!("{i}.ARW")))
                .collect();
            for (i, p) in paths.iter().enumerate() {
                xmp::write(
                    p,
                    &Updates {
                        rating: Some(i as u8 + 1),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
            let ctx = egui::Context::default();
            let cc = eframe::CreationContext::_new_kittest(ctx.clone());
            let mut app = App::new(&cc, paths.clone());
            for (i, p) in paths.iter().enumerate() {
                app.photos[i].loaded = true;
                app.photos[i].values = xmp::read(p).unwrap();
            }
            ctx.begin_pass(key_input(&keys));
            app.keyboard(&ctx);
            let _ = ctx.end_pass();
            for (p, rating) in paths.iter().zip(ratings) {
                assert_eq!(xmp::read(p).unwrap().rating, Some(rating), "{keys:?}");
            }
            assert_eq!(app.current, 1);
        }
    }
    #[test]
    fn open_menu_keeps_keys_for_the_menu_instead_of_grading_or_navigating() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("a.ARW"), PathBuf::from("b.ARW")]);
        ctx.begin_pass(key_input(&[Key::ArrowRight, Key::Q]));
        egui::Popup::open_id(&ctx, egui::Id::new("review-menu"));
        app.keyboard(&ctx);
        assert_eq!(app.current, 0);
        assert!(!app.close_dialog);
        assert!(ctx.input(|i| i.key_pressed(Key::ArrowRight)));
        let _ = ctx.end_pass();
    }
    #[test]
    fn filtering_comparison_grading_and_exposure_keep_correct_photo_and_session_fields() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..2)
            .map(|i| dir.path().join(format!("{i}.ARW")))
            .collect();
        let session = Arc::new(Session::new(
            paths
                .iter()
                .map(|p| state::Item {
                    path: p.clone(),
                    updates: Updates {
                        rating: Some(4),
                        ..Default::default()
                    },
                })
                .collect(),
        ));
        for (i, p) in paths.iter().enumerate() {
            session.initialize(i).unwrap();
            xmp::write(
                p,
                &Updates {
                    exposure: Some(100000),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let manifest = dir.path().join("session.txt");
        fastcull::handoff::Manifest::new("combined-test".into(), &paths)
            .unwrap()
            .write(&manifest)
            .unwrap();
        session.attach_manifest(manifest.clone()).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx);
        let mut app = App::new_session(&cc, paths.clone(), Some(session));
        for (i, p) in paths.iter().enumerate() {
            app.photos[i].loaded = true;
            app.photos[i].values = xmp::read(p).unwrap();
            app.photos[i].exposure_base = Some(100000);
        }
        app.current = 1;
        app.compare = Some(Compare {
            indices: [0, 1],
            views: [View {
                zoom: 1.,
                pan: Vec2::new(0.1, 0.2),
            }; 2],
            active: 1,
            linked: false,
        });
        app.filters.selected.insert(
            fastcull::filtering::Field::Rating,
            [Some("4".into())].into(),
        );
        app.adjust_exposure(1);
        app.refilter();
        assert_eq!(
            app.compare.as_ref().unwrap().views[0].pan,
            Vec2::new(0.1, 0.2)
        );
        assert_eq!(xmp::read(&paths[0]).unwrap().exposure, Some(100000));
        assert_eq!(xmp::read(&paths[1]).unwrap().exposure, Some(433333));
        app.rating(5);
        app.refilter();
        assert_eq!(app.visible, [0]);
        assert!(app.compare.is_none());
        assert_eq!(app.current, 0);
        let rows = fastcull::handoff::Manifest::read(&manifest)
            .unwrap()
            .rows_since(0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, paths[1]);
        assert_eq!(rows[0].1.as_ref().unwrap().rating, Some(5));
        assert_eq!(rows[0].1.as_ref().unwrap().exposure, Some(433333));
        app.filters.selected.insert(
            fastcull::filtering::Field::Rating,
            [Some("3".into())].into(),
        );
        app.refilter();
        app.rating(1);
        app.adjust_exposure(3);
        assert_eq!(xmp::read(&paths[0]).unwrap().rating, Some(4));
        assert_eq!(xmp::read(&paths[0]).unwrap().exposure, Some(100000));
    }
    #[test]
    fn selecting_other_compared_photo_activates_it_without_replacing_either() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx);
        let mut app = App::new(&cc, vec![PathBuf::from("a.ARW"), PathBuf::from("b.ARW")]);
        app.current = 1;
        app.compare = Some(Compare {
            indices: [0, 1],
            views: [View::default(); 2],
            active: 1,
            linked: false,
        });
        app.go(0);
        assert_eq!(app.current, 0);
        assert_eq!(app.compare.as_ref().unwrap().active, 0);
        assert_eq!(app.compare.as_ref().unwrap().indices, [0, 1]);
    }
    #[test]
    fn relative_pan_stops_both_at_first_edge_and_preserves_offset() {
        let mut c = Compare {
            indices: [0, 1],
            active: 1,
            linked: true,
            views: [
                View {
                    zoom: 1.,
                    pan: Vec2::new(0.29, -0.1),
                },
                View {
                    zoom: 2.,
                    pan: Vec2::new(-0.1, 0.08),
                },
            ],
        };
        let offset = c.views[0].pan - c.views[1].pan;
        c.translate(
            1,
            Vec2::new(0.1, 0.03),
            [Vec2::splat(0.3), Vec2::splat(0.4)],
        );
        assert!((c.views[0].pan.x - 0.3).abs() < 0.00001);
        assert!((c.views[1].pan.x + 0.09).abs() < 0.00001);
        assert!((c.views[0].pan - c.views[1].pan - offset).length() < 0.00001);
        c.translate(
            0,
            Vec2::new(-0.02, 0.),
            [Vec2::splat(0.3), Vec2::splat(0.4)],
        );
        assert!((c.views[0].pan.x - 0.28).abs() < 0.00001);
        assert!((c.views[0].pan - c.views[1].pan - offset).length() < 0.00001);
    }
    #[test]
    fn linked_zoom_limits_keep_ratio_and_fit_keeps_subject_anchors() {
        let mut c = Compare {
            indices: [0, 1],
            active: 0,
            linked: true,
            views: [
                View {
                    zoom: 2.,
                    pan: Vec2::new(0.1, 0.2),
                },
                View {
                    zoom: 4.,
                    pan: Vec2::new(-0.05, 0.08),
                },
            ],
        };
        c.scale(0, 8., [0.2, 0.4]);
        assert_eq!(c.views[0].zoom, 4.);
        assert_eq!(c.views[1].zoom, 8.);
        let anchors = c.views.map(|v| v.pan);
        c.scale(0, 0., [0.2, 0.4]);
        assert_eq!(c.views.map(|v| v.zoom), [0., 0.]);
        c.scale(0, 1., [0.2, 0.4]);
        assert_eq!(c.views.map(|v| v.zoom), [1., 2.]);
        assert_eq!(c.views.map(|v| v.pan), anchors);
    }
    #[test]
    fn linked_navigation_does_not_reset_reference_alignment() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx);
        let mut app = App::new(
            &cc,
            (0..3).map(|i| PathBuf::from(format!("{i}.ARW"))).collect(),
        );
        app.current = 1;
        app.compare = Some(Compare {
            indices: [0, 1],
            active: 1,
            linked: true,
            views: [
                View {
                    zoom: 2.,
                    pan: Vec2::new(0.12, -0.08),
                },
                View {
                    zoom: 1.,
                    pan: Vec2::new(-0.06, 0.15),
                },
            ],
        });
        app.go(2);
        let c = app.compare.as_ref().unwrap();
        assert_eq!(c.indices, [0, 2]);
        assert_eq!(c.views[0].zoom, 2.);
        assert_eq!(c.views[0].pan, Vec2::new(0.12, -0.08));
        assert_eq!(c.views[1].pan, Vec2::new(-0.06, 0.15));
    }
    #[test]
    fn linked_zoom_preserves_separately_aligned_subjects_and_scale_ratio() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx);
        let mut app = App::new(&cc, vec![PathBuf::from("a.ARW"), PathBuf::from("b.ARW")]);
        app.compare = Some(Compare {
            indices: [0, 1],
            views: [
                View {
                    zoom: 0.5,
                    pan: Vec2::new(0.12, -0.08),
                },
                View {
                    zoom: 1.,
                    pan: Vec2::new(-0.06, 0.15),
                },
            ],
            active: 0,
            linked: true,
        });
        app.set_zoom(true);
        let c = app.compare.as_ref().unwrap();
        assert_eq!(c.views[0].zoom, 1.);
        assert_eq!(
            c.views[1].zoom, 2.,
            "linked zoom must apply a ratio, not copy absolute scale"
        );
        assert_eq!(c.views[0].pan, Vec2::new(0.12, -0.08));
        assert_eq!(c.views[1].pan, Vec2::new(-0.06, 0.15));
    }
    #[test]
    fn comparison_keyboard_pan_moves_active_view_and_respects_linking() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("a.ARW"), PathBuf::from("b.ARW")]);
        app.current = 1;
        app.zoom = true;
        app.compare = Some(Compare {
            indices: [0, 1],
            views: [
                View {
                    zoom: 1.,
                    pan: Vec2::new(0.12, -0.08),
                },
                View {
                    zoom: 1.,
                    pan: Vec2::new(-0.06, 0.15),
                },
            ],
            active: 1,
            linked: true,
        });
        app.full.insert(
            1,
            ctx.load_texture(
                "pan",
                egui::ColorImage::filled([100, 100], Color32::WHITE),
                TextureOptions::LINEAR,
            ),
            1,
        );
        let reference = app.full.items.get(&1).unwrap().handle.clone();
        app.full.insert(0, reference, 1);
        let offset =
            app.compare.as_ref().unwrap().views[0].pan - app.compare.as_ref().unwrap().views[1].pan;
        ctx.begin_pass(key_input(&[Key::ArrowUp]));
        app.keyboard(&ctx);
        let _ = ctx.end_pass();
        let c = app.compare.as_ref().unwrap();
        assert!(c.views[1].pan.y > 0.);
        assert!((c.views[0].pan - c.views[1].pan - offset).length() < 0.00001);
        let previous = c.views[0].pan;
        app.compare.as_mut().unwrap().linked = false;
        ctx.begin_pass(key_input(&[Key::ArrowDown]));
        app.keyboard(&ctx);
        let _ = ctx.end_pass();
        assert_eq!(app.compare.as_ref().unwrap().views[0].pan, previous);
        assert!(app.compare.as_ref().unwrap().views[1].pan.y < previous.y);
    }
    #[test]
    fn exposure_thirds_accumulate_to_a_stop_and_keep_other_develop_settings() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("exposure.ARW");
        std::fs::write(raw.with_extension("xmp"),r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Contrast2012="23"/>"#).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx);
        let mut app = App::new(&cc, vec![raw.clone()]);
        app.photos[0].loaded = true;
        app.photos[0].exposure_base = Some(100000);
        app.photos[0].values.exposure = Some(100000);
        for target in [433333, 766667, 1100000] {
            app.adjust_exposure(1);
            assert_eq!(xmp::read(&raw).unwrap().exposure, Some(target));
        }
        app.adjust_exposure(-3);
        assert_eq!(xmp::read(&raw).unwrap().exposure, Some(100000));
        for _ in 0..8 {
            app.adjust_exposure(3);
        }
        assert_eq!(xmp::read(&raw).unwrap().exposure, Some(5_000_000));
        app.adjust_exposure(-3);
        assert_eq!(
            xmp::read(&raw).unwrap().exposure,
            Some(4_000_000),
            "{:?}",
            app.error
        );
        assert!(
            std::fs::read_to_string(raw.with_extension("xmp"))
                .unwrap()
                .contains("crs:Contrast2012=\"23\"")
        );
    }
    #[test]
    fn comparing_keeps_two_distinct_photos_and_keyboard_navigation_skips_reference() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(
            &cc,
            (0..4)
                .map(|i| PathBuf::from(format!("missing-{i}.ARW")))
                .collect(),
        );
        app.current = 2;
        app.zoom = true;
        app.compare = Some(Compare {
            indices: [1, 2],
            views: [
                View {
                    zoom: 1.,
                    pan: Vec2::new(0.1, 0.2),
                },
                View {
                    zoom: 2.,
                    pan: Vec2::ZERO,
                },
            ],
            active: 1,
            linked: false,
        });
        app.next(-1);
        assert_eq!(app.current, 0);
        assert_eq!(app.compare.as_ref().unwrap().indices, [1, 0]);
        assert_eq!(
            app.compare.as_ref().unwrap().views[0].pan,
            Vec2::new(0.1, 0.2)
        );
        app.activate_compare(0);
        app.set_zoom(false);
        assert_eq!(app.compare.as_ref().unwrap().views[1].zoom, 2.);
        app.compare.as_mut().unwrap().linked = true;
        app.set_zoom(true);
        assert_eq!(app.compare.as_ref().unwrap().views[1].zoom, 2.);
        // Editing a filter cannot turn numeric/star keys into grading actions.
        ctx.begin_pass(egui::RawInput::default());
        ctx.memory_mut(|m| m.request_focus(egui::Id::new(("fastcull-filter-input", "query"))));
        ctx.input_mut(|i| {
            i.events.push(egui::Event::Key {
                key: Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
        });
        app.keyboard(&ctx);
        assert_eq!(app.current, 1);
        let _ = ctx.end_pass();
    }
    #[test]
    fn close_keyboard_opens_confirmation_without_writing_a_sync_request() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("missing.ARW")]);
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: Key::Q,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        ctx.begin_pass(input);
        app.keyboard(&ctx);
        assert!(app.close_dialog);
        assert!(!app.allow_close);
        let _ = ctx.end_pass();
    }
    #[test]
    fn comparison_cache_retains_both_large_images_when_neighbors_arrive() {
        let ctx = egui::Context::default();
        let mut cache = TextureCache::new(2 * 64 * 64 * 4, 4);
        cache.pinned = Some(1);
        for i in 0..8 {
            cache.insert_near(
                i,
                ctx.load_texture(
                    format!("compare{i}"),
                    egui::ColorImage::filled([64, 64], Color32::WHITE),
                    TextureOptions::LINEAR,
                ),
                0,
            );
        }
        assert_eq!(cache.items.len(), 2);
        assert!(cache.items.contains_key(&0));
        assert!(cache.items.contains_key(&1));
    }
    #[test]
    fn fit_canvas_does_not_request_full_synchronously_and_thumbnail_error_is_independent() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("missing.ARW")]);
        app.photos[0]
            .image_errors
            .insert(Level::Thumb, "bad thumbnail".into());
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.central(ui, ctx));
        });
        assert!(app.pool.requested(0, Level::Fit(app.fit_edge)));
        assert!(!app.pool.requested(0, Level::Full));
        app.zoom = true;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.central(ui, ctx));
        });
        assert!(app.pool.requested(0, Level::Full));
    }
    #[test]
    fn arrows_select_photos_even_at_one_hundred_percent() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("a.ARW"), PathBuf::from("b.ARW")]);
        app.zoom = true;
        app.pan = Vec2::new(0.1, 0.2);
        for (key, expected) in [(Key::ArrowRight, 1), (Key::ArrowLeft, 0)] {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            ctx.begin_pass(input);
            app.keyboard(&ctx);
            assert_eq!(
                app.current, expected,
                "100% arrow navigation changed pan instead of photo"
            );
            assert!(app.zoom);
            assert_eq!(app.pan, Vec2::new(0.1, 0.2));
            let _ = ctx.end_pass();
        }
    }
    #[test]
    fn zoom_navigation_keeps_relative_anchor_across_sizes_and_orientations() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, vec![PathBuf::from("a.jpg"), PathBuf::from("b.jpg")]);
        for (i, size) in [[600, 400], [800, 1200]].into_iter().enumerate() {
            let texture = ctx.load_texture(
                format!("anchor-{i}"),
                egui::ColorImage::filled(size, Color32::WHITE),
                TextureOptions::LINEAR,
            );
            app.full.insert(i, texture, 0);
        }
        app.photos[1].values.orientation = Some(6);
        app.zoom = true;
        app.canvas_size = Vec2::splat(100.);
        app.move_view(Vec2::new(30., -20.), 2.);
        let anchor = Vec2::new(0.1, -0.1);
        assert_eq!(app.pan, anchor);
        app.next(1);
        assert_eq!(app.pan, anchor);
        // Rotated second photo displays as 600 x 400 logical pixels at 200% DPI.
        app.move_view(Vec2::new(60., -40.), 2.);
        assert_eq!(app.pan, anchor * 2.);
        app.next(-1);
        assert_eq!(app.pan, anchor * 2.);
        app.compare = Some(Compare {
            indices: [0, 1],
            views: [View {
                zoom: 1.,
                pan: anchor,
            }; 2],
            active: 0,
            linked: true,
        });
        // Direct selection of the reference activates it without resetting either view.
        app.go(1);
        assert_eq!(
            app.compare.as_ref().unwrap().views.map(|v| v.pan),
            [anchor; 2]
        );
    }
    #[test]
    fn rotation_returns_to_original() {
        for base in 1..=8 {
            let mut o = base;
            for _ in 0..4 {
                o = clockwise(o);
            }
            assert_eq!(base, o);
        }
    }
    #[test]
    fn toolbar_focus_does_not_steal_or_duplicate_navigation() {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        ctx.begin_pass(input);
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("toolbar-button")));
        assert!(ctx.wants_keyboard_input());
        assert!(shortcut(&ctx, Key::Space));
        assert!(!shortcut(&ctx, Key::Space));
        assert!(!ctx.input(|i| i.key_pressed(Key::Space)));
        let _ = ctx.end_pass();
    }
    #[test]
    fn texture_budget_protects_current() {
        let ctx = egui::Context::default();
        let mut c = TextureCache::new(2 * 64 * 64 * 4, 4);
        for i in 0..8 {
            c.insert(
                i,
                ctx.load_texture(
                    format!("test{i}"),
                    egui::ColorImage::filled([64, 64], Color32::WHITE),
                    TextureOptions::LINEAR,
                ),
                0,
            );
        }
        assert!(c.items.contains_key(&0));
        assert!(c.items.len() <= 2);
    }
    #[test]
    fn far_prefetch_cannot_evict_the_next_photo_on_a_small_preview_budget() {
        let ctx = egui::Context::default();
        let mut c = TextureCache::new(2 * 64 * 64 * 4, 4);
        for i in 0..3 {
            c.insert_near(
                i,
                ctx.load_texture(
                    format!("preview{i}"),
                    egui::ColorImage::filled([64, 64], Color32::WHITE),
                    TextureOptions::LINEAR,
                ),
                0,
            );
        }
        assert!(c.items.contains_key(&0));
        assert!(c.items.contains_key(&1));
        assert!(!c.items.contains_key(&2));
    }
}
