use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tokio::sync::Mutex;
use crate::providers::Track;

pub struct SpotifyCache {
    track_cache: Mutex<HashMap<String, Track>>,
    playlists_cache: Mutex<Option<Vec<Track>>>,
    // In-memory hot layers
    playlist_mem_cache: Mutex<HashMap<String, (String, String, String, Vec<Track>)>>,
    artist_mem_cache: Mutex<HashMap<String, (String, String, Vec<Track>, Vec<Track>)>>,
    album_mem_cache: Mutex<HashMap<String, (String, String, String, Vec<Track>)>>,
    // Reverse index: track_id -> Vec<playlist_id>
    playlist_tracks_index: Mutex<HashMap<String, Vec<String>>>,
}

impl SpotifyCache {
    pub fn new() -> Self {
        Self {
            track_cache: Mutex::new(HashMap::new()),
            playlists_cache: Mutex::new(None),
            playlist_mem_cache: Mutex::new(HashMap::new()),
            artist_mem_cache: Mutex::new(HashMap::new()),
            album_mem_cache: Mutex::new(HashMap::new()),
            playlist_tracks_index: Mutex::new(HashMap::new()),
        }
    }

    // --- In-Memory Track Cache ---
    pub async fn get_track(&self, id: &str) -> Option<Track> {
        let cache = self.track_cache.lock().await;
        cache.get(id).cloned()
    }

    pub async fn insert_track(&self, id: String, track: Track) {
        let mut cache = self.track_cache.lock().await;
        cache.insert(id, track);
    }

    // --- In-Memory Playlist Cache (User's Playlists) ---
    pub async fn get_user_playlists(&self) -> Option<Vec<Track>> {
        let cache = self.playlists_cache.lock().await;
        cache.clone()
    }

    pub async fn save_user_playlists(&self, playlists: Vec<Track>) {
        let mut cache = self.playlists_cache.lock().await;
        *cache = Some(playlists);
    }

    // --- Async & In-Memory Playlist Cache ---
    pub async fn get_playlist(&self, playlist_id: &str, snapshot_id: &str) -> Option<(String, String, String, Vec<Track>)> {
        // Check in-memory layer first
        {
            let mem = self.playlist_mem_cache.lock().await;
            if let Some(cached) = mem.get(playlist_id) {
                return Some(cached.clone());
            }
        }

        let playlist_id = playlist_id.to_string();
        let snapshot_id = snapshot_id.to_string();
        let playlist_id_clone = playlist_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let cache_dir = ".cache/playlists";
            let safe_snapshot = snapshot_id.replace("/", "_").replace("+", "-").replace("=", "");
            let cache_file = format!("{}/{}_{}.json", cache_dir, playlist_id_clone, safe_snapshot);

            if Path::new(&cache_file).exists() {
                if let Ok(file_content) = fs::read_to_string(&cache_file) {
                    if let Ok(cached_data) = serde_json::from_str::<(String, String, String, Vec<Track>)>(&file_content) {
                        return Some(cached_data);
                    }
                }
            }
            None
        }).await.ok()?;

        if let Some(ref data) = result {
            // Update in-memory
            let mut mem = self.playlist_mem_cache.lock().await;
            mem.insert(playlist_id.clone(), data.clone());
            
            // Update reverse index
            let mut index = self.playlist_tracks_index.lock().await;
            for track in &data.3 {
                let entry = index.entry(track.id.clone()).or_default();
                if !entry.contains(&playlist_id) {
                    entry.push(playlist_id.clone());
                }
            }
        }

        result
    }

    pub async fn save_playlist(&self, playlist_id: &str, snapshot_id: &str, data: &(String, String, String, Vec<Track>)) {
        // Update in-memory
        {
            let mut mem = self.playlist_mem_cache.lock().await;
            mem.insert(playlist_id.to_string(), data.clone());
        }

        // Update reverse index
        {
            let mut index = self.playlist_tracks_index.lock().await;
            for tracks in index.values_mut() {
                tracks.retain(|id| id != playlist_id);
            }
            for track in &data.3 {
                let entry = index.entry(track.id.clone()).or_default();
                if !entry.contains(&playlist_id.to_string()) {
                    entry.push(playlist_id.to_string());
                }
            }
        }

        let playlist_id = playlist_id.to_string();
        let snapshot_id = snapshot_id.to_string();
        let data = data.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let cache_dir = ".cache/playlists";
            let _ = fs::create_dir_all(cache_dir);
            let safe_snapshot = snapshot_id.replace("/", "_").replace("+", "-").replace("=", "");
            let cache_file = format!("{}/{}_{}.json", cache_dir, playlist_id, safe_snapshot);

            // Remove old cache files for this playlist first
            if let Ok(entries) = fs::read_dir(cache_dir) {
                for entry in entries.flatten() {
                    if let Ok(file_name) = entry.file_name().into_string() {
                        if file_name.starts_with(&playlist_id) {
                            let _ = fs::remove_file(entry.path());
                        }
                    }
                }
            }

            if let Ok(json_str) = serde_json::to_string(&data) {
                let _ = fs::write(&cache_file, json_str);
            }
        }).await;
    }

    // --- Async & In-Memory Artist Cache ---
    pub async fn get_artist(&self, artist_id: &str) -> Option<(String, String, Vec<Track>, Vec<Track>)> {
        // Check in-memory layer
        {
            let mem = self.artist_mem_cache.lock().await;
            if let Some(cached) = mem.get(artist_id) {
                return Some(cached.clone());
            }
        }

        let artist_id = artist_id.to_string();
        let artist_id_clone = artist_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let cache_dir = ".cache/artists";
            let cache_file = format!("{}/{}.json", cache_dir, artist_id_clone);

            if Path::new(&cache_file).exists() {
                if let Ok(content) = fs::read_to_string(&cache_file) {
                    if let Ok(data) = serde_json::from_str::<(String, String, Vec<Track>, Vec<Track>)>(&content) {
                        return Some(data);
                    }
                }
            }
            None
        }).await.ok()?;

        if let Some(ref data) = result {
            let mut mem = self.artist_mem_cache.lock().await;
            mem.insert(artist_id, data.clone());
        }

        result
    }

    pub async fn save_artist(&self, artist_id: &str, data: &(String, String, Vec<Track>, Vec<Track>)) {
        // Update in-memory
        {
            let mut mem = self.artist_mem_cache.lock().await;
            mem.insert(artist_id.to_string(), data.clone());
        }

        let artist_id = artist_id.to_string();
        let data = data.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let cache_dir = ".cache/artists";
            let _ = fs::create_dir_all(cache_dir);
            let cache_file = format!("{}/{}.json", cache_dir, artist_id);

            if let Ok(json_str) = serde_json::to_string(&data) {
                let _ = fs::write(&cache_file, json_str);
            }
        }).await;
    }

    // --- Async & In-Memory Album Cache ---
    pub async fn get_album(&self, album_id: &str) -> Option<(String, String, String, Vec<Track>)> {
        // Check in-memory layer
        {
            let mem = self.album_mem_cache.lock().await;
            if let Some(cached) = mem.get(album_id) {
                return Some(cached.clone());
            }
        }

        let album_id = album_id.to_string();
        let album_id_clone = album_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let cache_dir = ".cache/albums";
            let cache_file = format!("{}/{}.json", cache_dir, album_id_clone);

            if Path::new(&cache_file).exists() {
                if let Ok(content) = fs::read_to_string(&cache_file) {
                    if let Ok(data) = serde_json::from_str::<(String, String, String, Vec<Track>)>(&content) {
                        return Some(data);
                    }
                }
            }
            None
        }).await.ok()?;

        if let Some(ref data) = result {
            let mut mem = self.album_mem_cache.lock().await;
            mem.insert(album_id, data.clone());
        }

        result
    }

    pub async fn save_album(&self, album_id: &str, data: &(String, String, String, Vec<Track>)) {
        // Update in-memory
        {
            let mut mem = self.album_mem_cache.lock().await;
            mem.insert(album_id.to_string(), data.clone());
        }

        let album_id = album_id.to_string();
        let data = data.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let cache_dir = ".cache/albums";
            let _ = fs::create_dir_all(cache_dir);
            let cache_file = format!("{}/{}.json", cache_dir, album_id);

            if let Ok(json_str) = serde_json::to_string(&data) {
                let _ = fs::write(&cache_file, json_str);
            }
        }).await;
    }

    // --- Background Playlist Indexing ---
    pub async fn initialize_index(&self) {
        let mut index = self.playlist_tracks_index.lock().await;
        let mut mem_cache = self.playlist_mem_cache.lock().await;

        let result = tokio::task::spawn_blocking(move || {
            let cache_dir = ".cache/playlists";
            let mut playlists = Vec::new();
            if let Ok(entries) = fs::read_dir(cache_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                        if let Ok(file_name) = entry.file_name().into_string() {
                            if let Some(underscore_pos) = file_name.rfind('_') {
                                let playlist_id = file_name[..underscore_pos].to_string();
                                if let Ok(content) = fs::read_to_string(&path) {
                                    if let Ok(data) = serde_json::from_str::<(String, String, String, Vec<Track>)>(&content) {
                                        playlists.push((playlist_id, data));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            playlists
        }).await;

        if let Ok(playlists) = result {
            println!("[cache] Background indexing completed. Indexed {} playlists.", playlists.len());
            for (playlist_id, data) in playlists {
                mem_cache.insert(playlist_id.clone(), data.clone());
                for track in &data.3 {
                    let entry = index.entry(track.id.clone()).or_default();
                    if !entry.contains(&playlist_id) {
                        entry.push(playlist_id.clone());
                    }
                }
            }
        }
    }

    pub async fn get_playlists_containing_track(&self, track_id: &str) -> Vec<String> {
        let index = self.playlist_tracks_index.lock().await;
        index.get(track_id).cloned().unwrap_or_default()
    }

    // --- User Dashboard Disk Cache ---
    fn get_user_cache_path(&self, username: &str, category: &str) -> String {
        let sanitized = username.replace(|c: char| !c.is_alphanumeric(), "_");
        format!(".cache/user_{}_{}.json", sanitized, category)
    }

    pub async fn get_user_playlists_disk(&self, username: &str) -> Option<Vec<Track>> {
        let path = self.get_user_cache_path(username, "playlists");
        tokio::task::spawn_blocking(move || {
            if Path::new(&path).exists() {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(data) = serde_json::from_str::<Vec<Track>>(&content) {
                        return Some(data);
                    }
                }
            }
            None
        }).await.ok()?
    }

    pub async fn save_user_playlists_disk(&self, username: &str, playlists: Vec<Track>) {
        self.save_user_playlists(playlists.clone()).await;
        let path = self.get_user_cache_path(username, "playlists");
        let _ = tokio::task::spawn_blocking(move || {
            let _ = fs::create_dir_all(".cache");
            if let Ok(json_str) = serde_json::to_string(&playlists) {
                let _ = fs::write(&path, json_str);
            }
        }).await;
    }

    pub async fn get_recently_played_disk(&self, username: &str) -> Option<Vec<Track>> {
        let path = self.get_user_cache_path(username, "recently_played");
        tokio::task::spawn_blocking(move || {
            if Path::new(&path).exists() {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(data) = serde_json::from_str::<Vec<Track>>(&content) {
                        return Some(data);
                    }
                }
            }
            None
        }).await.ok()?
    }

    pub async fn save_recently_played_disk(&self, username: &str, tracks: Vec<Track>) {
        let path = self.get_user_cache_path(username, "recently_played");
        let _ = tokio::task::spawn_blocking(move || {
            let _ = fs::create_dir_all(".cache");
            if let Ok(json_str) = serde_json::to_string(&tracks) {
                let _ = fs::write(&path, json_str);
            }
        }).await;
    }

    pub async fn get_top_artists_disk(&self, username: &str) -> Option<Vec<Track>> {
        let path = self.get_user_cache_path(username, "top_artists");
        tokio::task::spawn_blocking(move || {
            if Path::new(&path).exists() {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(data) = serde_json::from_str::<Vec<Track>>(&content) {
                        return Some(data);
                    }
                }
            }
            None
        }).await.ok()?
    }

    pub async fn save_top_artists_disk(&self, username: &str, artists: Vec<Track>) {
        let path = self.get_user_cache_path(username, "top_artists");
        let _ = tokio::task::spawn_blocking(move || {
            let _ = fs::create_dir_all(".cache");
            if let Ok(json_str) = serde_json::to_string(&artists) {
                let _ = fs::write(&path, json_str);
            }
        }).await;
    }
}
