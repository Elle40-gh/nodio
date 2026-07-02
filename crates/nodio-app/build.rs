fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let ico_path = format!("{out_dir}/nodio.ico");

    write_ico(&ico_path);

    let mut res = winres::WindowsResource::new();
    res.set_icon(&ico_path);
    res.compile().unwrap();
}

fn write_ico(path: &str) {
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16u32, 32, 48] {
        let rgba = icon_rgba(size);
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        icon_dir.add_entry(ico::IconDirEntry::encode(&image).unwrap());
    }
    let file = std::fs::File::create(path).unwrap();
    icon_dir.write(file).unwrap();
}

fn icon_rgba(size: u32) -> Vec<u8> {
    let w = size;
    let h = size;
    let s = size as f32 / 32.0; // scale factor relative to 32px reference
    let mut rgba = vec![0u8; (w * h * 4) as usize];

    fn paint(rgba: &mut [u8], w: u32, h: u32, cx: f32, cy: f32, radius: f32) {
        let x0 = (cx - radius - 1.0).max(0.0) as u32;
        let x1 = ((cx + radius + 2.0).min(w as f32)) as u32;
        let y0 = (cy - radius - 1.0).max(0.0) as u32;
        let y1 = ((cy + radius + 2.0).min(h as f32)) as u32;
        for py in y0..y1 {
            for px in x0..x1 {
                let dx = px as f32 + 0.5 - cx;
                let dy = py as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let a = ((radius - dist + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
                if a == 0 {
                    continue;
                }
                let i = ((py * w + px) * 4) as usize;
                if a > rgba[i + 3] {
                    rgba[i] = 100;
                    rgba[i + 1] = 210;
                    rgba[i + 2] = 255;
                    rgba[i + 3] = a;
                }
            }
        }
    }

    paint(&mut rgba, w, h, 5.5 * s, h as f32 / 2.0, 4.5 * s);
    paint(&mut rgba, w, h, 26.5 * s, h as f32 / 2.0, 4.5 * s);
    for step in 0..=120_u32 {
        let t = step as f32 / 120.0;
        let wx = (9.5 + t * 13.0) * s;
        let wy = h as f32 / 2.0 + 5.0 * s * (t * std::f32::consts::PI * 3.0).sin();
        paint(&mut rgba, w, h, wx, wy, 1.5 * s.max(0.75));
    }

    rgba
}
