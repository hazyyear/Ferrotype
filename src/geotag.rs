//! Geotagging from a GPX track (a phone app, watch or GPS logger): each photo gets the position the track was at
//! when it was taken. Camera clocks have no time zone, so the user gives the camera's offset from UTC (or lets
//! `guess_offset` find the one that fits the track best).
//!
//! Locations are stored with the photo's edits and are only written into exports when asked for.
use std::path::Path;

/// One track point: seconds since 2000-01-01 UTC, latitude and longitude in degrees, elevation in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub t: i64,
    pub lat: f64,
    pub lon: f64,
    pub ele: Option<f64>,
}

/// A position on the track: latitude, longitude (degrees, + is north / east) and elevation in metres (NaN unknown).
pub type Gps = [f64; 3];

/// A photo's location as saved with its edits.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ele: Option<f64>,
}

impl From<Gps> for Location {
    fn from(g: Gps) -> Self {
        Location { lat: g[0], lon: g[1], ele: g[2].is_finite().then_some(g[2]) }
    }
}

impl Location {
    pub fn gps(&self) -> Gps {
        [self.lat, self.lon, self.ele.unwrap_or(f64::NAN)]
    }
}

/// Seconds since 2000-01-01 for a civil date and time (proleptic Gregorian, no leap seconds).
pub fn civil_seconds(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    // Days from civil, after Howard Hinnant's algorithm.
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468 - 10957; // 1970 → 2000
    days * 86400 + h * 3600 + mi * 60 + s
}

/// An ISO 8601 time as written in GPX ("2026-05-04T12:00:00Z", "…T12:00:00.250+02:00") in UTC seconds.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let d: Vec<i64> = date.split('-').filter_map(|v| v.parse().ok()).collect();
    let [y, mo, dd] = d.get(..3)?.try_into().ok()?;
    // Time, then an optional zone: Z, +hh:mm or -hh:mm.
    let zone_at = rest.find(['Z', 'z', '+']).or_else(|| rest.rfind('-'));
    let (clock, zone) = match zone_at {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let c: Vec<f64> = clock.split(':').filter_map(|v| v.parse().ok()).collect();
    let [h, mi, sec] = c.get(..3)?.try_into().ok()?;
    let mut t = civil_seconds(y, mo, dd, h as i64, mi as i64, sec.floor() as i64);
    if let Some(sign) = zone.chars().next().filter(|c| *c == '+' || *c == '-') {
        let z: Vec<i64> = zone[1..].split(':').filter_map(|v| v.parse().ok()).collect();
        let off = z.first().copied().unwrap_or(0) * 3600 + z.get(1).copied().unwrap_or(0) * 60;
        t -= if sign == '+' { off } else { -off };
    }
    Some(t)
}

fn attr(tag: &str, name: &str) -> Option<f64> {
    let i = tag.find(&format!("{name}="))? + name.len() + 1;
    let q = tag[i..].chars().next()?;
    let rest = &tag[i + 1..];
    rest[..rest.find(q)?].trim().parse().ok()
}

fn inner<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let a = body.find(&format!("<{name}>"))? + name.len() + 2;
    let b = body[a..].find(&format!("</{name}>"))? + a;
    Some(&body[a..b])
}

/// The timed points of a GPX file (track points, then route and way points), sorted by time.
pub fn parse_gpx(xml: &str) -> Vec<Point> {
    let mut out = vec![];
    for kind in ["trkpt", "rtept", "wpt"] {
        let open = format!("<{kind}");
        let mut rest = xml;
        while let Some(i) = rest.find(&open) {
            rest = &rest[i + open.len()..];
            let Some(end) = rest.find('>') else { break };
            let tag = &rest[..end];
            let body = if tag.ends_with('/') { "" } else { rest[end..].split(&format!("</{kind}>")).next().unwrap_or("") };
            if let (Some(lat), Some(lon), Some(t)) = (attr(tag, "lat"), attr(tag, "lon"), inner(body, "time").and_then(parse_time)) {
                out.push(Point { t, lat, lon, ele: inner(body, "ele").and_then(|e| e.trim().parse().ok()) });
            }
        }
    }
    out.sort_by_key(|p| p.t);
    out.dedup_by_key(|p| p.t);
    out
}

pub fn read_gpx(path: &Path) -> Result<Vec<Point>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let pts = parse_gpx(&text);
    if pts.is_empty() {
        return Err("no timed track points in this file".into());
    }
    Ok(pts)
}

/// Longest gap between two track points that is still bridged (the logger may pause when you stand still).
const MAX_GAP: i64 = 30 * 60;
/// How far before the first or after the last point a photo still takes that end's position.
const MAX_EDGE: i64 = 5 * 60;

/// Where the track was at UTC time `t`: between the two points around it, or at the nearest end.
pub fn locate(track: &[Point], t: i64) -> Option<Gps> {
    locate_q(track, t).map(|r| r.0)
}

/// `locate`, and whether the position lies on the track (true) or is the nearest end of it (false).
fn locate_q(track: &[Point], t: i64) -> Option<(Gps, bool)> {
    let (first, last) = (track.first()?, track.last()?);
    let at = |p: &Point| [p.lat, p.lon, p.ele.unwrap_or(f64::NAN)];
    if t <= first.t {
        return (first.t - t <= MAX_EDGE).then(|| (at(first), t == first.t));
    }
    if t >= last.t {
        return (t - last.t <= MAX_EDGE).then(|| (at(last), t == last.t));
    }
    let i = track.partition_point(|p| p.t <= t);
    let (a, b) = (&track[i - 1], &track[i]);
    if b.t - a.t > MAX_GAP {
        // A long pause: only a photo near one end of it is placed.
        let near = if t - a.t < b.t - t { a } else { b };
        return ((t - near.t).abs() <= MAX_EDGE).then(|| (at(near), false));
    }
    let f = (t - a.t) as f64 / (b.t - a.t).max(1) as f64;
    let ele = match (a.ele, b.ele) { (Some(x), Some(y)) => x + (y - x) * f, _ => f64::NAN };
    Some(([a.lat + (b.lat - a.lat) * f, a.lon + (b.lon - a.lon) * f, ele], true))
}

/// A camera's "YYYY:MM:DD HH:MM:SS" capture time in seconds since 2000-01-01 on its own clock.
pub fn camera_seconds(taken: &str) -> Option<i64> {
    let n: Vec<i64> = taken.split([':', ' ', '-', 'T']).filter_map(|v| v.trim().parse().ok()).collect();
    let [y, mo, d, h, mi, s] = n.get(..6)?.try_into().ok()?;
    (y > 1900 && (1..=12).contains(&mo) && (1..=31).contains(&d)).then(|| civil_seconds(y, mo, d, h, mi, s))
}

/// The camera-clock offset from UTC (in minutes, quarter hours from -14 h to +14 h) that puts the most `photos`
/// (camera-clock seconds) on the track. A photo on the track counts twice one only near its ends; the smallest
/// offset wins a tie.
pub fn guess_offset(track: &[Point], photos: &[i64]) -> i32 {
    let mut best = (0usize, 0i32);
    for q in -56..=56 {
        let m: i32 = q * 15;
        let n: usize = photos.iter().filter_map(|t| locate_q(track, *t - m as i64 * 60)).map(|(_, on)| if on { 2 } else { 1 }).sum();
        if n > best.0 || (n == best.0 && n > 0 && m.abs() < best.1.abs()) {
            best = (n, m);
        }
    }
    best.1
}

/// "52°31.0′ N, 13°24.6′ E": for the info panel.
pub fn describe(g: &Gps) -> String {
    let dm = |v: f64, pos: char, neg: char| { let a = v.abs(); format!("{}°{:.1}′ {}", a.floor(), (a - a.floor()) * 60.0, if v >= 0.0 { pos } else { neg }) };
    format!("{}, {}", dm(g[0], 'N', 'S'), dm(g[1], 'E', 'W'))
}

/// A web map at the location (OpenStreetMap).
pub fn map_url(g: &Gps) -> String {
    format!("https://www.openstreetmap.org/?mlat={:.6}&mlon={:.6}#map=15/{:.6}/{:.6}", g[0], g[1], g[0], g[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    const GPX: &str = r#"<?xml version="1.0"?><gpx version="1.1"><trk><trkseg>
        <trkpt lat="52.5000" lon="13.4000"><ele>30</ele><time>2026-05-04T10:00:00Z</time></trkpt>
        <trkpt lat='52.5100' lon='13.4200'><ele>50</ele><time>2026-05-04T10:10:00Z</time></trkpt>
        <trkpt lat="52.6000" lon="13.5000"><time>2026-05-04T12:00:00+00:00</time></trkpt>
        <trkpt lat="1" lon="1"/>
    </trkseg></trk></gpx>"#;

    #[test]
    fn times_parse_with_zones() {
        assert_eq!(parse_time("2000-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-01-01T02:00:00+02:00"), Some(0));
        assert_eq!(parse_time("1999-12-31T23:30:00.5-00:30"), Some(0));
        assert_eq!(civil_seconds(2026, 5, 4, 10, 0, 0) - civil_seconds(2026, 5, 3, 10, 0, 0), 86400);
    }

    #[test]
    fn track_points_are_read_and_interpolated() {
        let t = parse_gpx(GPX);
        assert_eq!(t.len(), 3, "the point without a time is skipped");
        let mid = locate(&t, civil_seconds(2026, 5, 4, 10, 5, 0)).unwrap();
        assert!((mid[0] - 52.505).abs() < 1e-9 && (mid[1] - 13.41).abs() < 1e-9 && (mid[2] - 40.0).abs() < 1e-9);
        // Inside the long pause, far from both ends: no position.
        assert!(locate(&t, civil_seconds(2026, 5, 4, 11, 0, 0)).is_none());
        assert!(locate(&t, civil_seconds(2026, 5, 4, 11, 57, 0)).is_some());
        assert!(locate(&t, civil_seconds(2026, 5, 4, 9, 0, 0)).is_none());
    }

    #[test]
    fn offset_is_found_from_the_photos() {
        let t = parse_gpx(GPX);
        // Camera set to UTC+2: photos at 12:02 and 12:07 local were taken at 10:02 and 10:07 UTC.
        let photos = [civil_seconds(2026, 5, 4, 12, 2, 0), civil_seconds(2026, 5, 4, 12, 7, 0)];
        assert_eq!(guess_offset(&t, &photos), 120);
        assert_eq!(describe(&[52.5, -13.41, f64::NAN]), "52°30.0′ N, 13°24.6′ W");
    }
}
