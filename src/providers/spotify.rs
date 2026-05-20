use std::sync::Arc;
use super::{AudioProvider, Track};

pub mod parser;
pub mod api;
pub mod cache;
pub mod auth;
pub mod recommendations;

use async_trait::async_trait;
use tokio::sync::Mutex;

pub struct SpotifyProvider {
    pub auth: Arc<auth::SpotifyAuthManager>,
    pub next_url: Mutex<Option<String>>,
    pub cache: cache::SpotifyCache,
    pub api: api::SpotifyApiService,
    pub recommendations: recommendations::SpotifyRecommendationService,
}

impl SpotifyProvider {
    pub fn new() -> Self {
        let client = reqwest::Client::new();
        let auth = Arc::new(auth::SpotifyAuthManager::new());
        let cache = cache::SpotifyCache::new();
        let api = api::SpotifyApiService::new(Arc::clone(&auth), client.clone());
        let recommendations = recommendations::SpotifyRecommendationService::new(Arc::clone(&auth), client);

        Self {
            auth,
            next_url: Mutex::new(None),
            cache,
            api,
            recommendations,
        }
    }

    pub async fn get_cached_username(&self) -> Option<String> {
        let guard = self.auth.active_user.lock().await;
        guard.clone()
    }

    pub async fn get_user_playlists_disk(&self, username: &str) -> Option<Vec<Track>> {
        self.cache.get_user_playlists_disk(username).await
    }

    pub async fn save_user_playlists_disk(&self, username: &str, playlists: Vec<Track>) {
        self.cache.save_user_playlists_disk(username, playlists).await;
    }

    pub async fn get_recently_played_disk(&self, username: &str) -> Option<Vec<Track>> {
        self.cache.get_recently_played_disk(username).await
    }

    pub async fn save_recently_played_disk(&self, username: &str, tracks: Vec<Track>) {
        self.cache.save_recently_played_disk(username, tracks).await;
    }

    pub async fn get_top_artists_disk(&self, username: &str) -> Option<Vec<Track>> {
        self.cache.get_top_artists_disk(username).await
    }

    pub async fn save_top_artists_disk(&self, username: &str, artists: Vec<Track>) {
        self.cache.save_top_artists_disk(username, artists).await;
    }

    pub async fn get_access_token(&self) -> Result<String, String> {
        self.auth.get_access_token().await
    }

    pub async fn load_more(&self) -> Result<Vec<Track>, String> {
        self.api.load_more(&self.next_url).await
    }

    pub async fn get_current_user_name(&self) -> Result<String, String> {
        self.api.get_current_user_name().await
    }

    pub async fn get_current_user_id(&self) -> Result<String, String> {
        self.api.get_current_user_id().await
    }

    pub async fn get_top_artists(&self) -> Result<Vec<Track>, String> {
        let artists = self.api.get_top_artists().await?;
        if let Some(username) = self.get_cached_username().await {
            self.save_top_artists_disk(&username, artists.clone()).await;
        }
        Ok(artists)
    }

    pub async fn get_album_from_track(&self, track_id: &str) -> Result<(String, String, String, Vec<Track>), String> {
        self.api.get_album_from_track(track_id, &self.cache).await
    }

    pub async fn get_track(&self, track_id: &str) -> Result<Track, String> {
        self.api.get_track(track_id, &self.cache).await
    }

    pub async fn get_user_playlists(&self) -> Result<Vec<Track>, String> {
        if let Some(playlists) = self.cache.get_user_playlists().await {
            return Ok(playlists);
        }
        let playlists = self.api.get_user_playlists().await?;
        self.cache.save_user_playlists(playlists.clone()).await;
        if let Some(username) = self.get_cached_username().await {
            self.save_user_playlists_disk(&username, playlists.clone()).await;
        }
        Ok(playlists)
    }
    
    pub async fn get_playlist(&self, playlist_id: &str, snapshot_id: &str) -> Result<(String, String, String, Vec<Track>), String> {
        self.api.get_playlist(playlist_id, snapshot_id, &self.cache).await
    }

    pub async fn get_artist(
        &self,
        artist_id: &str,
    ) -> Result<(String, String, Vec<Track>, Vec<Track>), String> {
        self.api.get_artist(artist_id, &self.cache).await
    }

    pub async fn get_recently_played(&self) -> Result<Vec<Track>, String> {
        let tracks = self.api.get_recently_played().await?;
        if let Some(username) = self.get_cached_username().await {
            self.save_recently_played_disk(&username, tracks.clone()).await;
        }
        Ok(tracks)
    }

    pub async fn get_album_by_id(
        &self,
        album_id: &str,
    ) -> Result<(String, String, String, Vec<Track>), String> {
        self.api.get_album_by_id(album_id, &self.cache).await
    }

    pub async fn get_recommendations(&self, track_title: &str, artist_name: &str) -> Result<Vec<Track>, String> {
        self.recommendations.get_recommendations(track_title, artist_name).await
    }

    pub async fn add_track_to_liked_songs(&self, track_id: &str) -> Result<(), String> {
        self.api.add_track_to_liked_songs(track_id).await
    }

    pub async fn add_track_to_playlist(&self, track_id: &str, playlist_id: &str) -> Result<(), String> {
        self.api.add_track_to_playlist(track_id, playlist_id).await
    }

    pub async fn check_track_liked(&self, track_id: &str) -> Result<bool, String> {
        self.api.check_track_liked(track_id).await
    }

    pub async fn check_playlists_containing_track(&self, track_id: &str) -> Result<Vec<String>, String> {
        Ok(self.cache.get_playlists_containing_track(track_id).await)
    }
}

#[async_trait]
impl AudioProvider for SpotifyProvider {
    async fn search(&self, query: &str) -> Result<Vec<Track>, String> {
        let (tracks, next) = self.api.search(query).await?;
        *self.next_url.lock().await = next;
        Ok(tracks)
    }

    #[allow(dead_code)]
    async fn get_audio_stream(&self, _track_id: &str) -> Result<Vec<u8>, String> {
        Ok(vec![])
    }
}