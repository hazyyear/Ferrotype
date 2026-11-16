//! The built-in looks in Develop's Presets list, in groups. Each is a handful of ordinary settings, so picking
//! one leaves every slider where you can see and change it. All are Ferrotype's own: none is copied from, or
//! named after, a preset pack, a print stock or a film.
use crate::develop::{self, Params};

/// The groups in list order, with how many looks each has. Your own saved presets follow the last group.
pub const GROUPS: [(&str, usize); 4] = [("Basic", 7), ("Cinematic", 13), ("Film", 2), ("Black & white", 7)];

/// A gentle S on the RGB curve with the ends pulled in: soft blacks and a shoulder that never hits paper white.
fn print_curve(black: f32, white: f32) -> Vec<[f32; 2]> {
    vec![[0.0, black], [0.25, 0.22 + black * 0.5], [0.5, 0.5], [0.75, 0.775 - (1.0 - white) * 0.4], [1.0, white]]
}

/// Black & white through a colour filter (see `develop::BW_FILTERS`) and a toner (`develop::TONERS`), by name.
fn mono(filter: &str, toner: &str, p: Params) -> Params {
    let mut p = Params { bw: true, ..p };
    if let Some(f) = develop::BW_FILTERS.iter().find(|f| f.0 == filter) {
        p.bw_mix = f.1;
    }
    if let Some(i) = develop::TONERS.iter().position(|t| t.0 == toner) {
        develop::apply_toner(&mut p, i);
    }
    p
}

/// Colour grading as [hue, amount] for shadows, midtones and highlights.
fn grade(sh: [f32; 2], mid: [f32; 2], hi: [f32; 2]) -> [[f32; 3]; 4] {
    [[sh[0], sh[1], 0.0], [mid[0], mid[1], 0.0], [hi[0], hi[1], 0.0], [0.0; 3]]
}

/// The colour mixer with some bands set: (band, [hue, saturation, brightness]). Bands: 0 red, 1 orange,
/// 2 yellow, 3 green, 4 aqua, 5 blue, 6 purple, 7 magenta.
fn mixer(bands: &[(usize, [f32; 3])]) -> [[f32; 3]; 8] {
    let mut m = [[0.0; 3]; 8];
    for (i, v) in bands {
        m[*i] = *v;
    }
    m
}

const NONE: [f32; 2] = [0.0, 0.0];

pub fn builtin() -> Vec<(String, Params)> {
    let d = Params::default();
    let c = |rgb: Vec<[f32; 2]>, r: Vec<[f32; 2]>, g: Vec<[f32; 2]>, b: Vec<[f32; 2]>| [rgb, r, g, b];
    let looks: Vec<(&str, Params)> = vec![
        // ---- Basic ----
        ("Ferro Natural", d.clone()),
        ("Punchy", Params { contrast: 25.0, vibrance: 25.0, clarity: 15.0, blacks: -10.0, ..d.clone() }),
        ("Soft film", Params { contrast: -15.0, highlights: -30.0, shadows: 20.0, blacks: 15.0, saturation: -10.0, gr_amt: 20.0, c_sh: 15.0, ..d.clone() }),
        ("Warm matte", Params { temp: 6300.0, blacks: 25.0, c_sh: 20.0, c_hi: -10.0, vibrance: -5.0, grade: [[35.0, 20.0, 0.0], [0.0; 3], [45.0, 15.0, 0.0], [0.0; 3]], ..d.clone() }),
        // Skin first: open shadows, held highlights, a little less texture, oranges a touch lighter.
        ("Clean portrait", Params { contrast: 6.0, highlights: -25.0, shadows: 15.0, texture: -10.0, clarity: -6.0, vibrance: 8.0, hsl: mixer(&[(0, [0.0, -6.0, 0.0]), (1, [0.0, -4.0, 8.0])]), grade: grade(NONE, NONE, [40.0, 6.0]), ..d.clone() }),
        // Deeper skies, livelier greens, detail in the clouds and the shade.
        ("Landscape pop", Params { contrast: 14.0, highlights: -40.0, shadows: 28.0, clarity: 14.0, dehaze: 8.0, vibrance: 28.0, hsl: mixer(&[(3, [6.0, 8.0, 0.0]), (4, [0.0, 6.0, -8.0]), (5, [0.0, 10.0, -16.0])]), ..d.clone() }),
        // Lifted blacks and quiet colour, without a tint.
        ("Soft matte", Params { contrast: -8.0, highlights: -20.0, saturation: -12.0, curves: c(print_curve(0.07, 0.97), vec![], vec![], vec![]), ..d.clone() }),
        // ---- Cinematic ----
        // Teal shade and warm skin, with the greens held back so it never turns garish.
        ("Teal & orange", Params { contrast: 12.0, highlights: -15.0, vibrance: 6.0, grade: grade([198.0, 26.0], NONE, [36.0, 20.0]), g_bal: 10.0, hsl: mixer(&[(1, [0.0, 10.0, 0.0]), (2, [-12.0, -10.0, 0.0]), (3, [10.0, -30.0, -5.0]), (5, [-18.0, 0.0, 0.0])]), ..d.clone() }),
        // Silver left in the print: hard, gritty, half the colour.
        ("Bleach bypass", Params { contrast: 38.0, highlights: -15.0, blacks: -12.0, clarity: 22.0, saturation: -48.0, grade: grade([212.0, 8.0], NONE, NONE), gr_amt: 24.0, gr_size: 30.0, ..d.clone() }),
        // A cinema print: warm highlights, cool deep shadows, a soft shoulder.
        ("Warm print", Params { contrast: 12.0, saturation: -4.0, curves: c(print_curve(0.015, 0.965), vec![], vec![], vec![]), grade: grade([208.0, 16.0], [36.0, 6.0], [42.0, 18.0]), hsl: mixer(&[(0, [0.0, 6.0, 0.0]), (3, [8.0, -12.0, 0.0])]), gr_amt: 10.0, ..d.clone() }),
        // The same print on a colder stock: steel shadows, clean whites.
        ("Cool print", Params { temp: 5250.0, contrast: 14.0, saturation: -10.0, curves: c(print_curve(0.015, 0.965), vec![], vec![], vec![]), grade: grade([216.0, 20.0], [205.0, 6.0], [185.0, 6.0]), hsl: mixer(&[(1, [0.0, 6.0, 0.0])]), gr_amt: 10.0, ..d.clone() }),
        // Shot in sun, printed as moonlight: dark, blue, little colour.
        ("Day for night", Params { temp: 4300.0, exposure: -1.5, contrast: 18.0, highlights: -45.0, whites: -20.0, saturation: -38.0, grade: grade([226.0, 26.0], [220.0, 14.0], [210.0, 8.0]), hsl: mixer(&[(5, [0.0, 0.0, -25.0])]), vig: -22.0, ..d.clone() }),
        // Wet streets at night: inky blacks, green-blue shade, a blush in the lights.
        ("Neo-noir", Params { contrast: 32.0, blacks: -18.0, clarity: 10.0, saturation: -24.0, grade: grade([188.0, 24.0], NONE, [335.0, 12.0]), vig: -20.0, hal_amt: 14.0, ..d.clone() }),
        // A print that sat in a drawer: milky blacks, yellowed whites, soft colour.
        ("Faded 70s", Params { temp: 5900.0, contrast: -6.0, saturation: -16.0, curves: c(vec![[0.0, 0.05], [0.5, 0.5], [1.0, 0.96]], vec![], vec![], vec![[0.0, 0.07], [0.5, 0.49], [1.0, 0.88]]), grade: grade(NONE, [44.0, 10.0], NONE), gr_amt: 24.0, gr_size: 32.0, ..d.clone() }),
        // Low sun: amber highlights, warm shade, glowing edges.
        ("Golden hour", Params { temp: 6500.0, tint: 4.0, contrast: 6.0, highlights: -22.0, vibrance: 12.0, grade: grade([24.0, 10.0], NONE, [42.0, 24.0]), hsl: mixer(&[(1, [0.0, 10.0, 0.0]), (2, [-8.0, 0.0, 0.0])]), hal_amt: 10.0, ..d.clone() }),
        // Forest shade: deep muted greens, a little lift in the blacks.
        ("Moody green", Params { exposure: -0.2, contrast: 14.0, saturation: -10.0, curves: c(vec![[0.0, 0.03], [0.5, 0.5], [1.0, 1.0]], vec![], vec![], vec![]), grade: grade([150.0, 28.0], [140.0, 8.0], [46.0, 8.0]), hsl: mixer(&[(2, [12.0, -28.0, -10.0]), (3, [14.0, -30.0, -16.0])]), vig: -15.0, ..d.clone() }),
        // Slide film in negative chemistry: yellow-green highlights, blue shadows, hard colour.
        ("Cross process", Params { contrast: 14.0, saturation: 8.0, curves: c(vec![], vec![[0.0, 0.0], [0.3, 0.24], [0.7, 0.78], [1.0, 1.0]], vec![[0.0, 0.0], [0.5, 0.56], [1.0, 1.0]], vec![[0.0, 0.12], [0.5, 0.47], [1.0, 0.84]]), ..d.clone() }),
        // Tungsten film after dark: cool shade and a red glow around every lamp.
        ("Tungsten night", Params { temp: 4900.0, contrast: 10.0, blacks: 6.0, saturation: 4.0, grade: grade([202.0, 22.0], NONE, [28.0, 10.0]), hal_amt: 55.0, hal_size: 50.0, hal_thr: 60.0, gr_amt: 28.0, gr_size: 34.0, ..d.clone() }),
        // An old wide-screen lens: low contrast, gentle highlights, a faint glow, darker corners.
        ("Soft widescreen", Params { contrast: -12.0, highlights: -35.0, shadows: 20.0, clarity: -10.0, saturation: -5.0, grade: grade([206.0, 10.0], NONE, [40.0, 8.0]), hsl: mixer(&[(5, [-8.0, 0.0, 0.0])]), hal_amt: 12.0, vig: -12.0, ..d.clone() }),
        // Light and airy: open shadows, pale colour, a hint of pink in the whites.
        ("Pastel", Params { exposure: 0.3, contrast: -20.0, highlights: -30.0, shadows: 30.0, vibrance: -18.0, curves: c(vec![[0.0, 0.07], [0.5, 0.52], [1.0, 1.0]], vec![], vec![], vec![]), grade: grade([200.0, 10.0], NONE, [340.0, 8.0]), ..d.clone() }),
        // ---- Film ----
        // A peel-apart print: flat blacks, creamy whites, green-blue shade.
        ("Instant fade", Params { temp: 5300.0, contrast: -8.0, saturation: -18.0, curves: c(vec![[0.0, 0.09], [0.5, 0.5], [1.0, 0.93]], vec![], vec![], vec![[0.0, 0.04], [1.0, 0.94]]), grade: grade([188.0, 14.0], NONE, [50.0, 12.0]), vig: -14.0, gr_amt: 14.0, ..d.clone() }),
        // A projected slide: dense blacks, clean saturated colour, deep sky.
        ("Vivid slide", Params { contrast: 28.0, highlights: -10.0, blacks: -10.0, vibrance: 24.0, saturation: 4.0, hsl: mixer(&[(3, [0.0, 8.0, 0.0]), (5, [0.0, 8.0, -10.0])]), ..d.clone() }),
        // ---- Black & white ----
        ("B&W classic", Params { saturation: -100.0, contrast: 30.0, clarity: 20.0, gr_amt: 15.0, ..d.clone() }),
        ("B&W high key", Params { saturation: -100.0, exposure: 0.6, highlights: -40.0, whites: 20.0, contrast: -10.0, ..d.clone() }),
        // Through a red filter: dark skies, hard light, grain.
        ("B&W high contrast", mono("Red", "None", Params { contrast: 45.0, blacks: -18.0, clarity: 20.0, gr_amt: 25.0, gr_size: 30.0, ..d.clone() })),
        // Through a yellow filter, printed soft.
        ("B&W soft", mono("Yellow", "None", Params { contrast: -15.0, highlights: -25.0, shadows: 25.0, blacks: 12.0, clarity: -8.0, gr_amt: 10.0, ..d.clone() })),
        ("Sepia tone", mono("Yellow", "Sepia", Params { contrast: 10.0, blacks: 8.0, ..d.clone() })),
        ("Selenium tone", mono("Orange", "Selenium", Params { contrast: 22.0, blacks: -6.0, ..d.clone() })),
        ("Cyanotype", mono("None", "Cyanotype", Params { contrast: 16.0, ..d })),
    ];
    looks.into_iter().map(|(n, p)| (n.to_string(), p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb32FImage;

    /// A linear test picture: a sky-to-ground scene on top (bright sky, a sun, greens, skin tones, deep shade),
    /// then a hue sweep at two brightnesses, then a grey ramp.
    fn chart(w: u32, h: u32) -> Rgb32FImage {
        Rgb32FImage::from_fn(w, h, |x, y| {
            let (u, v) = (x as f32 / (w - 1) as f32, y as f32 / (h - 1) as f32);
            let c = if v < 0.5 {
                let t = v / 0.5;
                let sky = [0.25 + 0.5 * t, 0.42 + 0.4 * t, 0.85];
                let sun = (-((u - 0.75).powi(2) + (t - 0.25).powi(2)) / 0.004).exp() * 4.0;
                if t < 0.6 {
                    sky.map(|c| c + sun)
                } else if u < 0.35 {
                    [0.05 + 0.1 * u, 0.22 - 0.2 * (t - 0.6), 0.04] // foliage
                } else if u < 0.6 {
                    [0.52, 0.30, 0.2].map(|c| c * (0.5 + (u - 0.35) * 3.0)) // skin, dark to light
                } else {
                    [0.02, 0.022, 0.03].map(|c| c * (1.0 + (u - 0.6) * 6.0)) // shade
                }
            } else if v < 0.8 {
                let hsv = develop::hsv_to_rgb(u * 360.0, 0.75, if v < 0.65 { 0.9 } else { 0.4 });
                hsv.map(crate::load::srgb_to_linear)
            } else {
                [u * u * 1.1; 3]
            };
            image::Rgb(c)
        })
    }

    fn luma(p: &[f32]) -> f32 {
        0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
    }

    /// Mean of each channel, mean brightness, share of clipped-white and of black pixels, mean colourfulness.
    fn stats(img: &Rgb32FImage) -> ([f32; 3], f32, f32, f32, f32) {
        let n = (img.width() * img.height()) as f32;
        let mut mean = [0.0f32; 3];
        let (mut y, mut white, mut black, mut chroma) = (0.0, 0.0, 0.0, 0.0);
        for p in img.pixels() {
            for k in 0..3 {
                mean[k] += p.0[k] / n;
            }
            y += luma(&p.0) / n;
            white += if p.0.iter().all(|v| *v >= 0.995) { 1.0 / n } else { 0.0 };
            black += if p.0.iter().all(|v| *v <= 0.005) { 1.0 / n } else { 0.0 };
            chroma += (p.0.iter().cloned().fold(0.0, f32::max) - p.0.iter().cloned().fold(1.0, f32::min)) / n;
        }
        (mean, y, white, black, chroma)
    }

    fn group_of(i: usize) -> &'static str {
        let mut end = 0;
        GROUPS.iter().find(|(_, n)| { end += n; i < end }).map_or("", |g| g.0)
    }

    #[test]
    fn groups_cover_the_list_and_names_are_plain() {
        let all = builtin();
        assert_eq!(GROUPS.iter().map(|g| g.1).sum::<usize>(), all.len());
        for (i, (name, _)) in all.iter().enumerate() {
            assert!(all[i + 1..].iter().all(|(n, _)| n != name), "{name} is listed twice");
            assert!(name.len() <= 18 && name.is_ascii(), "{name}");
        }
        assert_eq!(group_of(0), "Basic");
        assert_eq!(group_of(all.len() - 1), "Black & white");
        // A look is a look: nothing that belongs to one photo, and no file on someone's disk.
        let d = Params::default();
        for (name, p) in &all {
            assert!(p.lut.is_empty() && p.masks.is_empty() && p.crop == d.crop && p.neg == d.neg && p.rating == 0, "{name}");
        }
    }

    #[test]
    fn every_look_renders_a_usable_picture() {
        let src = chart(240, 160);
        let plain = stats(&develop::render(&src, &Params::default(), 0.1));
        for (i, (name, p)) in builtin().iter().enumerate() {
            let out = develop::render(&src, p, 0.1);
            assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{name}: a value out of range");
            let (mean, y, white, black, chroma) = stats(&out);
            let dark = name == "Day for night";
            assert!(y > if dark { 0.04 } else { 0.15 } && y < 0.8, "{name}: mean brightness {y}");
            assert!(white < plain.2 + 0.2 && black < plain.3 + 0.25, "{name}: clipped white {white}, black {black}");
            // Tones still span a range: neither a grey fog nor a silhouette.
            let ys: Vec<f32> = out.pixels().map(|p| luma(&p.0)).collect();
            let (lo, hi) = (ys.iter().cloned().fold(1.0, f32::min), ys.iter().cloned().fold(0.0, f32::max));
            assert!(hi - lo > if dark { 0.3 } else { 0.6 }, "{name}: tones only span {lo}..{hi}");
            let bw = group_of(i) == "Black & white";
            let warmth = mean[0] - mean[2];
            match name.as_str() {
                "Sepia tone" => assert!(warmth > 0.03 && chroma < 0.2, "{name}: {mean:?}"),
                "Selenium tone" => assert!(chroma > 0.002 && chroma < 0.08, "{name}: {mean:?} {chroma}"),
                "Cyanotype" => assert!(warmth < -0.02 && chroma < 0.3, "{name}: {mean:?}"),
                _ if bw => assert!(chroma < 0.004, "{name} should be neutral grey: {chroma}"),
                _ => assert!(chroma > plain.4 * 0.3, "{name} lost its colour: {chroma} vs {}", plain.4),
            }
            // The looks named for a colour lean that way on the grey ramp, which the plain render leaves grey.
            let ramp = stats(&image::imageops::crop_imm(&out, 0, 136, 240, 24).to_image()).0;
            let (lean, mean) = (ramp[0] - ramp[2], ramp);
            match name.as_str() {
                "Warm matte" | "Golden hour" | "Faded 70s" => assert!(lean > 0.01, "{name}: {lean}"),
                "Day for night" | "Cool print" | "Tungsten night" => assert!(lean < -0.01, "{name}: {lean}"),
                "Moody green" => assert!(mean[1] - (mean[0] + mean[2]) / 2.0 > 0.004, "{name}: {mean:?}"),
                _ => {}
            }
        }
    }

    /// A contact sheet of every look, for looking at: `FERROTYPE_PRESET_SHEET=out.jpg cargo test --release
    /// presets_contact_sheet -- --ignored`. `FERROTYPE_PRESET_PHOTO` names a photo to use instead of the chart.
    #[test]
    #[ignore]
    fn presets_contact_sheet() {
        let out = std::env::var("FERROTYPE_PRESET_SHEET").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir().join("ferrotype-presets.jpg"));
        let (src, toned) = match std::env::var("FERROTYPE_PRESET_PHOTO") {
            Ok(f) => crate::load::load_quick_toned(std::path::Path::new(&f), 480).expect("photo"),
            Err(_) => (chart(480, 320), false),
        };
        let src = crate::load::fit(&src, 480);
        let all = builtin();
        let (cols, gap, label) = (6u32, 16u32, 30u32);
        let (fw, fh) = src.dimensions();
        let rows = (all.len() as u32).div_ceil(cols);
        let mut sheet = Rgb32FImage::from_pixel(cols * (fw + gap) + gap, rows * (fh + label + gap) + gap, image::Rgb([0.04; 3]));
        for (i, (name, p)) in all.iter().enumerate() {
            let _ = toned;
            let img = develop::render(&src, p, 0.15);
            let (x0, y0) = (gap + (i as u32 % cols) * (fw + gap), gap + (i as u32 / cols) * (fh + label + gap));
            image::imageops::replace(&mut sheet, &img, x0 as i64, y0 as i64);
            develop::draw_text(&mut sheet, &format!("{} / {name}", group_of(i)), (x0 + fw / 2) as f32, (y0 + fh + label / 2) as f32, 15.0, 0.8);
        }
        crate::save_output(sheet, &out, 90).unwrap();
        println!("saved {}", out.display());
    }
}
