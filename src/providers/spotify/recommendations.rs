use std::sync::Arc;
use serde_json::Value;
use crate::providers::Track;
use super::parser::SpotifyParser;
use super::auth::SpotifyAuthManager;

#[derive(serde::Deserialize)]
struct LastfmSimilarTracksResponse {
    similartracks: LastfmSimilarTracks,
}

#[derive(serde::Deserialize)]
struct LastfmSimilarTracks {
    #[serde(default)]
    track: Vec<LastfmTrack>,
}

#[derive(serde::Deserialize)]
struct LastfmTrack {
    name: String,
    artist: LastfmArtist,
}

#[derive(serde::Deserialize)]
struct LastfmArtist {
    name: String,
}

pub struct SpotifyRecommendationService {
    auth: Arc<SpotifyAuthManager>,
    client: reqwest::Client,
}

impl SpotifyRecommendationService {
    pub fn new(auth: Arc<SpotifyAuthManager>, client: reqwest::Client) -> Self {
        Self { auth, client }
    }

    pub async fn get_recommendations(&self, track_title: &str, artist_name: &str) -> Result<Vec<Track>, String> {
        let api_key = std::env::var("LASTFM_API_KEY")
            .map_err(|_| "LASTFM_API_KEY environment variable not set".to_string())?;

        if api_key.is_empty() {
            return Err("LASTFM_API_KEY is empty".to_string());
        }

        println!("[spotify] Querying Last.fm for similar tracks to '{}' by '{}'...", track_title, artist_name);

        let client = self.client.clone();
        let response = client.get("https://ws.audioscrobbler.com/2.0/")
            .query(&[
                ("method", "track.getsimilar"),
                ("artist", artist_name),
                ("track", track_title),
                ("api_key", &api_key),
                ("format", "json"),
                ("limit", "10"),
                ("autocorrect", "1"),
            ])
            .send()
            .await
            .map_err(|e| format!("Last.fm API request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Last.fm API returned HTTP status {}", response.status()));
        }

        let resp_json: LastfmSimilarTracksResponse = response.json()
            .await
            .map_err(|e| format!("Failed to parse Last.fm response JSON: {}", e))?;

        let lastfm_tracks = resp_json.similartracks.track;
        if lastfm_tracks.is_empty() {
            println!("[spotify] Last.fm returned 0 recommendations.");
            return Ok(vec![]);
        }

        println!("[spotify] Found {} similar tracks on Last.fm. Resolving on Spotify...", lastfm_tracks.len());
        let access_token = self.auth.get_access_token().await?;

        let mut join_set = tokio::task::JoinSet::new();

        for l_track in lastfm_tracks {
            let title = l_track.name;
            let artist = l_track.artist.name;
            let client = self.client.clone();
            let token = access_token.clone();

            join_set.spawn(async move {
                let query = format!("track:\"{}\" artist:\"{}\"", title, artist);

                let response = client
                    .get("https://api.spotify.com/v1/search")
                    .query(&[("q", query.as_str()), ("type", "track"), ("limit", "1")])
                    .header("Authorization", format!("Bearer {}", token))
                    .send()
                    .await
                    .ok()?;

                let json: Value = if response.status().is_success() {
                    response.json().await.ok()?
                } else {
                    // Fallback to simpler query if strict search fails
                    let fb_query = format!("{} {}", title, artist);
                    let fb_resp = client
                        .get("https://api.spotify.com/v1/search")
                        .query(&[("q", fb_query.as_str()), ("type", "track"), ("limit", "1")])
                        .header("Authorization", format!("Bearer {}", token))
                        .send()
                        .await
                        .ok()?;
                    if fb_resp.status().is_success() {
                        fb_resp.json().await.ok()?
                    } else {
                        return None;
                    }
                };

                if let Some(items) = json["tracks"]["items"].as_array() {
                    if let Some(item) = items.first() {
                        return SpotifyParser::parse_track_lenient(item);
                    }
                }
                None
            });
        }

        let mut resolved = Vec::new();
        while let Some(res) = join_set.join_next().await {
            if let Ok(Some(track)) = res {
                resolved.push(track);
            }
        }

        println!("[spotify] Successfully resolved {} recommended tracks on Spotify.", resolved.len());
        Ok(resolved)
    }
}
