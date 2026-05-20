use serde_json::Value;
use crate::providers::{ProviderType, Track};

pub struct SpotifyParser;

impl SpotifyParser {
    /// A lenient track parser that handles missing fields
    pub fn parse_track_lenient(v: &Value) -> Option<Track> {
        let id = v["id"].as_str()?.to_string();
        let title = v["name"].as_str().unwrap_or("Unknown").to_string();
        let artists = v["artists"].as_array()?;
        let artist_node = artists.first()?;
        let artist = artist_node["name"].as_str().unwrap_or("Unknown Artist").to_string();
        let artist_id = artist_node["id"].as_str().unwrap_or("").to_string();
        
        let album_url = v["album"]["images"].as_array()
            .and_then(|imgs| imgs.first())
            .and_then(|img| img["url"].as_str())
            .unwrap_or("")
            .to_string();

        let duration_ms = v["duration_ms"].as_u64().unwrap_or(0);
        let seconds = (duration_ms + 500) / 1000;
        
        Some(Track {
            album_url,
            id,
            title,
            artist,
            artist_id,
            source: ProviderType::Spotify,
            sample_rate: 44100,
            bit_depth: 16,
            duration_str: format!("{}:{:02}", seconds / 60, seconds % 60),
        })
    }

    pub fn parse_tracks_from_json(json: Value) -> Result<Vec<Track>, String> {
        let mut tracks = Vec::new();

        if let Some(items) = json["tracks"]["items"].as_array() {
            for item in items {
                let album_url = item["album"]["images"]
                    .as_array()
                    .and_then(|imgs| imgs.get(0))
                    .and_then(|img| img["url"].as_str())
                    .unwrap_or("")
                    .to_string();

                let id = item["id"].as_str().unwrap_or("").to_string();
                let title = item["name"].as_str().unwrap_or("Unknown").to_string();
                let artist = item["artists"][0]["name"]
                    .as_str()
                    .unwrap_or("Unknown Artist")
                    .to_string();

                let duration_ms = item["duration_ms"].as_u64().unwrap_or(0);
                let seconds = duration_ms / 1000;
                let duration_str = format!("{}:{:02}", seconds / 60, seconds % 60);

                tracks.push(Track {
                    album_url,
                    id,
                    title,
                    artist,
                    artist_id: item["artists"][0]["id"].as_str().unwrap_or("").to_string(),
                    source: ProviderType::Spotify,
                    sample_rate: 44100,
                    bit_depth: 16,
                    duration_str
                });
            }
        }
        Ok(tracks)
    }
}