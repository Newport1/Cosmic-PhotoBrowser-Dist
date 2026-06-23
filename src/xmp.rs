use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorLabel {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl ColorLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            ColorLabel::Red => "Red",
            ColorLabel::Yellow => "Yellow",
            ColorLabel::Green => "Green",
            ColorLabel::Blue => "Blue",
            ColorLabel::Purple => "Purple",
        }
    }

    /// Case-insensitive match of supported color-label names.
    ///
    /// Returns `Option` for compatibility with existing call sites rather than
    /// implementing `std::str::FromStr`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        let t = s.trim().to_ascii_lowercase();
        match t.as_str() {
            "red" => Some(ColorLabel::Red),
            "yellow" => Some(ColorLabel::Yellow),
            "green" => Some(ColorLabel::Green),
            "blue" => Some(ColorLabel::Blue),
            "purple" => Some(ColorLabel::Purple),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn all() -> [ColorLabel; 5] {
        [
            ColorLabel::Red,
            ColorLabel::Yellow,
            ColorLabel::Green,
            ColorLabel::Blue,
            ColorLabel::Purple,
        ]
    }
}

/// Compact cull metadata (rating + color label + reject flag) cached per image/RAW path
/// for grid display and filtering. Populated from sidecar via read_sidecar_cull.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CullMeta {
    pub rating: Option<u8>,
    pub label: Option<ColorLabel>,
    pub rejected: bool,
}

// (color helpers moved to crate::view::color — xmp is now cosmic-free)

/// Read cull meta (rating/label/rejected) from the sidecar for `image_path`.
/// Returns default (no rating/label, not rejected) if no sidecar or unparseable.
pub fn read_sidecar_cull(image_path: &Path) -> CullMeta {
    let side = sidecar_path(image_path);
    match std::fs::read_to_string(&side) {
        Ok(xml) => {
            let d = parse_xmp_sidecar(&xml);
            CullMeta {
                rating: d.rating,
                label: d.label,
                rejected: d.rejected,
            }
        }
        Err(_) => CullMeta::default(),
    }
}

/// Parsed Adobe Camera Raw (`crs:`) develop settings from an XMP sidecar.
///
/// Values are stored as raw slider/settings values, such as exposure in stops
/// and -100..+100 adjustment sliders.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DevelopParams {
    pub exposure: Option<f32>,    // crs:Exposure2012 (stops, e.g. +0.50)
    pub contrast: Option<i32>,    // crs:Contrast2012
    pub highlights: Option<i32>,  // crs:Highlights2012
    pub shadows: Option<i32>,     // crs:Shadows2012
    pub whites: Option<i32>,      // crs:Whites2012
    pub blacks: Option<i32>,      // crs:Blacks2012
    pub temperature: Option<i32>, // crs:Temperature (Kelvin, RAW WB)
    pub incremental_temperature: Option<i32>, // crs:IncrementalTemperature (delta, applied-to-rendered)
    pub tint: Option<i32>,                    // crs:Tint
    pub incremental_tint: Option<i32>,        // crs:IncrementalTint
    pub vibrance: Option<i32>,                // crs:Vibrance
    pub saturation: Option<i32>,              // crs:Saturation
    pub tone_curve: Vec<(f32, f32)>, // crs:ToneCurvePV2012 points (x,y 0..=255), empty if absent
    pub process_version: Option<String>, // crs:ProcessVersion (e.g. "11.0")
}

/// Parsed, display-oriented view of an XMP sidecar. Read-only; no writes here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct XmpData {
    /// xmp:Rating, clamped to 0..=5. None if absent/unparseable or if xmp:Rating="-1" (reject).
    pub rating: Option<u8>,
    /// xmp:Label as one of Red/Yellow/Green/Blue/Purple; None if absent/empty/unknown.
    pub label: Option<ColorLabel>,
    /// True when xmp:Rating == "-1" (reject). Rating and reject are mutually exclusive in the parsed view.
    pub rejected: bool,
    /// xmp:CreatorTool (e.g. camera/firmware or editor that wrote the sidecar).
    pub creator_tool: Option<String>,
    /// True if the sidecar carries Camera-Raw develop adjustments (crs:/crd: namespace
    /// attributes present) — i.e. the RAW has edits described in the sidecar. (We only
    /// DETECT edits here; rendering the developed look is out of scope / deferred to v3.)
    pub has_develop_edits: bool,
    /// dc:subject keywords (rdf:Bag), in document order.
    pub keywords: Vec<String>,
    /// tiff:Model (camera model), for the info panel.
    pub model: Option<String>,
    /// exif:ExposureTime (e.g. "1/2500").
    pub exposure_time: Option<String>,
    /// exif:FNumber (raw string, e.g. "63/10").
    pub f_number: Option<String>,
    /// exif:ISOSpeedRatings / exif:PhotographicSensitivity if present.
    pub iso: Option<String>,
    /// Parsed `crs:` develop settings (if any). Empty/default when none present.
    pub develop: DevelopParams,
}

/// Parse XMP sidecar text into XmpData. Pure; never panics on malformed input
/// (returns Default / None for missing fields).
pub fn parse_xmp_sidecar(xml: &str) -> XmpData {
    let mut data = XmpData {
        has_develop_edits: xml.contains("crs:") || xml.contains("crd:"),
        ..XmpData::default()
    };

    if let Some(r) = xmp_attr(xml, "xmp:Rating") {
        // Parse as i8 first so -1 is not silently dropped by unsigned parse.
        if let Ok(n) = r.trim().parse::<i8>() {
            if n == -1 {
                data.rejected = true;
                data.rating = None;
            } else if (0..=5).contains(&n) {
                data.rating = Some(n as u8);
                data.rejected = false;
            }
            // else: leave rating=None, rejected=false (default)
        }
    }

    if let Some(lab) = xmp_attr(xml, "xmp:Label") {
        data.label = ColorLabel::from_str(lab);
    }

    if let Some(ct) = xmp_attr(xml, "xmp:CreatorTool") {
        let t = ct.trim();
        if !t.is_empty() {
            data.creator_tool = Some(t.to_owned());
        }
    }

    if let Some(m) = xmp_attr(xml, "tiff:Model") {
        let t = m.trim();
        if !t.is_empty() {
            data.model = Some(t.to_owned());
        }
    }

    if let Some(e) = xmp_attr(xml, "exif:ExposureTime") {
        let t = e.trim();
        if !t.is_empty() {
            data.exposure_time = Some(t.to_owned());
        }
    }

    if let Some(f) = xmp_attr(xml, "exif:FNumber") {
        let t = f.trim();
        if !t.is_empty() {
            data.f_number = Some(t.to_owned());
        }
    }

    data.keywords = parse_keywords(xml);

    for name in ["exif:ISOSpeedRatings", "exif:PhotographicSensitivity"] {
        if let Some(i) = xmp_attr(xml, name) {
            let t = i.trim();
            if !t.is_empty() {
                data.iso = Some(t.to_owned());
                break;
            }
        }
    }

    data.develop = parse_develop_params(xml);

    data
}

/// Find `name="..."` (attribute form) OR `<name>...</name>` (element form) and return the value.
#[allow(dead_code)]
fn xmp_attr<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    // Attribute form: name="value"  -- exact prefix match on name=" prevents
    // matching a longer attribute such as xmp:RatingPercent when name is xmp:Rating.
    let attr_needle = format!("{}=\"", name);
    if let Some(pos) = xml.find(&attr_needle) {
        let start = pos + attr_needle.len();
        if let Some(len) = xml[start..].find('"') {
            return Some(&xml[start..start + len]);
        }
    }

    // Element form: <name ...>value</name>
    // Supports both direct text and nested content (e.g. rdf:Seq/rdf:li for ISOSpeedRatings).
    // Uses first non-whitespace text node inside the element.
    let open_needle = format!("<{}", name);
    if let Some(pos) = xml.find(&open_needle) {
        let after = &xml[pos + open_needle.len()..];
        if let Some(gt_rel) = after.find('>') {
            let inner = &after[gt_rel + 1..];
            if let Some(end_rel) = inner.find(&format!("</{}>", name)) {
                let content = &inner[..end_rel];
                if let Some(txt) = first_text_node(content) {
                    return Some(txt);
                }
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }
    }

    None
}

/// Escape XML element text: & first, then < and >. (li content needs no quote-escaping.)
fn xml_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Unescape the entities we (and common writers) emit. &amp; LAST to avoid double-unescape.
fn xml_unescape_text(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Extract the first non-whitespace text node from content that may contain nested XML tags.
/// Bytes scan keeps it allocation-free and panic-free.
#[allow(dead_code)]
fn first_text_node(s: &str) -> Option<&str> {
    let mut in_tag = false;
    let mut start: Option<usize> = None;
    for (i, b) in s.bytes().enumerate() {
        if b == b'<' {
            if let Some(st) = start {
                let cand = &s[st..i];
                let t = cand.trim();
                if !t.is_empty() {
                    return Some(t);
                }
            }
            in_tag = true;
            start = None;
        } else if b == b'>' {
            in_tag = false;
        } else if !in_tag && start.is_none() && !b.is_ascii_whitespace() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        let cand = &s[st..];
        let t = cand.trim();
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

/// Extract dc:subject keywords (the rdf:li texts within the FIRST <dc:subject>…</dc:subject>),
/// XML-unescaped, trimmed, empties skipped, in document order. Empty if no dc:subject.
fn parse_keywords(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Locate the dc:subject element span (open form). Self-closing dc:subject has no items.
    let Some(s_open) = xml.find("<dc:subject") else {
        return out;
    };
    // start scanning after the dc:subject opening tag's '>'
    let Some(gt_rel) = xml[s_open..].find('>') else {
        return out;
    };
    let scan_start = s_open + gt_rel + 1;
    let end = xml[scan_start..]
        .find("</dc:subject>")
        .map(|r| scan_start + r)
        .unwrap_or(xml.len());
    let mut rest = &xml[scan_start..end];
    while let Some(li_open) = rest.find("<rdf:li") {
        let after_open_tag = match rest[li_open..].find('>') {
            Some(r) => li_open + r + 1,
            None => break,
        };
        let Some(li_close_rel) = rest[after_open_tag..].find("</rdf:li>") else {
            break;
        };
        let text = &rest[after_open_tag..after_open_tag + li_close_rel];
        let val = xml_unescape_text(text).trim().to_owned();
        if !val.is_empty() {
            out.push(val);
        }
        rest = &rest[after_open_tag + li_close_rel + "</rdf:li>".len()..];
    }
    out
}

fn parse_develop_params(xml: &str) -> DevelopParams {
    // Scalar helpers: trim, then parse (handles leading '+' for both f32 and i32).
    let f = |name: &str| xmp_attr(xml, name).and_then(|v| v.trim().parse::<f32>().ok());
    let i = |name: &str| xmp_attr(xml, name).and_then(|v| v.trim().parse::<i32>().ok());
    DevelopParams {
        exposure: f("crs:Exposure2012"),
        contrast: i("crs:Contrast2012"),
        highlights: i("crs:Highlights2012"),
        shadows: i("crs:Shadows2012"),
        whites: i("crs:Whites2012"),
        blacks: i("crs:Blacks2012"),
        temperature: i("crs:Temperature"),
        incremental_temperature: i("crs:IncrementalTemperature"),
        tint: i("crs:Tint"),
        incremental_tint: i("crs:IncrementalTint"),
        vibrance: i("crs:Vibrance"),
        saturation: i("crs:Saturation"),
        tone_curve: parse_tone_curve(xml),
        process_version: xmp_attr(xml, "crs:ProcessVersion")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty()),
    }
}

fn parse_tone_curve(xml: &str) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    let Some(s_open) = xml.find("<crs:ToneCurvePV2012") else {
        return out;
    };
    let Some(gt_rel) = xml[s_open..].find('>') else {
        return out;
    };
    let scan_start = s_open + gt_rel + 1;
    let end = xml[scan_start..]
        .find("</crs:ToneCurvePV2012>")
        .map(|r| scan_start + r)
        .unwrap_or(xml.len());
    let mut rest = &xml[scan_start..end];
    while let Some(li_open) = rest.find("<rdf:li") {
        let after = match rest[li_open..].find('>') {
            Some(r) => li_open + r + 1,
            None => break,
        };
        let Some(close_rel) = rest[after..].find("</rdf:li>") else {
            break;
        };
        let text = rest[after..after + close_rel].trim();
        if let Some((x, y)) = text.split_once(',') {
            if let (Ok(x), Ok(y)) = (x.trim().parse::<f32>(), y.trim().parse::<f32>()) {
                out.push((x, y));
            }
        }
        rest = &rest[after + close_rel + "</rdf:li>".len()..];
    }
    out
}

/// The XMP sidecar path for a RAW/image file: same path with the extension replaced by `xmp`.
/// e.g. `/p/HYD_8820.NEF` -> `/p/HYD_8820.xmp`, matching common Adobe/digiKam sidecar behavior.
#[allow(dead_code)]
pub fn sidecar_path(image_path: &Path) -> PathBuf {
    image_path.with_extension("xmp")
}

/// Read the rating from the sidecar for `image_path`, if the sidecar exists & parses. None otherwise.
#[allow(dead_code)]
pub fn read_sidecar_rating(image_path: &Path) -> Option<u8> {
    let side = sidecar_path(image_path);
    match std::fs::read_to_string(&side) {
        Ok(xml) => parse_xmp_sidecar(&xml).rating,
        Err(_) => None,
    }
}

/// Read (rating, parsed data) from the sidecar for an image. Both None if no sidecar.
pub fn read_loupe_sidecar(image_path: &Path) -> (Option<u8>, Option<XmpData>) {
    let side = sidecar_path(image_path);
    match std::fs::read_to_string(&side) {
        Ok(xml) => {
            let d = parse_xmp_sidecar(&xml);
            (d.rating, Some(d))
        }
        Err(_) => (None, None),
    }
}

/// Read the full parsed XMP sidecar (including label/rejected) if present. None if no sidecar.
#[allow(dead_code)]
pub fn read_sidecar_xmp(image_path: &Path) -> Option<XmpData> {
    let side = sidecar_path(image_path);
    match std::fs::read_to_string(&side) {
        Ok(xml) => Some(parse_xmp_sidecar(&xml)),
        Err(_) => None,
    }
}

/// Set the XMP `xmp:Rating` (0..=5; 0 = unrated) in the sidecar for `image_path`.
/// - If the sidecar exists: update the existing xmp:Rating value in place, PRESERVING all other
///   content; if it has no xmp:Rating, INSERT the attribute into the first <rdf:Description ...> tag
///   (adding the xmp: namespace decl if missing).
/// - If the sidecar does NOT exist: create a minimal valid XMP packet carrying the rating.
///
/// NEVER touches `image_path` itself. Atomic write (temp-in-dir + rename).
#[allow(dead_code)]
pub fn write_sidecar_rating(image_path: &Path, rating: u8) -> std::io::Result<()> {
    let rating = rating.clamp(0, 5) as i8;
    set_rating_value(image_path, rating)
}

/// Set/clear the color label in the sidecar. None clears it (writes xmp:Label="").
/// Other content preserved. Sidecar-only; atomic.
#[allow(dead_code)]
pub fn write_sidecar_label(image_path: &Path, label: Option<ColorLabel>) -> std::io::Result<()> {
    let side = sidecar_path(image_path);

    if side.exists() {
        let xml = std::fs::read_to_string(&side)?;
        let updated = update_label_in_xmp(&xml, label);
        atomic_write(&side, &updated)
    } else {
        let minimal = make_minimal_xmp_label(label);
        atomic_write(&side, &minimal)
    }
}

/// Set or clear reject. true => xmp:Rating="-1"; false => xmp:Rating="0" (clears reject, unrated).
/// Shares the rating attribute update path with write_sidecar_rating.
#[allow(dead_code)]
pub fn write_sidecar_reject(image_path: &Path, rejected: bool) -> std::io::Result<()> {
    let val: i8 = if rejected { -1 } else { 0 };
    set_rating_value(image_path, val)
}

/// Set the dc:subject keywords in the sidecar for `image_path` to exactly `keywords`
/// (replacing any existing keywords; empty list clears them). Preserves other sidecar content.
/// NEVER touches `image_path` itself. Atomic write.
#[allow(dead_code)]
pub fn write_sidecar_keywords(image_path: &Path, keywords: &[String]) -> std::io::Result<()> {
    let side = sidecar_path(image_path);
    if side.exists() {
        let xml = std::fs::read_to_string(&side)?;
        let updated = update_keywords_in_xmp(&xml, keywords);
        atomic_write(&side, &updated)
    } else {
        let minimal = make_minimal_xmp_keywords(keywords);
        atomic_write(&side, &minimal)
    }
}

/// Read dc:subject keywords from the sidecar for `image_path` (empty if none / no sidecar).
#[allow(dead_code)]
pub fn read_sidecar_keywords(image_path: &Path) -> Vec<String> {
    let side = sidecar_path(image_path);
    match std::fs::read_to_string(&side) {
        Ok(xml) => parse_xmp_sidecar(&xml).keywords,
        Err(_) => Vec::new(),
    }
}

/// Add `kw` (trimmed) to the sidecar keywords for `image_path` if not already present
/// (case-insensitive). Returns the resulting keyword list. No-op (still returns the list) for
/// empty/whitespace `kw` or an already-present keyword. Sidecar-only; original never touched.
#[allow(dead_code)]
pub fn add_keyword(image_path: &Path, kw: &str) -> std::io::Result<Vec<String>> {
    let mut kws = read_sidecar_keywords(image_path);
    let t = kw.trim();
    if !t.is_empty() && !kws.iter().any(|k| k.eq_ignore_ascii_case(t)) {
        kws.push(t.to_owned());
        write_sidecar_keywords(image_path, &kws)?;
    }
    Ok(kws)
}

/// Remove `kw` (case-insensitive, trimmed) from the sidecar keywords for `image_path`.
/// Returns the resulting list. No write if nothing changed. Sidecar-only.
#[allow(dead_code)]
pub fn remove_keyword(image_path: &Path, kw: &str) -> std::io::Result<Vec<String>> {
    let mut kws = read_sidecar_keywords(image_path);
    let t = kw.trim();
    let before = kws.len();
    kws.retain(|k| !k.eq_ignore_ascii_case(t));
    if kws.len() != before {
        write_sidecar_keywords(image_path, &kws)?;
    }
    Ok(kws)
}

/// Internal: set xmp:Rating to any allowed i8 value (-1 for reject, 0..=5 for stars/unrated).
/// Used by both write_sidecar_rating and write_sidecar_reject.
fn set_rating_value(image_path: &Path, value: i8) -> std::io::Result<()> {
    let side = sidecar_path(image_path);

    if side.exists() {
        let xml = std::fs::read_to_string(&side)?;
        let updated = update_rating_in_xmp(&xml, value);
        atomic_write(&side, &updated)
    } else {
        let minimal = make_minimal_xmp(value);
        atomic_write(&side, &minimal)
    }
}

/// Atomic write: write to a temp file in the SAME directory as target, then rename.
/// This ensures an interrupted write cannot corrupt an existing sidecar.
fn atomic_write(target: &Path, data: &str) -> std::io::Result<()> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // Use pid-suffixed temp name in the target dir. Collisions are overwritten (harmless for our use).
    let tmp_name = format!(
        ".{}.tmp-{}",
        target
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "xmp".into()),
        std::process::id()
    );
    let tmp = parent.join(tmp_name);
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, target)?;
    Ok(())
}

/// Build the minimal XMP packet with the given rating (i8 to allow -1 for reject).
fn make_minimal_xmp(rating: i8) -> String {
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{}"/>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#,
        rating
    )
}

/// Build minimal XMP carrying only an xmp:Label (for write_sidecar_label on new sidecar).
fn make_minimal_xmp_label(label: Option<ColorLabel>) -> String {
    let val = label.map_or("", |l| l.as_str());
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Label="{}"/>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#,
        val
    )
}

/// Update an existing XMP sidecar text to set xmp:Rating = N (N may be -1), preserving all other content.
/// Strategy (simple string surgery, robust to the parseable forms):
/// - Prefer replacing value in xmp:Rating="..." attribute form
/// - Else replace inner text of <xmp:Rating>...</xmp:Rating>
/// - Else insert ` xmp:Rating="N"` (and xmlns:xmp if missing in the tag) into the FIRST <rdf:Description ...> opening tag.
fn update_rating_in_xmp(xml: &str, rating: i8) -> String {
    let r = rating.to_string();

    // 1. Attribute form: xmp:Rating="..."  -- replace only the value
    // Use a find that matches the exact attr start to avoid partials.
    let attr_prefix = r#"xmp:Rating=""#;
    if let Some(pos) = xml.find(attr_prefix) {
        let val_start = pos + attr_prefix.len();
        if let Some(quote_rel) = xml[val_start..].find('"') {
            let mut out = String::with_capacity(xml.len());
            out.push_str(&xml[..val_start]);
            out.push_str(&r);
            out.push_str(&xml[val_start + quote_rel..]);
            return out;
        }
    }

    // 2. Element form: <xmp:Rating>VALUE</xmp:Rating>
    let elem_open = "<xmp:Rating>";
    if let Some(pos) = xml.find(elem_open) {
        let after_open = pos + elem_open.len();
        if let Some(close_rel) = xml[after_open..].find("</xmp:Rating>") {
            let mut out = String::with_capacity(xml.len());
            out.push_str(&xml[..after_open]);
            out.push_str(&r);
            out.push_str(&xml[after_open + close_rel..]);
            return out;
        }
    }

    // 3. Insert into the first <rdf:Description ...> tag.
    // Insert the attr (and ns decl if absent from this tag) immediately before the closing '>'.
    if let Some(desc_pos) = xml.find("<rdf:Description") {
        if let Some(gt_rel) = xml[desc_pos..].find('>') {
            let gt_abs = desc_pos + gt_rel;
            let tag_open = &xml[desc_pos..gt_abs]; // up to but not including '>'
            let has_xmp_ns = tag_open.contains(r#"xmlns:xmp=""#) || tag_open.contains("xmlns:xmp=");
            let mut out = String::with_capacity(xml.len() + 80);
            out.push_str(&xml[..desc_pos]);
            out.push_str(tag_open);
            if !has_xmp_ns {
                out.push_str(r#" xmlns:xmp="http://ns.adobe.com/xap/1.0/""#);
            }
            out.push_str(&format!(r#" xmp:Rating="{}""#, rating));
            out.push_str(&xml[gt_abs..]); // the '>' and rest
            return out;
        }
    }

    // Fallback for sidecars without rdf:Description (unusual): append a minimal description
    // inside the first rdf:RDF if possible. This still tries to preserve other content.
    if let Some(rdf_pos) = xml.find("<rdf:RDF") {
        if let Some(gt_rel) = xml[rdf_pos..].find('>') {
            let after = rdf_pos + gt_rel + 1;
            let mut out = String::with_capacity(xml.len() + 100);
            out.push_str(&xml[..after]);
            out.push_str(&format!(
                r#"
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{}"/>"#,
                rating
            ));
            out.push_str(&xml[after..]);
            return out;
        }
    }

    // No rdf:Description or rdf:RDF to edit (malformed/foreign sidecar): preserve it unchanged
    // rather than risk corrupting it. The rating simply isn't applied in this rare case.
    xml.to_owned()
}

/// Update an existing XMP sidecar text to set (or clear) xmp:Label, preserving all other content.
/// For label=None we write the empty string value (readers treat "" as no label).
/// Mirrors the surgery strategy used for Rating.
fn update_label_in_xmp(xml: &str, label: Option<ColorLabel>) -> String {
    let val = label.map_or(String::new(), |l| l.as_str().to_owned());

    // 1. Attribute form: xmp:Label="..." -- replace only the value
    let attr_prefix = r#"xmp:Label=""#;
    if let Some(pos) = xml.find(attr_prefix) {
        let val_start = pos + attr_prefix.len();
        if let Some(quote_rel) = xml[val_start..].find('"') {
            let mut out = String::with_capacity(xml.len());
            out.push_str(&xml[..val_start]);
            out.push_str(&val);
            out.push_str(&xml[val_start + quote_rel..]);
            return out;
        }
    }

    // 2. Element form: <xmp:Label>VALUE</xmp:Label>
    let elem_open = "<xmp:Label>";
    if let Some(pos) = xml.find(elem_open) {
        let after_open = pos + elem_open.len();
        if let Some(close_rel) = xml[after_open..].find("</xmp:Label>") {
            let mut out = String::with_capacity(xml.len());
            out.push_str(&xml[..after_open]);
            out.push_str(&val);
            out.push_str(&xml[after_open + close_rel..]);
            return out;
        }
    }

    // 3. Insert into the first <rdf:Description ...> tag.
    if let Some(desc_pos) = xml.find("<rdf:Description") {
        if let Some(gt_rel) = xml[desc_pos..].find('>') {
            let gt_abs = desc_pos + gt_rel;
            let tag_open = &xml[desc_pos..gt_abs];
            let has_xmp_ns = tag_open.contains(r#"xmlns:xmp=""#) || tag_open.contains("xmlns:xmp=");
            let mut out = String::with_capacity(xml.len() + 80);
            out.push_str(&xml[..desc_pos]);
            out.push_str(tag_open);
            if !has_xmp_ns {
                out.push_str(r#" xmlns:xmp="http://ns.adobe.com/xap/1.0/""#);
            }
            out.push_str(&format!(r#" xmp:Label="{}""#, val));
            out.push_str(&xml[gt_abs..]);
            return out;
        }
    }

    // Fallback append inside rdf:RDF
    if let Some(rdf_pos) = xml.find("<rdf:RDF") {
        if let Some(gt_rel) = xml[rdf_pos..].find('>') {
            let after = rdf_pos + gt_rel + 1;
            let mut out = String::with_capacity(xml.len() + 100);
            out.push_str(&xml[..after]);
            out.push_str(&format!(
                r#"
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Label="{}"/>"#,
                val
            ));
            out.push_str(&xml[after..]);
            return out;
        }
    }

    xml.to_owned()
}

const DC_NS: &str = r#"xmlns:dc="http://purl.org/dc/elements/1.1/""#;

/// The <dc:subject><rdf:Bag>…</rdf:Bag></dc:subject> block for these keywords (escaped, empties skipped),
/// or "" if no non-empty keywords. Indented to sit as a child of rdf:Description.
fn dc_subject_block(keywords: &[String]) -> String {
    let items: Vec<&String> = keywords.iter().filter(|k| !k.trim().is_empty()).collect();
    if items.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n   <dc:subject>\n    <rdf:Bag>\n");
    for k in items {
        s.push_str(&format!(
            "     <rdf:li>{}</rdf:li>\n",
            xml_escape_text(k.trim())
        ));
    }
    s.push_str("    </rdf:Bag>\n   </dc:subject>");
    s
}

/// Build the minimal XMP packet with dc:subject keywords.
fn make_minimal_xmp_keywords(keywords: &[String]) -> String {
    let block = dc_subject_block(keywords);
    // Even with no keywords we emit a valid (empty) description so the sidecar exists.
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">{block}
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
    )
}

/// Set dc:subject to exactly `keywords` (replacing any existing dc:subject), preserving all other content.
/// Empty keywords removes the dc:subject element.
fn update_keywords_in_xmp(xml: &str, keywords: &[String]) -> String {
    let block = dc_subject_block(keywords); // "" if empty

    // CASE 1: an existing <dc:subject ...> … replace its whole span.
    if let Some(start) = xml.find("<dc:subject") {
        // Find the opening tag's '>'.
        if let Some(gt_rel) = xml[start..].find('>') {
            let gt_abs = start + gt_rel; // index of '>'
            let self_closing = xml.as_bytes().get(gt_abs.wrapping_sub(1)) == Some(&b'/');
            let span_end = if self_closing {
                gt_abs + 1 // <dc:subject .../> ends here
            } else if let Some(close_rel) = xml[gt_abs..].find("</dc:subject>") {
                gt_abs + close_rel + "</dc:subject>".len()
            } else {
                gt_abs + 1 // malformed; only drop the open tag
            };
            let mut out = String::with_capacity(xml.len() + block.len());
            out.push_str(&xml[..start]);
            out.push_str(block.trim_start_matches('\n')); // block already has leading indent newline; keep tidy
            out.push_str(&xml[span_end..]);
            return out;
        }
    }

    // From here: no existing dc:subject. If nothing to add, return unchanged.
    if block.is_empty() {
        return xml.to_owned();
    }

    // CASE 2: insert into the FIRST <rdf:Description …>, adding xmlns:dc if absent.
    if let Some(desc_pos) = xml.find("<rdf:Description") {
        if let Some(gt_rel) = xml[desc_pos..].find('>') {
            let gt_abs = desc_pos + gt_rel; // index of '>'
            let self_closing = xml.as_bytes().get(gt_abs.wrapping_sub(1)) == Some(&b'/');
            // The opening-tag text WITHOUT the trailing '>' (and without trailing '/' if self-closing):
            let tag_text_end = if self_closing { gt_abs - 1 } else { gt_abs };
            let tag_text = &xml[desc_pos..tag_text_end];
            let needs_dc = !tag_text.contains("xmlns:dc=");
            let mut out = String::with_capacity(xml.len() + block.len() + 60);
            out.push_str(&xml[..desc_pos]);
            out.push_str(tag_text);
            if needs_dc {
                out.push(' ');
                out.push_str(DC_NS);
            }
            out.push('>'); // open the description
            out.push_str(&block); // the dc:subject child (leading newline+indent already in block)
            out.push_str("\n  </rdf:Description>");
            // Skip the original opening tag (whether it was `...>` or `.../>`):
            out.push_str(&xml[gt_abs + 1..]);
            return out;
        }
    }

    // CASE 3 (fallback): no rdf:Description but an rdf:RDF — add a fresh description (mirror rating fallback).
    if let Some(rdf_pos) = xml.find("<rdf:RDF") {
        if let Some(gt_rel) = xml[rdf_pos..].find('>') {
            let after = rdf_pos + gt_rel + 1;
            let mut out = String::with_capacity(xml.len() + block.len() + 80);
            out.push_str(&xml[..after]);
            out.push_str(&format!(
                "\n  <rdf:Description rdf:about=\"\" {DC_NS}>{block}\n  </rdf:Description>"
            ));
            out.push_str(&xml[after..]);
            return out;
        }
    }

    // No anchor to edit: preserve unchanged rather than corrupt.
    xml.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 7.0">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
    xmlns:exif="http://ns.adobe.com/exif/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:crd="http://ns.adobe.com/camera-raw-defaults/1.0/"
   xmp:Rating="3"
   xmp:CreatorTool="NIKON Z 6_2 Ver.01.70   "
   tiff:Model="NIKON Z 6_2"
   exif:ExposureTime="1/2500"
   exif:FNumber="63/10"
   crs:HasSettings="False"
   crs:Exposure2012="+0.50"
   crs:Contrast2012="25"
   crs:ProcessVersion="11.0">
   <exif:ISOSpeedRatings>
    <rdf:Seq>
     <rdf:li>100</rdf:li>
    </rdf:Seq>
   </exif:ISOSpeedRatings>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    #[test]
    fn parses_representative_acr_sample() {
        let x = parse_xmp_sidecar(SAMPLE);
        assert_eq!(x.rating, Some(3));
        assert_eq!(x.creator_tool, Some("NIKON Z 6_2 Ver.01.70".into()));
        assert_eq!(x.model, Some("NIKON Z 6_2".into()));
        assert_eq!(x.exposure_time, Some("1/2500".into()));
        assert_eq!(x.f_number, Some("63/10".into()));
        assert_eq!(x.iso, Some("100".into()));
        assert!(x.has_develop_edits);
        // develop values now present from extended fixture
        assert_eq!(x.develop.exposure, Some(0.5));
        assert_eq!(x.develop.contrast, Some(25));
        assert_eq!(x.develop.process_version, Some("11.0".to_string()));
    }

    #[test]
    fn has_develop_edits_false_without_crs_crd() {
        let no_dev = r#"<rdf:Description xmp:Rating="2" tiff:Model="X"></rdf:Description>"#;
        assert!(!parse_xmp_sidecar(no_dev).has_develop_edits);
        assert!(!parse_xmp_sidecar("").has_develop_edits);
    }

    #[test]
    fn rating_out_of_range_is_none() {
        assert_eq!(parse_xmp_sidecar(r#"xmp:Rating="9""#).rating, None);
        assert_eq!(parse_xmp_sidecar(r#"xmp:Rating="99""#).rating, None);
        assert_eq!(parse_xmp_sidecar(r#"xmp:Rating="-1""#).rating, None);
        assert_eq!(parse_xmp_sidecar(r#"xmp:Rating="foo""#).rating, None);
    }

    #[test]
    fn missing_rating_is_none() {
        assert_eq!(
            parse_xmp_sidecar("<rdf:Description></rdf:Description>").rating,
            None
        );
        assert_eq!(parse_xmp_sidecar("").rating, None);
        assert_eq!(parse_xmp_sidecar(r#"xmp:CreatorTool="ACR""#).rating, None);
    }

    #[test]
    fn element_form_rating() {
        assert_eq!(
            parse_xmp_sidecar(r#"<xmp:Rating>4</xmp:Rating>"#).rating,
            Some(4)
        );
        assert_eq!(
            parse_xmp_sidecar(r#"<xmp:Rating>  5  </xmp:Rating>"#).rating,
            Some(5)
        );
        assert_eq!(
            parse_xmp_sidecar(r#"<xmp:Rating>0</xmp:Rating>"#).rating,
            Some(0)
        );
    }

    #[test]
    fn empty_or_garbage_yields_default() {
        let d = XmpData::default();
        assert_eq!(parse_xmp_sidecar(""), d);
        assert_eq!(parse_xmp_sidecar("garbage <<>> !! no tags at all"), d);
        assert_eq!(parse_xmp_sidecar(r#"<<<  >>> malformed"#), d);
    }

    #[test]
    fn trims_all_string_fields() {
        let s = r#"xmp:CreatorTool="  ACR 1.2  " tiff:Model="  CAM  " exif:ExposureTime=" 1/100 " exif:FNumber=" 8/1  " exif:ISOSpeedRatings=" 200  ""#;
        let x = parse_xmp_sidecar(s);
        assert_eq!(x.creator_tool, Some("ACR 1.2".into()));
        assert_eq!(x.model, Some("CAM".into()));
        assert_eq!(x.exposure_time, Some("1/100".into()));
        assert_eq!(x.f_number, Some("8/1".into()));
        assert_eq!(x.iso, Some("200".into()));
    }

    #[test]
    fn does_not_match_longer_attr_names() {
        // xmp:Rating must not accidentally take value from xmp:RatingPercent if present
        let s = r#"xmp:Rating="3" xmp:RatingPercent="75""#;
        let x = parse_xmp_sidecar(s);
        assert_eq!(x.rating, Some(3));
    }

    // --- NEW TESTS for C5-M2 write (added only; existing tests untouched) ---

    #[test]
    fn sidecar_path_basic() {
        use std::path::Path;
        assert_eq!(sidecar_path(Path::new("foo.NEF")), Path::new("foo.xmp"));
        assert_eq!(sidecar_path(Path::new("foo.nef")), Path::new("foo.xmp"));
        assert_eq!(sidecar_path(Path::new("a.b.NEF")), Path::new("a.b.xmp"));
        assert_eq!(
            sidecar_path(Path::new("/x/y/IMG.ARW")),
            Path::new("/x/y/IMG.xmp")
        );
        assert_eq!(sidecar_path(Path::new("noext")), Path::new("noext.xmp"));
    }

    #[test]
    fn write_then_read_roundtrip() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("HYD_8820.NEF"); // the image itself need not exist
                                                   // write 5
        write_sidecar_rating(&img, 5).unwrap();
        assert_eq!(read_sidecar_rating(&img), Some(5));
        // overwrite with 2
        write_sidecar_rating(&img, 2).unwrap();
        assert_eq!(read_sidecar_rating(&img), Some(2));
        // also check sidecar file name
        let expected_side = dir.path().join("HYD_8820.xmp");
        assert!(expected_side.exists());
    }

    #[test]
    fn update_preserves_other_content() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("IMG.CR2");
        let side = sidecar_path(&img);
        // Pre-create a sidecar with rating + sentinel other content
        let initial = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
   xmp:Rating="1"
   tiff:Model="SENTINEL_CAM"/>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&side, initial).unwrap();
        // Now update rating to 4
        write_sidecar_rating(&img, 4).unwrap();
        let after = std::fs::read_to_string(&side).unwrap();
        assert!(
            after.contains("SENTINEL_CAM"),
            "other content must be preserved: {}",
            after
        );
        assert_eq!(read_sidecar_rating(&img), Some(4));
        // rating changed
        assert!(after.contains(r#"xmp:Rating="4""#));
    }

    #[test]
    fn insert_rating_when_absent() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("NO_RATING.NEF");
        let side = sidecar_path(&img);
        // sidecar exists with rdf:Description but NO rating at all
        let initial = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" tiff:Model="CANON"/>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&side, initial).unwrap();
        write_sidecar_rating(&img, 3).unwrap();
        assert_eq!(read_sidecar_rating(&img), Some(3));
        let after = std::fs::read_to_string(&side).unwrap();
        assert!(after.contains(r#"xmp:Rating="3""#));
        // ns should have been added too
        assert!(after.contains(r#"xmlns:xmp="http://ns.adobe.com/xap/1.0/""#));
        // original content preserved
        assert!(after.contains("CANON"));
    }

    /// Ensures rating writes modify only the `.xmp` sidecar, never the original image bytes.
    #[test]
    fn write_rating_never_modifies_the_raw() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img_nef = dir.path().join("img.NEF");
        let raw_bytes: &[u8] = b"RAWBYTES-DO-NOT-TOUCH";
        std::fs::write(&img_nef, raw_bytes).unwrap();
        let orig_hash = {
            // simple checksum without extra deps
            let b = std::fs::read(&img_nef).unwrap();
            b.iter()
                .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64))
        };

        // Call write on the NEF path (this must create .xmp only)
        write_sidecar_rating(&img_nef, 5).unwrap();

        // sidecar created and has rating
        let side = sidecar_path(&img_nef);
        assert!(side.exists(), "sidecar must exist");
        assert_eq!(read_sidecar_rating(&img_nef), Some(5));

        // RAW must be byte-for-byte untouched
        let after_bytes = std::fs::read(&img_nef).unwrap();
        let after_hash = after_bytes
            .iter()
            .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64));
        assert_eq!(after_bytes, raw_bytes, "RAW file bytes must be identical");
        assert_eq!(after_hash, orig_hash, "RAW content hash must match");

        // sanity: sidecar should not contain the raw marker
        let side_str = std::fs::read_to_string(&side).unwrap();
        assert!(!side_str.contains("RAWBYTES"));
    }

    #[test]
    fn clamps_out_of_range() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("clamp.NEF");
        // 9 should clamp to 5
        write_sidecar_rating(&img, 9).unwrap();
        assert_eq!(read_sidecar_rating(&img), Some(5));
        // negative not possible (u8) but 0 is accepted
        write_sidecar_rating(&img, 0).unwrap();
        assert_eq!(read_sidecar_rating(&img), Some(0));
    }

    // --- Color label and reject tests ---

    #[test]
    fn color_label_from_str_roundtrips_and_case_insensitive() {
        for c in ColorLabel::all() {
            assert_eq!(ColorLabel::from_str(c.as_str()), Some(c));
            assert_eq!(ColorLabel::from_str(&c.as_str().to_lowercase()), Some(c));
            assert_eq!(ColorLabel::from_str(&c.as_str().to_uppercase()), Some(c));
            // with whitespace
            assert_eq!(
                ColorLabel::from_str(&format!("  {}  ", c.as_str())),
                Some(c)
            );
        }
        assert_eq!(ColorLabel::from_str(""), None);
        assert_eq!(ColorLabel::from_str("foo"), None);
        assert_eq!(ColorLabel::from_str("redish"), None);
        assert_eq!(ColorLabel::from_str("GreenX"), None);
    }

    #[test]
    fn parse_label_and_reject() {
        // label
        assert_eq!(
            parse_xmp_sidecar(r#"xmp:Label="Green""#).label,
            Some(ColorLabel::Green)
        );
        assert_eq!(
            parse_xmp_sidecar(r#"xmp:Label=" blue ""#).label,
            Some(ColorLabel::Blue)
        );
        assert_eq!(parse_xmp_sidecar(r#"xmp:Label="unknown""#).label, None);
        assert_eq!(parse_xmp_sidecar(r#"xmp:Label="""#).label, None);
        assert_eq!(parse_xmp_sidecar("").label, None);

        // rating -1 => rejected
        let rj = parse_xmp_sidecar(r#"xmp:Rating="-1""#);
        assert!(rj.rejected);
        assert_eq!(rj.rating, None);

        // normal rating => not rejected
        let r3 = parse_xmp_sidecar(r#"xmp:Rating="3""#);
        assert!(!r3.rejected);
        assert_eq!(r3.rating, Some(3));

        // out of range not -1
        assert!(!parse_xmp_sidecar(r#"xmp:Rating="9""#).rejected);
        assert_eq!(parse_xmp_sidecar(r#"xmp:Rating="9""#).rating, None);
        assert!(!parse_xmp_sidecar(r#"xmp:Rating="foo""#).rejected);
    }

    #[test]
    fn write_label_then_read_roundtrip() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("test.NEF");
        // set Blue
        write_sidecar_label(&img, Some(ColorLabel::Blue)).unwrap();
        let d = read_sidecar_xmp(&img).expect("sidecar");
        assert_eq!(d.label, Some(ColorLabel::Blue));
        assert!(!d.rejected);

        // overwrite to Red
        write_sidecar_label(&img, Some(ColorLabel::Red)).unwrap();
        let d2 = read_sidecar_xmp(&img).unwrap();
        assert_eq!(d2.label, Some(ColorLabel::Red));

        // clear with None -> ""
        write_sidecar_label(&img, None).unwrap();
        let d3 = read_sidecar_xmp(&img).unwrap();
        assert_eq!(d3.label, None);
        // sidecar still exists with empty
        let txt = std::fs::read_to_string(sidecar_path(&img)).unwrap();
        assert!(txt.contains(r#"xmp:Label="""#) || txt.contains("xmp:Label=\"\""));
    }

    #[test]
    fn write_reject_then_read_roundtrip() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("rej.NEF");
        write_sidecar_reject(&img, true).unwrap();
        let d = read_sidecar_xmp(&img).expect("sidecar");
        assert!(d.rejected);
        assert_eq!(d.rating, None);

        // clear reject
        write_sidecar_reject(&img, false).unwrap();
        let d2 = read_sidecar_xmp(&img).unwrap();
        assert!(!d2.rejected);
        // rating set to 0
        assert_eq!(d2.rating, Some(0));

        // writing a normal rating after should clear reject semantics
        write_sidecar_rating(&img, 4).unwrap();
        let d3 = read_sidecar_xmp(&img).unwrap();
        assert!(!d3.rejected);
        assert_eq!(d3.rating, Some(4));
    }

    /// Ensures label writes modify only the `.xmp` sidecar, never the original bytes.
    #[test]
    fn write_label_never_modifies_the_raw() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img_nef = dir.path().join("img.NEF");
        let raw_bytes: &[u8] = b"RAWBYTES-DO-NOT-TOUCH-LABEL";
        std::fs::write(&img_nef, raw_bytes).unwrap();
        let orig_hash = {
            let b = std::fs::read(&img_nef).unwrap();
            b.iter()
                .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64))
        };

        write_sidecar_label(&img_nef, Some(ColorLabel::Purple)).unwrap();

        let side = sidecar_path(&img_nef);
        assert!(side.exists(), "sidecar must exist");
        assert_eq!(
            read_sidecar_xmp(&img_nef).and_then(|d| d.label),
            Some(ColorLabel::Purple)
        );

        let after_bytes = std::fs::read(&img_nef).unwrap();
        let after_hash = after_bytes
            .iter()
            .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64));
        assert_eq!(
            after_bytes, raw_bytes,
            "RAW file bytes must be identical for label write"
        );
        assert_eq!(
            after_hash, orig_hash,
            "RAW content hash must match for label write"
        );

        let side_str = std::fs::read_to_string(&side).unwrap();
        assert!(!side_str.contains("RAWBYTES-DO-NOT-TOUCH-LABEL"));
    }

    /// Ensures reject writes modify only the `.xmp` sidecar, never the original bytes.
    #[test]
    fn write_reject_never_modifies_the_raw() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img_nef = dir.path().join("img.NEF");
        let raw_bytes: &[u8] = b"RAWBYTES-DO-NOT-TOUCH-REJECT";
        std::fs::write(&img_nef, raw_bytes).unwrap();
        let orig_hash = {
            let b = std::fs::read(&img_nef).unwrap();
            b.iter()
                .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64))
        };

        write_sidecar_reject(&img_nef, true).unwrap();

        let side = sidecar_path(&img_nef);
        assert!(side.exists());
        let d = read_sidecar_xmp(&img_nef).unwrap();
        assert!(d.rejected);
        assert_eq!(d.rating, None);

        let after_bytes = std::fs::read(&img_nef).unwrap();
        let after_hash = after_bytes
            .iter()
            .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64));
        assert_eq!(
            after_bytes, raw_bytes,
            "RAW file bytes must be identical for reject write"
        );
        assert_eq!(
            after_hash, orig_hash,
            "RAW content hash must match for reject write"
        );

        let side_str = std::fs::read_to_string(&side).unwrap();
        assert!(!side_str.contains("RAWBYTES-DO-NOT-TOUCH-REJECT"));
    }

    #[test]
    fn write_label_preserves_existing_rating_and_sentinel() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("IMG.NEF");
        let side = sidecar_path(&img);
        let initial = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
   xmp:Rating="2"
   tiff:Model="SENTINEL_FOR_LABEL"/>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&side, initial).unwrap();

        write_sidecar_label(&img, Some(ColorLabel::Yellow)).unwrap();
        let after = std::fs::read_to_string(&side).unwrap();
        assert!(after.contains("SENTINEL_FOR_LABEL"));
        assert!(after.contains(r#"xmp:Rating="2""#));
        assert!(after.contains(r#"xmp:Label="Yellow""#));
        // rating still readable
        assert_eq!(read_sidecar_rating(&img), Some(2));
    }

    #[test]
    fn write_rating_preserves_existing_label_and_sentinel() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("IMG.NEF");
        let side = sidecar_path(&img);
        let initial = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
   xmp:Label="Green"
   tiff:Model="SENTINEL_FOR_RATING"/>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&side, initial).unwrap();

        write_sidecar_rating(&img, 5).unwrap();
        let after = std::fs::read_to_string(&side).unwrap();
        assert!(after.contains("SENTINEL_FOR_RATING"));
        assert!(after.contains(r#"xmp:Label="Green""#));
        assert!(after.contains(r#"xmp:Rating="5""#));
    }

    #[test]
    fn cull_meta_default_is_unrated_unlabeled_not_rejected() {
        let c = CullMeta::default();
        assert_eq!(c.rating, None);
        assert_eq!(c.label, None);
        assert!(!c.rejected);
    }

    #[test]
    fn read_sidecar_cull_defaults_when_no_sidecar() {
        use std::path::Path;
        let c = read_sidecar_cull(Path::new("/nonexistent/NOPE.NEF"));
        assert_eq!(c, CullMeta::default());
    }

    #[test]
    fn read_sidecar_cull_parses_label_and_reject_from_sidecar() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("cull.NEF");
        // write label
        write_sidecar_label(&img, Some(ColorLabel::Green)).unwrap();
        let c = read_sidecar_cull(&img);
        assert_eq!(c.label, Some(ColorLabel::Green));
        assert!(!c.rejected);
        // now reject (note: reject sets rating -1, label remains)
        write_sidecar_reject(&img, true).unwrap();
        let c2 = read_sidecar_cull(&img);
        assert!(c2.rejected);
        assert_eq!(c2.label, Some(ColorLabel::Green)); // label preserved
        assert_eq!(c2.rating, None);
    }

    // --- Cat-M7b: dc:subject keywords tests ---

    #[test]
    fn keywords_roundtrip_no_existing_sidecar() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("IMG.NEF");
        write_sidecar_keywords(&img, &["beach".into(), "sunset".into()]).unwrap();
        assert_eq!(
            read_sidecar_keywords(&img),
            vec!["beach".to_string(), "sunset".to_string()]
        );
        let side = sidecar_path(&img);
        let txt = std::fs::read_to_string(&side).unwrap();
        assert!(txt.contains("<dc:subject>"));
        assert!(txt.matches("<rdf:li>").count() == 2);
    }

    #[test]
    fn keywords_xml_escaping_roundtrips() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("ESC.NEF");
        write_sidecar_keywords(&img, &["a&b".into(), "x<y>z".into()]).unwrap();
        let side = sidecar_path(&img);
        let txt = std::fs::read_to_string(&side).unwrap();
        assert!(txt.contains("a&amp;b"));
        assert!(txt.contains("x&lt;y&gt;z"));
        assert_eq!(
            read_sidecar_keywords(&img),
            vec!["a&b".to_string(), "x<y>z".to_string()]
        );
    }

    #[test]
    fn write_keywords_preserves_rating_and_label() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("KEEP.NEF");
        write_sidecar_rating(&img, 5).unwrap();
        write_sidecar_label(&img, Some(ColorLabel::Red)).unwrap();
        write_sidecar_keywords(&img, &["k".into()]).unwrap();
        let d = parse_xmp_sidecar(&std::fs::read_to_string(sidecar_path(&img)).unwrap());
        assert_eq!(d.rating, Some(5));
        assert_eq!(d.label, Some(ColorLabel::Red));
        assert_eq!(d.keywords, vec!["k".to_string()]);
    }

    #[test]
    fn write_rating_preserves_keywords() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("KEEP2.NEF");
        write_sidecar_keywords(&img, &["k".into()]).unwrap();
        write_sidecar_rating(&img, 3).unwrap();
        let d = parse_xmp_sidecar(&std::fs::read_to_string(sidecar_path(&img)).unwrap());
        assert_eq!(d.keywords, vec!["k".to_string()]);
        assert_eq!(d.rating, Some(3));
    }

    #[test]
    fn replace_existing_keywords() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("REP.NEF");
        write_sidecar_keywords(&img, &["a".into()]).unwrap();
        write_sidecar_keywords(&img, &["b".into(), "c".into()]).unwrap();
        assert_eq!(
            read_sidecar_keywords(&img),
            vec!["b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn clear_keywords_removes_dc_subject() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("CLR.NEF");
        write_sidecar_rating(&img, 2).unwrap();
        write_sidecar_keywords(&img, &["k".into()]).unwrap();
        write_sidecar_keywords(&img, &[]).unwrap();
        assert!(read_sidecar_keywords(&img).is_empty());
        let d = parse_xmp_sidecar(&std::fs::read_to_string(sidecar_path(&img)).unwrap());
        assert_eq!(d.rating, Some(2));
        // no dc:subject block should remain
        let txt = std::fs::read_to_string(sidecar_path(&img)).unwrap();
        assert!(!txt.contains("<dc:subject"));
    }

    #[test]
    fn parse_keywords_handles_indented_bag() {
        let xml = r#"<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>one</rdf:li>
     <rdf:li> two </rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>"#;
        let d = parse_xmp_sidecar(xml);
        assert_eq!(d.keywords, vec!["one".to_string(), "two".to_string()]);
    }

    /// Ensures keyword writes modify only the `.xmp` sidecar, never the original image bytes.
    #[test]
    fn write_keywords_never_modifies_the_raw() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img_nef = dir.path().join("img.NEF");
        let raw_bytes: &[u8] = b"RAWBYTES-DO-NOT-TOUCH-KEYWORDS";
        std::fs::write(&img_nef, raw_bytes).unwrap();
        let orig_hash = {
            let b = std::fs::read(&img_nef).unwrap();
            b.iter()
                .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64))
        };

        write_sidecar_keywords(&img_nef, &["kw".into()]).unwrap();

        let side = sidecar_path(&img_nef);
        assert!(side.exists(), "sidecar must exist");
        assert_eq!(read_sidecar_keywords(&img_nef), vec!["kw".to_string()]);

        let after_bytes = std::fs::read(&img_nef).unwrap();
        let after_hash = after_bytes
            .iter()
            .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64));
        assert_eq!(after_bytes, raw_bytes, "RAW file bytes must be identical");
        assert_eq!(after_hash, orig_hash, "RAW content hash must match");

        let side_str = std::fs::read_to_string(&side).unwrap();
        assert!(!side_str.contains("RAWBYTES-DO-NOT-TOUCH-KEYWORDS"));
    }

    #[test]
    fn add_keyword_adds_and_dedups_case_insensitive() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("KW1.NEF");
        let res = add_keyword(&img, "Beach").unwrap();
        assert_eq!(res, vec!["Beach".to_string()]);
        // case-insens dup: should not add, keep original casing
        let res2 = add_keyword(&img, "beach").unwrap();
        assert_eq!(res2, vec!["Beach".to_string()]);
        let res3 = add_keyword(&img, "Sunset").unwrap();
        assert_eq!(res3, vec!["Beach".to_string(), "Sunset".to_string()]);
        // persisted
        assert_eq!(
            read_sidecar_keywords(&img),
            vec!["Beach".to_string(), "Sunset".to_string()]
        );
    }

    #[test]
    fn add_keyword_creates_sidecar_when_absent() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("KWNEW.NEF");
        let side = sidecar_path(&img);
        assert!(!side.exists());
        let res = add_keyword(&img, "first").unwrap();
        assert_eq!(res, vec!["first".to_string()]);
        assert!(side.exists());
        assert_eq!(read_sidecar_keywords(&img), vec!["first".to_string()]);
    }

    #[test]
    fn remove_keyword_removes_case_insensitive() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img = dir.path().join("KW2.NEF");
        write_sidecar_keywords(&img, &["Beach".into(), "Sunset".into()]).unwrap();
        let res = remove_keyword(&img, "beach").unwrap();
        assert_eq!(res, vec!["Sunset".to_string()]);
        assert_eq!(read_sidecar_keywords(&img), vec!["Sunset".to_string()]);
        // no-op remove
        let res2 = remove_keyword(&img, "missing").unwrap();
        assert_eq!(res2, vec!["Sunset".to_string()]);
    }

    #[test]
    fn add_keyword_never_modifies_the_raw() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img_nef = dir.path().join("img.NEF");
        let raw_bytes: &[u8] = b"RAWBYTES-DO-NOT-TOUCH-ADDKW";
        std::fs::write(&img_nef, raw_bytes).unwrap();
        let orig_hash = {
            let b = std::fs::read(&img_nef).unwrap();
            b.iter()
                .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64))
        };

        let _ = add_keyword(&img_nef, "kw").unwrap();

        let side = sidecar_path(&img_nef);
        assert!(side.exists(), "sidecar must exist");
        assert_eq!(read_sidecar_keywords(&img_nef), vec!["kw".to_string()]);

        let after_bytes = std::fs::read(&img_nef).unwrap();
        let after_hash = after_bytes
            .iter()
            .fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64));
        assert_eq!(after_bytes, raw_bytes, "RAW file bytes must be identical");
        assert_eq!(after_hash, orig_hash, "RAW content hash must match");

        let side_str = std::fs::read_to_string(&side).unwrap();
        assert!(!side_str.contains("RAWBYTES-DO-NOT-TOUCH-ADDKW"));
    }

    // --- RAW develop parsing tests ---

    #[test]
    fn parse_develop_params_reads_scalars() {
        let xml = r#"<rdf:Description
            xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
            crs:Exposure2012="+0.50"
            crs:Contrast2012="+25"
            crs:Shadows2012="-10"
            crs:Temperature="5500"
            crs:Vibrance="15"
            crs:ProcessVersion="11.0"
            xmp:Rating="0"
        />"#;
        let d = parse_xmp_sidecar(xml);
        assert_eq!(d.develop.exposure, Some(0.5));
        assert_eq!(d.develop.contrast, Some(25));
        assert_eq!(d.develop.shadows, Some(-10));
        assert_eq!(d.develop.temperature, Some(5500));
        assert_eq!(d.develop.vibrance, Some(15));
        assert_eq!(d.develop.process_version, Some("11.0".to_string()));
        // unset field is None
        assert_eq!(d.develop.whites, None);
        // has_develop_edits still set (by presence of crs:)
        assert!(d.has_develop_edits);
    }

    #[test]
    fn parse_tone_curve_reads_points() {
        let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
   <crs:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>128, 140</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        let d = parse_xmp_sidecar(xml);
        assert_eq!(
            d.develop.tone_curve,
            vec![(0.0, 0.0), (128.0, 140.0), (255.0, 255.0)]
        );
    }

    #[test]
    fn parse_develop_params_absent_is_default() {
        // plain rating-only sidecar, no crs: develop fields (but may have crs: for has flag? use none)
        let xml = r#"<rdf:Description xmp:Rating="3" xmlns:xmp="http://ns.adobe.com/xap/1.0/"/>"#;
        let d = parse_xmp_sidecar(xml);
        assert_eq!(d.develop, DevelopParams::default());
        assert_eq!(d.develop.exposure, None);
        assert!(d.develop.tone_curve.is_empty());
        // no crs: so has_develop_edits false here
        assert!(!d.has_develop_edits);
    }

    #[test]
    fn parse_develop_element_form_scalars() {
        // confirm xmp_attr element form works for crs: scalars too
        let xml =
            r#"<crs:Exposure2012>0.30</crs:Exposure2012><crs:Contrast2012>-5</crs:Contrast2012>"#;
        let d = parse_xmp_sidecar(xml);
        assert_eq!(d.develop.exposure, Some(0.30));
        assert_eq!(d.develop.contrast, Some(-5));
    }
}
