// The per-pixel tone and colour stage of develop::finish, on the GPU. Must match the CPU code there.
@group(0) @binding(0) var<storage, read_write> px: array<f32>;
// Planar: lp, big blur, small blur, haze, each `w * rows` long (only read when that feature is on).
@group(0) @binding(1) var<storage, read> aux: array<f32>;
// Settings, laid out by develop::gpu_params.
@group(0) @binding(2) var<storage, read> P: array<f32>;

const CURVES: u32 = 128u;

fn luma(c: vec3f) -> f32 { return 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z; }
fn smooth_(a: f32, b: f32, x: f32) -> f32 { let t = clamp((x - a) / (b - a), 0.0, 1.0); return t * t * (3.0 - 2.0 * t); }
fn bump(x: f32, c: f32, w: f32) -> f32 { let t = (x - c) / w; let m = max(1.0 - t * t, 0.0); return m * m; }
fn rem_e(a: f32, b: f32) -> f32 { let r = a - b * trunc(a / b); return select(r, r + abs(b), r < 0.0); }
// pow for x >= 0, y > 0, with pow(0, y) = 0 everywhere.
fn pw(x: f32, y: f32) -> f32 { return select(pow(x, y), 0.0, x <= 0.0); }
fn on(i: u32) -> bool { return P[i] > 0.5; }

// Extended sRGB: mirrored below 0, so colours outside sRGB (a negative channel) survive to a wide-gamut export.
fn srgb(v: f32) -> f32 {
    let x = min(abs(v), 1.0);
    return sign(v) * select(1.055 * pw(x, 1.0 / 2.4) - 0.055, x * 12.92, x <= 0.0031308);
}

fn table(k: u32, v: f32) -> f32 {
    let f = clamp(v, 0.0, 1.0) * 255.0;
    let i = min(u32(f), 254u);
    let a = P[CURVES + k * 256u + i];
    return a + (P[CURVES + k * 256u + i + 1u] - a) * (f - f32(i));
}

fn rgb_to_hsv(c: vec3f) -> vec3f {
    let mx = max(c.x, max(c.y, c.z));
    let mn = min(c.x, min(c.y, c.z));
    let d = mx - mn;
    var h = 0.0;
    if d > 1e-6 {
        if mx == c.x { h = 60.0 * rem_e((c.y - c.z) / d, 6.0); }
        else if mx == c.y { h = 60.0 * ((c.z - c.x) / d + 2.0); }
        else { h = 60.0 * ((c.x - c.y) / d + 4.0); }
    }
    return vec3f(h, select(d / mx, 0.0, mx <= 1e-6), mx);
}

fn hsv_to_rgb(hh: f32, s: f32, v: f32) -> vec3f {
    let h = rem_e(hh, 360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - abs(h % 2.0 - 1.0));
    let i = u32(h);
    var r = vec3f(c, 0.0, x);
    if i == 0u { r = vec3f(c, x, 0.0); }
    else if i == 1u { r = vec3f(x, c, 0.0); }
    else if i == 2u { r = vec3f(0.0, c, x); }
    else if i == 3u { r = vec3f(0.0, x, c); }
    else if i == 4u { r = vec3f(x, 0.0, c); }
    return r + vec3f(v - c);
}

const HUES = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 275.0, 310.0);

fn band_weight(hue: f32, i: u32) -> f32 {
    var hues = HUES;
    let c = hues[i];
    let prev = hues[(i + 7u) % 8u];
    let next = hues[(i + 1u) % 8u];
    let d = rem_e(hue - c + 540.0, 360.0) - 180.0;
    let span = select(rem_e(next - c, 360.0), rem_e(c - prev, 360.0), d < 0.0);
    return max(1.0 - abs(d) / span, 0.0);
}

fn hash(x: i32, y: i32) -> f32 {
    var n = (bitcast<u32>(x) * 374761393u) ^ (bitcast<u32>(y) * 668265263u);
    n = (n ^ (n >> 13u)) * 1274126177u;
    return f32((n ^ (n >> 16u)) & 0xffffu) / 65535.0;
}

fn value_noise(x: f32, y: f32) -> f32 {
    let xi = i32(floor(x));
    let yi = i32(floor(y));
    let fx = smooth_(0.0, 1.0, x - f32(xi));
    let fy = smooth_(0.0, 1.0, y - f32(yi));
    let a = hash(xi, yi) * (1.0 - fx) + hash(xi + 1, yi) * fx;
    let b = hash(xi, yi + 1) * (1.0 - fx) + hash(xi + 1, yi + 1) * fx;
    return a * (1.0 - fy) + b * fy;
}

fn tone(l0: f32) -> f32 {
    let bk = -P[33] / 100.0 * 0.08;
    let wh = 1.0 - P[34] / 100.0 * 0.2;
    var l = (l0 - bk) / (wh - bk);
    l += P[35] / 100.0 * 0.22 * smooth_(0.45, 1.0, l) * (1.0 - smooth_(1.0, 1.6, l) * 0.5);
    l += P[36] / 100.0 * 0.22 * (1.0 - smooth_(0.0, 0.5, l)) * smooth_(-0.05, 0.08, l);
    let k = P[37] / 100.0;
    let lc = clamp(l, 0.0, 1.0);
    l += select(k * (lc - 0.46) * 0.6, k * (smooth_(0.0, 1.0, lc) - lc), k > 0.0);
    return l + 0.15 * (P[38] / 100.0 * bump(l, 0.12, 0.2) + P[39] / 100.0 * bump(l, 0.35, 0.22) + P[40] / 100.0 * bump(l, 0.62, 0.22) + P[41] / 100.0 * bump(l, 0.88, 0.2));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let w = u32(P[0]);
    let rows = u32(P[1]);
    if id.x >= w || id.y >= rows { return; }
    let i = id.y * w + id.x;
    let n = w * rows;
    let gx = f32(id.x) + P[2];
    let gy = f32(id.y) + P[3];
    let cx = P[4];
    let cy = P[5];
    let big_w = P[6];
    let big_h = P[7];
    var c = vec3f(px[i * 3u], px[i * 3u + 1u], px[i * 3u + 2u]);

    if on(8u) {
        let hz = aux[3u * n + i];
        let s = clamp(P[9] / 100.0 * 0.9, -0.9, 0.9);
        if s > 0.0 { c = max((c - vec3f(s * hz)) / (1.0 - s * hz), vec3f(0.0)); }
        else { c = c + (-s) * 0.25 * max(vec3f(0.6) - c, vec3f(0.0)); }
    }
    if on(10u) {
        let y0 = luma(c);
        var l = pw(max(y0, 0.0), 1.0 / 2.2);
        let midw = bump(l, 0.5, 0.6);
        if on(11u) { l += P[12] / 100.0 * 0.6 * (aux[i] - aux[n + i]) * midw; }
        if on(13u) { l += P[14] / 100.0 * 1.2 * (aux[i] - aux[2u * n + i]); }
        l = tone(l);
        let y1 = pw(max(l, 0.0), 2.2);
        c = select(vec3f(y1), c * (y1 / y0), y0 > 1e-6);
    }
    if on(15u) {
        let yl = luma(c);
        let sat0 = min(rgb_to_hsv(c).y, 1.0);
        let k = 1.0 + P[16] / 100.0 + P[17] / 100.0 * (1.0 - sat0) * (1.0 - sat0) + P[9] / 400.0;
        c = vec3f(yl) + (c - vec3f(yl)) * k;
    }
    var e = vec3f(srgb(c.x), srgb(c.y), srgb(c.z));
    if on(18u) {
        if on(19u) {
            let l = luma(e);
            let k = select(1.0, table(0u, l) / l, l > 1e-5);
            e = vec3f(table(1u, e.x * k), table(2u, e.y * k), table(3u, e.z * k));
        } else {
            e = vec3f(table(1u, table(0u, e.x)), table(2u, table(0u, e.y)), table(3u, table(0u, e.z)));
        }
    }
    if on(20u) {
        let hsv = rgb_to_hsv(e);
        var d = vec3f(0.0);
        for (var b = 0u; b < 8u; b++) {
            let wgt = band_weight(hsv.x, b);
            if wgt > 0.0 { d += wgt * vec3f(P[48u + b * 3u], P[49u + b * 3u], P[50u + b * 3u]); }
        }
        e = hsv_to_rgb(hsv.x + d.x * 0.3, clamp(hsv.y * (1.0 + d.y / 100.0), 0.0, 1.0), max(hsv.z * (1.0 + d.z / 100.0 * 0.4 * hsv.y), 0.0));
    }
    if on(21u) {
        let mid = P[22];
        let le = clamp(luma(e), 0.0, 1.0);
        let ws = pw(clamp(1.0 - le / mid, 0.0, 1.0), P[23]);
        let wh = pw(clamp((le - mid) / (1.0 - mid), 0.0, 1.0), P[23]);
        let wm = max(1.0 - ws - wh, 0.0);
        let wz = array<f32, 4>(ws, wm, wh, 1.0);
        for (var z = 0u; z < 4u; z++) {
            let lum = P[84u + z] / 100.0 * 0.2;
            e += wz[z] * (vec3f(P[72u + z * 3u], P[73u + z * 3u], P[74u + z * 3u]) + vec3f(lum));
        }
    }
    if P[24] != 0.0 {
        let vr = P[25];
        let nx = (gx - cx) / cx;
        let ny = (gy - cy) / cy;
        let t = max(vr, 0.0);
        let ax = 1.0 + t * max(big_w / big_h - 1.0, 0.0);
        let ay = 1.0 + t * max(big_h / big_w - 1.0, 0.0);
        let ex = nx * ax / max(ax, ay);
        let ey = ny * ay / max(ax, ay);
        let pe = 2.0 - min(vr, 0.0) * 4.0;
        let r = pw(pw(abs(ex), pe) + pw(abs(ey), pe), 1.0 / pe) / pow(2.0, 1.0 / pe);
        let start = P[26] / 100.0 * 0.9;
        let m = smooth_(start, start + 0.05 + P[27] / 100.0 * 0.6, r);
        let a = P[24] / 100.0;
        let protect = 1.0 - P[28] / 100.0 * smooth_(0.6, 1.0, luma(e));
        if a < 0.0 { e = e * (1.0 + a * m * protect); } else { e = e + (vec3f(1.0) - e) * a * m; }
    }
    if P[29] > 0.0 {
        let x = gx / P[30];
        let y = gy / P[30];
        let rough = P[31] / 100.0;
        let nz = value_noise(x, y) * (1.0 - rough * 0.5) + value_noise(x * 2.3 + 17.0, y * 2.3 + 5.0) * rough * 0.5 - 0.5;
        let le = luma(e);
        let amt = P[29] / 100.0 * 0.22 * (0.3 + bump(le, 0.5, 0.7)) * (1.0 - (1.0 - P[32] / 100.0) * smooth_(0.55, 0.95, le));
        e = e + vec3f(nz * amt);
    }
    px[i * 3u] = e.x;
    px[i * 3u + 1u] = e.y;
    px[i * 3u + 2u] = e.z;
}
