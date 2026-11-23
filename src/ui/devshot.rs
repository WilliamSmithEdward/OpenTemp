//! Development aid (feature `dev-screenshot`): set `OPENTEMP_SCREENSHOT=out.png`
//! and the app saves a PNG of its window after `OPENTEMP_SCREENSHOT_AFTER`
//! seconds (default 4) and exits. `OPENTEMP_SCREENSHOT_SIZE=1280x820` resizes the
//! window first. Used to review the UI in automation.

use std::io::Write;

pub struct DevShot {
    path: Option<String>,
    after: f64,
    requested: bool,
    sized: bool,
}

impl DevShot {
    pub fn from_env() -> Self {
        Self {
            path: std::env::var("OPENTEMP_SCREENSHOT").ok(),
            after: std::env::var("OPENTEMP_SCREENSHOT_AFTER").ok().and_then(|v| v.parse().ok()).unwrap_or(4.0),
            requested: false,
            sized: false,
        }
    }

    pub fn tick(&mut self, ctx: &egui::Context) {
        let Some(path) = self.path.clone() else { return };
        let t = ctx.input(|i| i.time);
        if !self.sized {
            self.sized = true;
            match std::env::var("OPENTEMP_SCREENSHOT_THEME").as_deref() {
                Ok("light") => ctx.set_theme(egui::ThemePreference::Light),
                Ok("dark") => ctx.set_theme(egui::ThemePreference::Dark),
                _ => {}
            }
            if let Some((w, h)) = std::env::var("OPENTEMP_SCREENSHOT_SIZE").ok().and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
            }) {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
            }
        }
        if !self.requested && t >= self.after {
            self.requested = true;
            eprintln!("viewport: {:?}", ctx.input(|i| i.viewport().inner_rect));
            let cb = egui::ScreenshotCallback::new(move |img| {
                if let Err(e) = write_png(&path, img.width(), img.height(), img.as_raw()) {
                    eprintln!("screenshot failed: {e}");
                }
                std::process::exit(0);
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(cb));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// Minimal PNG writer: RGBA8, zlib "stored" (uncompressed) blocks.
fn write_png(path: &str, w: usize, h: usize, rgba: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for row in rgba.chunks(w * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    for (i, chunk) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        z.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = std::fs::File::create(path)?;
    out.write_all(b"\x89PNG\r\n\x1a\n")?;
    let mut chunk = |ty: &[u8], data: &[u8]| -> std::io::Result<()> {
        out.write_all(&(data.len() as u32).to_be_bytes())?;
        let mut c = ty.to_vec();
        c.extend_from_slice(data);
        out.write_all(&c)?;
        out.write_all(&crc32(&c).to_be_bytes())
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr)?;
    chunk(b"IDAT", &z)?;
    chunk(b"IEND", &[])
}
