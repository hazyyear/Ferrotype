//! GPU acceleration through wgpu: Vulkan (AMD, NVIDIA, Intel, integrated and APUs), Metal (Apple), DX12 (Windows).
//! Runs the per-pixel tone and colour stage. Any problem, or no usable GPU, and the CPU path runs instead.
//! FERROTYPE_GPU=off turns it off; FERROTYPE_GPU=force also accepts software adapters (for testing).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    name: String,
    /// Largest storage buffer we may bind, in bytes.
    max_bind: u64,
    lock: Mutex<()>,
}

static BROKEN: AtomicBool = AtomicBool::new(false);
/// Successful GPU passes, so tests can tell the GPU really ran.
static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Below this many pixels, the copy to and from the GPU costs more than it saves.
pub const MIN_PIXELS: usize = 250_000;

thread_local! {
    static CPU_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with the GPU off on this thread (to compare against the CPU).
#[cfg(test)]
pub fn on_cpu<R>(f: impl FnOnce() -> R) -> R {
    CPU_ONLY.with(|c| c.set(true));
    let r = f();
    CPU_ONLY.with(|c| c.set(false));
    r
}

static G: OnceLock<Option<Gpu>> = OnceLock::new();

fn gpu() -> Option<&'static Gpu> {
    if BROKEN.load(Ordering::Relaxed) || CPU_ONLY.with(|c| c.get()) {
        return None;
    }
    G.get_or_init(|| {
        let mode = std::env::var("FERROTYPE_GPU").unwrap_or_default().to_lowercase();
        if mode == "off" || mode == "0" {
            return None;
        }
        // A driver crash during setup must not take the app down.
        let g = std::panic::catch_unwind(|| pollster::block_on(init(mode == "force"))).ok().flatten();
        match &g {
            Some(g) => eprintln!("ferrotype: GPU acceleration on {}", g.name),
            None => eprintln!("ferrotype: no usable GPU, rendering on the CPU"),
        }
        g
    })
    .as_ref()
}

async fn init(allow_software: bool) -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })
        .await
        .ok()?;
    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu && !allow_software {
        return None;
    }
    if !adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS) {
        return None;
    }
    let al = adapter.limits();
    let limits = wgpu::Limits {
        max_storage_buffer_binding_size: al.max_storage_buffer_binding_size,
        max_buffer_size: al.max_buffer_size,
        max_storage_buffers_per_shader_stage: 3,
        ..wgpu::Limits::downlevel_defaults()
    };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor { label: Some("ferrotype"), required_limits: limits, ..Default::default() })
        .await
        .ok()?;
    device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| {
        eprintln!("ferrotype: GPU error, switching to the CPU: {e}");
        BROKEN.store(true, Ordering::Relaxed);
    }));
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(include_str!("gpu.wgsl").into()) });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("tone"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let max_bind = (al.max_storage_buffer_binding_size as u64).min(al.max_buffer_size).min(256 << 20);
    Some(Gpu { device, queue, pipeline, name: format!("{} ({:?})", info.name, info.backend), max_bind, lock: Mutex::new(()) })
}

/// What renders, for display; doesn't start the GPU (it starts with the first big render).
pub fn status() -> String {
    match G.get() {
        _ if BROKEN.load(Ordering::Relaxed) => "CPU (the GPU reported an error)".into(),
        Some(Some(g)) => format!("GPU: {}", g.name),
        Some(None) => "CPU (no usable GPU)".into(),
        None => "Not started yet".into(),
    }
}

/// Run the tone and colour stage on `img` (packed RGB, `w` wide) in place. `aux` holds the optional
/// per-pixel inputs (lp, big blur, small blur, haze) and `prm` the settings from develop::gpu_params,
/// with indices 0..=3 filled in here. Returns false if the GPU is unavailable; `img` is then unchanged.
pub fn tone_stage(img: &mut [f32], w: usize, y_off: f32, aux: [Option<&[f32]>; 4], mut prm: Vec<f32>) -> bool {
    let Some(g) = gpu() else { return false };
    let h = img.len() / 3 / w.max(1);
    if w == 0 || h == 0 {
        return false;
    }
    let _guard = g.lock.lock().unwrap_or_else(|e| e.into_inner());
    // Rows per pass, so every buffer fits the adapter's binding limit (aux: four floats a pixel).
    let band = ((g.max_bind / (w as u64 * 16)) as usize).clamp(1, h);
    let mut out = img.to_vec();
    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for y in (0..h).step_by(band) {
            let rows = band.min(h - y);
            prm[0] = w as f32;
            prm[1] = rows as f32;
            prm[3] = y_off + y as f32;
            let px = &mut out[y * w * 3..(y + rows) * w * 3];
            if !pass(g, px, w, rows, y, &aux, &prm) {
                return false;
            }
        }
        true
    }))
    .unwrap_or(false);
    if ok && !BROKEN.load(Ordering::Relaxed) {
        img.copy_from_slice(&out);
        RUNS.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        BROKEN.store(true, Ordering::Relaxed);
        false
    }
}

fn pass(g: &Gpu, px: &mut [f32], w: usize, rows: usize, y: usize, aux: &[Option<&[f32]>; 4], prm: &[f32]) -> bool {
    use wgpu::util::DeviceExt;
    let d = &g.device;
    let n = w * rows;
    let bytes = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
    let storage = |data: &[u8], extra: wgpu::BufferUsages| d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: data, usage: wgpu::BufferUsages::STORAGE | extra });
    let img_buf = storage(&bytes(px), wgpu::BufferUsages::COPY_SRC);
    let mut aux_data = vec![0f32; if aux.iter().any(|a| a.is_some()) { 4 * n } else { 4 }];
    for (k, a) in aux.iter().enumerate() {
        if let Some(a) = a {
            aux_data[k * n..(k + 1) * n].copy_from_slice(&a[y * w..y * w + n]);
        }
    }
    let aux_buf = storage(&bytes(&aux_data), wgpu::BufferUsages::empty());
    let prm_buf = storage(&bytes(prm), wgpu::BufferUsages::empty());
    let size = (n * 12) as u64;
    let read = d.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let layout = g.pipeline.get_bind_group_layout(0);
    let group = d.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: img_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: aux_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: prm_buf.as_entire_binding() },
        ],
    });
    let mut enc = d.create_command_encoder(&Default::default());
    {
        let mut cp = enc.begin_compute_pass(&Default::default());
        cp.set_pipeline(&g.pipeline);
        cp.set_bind_group(0, &group, &[]);
        cp.dispatch_workgroups((w as u32).div_ceil(16), (rows as u32).div_ceil(16), 1);
    }
    enc.copy_buffer_to_buffer(&img_buf, 0, &read, 0, size);
    g.queue.submit([enc.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    read.map_async(wgpu::MapMode::Read, .., move |r| { let _ = tx.send(r.is_ok()); });
    if d.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).is_err() || rx.recv() != Ok(true) {
        return false;
    }
    let Ok(view) = read.get_mapped_range(..) else { return false };
    for (o, b) in px.iter_mut().zip(view.chunks_exact(4)) {
        *o = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::develop::{Params, render};

    #[test]
    fn gpu_matches_cpu() {
        // SAFETY: set before any GPU use; lets the test use a software GPU (lavapipe) when there is no real one.
        unsafe { std::env::set_var("FERROTYPE_GPU", "force") };
        if super::gpu().is_none() {
            eprintln!("no GPU adapter here, skipping");
            return;
        }
        let (w, h) = (640u32, 480u32);
        let img = image::Rgb32FImage::from_fn(w, h, |x, y| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            image::Rgb([u * 1.4, v * v, ((u * 9.0).sin() * 0.5 + 0.5) * (1.0 - v)])
        });
        let mut p = Params::default();
        (p.exposure, p.contrast, p.highlights, p.shadows, p.whites, p.blacks) = (0.3, 30.0, -40.0, 35.0, 10.0, -15.0);
        (p.clarity, p.texture, p.dehaze, p.saturation, p.vibrance) = (25.0, 20.0, 15.0, 10.0, 20.0);
        (p.c_sh, p.c_li) = (10.0, -10.0);
        p.hsl[0] = [10.0, 20.0, -10.0];
        p.hsl[5] = [-15.0, -30.0, 15.0];
        p.grade[0] = [220.0, 30.0, 5.0];
        p.grade[2] = [40.0, 25.0, -5.0];
        p.curves[0] = vec![[0.0, 0.05], [0.5, 0.55], [1.0, 0.95]];
        (p.vig, p.vig_round, p.gr_amt) = (-30.0, 20.0, 30.0);
        let check = |p: &Params| {
            let g = render(&img, p, 1.0);
            let c = super::on_cpu(|| render(&img, p, 1.0));
            let d: Vec<f32> = g.iter().zip(c.iter()).map(|(a, b)| (a - b).abs()).collect();
            let (max, mean) = (d.iter().cloned().fold(0.0, f32::max), d.iter().sum::<f32>() / d.len() as f32);
            assert!(max < 0.03 && mean < 0.001, "GPU differs from CPU: max {max}, mean {mean}");
        };
        check(&p);
        assert!(super::RUNS.load(super::Ordering::Relaxed) > 0, "the GPU path never ran");
        p.curve_lum = true;
        p.dehaze = -20.0;
        p.vig = 25.0;
        check(&p);
    }
}
