//! Byte-preserving edits: resolve XML namespace URIs, replace only value spans.
use anyhow::{Context, Result, bail, ensure};
use quick_xml::{Reader, events::Event};
use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::Path,
    sync::Arc,
};

const XMP: &str = "http://ns.adobe.com/xap/1.0/";
const SR: &str = "urn:zrtc:syncratings:1/";
const TIFF: &str = "http://ns.adobe.com/tiff/1.0/";
const CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// An atomic, absolute RAW white-balance edit. Never invent an As Shot temperature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteBalance {
    AsShot,
    Custom { temperature: u32, tint: i32 },
}
impl WhiteBalance {
    /// Conservative RAW editor limits, not a restriction on existing XMP values.
    pub fn valid(self) -> bool {
        match self {
            Self::AsShot => true,
            Self::Custom { temperature, tint } => {
                (2_000..=50_000).contains(&temperature) && (-150..=150).contains(&tint)
            }
        }
    }
}

/// The original XMP values, including presets or modes we do not edit.
/// Adobe may omit Temperature/Tint for As Shot; missing values stay missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WhiteBalanceSettings {
    pub mode: Option<String>,
    pub temperature: Option<u32>,
    pub tint: Option<i32>,
}
impl WhiteBalanceSettings {
    /// Convert only complete, supported settings to an editable value.
    pub fn edit(&self) -> Option<WhiteBalance> {
        let value = match self.mode.as_deref()? {
            "As Shot" => WhiteBalance::AsShot,
            "Custom" => WhiteBalance::Custom {
                temperature: self.temperature?,
                tint: self.tint?,
            },
            _ => return None,
        };
        value.valid().then_some(value)
    }
}
impl From<WhiteBalance> for WhiteBalanceSettings {
    fn from(value: WhiteBalance) -> Self {
        match value {
            WhiteBalance::AsShot => Self {
                mode: Some("As Shot".into()),
                ..Default::default()
            },
            WhiteBalance::Custom { temperature, tint } => Self {
                mode: Some("Custom".into()),
                temperature: Some(temperature),
                tint: Some(tint),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BasicControl {
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    Vibrance,
    Saturation,
}
impl BasicControl {
    pub const ALL: [Self; 7] = [
        Self::Contrast,
        Self::Highlights,
        Self::Shadows,
        Self::Whites,
        Self::Blacks,
        Self::Vibrance,
        Self::Saturation,
    ];
    fn modern_key(self) -> &'static str {
        match self {
            Self::Contrast => "crs:Contrast2012",
            Self::Highlights => "crs:Highlights2012",
            Self::Shadows => "crs:Shadows2012",
            Self::Whites => "crs:Whites2012",
            Self::Blacks => "crs:Blacks2012",
            Self::Vibrance => "crs:Vibrance",
            Self::Saturation => "crs:Saturation",
        }
    }
}
/// Explicit basic-panel edits. None keeps the existing value; Some(0) resets it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BasicAdjustments {
    pub contrast: Option<i32>,
    pub highlights: Option<i32>,
    pub shadows: Option<i32>,
    pub whites: Option<i32>,
    pub blacks: Option<i32>,
    pub vibrance: Option<i32>,
    pub saturation: Option<i32>,
}
impl BasicAdjustments {
    pub fn get(self, control: BasicControl) -> Option<i32> {
        match control {
            BasicControl::Contrast => self.contrast,
            BasicControl::Highlights => self.highlights,
            BasicControl::Shadows => self.shadows,
            BasicControl::Whites => self.whites,
            BasicControl::Blacks => self.blacks,
            BasicControl::Vibrance => self.vibrance,
            BasicControl::Saturation => self.saturation,
        }
    }
    pub fn set(&mut self, control: BasicControl, value: Option<i32>) {
        *match control {
            BasicControl::Contrast => &mut self.contrast,
            BasicControl::Highlights => &mut self.highlights,
            BasicControl::Shadows => &mut self.shadows,
            BasicControl::Whites => &mut self.whites,
            BasicControl::Blacks => &mut self.blacks,
            BasicControl::Vibrance => &mut self.vibrance,
            BasicControl::Saturation => &mut self.saturation,
        } = value;
    }
    pub fn empty(self) -> bool {
        BasicControl::ALL.into_iter().all(|c| self.get(c).is_none())
    }
    pub fn valid(self) -> bool {
        BasicControl::ALL
            .into_iter()
            .all(|c| self.get(c).is_none_or(|v| (-100..=100).contains(&v)))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values {
    pub crop: Option<crate::crop::Crop>,
    /// Millionths of an EV; legacy process range -4..4, Process 2012+ -5..5.
    pub exposure: Option<i32>,
    pub white_balance: Option<WhiteBalanceSettings>,
    pub basic: BasicAdjustments,
    /// Profile reference and processing version are read-only in this editor.
    pub camera_profile: Option<String>,
    pub process_version: Option<String>,
    pub rating: Option<u8>,
    pub label: Option<String>,
    pub flag: Option<String>,
    pub orientation: Option<u16>,
}
#[derive(Clone, Debug, Default)]
pub struct Updates {
    pub crop: Option<crate::crop::Crop>,
    pub exposure: Option<i32>,
    pub white_balance: Option<WhiteBalance>,
    pub basic: BasicAdjustments,
    pub rating: Option<u8>,
    pub label: Option<String>,
    pub flag: Option<String>,
    pub orientation: Option<u16>,
}
impl Updates {
    fn entries(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if self.crop.is_some()
            || self.exposure.is_some()
            || self.white_balance.is_some()
            || !self.basic.empty()
        {
            out.push(("crs:HasSettings", "True".into()));
        }
        if let Some(c) = self.crop {
            out.push((
                "crs:HasCrop",
                if c.enabled { "True" } else { "False" }.into(),
            ));
            for (key, value) in [
                ("crs:CropLeft", c.left),
                ("crs:CropTop", c.top),
                ("crs:CropRight", c.right),
                ("crs:CropBottom", c.bottom),
                ("crs:CropAngle", c.angle),
            ] {
                out.push((key, format!("{:.6}", value as f64 / 1_000_000.)));
            }
        }
        if let Some(v) = self.exposure {
            out.push(("crs:Exposure2012", format!("{:.6}", v as f64 / 1_000_000.)));
        }
        if let Some(v) = self.white_balance {
            match v {
                WhiteBalance::AsShot => out.push(("crs:WhiteBalance", "As Shot".into())),
                WhiteBalance::Custom { temperature, tint } => {
                    out.push(("crs:WhiteBalance", "Custom".into()));
                    out.push(("crs:Temperature", temperature.to_string()));
                    out.push(("crs:Tint", tint.to_string()));
                }
            }
        }
        for control in BasicControl::ALL {
            if let Some(value) = self.basic.get(control) {
                out.push((control.modern_key(), value.to_string()));
            }
        }
        if let Some(v) = self.rating {
            out.push(("xmp:Rating", v.to_string()));
        }
        if let Some(v) = &self.label {
            out.push(("xmp:Label", v.clone()));
        }
        if let Some(v) = &self.flag {
            out.push(("sr:Flag", v.clone()));
        }
        if let Some(v) = self.orientation {
            out.push(("tiff:Orientation", v.to_string()));
        }
        out
    }
    pub fn empty(&self) -> bool {
        self.entries().is_empty()
    }
    pub fn zero(&self) -> bool {
        self.rating.unwrap_or(0) == 0
            && self.label.as_deref().unwrap_or("").is_empty()
            && self.flag.as_deref().unwrap_or("").is_empty()
            && self.orientation.is_none()
            && self.exposure.is_none()
            && self.white_balance.is_none()
            && self.basic.empty()
            && self.crop.is_none()
    }
}
struct Document {
    spans: HashMap<String, Vec<Range<usize>>>,
    insert: usize,
    namespaces: HashMap<String, String>,
    namespace_repairs: Vec<(Range<usize>, String)>,
}
fn scalar_value(text: &str, document: &Document, key: &str) -> Result<Option<String>> {
    let mut value = None;
    for range in document.spans.get(key).into_iter().flatten() {
        let current = quick_xml::escape::unescape(text[range.clone()].trim())?.into_owned();
        ensure!(
            value.as_ref().is_none_or(|v| v == &current),
            "conflicting {key} values"
        );
        value = Some(current);
    }
    Ok(value)
}
#[derive(Clone, Copy)]
enum ExposureKind {
    Legacy,
    Process2012,
}
impl ExposureKind {
    fn key(self) -> &'static str {
        match self {
            Self::Legacy => "crs:Exposure",
            Self::Process2012 => "crs:Exposure2012",
        }
    }
    fn limit(self) -> i32 {
        match self {
            Self::Legacy => 4_000_000,
            Self::Process2012 => 5_000_000,
        }
    }
}
fn exposure_kind(text: &str, document: &Document) -> Result<ExposureKind> {
    if let Some(version) = scalar_value(text, document, "crs:ProcessVersion")? {
        let version: f64 = version
            .parse()
            .context("unrecognized Camera Raw ProcessVersion for exposure")?;
        ensure!(version.is_finite(), "invalid Camera Raw ProcessVersion");
        if version == 5.0 || version == 5.7 {
            return Ok(ExposureKind::Legacy);
        }
        if version == 6.6 || version >= 6.7 {
            return Ok(ExposureKind::Process2012);
        }
        bail!(
            "unsupported Camera Raw ProcessVersion for exposure; process version was not changed"
        );
    }
    let legacy = [
        "crs:Exposure",
        "crs:Contrast",
        "crs:Brightness",
        "crs:Shadows",
    ]
    .iter()
    .any(|key| document.spans.contains_key(*key));
    let modern = [
        "crs:Exposure2012",
        "crs:Contrast2012",
        "crs:Highlights2012",
        "crs:Shadows2012",
        "crs:Whites2012",
        "crs:Blacks2012",
    ]
    .iter()
    .any(|key| document.spans.contains_key(*key));
    match (legacy, modern) {
        (true, true) => bail!("ambiguous tone fields without Camera Raw ProcessVersion"),
        (true, false) => Ok(ExposureKind::Legacy),
        _ => Ok(ExposureKind::Process2012),
    }
}
/// Absolute editor bounds in millionths of an EV, preserving the packet's process.
/// Unknown process versions or ambiguous unversioned fields are explicit errors.
pub fn exposure_range(text: &str) -> Result<std::ops::RangeInclusive<i32>> {
    let limit = exposure_kind(text, &document(text)?)?.limit();
    Ok(-limit..=limit)
}
fn basic_spec(
    text: &str,
    document: &Document,
    control: BasicControl,
) -> Result<(&'static str, std::ops::RangeInclusive<i32>)> {
    if matches!(control, BasicControl::Vibrance | BasicControl::Saturation) {
        return Ok((control.modern_key(), -100..=100));
    }
    match (exposure_kind(text, document)?, control) {
        (ExposureKind::Legacy, BasicControl::Contrast) => Ok(("crs:Contrast", -50..=100)),
        (ExposureKind::Legacy, _) => bail!(
            "{control:?} has no equivalent in this Camera Raw process version; process version was not changed"
        ),
        (ExposureKind::Process2012, _) => Ok((control.modern_key(), -100..=100)),
    }
}
/// Capability and legal absolute values for one basic-panel control. Compute when
/// a packet is loaded; unsupported legacy controls and unknown processes return Err.
pub fn basic_range(text: &str, control: BasicControl) -> Result<std::ops::RangeInclusive<i32>> {
    Ok(basic_spec(text, &document(text)?, control)?.1)
}
type XmlFrame = (HashMap<String, String>, Option<(&'static str, usize)>, bool);
// The Python prototype could insert these known fields without their xmlns.
// Only infer our frozen schemas when the prefix is UNBOUND; never override a binding.
fn legacy_schema(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        "xmp:Rating" | "xmp:Label" => Some(("xmp", XMP)),
        "sr:Flag" => Some(("sr", SR)),
        "tiff:Orientation" => Some(("tiff", TIFF)),
        _ => None,
    }
}
fn field(name: &str, ns: &HashMap<String, String>, attribute: bool) -> Option<&'static str> {
    let (prefix, local) = match name.split_once(':') {
        Some(value) => value,
        None if !attribute => ("", name),
        _ => return None, // XML default namespaces never apply to attributes.
    };
    match (ns.get(prefix)?.as_str(), local) {
        (XMP, "Rating") => Some("xmp:Rating"),
        (XMP, "Label") => Some("xmp:Label"),
        (SR, "Flag") => Some("sr:Flag"),
        (TIFF, "Orientation") => Some("tiff:Orientation"),
        (CRS, "Exposure2012") => Some("crs:Exposure2012"),
        (CRS, "Exposure") => Some("crs:Exposure"),
        (CRS, "AutoExposure") => Some("crs:AutoExposure"),
        (CRS, "AutoContrast") => Some("crs:AutoContrast"),
        (CRS, "Contrast") => Some("crs:Contrast"),
        (CRS, "Brightness") => Some("crs:Brightness"),
        (CRS, "Shadows") => Some("crs:Shadows"),
        (CRS, "Contrast2012") => Some("crs:Contrast2012"),
        (CRS, "Highlights2012") => Some("crs:Highlights2012"),
        (CRS, "Shadows2012") => Some("crs:Shadows2012"),
        (CRS, "Whites2012") => Some("crs:Whites2012"),
        (CRS, "Blacks2012") => Some("crs:Blacks2012"),
        (CRS, "Vibrance") => Some("crs:Vibrance"),
        (CRS, "Saturation") => Some("crs:Saturation"),
        (CRS, "WhiteBalance") => Some("crs:WhiteBalance"),
        (CRS, "Temperature") => Some("crs:Temperature"),
        (CRS, "Tint") => Some("crs:Tint"),
        (CRS, "CameraProfile") => Some("crs:CameraProfile"),
        (CRS, "ProcessVersion") => Some("crs:ProcessVersion"),
        (CRS, "HasCrop") => Some("crs:HasCrop"),
        (CRS, "HasSettings") => Some("crs:HasSettings"),
        (CRS, "CropLeft") => Some("crs:CropLeft"),
        (CRS, "CropTop") => Some("crs:CropTop"),
        (CRS, "CropRight") => Some("crs:CropRight"),
        (CRS, "CropBottom") => Some("crs:CropBottom"),
        (CRS, "CropAngle") => Some("crs:CropAngle"),
        _ => None,
    }
}
fn attrs(text: &str, start: usize, end: usize) -> Result<Vec<(String, Range<usize>)>> {
    let b = text.as_bytes();
    let mut i = start + 1;
    while i < end && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
        i += 1;
    }
    let mut out = Vec::new();
    while i < end {
        while i < end && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= end || b[i] == b'/' || b[i] == b'>' {
            break;
        }
        let a = i;
        while i < end && !b[i].is_ascii_whitespace() && b[i] != b'=' {
            i += 1;
        }
        let name = text[a..i].to_owned();
        while i < end && b[i].is_ascii_whitespace() {
            i += 1;
        }
        ensure!(b.get(i) == Some(&b'='), "invalid XML attribute");
        i += 1;
        while i < end && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let q = *b.get(i).context("missing XML quote")?;
        ensure!(q == b'\'' || q == b'"', "invalid XML quote");
        i += 1;
        let v = i;
        while i < end && b[i] != q {
            i += 1;
        }
        ensure!(i < end, "unterminated XML attribute");
        out.push((name, v..i));
        i += 1;
    }
    Ok(out)
}
fn document(text: &str) -> Result<Document> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut spans: HashMap<String, Vec<Range<usize>>> = HashMap::new();
    let mut stack: Vec<XmlFrame> = Vec::new();
    let (mut insert, mut namespaces) = (None, HashMap::new());
    let mut roots = 0;
    let mut namespace_repairs = Vec::new();
    loop {
        let begin = reader.buffer_position() as usize;
        let ev = reader.read_event().context("malformed XMP XML")?;
        let end = reader.buffer_position() as usize;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                if stack.is_empty() {
                    roots += 1;
                    ensure!(roots == 1, "multiple XML roots");
                }
                let mut ns = stack.last().map(|x| x.0.clone()).unwrap_or_default();
                let tagattrs = attrs(text, begin, end)?;
                // Let quick-xml check duplicate attributes as well.
                for a in e.attributes() {
                    a.context("invalid XML attributes")?;
                }
                for (name, range) in &tagattrs {
                    if let Some(p) = if name == "xmlns" {
                        Some("")
                    } else {
                        name.strip_prefix("xmlns:")
                    } {
                        ns.insert(
                            p.to_owned(),
                            quick_xml::escape::unescape(&text[range.clone()])?.into_owned(),
                        );
                    }
                }
                let name = std::str::from_utf8(e.name().as_ref())?.to_owned();
                let (prefix, local) = name.split_once(':').unwrap_or(("", &name));
                let description = !stack.iter().any(|frame| frame.2)
                    && local == "Description"
                    && ns.get(prefix).is_some_and(|v| v == RDF);
                if description {
                    let mut at = end - 1;
                    if text.as_bytes()[at - 1] == b'/' {
                        at -= 1;
                    }
                    let mut repair = String::new();
                    for (attribute, _) in &tagattrs {
                        if let Some((prefix, uri)) = legacy_schema(attribute)
                            && !ns.contains_key(prefix)
                        {
                            repair.push_str(&format!("\n    xmlns:{prefix}=\"{uri}\""));
                            ns.insert(prefix.to_owned(), uri.to_owned());
                        }
                    }
                    if !repair.is_empty() {
                        namespace_repairs.push((at..at, repair));
                    }
                    if insert.is_none() {
                        insert = Some(at);
                        namespaces = ns.clone();
                    }
                }
                if description {
                    for (name, range) in tagattrs {
                        if let Some(k) = field(&name, &ns, true) {
                            spans.entry(k.to_owned()).or_default().push(range);
                        }
                    }
                }
                ensure!(
                    stack.last().and_then(|s| s.1).is_none(),
                    "nested markup in XMP scalar field"
                );
                let target = if stack.last().is_some_and(|s| s.2) {
                    field(&name, &ns, false)
                } else {
                    None
                };
                if empty && target.is_some() {
                    bail!("self-closing XMP scalar field unsupported; original kept");
                }
                if !empty {
                    stack.push((ns, target.map(|k| (k, end)), description));
                }
            }
            Event::End(_) => {
                let (_, target, _) = stack.pop().context("unbalanced XML")?;
                if let Some((key, start)) = target {
                    spans.entry(key.to_owned()).or_default().push(start..begin);
                }
            }
            Event::DocType(_) => bail!("XMP DTD unsupported; original kept"),
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(stack.is_empty(), "truncated XML");
    Ok(Document {
        spans,
        insert: insert.context("XMP has no rdf:Description")?,
        namespaces,
        namespace_repairs,
    })
}
/// Read the complete packet for a rendering backend. A DNG sidecar, when present,
/// remains authoritative over its embedded packet, as in the culling path.
pub fn packet(raw: &Path) -> Result<Option<String>> {
    Ok(if crate::formats::is_jpeg(raw) {
        crate::jpeg::read_packet(raw)?
    } else {
        match fs::read_to_string(raw.with_extension("xmp")) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if crate::formats::is_dng(raw) {
                    crate::dng::read_xmp(raw)?
                } else {
                    None
                }
            }
            Err(e) => return Err(e.into()),
        }
    })
}
pub fn read(raw: &Path) -> Result<Values> {
    let text = packet(raw)?;
    let mut values = text
        .as_deref()
        .map(parse_values)
        .transpose()?
        .unwrap_or_default();
    // Explicit zero is authoritative, just like any other sidecar/XMP rating.
    if values.rating.is_none() {
        values.rating = crate::camera_rating::read(raw);
    }
    Ok(values)
}
/// Read a packet in memory without applying camera-file metadata fallbacks.
pub fn parse_values(text: &str) -> Result<Values> {
    let doc = document(text)?;
    let get = |k: &str| -> Result<Option<String>> {
        doc.spans
            .get(k)
            .and_then(|v| v.first())
            .map(|r| {
                quick_xml::escape::unescape(text[r.clone()].trim())
                    .map(|v| v.into_owned())
                    .map_err(Into::into)
            })
            .transpose()
    };
    // An inconsistent duplicate is not a usable develop baseline. Keep the file
    // untouched and report the ambiguity instead of choosing an arbitrary value.
    let strict = |key: &str| scalar_value(text, &doc, key);
    let mode = strict("crs:WhiteBalance")?;
    ensure!(mode.as_deref() != Some(""), "empty crs:WhiteBalance");
    let temperature = strict("crs:Temperature")?
        .map(|value| -> Result<u32> {
            let value = value.parse::<u32>().context("invalid crs:Temperature")?;
            ensure!(value > 0, "invalid crs:Temperature");
            Ok(value)
        })
        .transpose()?;
    let tint = strict("crs:Tint")?
        .map(|value| value.parse::<i32>().context("invalid crs:Tint"))
        .transpose()?;
    let white_balance = if mode.is_some() || temperature.is_some() || tint.is_some() {
        Some(WhiteBalanceSettings {
            mode,
            temperature,
            tint,
        })
    } else {
        None
    };
    let crop = match get("crs:HasCrop")?.as_deref() {
        Some("True" | "true" | "1") => {
            let number = |key: &str| -> Result<i32> {
                let value: f64 = get(key)?.context(format!("missing {key}"))?.parse()?;
                ensure!(value.is_finite() && value.abs() <= 45., "invalid {key}");
                Ok((value * 1_000_000.).round() as i32)
            };
            let c = crate::crop::Crop {
                enabled: true,
                left: number("crs:CropLeft")?,
                top: number("crs:CropTop")?,
                right: number("crs:CropRight")?,
                bottom: number("crs:CropBottom")?,
                angle: if get("crs:CropAngle")?.is_some() {
                    number("crs:CropAngle")?
                } else {
                    0
                },
            };
            ensure!(c.valid(), "invalid Camera Raw crop; original kept");
            Some(c)
        }
        Some("False" | "false" | "0") => Some(crate::crop::Crop::default()),
        _ => None,
    };
    let exposure = if let Ok(kind) = exposure_kind(text, &doc) {
        let limit = kind.limit() as f64 / 1_000_000.;
        strict(kind.key())?
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && (-limit..=limit).contains(v))
            .map(|v| (v * 1_000_000.).round() as i32)
    } else {
        None
    };
    let mut basic = BasicAdjustments::default();
    for control in BasicControl::ALL {
        if let Ok((key, range)) = basic_spec(text, &doc, control) {
            let value = strict(key)?
                .map(|raw| raw.parse::<i32>().with_context(|| format!("invalid {key}")))
                .transpose()?;
            ensure!(
                value.is_none_or(|v| range.contains(&v)),
                "{key} outside current process version range"
            );
            basic.set(control, value);
        }
    }
    Ok(Values {
        crop,
        white_balance,
        camera_profile: strict("crs:CameraProfile")?,
        process_version: strict("crs:ProcessVersion")?,
        exposure,
        basic,
        rating: get("xmp:Rating")?
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|v| *v <= 5),
        label: get("xmp:Label")?,
        flag: get("sr:Flag")?,
        orientation: get("tiff:Orientation")?
            .and_then(|v| v.parse().ok())
            .filter(|v| (1..=8).contains(v)),
    })
}
pub fn patch(text: &str, updates: &Updates) -> Result<String> {
    ensure!(updates.crop.is_none_or(|c| c.valid()), "invalid crop");
    ensure!(updates.basic.valid(), "basic adjustment outside -100..100");
    ensure!(
        updates.white_balance.is_none_or(WhiteBalance::valid),
        "white balance outside editor limits"
    );
    ensure!(
        updates
            .exposure
            .is_none_or(|v| (-5_000_000..=5_000_000).contains(&v)),
        "invalid exposure"
    );
    ensure!(updates.rating.is_none_or(|v| v <= 5), "rating outside 0..5");
    ensure!(
        updates.orientation.is_none_or(|v| (1..=8).contains(&v)),
        "invalid orientation"
    );
    ensure!(
        updates
            .flag
            .as_deref()
            .is_none_or(|v| matches!(v, "" | "pick" | "reject")),
        "invalid flag"
    );
    let doc = document(text)?;
    let exposure = if let Some(value) = updates.exposure {
        let kind = exposure_kind(text, &doc)?;
        ensure!(
            value.abs() <= kind.limit(),
            "exposure outside current process version range"
        );
        Some(kind)
    } else {
        None
    };
    let mut entries = updates.entries();
    if let Some(kind) = exposure {
        for (key, _) in &mut entries {
            if *key == "crs:Exposure2012" {
                *key = kind.key();
            }
        }
        if matches!(kind, ExposureKind::Legacy) {
            entries.push(("crs:AutoExposure", "False".into()));
        }
    }
    for control in BasicControl::ALL {
        if let Some(value) = updates.basic.get(control) {
            let (key, range) = basic_spec(text, &doc, control)?;
            ensure!(
                range.contains(&value),
                "{control:?} outside current process version range"
            );
            for (entry_key, _) in &mut entries {
                if *entry_key == control.modern_key() {
                    *entry_key = key;
                }
            }
            if key == "crs:Contrast" {
                entries.push(("crs:AutoContrast", "False".into()));
            }
        }
    }
    let mut edits = doc.namespace_repairs;
    let mut additions = String::new();
    let mut ns = doc.namespaces;
    for (key, value) in entries {
        let escaped = quick_xml::escape::escape(&value).into_owned();
        if let Some(ranges) = doc.spans.get(key) {
            for r in ranges {
                edits.push((r.clone(), escaped.clone()));
            }
        } else {
            let (prefix, local) = key.split_once(':').unwrap();
            let uri = match prefix {
                "xmp" => XMP,
                "tiff" => TIFF,
                "crs" => CRS,
                _ => SR,
            };
            let use_prefix = if let Some((p, _)) = ns.iter().find(|(_, v)| v.as_str() == uri) {
                p.clone()
            } else {
                ensure!(!ns.contains_key(prefix), "XMP namespace prefix collision");
                additions.push_str(&format!("\n    xmlns:{prefix}=\"{uri}\""));
                ns.insert(prefix.to_owned(), uri.to_owned());
                prefix.to_owned()
            };
            additions.push_str(&format!("\n    {use_prefix}:{local}=\"{escaped}\""));
        }
    }
    if !additions.is_empty() {
        edits.push((doc.insert..doc.insert, additions));
    }
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
    let mut out = text.to_owned();
    for (r, v) in edits {
        out.replace_range(r, &v);
    }
    ensure!(
        document(&out)?.namespace_repairs.is_empty(),
        "XMP namespace repair incomplete"
    );
    Ok(out)
}
/// A single canonical empty packet for new sidecars and unsaved render drafts.
/// Deliberately does not choose a process version, camera profile or white balance.
pub fn template() -> String {
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"{RDF}\"><rdf:Description rdf:about=\"\" xmlns:xmp=\"{XMP}\" xmlns:tiff=\"{TIFF}\" xmlns:sr=\"{SR}\"/></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>\n"
    )
}
pub fn write(raw: &Path, updates: &Updates) -> Result<bool> {
    use std::io::Write;
    if updates.empty() {
        return Ok(false);
    }
    let path = raw.with_extension("xmp");
    let jpeg = crate::formats::is_jpeg(raw);
    let original = if jpeg {
        crate::jpeg::read_packet(raw)?
    } else {
        match fs::read_to_string(&path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        }
    };
    // Seed a new DNG sidecar from embedded XMP so rating edits also preserve
    // existing crop/exposure/develop settings. Keep `original` as the on-disk
    // sidecar snapshot for the concurrent-write check below.
    let embedded = if !jpeg && original.is_none() && crate::formats::is_dng(raw) {
        crate::dng::read_xmp(raw)?
    } else {
        None
    };
    let template = template();
    let out = patch(
        original
            .as_deref()
            .or(embedded.as_deref())
            .unwrap_or(&template),
        updates,
    )?;
    if original.as_ref() == Some(&out) {
        return Ok(false);
    }
    if jpeg {
        crate::jpeg::write_packet_with_orientation(
            raw,
            original.as_deref(),
            &out,
            updates.orientation,
        )?;
        log::info!("saved embedded JPEG XMP {} {:?}", raw.display(), updates);
        return Ok(true);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".fastcull-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    tmp.write_all(out.as_bytes())?;
    tmp.as_file().sync_all()?;
    // Detect intervening external writes before committing our replacement.
    ensure!(
        fs::read_to_string(&path).ok() == original,
        "XMP changed while saving; retry after LR finishes"
    );
    tmp.persist(&path).map_err(|e| e.error)?;
    log::info!("saved {} {:?}", path.display(), updates);
    Ok(true)
}

const XML: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
fn xml_character(c: char) -> bool {
    matches!(c as u32, 0x9 | 0xa | 0xd | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
}
#[derive(Clone, Copy)]
enum DevelopNode {
    Meta,
    Rdf,
    Description(usize),
    Property(usize, usize),
    Other,
}
struct DevelopFrame {
    namespaces: Arc<HashMap<String, String>>,
    lang: String,
    space: String,
    node: DevelopNode,
    begin: usize,
}
struct DevelopDescription {
    subject: String,
    namespaces: Arc<HashMap<String, String>>,
    lang: String,
    space: String,
    attributes: Vec<Range<usize>>,
    elements: Vec<Range<usize>>,
    properties: Vec<String>,
}
struct DevelopInsertion {
    range: Range<usize>,
    before: String,
    after: String,
}
struct DevelopPacket {
    descriptions: Vec<DevelopDescription>,
    insertion: Option<DevelopInsertion>,
    bare: Option<Range<usize>>,
}
fn expanded<'a>(
    name: &'a str,
    ns: &'a HashMap<String, String>,
    attribute: bool,
) -> Result<(&'a str, &'a str)> {
    if let Some((prefix, local)) = name.split_once(':') {
        ensure!(
            !prefix.is_empty() && !local.is_empty() && !local.contains(':'),
            "invalid XML qualified name"
        );
        Ok((
            ns.get(prefix)
                .context(format!("unbound XML prefix {prefix}"))?,
            local,
        ))
    } else {
        ensure!(!name.is_empty(), "empty XML name");
        Ok((
            if attribute {
                ""
            } else {
                ns.get("").map_or("", String::as_str)
            },
            name,
        ))
    }
}
fn whole_attribute(text: &str, name: &str, value: &Range<usize>) -> Result<Range<usize>> {
    let mut start = value.start - 1;
    while start > 0 && text.as_bytes()[start - 1].is_ascii_whitespace() {
        start -= 1;
    }
    ensure!(
        start > 0 && text.as_bytes()[start - 1] == b'=',
        "invalid XML attribute span"
    );
    start -= 1;
    while start > 0 && text.as_bytes()[start - 1].is_ascii_whitespace() {
        start -= 1;
    }
    ensure!(
        text[..start].ends_with(name),
        "invalid XML attribute name span"
    );
    Ok(start - name.len()..value.end + 1)
}
/// Scan property ownership, rather than a list of known Camera Raw field names.
/// Namespace maps are shared until a declaration changes, including in large masks.
fn develop_packet(text: &str) -> Result<DevelopPacket> {
    ensure!(text.chars().all(xml_character), "invalid XML character");
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    reader.config_mut().check_comments = true;
    let mut stack: Vec<DevelopFrame> = Vec::new();
    let mut descriptions = Vec::<DevelopDescription>::new();
    let (mut roots, mut rdf_count, mut insertion, mut bare) = (0, 0, None, None);
    loop {
        let begin = reader.buffer_position() as usize;
        let event = reader.read_event().context("malformed develop XMP")?;
        let end = reader.buffer_position() as usize;
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let empty = matches!(event, Event::Empty(_));
                ensure!(stack.len() < 256, "XMP nesting exceeds merge limit");
                if stack.is_empty() {
                    roots += 1;
                    ensure!(roots == 1, "multiple XML roots");
                }
                let mut ns = stack
                    .last()
                    .map(|f| f.namespaces.clone())
                    .unwrap_or_else(|| Arc::new(HashMap::from([("xml".into(), XML.into())])));
                let raw = attrs(text, begin, end)?;
                // Check exact duplicate attributes, then expanded duplicates below.
                for attribute in element.attributes() {
                    attribute.context("invalid XML attributes")?;
                }
                for (name, range) in &raw {
                    let prefix = if name == "xmlns" {
                        Some("")
                    } else {
                        name.strip_prefix("xmlns:")
                    };
                    if let Some(prefix) = prefix {
                        let uri = quick_xml::escape::unescape(&text[range.clone()])?.into_owned();
                        ensure!(
                            !prefix.contains(':')
                                && prefix != "xmlns"
                                && uri != XMLNS
                                && (prefix.is_empty() || !uri.is_empty()),
                            "invalid XML namespace declaration"
                        );
                        ensure!(
                            (prefix == "xml") == (uri == XML),
                            "invalid reserved XML namespace"
                        );
                        Arc::make_mut(&mut ns).insert(prefix.into(), uri);
                    }
                }
                let name = std::str::from_utf8(element.name().as_ref())?.to_owned();
                let (uri, local) = expanded(&name, &ns, false)?;
                let parent = stack.last().map(|f| f.node);
                let node = if stack.is_empty() && uri == "adobe:ns:meta/" && local == "xmpmeta" {
                    DevelopNode::Meta
                } else if matches!(parent, None | Some(DevelopNode::Meta))
                    && uri == RDF
                    && local == "RDF"
                {
                    rdf_count += 1;
                    ensure!(rdf_count == 1, "multiple RDF containers");
                    if empty {
                        insertion = Some(DevelopInsertion {
                            range: end - 2..end,
                            before: ">".into(),
                            after: format!("</{name}>"),
                        });
                    }
                    DevelopNode::Rdf
                } else if matches!(parent, None | Some(DevelopNode::Rdf))
                    && uri == RDF
                    && local == "Description"
                {
                    DevelopNode::Description(descriptions.len())
                } else if let Some(DevelopNode::Description(index)) = parent {
                    if uri == CRS {
                        DevelopNode::Property(index, begin)
                    } else {
                        DevelopNode::Other
                    }
                } else {
                    ensure!(
                        !matches!(
                            parent,
                            None | Some(DevelopNode::Meta) | Some(DevelopNode::Rdf)
                        ),
                        "unsupported XMP/RDF root structure"
                    );
                    DevelopNode::Other
                };
                let mut lang = stack.last().map_or_else(String::new, |f| f.lang.clone());
                let mut space = stack
                    .last()
                    .map_or_else(|| "default".into(), |f| f.space.clone());
                let mut subject = String::new();
                let mut properties = Vec::new();
                let mut attributes = Vec::new();
                let mut seen = HashSet::new();
                for (attribute, range) in &raw {
                    if attribute == "xmlns" || attribute.starts_with("xmlns:") {
                        continue;
                    }
                    let (attribute_uri, attribute_local) = expanded(attribute, &ns, true)?;
                    ensure!(
                        seen.insert((attribute_uri, attribute_local)),
                        "duplicate expanded XML attribute"
                    );
                    let value = quick_xml::escape::unescape(&text[range.clone()])?;
                    ensure!(
                        value.chars().all(xml_character),
                        "invalid XML attribute character"
                    );
                    if attribute_uri == XML {
                        match attribute_local {
                            "lang" => lang = value.into_owned(),
                            "space" => {
                                ensure!(
                                    matches!(value.as_ref(), "default" | "preserve"),
                                    "invalid xml:space"
                                );
                                space = value.into_owned();
                            }
                            "base" => ensure!(
                                !matches!(
                                    node,
                                    DevelopNode::Meta
                                        | DevelopNode::Rdf
                                        | DevelopNode::Description(_)
                                ),
                                "inherited xml:base is unsupported in develop merge"
                            ),
                            _ => {}
                        }
                    } else if matches!(node, DevelopNode::Description(_)) {
                        if attribute_uri == RDF {
                            ensure!(
                                attribute_local == "about",
                                "unsupported RDF subject identifier"
                            );
                            subject = value.into_owned();
                        } else if attribute_uri == CRS {
                            attributes.push(whole_attribute(text, attribute, range)?);
                            properties.push(attribute_local.to_owned());
                        }
                    }
                }
                if let DevelopNode::Description(_) = node {
                    descriptions.push(DevelopDescription {
                        subject,
                        namespaces: ns.clone(),
                        lang: lang.clone(),
                        space: space.clone(),
                        attributes,
                        elements: Vec::new(),
                        properties,
                    });
                    if stack.is_empty() && empty {
                        bare = Some(begin..end);
                    }
                }
                if let DevelopNode::Property(index, _) = node {
                    descriptions[index].properties.push(local.to_owned());
                    if empty {
                        descriptions[index].elements.push(begin..end);
                    }
                }
                if !empty {
                    stack.push(DevelopFrame {
                        namespaces: ns,
                        lang,
                        space,
                        node,
                        begin,
                    });
                }
            }
            Event::End(_) => {
                let frame = stack.pop().context("unbalanced XML")?;
                match frame.node {
                    DevelopNode::Rdf => {
                        insertion = Some(DevelopInsertion {
                            range: begin..begin,
                            before: String::new(),
                            after: String::new(),
                        })
                    }
                    DevelopNode::Description(_) if stack.is_empty() => {
                        bare = Some(frame.begin..end)
                    }
                    DevelopNode::Property(index, start) => {
                        descriptions[index].elements.push(start..end)
                    }
                    _ => {}
                }
            }
            Event::Text(ref value) => {
                let decoded = value.decode()?;
                let unescaped = quick_xml::escape::unescape(&decoded)?;
                ensure!(
                    unescaped.chars().all(xml_character),
                    "invalid XML text character"
                );
                ensure!(
                    !stack.is_empty() || decoded.trim().is_empty(),
                    "text outside XML root"
                );
                if stack.last().is_some_and(|f| {
                    matches!(
                        f.node,
                        DevelopNode::Meta | DevelopNode::Rdf | DevelopNode::Description(_)
                    )
                }) {
                    ensure!(unescaped.trim().is_empty(), "text outside RDF property");
                }
            }
            Event::GeneralRef(ref reference) => {
                ensure!(!stack.is_empty(), "entity outside XML root");
                let escaped = format!("&{};", reference.decode()?);
                let value = quick_xml::escape::unescape(&escaped)?;
                ensure!(
                    value.chars().all(xml_character),
                    "invalid XML entity character"
                );
                if stack.last().is_some_and(|f| {
                    matches!(
                        f.node,
                        DevelopNode::Meta | DevelopNode::Rdf | DevelopNode::Description(_)
                    )
                }) {
                    ensure!(value.trim().is_empty(), "entity outside RDF property");
                }
            }
            Event::CData(_) => ensure!(
                stack.last().is_some_and(|f| !matches!(
                    f.node,
                    DevelopNode::Meta | DevelopNode::Rdf | DevelopNode::Description(_)
                )),
                "CDATA outside RDF property"
            ),
            Event::DocType(_) => bail!("XMP DTD unsupported in develop merge"),
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(
        roots == 1 && stack.is_empty() && (insertion.is_some() || bare.is_some()),
        "incomplete XMP/RDF packet"
    );
    Ok(DevelopPacket {
        descriptions,
        insertion,
        bare,
    })
}

/// Replace complete top-level Camera Raw properties, including unknown structures,
/// while retaining the source's rating, flags and every other metadata property.
/// Non-CRS source bytes are retained; imported properties receive their original
/// namespace context. Distinct RDF subjects must agree, and ambiguous Adobe CRS
/// duplicates or inherited xml:base are errors rather than silent reinterpretation.
pub fn merge_develop(source: &str, adobe: &str) -> Result<String> {
    let original = develop_packet(source).context("source develop packet")?;
    let developed = develop_packet(adobe).context("Adobe develop packet")?;
    let subjects: HashSet<_> = original
        .descriptions
        .iter()
        .map(|d| d.subject.as_str())
        .collect();
    let mut seen = HashSet::new();
    let mut imported = String::new();
    for description in &developed.descriptions {
        if description.properties.is_empty() {
            continue;
        }
        ensure!(
            subjects.is_empty() || subjects.contains(description.subject.as_str()),
            "Camera Raw RDF subject differs from source"
        );
        for property in &description.properties {
            ensure!(
                seen.insert((description.subject.as_str(), property.as_str())),
                "duplicate Adobe Camera Raw property {property}"
            );
        }
        let mut prefix = "fcRdf".to_owned();
        while description.namespaces.contains_key(&prefix) {
            prefix.push('_');
        }
        imported.push_str(&format!(
            "\n<{prefix}:Description xmlns:{prefix}=\"{RDF}\" {prefix}:about=\"{}\"",
            quick_xml::escape::escape(&description.subject)
        ));
        // An absent default namespace must override any default on the SOURCE.
        imported.push_str(&format!(
            " xmlns=\"{}\"",
            quick_xml::escape::escape(description.namespaces.get("").map_or("", String::as_str))
        ));
        let mut namespaces: Vec<_> = description
            .namespaces
            .iter()
            .filter(|(p, _)| !p.is_empty() && p.as_str() != "xml")
            .collect();
        namespaces.sort_by_key(|(p, _)| *p);
        for (namespace, uri) in namespaces {
            imported.push_str(&format!(
                " xmlns:{namespace}=\"{}\"",
                quick_xml::escape::escape(uri)
            ));
        }
        imported.push_str(&format!(
            " xml:lang=\"{}\" xml:space=\"{}\"",
            quick_xml::escape::escape(&description.lang),
            description.space
        ));
        for attribute in &description.attributes {
            imported.push(' ');
            imported.push_str(&adobe[attribute.clone()]);
        }
        if description.elements.is_empty() {
            imported.push_str("/>");
        } else {
            imported.push('>');
            for element in &description.elements {
                imported.push_str(&adobe[element.clone()]);
            }
            imported.push_str(&format!("</{prefix}:Description>"));
        }
    }
    let mut edits: Vec<_> = original
        .descriptions
        .iter()
        .flat_map(|d| d.attributes.iter().chain(&d.elements))
        .map(|range| (range.clone(), String::new()))
        .collect();
    if !imported.is_empty() {
        if let Some(insertion) = original.insertion {
            edits.push((
                insertion.range,
                insertion.before + &imported + &insertion.after,
            ));
        } else {
            let root = original.bare.context("missing source RDF root")?;
            edits.push((root.end..root.end, imported + "</fcRdf:RDF>"));
            edits.push((
                root.start..root.start,
                format!("<fcRdf:RDF xmlns:fcRdf=\"{RDF}\">"),
            ));
        }
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut merged = source.to_owned();
    for (range, value) in edits {
        merged.replace_range(range, &value);
    }
    develop_packet(&merged).context("merged develop packet")?;
    Ok(merged)
}
