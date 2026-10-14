//! Bridge-style facets: OR within a field, AND across fields. Original indices
//! remain stable so filtering never changes photo/session identity.
use crate::{arw::CaptureMetadata, xmp::Values};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    Rating,
    Flag,
    Label,
    Day,
    Camera,
    Lens,
    Iso,
    Aperture,
    Shutter,
    Focal,
}
impl Field {
    pub const ALL: [Self; 10] = [
        Self::Rating,
        Self::Flag,
        Self::Label,
        Self::Day,
        Self::Camera,
        Self::Lens,
        Self::Iso,
        Self::Aperture,
        Self::Shutter,
        Self::Focal,
    ];
    pub fn capture(self) -> bool {
        self >= Self::Day
    }
    pub fn numeric(self) -> bool {
        self >= Self::Iso
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Comparison {
    #[default]
    Equal,
    AtLeast,
    AtMost,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Bounds {
    pub min: Option<f64>,
    pub max: Option<f64>,
}
impl Bounds {
    pub fn accepts(self, value: f64) -> bool {
        value.is_finite()
            && self.min.is_none_or(|min| value >= min)
            && self.max.is_none_or(|max| value <= max)
    }
}
#[derive(Clone, Debug, Default)]
pub struct Filters {
    pub selected: BTreeMap<Field, BTreeSet<Option<String>>>,
    pub comparison: Comparison,
    pub query: String,
    pub date_from: Option<u64>,
    pub date_to: Option<u64>,
    pub ranges: BTreeMap<Field, Bounds>,
}
pub struct Row<'a> {
    pub path: &'a Path,
    pub values: &'a Values,
    pub capture: Option<&'a CaptureMetadata>,
    pub indexed: bool,
}
pub fn time_key(value: &str, end_of_day: bool) -> Option<u64> {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() != 10 && bytes.len() != 19 {
        return None;
    }
    if !matches!(bytes[4], b'-' | b':') || !matches!(bytes[7], b'-' | b':') {
        return None;
    }
    if bytes.len() == 19
        && (!matches!(bytes[10], b' ' | b'T') || bytes[13] != b':' || bytes[16] != b':')
    {
        return None;
    }
    let number = |start, end| {
        let v = bytes.get(start..end)?;
        v.iter()
            .all(u8::is_ascii_digit)
            .then(|| v.iter().fold(0u64, |n, b| n * 10 + (b - b'0') as u64))
    };
    let (y, m, d) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return None,
    };
    if y == 0 || d == 0 || d > days {
        return None;
    }
    let (h, min, s) = if bytes.len() == 19 {
        (number(11, 13)?, number(14, 16)?, number(17, 19)?)
    } else if end_of_day {
        (23, 59, 59)
    } else {
        (0, 0, 0)
    };
    if h > 23 || min > 59 || s > 59 {
        return None;
    }
    Some((((y * 100 + m) * 100 + d) * 100 + h) * 10000 + min * 100 + s)
}
fn numeric_key(value: f64) -> Option<String> {
    (value.is_finite() && value > 0.0).then(|| {
        format!("{value:.9}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    })
}
impl Row<'_> {
    pub fn number(&self, field: Field) -> Option<f64> {
        let c = self.capture?;
        match field {
            Field::Iso => c.iso.map(f64::from),
            Field::Aperture => c.aperture,
            Field::Shutter => c.shutter,
            Field::Focal => c.focal,
            _ => None,
        }
    }
    pub fn key(&self, field: Field) -> Option<String> {
        match field {
            Field::Rating => Some(self.values.rating.unwrap_or(0).to_string()),
            Field::Flag => self.values.flag.clone().filter(|s| !s.is_empty()),
            Field::Label => self
                .values
                .label
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_lowercase()),
            Field::Day => self
                .capture?
                .captured_at
                .as_ref()
                .and_then(|s| time_key(s, false))
                .map(|t| {
                    let day = t / 1_000_000;
                    format!(
                        "{:04}-{:02}-{:02}",
                        day / 10000,
                        (day / 100) % 100,
                        day % 100
                    )
                }),
            Field::Camera => self
                .capture?
                .camera
                .clone()
                .or_else(|| self.capture?.make.clone()),
            Field::Lens => self.capture?.lens.clone(),
            field => self.number(field).and_then(numeric_key),
        }
    }
}
impl Filters {
    pub fn active(&self) -> bool {
        !self.query.trim().is_empty()
            || self.date_from.is_some()
            || self.date_to.is_some()
            || !self.ranges.is_empty()
            || self.selected.values().any(|s| !s.is_empty())
    }
    pub fn matches(&self, row: &Row<'_>, ignore: Option<Field>) -> bool {
        if !self.query.trim().is_empty() {
            let name = row
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if !self
                .query
                .split_whitespace()
                .all(|part| name.contains(&part.to_lowercase()))
            {
                return false;
            }
        }
        for field in Field::ALL {
            if ignore == Some(field) {
                continue;
            }
            let choices = self.selected.get(&field).filter(|s| !s.is_empty());
            let range = self.ranges.get(&field);
            let date = field == Field::Day && (self.date_from.is_some() || self.date_to.is_some());
            if choices.is_none() && range.is_none() && !date {
                continue;
            }
            if field.capture() && !row.indexed {
                return false;
            }
            if let Some(choices) = choices {
                let key = row.key(field);
                let accepted = if field == Field::Rating && self.comparison != Comparison::Equal {
                    let value = row.values.rating.unwrap_or(0);
                    choices
                        .iter()
                        .flatten()
                        .filter_map(|s| s.parse::<u8>().ok())
                        .any(|threshold| match self.comparison {
                            Comparison::AtLeast => value >= threshold,
                            Comparison::AtMost => value <= threshold,
                            Comparison::Equal => value == threshold,
                        })
                } else {
                    choices.contains(&key)
                };
                if !accepted {
                    return false;
                }
            }
            if let Some(range) = range
                && !row.number(field).is_some_and(|v| range.accepts(v))
            {
                return false;
            }
            if date {
                let value = row
                    .capture
                    .and_then(|c| c.captured_at.as_deref())
                    .and_then(|s| time_key(s, false));
                if !value.is_some_and(|v| {
                    self.date_from.is_none_or(|min| v >= min)
                        && self.date_to.is_none_or(|max| v <= max)
                }) {
                    return false;
                }
            }
        }
        true
    }
    pub fn facets(&self, rows: &[Row<'_>], field: Field) -> BTreeMap<Option<String>, usize> {
        let mut counts = BTreeMap::new();
        match field {
            Field::Rating => {
                for rating in 0..=5 {
                    counts.insert(Some(rating.to_string()), 0);
                }
            }
            Field::Flag => {
                for flag in [None, Some("pick"), Some("reject")] {
                    counts.insert(flag.map(str::to_owned), 0);
                }
            }
            Field::Label => {
                for label in [
                    None,
                    Some("red"),
                    Some("yellow"),
                    Some("green"),
                    Some("blue"),
                    Some("purple"),
                ] {
                    counts.insert(label.map(str::to_owned), 0);
                }
            }
            _ => {}
        }
        for row in rows {
            if (!field.capture() || row.indexed) && self.matches(row, Some(field)) {
                *counts.entry(row.key(field)).or_default() += 1;
            }
        }
        if let Some(selected) = self.selected.get(&field) {
            for value in selected {
                counts.entry(value.clone()).or_default();
            }
        }
        counts
    }
}

pub fn reconcile_visible(visible: &[usize], current: usize) -> Option<usize> {
    if visible.contains(&current) {
        Some(current)
    } else {
        visible
            .iter()
            .copied()
            .find(|i| *i >= current)
            .or_else(|| visible.last().copied())
    }
}
pub fn step_visible(visible: &[usize], current: usize, delta: isize) -> Option<usize> {
    let position = visible.iter().position(|i| *i == current)?;
    visible
        .get(
            position
                .saturating_add_signed(delta)
                .min(visible.len().saturating_sub(1)),
        )
        .copied()
}
