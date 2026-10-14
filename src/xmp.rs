//! Byte-preserving edits: resolve XML namespace URIs, replace only value spans.
use anyhow::{Context, Result, bail, ensure};
use quick_xml::{Reader, events::Event};
use std::{collections::HashMap, fs, ops::Range, path::Path};

const XMP: &str = "http://ns.adobe.com/xap/1.0/";
const SR: &str = "urn:zrtc:syncratings:1/";
const TIFF: &str = "http://ns.adobe.com/tiff/1.0/";
const CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values {
    pub crop: Option<crate::crop::Crop>,
    /// Millionths of an EV; exact durable representation, valid range -5..5.
    pub exposure: Option<i32>,
    pub rating: Option<u8>,
    pub label: Option<String>,
    pub flag: Option<String>,
    pub orientation: Option<u16>,
}
#[derive(Clone, Debug, Default)]
pub struct Updates {
    pub crop: Option<crate::crop::Crop>,
    pub exposure: Option<i32>,
    pub rating: Option<u8>,
    pub label: Option<String>,
    pub flag: Option<String>,
    pub orientation: Option<u16>,
}
impl Updates {
    fn entries(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(c) = self.crop {
            out.push((
                "crs:HasCrop",
                if c.enabled { "True" } else { "False" }.into(),
            ));
            out.push(("crs:HasSettings", "True".into()));
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
            && self.crop.is_none()
    }
}
struct Document {
    spans: HashMap<String, Vec<Range<usize>>>,
    insert: usize,
    namespaces: HashMap<String, String>,
    namespace_repairs: Vec<(Range<usize>, String)>,
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
fn field(name: &str, ns: &HashMap<String, String>) -> Option<&'static str> {
    let (prefix, local) = name.split_once(':')?;
    match (ns.get(prefix)?.as_str(), local) {
        (XMP, "Rating") => Some("xmp:Rating"),
        (XMP, "Label") => Some("xmp:Label"),
        (SR, "Flag") => Some("sr:Flag"),
        (TIFF, "Orientation") => Some("tiff:Orientation"),
        (CRS, "Exposure2012") => Some("crs:Exposure2012"),
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
                    if let Some(p) = name.strip_prefix("xmlns:") {
                        ns.insert(
                            p.to_owned(),
                            quick_xml::escape::unescape(&text[range.clone()])?.into_owned(),
                        );
                    }
                }
                let name = std::str::from_utf8(e.name().as_ref())?.to_owned();
                let description = !stack.iter().any(|frame| frame.2)
                    && name.split_once(':').is_some_and(|(p, l)| {
                        l == "Description" && ns.get(p).is_some_and(|v| v == RDF)
                    });
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
                        if let Some(k) = field(&name, &ns) {
                            spans.entry(k.to_owned()).or_default().push(range);
                        }
                    }
                }
                ensure!(
                    stack.last().and_then(|s| s.1).is_none(),
                    "nested markup in XMP scalar field"
                );
                let target = if stack.last().is_some_and(|s| s.2) {
                    field(&name, &ns)
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
pub fn read(raw: &Path) -> Result<Values> {
    let text = if crate::formats::is_jpeg(raw) {
        crate::jpeg::read_packet(raw)?
    } else {
        match fs::read_to_string(raw.with_extension("xmp")) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        }
    };
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
pub(crate) fn parse_values(text: &str) -> Result<Values> {
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
    Ok(Values {
        crop,
        exposure: get("crs:Exposure2012")?
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && (-5.0..=5.0).contains(v))
            .map(|v| (v * 1_000_000.).round() as i32),
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
    let mut edits = doc.namespace_repairs;
    let mut additions = String::new();
    let mut ns = doc.namespaces;
    for (key, value) in updates.entries() {
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
    let template = format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"{RDF}\"><rdf:Description rdf:about=\"\" xmlns:xmp=\"{XMP}\" xmlns:tiff=\"{TIFF}\" xmlns:sr=\"{SR}\"/></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>\n"
    );
    let out = patch(original.as_deref().unwrap_or(&template), updates)?;
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
