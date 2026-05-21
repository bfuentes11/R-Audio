use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use crate::providers::Track;

pub struct SpotifyCache {
    track_cache: Mutex<HashMap<String, Track>>,
    playlists_cache: Mutex<Option<Vec<Track>>>,
    // In-memory hot layers — std::sync::Mutex is fine: no guards held across .await.
    playlist_mem_cache: Mutex<HashMap<String, (String, String, String, Vec<Track>)>>,
    artist_mem_cache: Mutex<HashMap<String, (String, String, Vec<Track>, Vec<Track>)>>,
    album_mem_cache: Mutex<HashMap<String, (String, String, String, Vec<Track>)>>,
    // Reverse index: track_id -> Vec<playlist_id>
    playlist_tracks_index: Mutex<HashMap<String, Vec<String>>>,
    // Forward index: playlist_id -> set of track_ids it currently contains.
    // Lets save_playlist diff the change instead of scanning every entry of the
    // reverse index to retain().
    playlist_forward_index: Mutex<HashMap<String, HashSet<String>>>,
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
            playlist_forward_index: Mutex::new(HashMap::new()),
        }
    }

    // --- In-Memory Track Cache ---
    pub async fn get_track(&self, id: &str) -> Option<Track> {
        self.track_cache.lock().unwrap().get(id).cloned()
    }

    pub async fn insert_track(&self, id: String, track: Track) {
        self.track_cache.lock().unwrap().insert(id, track);
    }

    // --- In-Memory Playlist Cache (User's Playlists) ---
    pub async fn get_user_playlists(&self) -> Option<Vec<Track>> {
        self.playlists_cache.lock().unwrap().clone()
    }

    pub async fn save_user_playlists(&self, playlists: Vec<Track>) {
        *self.playlists_cache.lock().unwrap() = Some(playlists);
    }

    /// Apply a playlist's track membership to the reverse + forward indexes.
    /// Only touches tracks that were actually added or removed.
    fn apply_index_change(&self, playlist_id: &str, new_tracks: &[Track]) {
        let new_set: HashSet<String> = new_tracks.iter().map(|t| {
            t.id.strip_prefix("spotify:track:").unwrap_or(&t.id).to_string()
        }).collect();

        let mut forward = self.playlist_forward_index.lock().unwrap();
        let mut reverse = self.playlist_tracks_index.lock().unwrap();

        let old_set = forward.remove(playlist_id).unwrap_or_default();

        // Tracks that left this playlist: drop playlist_id from each.
        for removed in old_set.difference(&new_set) {
            if let Some(entry) = reverse.get_mut(removed) {
                entry.retain(|p| p != playlist_id);
                if entry.is_empty() {
                    reverse.remove(removed);
                }
            }
        }

        // Tracks that joined this playlist: append playlist_id if not present.
        for added in new_set.difference(&old_set) {
            let entry = reverse.entry(added.clone()).or_default();
            if !entry.iter().any(|p| p == playlist_id) {
                entry.push(playlist_id.to_string());
            }
        }

        forward.insert(playlist_id.to_string(), new_set);
    }

    // --- Async & In-Memory Playlist Cache ---
    pub async fn get_playlist(&self, playlist_id: &str, snapshot_id: &str) -> Option<(String, String, String, Vec<Track>)> {
        // Check in-memory layer first
        if let Some(cached) = self.playlist_mem_cache.lock().unwrap().get(playlist_id).cloned() {
            return Some(cached);
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
            self.playlist_mem_cache.lock().unwrap().insert(playlist_id.clone(), data.clone());
            self.apply_index_change(&playlist_id, &data.3);
        }

        result
    }

    pub async fn save_playlist(&self, playlist_id: &str, snapshot_id: &str, data: &(String, String, String, Vec<Track>)) {
        self.playlist_mem_cache.lock().unwrap().insert(playlist_id.to_string(), data.clone());
        self.apply_index_change(playlist_id, &data.3);

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
        if let Some(cached) = self.artist_mem_cache.lock().unwrap().get(artist_id).cloned() {
            return Some(cached);
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
            self.artist_mem_cache.lock().unwrap().insert(artist_id, data.clone());
        }

        result
    }

    pub async fn save_artist(&self, artist_id: &str, data: &(String, String, Vec<Track>, Vec<Track>)) {
        self.artist_mem_cache.lock().unwrap().insert(artist_id.to_string(), data.clone());

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
        if let Some(cached) = self.album_mem_cache.lock().unwrap().get(album_id).cloned() {
            return Some(cached);
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
            self.album_mem_cache.lock().unwrap().insert(album_id, data.clone());
        }

        result
    }

    pub async fn save_album(&self, album_id: &str, data: &(String, String, String, Vec<Track>)) {
        self.album_mem_cache.lock().unwrap().insert(album_id.to_string(), data.clone());

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
            let mut mem_cache = self.playlist_mem_cache.lock().unwrap();
            for (playlist_id, data) in &playlists {
                mem_cache.insert(playlist_id.clone(), data.clone());
            }
            drop(mem_cache);
            for (playlist_id, data) in playlists {
                self.apply_index_change(&playlist_id, &data.3);
            }
        }
    }

    async fn load_playlist_from_disk_any_snapshot(&self, playlist_id: &str) -> Option<(String, String, String, Vec<Track>)> {
        let playlist_id = playlist_id.to_string();
        tokio::task::spawn_blocking(move || {
            let cache_dir = ".cache/playlists";
            if let Ok(entries) = fs::read_dir(cache_dir) {
                for entry in entries.flatten() {
                    if let Ok(file_name) = entry.file_name().into_string() {
                        if file_name.starts_with(&playlist_id) && file_name.ends_with(".json") {
                            if let Ok(content) = fs::read_to_string(entry.path()) {
                                if let Ok(data) = serde_json::from_str::<(String, String, String, Vec<Track>)>(&content) {
                                    return Some(data);
                                }
                            }
                        }
                    }
                }
            }
            None
        }).await.ok()?
    }

    pub async fn add_track_to_cached_playlist(&self, playlist_id: &str, new_snapshot_id: &str, track: Track) {
        let mut data_opt = self.playlist_mem_cache.lock().unwrap().get(playlist_id).cloned();
        
        if data_opt.is_none() {
            data_opt = self.load_playlist_from_disk_any_snapshot(playlist_id).await;
        }

        if let Some(mut data) = data_opt {
            let clean_track_id = track.id.strip_prefix("spotify:track:").unwrap_or(&track.id).to_string();
            if !data.3.iter().any(|t| t.id.strip_prefix("spotify:track:").unwrap_or(&t.id) == clean_track_id) {
                let mut track_clone = track.clone();
                track_clone.id = clean_track_id;
                data.3.push(track_clone);
                self.save_playlist(playlist_id, new_snapshot_id, &data).await;
            }
        }
    }

    pub async fn remove_track_from_cached_playlist(&self, playlist_id: &str, new_snapshot_id: &str, track_id: &str) {
        let mut data_opt = self.playlist_mem_cache.lock().unwrap().get(playlist_id).cloned();
        
        if data_opt.is_none() {
            data_opt = self.load_playlist_from_disk_any_snapshot(playlist_id).await;
        }

        if let Some(mut data) = data_opt {
            let clean_track_id = track_id.strip_prefix("spotify:track:").unwrap_or(track_id);
            let initial_len = data.3.len();
            data.3.retain(|t| t.id.strip_prefix("spotify:track:").unwrap_or(&t.id) != clean_track_id);
            if data.3.len() < initial_len {
                self.save_playlist(playlist_id, new_snapshot_id, &data).await;
            }
        }
    }

    pub async fn update_playlist_snapshot_in_user_playlists(&self, playlist_id: &str, new_snapshot_id: &str, username: Option<&str>) {
        // Mutate in place under the lock and clone exactly once (only when we
        // actually changed something, and only because the disk save needs an
        // owned Vec it can carry to the blocking pool).
        let snapshot_for_disk = {
            let mut guard = self.playlists_cache.lock().unwrap();
            let Some(playlists) = guard.as_mut() else { return; };
            let Some(p) = playlists.iter_mut().find(|p| p.id == playlist_id) else { return; };
            p.duration_str = new_snapshot_id.to_string();
            playlists.clone()
        };
        if let Some(uname) = username {
            self.save_user_playlists_disk(uname, snapshot_for_disk).await;
        }
    }

    pub async fn get_playlists_containing_track(&self, track_id: &str) -> Vec<String> {
        let clean_id = track_id.strip_prefix("spotify:track:").unwrap_or(track_id);
        self.playlist_tracks_index.lock().unwrap().get(clean_id).cloned().unwrap_or_default()
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
