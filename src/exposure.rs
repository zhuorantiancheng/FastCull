//! Nonblocking LR develop baseline mailbox. Failed/missing settings stay disabled.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
pub struct Baselines {
    stop: Arc<AtomicBool>,
    values: Arc<Mutex<HashMap<PathBuf, Baseline>>>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Baseline {
    pub exposure: Option<i32>,
    pub crop: Option<fastcull::crop::Crop>,
    pub orientation: Option<u16>,
    pub white_balance: Option<fastcull::xmp::WhiteBalance>,
}
fn parse_row(line: &str) -> Option<(PathBuf, Baseline)> {
    let fields: Vec<_> = line.trim_end_matches(['\r', '\n']).split('\t').collect();
    if !matches!(fields.len(), 2 | 9 | 12) {
        return None;
    }
    let path = PathBuf::from(fastcull::handoff::decode(fields[0]).ok()?);
    let mut value = Baseline {
        exposure: fields[1]
            .parse::<i32>()
            .ok()
            .filter(|v| (-5_000_000..=5_000_000).contains(v)),
        ..Default::default()
    };
    if fields.len() >= 9 {
        value.orientation = fields[2]
            .parse::<u16>()
            .ok()
            .filter(|v| (1..=8).contains(v));
        value.crop = match fields[3] {
            "0" => Some(fastcull::crop::Crop::default()),
            "1" => (|| {
                let crop = fastcull::crop::Crop {
                    enabled: true,
                    left: fields[4].parse().ok()?,
                    top: fields[5].parse().ok()?,
                    right: fields[6].parse().ok()?,
                    bottom: fields[7].parse().ok()?,
                    angle: fields[8].parse().ok()?,
                };
                crop.valid().then_some(crop)
            })(),
            _ => None,
        };
    }
    if fields.len() == 12 {
        let mode = fastcull::handoff::decode(fields[9]).ok();
        value.white_balance = match mode.as_deref() {
            Some("As Shot") => Some(fastcull::xmp::WhiteBalance::AsShot),
            Some("Custom") => fields[10]
                .parse()
                .ok()
                .zip(fields[11].parse().ok())
                .map(|(temperature, tint)| fastcull::xmp::WhiteBalance::Custom {
                    temperature,
                    tint,
                })
                .filter(|wb| wb.valid()),
            _ => None,
        };
    }
    Some((path, value))
}
impl Baselines {
    pub fn new(path: Option<PathBuf>, ctx: eframe::egui::Context) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let values = Arc::new(Mutex::new(HashMap::new()));
        if let Some(path) = path {
            let (stop, values) = (stop.clone(), values.clone());
            thread::Builder::new()
                .name("lr-exposure".into())
                .spawn(move || {
                    let mut previous = String::new();
                    while !stop.load(Ordering::Relaxed) {
                        if let Ok(text) = std::fs::read_to_string(&path)
                            && text != previous
                        {
                            let mut rows = HashMap::new();
                            for line in text.split_inclusive('\n').filter(|l| l.ends_with('\n')) {
                                if let Some((path, value)) = parse_row(line) {
                                    rows.insert(path, value);
                                }
                            }
                            *values.lock().unwrap() = rows;
                            previous = text;
                            ctx.request_repaint();
                            if previous.ends_with("END\n") {
                                return;
                            }
                        }
                        thread::sleep(Duration::from_millis(250));
                    }
                })
                .expect("spawn LR exposure mailbox");
        }
        Self { stop, values }
    }
    pub fn get(&self, path: &std::path::Path) -> Option<Baseline> {
        self.values.lock().unwrap().get(path).copied()
    }
}
impl Drop for Baselines {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wb_baseline_is_atomic_and_never_invents_as_shot_parameters() {
        use fastcull::xmp::WhiteBalance;
        let prefix = "61\t120000\t6\t0\t0\t0\t1000000\t1000000\t0";
        let (_, custom) = parse_row(&format!("{prefix}\t437573746f6d\t4850\t17\n")).unwrap();
        assert_eq!(
            custom.white_balance,
            Some(WhiteBalance::Custom {
                temperature: 4_850,
                tint: 17
            })
        );
        let (_, shot) = parse_row(&format!("{prefix}\t41732053686f74\t-\t-\n")).unwrap();
        assert_eq!(shot.white_balance, Some(WhiteBalance::AsShot));
        for tail in [
            "437573746f6d\t4850\t-",
            "437573746f6d\t1999\t0",
            "437573746f6d\t5000\t151",
            "4175746f\t5000\t0",
            "garbage\t5000\t0",
        ] {
            let (_, partial) = parse_row(&format!("{prefix}\t{tail}\n")).unwrap();
            assert_eq!(partial.white_balance, None);
            assert_eq!(partial.exposure, Some(120_000));
            assert_eq!(partial.orientation, Some(6));
        }
        assert!(parse_row(&format!("{prefix}\t437573746f6d\t4850\n")).is_none());
    }
    #[test]
    fn catalog_crop_is_independent_of_exposure_and_legacy_rows_still_work() {
        let (_, b) = parse_row("61\t-\t6\t1\t100000\t200000\t900000\t800000\t2500000\n").unwrap();
        assert_eq!(b.exposure, None);
        assert_eq!(b.orientation, Some(6));
        let crop = b.crop.unwrap();
        assert_eq!(
            (crop.left, crop.bottom, crop.angle),
            (100000, 800000, 2500000)
        );
        assert!(crop.enabled);
        let (_, reset) = parse_row("61\t0\t8\t0\t-\t-\t-\t-\t-\n").unwrap();
        assert_eq!(reset.crop, Some(Default::default()));
        let (_, invalid) = parse_row("61\t0\t99\t1\t-1\t0\t900000\t800000\t0\n").unwrap();
        assert!(invalid.crop.is_none() && invalid.orientation.is_none());
        assert_eq!(invalid.exposure, Some(0));
        let (_, legacy) = parse_row("61\t433333\n").unwrap();
        assert_eq!(legacy.exposure, Some(433333));
        assert!(legacy.crop.is_none() && legacy.orientation.is_none());
        assert!(parse_row("61\t0\t6\t1").is_none());
    }
    #[test]
    fn partial_sdk_records_are_not_published_and_completed_values_remain_available() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("session.exposure");
        let raw = PathBuf::from("中文 照片.ARW");
        let token = fastcull::handoff::encode(raw.to_str().unwrap());
        std::fs::write(&file, format!("{token}\t433")).unwrap();
        let reader = Baselines::new(Some(file.clone()), eframe::egui::Context::default());
        thread::sleep(Duration::from_millis(40));
        assert_eq!(reader.get(&raw), None);
        std::fs::write(&file, format!("{token}\t433333\nEND\n")).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while reader.get(&raw).is_none() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(reader.get(&raw).unwrap().exposure, Some(433333));
        std::fs::write(&file, format!("{token}\t0\nEND\n")).unwrap();
        thread::sleep(Duration::from_millis(300));
        assert_eq!(reader.get(&raw).unwrap().exposure, Some(433333)); // Stream completed, no repeated I/O.
    }
}
