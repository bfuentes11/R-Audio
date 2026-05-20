use librespot_core::authentication::Credentials;
use librespot_protocol::authentication::AuthenticationType;
use librespot::core::config::SessionConfig;
use librespot::core::session::Session;
use librespot::core::spotify_id::SpotifyId;
use librespot::playback::audio_backend;
use librespot::playback::config::{AudioFormat, PlayerConfig};
use librespot::playback::mixer::{Mixer, VolumeGetter, softmixer::SoftMixer};
use librespot::playback::player::{Player, PlayerEvent};
use std::sync::Arc;
use tokio::sync::Mutex;

struct SharedMixer {
    mixer: Arc<Mutex<SoftMixer>>,
}

impl VolumeGetter for SharedMixer {
    fn attenuation_factor(&self) -> f64 {
        if let Ok(m) = self.mixer.try_lock() {
            let vol = Mixer::volume(&*m);
            let ratio = vol as f64 / 65535.0;
            ratio * ratio
        } else {
            1.0
        }
    }
}

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
    Progress {
        position_ms: u32,
        duration_ms: u32,
    },
}

pub struct LibrespotPlayer {
    pub username: String,
    player: Arc<Mutex<Player>>,
    pub duration_ms: Arc<Mutex<u32>>,
    pub is_playing: Arc<Mutex<bool>>,
    mixer: Arc<Mutex<SoftMixer>>,
}

impl LibrespotPlayer {
    pub async fn new(
        username: &str,
        access_token: &str,
    ) -> Result<(Self, tokio::sync::mpsc::UnboundedReceiver<PlaybackEvent>), String> {
        let session_config = SessionConfig::default();

        // Construct token credentials manually — 0.4.2 has no with_access_token()
        let credentials = Credentials {
            username: username.to_string(),
            auth_type: AuthenticationType::AUTHENTICATION_SPOTIFY_TOKEN,
            auth_data: access_token.as_bytes().to_vec(),
        };

        println!("[librespot] Connecting for '{}'...", username);

        let cache_dir = std::path::PathBuf::from(".spotify_cache");
        let _ = std::fs::create_dir_all(&cache_dir);

        let cache = librespot::core::cache::Cache::new(
            Some(cache_dir.clone()),
            None,
            Some(cache_dir.join("volume")),
            Some(1024 * 1024 * 1024),
        )
        .map_err(|e| format!("Failed to create cache: {}", e))?;

        // 0.4.2: connect takes a 4th bool (store_credentials) and returns (Session, Credentials)
        let (session, _) = Session::connect(session_config, credentials, Some(cache), false)
            .await
            .map_err(|e| format!("Librespot session failed: {}", e))?;

        println!("[librespot] Session established.");

        let player_config = PlayerConfig::default();
        let audio_format = AudioFormat::default();
        let backend = audio_backend::find(None).ok_or("No audio backend found")?;

        // 0.4.2: SoftMixer::open returns SoftMixer directly, not Result
        let inner_mixer = SoftMixer::open(Default::default());
        let mixer_arc = Arc::new(Mutex::new(inner_mixer));
        let volume_ctrl = Box::new(SharedMixer {
            mixer: Arc::clone(&mixer_arc),
        });

        // 0.4.2: Player::new returns (Player, PlayerEventChannel) — no get_player_event_channel()
        let (player, mut raw_channel) = Player::new(player_config, session, volume_ctrl, move || {
            backend(None, audio_format)
        });

        let player_arc = Arc::new(Mutex::new(player));
        let duration_ms = Arc::new(Mutex::new(0u32));
        let is_playing = Arc::new(Mutex::new(false));
        let position_ms = Arc::new(Mutex::new(0u32));

        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<PlaybackEvent>();

        let dur_bg = Arc::clone(&duration_ms);
        let play_bg = Arc::clone(&is_playing);
        let pos_bg = Arc::clone(&position_ms);
        let tx_bg = event_tx.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(250));
            loop {
                tokio::select! {
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
                                    println!("[player] WARNING: Early EndOfTrack at {}ms / {}ms. Ignoring.", pos, dur);
                                } else {
                                    println!("[player] End of track.");
                                    *play_bg.lock().await = false;
                                    let _ = tx_bg.send(PlaybackEvent::EndOfTrack);
                                }
                            }
                            None => {
                                println!("[player] Event channel closed");
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = interval.tick() => {
                        if *play_bg.lock().await {
                            let dur = *dur_bg.lock().await;
                            let pos = *pos_bg.lock().await;
                            if dur > 0 {
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
            self.player.lock().await.load(id, true, 0);
        }
    }

    pub async fn preload(&self, track_id: &str) {
        if let Ok(id) = SpotifyId::from_base62(track_id) {
            println!("[player] Preloading track: {}", track_id);
            self.player.lock().await.preload(id);
        }
    }

    pub async fn pause(&self) {
        self.player.lock().await.pause();
        *self.is_playing.lock().await = false;
    }

    pub async fn resume(&self) {
        self.player.lock().await.play();
        *self.is_playing.lock().await = true;
    }

    pub async fn seek(&self, position_ms: u32) {
        self.player.lock().await.seek(position_ms);
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
