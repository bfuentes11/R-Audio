use rspotify::{prelude::*, AuthCodePkceSpotify, Credentials, OAuth, Config};
use tokio::sync::Mutex;
use std::sync::Arc;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UserProfile {
    pub token: rspotify::Token,
    pub image_url: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MultiUserTokenCache {
    pub active_user: Option<String>,
    pub users: std::collections::HashMap<String, UserProfile>,
}

pub struct SpotifyAuthManager {
    client: Mutex<AuthCodePkceSpotify>,
    pub active_user: Arc<Mutex<Option<String>>>,
}

#[allow(dead_code)]
impl SpotifyAuthManager {
    pub fn new() -> Self {
        let creds = Credentials::from_env()
            .expect("Missing RSPOTIFY_CLIENT_ID / RSPOTIFY_CLIENT_SECRET");

        let oauth = OAuth::from_env(rspotify::scopes!(
            "user-read-playback-state",
            "user-modify-playback-state",
            "streaming",
            "user-read-recently-played",
            "playlist-read-private",
            "user-top-read"
        )).expect("Missing RSPOTIFY_REDIRECT_URI");

        // token_cached: false since we manually serialize and manage multiple users in a single file
        let config = Config {
            token_cached: false,
            token_refreshing: true,
            cache_path: std::path::PathBuf::from(".spotify_token_cache.json"),
            ..Default::default()
        };

        Self {
            client: AuthCodePkceSpotify::with_config(creds, oauth, config).into(),
            active_user: Arc::new(Mutex::new(None)),
        }
    }

    pub fn load_multi_cache() -> MultiUserTokenCache {
        let cache_path = std::path::PathBuf::from(".spotify_token_cache.json");
        if !cache_path.exists() {
            return MultiUserTokenCache::default();
        }
        if let Ok(content) = std::fs::read_to_string(&cache_path) {
            // Try parsing as the MultiUserTokenCache
            if let Ok(cache) = serde_json::from_str::<MultiUserTokenCache>(&content) {
                return cache;
            }
            // Fallback: Migrate legacy single token cache
            if let Ok(token) = serde_json::from_str::<rspotify::Token>(&content) {
                println!("[auth] Migrating legacy single token cache to multi-user format...");
                let mut cache = MultiUserTokenCache::default();
                let default_name = "Spotify User".to_string();
                cache.active_user = Some(default_name.clone());
                cache.users.insert(default_name, UserProfile {
                    token,
                    image_url: None,
                });
                Self::save_multi_cache(&cache);
                return cache;
            }
        }
        MultiUserTokenCache::default()
    }

    pub fn save_multi_cache(cache: &MultiUserTokenCache) {
        let cache_path = std::path::PathBuf::from(".spotify_token_cache.json");
        if let Ok(pretty_json) = serde_json::to_string_pretty(cache) {
            if let Err(e) = std::fs::write(&cache_path, pretty_json) {
                println!("[auth] Warning: Failed to write multi-user cache to disk: {}", e);
            }
        }
    }

    /// Verifies if a cached session exists and is successfully refreshed
    pub async fn is_session_valid(&self) -> bool {
        let cache = Self::load_multi_cache();
        let active = match &cache.active_user {
            Some(u) => u.clone(),
            None => {
                println!("[auth] No active user specified in cache.");
                return false;
            }
        };

        let profile = match cache.users.get(&active) {
            Some(p) => p.clone(),
            None => {
                println!("[auth] Active user '{}' has no token cached.", active);
                return false;
            }
        };

        let client = self.client.lock().await;

        // Set the token in-memory
        let token_arc = client.get_token();
        if let Ok(mut token_guard) = token_arc.lock().await {
            *token_guard = Some(profile.token.clone());
        }

        println!("[auth] Verifying cached session for user: {}", active);
        if client.refresh_token().await.is_ok() {
            println!("[auth] Session verified successfully!");
            
            // Try fetching the live user profile info to keep display name and avatar fresh
            let mut current_name = active.clone();
            let mut image_url = None;
            if let Ok(user) = client.current_user().await {
                let name = user.display_name.unwrap_or_else(|| "Spotify User".to_string());
                current_name = name;
                image_url = user.images.as_ref().and_then(|imgs| imgs.first()).map(|img| img.url.clone());
            }

            // Read back the possibly refreshed token and persist it
            if let Ok(token_guard) = token_arc.lock().await {
                if let Some(ref_token) = &*token_guard {
                    let mut cache = Self::load_multi_cache();
                    
                    // Remove the old entry if the username changed (e.g. from "Spotify User" to their actual name)
                    if current_name != active {
                        println!("[auth] Migrating cache key from '{}' to actual name '{}'", active, current_name);
                        if let Some(mut profile) = cache.users.remove(&active) {
                            profile.token = ref_token.clone();
                            profile.image_url = image_url;
                            cache.users.insert(current_name.clone(), profile);
                            cache.active_user = Some(current_name.clone());
                        }
                    } else if let Some(user_profile) = cache.users.get_mut(&active) {
                        user_profile.token = ref_token.clone();
                        user_profile.image_url = image_url;
                    }
                    
                    Self::save_multi_cache(&cache);
                }
            }
            *self.active_user.lock().await = Some(current_name);
            true
        } else {
            println!("[auth] Session verification/refresh failed for: {}", active);
            false
        }
    }

    /// Generates the official Spotify Authorization URL
    pub async fn get_auth_url(&self) -> Result<String, String> {
        let mut client = self.client.lock().await;
        client.get_authorize_url(None).map_err(|e| e.to_string())
    }

    /// Exchanges the callback code for the persistent OAuth token
    pub async fn complete_auth(&self, _code: &str) -> Result<(), String> {
        // Token exchange is handled by the Cloudflare Worker (PKCE flow).
        // This method is unused in the QR-code pairing flow.
        Ok(())
    }

    pub async fn get_access_token(&self) -> Result<String, String> {
        let client = self.client.lock().await;
        
        let token_arc = client.get_token();
        let is_expired = {
            let token_guard = token_arc.lock().await
                .map_err(|_| "Failed to lock token".to_string())?;
            match &*token_guard {
                Some(token) => token.is_expired(),
                None => true,
            }
        };

        // Only trigger an actual API refresh request if the token is expired
        if is_expired {
            let _ = client.refresh_token().await;
        }

        let token_guard = token_arc.lock().await
            .map_err(|_| "Failed to lock token".to_string())?;
        match &*token_guard {
            Some(token) => {
                // Self-healing: if the token was refreshed, save it to the cache file
                let active_opt = self.active_user.lock().await.clone();
                if let Some(active) = active_opt {
                    let mut cache = Self::load_multi_cache();
                    if let Some(user_profile) = cache.users.get_mut(&active) {
                        if user_profile.token.access_token != token.access_token {
                            user_profile.token = token.clone();
                            Self::save_multi_cache(&cache);
                            println!("[auth] Saved auto-refreshed token to cache file for '{}'", active);
                        }
                    }
                }
                Ok(token.access_token.clone())
            }
            None => Err("No access token available".to_string()),
        }
    }

    pub async fn get_client(&self) -> &Mutex<AuthCodePkceSpotify> {
        &self.client
    }
}

/// Helper to start the remote pairing process and stream via SSE from the Cloudflare Worker
pub fn start_remote_pairing(
    auth_manager: Arc<SpotifyAuthManager>,
    session_id: String,
) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let app_secret = std::env::var("SPOTIFY_APP_SECRET")
            .or_else(|_| std::env::var("APP_SECRET"))
            .unwrap_or_else(|_| "secret".to_string());

        let wait_url = format!("https://raudio.bryantfuentes.com/wait?session={}", session_id);
        let client = reqwest::Client::new();

        println!("[auth] Connecting to SSE wait channel for session: {}", session_id);

        let mut res = match client.get(&wait_url)
            .header("X-App-Secret", &app_secret)
            .send()
            .await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(format!("SSE request failed: {}", e)));
                    return;
                }
            };

        if !res.status().is_success() {
            let status = res.status();
            let body_text = res.text().await.unwrap_or_default();
            let _ = tx.send(Err(format!("SSE connection failed with status {}: {}", status, body_text)));
            return;
        }

        println!("[auth] SSE connection established. Waiting for token push...");

        let mut buffer = Vec::new();

        loop {
            match res.chunk().await {
                Ok(Some(chunk)) => {
                    buffer.extend_from_slice(&chunk);

                    // Process lines in the buffer
                    while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                        let line_bytes = buffer.drain(..=pos).collect::<Vec<u8>>();
                        let line = String::from_utf8_lossy(&line_bytes);
                        let trimmed = line.trim();

                        if trimmed.starts_with("data:") {
                            let data_json = trimmed["data:".len()..].trim();
                            println!("[auth] Received SSE token event!");

                            match serde_json::from_str::<serde_json::Value>(data_json) {
                                Ok(mut json_val) => {
                                    // If expires_at is not present, calculate and insert it
                                    if json_val.get("expires_at").is_none() {
                                        let expires_in_secs = json_val
                                            .get("expires_in")
                                            .and_then(|v| v.as_i64())
                                            .unwrap_or(3600);
                                        
                                        let now = chrono::Utc::now();
                                        let expires_at = now + chrono::Duration::try_seconds(expires_in_secs).unwrap_or_else(|| chrono::Duration::seconds(3600));
                                        json_val["expires_at"] = serde_json::Value::String(expires_at.to_rfc3339());
                                        println!("[auth] Calculated and injected expires_at: {}", expires_at.to_rfc3339());
                                    }

                                    match serde_json::from_value::<rspotify::Token>(json_val) {
                                        Ok(token) => {
                                            println!("[auth] Remote pairing token constructed successfully!");

                                            // Set token in rspotify client
                                            let rspotify_client = auth_manager.get_client().await;
                                            let token_arc = rspotify_client.lock().await.get_token();
                                            if let Ok(mut token_guard) = token_arc.lock().await {
                                                *token_guard = Some(token.clone());
                                            }

                                            // Fetch current user profile name & avatar URL from Spotify API
                                            let client_locked = rspotify_client.lock().await;
                                            let mut display_name = "Spotify User".to_string();
                                            let mut image_url = None;
                                            if let Ok(user) = client_locked.current_user().await {
                                                display_name = user.display_name.unwrap_or_else(|| "Spotify User".to_string());
                                                image_url = user.images.as_ref().and_then(|imgs| imgs.first()).map(|img| img.url.clone());
                                                println!("[auth] Fetched pairing user profile: name='{}', avatar='{:?}'", display_name, image_url);
                                            } else {
                                                println!("[auth] Warning: Failed to query current user profile after pairing.");
                                            }
                                            drop(client_locked);

                                            // Update multi-user token cache
                                            let mut cache = SpotifyAuthManager::load_multi_cache();
                                            cache.active_user = Some(display_name.clone());
                                            cache.users.insert(display_name.clone(), UserProfile {
                                                token,
                                                image_url,
                                            });
                                            SpotifyAuthManager::save_multi_cache(&cache);

                                            // Set in-memory active user name
                                            *auth_manager.active_user.lock().await = Some(display_name);

                                            let _ = tx.send(Ok(()));
                                            return;
                                        }
                                        Err(e) => {
                                            println!("[auth] Error deserializing raw token to rspotify::Token: {}", e);
                                            println!("[auth] Raw data was: {}", data_json);
                                            let _ = tx.send(Err(format!("Token deserialization failed: {}", e)));
                                            return;
                                        }
                                    }
                                }
                                Err(e) => {
                                    println!("[auth] Error parsing token body as JSON: {}", e);
                                    println!("[auth] Raw data was: {}", data_json);
                                    let _ = tx.send(Err(format!("JSON parsing failed: {}", e)));
                                    return;
                                }
                            }
                        }
                    }
                }
                Ok(None) => {
                    println!("[auth] SSE connection closed by server.");
                    let _ = tx.send(Err("SSE connection closed by server without delivering token.".to_string()));
                    break;
                }
                Err(e) => {
                    println!("[auth] Error reading SSE chunk: {}", e);
                    let _ = tx.send(Err(format!("SSE chunk streaming error: {}", e)));
                    break;
                }
            }
        }
    });

    rx
}
