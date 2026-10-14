use eframe::egui;
use fastcull::filtering::{Bounds, Comparison, Field, Filters, Row, time_key};
use std::collections::BTreeMap;
type CompiledRanges = (Option<u64>, Option<u64>, BTreeMap<Field, Bounds>);

#[derive(Default)]
pub struct Editor {
    pub open: bool,
    query: String,
    from: String,
    to: String,
    ranges: BTreeMap<Field, (String, String)>,
    error: Option<String>,
    counts: BTreeMap<Field, BTreeMap<Option<String>, usize>>,
}
fn id(name: &str) -> egui::Id {
    egui::Id::new(("fastcull-filter-input", name))
}
impl Editor {
    /// Called when photo metadata, folder scope or filter conditions change.
    pub fn invalidate(&mut self) {
        self.counts.clear();
    }
    pub fn opened() -> Self {
        Self {
            open: true,
            ..Default::default()
        }
    }
    pub fn editing(&self, ctx: &egui::Context) -> bool {
        let focused = ctx.memory(|m| m.focused());
        [
            "query",
            "from",
            "to",
            "iso-min",
            "iso-max",
            "aperture-min",
            "aperture-max",
            "shutter-min",
            "shutter-max",
            "focal-min",
            "focal-max",
        ]
        .iter()
        .any(|name| focused == Some(id(name)))
    }
    pub fn clear(&mut self, filters: &mut Filters) {
        let open = self.open;
        *self = Self {
            open,
            ..Default::default()
        };
        *filters = Filters::default();
    }
    pub fn show(&mut self, ui: &mut egui::Ui, filters: &mut Filters, rows: &[Row<'_>]) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("照片筛选").size(15.).strong())
                .on_hover_text("同一组多选：或；不同组之间：且");
            if ui
                .add_enabled(filters.active(), egui::Button::new("重置").frame(false))
                .clicked()
            {
                self.clear(filters);
                changed = true;
            }
        });
        let indexed = rows.iter().filter(|row| row.indexed).count();
        if indexed < rows.len() {
            ui.weak(format!("读取拍摄信息 {indexed}/{}", rows.len()));
        }
        ui.separator();
        if ui
            .add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("搜索文件名…")
                    .id(id("query"))
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            filters.query = self.query.clone();
            changed = true;
        }
        ui.collapsing("拍摄时间范围", |ui| {
            ui.label("YYYY-MM-DD，可附 HH:mm:ss");
            let edited = ui
                .add(
                    egui::TextEdit::singleline(&mut self.from)
                        .id(id("from"))
                        .hint_text("开始日期"),
                )
                .changed()
                | ui.add(
                    egui::TextEdit::singleline(&mut self.to)
                        .id(id("to"))
                        .hint_text("结束日期（含当天）"),
                )
                .changed();
            if edited {
                match self.compile_ranges() {
                    Ok((from, to, ranges)) => {
                        filters.date_from = from;
                        filters.date_to = to;
                        filters.ranges = ranges;
                        self.error = None;
                        changed = true;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
            ui.label("输入无效时保留上一次有效条件");
        }
        for field in Field::ALL {
            ui.add_space(3.);
            egui::CollapsingHeader::new(title(field))
                .default_open(field == Field::Rating)
                .show(ui, |ui| {
                    if changed {
                        self.invalidate();
                    }
                    // Do not traverse the catalogue for collapsed groups or
                    // unchanged repaint frames (pan, hover, loading animation).
                    let counts = self
                        .counts
                        .entry(field)
                        .or_insert_with(|| filters.facets(rows, field))
                        .clone();
                    if field == Field::Rating {
                        let before = filters.comparison;
                        egui::ComboBox::from_id_salt("rating-comparison")
                            .selected_text(match before {
                                Comparison::Equal => "等于（多选）",
                                Comparison::AtLeast => "大于等于",
                                Comparison::AtMost => "小于等于",
                            })
                            .show_ui(ui, |ui| {
                                for (value, text) in [
                                    (Comparison::Equal, "等于（多选）"),
                                    (Comparison::AtLeast, "大于等于"),
                                    (Comparison::AtMost, "小于等于"),
                                ] {
                                    ui.selectable_value(&mut filters.comparison, value, text);
                                }
                            });
                        changed |= before != filters.comparison;
                        if before != filters.comparison
                            && filters.comparison != Comparison::Equal
                            && let Some(selected) = filters.selected.get_mut(&Field::Rating)
                            && selected.len() > 1
                        {
                            let threshold = selected.last().cloned().unwrap();
                            selected.clear();
                            selected.insert(threshold);
                        }
                    }
                    if field.numeric() {
                        let (names, unit) = match field {
                            Field::Iso => (["iso-min", "iso-max"], "ISO"),
                            Field::Aperture => (["aperture-min", "aperture-max"], "f 值"),
                            Field::Shutter => (["shutter-min", "shutter-max"], "秒，支持 1/250"),
                            _ => (["focal-min", "focal-max"], "mm"),
                        };
                        ui.label(format!("范围（{unit}）"));
                        let range = self.ranges.entry(field).or_default();
                        let mut edited = false;
                        ui.horizontal(|ui| {
                            edited |= ui
                                .add(
                                    egui::TextEdit::singleline(&mut range.0)
                                        .id(id(names[0]))
                                        .hint_text("最小")
                                        .desired_width(86.),
                                )
                                .changed();
                            edited |= ui
                                .add(
                                    egui::TextEdit::singleline(&mut range.1)
                                        .id(id(names[1]))
                                        .hint_text("最大")
                                        .desired_width(86.),
                                )
                                .changed();
                        });
                        if edited {
                            match self.compile_ranges() {
                                Ok((from, to, ranges)) => {
                                    filters.date_from = from;
                                    filters.date_to = to;
                                    filters.ranges = ranges;
                                    self.error = None;
                                    changed = true;
                                }
                                Err(error) => self.error = Some(error),
                            }
                        }
                    }
                    let mut entries: Vec<_> = counts.into_iter().collect();
                    if field.numeric() {
                        entries.sort_by(|a, b| {
                            a.0.as_ref()
                                .and_then(|s| s.parse::<f64>().ok())
                                .partial_cmp(&b.0.as_ref().and_then(|s| s.parse::<f64>().ok()))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                    }
                    egui::ScrollArea::vertical()
                        .id_salt(("facet", field as u8))
                        .max_height(if field == Field::Rating { 220. } else { 180. })
                        .show(ui, |ui| {
                            if entries.is_empty() {
                                ui.weak("尚无可用信息");
                            }
                            for (key, count) in entries {
                                let selected = filters.selected.entry(field).or_default();
                                let mut checked = selected.contains(&key);
                                let response = ui
                                    .horizontal(|ui| {
                                        let response = ui.checkbox(
                                            &mut checked,
                                            value_label(field, key.as_deref()),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.weak(count.to_string());
                                            },
                                        );
                                        response
                                    })
                                    .inner;
                                if response.changed() {
                                    if checked {
                                        if field == Field::Rating
                                            && filters.comparison != Comparison::Equal
                                        {
                                            selected.clear();
                                        }
                                        selected.insert(key);
                                    } else {
                                        selected.remove(&key);
                                    }
                                    changed = true;
                                }
                            }
                        });
                });
        }
        if changed {
            self.invalidate();
        }
        changed
    }
    fn compile_ranges(&self) -> Result<CompiledRanges, String> {
        let date = |value: &str, end| {
            if value.trim().is_empty() {
                Ok(None)
            } else {
                time_key(value, end)
                    .map(Some)
                    .ok_or_else(|| "拍摄日期格式无效".to_owned())
            }
        };
        let from = date(&self.from, false)?;
        let to = date(&self.to, true)?;
        if from.zip(to).is_some_and(|(a, b)| a > b) {
            return Err("开始日期不能晚于结束日期".into());
        }
        let mut ranges = BTreeMap::new();
        for (&field, (min, max)) in &self.ranges {
            let parse = |text: &str| -> Result<Option<f64>, String> {
                if text.trim().is_empty() {
                    return Ok(None);
                }
                let value = if field == Field::Shutter && text.contains('/') {
                    let (a, b) = text.trim().split_once('/').unwrap();
                    a.parse::<f64>()
                        .ok()
                        .zip(b.parse::<f64>().ok())
                        .map(|(a, b)| a / b)
                } else {
                    text.trim().parse::<f64>().ok()
                };
                value
                    .filter(|v| v.is_finite() && *v > 0.)
                    .map(Some)
                    .ok_or_else(|| format!("{}范围必须为正数", title(field)))
            };
            let range = Bounds {
                min: parse(min)?,
                max: parse(max)?,
            };
            if range.min.zip(range.max).is_some_and(|(a, b)| a > b) {
                return Err(format!("{}最小值不能大于最大值", title(field)));
            }
            if range.min.is_some() || range.max.is_some() {
                ranges.insert(field, range);
            }
        }
        Ok((from, to, ranges))
    }
}
fn title(field: Field) -> &'static str {
    match field {
        Field::Rating => "星级",
        Field::Flag => "旗标",
        Field::Label => "颜色标签",
        Field::Day => "拍摄日期",
        Field::Camera => "相机",
        Field::Lens => "镜头",
        Field::Iso => "ISO",
        Field::Aperture => "光圈",
        Field::Shutter => "快门",
        Field::Focal => "焦距",
    }
}
fn value_label(field: Field, key: Option<&str>) -> String {
    let Some(key) = key else {
        return match field {
            Field::Label => "无颜色标签",
            Field::Flag => "未标记",
            _ => "未知",
        }
        .into();
    };
    match field {
        Field::Rating => {
            if key == "0" {
                "未评级 (0)".into()
            } else {
                format!("{} 星", key)
            }
        }
        Field::Flag => match key {
            "pick" => "拾取".into(),
            "reject" => "拒绝".into(),
            _ => key.into(),
        },
        Field::Label => match key {
            "red" => "红色".into(),
            "yellow" => "黄色".into(),
            "green" => "绿色".into(),
            "blue" => "蓝色".into(),
            "purple" => "紫色".into(),
            _ => key.into(),
        },
        Field::Aperture => format!("f/{key}"),
        Field::Focal => format!("{key} mm"),
        Field::Shutter => key
            .parse::<f64>()
            .map(|v| {
                if v < 1. {
                    format!("1/{:.0} 秒", 1. / v)
                } else {
                    format!("{v} 秒")
                }
            })
            .unwrap_or_else(|_| key.into()),
        _ => key.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collapsed_facets_are_lazy_and_metadata_changes_refresh_counts() {
        let ctx = egui::Context::default();
        let mut editor = Editor::opened();
        let mut filters = Filters::default();
        let mut values = fastcull::xmp::Values::default();
        let render =
            |editor: &mut Editor, filters: &mut Filters, values: &fastcull::xmp::Values| {
                let rows = [Row {
                    path: std::path::Path::new("photo.ARW"),
                    values,
                    capture: None,
                    indexed: false,
                }];
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        editor.show(ui, filters, &rows);
                    });
                });
            };
        render(&mut editor, &mut filters, &values);
        assert_eq!(editor.counts.len(), 1);
        assert_eq!(editor.counts[&Field::Rating][&Some("0".into())], 1);
        values.rating = Some(5);
        editor.invalidate();
        render(&mut editor, &mut filters, &values);
        assert_eq!(editor.counts[&Field::Rating][&Some("0".into())], 0);
        assert_eq!(editor.counts[&Field::Rating][&Some("5".into())], 1);
    }
}
