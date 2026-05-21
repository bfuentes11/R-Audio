use std::path::Path;
use slint::SharedPixelBuffer;

use std::num::NonZeroUsize;
use std::sync::LazyLock;
use lru::LruCache;
use tokio::sync::{Mutex, Semaphore};

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

// Single shared HTTP client — avoids re-creating a TLS context + connection
// pool for every image fetch. reqwest::Client is cheaply cloneable (Arc inside).
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

// Decoded-image cache keyed by Spotify image hash. SharedPixelBuffer is
// reference-counted, so handing out clones costs nothing — many tracks in a
// playlist or album share the same cover art and can all point at one buffer.
// Bounded LRU so a long-running kiosk session doesn't accumulate every cover
// ever scrolled past. ~512 covers ≈ a few MB at 300×300 RGBA.
const IMAGE_CACHE_CAPACITY: usize = 512;
static IMAGE_MEM_CACHE: LazyLock<Mutex<LruCache<String, SharedPixelBuffer<slint::Rgba8Pixel>>>> =
    LazyLock::new(|| Mutex::new(LruCache::new(NonZeroUsize::new(IMAGE_CACHE_CAPACITY).unwrap())));

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
    let mem_key = cache_id.to_string();

    // Fast path: decoded buffer already in memory (shared across all tracks
    // with the same cover art).
    if let Some(buf) = IMAGE_MEM_CACHE.lock().await.get(&mem_key).cloned() {
        return Some(buf);
    }

    if Path::new(&cache_path).exists() {
        if let Ok(bytes) = tokio::fs::read(&cache_path).await {
            if let Ok(Some(buf)) = tokio::task::spawn_blocking(move || decode_image(&bytes)).await {
                // Another task may have decoded the same image while we were on the
                // blocking pool. Prefer the existing entry so all callers share one
                // SharedPixelBuffer instead of holding parallel decoded copies.
                let mut cache = IMAGE_MEM_CACHE.lock().await;
                if let Some(existing) = cache.get(&mem_key).cloned() {
                    return Some(existing);
                }
                cache.put(mem_key, buf.clone());
                return Some(buf);
            }
        }
    }

    if !url.is_empty() {
        let _permit = IMAGE_SEMAPHORE.acquire().await;
        // Re-check the cache after waiting on the semaphore — another task may
        // have decoded the same image while we were queued.
        if let Some(buf) = IMAGE_MEM_CACHE.lock().await.get(&mem_key).cloned() {
            return Some(buf);
        }
        if let Ok(response) = HTTP_CLIENT.get(url).send().await {
            if let Ok(bytes) = response.bytes().await {
                let _ = tokio::fs::create_dir_all(cache_dir).await;
                let _ = tokio::fs::write(&cache_path, &bytes).await;

                let bytes_vec = bytes.to_vec();
                if let Ok(Some(buf)) = tokio::task::spawn_blocking(move || decode_image(&bytes_vec)).await {
                    // Same race window as the disk path — prefer an existing entry.
                    let mut cache = IMAGE_MEM_CACHE.lock().await;
                    if let Some(existing) = cache.get(&mem_key).cloned() {
                        return Some(existing);
                    }
                    cache.put(mem_key, buf.clone());
                    return Some(buf);
                }
            }
        }
    }

    None
}

fn decode_image(bytes: &[u8]) -> Option<SharedPixelBuffer<slint::Rgba8Pixel>> {
    let dynamic_image = image::load_from_memory(bytes).ok()?;
    let rgba = dynamic_image.into_rgba8();
    Some(SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}
