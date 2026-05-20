use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProviderType {
    Spotify,
    YoutubeMusic,
    LocalMusic
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub album_url: String,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: String,
    pub source: ProviderType,
    pub sample_rate: u32,
    pub bit_depth: u8,
    pub duration_str: String,
}

#[async_trait::async_trait]
pub trait AudioProvider {
    async fn search(&self, query: &str) -> Result<Vec<Track>, String>;
    #[allow(dead_code)]
    async fn get_audio_stream(&self, track_id: &str) -> Result<Vec<u8>, String>;
}

pub mod spotify;
pub mod spotify_player;