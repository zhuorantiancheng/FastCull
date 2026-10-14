//! Durable session identity and per-field change revisions. Image paths are UTF-8 hex,
//! so the CLI bridge never needs to interpret labels or paths as shell/Lua code.
use crate::xmp::{self, Updates, Values};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    // Rating, label, flag, exposure, crop, absolute TIFF orientation.
    pub revisions: [u64; 6],
}
#[derive(Clone, Debug)]
pub struct Manifest {
    pub id: String,
    pub closed: bool,
    pub revision: u64,
    pub entries: Vec<Entry>,
}
pub fn encode(s: &str) -> String {
    if s.is_empty() {
        return "~".into();
    }
    s.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}
pub fn decode(s: &str) -> Result<String> {
    if s == "~" {
        return Ok(String::new());
    }
    ensure!(
        !s.is_empty() && s.len().is_multiple_of(2) && s.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid UTF-8 hex token"
    );
    let bytes = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(String::from_utf8(bytes)?)
}
impl Manifest {
    pub fn new(id: String, paths: &[PathBuf]) -> Result<Self> {
        ensure!(
            !id.is_empty()
                && id.len() <= 128
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "invalid session ID"
        );
        Ok(Self {
            id,
            closed: false,
            revision: 0,
            entries: paths
                .iter()
                .map(|p| Entry {
                    path: p.clone(),
                    revisions: [0; 6],
                })
                .collect(),
        })
    }
    pub fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).context("read session manifest")?;
        let mut lines = text.lines();
        let h: Vec<_> = lines
            .next()
            .context("empty session manifest")?
            .split('\t')
            .collect();
        ensure!(
            h.len() == 4
                && matches!(
                    h[0],
                    "FASTCULL_SESSION_V1" | "FASTCULL_SESSION_V2" | "FASTCULL_SESSION_V3"
                ),
            "invalid session manifest header"
        );
        let mut m = Self::new(h[1].into(), &[])?;
        ensure!(
            h[2] == "active" || h[2] == "closed",
            "invalid session status"
        );
        m.closed = h[2] == "closed";
        m.revision = h[3].parse()?;
        for line in lines {
            let f: Vec<_> = line.split('\t').collect();
            ensure!(
                f.len()
                    == match h[0] {
                        "FASTCULL_SESSION_V3" => 7,
                        "FASTCULL_SESSION_V2" => 5,
                        _ => 4,
                    },
                "invalid session row"
            );
            let revisions = [
                f[1].parse()?,
                f[2].parse()?,
                f[3].parse()?,
                if f.len() >= 5 { f[4].parse()? } else { 0 },
                if f.len() == 7 { f[5].parse()? } else { 0 },
                if f.len() == 7 { f[6].parse()? } else { 0 },
            ];
            ensure!(
                revisions.iter().all(|r| *r <= m.revision),
                "invalid field revision"
            );
            let path = PathBuf::from(decode(f[0])?);
            ensure!(!path.as_os_str().is_empty(), "empty session path");
            m.entries.push(Entry { path, revisions });
        }
        Ok(m)
    }
    pub fn write(&self, path: &Path) -> Result<()> {
        let v2 = self.entries.iter().any(|e| e.revisions[3] > 0);
        let v3 = self
            .entries
            .iter()
            .any(|e| e.revisions[4..].iter().any(|r| *r > 0));
        let mut text = format!(
            "{}\t{}\t{}\t{}\n",
            if v3 {
                "FASTCULL_SESSION_V3"
            } else if v2 {
                "FASTCULL_SESSION_V2"
            } else {
                "FASTCULL_SESSION_V1"
            },
            self.id,
            if self.closed { "closed" } else { "active" },
            self.revision
        );
        for e in &self.entries {
            text.push_str(&format!(
                "{}\t{}\t{}\t{}{}\n",
                encode(e.path.to_str().context("session path must be UTF-8")?),
                e.revisions[0],
                e.revisions[1],
                e.revisions[2],
                if v3 {
                    format!(
                        "\t{}\t{}\t{}",
                        e.revisions[3], e.revisions[4], e.revisions[5]
                    )
                } else if v2 {
                    format!("\t{}", e.revisions[3])
                } else {
                    String::new()
                }
            ));
        }
        let mut tmp = tempfile::NamedTempFile::new_in(path.parent().context("session parent")?)?;
        tmp.write_all(text.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
    pub fn record(&mut self, index: usize, u: &Updates) {
        if u.empty() {
            return;
        }
        self.revision += 1;
        let path = self.entries[index].path.clone();
        // LR may contain duplicate state rows for the same source file.
        for e in self.entries.iter_mut().filter(|e| e.path == path) {
            for (i, changed) in [
                u.rating.is_some(),
                u.label.is_some(),
                u.flag.is_some(),
                u.exposure.is_some(),
                u.crop.is_some(),
                u.orientation.is_some(),
            ]
            .into_iter()
            .enumerate()
            {
                if changed {
                    e.revisions[i] = self.revision;
                }
            }
        }
    }
    pub fn rows_since(&self, after: u64) -> Vec<(PathBuf, Result<Values>)> {
        self.rows_since_fields([after; 6])
    }
    pub fn rows_with_stars_ack(
        &self,
        after: u64,
        stars_after: u64,
    ) -> Vec<(PathBuf, Result<Values>)> {
        self.rows_since_fields([after.max(stars_after), after, after, after, after, after])
    }
    pub fn stars_since(&self, after: u64) -> Vec<(PathBuf, Result<Values>)> {
        self.rows_since_fields([after, u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX])
    }
    pub fn other_fields_since(&self, after: u64) -> usize {
        self.entries
            .iter()
            .filter(|e| e.revisions[1..].iter().any(|r| *r > after))
            .count()
    }
    fn rows_since_fields(&self, after: [u64; 6]) -> Vec<(PathBuf, Result<Values>)> {
        let mut seen = std::collections::HashSet::new();
        self.entries
            .iter()
            .filter(|e| {
                e.revisions.iter().zip(after).any(|(r, a)| *r > a) && seen.insert(e.path.clone())
            })
            .map(|e| {
                (
                    e.path.clone(),
                    xmp::read(&e.path).and_then(|mut v| {
                        // Missing changed fields are errors, never silently acknowledged.
                        ensure!(
                            e.revisions[0] <= after[0] || v.rating.is_some(),
                            "changed rating is missing"
                        );
                        ensure!(
                            e.revisions[1] <= after[1] || v.label.is_some(),
                            "changed label is missing"
                        );
                        ensure!(
                            e.revisions[2] <= after[2] || v.flag.is_some(),
                            "changed flag is missing"
                        );
                        if e.revisions[0] <= after[0] {
                            v.rating = None;
                        }
                        if e.revisions[1] <= after[1] {
                            v.label = None;
                        }
                        if e.revisions[2] <= after[2] {
                            v.flag = None;
                        }
                        ensure!(
                            e.revisions[3] <= after[3] || v.exposure.is_some(),
                            "changed exposure is missing"
                        );
                        if e.revisions[3] <= after[3] {
                            v.exposure = None;
                        }
                        ensure!(
                            e.revisions[4] <= after[4] || v.crop.is_some(),
                            "changed crop is missing"
                        );
                        ensure!(
                            e.revisions[5] <= after[5] || v.orientation.is_some(),
                            "changed orientation is missing"
                        );
                        if e.revisions[4] <= after[4] {
                            v.crop = None;
                        }
                        if e.revisions[5] <= after[5] {
                            v.orientation = None;
                        }
                        Ok(v)
                    }),
                )
            })
            .collect()
    }
}
pub fn print_row(path: &Path, v: &Values) -> Result<()> {
    if v.crop.is_some() || v.orientation.is_some() {
        let crop = v.crop.map_or_else(
            || "-\t-\t-\t-\t-\t-".to_owned(),
            |c| {
                format!(
                    "{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}",
                    u8::from(c.enabled),
                    c.left as f64 / 1_000_000.,
                    c.top as f64 / 1_000_000.,
                    c.right as f64 / 1_000_000.,
                    c.bottom as f64 / 1_000_000.,
                    c.angle as f64 / 1_000_000.
                )
            },
        );
        println!(
            "ROW_V3\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            encode(path.to_str().context("path must be UTF-8")?),
            v.rating.map_or("-".into(), |r| r.to_string()),
            v.label.as_deref().map_or("-".into(), encode),
            v.flag.as_deref().map_or("-".into(), encode),
            v.exposure
                .map_or("-".into(), |e| format!("{:.6}", e as f64 / 1_000_000.)),
            v.orientation.map_or("-".into(), |o| o.to_string()),
            crop
        );
        return Ok(());
    }
    println!(
        "ROW\t{}\t{}\t{}\t{}{}",
        encode(path.to_str().context("path must be UTF-8")?),
        v.rating.map_or("-".into(), |r| r.to_string()),
        v.label.as_deref().map_or("-".into(), encode),
        v.flag.as_deref().map_or("-".into(), encode),
        v.exposure
            .map_or(String::new(), |v| format!("\t{:.6}", v as f64 / 1_000_000.))
    );
    Ok(())
}
