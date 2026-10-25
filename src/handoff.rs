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
    // Rating, label, flag, exposure, crop, absolute TIFF orientation, WB, basics.
    pub revisions: [u64; 8],
    // Basic subfields share an atomic group revision, but retain their own last
    // edits so a later group edit cannot replay already acknowledged subfields.
    pub basic_revisions: [u64; 7],
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
                    revisions: [0; 8],
                    basic_revisions: [0; 7],
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
                    "FASTCULL_SESSION_V1"
                        | "FASTCULL_SESSION_V2"
                        | "FASTCULL_SESSION_V3"
                        | "FASTCULL_SESSION_V4"
                        | "FASTCULL_SESSION_V5"
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
                        "FASTCULL_SESSION_V5" => 16,
                        "FASTCULL_SESSION_V4" => 8,
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
                if f.len() >= 7 { f[5].parse()? } else { 0 },
                if f.len() >= 7 { f[6].parse()? } else { 0 },
                if f.len() >= 8 { f[7].parse()? } else { 0 },
                if f.len() == 16 { f[8].parse()? } else { 0 },
            ];
            let mut basic_revisions = [0; 7];
            if f.len() == 16 {
                for (revision, token) in basic_revisions.iter_mut().zip(&f[9..]) {
                    *revision = token.parse()?;
                }
                ensure!(
                    basic_revisions.iter().copied().max() == Some(revisions[7]),
                    "invalid basic group revision"
                );
            }
            ensure!(
                revisions.iter().all(|r| *r <= m.revision),
                "invalid field revision"
            );
            let path = PathBuf::from(decode(f[0])?);
            ensure!(!path.as_os_str().is_empty(), "empty session path");
            m.entries.push(Entry {
                path,
                revisions,
                basic_revisions,
            });
        }
        Ok(m)
    }
    pub fn write(&self, path: &Path) -> Result<()> {
        let v2 = self.entries.iter().any(|e| e.revisions[3] > 0);
        let v3 = self
            .entries
            .iter()
            .any(|e| e.revisions[4..6].iter().any(|r| *r > 0));
        let v4 = self.entries.iter().any(|e| e.revisions[6] > 0);
        let v5 = self.entries.iter().any(|e| e.revisions[7] > 0);
        let mut text = format!(
            "{}\t{}\t{}\t{}\n",
            if v5 {
                "FASTCULL_SESSION_V5"
            } else if v4 {
                "FASTCULL_SESSION_V4"
            } else if v3 {
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
                if v5 {
                    e.revisions[3..]
                        .iter()
                        .chain(e.basic_revisions.iter())
                        .map(|revision| format!("\t{revision}"))
                        .collect::<String>()
                } else if v4 {
                    format!(
                        "\t{}\t{}\t{}\t{}",
                        e.revisions[3], e.revisions[4], e.revisions[5], e.revisions[6]
                    )
                } else if v3 {
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
                u.white_balance.is_some(),
                !u.basic.empty(),
            ]
            .into_iter()
            .enumerate()
            {
                if changed {
                    e.revisions[i] = self.revision;
                }
            }
            for (revision, changed) in e.basic_revisions.iter_mut().zip([
                u.basic.contrast,
                u.basic.highlights,
                u.basic.shadows,
                u.basic.whites,
                u.basic.blacks,
                u.basic.vibrance,
                u.basic.saturation,
            ]) {
                if changed.is_some() {
                    *revision = self.revision;
                }
            }
        }
    }
    pub fn rows_since(&self, after: u64) -> Vec<(PathBuf, Result<Values>)> {
        self.rows_since_fields([after; 8])
    }
    pub fn rows_with_stars_ack(
        &self,
        after: u64,
        stars_after: u64,
    ) -> Vec<(PathBuf, Result<Values>)> {
        self.rows_since_fields([
            after.max(stars_after),
            after,
            after,
            after,
            after,
            after,
            after,
            after,
        ])
    }
    pub fn stars_since(&self, after: u64) -> Vec<(PathBuf, Result<Values>)> {
        self.rows_since_fields([
            after,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
        ])
    }
    pub fn other_fields_since(&self, after: u64) -> usize {
        self.entries
            .iter()
            .filter(|e| e.revisions[1..].iter().any(|r| *r > after))
            .count()
    }
    fn rows_since_fields(&self, after: [u64; 8]) -> Vec<(PathBuf, Result<Values>)> {
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
                        ensure!(
                            e.revisions[6] <= after[6]
                                || v.white_balance.as_ref().and_then(|wb| wb.edit()).is_some(),
                            "changed white balance is missing or unsupported"
                        );
                        if e.revisions[6] <= after[6] {
                            v.white_balance = None;
                        }
                        for (value, revision) in [
                            &mut v.basic.contrast,
                            &mut v.basic.highlights,
                            &mut v.basic.shadows,
                            &mut v.basic.whites,
                            &mut v.basic.blacks,
                            &mut v.basic.vibrance,
                            &mut v.basic.saturation,
                        ]
                        .into_iter()
                        .zip(e.basic_revisions)
                        {
                            ensure!(
                                revision <= after[7] || value.is_some(),
                                "changed basic adjustment is missing"
                            );
                            if revision <= after[7] {
                                *value = None;
                            }
                        }
                        Ok(v)
                    }),
                )
            })
            .collect()
    }
}
pub fn print_row(path: &Path, v: &Values) -> Result<()> {
    let wb = v
        .white_balance
        .as_ref()
        .and_then(|settings| settings.edit());
    let basic = !v.basic.empty();
    if v.crop.is_some() || v.orientation.is_some() || wb.is_some() || basic {
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
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}{}{}",
            if basic {
                "ROW_V5"
            } else if wb.is_some() {
                "ROW_V4"
            } else {
                "ROW_V3"
            },
            encode(path.to_str().context("path must be UTF-8")?),
            v.rating.map_or("-".into(), |r| r.to_string()),
            v.label.as_deref().map_or("-".into(), encode),
            v.flag.as_deref().map_or("-".into(), encode),
            v.exposure
                .map_or("-".into(), |e| format!("{:.6}", e as f64 / 1_000_000.)),
            v.orientation.map_or("-".into(), |o| o.to_string()),
            crop,
            wb.map_or_else(
                || if basic {
                    "\t-\t-\t-".into()
                } else {
                    String::new()
                },
                |wb| match wb {
                    xmp::WhiteBalance::AsShot => format!("\t{}\t-\t-", encode("As Shot")),
                    xmp::WhiteBalance::Custom { temperature, tint } => {
                        format!("\t{}\t{}\t{}", encode("Custom"), temperature, tint)
                    }
                }
            ),
            if basic {
                [
                    v.basic.contrast,
                    v.basic.highlights,
                    v.basic.shadows,
                    v.basic.whites,
                    v.basic.blacks,
                    v.basic.vibrance,
                    v.basic.saturation,
                ]
                .iter()
                .map(|value| format!("\t{}", value.map_or_else(|| "-".into(), |v| v.to_string())))
                .collect::<String>()
            } else {
                String::new()
            }
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
