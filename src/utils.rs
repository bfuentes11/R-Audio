use std::fs;
use std::path::Path;
use slint::SharedPixelBuffer;

use std::sync::LazyLock;
use tokio::sync::Semaphore;

// ── Colour helpers ────────────────────────────────────────────────────────────

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let h = if delta < 1e-6 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };

    let s = if max < 1e-6 { 0.0 } else { delta / max };
    (h, s, max)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if h < 60.0 { (c, x, 0.0) }
        else if h < 120.0 { (x, c, 0.0) }
        else if h < 180.0 { (0.0, c, x) }
        else if h < 240.0 { (0.0, x, c) }
        else if h < 300.0 { (x, 0.0, c) }
        else              { (c, 0.0, x) };
    (r + m, g + m, b + m)
}

/// Sample the album art, return the hue/saturation/value of the most vibrant pixel cluster.
pub fn extract_dominant_hsv(buf: &SharedPixelBuffer<slint::Rgba8Pixel>) -> (f32, f32, f32) {
    let w = buf.width() as usize;
    let h = buf.height() as usize;
    let pixels = buf.as_slice();
    let step = (w.min(h) / 20).max(1);

    // Default: deep indigo — used when the art is monochrome/greyscale
    let mut best = (240.0f32, 0.6f32, 0.5f32);
    let mut best_score = -1.0f32;

    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let p = &pixels[y * w + x];
            if p.a < 64 { continue; }

            let (hue, sat, val) = rgb_to_hsv(p.r as f32 / 255.0, p.g as f32 / 255.0, p.b as f32 / 255.0);

            // Skip almost-black, almost-white, and near-grey pixels
            if val < 0.12 || val > 0.96 || sat < 0.15 { continue; }

            // Score peaks at high saturation + medium-high brightness
            let score = sat * (1.0 - (val - 0.62).abs() * 1.5).max(0.0);
            if score > best_score {
                best_score = score;
                best = (hue, sat, val);
            }
        }
    }

    let (hue, sat, val) = best;
    (hue, sat.max(0.35), val.max(0.4))
}

/// Build a 2×256 vertical gradient image from the dominant HSV colour.
/// Top: darkened vibrant hue; middle: much darker; bottom: near-black.
/// The tiny image is stretched to fill the full player via image-fit: fill.
pub fn build_album_gradient_image(h: f32, s: f32, v: f32) -> SharedPixelBuffer<slint::Rgba8Pixel> {
    const HEIGHT: u32 = 256;
    const WIDTH: u32 = 2;

    let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(WIDTH, HEIGHT);
    let pixels = buf.make_mut_slice();

    for y in 0..HEIGHT {
        let t = y as f32 / (HEIGHT - 1) as f32;

        // Three-stop curve: vibrant-dark → very dark → near-black
        let cur_v = if t < 0.55 {
            let u = t / 0.55;
            v * 0.40 * (1.0 - u) + v * 0.14 * u
        } else {
            let u = (t - 0.55) / 0.45;
            v * 0.14 * (1.0 - u) + v * 0.05 * u
        };
        let cur_s = s * (1.0 - t * 0.35);

        let (r, g, b) = hsv_to_rgb(h, cur_s.min(1.0), cur_v.min(1.0));
        let pixel = slint::Rgba8Pixel {
            r: (r * 255.0) as u8,
            g: (g * 255.0) as u8,
            b: (b * 255.0) as u8,
            a: 250, // ~98% opaque — lets the window ambient glow bleed through slightly
        };

        for x in 0..WIDTH {
            pixels[(y * WIDTH + x) as usize] = pixel;
        }
    }

    buf
}

static IMAGE_SEMAPHORE: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(8));

pub async fn fetch_and_cache_image(
    id: &str,
    url: &str,
) -> Option<SharedPixelBuffer<slint::Rgba8Pixel>> {
    let cache_dir = ".cache/images";
    
    // Extract unique image hash from Spotify URL to allow multiple tracks to share the same cache file!
    let cache_id = if !url.is_empty() {
        if let Some(pos) = url.rfind('/') {
            &url[pos + 1..]
        } else {
            id
        }
    } else {
        id
    };
    
    let cache_path = format!("{}/{}.png", cache_dir, cache_id);

    if Path::new(&cache_path).exists() {
        if let Ok(bytes) = fs::read(&cache_path) {
            if let Ok(dynamic_image) = image::load_from_memory(&bytes) {
                let rgba = dynamic_image.into_rgba8();
                return Some(SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    rgba.as_raw(),
                    rgba.width(),
                    rgba.height(),
                ));
            }
        }
    }

    if !url.is_empty() {
        let _permit = IMAGE_SEMAPHORE.acquire().await;
        if let Ok(response) = reqwest::get(url).await {
            if let Ok(bytes) = response.bytes().await {
                // Save original compressed bytes directly to disk cache
                let _ = fs::create_dir_all(cache_dir);
                let _ = fs::write(&cache_path, &bytes);

                if let Ok(dynamic_image) = image::load_from_memory(&bytes) {
                    let rgba = dynamic_image.into_rgba8();
                    return Some(SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        rgba.as_raw(),
                        rgba.width(),
                        rgba.height(),
                    ));
                }
            }
        }
    }

    None
}
