//! Lightroom / Camera Raw import: reads the `crs:` settings from an .xmp sidecar or preset.
//! shortcut: values map slider-for-slider; Ferrotype's maths differs from Adobe's, so results are close, not identical.
//! White balance is only taken from presets' relative Temp/Tint (absolute kelvin needs the camera's as-shot value).
use crate::develop::Params;

/// A setting in attribute form (`crs:Name="v"`) or element form (`<crs:Name>v</crs:Name>`).
fn get(x: &str, name: &str) -> Option<String> {
    let a = format!("crs:{name}=\"");
    if let Some(i) = x.find(&a) {
        let r = &x[i + a.len()..];
        return Some(r[..r.find('"')?].to_string());
    }
    let o = format!("<crs:{name}>");
    let i = x.find(&o)?;
    let r = &x[i + o.len()..];
    Some(r[..r.find('<')?].trim().to_string())
}

fn num(x: &str, name: &str) -> Option<f32> {
    get(x, name)?.trim().parse().ok()
}

/// Points of a tone curve list (`<rdf:li>x, y</rdf:li>`, 0..255) scaled to 0..1.
fn curve(x: &str, name: &str) -> Vec<[f32; 2]> {
    let (open, close) = (format!("<crs:{name}>"), format!("</crs:{name}>"));
    let Some(body) = x.find(&open).and_then(|i| x[i..].find(&close).map(|j| &x[i..i + j])) else { return vec![] };
    let pts: Vec<[f32; 2]> = body.split("<rdf:li>").skip(1).filter_map(|li| {
        let (a, b) = li.split('<').next()?.split_once(',')?;
        Some([a.trim().parse::<f32>().ok()? / 255.0, b.trim().parse::<f32>().ok()? / 255.0])
    }).collect();
    // A straight 0,0 → 255,255 line is "no curve".
    if pts.len() == 2 && pts[0] == [0.0, 0.0] && pts[1] == [1.0, 1.0] { vec![] } else { pts }
}

/// Settings from Lightroom XMP text, and the preset's name when it has one.
pub fn parse(x: &str) -> (Option<String>, Params) {
    let mut p = Params::default();
    let set = |v: &mut f32, name: &str| {
        if let Some(n) = num(x, name) {
            *v = n;
        }
    };
    for (v, n) in [
        (&mut p.exposure, "Exposure2012"), (&mut p.contrast, "Contrast2012"), (&mut p.highlights, "Highlights2012"), (&mut p.shadows, "Shadows2012"),
        (&mut p.whites, "Whites2012"), (&mut p.blacks, "Blacks2012"), (&mut p.texture, "Texture"), (&mut p.clarity, "Clarity2012"), (&mut p.dehaze, "Dehaze"),
        (&mut p.vibrance, "Vibrance"), (&mut p.saturation, "Saturation"), (&mut p.c_sh, "ParametricShadows"), (&mut p.c_da, "ParametricDarks"),
        (&mut p.c_li, "ParametricLights"), (&mut p.c_hi, "ParametricHighlights"), (&mut p.g_blend, "ColorGradeBlending"), (&mut p.g_bal, "SplitToningBalance"),
        (&mut p.sh_amt, "Sharpness"), (&mut p.sh_rad, "SharpenRadius"), (&mut p.sh_det, "SharpenDetail"), (&mut p.sh_mask, "SharpenEdgeMasking"),
        (&mut p.nr_lum, "LuminanceSmoothing"), (&mut p.nr_det, "LuminanceNoiseReductionDetail"), (&mut p.nr_col, "ColorNoiseReduction"),
        (&mut p.o_dist, "LensManualDistortionAmount"), (&mut p.o_vig, "VignetteAmount"), (&mut p.o_fringe, "DefringePurpleAmount"),
        (&mut p.t_vert, "PerspectiveVertical"), (&mut p.t_hor, "PerspectiveHorizontal"), (&mut p.t_scale, "PerspectiveScale"),
        (&mut p.vig, "PostCropVignetteAmount"), (&mut p.vig_mid, "PostCropVignetteMidpoint"), (&mut p.vig_round, "PostCropVignetteRoundness"),
        (&mut p.vig_fea, "PostCropVignetteFeather"), (&mut p.vig_hi, "PostCropVignetteHighlightContrast"), (&mut p.gr_amt, "GrainAmount"),
        (&mut p.gr_size, "GrainSize"), (&mut p.gr_rough, "GrainFrequency"), (&mut p.cal_sh, "ShadowTint"), (&mut p.tint, "IncrementalTint"),
    ] {
        set(v, n);
    }
    if let Some(t) = num(x, "IncrementalTemperature") {
        p.temp = 5500.0 + t * 30.0;
    }
    p.t_rot = num(x, "CropAngle").unwrap_or(0.0) + num(x, "PerspectiveRotate").unwrap_or(0.0);
    for (b, name) in ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"].into_iter().enumerate() {
        for (k, kind) in ["Hue", "Saturation", "Luminance"].into_iter().enumerate() {
            set(&mut p.hsl[b][k], &format!("{kind}Adjustment{name}"));
        }
    }
    for (z, zone) in ["Shadow", "Midtone", "Highlight", "Global"].into_iter().enumerate() {
        set(&mut p.grade[z][0], &format!("ColorGrade{zone}Hue"));
        set(&mut p.grade[z][1], &format!("ColorGrade{zone}Sat"));
        set(&mut p.grade[z][2], &format!("ColorGrade{zone}Lum"));
    }
    // Older split toning, when there is no colour grading.
    if num(x, "ColorGradeShadowSat").is_none() {
        set(&mut p.grade[0][0], "SplitToningShadowHue");
        set(&mut p.grade[0][1], "SplitToningShadowSaturation");
        set(&mut p.grade[2][0], "SplitToningHighlightHue");
        set(&mut p.grade[2][1], "SplitToningHighlightSaturation");
    }
    for (j, c) in ["Red", "Green", "Blue"].into_iter().enumerate() {
        set(&mut p.cal[j][0], &format!("{c}Hue"));
        set(&mut p.cal[j][1], &format!("{c}Saturation"));
    }
    if get(x, "HasCrop").as_deref() == Some("True") {
        p.crop = [num(x, "CropLeft").unwrap_or(0.0), num(x, "CropTop").unwrap_or(0.0), 1.0 - num(x, "CropRight").unwrap_or(1.0), 1.0 - num(x, "CropBottom").unwrap_or(1.0)];
    }
    if get(x, "ConvertToGrayscale").as_deref() == Some("True") {
        p.saturation = -100.0;
    }
    for (i, n) in ["ToneCurvePV2012", "ToneCurvePV2012Red", "ToneCurvePV2012Green", "ToneCurvePV2012Blue"].into_iter().enumerate() {
        p.curves[i] = curve(x, n);
    }
    let name = get(x, "Name").filter(|n| !n.is_empty()).or_else(|| {
        // <crs:Name><rdf:Alt><rdf:li xml:lang="x-default">Name</rdf:li>…
        let i = x.find("<crs:Name>")?;
        let li = &x[i..];
        let start = li.find("<rdf:li")? + li[li.find("<rdf:li")?..].find('>')? + 1;
        Some(li[start..start + li[start..].find('<')?].to_string())
    });
    let (r, l) = rating_label(x);
    apply_rating_label(&mut p, r, l);
    (name, p)
}

pub const LABELS: [&str; 6] = ["", "Red", "Yellow", "Green", "Blue", "Purple"];

/// A standard XMP attribute or element, such as `xmp:Rating`.
fn std_get(x: &str, name: &str) -> Option<String> {
    let a = format!("{name}=\"");
    if let Some(i) = x.find(&a) {
        let r = &x[i + a.len()..];
        return Some(r[..r.find('"')?].to_string());
    }
    let o = format!("<{name}>");
    let i = x.find(&o)?;
    let r = &x[i + o.len()..];
    Some(r[..r.find('<')?].trim().to_string())
}

/// `xmp:Rating` (-1 = rejected, as darktable and Bridge write it) and `xmp:Label`.
pub fn rating_label(x: &str) -> (Option<i32>, Option<u8>) {
    let r = std_get(x, "xmp:Rating").and_then(|v| v.trim().parse::<f32>().ok()).map(|v| v as i32);
    let l = std_get(x, "xmp:Label").map(|v| LABELS.iter().position(|n| n.eq_ignore_ascii_case(v.trim())).unwrap_or(0) as u8);
    (r, l)
}

pub fn apply_rating_label(p: &mut Params, r: Option<i32>, l: Option<u8>) {
    match r {
        Some(-1) => p.flag = -1,
        Some(r) => {
            p.rating = r.clamp(0, 5) as u8;
            if p.flag == -1 {
                p.flag = 0;
            }
        }
        None => {}
    }
    if let Some(l) = l {
        p.label = l;
    }
}

/// Set (or add) one attribute on the first rdf:Description, declaring the xmp namespace if needed.
fn set_attr(x: &mut String, name: &str, value: &str) {
    let a = format!("{name}=\"");
    if let Some(i) = x.find(&a) {
        let start = i + a.len();
        if let Some(len) = x[start..].find('"') {
            x.replace_range(start..start + len, value);
            return;
        }
    }
    // Element form (<xmp:Rating>3</xmp:Rating>).
    let (o, c) = (format!("<{name}>"), format!("</{name}>"));
    if let (Some(i), Some(j)) = (x.find(&o), x.find(&c)) {
        x.replace_range(i + o.len()..j, value);
        return;
    }
    if let Some(i) = x.find("<rdf:Description") {
        let at = i + "<rdf:Description".len();
        let ns = if x.contains("xmlns:xmp=") { "" } else { " xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"" };
        x.insert_str(at, &format!("{ns} {name}=\"{value}\""));
    }
}

/// XMP text with this rating, reject flag and label, keeping everything else in `existing` (e.g. Lightroom's edits).
/// None when nothing needs writing.
pub fn with_rating_label(existing: Option<&str>, p: &Params) -> Option<String> {
    let rating = if p.flag == -1 { -1 } else { p.rating as i32 };
    let label = LABELS[p.label.min(5) as usize];
    match existing {
        Some(x) => {
            let (r, l) = rating_label(x);
            // A label outside Lightroom's five (Bridge's "Approved", "To Do") reads as none; only rewrite the label
            // when ours really differs, so changing the stars keeps it.
            let label_changed = l.unwrap_or(0) != p.label.min(5);
            if r.unwrap_or(0) == rating && !label_changed {
                return None;
            }
            let mut x = x.to_string();
            set_attr(&mut x, "xmp:Rating", &rating.to_string());
            if label_changed {
                set_attr(&mut x, "xmp:Label", label);
            }
            Some(x)
        }
        None if rating == 0 && label.is_empty() => None,
        None => Some(format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"{rating}\" xmp:Label=\"{label}\"/>\n </rdf:RDF>\n</x:xmpmeta>\n"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lightroom_settings() {
        let x = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:Exposure2012="+0.50" crs:Contrast2012="+12" crs:HueAdjustmentBlue="-20"
            crs:ColorGradeShadowHue="210" crs:ColorGradeShadowSat="15" crs:HasCrop="True" crs:CropLeft="0.1" crs:CropRight="0.9"
            crs:CropTop="0" crs:CropBottom="1" crs:ConvertToGrayscale="False">
            <crs:Name><rdf:Alt><rdf:li xml:lang="x-default">Moody blue</rdf:li></rdf:Alt></crs:Name>
            <crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 20</rdf:li><rdf:li>128, 128</rdf:li><rdf:li>255, 240</rdf:li></rdf:Seq></crs:ToneCurvePV2012>
            </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (name, p) = parse(x);
        assert_eq!(name.as_deref(), Some("Moody blue"));
        assert_eq!((p.exposure, p.contrast, p.hsl[5][0], p.grade[0]), (0.5, 12.0, -20.0, [210.0, 15.0, 0.0]));
        assert!((p.crop[0] - 0.1).abs() < 1e-6 && (p.crop[2] - 0.1).abs() < 1e-6);
        assert_eq!(p.curves[0].len(), 3);
        assert!((p.curves[0][0][1] - 20.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn ratings_and_labels_round_trip_and_keep_lightroom_settings() {
        let p = Params { rating: 4, label: 3, ..Default::default() };
        let fresh = with_rating_label(None, &p).unwrap();
        assert_eq!(rating_label(&fresh), (Some(4), Some(3)));
        let lr = r#"<x:xmpmeta><rdf:RDF><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+1.00" xmp:Rating="2"/></rdf:RDF></x:xmpmeta>"#;
        let rejected = Params { flag: -1, label: 1, ..Default::default() };
        let out = with_rating_label(Some(lr), &rejected).unwrap();
        assert!(out.contains("crs:Exposure2012=\"+1.00\"") && out.contains("xmlns:xmp="));
        let back = parse(&out).1;
        assert_eq!((back.flag, back.label, back.exposure), (-1, 1, 1.0));
        assert!(with_rating_label(Some(&out), &rejected).is_none(), "unchanged: nothing to write");
        // Bridge's own label set: new stars must not wipe a label Ferrotype can't show.
        let bridge = r#"<x:xmpmeta><rdf:RDF><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="1" xmp:Label="Approved"/></rdf:RDF></x:xmpmeta>"#;
        let out = with_rating_label(Some(bridge), &Params { rating: 3, ..Default::default() }).unwrap();
        assert!(out.contains("xmp:Label=\"Approved\"") && out.contains("xmp:Rating=\"3\""));
    }
}
