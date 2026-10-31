//! Bounded sampling of the displayed sRGB preview, including the GPU adjustments.
use crate::raw_preview::{LinearImage, Recipe, Settings, evaluate_scene};
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};

pub struct Histogram {
    samples: Vec<[f32; 3]>,
    settings: Option<Settings>,
    bins: [[u32; 256]; 4],
    recipe: Option<Recipe>,
}
impl Histogram {
    pub fn from_image(image: &egui::ColorImage) -> Self {
        let [w, h] = image.size;
        let mut samples = Vec::new();
        if w > 0 && h > 0 && w.checked_mul(h) == Some(image.pixels.len()) {
            let (nx, ny) = (w.min(128), h.min(128));
            for y in 0..ny {
                for x in 0..nx {
                    let sx = (2 * x + 1) * w / (2 * nx);
                    let sy = (2 * y + 1) * h / (2 * ny);
                    let p = image.pixels[sy * w + sx].to_array();
                    if p[3] == 255 {
                        samples.push([p[0], p[1], p[2]].map(|v| v as f32 / 255.));
                    }
                }
            }
        }
        Self {
            samples,
            settings: None,
            bins: [[0; 256]; 4],
            recipe: None,
        }
    }
    pub fn from_linear(image: &LinearImage) -> Self {
        let [w, h] = image.size;
        let mut samples = Vec::new();
        if w > 0 && h > 0 && w.checked_mul(h) == Some(image.pixels.len()) {
            let (nx, ny) = (w.min(128), h.min(128));
            for y in 0..ny {
                for x in 0..nx {
                    let sx = (2 * x + 1) * w / (2 * nx);
                    let sy = (2 * y + 1) * h / (2 * ny);
                    let p = image.pixels[sy * w + sx];
                    if p[..3].iter().all(|v| v.is_finite()) {
                        samples.push([p[0], p[1], p[2]]);
                    }
                }
            }
        }
        Self {
            samples,
            settings: None,
            bins: [[0; 256]; 4],
            recipe: Some(image.recipe.clone()),
        }
    }
    fn update(&mut self, settings: Settings) {
        if self.settings == Some(settings) {
            return;
        }
        self.bins = [[0; 256]; 4];
        for &sample in &self.samples {
            let rgb = self.recipe.as_ref().map_or_else(
                || adjusted(sample, settings),
                |recipe| evaluate_scene(sample, settings, recipe),
            );
            for (c, value) in rgb.into_iter().enumerate() {
                self.bins[c][(value * 255.).round() as usize] += 1;
            }
            let y = luminance(rgb);
            self.bins[3][(y * 255.).round().clamp(0., 255.) as usize] += 1;
        }
        self.settings = Some(settings);
    }
    pub fn show(&mut self, ui: &mut egui::Ui, settings: Settings) {
        self.update(settings);
        ui.horizontal(|ui| {
            ui.strong("直方图");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak("RGB · sRGB");
            });
        });
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 124.), egui::Sense::hover());
        response.on_hover_text("当前本地显影预览的 RGB / 亮度分布（采样）；随调整更新。左右三角表示通道存在暗部 / 高光剪切。");
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4., Color32::from_gray(19));
        let plot = rect.shrink2(Vec2::new(8., 12.));
        for i in 1..4 {
            let x = egui::lerp(plot.x_range(), i as f32 / 4.);
            painter.line_segment(
                [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                Stroke::new(1., Color32::from_gray(40)),
            );
        }
        let peak = self
            .bins
            .iter()
            .flatten()
            .copied()
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        for (channel, color) in [
            (3, Color32::from_rgba_unmultiplied(190, 190, 190, 70)),
            (0, Color32::from_rgba_unmultiplied(240, 85, 85, 100)),
            (1, Color32::from_rgba_unmultiplied(75, 215, 130, 100)),
            (2, Color32::from_rgba_unmultiplied(85, 145, 245, 110)),
        ] {
            let mut mesh = egui::Mesh::default();
            let mut line = Vec::with_capacity(256);
            for (i, count) in self.bins[channel].iter().enumerate() {
                let x = egui::lerp(plot.x_range(), i as f32 / 255.);
                let y = plot.bottom() - (*count as f32 / peak).sqrt() * plot.height();
                let top = Pos2::new(x, y);
                line.push(top);
                mesh.colored_vertex(Pos2::new(x, plot.bottom()), color);
                mesh.colored_vertex(top, color);
                if i > 0 {
                    let n = (i * 2) as u32;
                    mesh.add_triangle(n - 2, n - 1, n);
                    mesh.add_triangle(n - 1, n + 1, n);
                }
            }
            painter.add(egui::Shape::mesh(mesh));
            painter.add(egui::Shape::line(line, Stroke::new(1., color.to_opaque())));
        }
        for (right, bin) in [(false, 0), (true, 255)] {
            let color = Color32::from_rgb(
                if self.bins[0][bin] > 0 { 230 } else { 65 },
                if self.bins[1][bin] > 0 { 230 } else { 65 },
                if self.bins[2][bin] > 0 { 230 } else { 65 },
            );
            let x = if right {
                rect.right() - 12.
            } else {
                rect.left() + 12.
            };
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(x, rect.top() + 3.),
                    Pos2::new(x - 4., rect.top() + 9.),
                    Pos2::new(x + 4., rect.top() + 9.),
                ],
                color,
                Stroke::NONE,
            ));
        }
        ui.horizontal(|ui| {
            ui.weak("暗部");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak("高光");
            });
        });
    }
}
fn luminance(rgb: [f32; 3]) -> f32 {
    rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722
}
// Keep these equations in step with raw_preview's fragment shader. Native RAW
// exposure/WB is already baked into the sample; Settings contains only its delta.
fn adjusted(rgb: [f32; 3], settings: Settings) -> [f32; 3] {
    let gain = 2f32.powf(settings.exposure);
    let mut out = std::array::from_fn(|c| {
        let s = rgb[c];
        let linear = if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        };
        let v = (linear * gain * settings.wb[c]).clamp(0., 1.);
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1. / 2.4) - 0.055
        }
    });
    let y = luminance(out);
    let b = settings.basic;
    let lift = b[2] * (1. - y).powi(2) * 0.35
        + b[1] * y * y * 0.35
        + b[3] * y.powi(4) * 0.25
        + b[4] * (1. - y).powi(4) * 0.25;
    out = out.map(|v| ((v - 0.5) * (1. + b[0] * 0.8) + 0.5 + lift).clamp(0., 1.));
    let y = luminance(out);
    let saturation = out.into_iter().fold(f32::NEG_INFINITY, f32::max)
        - out.into_iter().fold(f32::INFINITY, f32::min);
    let gain = (1. + b[5] * (1. - saturation)).max(0.) * (1. + b[6]).max(0.);
    out.map(|v| (y + (v - y) * gain).clamp(0., 1.))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn float_histogram_keeps_headroom_until_the_display_transform() {
        let image = LinearImage {
            size: [1025, 769],
            pixels: vec![[2., 2., 2., 1.]; 1025 * 769],
            recipe: Recipe::default(),
        };
        let mut histogram = Histogram::from_linear(&image);
        assert_eq!(histogram.samples.len(), 128 * 128);
        assert_eq!(histogram.samples[0], [2.; 3]);
        histogram.update(Settings::default());
        assert_eq!(histogram.bins[0][255], 128 * 128);
        histogram.update(Settings {
            exposure: -2.,
            ..Default::default()
        });
        assert_eq!(histogram.bins[0][188], 128 * 128);
        assert_eq!(histogram.bins[0][255], 0);
        assert_eq!(histogram.bins[0], histogram.bins[1]);
    }
    #[test]
    fn bins_track_exposure_color_and_clipping_without_sampling_out_of_bounds() {
        let mut image = egui::ColorImage::filled([3, 1], Color32::BLACK);
        image.pixels[1] = Color32::from_rgb(128, 64, 32);
        image.pixels[2] = Color32::WHITE;
        let mut histogram = Histogram::from_image(&image);
        histogram.update(Settings::default());
        assert_eq!(histogram.bins[0][0], 1);
        assert_eq!(histogram.bins[2][255], 1);
        assert_eq!(histogram.bins[0][128], 1);
        let before = histogram.bins;
        let mut settings = Settings {
            exposure: 1.,
            ..Default::default()
        };
        histogram.update(settings);
        assert_ne!(histogram.bins, before);
        settings.basic[6] = -1.;
        histogram.update(settings);
        assert_eq!(histogram.bins[0], histogram.bins[1]);
        assert_eq!(histogram.bins[1], histogram.bins[2]);
        assert!(
            histogram
                .bins
                .iter()
                .all(|channel| channel.iter().sum::<u32>() == 3)
        );
        let huge = egui::ColorImage::filled([1025, 769], Color32::WHITE);
        assert_eq!(Histogram::from_image(&huge).samples.len(), 128 * 128);
    }
}
