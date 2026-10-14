#[allow(dead_code)]
#[path = "../src/filter_ui.rs"]
mod filter_ui;
use eframe::egui;
use fastcull::{
    arw::CaptureMetadata,
    filtering::{Filters, Row},
    xmp::Values,
};
use std::{path::Path, time::Instant};
fn main() {
    let ctx = egui::Context::default();
    let mut editor = filter_ui::Editor::opened();
    let mut filters = Filters::default();
    let values = Values::default();
    let capture = CaptureMetadata {
        iso: Some(100),
        camera: Some("Camera".into()),
        lens: Some("Lens".into()),
        ..Default::default()
    };
    let rows: Vec<_> = (0..10000)
        .map(|_| Row {
            path: Path::new("photo.ARW"),
            values: &values,
            capture: Some(&capture),
            indexed: true,
        })
        .collect();
    let mut times = Vec::new();
    for i in 0..110 {
        let start = Instant::now();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                editor.show(ui, &mut filters, &rows);
            });
        });
        if i >= 10 {
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "10000 rows, default sidebar, median_ms={:.3} p95_ms={:.3}",
        times[50], times[95]
    );
}
