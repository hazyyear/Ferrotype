//! TWAIN on Windows: the scanner's own window (Epson Scan 2 and the like) through the TWAIN data source manager
//! (TWAINDSM.dll, which scanner drivers install). Pictures come over by memory transfer, which every TWAIN
//! scanner must offer and which keeps 16 bits per channel, and are saved as TIFF.
//!
//! The data source runs its window on this thread, so the thread keeps a hidden window and a message loop, and
//! hands every message to the source first (TWAIN 1.x events, which every driver supports).

use super::Event;
use std::ffi::c_void;
use std::path::Path;
use std::ptr::null_mut;

// ---------- twain.h ----------

const DG_CONTROL: u32 = 0x1;
const DG_IMAGE: u32 = 0x2;

const DAT_CAPABILITY: u16 = 0x0001;
const DAT_EVENT: u16 = 0x0002;
const DAT_IDENTITY: u16 = 0x0003;
const DAT_PARENT: u16 = 0x0004;
const DAT_PENDINGXFERS: u16 = 0x0005;
const DAT_SETUPMEMXFER: u16 = 0x0006;
const DAT_USERINTERFACE: u16 = 0x0009;
const DAT_IMAGEINFO: u16 = 0x0101;
const DAT_IMAGEMEMXFER: u16 = 0x0103;
const DAT_ICCPROFILE: u16 = 0x0401;

const MSG_GET: u16 = 0x0001;
const MSG_GETDEFAULT: u16 = 0x0003;
const MSG_GETFIRST: u16 = 0x0004;
const MSG_GETNEXT: u16 = 0x0005;
const MSG_SET: u16 = 0x0006;
const MSG_RESET: u16 = 0x0007;
const MSG_XFERREADY: u16 = 0x0101;
const MSG_CLOSEDSREQ: u16 = 0x0102;
const MSG_CLOSEDSOK: u16 = 0x0103;
const MSG_OPENDSM: u16 = 0x0301;
const MSG_CLOSEDSM: u16 = 0x0302;
const MSG_OPENDS: u16 = 0x0401;
const MSG_CLOSEDS: u16 = 0x0402;
const MSG_USERSELECT: u16 = 0x0403;
const MSG_DISABLEDS: u16 = 0x0501;
const MSG_ENABLEDS: u16 = 0x0502;
const MSG_PROCESSEVENT: u16 = 0x0601;
const MSG_ENDXFER: u16 = 0x0701;

const TWRC_SUCCESS: u16 = 0;
const TWRC_CANCEL: u16 = 3;
const TWRC_DSEVENT: u16 = 4;
const TWRC_XFERDONE: u16 = 6;

const CAP_XFERCOUNT: u16 = 0x0001;
const ICAP_XFERMECH: u16 = 0x0103;
const TWSX_MEMORY: u32 = 2;
const TWON_ONEVALUE: u16 = 5;
const TWTY_INT16: u16 = 1;
const TWTY_UINT16: u16 = 4;
const TWON_DONTCARE16: u16 = 0xffff;
const TWON_DONTCARE32: u32 = 0xffff_ffff;
const TWMF_APPOWNS: u32 = 0x1;
const TWMF_DSOWNS: u32 = 0x2;
const TWMF_HANDLE: u32 = 0x4;
const TWMF_POINTER: u32 = 0x8;
const TWPT_GRAY: i16 = 1;
const TWPT_RGB: i16 = 2;

type Str32 = [u8; 34];

#[repr(C, packed(2))]
#[derive(Clone, Copy)]
struct Version {
    major: u16,
    minor: u16,
    language: u16,
    country: u16,
    info: Str32,
}

#[repr(C, packed(2))]
#[derive(Clone, Copy)]
struct Identity {
    id: u32,
    version: Version,
    protocol_major: u16,
    protocol_minor: u16,
    supported_groups: u32,
    manufacturer: Str32,
    product_family: Str32,
    product_name: Str32,
}

#[repr(C, packed(2))]
struct UserInterface {
    show_ui: u16,
    modal_ui: u16,
    parent: *mut c_void,
}

#[repr(C, packed(2))]
struct TwEvent {
    event: *mut c_void,
    message: u16,
}

#[repr(C, packed(2))]
struct PendingXfers {
    count: u16,
    eoj: u32,
}

#[repr(C, packed(2))]
#[derive(Clone, Copy, Default)]
struct Fix32 {
    whole: i16,
    frac: u16,
}

#[repr(C, packed(2))]
#[derive(Default)]
struct ImageInfo {
    x_resolution: Fix32,
    y_resolution: Fix32,
    width: i32,
    length: i32,
    samples_per_pixel: i16,
    bits_per_sample: [i16; 8],
    bits_per_pixel: i16,
    planar: u16,
    pixel_type: i16,
    compression: u16,
}

#[repr(C, packed(2))]
#[derive(Default)]
struct SetupMemXfer {
    min_buf_size: u32,
    max_buf_size: u32,
    preferred: u32,
}

#[repr(C, packed(2))]
struct Memory {
    flags: u32,
    length: u32,
    the_mem: *mut c_void,
}

#[repr(C, packed(2))]
struct ImageMemXfer {
    compression: u16,
    bytes_per_row: u32,
    columns: u32,
    rows: u32,
    x_offset: u32,
    y_offset: u32,
    bytes_written: u32,
    memory: Memory,
}

#[repr(C, packed(2))]
struct Capability {
    cap: u16,
    con_type: u16,
    container: *mut c_void,
}

#[repr(C, packed(2))]
struct OneValue {
    item_type: u16,
    item: u32,
}

// The sizes twain.h gives these with its 2-byte packing on 64-bit Windows.
const _: () = {
    use std::mem::size_of;
    assert!(size_of::<Identity>() == 156);
    assert!(size_of::<ImageInfo>() == 42);
    assert!(size_of::<ImageMemXfer>() == 42);
    assert!(size_of::<Capability>() == 12);
    assert!(size_of::<UserInterface>() == 12);
    assert!(size_of::<TwEvent>() == 10);
    assert!(size_of::<PendingXfers>() == 6);
    assert!(size_of::<OneValue>() == 6);
};

type DsmEntry = unsafe extern "system" fn(*mut Identity, *mut Identity, u32, u16, u16, *mut c_void) -> u16;

// ---------- Win32 ----------

#[repr(C)]
struct Msg {
    hwnd: *mut c_void,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: [i32; 2],
    private: u32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn CreateWindowExW(ex: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: *mut c_void, menu: *mut c_void, inst: *mut c_void, param: *mut c_void) -> *mut c_void;
    fn DestroyWindow(hwnd: *mut c_void) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: *mut c_void, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
    fn GlobalLock(h: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(h: *mut c_void) -> i32;
    fn GlobalFree(h: *mut c_void) -> *mut c_void;
    fn GlobalSize(h: *mut c_void) -> usize;
}

const GHND: u32 = 0x0042;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn str32(s: &str) -> Str32 {
    let mut out = [0u8; 34];
    for (o, b) in out.iter_mut().zip(s.bytes().take(33)) {
        *o = b;
    }
    out
}

fn from_str32(s: &Str32) -> String {
    String::from_utf8_lossy(&s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())]).trim().to_string()
}

/// The data source manager, a hidden parent window, and our side of the conversation. Closed on drop.
struct Session {
    _lib: libloading::Library,
    entry: DsmEntry,
    app: Identity,
    hwnd: *mut c_void,
    open: bool,
}

impl Session {
    fn new() -> Result<Session, String> {
        // SAFETY: TWAINDSM.dll is the TWAIN Working Group's manager; DSM_Entry has the signature above.
        let lib = unsafe { libloading::Library::new("TWAINDSM.dll") }
            .map_err(|_| "No TWAIN driver found. Install your scanner's software (Epson Scan 2 includes the TWAIN driver), or scan with it and let ferrotype pick up the files.".to_string())?;
        let entry: DsmEntry = unsafe { *lib.get::<DsmEntry>(b"DSM_Entry\0").map_err(|e| e.to_string())? };
        let class = wide("STATIC");
        let title = wide("Ferrotype scanner");
        // SAFETY: a plain hidden STATIC window; destroyed on drop.
        let hwnd = unsafe { CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), null_mut(), null_mut()) };
        if hwnd.is_null() {
            return Err("couldn't create a window for the scanner".into());
        }
        let app = Identity {
            id: 0,
            version: Version { major: 1, minor: 0, language: 2, country: 1, info: str32(env!("CARGO_PKG_VERSION")) },
            protocol_major: 2,
            protocol_minor: 4,
            supported_groups: DG_CONTROL | DG_IMAGE,
            manufacturer: str32("Ferrotype"),
            product_family: str32("Ferrotype"),
            product_name: str32("Ferrotype"),
        };
        let mut s = Session { _lib: lib, entry, app, hwnd, open: false };
        let mut parent = s.hwnd;
        let rc = s.call(None, DG_CONTROL, DAT_PARENT, MSG_OPENDSM, &mut parent as *mut _ as *mut c_void);
        if rc != TWRC_SUCCESS {
            return Err(format!("the TWAIN manager wouldn't open (code {rc})"));
        }
        s.open = true;
        Ok(s)
    }

    fn call(&mut self, src: Option<&mut Identity>, dg: u32, dat: u16, msg: u16, data: *mut c_void) -> u16 {
        let src = src.map_or(null_mut(), |s| s as *mut Identity);
        // SAFETY: every `data` passed is the twain.h structure that this DG/DAT/MSG triplet takes.
        unsafe { (self.entry)(&mut self.app, src, dg, dat, msg, data) }
    }

    /// The scanner called `name`, the system's default for "", or None.
    fn find(&mut self, name: &str) -> Option<Identity> {
        let mut id: Identity = unsafe { std::mem::zeroed() };
        if name.is_empty() {
            return (self.call(None, DG_CONTROL, DAT_IDENTITY, MSG_GETDEFAULT, &mut id as *mut _ as _) == TWRC_SUCCESS).then_some(id);
        }
        let mut msg = MSG_GETFIRST;
        while self.call(None, DG_CONTROL, DAT_IDENTITY, msg, &mut id as *mut _ as _) == TWRC_SUCCESS {
            if from_str32(&id.product_name) == name {
                return Some(id);
            }
            msg = MSG_GETNEXT;
        }
        None
    }

    fn select(&mut self) -> Result<Identity, String> {
        let mut id: Identity = unsafe { std::mem::zeroed() };
        match self.call(None, DG_CONTROL, DAT_IDENTITY, MSG_USERSELECT, &mut id as *mut _ as _) {
            TWRC_SUCCESS => Ok(id),
            TWRC_CANCEL => Err("no scanner chosen".into()),
            rc => Err(format!("couldn't list scanners (code {rc})")),
        }
    }

    /// Set a one-value capability.
    fn set_cap(&mut self, src: &mut Identity, cap: u16, item_type: u16, value: u32) -> bool {
        // SAFETY: a TW_ONEVALUE in a movable global handle, as TWAIN 1.x sources expect; freed after the call.
        unsafe {
            let h = GlobalAlloc(GHND, std::mem::size_of::<OneValue>());
            if h.is_null() {
                return false;
            }
            let p = GlobalLock(h) as *mut OneValue;
            p.write_unaligned(OneValue { item_type, item: value });
            GlobalUnlock(h);
            let mut c = Capability { cap, con_type: TWON_ONEVALUE, container: h };
            let rc = self.call(Some(src), DG_CONTROL, DAT_CAPABILITY, MSG_SET, &mut c as *mut _ as _);
            GlobalFree(h);
            rc == TWRC_SUCCESS
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.open {
            let mut parent = self.hwnd;
            self.call(None, DG_CONTROL, DAT_PARENT, MSG_CLOSEDSM, &mut parent as *mut _ as *mut c_void);
        }
        // SAFETY: our own window, created in `new`.
        unsafe { DestroyWindow(self.hwnd) };
    }
}

/// Show the system's list of TWAIN scanners; the name of the one picked.
pub fn choose() -> Result<String, String> {
    let mut s = Session::new()?;
    s.select().map(|id| from_str32(&id.product_name))
}

/// Open the scanner's window and save every picture it sends into `dir`, until it is closed.
pub fn scan(dir: &Path, name: &str, say: &dyn Fn(Event)) -> Result<usize, String> {
    let mut s = Session::new()?;
    let mut src = match s.find(name) {
        Some(id) => id,
        None => s.select()?,
    };
    let scanner = from_str32(&src.product_name);
    let rc = s.call(None, DG_CONTROL, DAT_IDENTITY, MSG_OPENDS, &mut src as *mut _ as _);
    if rc != TWRC_SUCCESS {
        return Err(format!("{scanner} wouldn't open (code {rc}). Is it switched on and connected?"));
    }
    if !s.set_cap(&mut src, ICAP_XFERMECH, TWTY_UINT16, TWSX_MEMORY) {
        let mut id = src;
        s.call(None, DG_CONTROL, DAT_IDENTITY, MSG_CLOSEDS, &mut id as *mut _ as _);
        return Err(format!("{scanner} won't send pictures by memory transfer"));
    }
    // As many pictures as the user scans (-1 as a signed 16-bit value).
    s.set_cap(&mut src, CAP_XFERCOUNT, TWTY_INT16, 0xffff);
    let mut ui = UserInterface { show_ui: 1, modal_ui: 0, parent: s.hwnd };
    let rc = s.call(Some(&mut src), DG_CONTROL, DAT_USERINTERFACE, MSG_ENABLEDS, &mut ui as *mut _ as _);
    if rc != TWRC_SUCCESS && rc != 2 {
        let mut id = src;
        s.call(None, DG_CONTROL, DAT_IDENTITY, MSG_CLOSEDS, &mut id as *mut _ as _);
        return Err(format!("{scanner}'s window wouldn't open (code {rc})"));
    }
    say(Event::Status(format!("{scanner} is open: scan from its window")));
    let mut saved = 0;
    let mut error = None;
    // SAFETY: a standard message loop; every message goes to the source first while it is enabled.
    unsafe {
        let mut msg: Msg = std::mem::zeroed();
        loop {
            if GetMessageW(&mut msg, null_mut(), 0, 0) <= 0 {
                break;
            }
            let mut ev = TwEvent { event: &mut msg as *mut Msg as *mut c_void, message: 0 };
            let rc = s.call(Some(&mut src), DG_CONTROL, DAT_EVENT, MSG_PROCESSEVENT, &mut ev as *mut _ as _);
            let message = ev.message;
            if rc != TWRC_DSEVENT {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            match message {
                MSG_XFERREADY => match receive(&mut s, &mut src, dir, &scanner, say) {
                    Ok(n) => saved += n,
                    Err(e) => {
                        error = Some(e);
                        break;
                    }
                },
                MSG_CLOSEDSREQ | MSG_CLOSEDSOK => break,
                _ => {}
            }
        }
    }
    let mut ui = UserInterface { show_ui: 0, modal_ui: 0, parent: s.hwnd };
    s.call(Some(&mut src), DG_CONTROL, DAT_USERINTERFACE, MSG_DISABLEDS, &mut ui as *mut _ as _);
    let mut id = src;
    s.call(None, DG_CONTROL, DAT_IDENTITY, MSG_CLOSEDS, &mut id as *mut _ as _);
    match error {
        Some(e) if saved == 0 => Err(e),
        Some(e) => {
            say(Event::Status(e));
            Ok(saved)
        }
        None => Ok(saved),
    }
}

/// Take every picture the source has ready.
fn receive(s: &mut Session, src: &mut Identity, dir: &Path, scanner: &str, say: &dyn Fn(Event)) -> Result<usize, String> {
    let mut saved = 0;
    loop {
        say(Event::Status(format!("Receiving scan {} from {scanner}…", saved + 1)));
        let picture = take(s, src);
        let mut pending = PendingXfers { count: 0, eoj: 0 };
        s.call(Some(src), DG_CONTROL, DAT_PENDINGXFERS, MSG_ENDXFER, &mut pending as *mut _ as _);
        match picture {
            Ok(Some((w, h, samples, bits, dpi, px, icc))) => {
                let dest = super::next_name(dir);
                let part = dir.join(format!(".{}.part", dest.file_name().unwrap_or_default().to_string_lossy()));
                let info = crate::scanfile::Info { software: format!("Ferrotype {} (TWAIN)", env!("CARGO_PKG_VERSION")), make: from_str32(&src.manufacturer), model: scanner.into(), dpi, ..Default::default() };
                crate::scanfile::write_tiff(&part, w, h, samples, bits, &px, icc.as_deref(), &info)
                    .and_then(|()| std::fs::rename(&part, &dest).map_err(|e| e.to_string()))
                    .map_err(|e| format!("couldn't save the scan: {e}"))?;
                saved += 1;
                say(Event::Saved(dest));
            }
            Ok(None) => {} // cancelled in the scanner's window
            Err(e) => {
                if pending.count != 0 {
                    let mut p = PendingXfers { count: 0, eoj: 0 };
                    s.call(Some(src), DG_CONTROL, DAT_PENDINGXFERS, MSG_RESET, &mut p as *mut _ as _);
                }
                return Err(e);
            }
        }
        if pending.count == 0 {
            return Ok(saved);
        }
    }
}

type Picture = (u32, u32, usize, u16, f32, Vec<u16>, Option<Vec<u8>>);

/// One picture by memory transfer: (width, height, samples, bits, ppi, samples as u16, ICC profile).
fn take(s: &mut Session, src: &mut Identity) -> Result<Option<Picture>, String> {
    let mut info = ImageInfo::default();
    if s.call(Some(src), DG_IMAGE, DAT_IMAGEINFO, MSG_GET, &mut info as *mut _ as _) != TWRC_SUCCESS {
        return Err("the scanner didn't describe the picture".into());
    }
    let (w, h) = (info.width, info.length);
    let (pixel_type, bits) = (info.pixel_type, info.bits_per_sample[0]);
    let samples = match pixel_type {
        TWPT_GRAY => 1,
        TWPT_RGB => 3,
        _ => return Err("only colour and greyscale scans can be received: set the scanner to Color or Grayscale".into()),
    };
    if w <= 0 || h <= 0 || !(bits == 8 || bits == 16) || info.planar != 0 {
        return Err(format!("unsupported picture from the scanner ({w}×{h}, {bits} bits)"));
    }
    let (w, h, bytes) = (w as usize, h as usize, bits as usize / 8);
    let dpi = info.x_resolution.whole as f32 + info.x_resolution.frac as f32 / 65536.0;
    let mut setup = SetupMemXfer::default();
    s.call(Some(src), DG_CONTROL, DAT_SETUPMEMXFER, MSG_GET, &mut setup as *mut _ as _);
    let size = match setup.preferred {
        0 | TWON_DONTCARE32 => setup.max_buf_size.min(4 << 20).max(setup.min_buf_size).max(64 << 10),
        p => p.max(setup.min_buf_size),
    } as usize;
    let mut buf = vec![0u8; size];
    let mut px = vec![0u16; w * h * samples];
    loop {
        let mut x = ImageMemXfer {
            compression: TWON_DONTCARE16,
            bytes_per_row: TWON_DONTCARE32,
            columns: TWON_DONTCARE32,
            rows: TWON_DONTCARE32,
            x_offset: TWON_DONTCARE32,
            y_offset: TWON_DONTCARE32,
            bytes_written: TWON_DONTCARE32,
            memory: Memory { flags: TWMF_APPOWNS | TWMF_POINTER, length: size as u32, the_mem: buf.as_mut_ptr() as *mut c_void },
        };
        let rc = s.call(Some(src), DG_IMAGE, DAT_IMAGEMEMXFER, MSG_GET, &mut x as *mut _ as _);
        if rc == TWRC_CANCEL {
            return Ok(None);
        }
        if rc != TWRC_SUCCESS && rc != TWRC_XFERDONE {
            return Err(format!("the transfer from the scanner failed (code {rc})"));
        }
        // A source that leaves an offset unset would send it as "don't care": count that as 0.
        let at = |v: u32| if v == TWON_DONTCARE32 { 0 } else { v as usize };
        super::copy_strip(&mut px, w, h, samples, bytes, &buf, x.bytes_per_row as usize, x.columns as usize, x.rows as usize, at(x.x_offset), at(x.y_offset));
        if rc == TWRC_XFERDONE {
            break;
        }
    }
    Ok(Some((w as u32, h as u32, samples, bits as u16, dpi, px, icc(s, src))))
}

/// The source's ICC profile for this picture, if it gives one (state 7, before the transfer ends).
fn icc(s: &mut Session, src: &mut Identity) -> Option<Vec<u8>> {
    let mut m = Memory { flags: 0, length: 0, the_mem: null_mut() };
    if s.call(Some(src), DG_IMAGE, DAT_ICCPROFILE, MSG_GET, &mut m as *mut _ as _) != TWRC_SUCCESS || m.the_mem.is_null() || m.length == 0 {
        return None;
    }
    let (flags, len, mem) = (m.flags, m.length as usize, m.the_mem);
    // SAFETY: the source allocated it (a handle or a pointer, per its flags) and it is ours to free.
    unsafe {
        let bytes = if flags & TWMF_HANDLE != 0 {
            let p = GlobalLock(mem) as *const u8;
            let v = (!p.is_null()).then(|| std::slice::from_raw_parts(p, len.min(GlobalSize(mem))).to_vec());
            GlobalUnlock(mem);
            v
        } else {
            Some(std::slice::from_raw_parts(mem as *const u8, len).to_vec())
        };
        // Ours to free unless the source keeps it. A pointer comes from a fixed global allocation, which
        // GlobalFree takes as well.
        if flags & TWMF_DSOWNS == 0 {
            GlobalFree(mem);
        }
        bytes.filter(|b| b.len() > 128)
    }
}
