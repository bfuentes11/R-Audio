use std::fs;
use std::path::Path;
use slint::SharedPixelBuffer;

use std::sync::LazyLock;
use tokio::sync::Semaphore;

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
