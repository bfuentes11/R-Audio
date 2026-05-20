use librespot::core::authentication::Credentials;
use librespot::core::config::SessionConfig;
use librespot::core::session::Session;
use librespot::core::spotify_id::SpotifyId;
use librespot::core::spotify_uri::SpotifyUri;
use librespot::playback::audio_backend;
use librespot::playback::config::{AudioFormat, PlayerConfig};
use librespot::playback::mixer::{Mixer, VolumeGetter, softmixer::SoftMixer};
use librespot::playback::player::{Player, PlayerEvent};
use std::sync::Arc;
use tokio::sync::Mutex;

/// A simple wrapper that allows us to keep a reference to the mixer
/// after it has been passed into the librespot Player.
struct SharedMixer {
    mixer: Arc<Mutex<SoftMixer>>,
}

impl VolumeGetter for SharedMixer {
    fn attenuation_factor(&self) -> f64 {
        if let Ok(m) = self.mixer.try_lock() {
            // Manual attenuation factor calculation
            // Spotify usually uses a cubic or quadratic scale for volume.
            let vol = Mixer::volume(&*m);
            let ratio = vol as f64 / 65535.0;
            ratio * ratio // Quadratic scaling for more natural volume control
        } else {
            1.0
        }
    }
}

/// Events emitted by the player background task to the main thread.
#[derive(Clone, Debug)]
pub enum PlaybackEvent {
    Playing {
        position_ms: u32,
        duration_ms: u32,
    },
    Paused {
        position_ms: u32,
        duration_ms: u32,
    },
    Stopped,
    EndOfTrack,
    /// Fired roughly every 250ms while playing so the UI progress bar stays live.
    Progress {
        position_ms: u32,
        duration_ms: u32,
    },
}

pub struct LibrespotPlayer {
    pub username: String,
    player: Arc<Player>,
    /// The last known total duration of the currently loaded track (ms).
    pub duration_ms: Arc<Mutex<u32>>,
    /// Whether the player is currently playing.
    pub is_playing: Arc<Mutex<bool>>,
    /// Handle to the mixer for volume control
    mixer: Arc<Mutex<SoftMixer>>,
}

impl LibrespotPlayer {
    /// Authenticate with an OAuth access token (Spotify shut down password auth in 2023).
    /// Returns `(player, event_rx)` — the receiver delivers `PlaybackEvent`s to the caller.
    pub async fn new(
        username: &str,
        access_token: &str,
    ) -> Result<(Self, tokio::sync::mpsc::UnboundedReceiver<PlaybackEvent>), String> {
        let session_config = SessionConfig::default();
        let credentials = Credentials::with_access_token(access_token);

        println!(
            "[librespot] Connecting for '{}' with client_id '{}'...",
            username, session_config.client_id
        );

        let cache_dir = std::path::PathBuf::from(".spotify_cache");
        if !cache_dir.exists() {
            let _ = std::fs::create_dir_all(&cache_dir);
        }

        // No credentials cache — we always supply a fresh access token,
        // and stale cached credentials from older sessions would break auth.
        let cache = librespot::core::cache::Cache::new(
            Some(cache_dir.clone()),
            None,
            Some(cache_dir.join("volume")),
            Some(1024 * 1024 * 1024),
        )
        .map_err(|e| format!("Failed to create cache: {}", e))?;

        let session = Session::new(session_config, Some(cache));
        session
            .connect(credentials, false)
            .await
            .map_err(|e| format!("Librespot session failed: {}", e))?;

        println!("[librespot] Session established.");

        let player_config = PlayerConfig::default();
        let audio_format = AudioFormat::default();
        let backend = audio_backend::find(None).ok_or("No audio backend found")?;

        // Use SoftMixer wrapped in our SharedMixer to maintain a handle to it.
        let inner_mixer =
            SoftMixer::open(Default::default()).map_err(|e| format!("Mixer failed: {}", e))?;
        let mixer_arc = Arc::new(Mutex::new(inner_mixer));
        let volume_ctrl = Box::new(SharedMixer {
            mixer: Arc::clone(&mixer_arc),
        });

        let player = Player::new(player_config, session, volume_ctrl, move || {
            backend(None, audio_format)
        });
        let mut raw_channel = player.get_player_event_channel();

        let player_arc = player;
        let duration_ms = Arc::new(Mutex::new(0u32));
        let is_playing = Arc::new(Mutex::new(false));
        let position_ms = Arc::new(Mutex::new(0u32));

        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<PlaybackEvent>();

        // Clone arcs for the background task
        let dur_bg = Arc::clone(&duration_ms);
        let play_bg = Arc::clone(&is_playing);
        let pos_bg = Arc::clone(&position_ms);
        let tx_bg = event_tx.clone();

        // Background task: relay raw PlayerEvents and drive a 250ms progress ticker
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(250));
            loop {
                tokio::select! {
                    // Process librespot events
                    event = raw_channel.recv() => {
                        match event {
                            Some(PlayerEvent::Playing { position_ms, .. }) => {
                                println!("[player] Playing: {}ms", position_ms);
                                *pos_bg.lock().await  = position_ms;
                                *play_bg.lock().await = true;
                                let dur = *dur_bg.lock().await;
                                let _ = tx_bg.send(PlaybackEvent::Playing { position_ms, duration_ms: dur });
                            }
                            Some(PlayerEvent::Paused { position_ms, .. }) => {
                                println!("[player] Paused: {}ms", position_ms);
                                *pos_bg.lock().await  = position_ms;
                                *play_bg.lock().await = false;
                                let dur = *dur_bg.lock().await;
                                let _ = tx_bg.send(PlaybackEvent::Paused { position_ms, duration_ms: dur });
                            }
                            Some(PlayerEvent::Stopped { .. }) => {
                                println!("[player] Stopped");
                                *play_bg.lock().await = false;
                                let _ = tx_bg.send(PlaybackEvent::Stopped);
                            }
                            Some(PlayerEvent::EndOfTrack { .. }) => {
                                let pos = *pos_bg.lock().await;
                                let dur = *dur_bg.lock().await;
                                if dur > 0 && pos < dur - 3000 {
                                    println!("[player] WARNING: Received early EndOfTrack at {}ms / {}ms. Ignoring to prevent premature stop.", pos, dur);
                                } else {
                                    println!("[player] End of track reached correctly.");
                                    *play_bg.lock().await = false;
                                    let _ = tx_bg.send(PlaybackEvent::EndOfTrack);
                                }
                            }
                            None => {
                                println!("[player] Event channel closed");
                                break;
                            }
                            _    => {}
                        }
                    }
                    // Send a progress tick every 250ms while playing
                    _ = interval.tick() => {
                        if *play_bg.lock().await {
                            let dur = *dur_bg.lock().await;
                            let pos = *pos_bg.lock().await;
                            if dur > 0 {
                                // Advance our local position estimate
                                let new_pos = (pos + 250).min(dur);
                                *pos_bg.lock().await = new_pos;
                                let _ = tx_bg.send(PlaybackEvent::Progress {
                                    position_ms: new_pos,
                                    duration_ms: dur,
                                });
                            }
                        }
                    }
                }
            }
        });

        Ok((
            Self {
                username: username.to_string(),
                player: player_arc,
                duration_ms,
                is_playing,
                mixer: mixer_arc,
            },
            event_rx,
        ))
    }

    pub async fn play(&self, track_id: &str) {
        if let Ok(id) = SpotifyId::from_base62(track_id) {
            *self.is_playing.lock().await = false;
            *self.duration_ms.lock().await = 0;
            let track_uri = SpotifyUri::Track { id };
            self.player.load(track_uri, true, 0);
        }
    }

    pub async fn preload(&self, track_id: &str) {
        if let Ok(id) = SpotifyId::from_base62(track_id) {
            println!("[player] Preloading track: {}", track_id);
            let track_uri = SpotifyUri::Track { id };
            self.player.preload(track_uri);
        }
    }

    pub async fn pause(&self) {
        self.player.pause();
        *self.is_playing.lock().await = false;
    }

    pub async fn resume(&self) {
        self.player.play();
        *self.is_playing.lock().await = true;
    }

    pub async fn seek(&self, position_ms: u32) {
        self.player.seek(position_ms);
    }

    pub async fn set_volume(&self, volume: f32) {
        let vol_u16 = (volume * 65535.0) as u16;
        if let Ok(m) = self.mixer.try_lock() {
            Mixer::set_volume(&*m, vol_u16);
            println!("[player] Volume updated to {}/65535", vol_u16);
        }
    }

    pub async fn get_is_playing(&self) -> bool {
        *self.is_playing.lock().await
    }
}

pub fn parse_duration_str_to_ms(dur_str: &str) -> u32 {
    let parts: Vec<&str> = dur_str.split(':').collect();
    let mut secs = 0u32;
    if parts.len() == 2 {
        if let (Ok(m), Ok(s)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            secs = m * 60 + s;
        }
    } else if parts.len() == 3 {
        if let (Ok(h), Ok(m), Ok(s)) = (
            parts[0].parse::<u32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            secs = h * 3600 + m * 60 + s;
        }
    } else if let Ok(s) = dur_str.parse::<u32>() {
        return s;
    }
    secs * 1000
}
