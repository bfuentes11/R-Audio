use std::sync::Arc;
use tokio::sync::Mutex;
use serde_json::Value;
use crate::providers::{ProviderType, Track};
use super::parser::SpotifyParser;
use super::cache::SpotifyCache;
use super::auth::SpotifyAuthManager;
use rspotify::prelude::*;

async fn parse_response_value(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!("HTTP Error {}: {}", status, text));
    }
    response.json().await.map_err(|e| e.to_string())
}

pub struct SpotifyApiService {
    auth: Arc<SpotifyAuthManager>,
    client: reqwest::Client,
}

impl SpotifyApiService {
    pub fn new(auth: Arc<SpotifyAuthManager>, client: reqwest::Client) -> Self {
        Self { auth, client }
    }

    pub async fn load_more(&self, next_url_ref: &Mutex<Option<String>>) -> Result<Vec<Track>, String> {
        let next_url = {
            let guard = next_url_ref.lock().await;
            guard.clone()
        };

        let Some(url) = next_url else {
            return Ok(vec![]); 
        };

        let access_token = self.auth.get_access_token().await?;

        let response = self.client.clone()
            .get(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let json = parse_response_value(response).await?;

        let next_string = json["tracks"]["next"].as_str().map(|s| s.to_string());
        println!("🔗 Saved Next URL for pagination: {:?}", next_string);

        *next_url_ref.lock().await = next_string;

        SpotifyParser::parse_tracks_from_json(json)
    }

    pub async fn get_current_user_name(&self) -> Result<String, String> {
        let client_mutex = self.auth.get_client().await;
        let client = client_mutex.lock().await;
        
        match client.current_user().await {
            Ok(user) => {
                let name = user.display_name.unwrap_or_else(|| "User".to_string());
                Ok(name)
            }
            Err(e) => Err(format!("Failed to get user profile: {}", e)),
        }
    }

    pub async fn get_current_user_id(&self) -> Result<String, String> {
        let client_mutex = self.auth.get_client().await;
        let client = client_mutex.lock().await;

        match client.current_user().await {
            Ok(user) => {
                let id_str = user.id.to_string();
                let stripped_id = id_str.strip_prefix("spotify:user:").unwrap_or(&id_str).to_string();
                Ok(stripped_id)
            }
            Err(e) => Err(format!("Failed to get user profile: {}", e)),
        }
    }

    pub async fn get_top_artists(&self) -> Result<Vec<Track>, String> {
        let access_token = self.auth.get_access_token().await?;

        let response = self.client.clone()
            .get("https://api.spotify.com/v1/me/top/artists")
            .query(&[("limit", "20"), ("time_range", "short_term")])
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let json = parse_response_value(response).await?;

        let mut artists = Vec::new();

        if let Some(items) = json["items"].as_array() {
            for item in items {
                let image_url = item["images"]
                    .as_array()
                    .and_then(|imgs| imgs.get(0))
                    .and_then(|img| img["url"].as_str())
                    .unwrap_or("")
                    .to_string();

                let id = item["id"].as_str().unwrap_or("").to_string();
                let name = item["name"].as_str().unwrap_or("Unknown").to_string();

                artists.push(Track {
                    album_url: image_url,
                    id: id.clone(),
                    title: name,
                    artist: "Artist".to_string(),
                    artist_id: id,
                    duration_str: "".to_string(), 
                    source: ProviderType::Spotify,
                    sample_rate: 44100,
                    bit_depth: 16,
                });
            }
        }

        Ok(artists)
    }

    pub async fn get_album_from_track(&self, track_id: &str, cache: &SpotifyCache) -> Result<(String, String, String, Vec<Track>), String> {
        let access_token = self.auth.get_access_token().await?;

        let track_res = self.client.clone()
            .get(&format!("https://api.spotify.com/v1/tracks/{}", track_id))
            .header("Authorization", format!("Bearer {}", access_token))
            .send().await.map_err(|e| e.to_string())?;
        let track_json: Value = track_res.json().await.map_err(|e| e.to_string())?;

        let album_id = track_json["album"]["id"].as_str().ok_or("No album id found")?;
        self.get_album_by_id(album_id, cache).await
    }

    pub async fn get_track(&self, track_id: &str, cache: &SpotifyCache) -> Result<Track, String> {
        if let Some(track) = cache.get_track(track_id).await {
            return Ok(track);
        }

        let access_token = self.auth.get_access_token().await?;
        let res = self.client.clone()
            .get(&format!("https://api.spotify.com/v1/tracks/{}", track_id))
            .header("Authorization", format!("Bearer {}", access_token))
            .send().await.map_err(|e| e.to_string())?;
        
        let json: Value = res.json().await.map_err(|e| e.to_string())?;
        let track = SpotifyParser::parse_track_lenient(&json).ok_or("Failed to parse track JSON")?;

        cache.insert_track(track_id.to_string(), track.clone()).await;

        Ok(track)
    }

    pub async fn get_user_playlists(&self) -> Result<Vec<Track>, String> {
        let access_token = self.auth.get_access_token().await?;

        // Spotify's /me/playlists endpoint default limit is 20; max is 50.
        // We request the max explicitly so users with 20+ playlists see them
        // all on the dashboard.
        let url = "https://api.spotify.com/v1/me/playlists?limit=50";
        
        let res = self.client.clone()
            .get(url)
            .header("Authorization", format!("Bearer {}", access_token))
            .send().await.map_err(|e| e.to_string())?;

        let json = parse_response_value(res).await?;
        
        let mut results = Vec::new();

        if let Some(items) = json["items"].as_array() {
            // Retrieve current user profile to get our own user ID for ownership check
            let me_res = self.client.clone()
                .get("https://api.spotify.com/v1/me")
                .header("Authorization", format!("Bearer {}", access_token))
                .send().await.map_err(|e| e.to_string())?;
            let me_json = parse_response_value(me_res).await?;
            let my_id = me_json["id"].as_str().unwrap_or("").to_string();

            for item in items {
                let owner_id = item["owner"]["id"].as_str().unwrap_or("").to_string();
                let collaborative = item["collaborative"].as_bool().unwrap_or(false);

                if owner_id == my_id || collaborative {
                    let img = item["images"]
                        .as_array()
                        .and_then(|i| i.get(0))
                        .and_then(|i| i["url"].as_str())
                        .unwrap_or("")
                        .to_string();
                        
                    let name = item["name"].as_str().unwrap_or("Playlist").to_string();
                    let id = item["id"].as_str().unwrap_or("").to_string();
                    let owner = item["owner"]["display_name"].as_str().unwrap_or("Spotify").to_string();
                    
                    let snapshot = item["snapshot_id"].as_str().unwrap_or("").to_string();

                    results.push(Track {
                        album_url: img,
                        id,
                        title: name,
                        artist: owner,
                        artist_id: "".to_string(),
                        source: ProviderType::Spotify,
                        sample_rate: 44100,
                        bit_depth: 16,
                        duration_str: snapshot, 
                    });
                }
            }
        }
        
        Ok(results)
    }

    pub async fn get_playlist(&self, playlist_id: &str, snapshot_id: &str, cache: &SpotifyCache) -> Result<(String, String, String, Vec<Track>), String> {
        // Self-heal: an empty cached track list is almost certainly a stale
        // write from a prior broken fetch. Drop it and refetch live.
        // Healthy non-empty caches are still served immediately.
        if let Some(cached_data) = cache.get_playlist(playlist_id, snapshot_id).await {
            if !cached_data.3.is_empty() {
                return Ok(cached_data);
            }
        }

        let access_token = self.auth.get_access_token().await?;

        // ── 1. Metadata only ──────────────────────────────────────────────
        // GET /playlists/{id} with `fields=` shaves the response down to just
        // name/owner/cover/total. We intentionally do NOT read the embedded
        // first-page tracks block from this response — it has been the
        // source of the "playlist appears empty" bug because its shape
        // sometimes diverges from /items. All tracks come from step 2.
        // ── 1. Metadata only ──────────────────────────────────────────────
        // Full /playlists/{id} response (no fields=… filter — its syntax
        // for nested fields is fragile and was zero'ing out tracks.total).
        // We only read name/owner/images here and intentionally IGNORE the
        // embedded tracks block; the actual track list comes from /items.
        let meta_url = format!("https://api.spotify.com/v1/playlists/{}", playlist_id);
        println!("[api] GET {}", meta_url);
        let meta_res = self.client.clone()
            .get(&meta_url)
            .header("Authorization", format!("Bearer {}", access_token))
            .send().await.map_err(|e| e.to_string())?;
        let meta_status = meta_res.status();
        let meta_body = meta_res.text().await.map_err(|e| e.to_string())?;
        if !meta_status.is_success() {
            eprintln!("[api] playlist metadata failed: {} body={}", meta_status,
                meta_body.chars().take(300).collect::<String>());
            return Err(format!("Spotify {} for playlist meta {}: {}",
                meta_status, playlist_id,
                meta_body.chars().take(300).collect::<String>()));
        }
        let meta_json: Value = serde_json::from_str(&meta_body)
            .map_err(|e| format!("playlist meta JSON parse failed: {}", e))?;

        let playlist_name = meta_json["name"].as_str().unwrap_or("Selected Playlist").to_string();
        let owner_name = meta_json["owner"]["display_name"].as_str().unwrap_or("Spotify").to_string();
        let cover_url = meta_json["images"]
            .as_array()
            .and_then(|imgs| imgs.get(0))
            .and_then(|img| img["url"].as_str())
            .unwrap_or("")
            .to_string();
        println!("[api] playlist '{}' (id={}, owner={})",
            playlist_name, playlist_id, owner_name);

        // ── 2. First page of items + true total ───────────────────────────
        // Always hit /items?offset=0 unconditionally — that response gives
        // us BOTH the first page AND the authoritative `total`. We don't
        // trust whatever the metadata fetch said about track count: it was
        // returning 0 even for non-empty playlists.
        let mut tracks = Vec::new();
        let first_page_url = format!(
            "https://api.spotify.com/v1/playlists/{}/items?offset=0&limit=50",
            playlist_id
        );
        println!("[api] GET {}", first_page_url);
        let first_res = self.client.clone()
            .get(&first_page_url)
            .header("Authorization", format!("Bearer {}", access_token))
            .send().await.map_err(|e| e.to_string())?;
        let first_status = first_res.status();
        let first_body = first_res.text().await.map_err(|e| e.to_string())?;
        if !first_status.is_success() {
            eprintln!("[api] /items page 0 failed: {} body={}", first_status,
                first_body.chars().take(300).collect::<String>());
            let final_data = (playlist_name, owner_name, cover_url, tracks);
            cache.save_playlist(playlist_id, snapshot_id, &final_data).await;
            return Ok(final_data);
        }
        let first_json: Value = serde_json::from_str(&first_body)
            .map_err(|e| format!("/items JSON parse failed: {}", e))?;

        let total_tracks = first_json["total"].as_u64().unwrap_or(0);
        let limit_hint = first_json["limit"].as_u64().unwrap_or(50);
        let items_per_page = if limit_hint == 0 { 50 } else { limit_hint };
        let first_count = first_json["items"].as_array().map(|a| a.len()).unwrap_or(0);
        println!("[api] /items page 0: {} items, total={}, limit={}",
            first_count, total_tracks, items_per_page);

        // Helper: parse one page's items into our tracks vec.
        let parse_items = |page_json: &Value, tracks: &mut Vec<Track>| {
            if let Some(items) = page_json["items"].as_array() {
                for item_node in items {
                    let t_node = if !item_node["track"].is_null() {
                        &item_node["track"]
                    } else if !item_node["item"].is_null() {
                        &item_node["item"]
                    } else {
                        item_node
                    };
                    if t_node.is_null() || t_node["id"].is_null() { continue; }

                    if let Some(mut track) = SpotifyParser::parse_track_lenient(t_node) {
                        if track.album_url.is_empty() {
                            track.album_url = cover_url.clone();
                        }
                        tracks.push(track);
                    }
                }
            }
        };
        parse_items(&first_json, &mut tracks);

        if total_tracks > items_per_page {
            let num_pages = (total_tracks - 1) / items_per_page + 1;
            let mut join_set = tokio::task::JoinSet::new();

            for page_idx in 1..num_pages {
                let offset = page_idx * items_per_page;
                let url = format!(
                    "https://api.spotify.com/v1/playlists/{}/items?offset={}&limit={}",
                    playlist_id, offset, items_per_page
                );
                let client = self.client.clone();
                let token = access_token.clone();

                join_set.spawn(async move {
                    let res = client
                        .get(&url)
                        .header("Authorization", format!("Bearer {}", token))
                        .send()
                        .await
                        .ok()?;
                    let page_json = parse_response_value(res).await.ok()?;
                    Some((page_idx, page_json))
                });
            }

            let mut pages = Vec::new();
            while let Some(res) = join_set.join_next().await {
                if let Ok(Some((page_idx, page_json))) = res {
                    pages.push((page_idx, page_json));
                }
            }
            pages.sort_by_key(|(idx, _)| *idx);

            for (_, page_json) in pages {
                parse_items(&page_json, &mut tracks);
            }
        }

        println!("[api] Total tracks loaded in parallel: {}", tracks.len());

        let final_data = (playlist_name, owner_name, cover_url, tracks);
        cache.save_playlist(playlist_id, snapshot_id, &final_data).await;

        Ok(final_data)
    }

    pub async fn get_artist(&self, artist_id: &str, cache: &SpotifyCache) -> Result<(String, String, Vec<Track>, Vec<Track>), String> {
        if let Some(data) = cache.get_artist(artist_id).await {
            return Ok(data);
        }

        let access_token = self.auth.get_access_token().await?;

        let http = self.client.clone();

        let artist_url = format!("https://api.spotify.com/v1/artists/{}", artist_id);
        let albums_url = format!("https://api.spotify.com/v1/artists/{}/albums?include_groups=album%2Csingle&limit=10&market=US", artist_id);

        let (artist_res, albums_res) = tokio::join!(
            http.get(&artist_url).header("Authorization", format!("Bearer {}", access_token)).send(),
            http.get(&albums_url).header("Authorization", format!("Bearer {}", access_token)).send(),
        );

        let artist_json: Value = artist_res.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
        let albums_json: Value = albums_res.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;

        if let Some(err) = artist_json["error"].as_object() {
            return Err(format!("Spotify API error: {:?}", err));
        }

        let name = artist_json["name"].as_str().unwrap_or("Unknown Artist").to_string();
        let image_url = artist_json["images"].as_array()
            .and_then(|a| a.first())
            .and_then(|img| img["url"].as_str())
            .unwrap_or("").to_string();

        let search_url = format!(
            "https://api.spotify.com/v1/search?q=artist%3A{}&type=track&limit=10&market=US",
            urlencoding::encode(&name)
        );
        let search_json: Value = http.get(&search_url)
            .header("Authorization", format!("Bearer {}", access_token))
            .send().await.map_err(|e| e.to_string())?
            .json().await.map_err(|e| e.to_string())?;

        let mut top_tracks = Vec::new();
        if let Some(items) = search_json["tracks"]["items"].as_array() {
            for item in items {
                if item["id"].is_null() { continue; }
                let t_cover = item["album"]["images"].as_array()
                    .and_then(|a| a.first())
                    .and_then(|img| img["url"].as_str())
                    .unwrap_or("").to_string();
                let dur_ms = item["duration_ms"].as_u64().unwrap_or(0);
                let secs = dur_ms / 1000;
                top_tracks.push(Track {
                    album_url: t_cover,
                    id: item["id"].as_str().unwrap_or("").to_string(),
                    title: item["name"].as_str().unwrap_or("Unknown").to_string(),
                    artist: item["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                    artist_id: item["artists"][0]["id"].as_str().unwrap_or("").to_string(),
                    source: ProviderType::Spotify,
                    sample_rate: 44100,
                    bit_depth: 16,
                    duration_str: format!("{}:{:02}", secs / 60, secs % 60),
                });
            }
        }

        let mut albums = Vec::new();
        if let Some(items) = albums_json["items"].as_array() {
            let mut sorted_items = items.to_vec();
            sorted_items.sort_by(|a, b| {
                let date_a = a["release_date"].as_str().unwrap_or("");
                let date_b = b["release_date"].as_str().unwrap_or("");
                date_b.cmp(date_a)
            });
            for item in sorted_items {
                if item["id"].is_null() { continue; }
                let cover = item["images"].as_array()
                    .and_then(|a| a.first())
                    .and_then(|img| img["url"].as_str())
                    .unwrap_or("").to_string();
                let year = item["release_date"].as_str()
                    .and_then(|d| d.split('-').next())
                    .unwrap_or("").to_string();
                albums.push(Track {
                    album_url: cover,
                    id: item["id"].as_str().unwrap_or("").to_string(),
                    title: item["name"].as_str().unwrap_or("Unknown").to_string(),
                    artist: year,
                    artist_id: "".to_string(),
                    source: ProviderType::Spotify,
                    sample_rate: 44100,
                    bit_depth: 16,
                    duration_str: item["album_type"].as_str().unwrap_or("album").to_string(),
                });
            }
        }

        let result = (name, image_url, top_tracks, albums);
        cache.save_artist(artist_id, &result).await;

        Ok(result)
    }

    pub async fn get_recently_played(&self) -> Result<Vec<Track>, String> {
        let access_token = self.auth.get_access_token().await?;
        
        let response = self.client.clone()
            .get("https://api.spotify.com/v1/me/player/recently-played?limit=20")
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let json = parse_response_value(response).await?;
        let mut tracks = Vec::new();
        let mut seen = std::collections::HashSet::new();

        if let Some(items) = json["items"].as_array() {
            for item in items {
                if let Some(track) = SpotifyParser::parse_track_lenient(&item["track"]) {
                    if seen.insert(track.id.clone()) {
                        tracks.push(track);
                    }
                }
            }
        }

        tracks.truncate(10);
        Ok(tracks)
    }

    pub async fn get_album_by_id(&self, album_id: &str, cache: &SpotifyCache) -> Result<(String, String, String, Vec<Track>), String> {
        if let Some(cached_data) = cache.get_album(album_id).await {
            return Ok(cached_data);
        }

        let access_token = self.auth.get_access_token().await?;

        let http = self.client.clone();
        let meta_url   = format!("https://api.spotify.com/v1/albums/{}", album_id);
        let tracks_url = format!("https://api.spotify.com/v1/albums/{}/tracks?limit=50", album_id);

        let (meta_res, tracks_res) = tokio::join!(
            http.get(&meta_url).header("Authorization", format!("Bearer {}", access_token)).send(),
            http.get(&tracks_url).header("Authorization", format!("Bearer {}", access_token)).send(),
        );

        let meta_json = parse_response_value(meta_res.map_err(|e| e.to_string())?).await?;
        let tracks_json = parse_response_value(tracks_res.map_err(|e| e.to_string())?).await?;

        let album_name  = meta_json["name"].as_str().unwrap_or("Unknown Album").to_string();
        let artist_name = meta_json["artists"][0]["name"].as_str().unwrap_or("Unknown").to_string();
        let cover_url   = meta_json["images"].as_array()
            .and_then(|a| a.first())
            .and_then(|img| img["url"].as_str())
            .unwrap_or("").to_string();

        let mut tracks = Vec::new();
        if let Some(items) = tracks_json["items"].as_array() {
            for item in items {
                if item["id"].is_null() { continue; }
                let dur_ms = item["duration_ms"].as_u64().unwrap_or(0);
                let secs   = dur_ms / 1000;
                tracks.push(Track {
                    album_url: cover_url.clone(),
                    id:        item["id"].as_str().unwrap_or("").to_string(),
                    title:     item["name"].as_str().unwrap_or("Unknown").to_string(),
                    artist:    item["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                    artist_id: item["artists"][0]["id"].as_str().unwrap_or("").to_string(),
                    source:    ProviderType::Spotify,
                    sample_rate: 44100,
                    bit_depth:   16,
                    duration_str: format!("{}:{:02}", secs / 60, secs % 60),
                });
            }
        }

        let result = (album_name, artist_name, cover_url, tracks);
        cache.save_album(album_id, &result).await;
        Ok(result)
    }

    pub async fn search(&self, query: &str) -> Result<(Vec<Track>, Option<String>), String> {
        let access_token = self.auth.get_access_token().await?;

        let response = self.client.clone()
            .get("https://api.spotify.com/v1/search")
            .query(&[("q", query), ("type", "track"), ("limit", "10")])
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let json = parse_response_value(response).await?;
        let mut tracks = Vec::new();

        let next_url = json["tracks"]["next"].as_str().map(|s| s.to_string());

        if let Some(items) = json["tracks"]["items"].as_array() {
            for item in items {
                if let Some(track) = SpotifyParser::parse_track_lenient(item) {
                    tracks.push(track);
                }
            }
        }

        Ok((tracks, next_url))
    }

    pub async fn add_track_to_liked_songs(&self, track_id: &str) -> Result<(), String> {
        let access_token = self.auth.get_access_token().await?;
        let track_uri = format!("spotify:track:{}", track_id);
        println!("[api] PUT /me/library uri={}", track_uri);

        let response = self.client.clone()
            .put("https://api.spotify.com/v1/me/library")
            .query(&[("uris", &track_uri)])
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            eprintln!("[api] add to library failed: {} body={}", status, text);
            return Err(format!("Failed to add to Liked Songs ({}): {}", status, text));
        }
        println!("[api] add to library OK ({})", status);
        Ok(())
    }

    pub async fn add_track_to_playlist(&self, track_id: &str, playlist_id: &str) -> Result<String, String> {
        let access_token = self.auth.get_access_token().await?;
        let url = format!("https://api.spotify.com/v1/playlists/{}/items", playlist_id);
        let track_uri = format!("spotify:track:{}", track_id);
        println!("[api] POST {} body uris=[{}]", url, track_uri);

        let response = self.client.clone()
            .post(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .json(&serde_json::json!({
                "uris": [track_uri]
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            eprintln!("[api] add to playlist failed: {} body={}", status, text);
            return Err(format!("Failed to add to playlist ({}): {}", status, text));
        }
        println!("[api] add to playlist OK ({})", status);

        let json: Value = response.json().await.map_err(|e| e.to_string())?;
        let snapshot_id = json["snapshot_id"].as_str()
            .ok_or_else(|| "No snapshot_id returned from Spotify API".to_string())?
            .to_string();

        Ok(snapshot_id)
    }

    pub async fn remove_track_from_liked_songs(&self, track_id: &str) -> Result<(), String> {
        let access_token = self.auth.get_access_token().await?;
        // /me/library expects `uris` with full Spotify URIs ("spotify:track:ID"),
        // NOT the classic /me/tracks shape which used `ids` with raw track IDs.
        let track_uri = format!("spotify:track:{}", track_id);
        println!("[api] DELETE /me/library uri={}", track_uri);

        let response = self.client.clone()
            .delete("https://api.spotify.com/v1/me/library")
            .query(&[("uris", &track_uri)])
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            eprintln!("[api] remove from library failed: {} body={}", status, text);
            return Err(format!("Failed to remove from Liked Songs ({}): {}", status, text));
        }
        println!("[api] remove from library OK ({})", status);
        Ok(())
    }

    pub async fn remove_track_from_playlist(&self, track_id: &str, playlist_id: &str) -> Result<String, String> {
        let access_token = self.auth.get_access_token().await?;
        let url = format!("https://api.spotify.com/v1/playlists/{}/items", playlist_id);
        let track_uri = format!("spotify:track:{}", track_id);
        println!("[api] DELETE {} body items=[{}]", url, track_uri);

        let response = self.client.clone()
            .delete(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .json(&serde_json::json!({
                "items": [{ "uri": track_uri }]
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            eprintln!("[api] remove from playlist failed: {} body={}", status, text);
            return Err(format!("Failed to remove from playlist ({}): {}", status, text));
        }
        println!("[api] remove from playlist OK ({})", status);

        let json: Value = response.json().await.map_err(|e| e.to_string())?;
        let snapshot_id = json["snapshot_id"].as_str()
            .ok_or_else(|| "No snapshot_id returned from Spotify API".to_string())?
            .to_string();

        Ok(snapshot_id)
    }

    pub async fn check_track_liked(&self, track_id: &str) -> Result<bool, String> {
        let access_token = self.auth.get_access_token().await?;
        // /me/library/contains expects `uris` with full Spotify URIs
        // ("spotify:track:ID"), NOT the classic /me/tracks/contains shape
        // which used `ids` with raw track IDs. Response is still a JSON
        // array of booleans in request order.
        let track_uri = format!("spotify:track:{}", track_id);
        println!("[api] GET /me/library/contains uri={}", track_uri);
        let response = self.client.clone()
            .get("https://api.spotify.com/v1/me/library/contains")
            .query(&[("uris", &track_uri)])
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = response.status();
        if !status.is_success() {
            let status_val = status;
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to check liked track ({}): {}", status_val, text));
        }

        let result: Vec<bool> = response.json().await.map_err(|e| e.to_string())?;
        Ok(result.first().copied().unwrap_or(false))
    }
}
