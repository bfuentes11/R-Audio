#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod providers;
mod utils;
mod spectrum_analyzer;

use providers::spotify::SpotifyProvider;
use providers::{AudioProvider, Track};
use std::sync::Arc;
use tokio::sync::Mutex;
use slint::Model;
use rspotify::clients::BaseClient;

use crate::providers::spotify_player::LibrespotPlayer;

// ---------------------------------------------------------------------------
// OOBE — first-run detection and Wi-Fi setup helpers
// ---------------------------------------------------------------------------

fn oobe_flag_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home).join(".config").join("r-audio").join("setup-done")
}

fn is_first_run() -> bool {
    !oobe_flag_path().exists()
}

fn mark_setup_complete() {
    let path = oobe_flag_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, "1");
    println!("[oobe] Setup complete flag written.");
}

fn oobe_start_hotspot() {
    if cfg!(target_os = "linux") {
        let _ = std::process::Command::new("nmcli")
            .args(["device", "wifi", "hotspot", "ssid", "R-Audio-Setup"])
            .spawn();
    } else {
        println!("[oobe] Simulating hotspot start.");
    }
}

fn oobe_stop_hotspot() {
    if cfg!(target_os = "linux") {
        let _ = std::process::Command::new("nmcli")
            .args(["connection", "down", "Hotspot"])
            .status();
    } else {
        println!("[oobe] Simulating hotspot stop.");
    }
}

fn oobe_connect_wifi(ssid: &str, password: &str) -> bool {
    if cfg!(target_os = "linux") {
        let mut args = vec!["device", "wifi", "connect", ssid];
        if !password.is_empty() {
            args.extend_from_slice(&["password", password]);
        }
        std::process::Command::new("nmcli")
            .args(&args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        println!("[oobe] Simulating Wi-Fi connect to '{}'", ssid);
        std::thread::sleep(std::time::Duration::from_secs(1));
        true
    }
}

fn oobe_check_internet() -> bool {
    use std::net::ToSocketAddrs;
    "api.spotify.com:80"
        .to_socket_addrs()
        .map(|mut a| a.next().is_some())
        .unwrap_or(false)
}

fn oobe_scan_networks() -> Vec<(String, String)> {
    if cfg!(target_os = "linux") {
        let Ok(out) = std::process::Command::new("nmcli")
            .args(["-t", "-f", "SSID,SIGNAL", "device", "wifi", "list"])
            .output()
        else {
            return vec![];
        };
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let mut nets = vec![];
        for line in text.lines() {
            let parts: Vec<&str> = line.splitn(2, ':').collect();
            if parts.len() >= 2 {
                let ssid = parts[0].trim().to_string();
                let signal = parts[1].trim().to_string();
                if !ssid.is_empty() && !nets.iter().any(|(s, _): &(String, String)| s == &ssid) {
                    nets.push((ssid, signal));
                }
            }
        }
        nets
    } else {
        vec![
            ("Home-WiFi-5G".to_string(), "95".to_string()),
            ("CoffeeShop_Guest".to_string(), "72".to_string()),
            ("Kiosk_Internal".to_string(), "60".to_string()),
        ]
    }
}

fn oobe_parse_form(body: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for pair in body.split('&') {
        let mut parts = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
            let key = urlencoding::decode(k)
                .unwrap_or_else(|_| std::borrow::Cow::Borrowed(k))
                .into_owned();
            let val = urlencoding::decode(v)
                .unwrap_or_else(|_| std::borrow::Cow::Borrowed(v))
                .into_owned();
            map.insert(key, val);
        }
    }
    map
}

fn oobe_start_http_server(tx: tokio::sync::mpsc::Sender<(String, String)>) {
    std::thread::spawn(move || {
        let server = match tiny_http::Server::http("0.0.0.0:8888") {
            Ok(s) => s,
            Err(e) => { eprintln!("[oobe] HTTP server bind failed: {}", e); return; }
        };
        println!("[oobe] Wi-Fi config server listening on :8888");
        for mut req in server.incoming_requests() {
            let url = req.url().to_string();
            if url == "/wifi" || url == "/" {
                let networks = oobe_scan_networks();
                let mut options = String::new();
                for (ssid, signal) in networks {
                    let esc = ssid.replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;");
                    options.push_str(&format!(
                        "<option value=\"{esc}\">{esc} ({signal}%)</option>\n"
                    ));
                }
                let html = format!(r#"<!DOCTYPE html>
<html>
<head>
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>R-Audio Wi-Fi Setup</title>
<style>
body{{background:linear-gradient(135deg,#102a4e 0%,#0a182d 100%);color:#fff;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;margin:0;padding:20px;display:flex;justify-content:center;align-items:center;min-height:100vh;box-sizing:border-box}}
.card{{background:rgba(255,255,255,.07);border:1px solid rgba(255,255,255,.1);border-radius:16px;padding:30px;width:100%;max-width:400px;box-shadow:0 8px 32px rgba(0,0,0,.37);backdrop-filter:blur(8px)}}
h2{{margin-top:0;font-weight:800;color:#4b9beb;letter-spacing:1px}}
p{{color:rgba(255,255,255,.7);font-size:14px;line-height:1.5}}
.fg{{margin-bottom:20px}}
label{{display:block;margin-bottom:8px;font-size:12px;font-weight:700;letter-spacing:1px;color:rgba(255,255,255,.5)}}
select,input{{width:100%;padding:12px;border-radius:8px;border:1px solid rgba(255,255,255,.15);background:rgba(0,0,0,.2);color:#fff;font-size:16px;box-sizing:border-box}}
button{{width:100%;padding:14px;border:none;border-radius:8px;background:#2b7bc5;color:#fff;font-weight:700;font-size:16px;cursor:pointer}}
button:hover{{background:#4b9beb}}
</style>
</head>
<body>
<div class="card">
<h2>R-Audio</h2>
<p>Select your Wi-Fi network to connect this kiosk to the internet.</p>
<form action="/connect" method="POST">
<div class="fg"><label>WI-FI NETWORK</label><select name="ssid" required>{options}</select></div>
<div class="fg"><label>PASSWORD</label><input type="password" name="password" placeholder="Leave blank if open"></div>
<button type="submit">Connect</button>
</form>
</div>
</body>
</html>"#);
                let resp = tiny_http::Response::from_string(html).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap()
                );
                let _ = req.respond(resp);
            } else if req.method() == &tiny_http::Method::Post && url == "/connect" {
                let mut body = String::new();
                if req.as_reader().read_to_string(&mut body).is_ok() {
                    let params = oobe_parse_form(&body);
                    let ssid = params.get("ssid").cloned().unwrap_or_default();
                    let password = params.get("password").cloned().unwrap_or_default();
                    let ack = tiny_http::Response::from_string(
                        r#"<!DOCTYPE html><html><head><meta name="viewport" content="width=device-width,initial-scale=1.0"><title>Connecting…</title>
<style>body{background:linear-gradient(135deg,#102a4e,#0a182d);color:#fff;font-family:sans-serif;display:flex;justify-content:center;align-items:center;min-height:100vh;margin:0}
.card{background:rgba(255,255,255,.07);border:1px solid rgba(255,255,255,.1);border-radius:16px;padding:30px;max-width:380px;text-align:center}
h2{color:#4b9beb;margin-top:0}</style></head>
<body><div class="card"><h2>Connecting…</h2><p>You can close this tab and watch the kiosk screen.</p></div></body></html>"#
                    ).with_header(
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap()
                    );
                    let _ = req.respond(ack);
                    let _ = tx.blocking_send((ssid, password));
                }
            } else {
                let _ = req.respond(tiny_http::Response::from_string("Not Found").with_status_code(404));
            }
        }
    });
}

async fn start_oobe_wifi(
    ui_handle: slint::Weak<MainWindow>,
    spotify: Arc<SpotifyProvider>,
    player: Arc<Mutex<Option<Arc<LibrespotPlayer>>>>,
    go_next: impl Fn() + Clone + Send + 'static,
) {
    let setup_url = "http://10.42.0.1:8888/wifi";

    // Generate QR code — keep as SharedPixelBuffer (Send) until inside invoke_from_event_loop
    let qr_buf: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> =
        match qrcode_generator::to_png_to_vec(setup_url, qrcode_generator::QrCodeEcc::Low, 250) {
            Ok(png) => {
                if let Ok(img) = image::load_from_memory(&png) {
                    let buf = img.to_rgba8();
                    Some(slint::SharedPixelBuffer::clone_from_slice(buf.as_raw(), buf.width(), buf.height()))
                } else {
                    None
                }
            }
            Err(_) => None,
        };

    let _ = slint::invoke_from_event_loop({
        let h = ui_handle.clone();
        move || {
            if let Some(ui) = h.upgrade() {
                let qr_image = match qr_buf {
                    Some(buf) => slint::Image::from_rgba8(buf),
                    None => slint::Image::default(),
                };
                ui.set_oobe_wifi_qr_code(qr_image);
                ui.set_oobe_wifi_status("Starting hotspot…".into());
                ui.set_active_view("oobe-wifi".into());
            }
        }
    });

    oobe_start_hotspot();
    // Give nmcli a moment to bring up the interface
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let _ = slint::invoke_from_event_loop({
        let h = ui_handle.clone();
        move || {
            if let Some(ui) = h.upgrade() {
                ui.set_oobe_wifi_status("Hotspot active — scan the QR code from your phone.".into());
            }
        }
    });

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, String)>(1);
    oobe_start_http_server(tx);

    while let Some((ssid, password)) = rx.recv().await {
        let _ = slint::invoke_from_event_loop({
            let h = ui_handle.clone();
            let s = ssid.clone();
            move || {
                if let Some(ui) = h.upgrade() {
                    ui.set_oobe_wifi_status(format!("Connecting to {}…", s).into());
                }
            }
        });

        oobe_stop_hotspot();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let connected = tokio::task::spawn_blocking({
            let ssid = ssid.clone();
            let pass = password.clone();
            move || {
                if oobe_connect_wifi(&ssid, &pass) {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    oobe_check_internet()
                } else {
                    false
                }
            }
        }).await.unwrap_or(false);

        if connected {
            println!("[oobe] Wi-Fi connected. Proceeding to Spotify pairing.");
            let _ = slint::invoke_from_event_loop({
                let h = ui_handle.clone();
                move || {
                    if let Some(ui) = h.upgrade() {
                        ui.set_oobe_wifi_status("Connected! Setting up Spotify…".into());
                    }
                }
            });
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            start_pairing_flow(ui_handle, spotify, player, go_next);
            return;
        } else {
            println!("[oobe] Wi-Fi connection failed. Restarting hotspot.");
            oobe_start_hotspot();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = slint::invoke_from_event_loop({
                let h = ui_handle.clone();
                move || {
                    if let Some(ui) = h.upgrade() {
                        ui.set_oobe_wifi_status("Connection failed — try again.".into());
                    }
                }
            });
        }
    }
}

slint::include_modules!();

#[derive(Clone, Debug)]
struct TrackItem {
    title: String,
    artist: String,
    artist_id: String,
    id: String,
    duration: String,
    image_buffer: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>,
}

thread_local! {
    static SEARCH_RESULTS_MODEL: std::cell::RefCell<Option<std::rc::Rc<slint::VecModel<UITrack>>>> = std::cell::RefCell::new(None);
    static SPECTRUM_BANDS_MODEL: std::cell::RefCell<Option<std::rc::Rc<slint::VecModel<f32>>>> = std::cell::RefCell::new(None);
}

async fn process_tracks(
    tracks: Vec<Track>,
) -> Vec<TrackItem> {
    let len = tracks.len();
    let mut slots: Vec<Option<TrackItem>> = (0..len).map(|_| None).collect();
    let mut join_set = tokio::task::JoinSet::new();

    for (index, track) in tracks.into_iter().enumerate() {
        let t_id = track.id.clone();
        let t_url = track.album_url.clone();

        join_set.spawn(async move {
            let image_buffer = utils::fetch_and_cache_image(&t_id, &t_url).await;
            (index, track, image_buffer)
        });
    }

    while let Some(res) = join_set.join_next().await {
        if let Ok((index, track, image_buffer)) = res {
            slots[index] = Some(TrackItem {
                title: track.title,
                artist: track.artist,
                artist_id: track.artist_id,
                id: track.id,
                duration: track.duration_str,
                image_buffer,
            });
        }
    }

    slots.into_iter().flatten().collect()
}

/// Returns how long until `expires_at` minus `margin_secs`, or `None` if
/// the deadline is already in the past or the expiry is unknown.
fn duration_until_token_expiry(
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    margin_secs: i64,
) -> Option<std::time::Duration> {
    let target = expires_at? - chrono::Duration::seconds(margin_secs);
    let remaining = (target - chrono::Utc::now()).num_seconds();
    if remaining > 0 {
        Some(std::time::Duration::from_secs(remaining as u64))
    } else {
        None
    }
}

/// Spawns the long-running audio-session manager. The task connects to
/// librespot, drives the event pump, and reconnects on:
///   • `SessionLost`  — librespot worker died (token expired, AP disconnect, etc.)
///   • pre-emptive    — token is 5 minutes from expiry; we rebuild cleanly before
///                       Spotify can yank the AP connection mid-song.
/// After every reconnect, if a track was playing it is resumed at its last
/// known position.
fn initialize_audio_player(
    user_name: String,
    initial_token: String,
    spotify_provider: Arc<SpotifyProvider>,
    player_container: Arc<Mutex<Option<Arc<LibrespotPlayer>>>>,
    ui_handle: slint::Weak<MainWindow>,
    go_next: impl Fn() + Clone + Send + 'static,
) {
    tokio::spawn(async move {
        let mut token = initial_token;
        // Persists across reconnects so we can resume mid-song.
        let mut resume_track: Option<String> = None;
        let mut resume_position_ms: u32 = 0;

        // Exponential backoff for repeated AP-construction failures. Doubles on each
        // consecutive miss (5 → 10 → 20 → 40 → 60) and resets to base on success, so
        // a flaky network won't tight-loop the AP and burn tokens.
        const BACKOFF_BASE_SECS: u64 = 5;
        const BACKOFF_MAX_SECS: u64 = 60;
        let mut backoff_secs: u64 = BACKOFF_BASE_SECS;

        loop {
            println!("[player] Initializing bare-metal Audio Engine...");
            let (player_arc, mut player_events) = match LibrespotPlayer::new(&user_name, &token).await {
                Ok((audio_player, events)) => {
                    backoff_secs = BACKOFF_BASE_SECS;
                    let arc = Arc::new(audio_player);
                    // Start (or restart) the FFT loop tied to this player's capture buffer.
                    spawn_fft_loop(
                        Arc::clone(&arc.sample_buffer),
                        Arc::clone(&arc.is_playing_atom),
                    );
                    *player_container.lock().await = Some(Arc::clone(&arc));
                    (arc, events)
                }
                Err(e) => {
                    println!("[player] Failed to initialize Audio Engine: {} — retry in {}s", e, backoff_secs);
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs.saturating_mul(2)).min(BACKOFF_MAX_SECS);
                    match spotify_provider.get_access_token().await {
                        Ok(t) => { token = t; continue; }
                        Err(e) => {
                            println!("[player] Token refresh failed: {} — giving up", e);
                            return;
                        }
                    }
                }
            };

            // Resume the interrupted track if we reconnected mid-song.
            if let Some(ref track_id) = resume_track {
                println!("[player] Resuming '{}' at {}ms after reconnect", track_id, resume_position_ms);
                player_arc.play(track_id).await;
                if resume_position_ms > 0 {
                    // Give librespot a moment to start buffering before seeking.
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    player_arc.seek(resume_position_ms).await;
                }
            }

            // Schedule a pre-emptive reconnect 5 minutes before the token expires so
            // we never let Spotify pull the AP connection out from under a live stream.
            // Sleep MAX (effectively never) if the expiry is unknown.
            let expires_at = spotify_provider.auth.token_expires_at().await;
            let preempt_dur = duration_until_token_expiry(expires_at, 5 * 60)
                .unwrap_or(std::time::Duration::MAX);
            println!("[player] Pre-emptive reconnect scheduled in {:.0}s", preempt_dur.as_secs_f64());
            let preempt_sleep = tokio::time::sleep(preempt_dur);
            tokio::pin!(preempt_sleep);

            // Snapshot of the currently-playing track updated by Playing/Progress events.
            let mut current_position_ms: u32 = 0;
            let mut session_lost = false;
            let mut preempt_triggered = false;

            loop {
                let ui_ref = ui_handle.clone();
                tokio::select! {
                    event = player_events.recv() => {
                        match event {
                            None => break, // channel closed without SessionLost — clean exit
                            Some(e) => match e {
                                crate::providers::spotify_player::PlaybackEvent::Playing { position_ms, duration_ms } => {
                                    println!("[main] Event: Playing {}/{}", position_ms, duration_ms);
                                    current_position_ms = position_ms;
                                    let progress = if duration_ms > 0 { position_ms as f32 / duration_ms as f32 } else { 0.0 };
                                    let cur_secs  = position_ms / 1000;
                                    let tot_secs  = duration_ms / 1000;
                                    let cur_str   = format!("{}:{:02}", cur_secs / 60, cur_secs % 60);
                                    let tot_str   = format!("{}:{:02}", tot_secs / 60, tot_secs % 60);
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = ui_ref.upgrade() {
                                            ui.set_player_progress_percent(progress);
                                            ui.set_player_current_time_str(cur_str.into());
                                            ui.set_player_total_time_str(tot_str.into());
                                            ui.set_player_is_playing(true);
                                        }
                                    });
                                }
                                crate::providers::spotify_player::PlaybackEvent::Progress { position_ms, duration_ms } => {
                                    current_position_ms = position_ms;
                                    let progress = if duration_ms > 0 { position_ms as f32 / duration_ms as f32 } else { 0.0 };
                                    let cur_secs  = position_ms / 1000;
                                    let tot_secs  = duration_ms / 1000;
                                    let cur_str   = format!("{}:{:02}", cur_secs / 60, cur_secs % 60);
                                    let tot_str   = format!("{}:{:02}", tot_secs / 60, tot_secs % 60);
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = ui_ref.upgrade() {
                                            ui.set_player_progress_percent(progress);
                                            ui.set_player_current_time_str(cur_str.into());
                                            ui.set_player_total_time_str(tot_str.into());
                                        }
                                    });
                                }
                                crate::providers::spotify_player::PlaybackEvent::Paused { position_ms, duration_ms } => {
                                    println!("[main] Event: Paused {}/{}", position_ms, duration_ms);
                                    current_position_ms = position_ms;
                                    let progress = if duration_ms > 0 { position_ms as f32 / duration_ms as f32 } else { 0.0 };
                                    let cur_secs = position_ms / 1000;
                                    let cur_str  = format!("{}:{:02}", cur_secs / 60, cur_secs % 60);
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = ui_ref.upgrade() {
                                            ui.set_player_is_playing(false);
                                            ui.set_player_progress_percent(progress);
                                            ui.set_player_current_time_str(cur_str.into());
                                        }
                                    });
                                }
                                crate::providers::spotify_player::PlaybackEvent::Stopped => {
                                    println!("[main] Event: Stopped");
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = ui_ref.upgrade() {
                                            ui.set_player_is_playing(false);
                                        }
                                    });
                                }
                                crate::providers::spotify_player::PlaybackEvent::EndOfTrack => {
                                    println!("[main] Event: EndOfTrack received in event loop.");
                                    go_next();
                                }
                                crate::providers::spotify_player::PlaybackEvent::SessionLost => {
                                    println!("[main] Event: SessionLost — reconnecting in 2s...");
                                    session_lost = true;
                                    break;
                                }
                            }
                        }
                    }
                    _ = &mut preempt_sleep => {
                        println!("[player] Pre-emptive reconnect — token expiring soon, rebuilding session cleanly.");
                        preempt_triggered = true;
                        break;
                    }
                }

            }

            let needs_reconnect = session_lost || preempt_triggered;
            if !needs_reconnect {
                println!("[main] Player event channel closed cleanly — exiting session manager.");
                return;
            }

            // Capture what was playing before we tear down the player.
            // current_track_id on the player is updated by play(), so it's always
            // the ground truth even if the event pump didn't see a Playing event yet.
            if player_arc.get_is_playing().await {
                resume_track = player_arc.current_track_id.lock().await.clone();
                resume_position_ms = current_position_ms;
            } else {
                resume_track = None;
                resume_position_ms = 0;
            }

            // Drop the dead/old player from the container before reconnecting.
            *player_container.lock().await = None;
            drop(player_arc);

            let delay = if session_lost { 2 } else { 0 };
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }

            match spotify_provider.get_access_token().await {
                Ok(t) => token = t,
                Err(e) => {
                    println!("[main] Reconnect failed — token error: {} — giving up", e);
                    return;
                }
            }
        }
    });
}

fn refresh_accounts_view(
    ui_handle: slint::Weak<MainWindow>,
) {
    tokio::spawn(async move {
        let cache = crate::providers::spotify::auth::SpotifyAuthManager::load_multi_cache();
        
        // Gather raw account data first (which is Send + Sync)
        let mut raw_accounts = Vec::new();
        for (name, profile) in &cache.users {
            let is_active = cache.active_user.as_ref() == Some(name);
            let first_char = name.chars().next().unwrap_or('U').to_uppercase().to_string();
            
            let buffer = if let Some(ref url) = profile.image_url {
                let cache_id = format!("profile_{}", name.replace(" ", "_"));
                utils::fetch_and_cache_image(&cache_id, url).await
            } else {
                None
            };
            
            raw_accounts.push((name.clone(), is_active, buffer, first_char));
        }

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_handle.upgrade() {
                let ui_accounts: Vec<UIAccount> = raw_accounts.into_iter().map(|(name, is_active, buffer, first_char)| {
                    let avatar = match buffer {
                        Some(buf) => slint::Image::from_rgba8(buf),
                        None => slint::Image::default(),
                    };
                    UIAccount {
                        name: name.into(),
                        is_active,
                        avatar,
                        avatar_letter: first_char.into(),
                    }
                }).collect();
                ui.set_spotify_accounts(std::rc::Rc::new(slint::VecModel::from(ui_accounts)).into());
            }
        });
    });
}

fn start_pairing_flow(
    ui_handle: slint::Weak<MainWindow>,
    spotify: Arc<SpotifyProvider>,
    player: Arc<Mutex<Option<Arc<LibrespotPlayer>>>>,
    go_next: impl Fn() + Clone + Send + 'static,
) {
    let ui_ref = ui_handle.clone();
    let spotify_ref = Arc::clone(&spotify);
    let player_ref = Arc::clone(&player);
    let go_next_ref = go_next.clone();

    tokio::spawn(async move {
        println!("[main] Launching Remote Pairing...");

        // Generate session and pairing URL
        let session_id = uuid::Uuid::new_v4().to_string();
        let pairing_url = format!("https://raudio.bryantfuentes.com/auth?session={}", session_id);

        // Generate QR code locally and instantaneously
        let slint_img_buf_opt = match qrcode_generator::to_png_to_vec(&pairing_url, qrcode_generator::QrCodeEcc::Low, 250) {
            Ok(png_bytes) => {
                if let Ok(img) = image::load_from_memory(&png_bytes) {
                    let buffer = img.to_rgba8();
                    Some(slint::SharedPixelBuffer::clone_from_slice(buffer.as_raw(), buffer.width(), buffer.height()))
                } else {
                    None
                }
            }
            Err(e) => {
                println!("[main] Failed to generate QR code locally: {}", e);
                None
            }
        };

        // Open pairing screen with the QR code already pre-loaded
        let ui_clone = ui_ref.clone();
        let pairing_url_clone = pairing_url.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_clone.upgrade() {
                ui.set_spotify_pairing_url(pairing_url_clone.into());
                let qr_image = match slint_img_buf_opt {
                    Some(buf) => slint::Image::from_rgba8(buf),
                    None => slint::Image::default(),
                };
                ui.set_spotify_pairing_qr_code(qr_image);
                ui.set_active_view("spotify-pairing".into());
            }
        });

        // Spin up pairing polling
        let auth_manager = Arc::clone(&spotify_ref.auth);
        let rx = crate::providers::spotify::auth::start_remote_pairing(auth_manager, session_id);

        let ui_pairing = ui_ref.clone();
        let spotify_pairing = Arc::clone(&spotify_ref);
        let player_pairing = Arc::clone(&player_ref);
        let go_next_pairing = go_next_ref.clone();

        tokio::spawn(async move {
            match rx.await {
                Ok(Ok(())) => {
                    println!("[main] Pairing completed successfully!");
                    mark_setup_complete();
                    run_authenticated_startup(ui_pairing, spotify_pairing, player_pairing, go_next_pairing, true).await;
                }
                Ok(Err(e)) => {
                    println!("[main] Pairing failed: {}", e);
                }
                Err(_) => {
                    println!("[main] Pairing channel closed.");
                }
            }
        });
    });
}

async fn run_authenticated_startup(
    ui_handle: slint::Weak<MainWindow>,
    spotify_provider: Arc<SpotifyProvider>,
    player_container: Arc<Mutex<Option<Arc<LibrespotPlayer>>>>,
    go_next: impl Fn() + Clone + Send + 'static,
    transition_view: bool,
) {
    println!("[main] Starting authenticated session routines...");
    

    
    // Trigger background indexing of cached playlists
    let cache_init = Arc::clone(&spotify_provider);
    tokio::spawn(async move {
        cache_init.cache.initialize_index().await;
    });
    
    // 1. Get user name and canonical user ID
    let user_name = spotify_provider.get_current_user_name().await.unwrap_or_else(|_| "Listener".to_string());
    let user_id = spotify_provider.get_current_user_id().await.unwrap_or_else(|_| user_name.clone());
    
    let ui_name = ui_handle.clone();
    let user_name_clone = user_name.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_name.upgrade() {
            ui.set_spotify_user_name(user_name_clone.into());
        }
    });

    // 2. Fetch playlists and dashboard data in parallel
    spawn_dashboard_loader(ui_handle.clone(), Arc::clone(&spotify_provider), user_name.clone());

    // 3. Authenticate and start Librespot player
    let mut player_lock = player_container.lock().await;
    let should_reinit = if let Some(ref active_player) = *player_lock {
        active_player.username != user_id
    } else {
        true
    };

    if should_reinit {
        println!("[main] Re-initializing Librespot player for ID: {}", user_id);
        if let Some(ref active_player) = *player_lock {
            active_player.pause().await;
        }
        *player_lock = None;
        drop(player_lock);

        let librespot_token = spotify_provider.get_access_token().await
            .expect("Failed to get access token for librespot");
        initialize_audio_player(
            user_id,
            librespot_token,
            Arc::clone(&spotify_provider),
            player_container.clone(),
            ui_handle.clone(),
            go_next,
        );
    } else {
        println!("[main] Audio Engine is already active for user ID {}. Skipping player initialization.", user_id);
        drop(player_lock);
    }

    // 4. Finally, transition UI view to dashboard (only if requested)
    if transition_view {
        let ui_view = ui_handle.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_view.upgrade() {
                ui.set_active_view("spotify-dashboard".into());
            }
        });
    }
}

fn spawn_dashboard_loader(
    ui_handle: slint::Weak<MainWindow>,
    spotify_provider: Arc<SpotifyProvider>,
    user_name: String,
) {
    let init_ui = ui_handle.clone();
    let init_spotify = Arc::clone(&spotify_provider);
    let user_name_clone = user_name.clone();

    tokio::spawn(async move {
        println!("[main] Loading dashboard feeds for user: {}", user_name_clone);

        // Fetch fresh data in parallel
        let (recent_res, playlists_res, artists_res) = tokio::join!(
            init_spotify.get_recently_played(),
            init_spotify.get_user_playlists(),
            init_spotify.get_top_artists()
        );

        let recent_failed = recent_res.is_err();
        let playlists_failed = playlists_res.is_err();
        let artists_failed = artists_res.is_err();

        // Process recently played
        let recent_tracks = match recent_res {
            Ok(tracks) => tracks,
            Err(ref e) => {
                println!("[main] Failed to fetch recently played: {}. Trying disk cache...", e);
                if let Some(cached) = init_spotify.get_recently_played_disk(&user_name_clone).await {
                    println!("[main] Loaded recently played from disk cache.");
                    cached
                } else {
                    vec![]
                }
            }
        };

        // Process user playlists
        let playlists_tracks = match playlists_res {
            Ok(tracks) => tracks,
            Err(ref e) => {
                println!("[main] Failed to fetch user playlists: {}. Trying disk cache...", e);
                if let Some(cached) = init_spotify.get_user_playlists_disk(&user_name_clone).await {
                    println!("[main] Loaded user playlists from disk cache.");
                    cached
                } else {
                    vec![]
                }
            }
        };

        // Process top artists
        let artists_tracks = match artists_res {
            Ok(tracks) => tracks,
            Err(ref e) => {
                println!("[main] Failed to fetch top artists: {}. Trying disk cache...", e);
                if let Some(cached) = init_spotify.get_top_artists_disk(&user_name_clone).await {
                    println!("[main] Loaded top artists from disk cache.");
                    cached
                } else {
                    vec![]
                }
            }
        };

        // Clone playlists once for the background crawler; move original into process_tracks.
        let crawl_tracks = if !playlists_failed { playlists_tracks.clone() } else { vec![] };

        // Process and map tracks to UI immediately
        let (proc_recent, proc_playlists, proc_artists) = tokio::join!(
            process_tracks(recent_tracks),
            process_tracks(playlists_tracks),
            process_tracks(artists_tracks)
        );

        let ui_update = init_ui.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_update.upgrade() {
                let map_to_ui = |data: Vec<TrackItem>| -> slint::ModelRc<UITrack> {
                    let ui_tracks: Vec<UITrack> = data.into_iter().map(|t| {
                        UITrack {
                            title: t.title.into(), artist: t.artist.into(), id: t.id.into(), artist_id: t.artist_id.into(), duration: t.duration.into(),
                            album_art: match t.image_buffer { Some(b) => slint::Image::from_rgba8(b), None => slint::Image::default() },
                        }
                    }).collect();
                    std::rc::Rc::new(slint::VecModel::from(ui_tracks)).into()
                };

                ui.set_spotify_recent_tracks(map_to_ui(proc_recent));
                ui.set_spotify_user_playlists(map_to_ui(proc_playlists));
                ui.set_spotify_top_artists(map_to_ui(proc_artists));
                ui.set_spotify_is_loading(false);
            }
        });

        // Spawn background crawler for user playlists tracks to populate the cache & index.
        // Concurrency: 4-way fan-out turns a serial ~25s warm-up (50 playlists × ~500ms)
        // into roughly ~6s. The Spotify Web API tolerates this burst, and the image
        // fetcher's own semaphore still bounds the downstream decode work.
        if !playlists_failed {
            let crawl_spotify = Arc::clone(&init_spotify);
            let crawl_playlists = crawl_tracks;
            let crawl_ui = init_ui.clone();
            tokio::spawn(async move {
                println!("[main] Starting background crawl of user playlists ({} total)...", crawl_playlists.len());

                // Read the currently-playing track once. The previous per-playlist re-read
                // hopped to the UI thread N times and was racy (a track change mid-crawl
                // would partially apply the "present" marker against the wrong track).
                let (tx, rx) = tokio::sync::oneshot::channel();
                let ui_for_id = crawl_ui.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let id = ui_for_id.upgrade()
                        .map(|u| u.get_player_track_id().to_string())
                        .unwrap_or_default();
                    let _ = tx.send(id);
                });
                let current_track_id = rx.await.unwrap_or_default();
                let clean_current_id = current_track_id
                    .strip_prefix("spotify:track:")
                    .unwrap_or(&current_track_id)
                    .to_string();

                let sem = Arc::new(tokio::sync::Semaphore::new(4));
                let matching_playlist_ids: Arc<tokio::sync::Mutex<Vec<String>>> =
                    Arc::new(tokio::sync::Mutex::new(Vec::new()));
                let mut join_set = tokio::task::JoinSet::new();

                for p in crawl_playlists {
                    let pid = p.id.clone();
                    let snap = p.duration_str.clone();
                    let provider = Arc::clone(&crawl_spotify);
                    let sem = Arc::clone(&sem);
                    let clean_id = clean_current_id.clone();
                    let matches = Arc::clone(&matching_playlist_ids);
                    join_set.spawn(async move {
                        let _permit = sem.acquire().await;
                        match provider.get_playlist(&pid, &snap).await {
                            Ok((_name, _owner, _cover, tracks)) => {
                                println!("[main] Background crawl: loaded/cached playlist {}", pid);
                                if !clean_id.is_empty() {
                                    let hit = tracks.iter().any(|t| {
                                        t.id.strip_prefix("spotify:track:").unwrap_or(&t.id) == clean_id
                                    });
                                    if hit {
                                        matches.lock().await.push(pid);
                                    }
                                }
                            }
                            Err(e) => {
                                println!("[main] Background crawl failed for playlist {}: {}", pid, e);
                            }
                        }
                    });
                }
                while join_set.join_next().await.is_some() {}

                // Single O(N) UI sweep at the end instead of per-playlist O(N) scans.
                let matches: Vec<String> = std::mem::take(&mut *matching_playlist_ids.lock().await);
                if !matches.is_empty() {
                    let ui_final = crawl_ui.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_final.upgrade() {
                            let match_set: std::collections::HashSet<String> = matches.into_iter().collect();
                            let playlists_model = ui.get_spotify_user_playlists();
                            for i in 0..playlists_model.row_count() {
                                if let Some(mut playlist) = playlists_model.row_data(i) {
                                    if match_set.contains(&playlist.id.to_string()) {
                                        playlist.artist_id = "present".into();
                                        playlists_model.set_row_data(i, playlist);
                                    }
                                }
                            }
                        }
                    });
                }
                println!("[main] Background crawl of user playlists completed.");
            });
        }

        // Spawn background retry loop if any endpoint failed
        let needs_retry = recent_failed || playlists_failed || artists_failed;
        if needs_retry {
            let retry_ui = init_ui.clone();
            let retry_spotify = Arc::clone(&init_spotify);
            let retry_username = user_name_clone.clone();

            tokio::spawn(async move {
                let mut retry_delay_sec = 5;
                let max_delay_sec = 60;
                
                let mut fetch_recent = recent_failed;
                let mut fetch_playlists = playlists_failed;
                let mut fetch_artists = artists_failed;

                loop {
                    println!("[main] Retrying dashboard API loading for user '{}' in {} seconds...", retry_username, retry_delay_sec);
                    tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_sec)).await;

                    let mut tasks = Vec::new();
                    
                    if fetch_recent {
                        let provider = Arc::clone(&retry_spotify);
                        tasks.push(tokio::spawn(async move {
                            (0, provider.get_recently_played().await)
                        }));
                    }
                    if fetch_playlists {
                        let provider = Arc::clone(&retry_spotify);
                        tasks.push(tokio::spawn(async move {
                            (1, provider.get_user_playlists().await)
                        }));
                    }
                    if fetch_artists {
                        let provider = Arc::clone(&retry_spotify);
                        tasks.push(tokio::spawn(async move {
                            (2, provider.get_top_artists().await)
                        }));
                    }

                    let mut recent_opt = None;
                    let mut playlists_opt = None;
                    let mut artists_opt = None;

                    for task in tasks {
                        if let Ok((id, res)) = task.await {
                            match (id, res) {
                                (0, Ok(tracks)) => {
                                    recent_opt = Some(tracks);
                                    fetch_recent = false;
                                }
                                (1, Ok(tracks)) => {
                                    playlists_opt = Some(tracks);
                                    fetch_playlists = false;
                                }
                                (2, Ok(tracks)) => {
                                    artists_opt = Some(tracks);
                                    fetch_artists = false;
                                }
                                (id, Err(ref e)) => {
                                    println!("[main] Retry task {} failed: {}", id, e);
                                }
                                _ => {}
                            }
                        }
                    }

                    // Process and update UI with any successfully fetched fresh items
                    if recent_opt.is_some() || playlists_opt.is_some() || artists_opt.is_some() {
                        let recent_val = recent_opt.clone();
                        let playlists_val = playlists_opt.clone();
                        let artists_val = artists_opt.clone();
                        
                        let ui_update = retry_ui.clone();
                        
                        tokio::spawn(async move {
                            let mut proc_rec = None;
                            let mut proc_play = None;
                            let mut proc_art = None;

                            if let Some(r) = recent_val {
                                proc_rec = Some(process_tracks(r).await);
                            }
                            if let Some(p) = playlists_val {
                                proc_play = Some(process_tracks(p).await);
                            }
                            if let Some(a) = artists_val {
                                proc_art = Some(process_tracks(a).await);
                            }

                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = ui_update.upgrade() {
                                    let map_to_ui = |data: Vec<TrackItem>| -> slint::ModelRc<UITrack> {
                                        let ui_tracks: Vec<UITrack> = data.into_iter().map(|t| {
                                            UITrack {
                                                title: t.title.into(), artist: t.artist.into(), id: t.id.into(), artist_id: t.artist_id.into(), duration: t.duration.into(),
                                                album_art: match t.image_buffer { Some(b) => slint::Image::from_rgba8(b), None => slint::Image::default() },
                                            }
                                        }).collect();
                                        std::rc::Rc::new(slint::VecModel::from(ui_tracks)).into()
                                    };

                                    if let Some(r) = proc_rec {
                                        ui.set_spotify_recent_tracks(map_to_ui(r));
                                    }
                                    if let Some(p) = proc_play {
                                        ui.set_spotify_user_playlists(map_to_ui(p));
                                    }
                                    if let Some(a) = proc_art {
                                        ui.set_spotify_top_artists(map_to_ui(a));
                                    }
                                }
                            });
                        });
                    }

                    if !fetch_recent && !fetch_playlists && !fetch_artists {
                        println!("[main] All dashboard data successfully loaded fresh. Stopping retry loop.");
                        break;
                    }

                    retry_delay_sec = (retry_delay_sec * 2).min(max_delay_sec);
                }
            });
        }
    });
}

fn spawn_fft_loop(
    sample_buffer: Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>,
    is_playing_atom: Arc<std::sync::atomic::AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut analyzer = spectrum_analyzer::SpectrumAnalyzer::new();
        // Reused scratch buffer for the snapshot we hand to the analyzer.
        // Sized to the capture buffer max so it never reallocates.
        let mut snapshot: Vec<f32> = Vec::with_capacity(spectrum_analyzer::CAPTURE_BUFFER_LEN);
        // Track the last bands we pushed to Slint so we can skip identical frames.
        let mut last_sent: Vec<f32> = vec![0.0f32; spectrum_analyzer::NUM_BANDS];
        // Whether we already sent the all-zeros frame on pause entry.
        let mut sent_pause_zeros = false;
        loop {
            if !is_playing_atom.load(std::sync::atomic::Ordering::Relaxed) {
                if !sent_pause_zeros {
                    // Send zeros once on pause entry, then go idle.
                    analyzer.reset();
                    last_sent.fill(0.0);
                    let zeros = vec![0.0f32; spectrum_analyzer::NUM_BANDS];
                    let _ = slint::invoke_from_event_loop(move || {
                        SPECTRUM_BANDS_MODEL.with(|m| {
                            if let Some(ref model) = *m.borrow() {
                                model.set_vec(zeros);
                            }
                        });
                    });
                    sent_pause_zeros = true;
                }
                // Sleep much longer while paused — no audio to process.
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
            // Music is playing: resume normal 30 Hz cadence.
            sent_pause_zeros = false;
            std::thread::sleep(std::time::Duration::from_millis(33));

            snapshot.clear();
            {
                let Ok(buf) = sample_buffer.lock() else { continue };
                snapshot.extend(buf.iter().copied());
            }

            if snapshot.len() < spectrum_analyzer::FFT_SIZE * 2 {
                continue;
            }

            let new_bands = analyzer.process(&snapshot);

            // Skip the Slint set_vec + re-render when bands haven't changed
            // meaningfully (e.g. near-silent passages).
            const EPSILON: f32 = 0.005;
            let changed = new_bands
                .iter()
                .zip(last_sent.iter())
                .any(|(a, b)| (a - b).abs() > EPSILON);

            if !changed {
                continue;
            }

            let bands_vec: Vec<f32> = new_bands.to_vec();
            last_sent.clone_from(&bands_vec);
            let _ = slint::invoke_from_event_loop(move || {
                SPECTRUM_BANDS_MODEL.with(|m| {
                    if let Some(ref model) = *m.borrow() {
                        model.set_vec(bands_vec);
                    }
                });
            });
        }
    });
}

#[tokio::main]
async fn main() -> Result<(), slint::PlatformError> {
    dotenvy::dotenv().ok();
    println!("Booting R-Audio Bare-Metal UI...");
    
    let ui = MainWindow::new()?;
    let spotify = Arc::new(SpotifyProvider::new());
    let player: Arc<Mutex<Option<Arc<LibrespotPlayer>>>> = Arc::new(Mutex::new(None));

    // Initialize spectrum bands model (24 bands, all silent)
    let bands_model = std::rc::Rc::new(slint::VecModel::from(
        vec![0.0f32; spectrum_analyzer::NUM_BANDS]
    ));
    ui.set_player_spectrum_bands(bands_model.clone().into());
    SPECTRUM_BANDS_MODEL.with(|m| {
        *m.borrow_mut() = Some(bands_model);
    });

    let ui_handle = ui.as_weak();
    
    ui.on_toggle_fullscreen({
        let ui_handle = ui_handle.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                let is_fullscreen = ui.window().is_fullscreen();
                ui.window().set_fullscreen(!is_fullscreen);
            }
        }
    });

    ui.on_backspace_pressed(|s| {
        let mut string = s.to_string();
        string.pop();
        string.into()
    });

    // TextManip global — used directly by VirtualKeyboard without per-site wiring
    let text_manip = TextManip::get(&ui);
    text_manip.on_char_count(|text| {
        text.to_string().chars().count() as i32
    });
    text_manip.on_get_before_cursor(|text, pos| {
        let text_str = text.to_string();
        let chars: Vec<char> = text_str.chars().collect();
        let pos = (pos.max(0) as usize).min(chars.len());
        chars[..pos].iter().collect::<String>().into()
    });
    text_manip.on_get_after_cursor(|text, pos| {
        let text_str = text.to_string();
        let chars: Vec<char> = text_str.chars().collect();
        let pos = (pos.max(0) as usize).min(chars.len());
        chars[pos..].iter().collect::<String>().into()
    });

    // --- SETTINGS CALLBACKS ---

    // Wi-Fi: Scan
    let ui_handle_settings = ui_handle.clone();
    ui.on_settings_scan_wifi({
        let ui_handle = ui_handle_settings.clone();
        move || {
            let ui_handle = ui_handle.clone();
            tokio::spawn(async move {
                // Set scanning indicator
                let _ = slint::invoke_from_event_loop({
                    let h = ui_handle.clone();
                    move || { if let Some(u) = h.upgrade() { u.set_settings_wifi_scanning(true); } }
                });

                let networks = if cfg!(target_os = "linux") {
                    let out = std::process::Command::new("nmcli")
                        .args(&["-t", "-f", "SSID,SIGNAL", "device", "wifi", "list"])
                        .output()
                        .unwrap_or_else(|_| std::process::Output {
                            status: std::process::ExitStatus::default(),
                            stdout: vec![],
                            stderr: vec![],
                        });
                    let text = String::from_utf8_lossy(&out.stdout).to_string();
                    let mut nets: Vec<UIWifiNetwork> = vec![];
                    for line in text.lines() {
                        let parts: Vec<&str> = line.splitn(2, ':').collect();
                        if parts.len() >= 2 {
                            let ssid = parts[0].trim().to_string();
                            let signal = parts[1].trim().to_string();
                            if !ssid.is_empty() && !nets.iter().any(|n: &UIWifiNetwork| n.ssid == ssid.as_str()) {
                                nets.push(UIWifiNetwork { ssid: ssid.into(), signal: signal.into() });
                            }
                        }
                    }
                    nets
                } else {
                    // Simulated networks on non-Linux
                    vec![
                        UIWifiNetwork { ssid: "Home-WiFi-5G".into(), signal: "95".into() },
                        UIWifiNetwork { ssid: "CoffeeShop_Guest".into(), signal: "72".into() },
                        UIWifiNetwork { ssid: "Kiosk_Internal".into(), signal: "60".into() },
                    ]
                };

                let _ = slint::invoke_from_event_loop({
                    let h = ui_handle.clone();
                    move || {
                        if let Some(u) = h.upgrade() {
                            let model = std::rc::Rc::new(slint::VecModel::from(networks));
                            u.set_settings_wifi_networks(model.into());
                            u.set_settings_wifi_scanning(false);
                        }
                    }
                });
            });
        }
    });

    // Wi-Fi: Connect
    let ui_handle_wifi = ui_handle.clone();
    ui.on_settings_connect_wifi({
        let ui_handle = ui_handle_wifi.clone();
        move |ssid, password| {
            let ui_handle = ui_handle.clone();
            let ssid = ssid.to_string();
            let password = password.to_string();
            tokio::spawn(async move {
                let _ = slint::invoke_from_event_loop({
                    let h = ui_handle.clone();
                    let s = ssid.clone();
                    move || {
                        if let Some(u) = h.upgrade() {
                            u.set_settings_status_message(format!("Connecting to {}…", s).into());
                        }
                    }
                });

                let result = if cfg!(target_os = "linux") {
                    let mut args = vec!["nmcli", "device", "wifi", "connect", &ssid];
                    if !password.is_empty() { args.extend_from_slice(&["password", &password]); }
                    std::process::Command::new("nmcli")
                        .args(&args)
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false)
                } else {
                    println!("[settings] Simulating Wi-Fi connect to '{}' with password '{}'", ssid, password);
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    true
                };

                let msg = if result {
                    format!("Connected to {}", ssid)
                } else {
                    format!("Failed to connect to {}", ssid)
                };

                let _ = slint::invoke_from_event_loop({
                    let h = ui_handle.clone();
                    move || {
                        if let Some(u) = h.upgrade() {
                            u.set_settings_status_message(msg.into());
                        }
                    }
                });
            });
        }
    });

    // Device Name: Update hostname
    let ui_handle_hostname = ui_handle.clone();
    ui.on_settings_update_device_name({
        let ui_handle = ui_handle_hostname.clone();
        move |name| {
            let ui_handle = ui_handle.clone();
            let name = name.to_string();
            tokio::spawn(async move {
                let result = if cfg!(target_os = "linux") {
                    let ok = std::process::Command::new("hostnamectl")
                        .args(&["set-hostname", &name])
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    // Also update /etc/hosts so local resolution works
                    if ok {
                        let hosts = std::fs::read_to_string("/etc/hosts").unwrap_or_default();
                        let updated = hosts.lines()
                            .map(|l| if l.starts_with("127.0.1.1") { format!("127.0.1.1\t{}", name) } else { l.to_string() })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let _ = std::fs::write("/etc/hosts", updated);
                    }
                    ok
                } else {
                    println!("[settings] Simulating hostname change to '{}'", name);
                    true
                };

                let msg = if result {
                    format!("Device name set to \"{}\"", name)
                } else {
                    "Failed to update hostname".to_string()
                };

                let _ = slint::invoke_from_event_loop({
                    let h = ui_handle.clone();
                    let name_copy = name.clone();
                    move || {
                        if let Some(u) = h.upgrade() {
                            u.set_settings_status_message(msg.into());
                            u.set_settings_current_device_name(name_copy.into());
                        }
                    }
                });
            });
        }
    });

    // Bluetooth: Scan
    let ui_handle_bt_scan = ui_handle.clone();
    ui.on_settings_scan_bluetooth({
        let ui_handle = ui_handle_bt_scan.clone();
        move || {
            let ui_handle = ui_handle.clone();
            tokio::spawn(async move {
                let _ = slint::invoke_from_event_loop({
                    let h = ui_handle.clone();
                    move || { if let Some(u) = h.upgrade() { u.set_settings_bluetooth_scanning(true); } }
                });

                // Run bluetoothctl scan briefly then list
                if cfg!(target_os = "linux") {
                    let _ = std::process::Command::new("bluetoothctl").args(&["scan", "on"]).spawn();
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let _ = std::process::Command::new("bluetoothctl").args(&["scan", "off"]).spawn();
                } else {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }

                let devices = if cfg!(target_os = "linux") {
                    let out = std::process::Command::new("bluetoothctl")
                        .args(&["devices"])
                        .output()
                        .unwrap_or_else(|_| std::process::Output {
                            status: std::process::ExitStatus::default(),
                            stdout: vec![],
                            stderr: vec![],
                        });
                    let text = String::from_utf8_lossy(&out.stdout).to_string();
                    text.lines()
                        .filter_map(|l| {
                            // Format: "Device AA:BB:CC:DD:EE:FF Device Name"
                            let parts: Vec<&str> = l.splitn(3, ' ').collect();
                            if parts.len() == 3 && parts[0] == "Device" {
                                Some(UIBluetoothDevice {
                                    mac: parts[1].into(),
                                    name: parts[2].into(),
                                    connected: false,
                                })
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![
                        UIBluetoothDevice { name: "JBL Flip 6".into(), mac: "AA:BB:CC:DD:EE:01".into(), connected: false },
                        UIBluetoothDevice { name: "Sony WH-1000XM5".into(), mac: "AA:BB:CC:DD:EE:02".into(), connected: true },
                    ]
                };

                let _ = slint::invoke_from_event_loop({
                    let h = ui_handle.clone();
                    move || {
                        if let Some(u) = h.upgrade() {
                            let model = std::rc::Rc::new(slint::VecModel::from(devices));
                            u.set_settings_bluetooth_devices(model.into());
                            u.set_settings_bluetooth_scanning(false);
                        }
                    }
                });
            });
        }
    });

    // Bluetooth: Connect / Disconnect toggle
    let ui_handle_bt_connect = ui_handle.clone();
    ui.on_settings_connect_bluetooth({
        let ui_handle = ui_handle_bt_connect.clone();
        move |mac| {
            let ui_handle = ui_handle.clone();
            let mac = mac.to_string();
            tokio::spawn(async move {
                let _ = slint::invoke_from_event_loop({
                    let h = ui_handle.clone();
                    let m = mac.clone();
                    move || {
                        if let Some(u) = h.upgrade() {
                            u.set_settings_status_message(format!("Connecting to {}…", m).into());
                        }
                    }
                });

                let result = if cfg!(target_os = "linux") {
                    std::process::Command::new("bluetoothctl")
                        .args(&["connect", &mac])
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false)
                } else {
                    println!("[settings] Simulating Bluetooth connect to '{}'", mac);
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    true
                };

                let msg = if result {
                    format!("Bluetooth connected: {}", mac)
                } else {
                    format!("Failed to connect: {}", mac)
                };

                let _ = slint::invoke_from_event_loop({
                    let h = ui_handle.clone();
                    move || {
                        if let Some(u) = h.upgrade() {
                            u.set_settings_status_message(msg.into());
                        }
                    }
                });
            });
        }
    });

    let master_tracks = Arc::new(Mutex::new(Vec::<TrackItem>::new()));


    // --- PLAYBACK QUEUE STATE ---
    #[derive(Default)]
    struct PlaybackQueue {
        tracks: Vec<TrackItem>,
        current_index: usize,
    }
    let playback_queue = Arc::new(Mutex::new(PlaybackQueue::default()));
    let active_source_tracks = Arc::new(Mutex::new(Vec::<Track>::default()));

    // --- SEARCH CALLBACK ---
    let spotify_search = Arc::clone(&spotify);
    let master_search = Arc::clone(&master_tracks);
    let ui_handle_search = ui_handle.clone();
    let ast_search = Arc::clone(&active_source_tracks);
    
    let search_results_model = std::rc::Rc::new(slint::VecModel::<UITrack>::default());
    ui.set_spotify_search_results(search_results_model.clone().into());
    SEARCH_RESULTS_MODEL.with(|m| {
        *m.borrow_mut() = Some(search_results_model);
    });

    ui.on_search_music(move |query| {
        let ui_handle_clone = ui_handle_search.clone();
        let spotify_thread = Arc::clone(&spotify_search);
        let master_thread = Arc::clone(&master_search);
        let ast_thread = Arc::clone(&ast_search);
        let query_string = query.to_string();
        println!("[search] search_music callback triggered with query: '{}'", query_string);

        tokio::spawn(async move {
            let provider = &*spotify_thread;
            match provider.search(&query_string).await {
                Ok(tracks) => {
                    println!("[search] Found {} tracks", tracks.len());
                    let has_more = provider.next_url.lock().await.is_some();
                    
                    // Populate active_source_tracks
                    {
                        let mut ast_lock = ast_thread.lock().await;
                        ast_lock.clear();
                        ast_lock.extend(tracks.clone());
                    }

                    let new_data = process_tracks(tracks).await;
                    
                    {
                        let mut master_lock = master_thread.lock().await;
                        master_lock.clear();
                        master_lock.extend(new_data.clone());
                    }

                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_handle_clone.upgrade() {
                            ui.set_spotify_has_more(has_more);
                            let ui_tracks: Vec<UITrack> = new_data.into_iter().map(|t| {
                                UITrack {
                                    title: t.title.into(), artist: t.artist.into(), id: t.id.into(), artist_id: t.artist_id.into(), duration: t.duration.into(),
                                    album_art: match t.image_buffer { Some(b) => slint::Image::from_rgba8(b), None => slint::Image::default() },
                                }
                            }).collect();
                            SEARCH_RESULTS_MODEL.with(|m| {
                                if let Some(ref model) = *m.borrow() {
                                    model.set_vec(ui_tracks);
                                }
                            });
                        }
                    });
                }
                Err(e) => {
                    println!("[search] Search failed with error: {}", e);
                }
            }
        });
    });

    // --- LOAD MORE CALLBACK ---
    let spotify_more = Arc::clone(&spotify);
    let master_more = Arc::clone(&master_tracks);
    let ui_handle_more = ui_handle.clone();
    let ast_more = Arc::clone(&active_source_tracks);

    ui.on_load_more(move || {
        let ui_handle_clone = ui_handle_more.clone();
        let spotify_thread = Arc::clone(&spotify_more);
        let master_thread = Arc::clone(&master_more);
        let ast_thread = Arc::clone(&ast_more);

        tokio::spawn(async move {
            let provider = &*spotify_thread;
            if let Ok(tracks) = provider.load_more().await {
                if tracks.is_empty() { return; }

                let has_more = provider.next_url.lock().await.is_some();
                
                // Populate active_source_tracks
                {
                    let mut ast_lock = ast_thread.lock().await;
                    ast_lock.extend(tracks.clone());
                }

                let new_data = process_tracks(tracks).await;
                
                {
                    let mut master_lock = master_thread.lock().await;
                    master_lock.extend(new_data.clone());
                }

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle_clone.upgrade() {
                        ui.set_spotify_has_more(has_more);
                        SEARCH_RESULTS_MODEL.with(|m| {
                            if let Some(ref model) = *m.borrow() {
                                for t in new_data {
                                    model.push(UITrack {
                                        title: t.title.into(), artist: t.artist.into(), id: t.id.into(), artist_id: t.artist_id.into(), duration: t.duration.into(),
                                        album_art: match t.image_buffer { Some(b) => slint::Image::from_rgba8(b), None => slint::Image::default() },
                                    });
                                }
                            }
                        });
                    }
                });
            }
        });
    });

    // --- OPEN ALBUM CALLBACK ---
    let spotify_album = Arc::clone(&spotify);
    let ui_handle_album = ui_handle.clone();
    let ast_album = Arc::clone(&active_source_tracks);

    ui.on_open_album(move |track_id| {
        let ui_handle_clone = ui_handle_album.clone();
        let spotify_thread = Arc::clone(&spotify_album);
        let ast_thread = Arc::clone(&ast_album);
        let t_id = track_id.to_string();

        tokio::spawn(async move {
            let provider = &*spotify_thread;
            let result = provider.get_album_from_track(&t_id).await;


            if let Ok((album_name, artist_name, cover_url, tracks)) = result {
                // Populate active_source_tracks
                {
                    let mut ast_lock = ast_thread.lock().await;
                    ast_lock.clear();
                    ast_lock.extend(tracks.clone());
                }

                // 1. Render and transition UI instantly!
                let ui_handle_bg = ui_handle_clone.clone();
                let album_name_clone = album_name.clone();
                let artist_name_clone = artist_name.clone();
                let tracks_clone = tracks.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle_bg.upgrade() {
                        ui.set_spotify_album_title(album_name_clone.into());
                        ui.set_spotify_album_artist(artist_name_clone.into());
                        ui.set_spotify_album_cover(slint::Image::default());

                        let ui_tracks: Vec<UITrack> = tracks_clone.iter().map(|t| UITrack {
                            title: t.title.clone().into(),
                            artist: t.artist.clone().into(),
                            id: t.id.clone().into(),
                            artist_id: t.artist_id.clone().into(),
                            duration: t.duration_str.clone().into(),
                            album_art: slint::Image::default(),
                        }).collect();
                        
                        ui.set_spotify_album_tracks(std::rc::Rc::new(slint::VecModel::from(ui_tracks)).into());
                        ui.set_spotify_album_is_loading(false);
                        ui.set_active_view("spotify-album".into());
                    }
                });

                // 2. Fetch the album cover art asynchronously in the background!
                let cover_id = if let Some(first_track) = tracks.first() {
                    first_track.id.clone()
                } else {
                    t_id.clone()
                };
                let cover_buffer = utils::fetch_and_cache_image(&cover_id, &cover_url).await;
                let ui_img = ui_handle_clone.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_img.upgrade() {
                        if let Some(buf) = cover_buffer {
                            let img = slint::Image::from_rgba8(buf);
                            ui.set_spotify_album_cover(img.clone());
                            
                            // Also update the track listing's album art!
                            let tracks_model = ui.get_spotify_album_tracks();
                            for i in 0..tracks_model.row_count() {
                                if let Some(mut track) = tracks_model.row_data(i) {
                                    track.album_art = img.clone();
                                    tracks_model.set_row_data(i, track);
                                }
                            }
                        }
                    }
                });
            } else {
                println!("ERROR loading album from track: {}", t_id);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle_clone.upgrade() {
                        ui.set_spotify_album_title("Error loading album".into());
                        ui.set_spotify_album_is_loading(false);
                    }
                });
            }
        });
    });

    // --- OPEN PLAYLIST CALLBACK ---
    let spotify_playlist = Arc::clone(&spotify);
    let ui_handle_playlist = ui_handle.clone();
    let ast_playlist = Arc::clone(&active_source_tracks);
    
    ui.on_open_playlist(move |id, snapshot_id| {
        if let Some(ui) = ui_handle_playlist.upgrade() {
            ui.set_spotify_album_is_loading(true); 
            ui.set_spotify_album_title("Loading Playlist...".into());
            ui.set_spotify_album_artist("".into());
            ui.set_spotify_album_cover(slint::Image::default()); 
            ui.set_spotify_album_tracks(std::rc::Rc::new(slint::VecModel::from(vec![])).into()); 
            ui.set_active_view("spotify-album".into()); 
        }

        let ui_handle_clone = ui_handle_playlist.clone();
        let spotify_thread = Arc::clone(&spotify_playlist);
        let ast_thread = Arc::clone(&ast_playlist);
        let playlist_id = id.to_string();
        let snap_id = snapshot_id.to_string();

        tokio::spawn(async move {
            let provider = &*spotify_thread;
            
            if let Ok((name, owner, cover_url, tracks)) = provider.get_playlist(&playlist_id, &snap_id).await {
                // Populate active_source_tracks
                {
                    let mut ast_lock = ast_thread.lock().await;
                    ast_lock.clear();
                    ast_lock.extend(tracks.clone());
                }
                
    
                
                let mut tracks_data = Vec::new();
                for t in &tracks {
                    tracks_data.push((t.title.clone(), t.artist.clone(), t.artist_id.clone(), t.id.clone(), t.duration_str.clone()));
                }
                
                let cover_buffer = utils::fetch_and_cache_image(&playlist_id, &cover_url).await;

                let ui_handle_bg = ui_handle_clone.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle_bg.upgrade() {
                        let mut ui_tracks_init = Vec::new();
                        for (title, artist, aid, id, duration) in tracks_data {
                            ui_tracks_init.push(UITrack {
                                title: title.into(),
                                artist: artist.into(),
                                id: id.into(),
                                artist_id: aid.into(),
                                duration: duration.into(),
                                album_art: slint::Image::default(),
                            });
                        }
                        
                        ui.set_spotify_album_title(name.into());
                        ui.set_spotify_album_artist(owner.into());
                        
                        if let Some(buf) = cover_buffer {
                            ui.set_spotify_album_cover(slint::Image::from_rgba8(buf));
                        } else {
                            ui.set_spotify_album_cover(slint::Image::default()); 
                        }
                        
                        let model = std::rc::Rc::new(slint::VecModel::from(ui_tracks_init));
                        ui.set_spotify_album_tracks(model.into());
                        ui.set_spotify_album_is_loading(false); 
                    }
                });
                
                // Batch album-art updates: workers send (index, buf) to a channel;
                // a single coalescing task flushes every 50ms in one event-loop hop.
                let (img_tx, mut img_rx) =
                    tokio::sync::mpsc::unbounded_channel::<(usize, slint::SharedPixelBuffer<slint::Rgba8Pixel>)>();
                let ui_handle_flusher = ui_handle_clone.clone();
                tokio::spawn(async move {
                    let mut pending: Vec<(usize, slint::SharedPixelBuffer<slint::Rgba8Pixel>)> = Vec::new();
                    let mut tick = tokio::time::interval(tokio::time::Duration::from_millis(50));
                    tick.tick().await; // consume immediate tick
                    loop {
                        tokio::select! {
                            msg = img_rx.recv() => {
                                match msg {
                                    Some(item) => pending.push(item),
                                    None => {
                                        // All senders dropped — final flush and exit.
                                        if !pending.is_empty() {
                                            let batch = std::mem::take(&mut pending);
                                            let ui_h = ui_handle_flusher.clone();
                                            let _ = slint::invoke_from_event_loop(move || {
                                                if let Some(ui) = ui_h.upgrade() {
                                                    let tracks_model = ui.get_spotify_album_tracks();
                                                    for (idx, buf) in batch {
                                                        if let Some(mut t) = tracks_model.row_data(idx) {
                                                            t.album_art = slint::Image::from_rgba8(buf);
                                                            tracks_model.set_row_data(idx, t);
                                                        }
                                                    }
                                                }
                                            });
                                        }
                                        break;
                                    }
                                }
                            }
                            _ = tick.tick() => {
                                if !pending.is_empty() {
                                    let batch = std::mem::take(&mut pending);
                                    let ui_h = ui_handle_flusher.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = ui_h.upgrade() {
                                            let tracks_model = ui.get_spotify_album_tracks();
                                            for (idx, buf) in batch {
                                                if let Some(mut t) = tracks_model.row_data(idx) {
                                                    t.album_art = slint::Image::from_rgba8(buf);
                                                    tracks_model.set_row_data(idx, t);
                                                }
                                            }
                                        }
                                    });
                                }
                            }
                        }
                    }
                });

                for (index, track) in tracks.into_iter().enumerate() {
                    let t_id = track.id.clone();
                    let track_album_url = track.album_url.clone();
                    let tx = img_tx.clone();
                    tokio::spawn(async move {
                        if let Some(buf) = utils::fetch_and_cache_image(&t_id, &track_album_url).await {
                            let _ = tx.send((index, buf));
                        }
                    });
                }
                drop(img_tx); // Last sender goes when all workers drop their clones.

            } else {
                println!("ERROR: Failed to fetch playlist from Spotify API!");
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle_clone.upgrade() {
                        ui.set_spotify_album_title("Error loading playlist".into());
                        ui.set_spotify_album_is_loading(false); 
                    }
                });
            }
        });
    });

    // --- OPEN ALBUM BY ID CALLBACK (from artist discography) ---
    let spotify_album_by_id = Arc::clone(&spotify);
    let ui_handle_album_by_id = ui_handle.clone();
    let ast_album_by_id = Arc::clone(&active_source_tracks);

    ui.on_open_album_by_id(move |album_id| {
        let ui_handle_clone = ui_handle_album_by_id.clone();
        let spotify_thread = Arc::clone(&spotify_album_by_id);
        let ast_thread = Arc::clone(&ast_album_by_id);
        let aid = album_id.to_string();

        tokio::spawn(async move {
            let provider = &*spotify_thread;
            
            // Check cache FIRST!
            if let Some((album_name, artist_name, cover_url, tracks)) = provider.cache.get_album(&aid).await {
                // Populate active_source_tracks
                {
                    let mut ast_lock = ast_thread.lock().await;
                    ast_lock.clear();
                    ast_lock.extend(tracks.clone());
                }
    

                // 1. Transition and show tracks instantly!
                let ui_bg = ui_handle_clone.clone();
                let album_name_clone = album_name.clone();
                let artist_name_clone = artist_name.clone();
                let tracks_clone = tracks.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_bg.upgrade() {
                        let ui_tracks: Vec<UITrack> = tracks_clone.iter().map(|t| UITrack {
                            title: t.title.clone().into(), artist: t.artist.clone().into(),
                            id: t.id.clone().into(), artist_id: t.artist_id.clone().into(), duration: t.duration_str.clone().into(),
                            album_art: slint::Image::default(),
                        }).collect();
                        
                        ui.set_spotify_album_title(album_name_clone.into());
                        ui.set_spotify_album_artist(artist_name_clone.into());
                        ui.set_spotify_album_cover(slint::Image::default());
                        ui.set_spotify_album_tracks(std::rc::Rc::new(slint::VecModel::from(ui_tracks)).into());
                        ui.set_spotify_album_is_loading(false);
                        ui.set_active_view("spotify-album".into());
                    }
                });

                // 2. Fetch cover image in background!
                let cover_buffer = utils::fetch_and_cache_image(&aid, &cover_url).await;
                let ui_img = ui_handle_clone.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_img.upgrade() {
                        if let Some(buf) = cover_buffer {
                            let img = slint::Image::from_rgba8(buf);
                            ui.set_spotify_album_cover(img.clone());
                            
                            let tracks_model = ui.get_spotify_album_tracks();
                            for i in 0..tracks_model.row_count() {
                                if let Some(mut track) = tracks_model.row_data(i) {
                                    track.album_art = img.clone();
                                    tracks_model.set_row_data(i, track);
                                }
                            }
                        }
                    }
                });
                return;
            }

            // NOT cached: Show skeleton first, then fetch!
            let ui_init = ui_handle_clone.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_init.upgrade() {
                    ui.set_spotify_album_is_loading(true);
                    ui.set_spotify_album_title("Loading Album...".into());
                    ui.set_spotify_album_artist("".into());
                    ui.set_spotify_album_cover(slint::Image::default());
                    ui.set_spotify_album_tracks(std::rc::Rc::new(slint::VecModel::from(vec![])).into());
                    ui.set_active_view("spotify-album".into());
                }
            });

            let result = provider.get_album_by_id(&aid).await;


            match result {
                Ok((album_name, artist_name, cover_url, tracks)) => {
                    // Populate active_source_tracks
                    {
                        let mut ast_lock = ast_thread.lock().await;
                        ast_lock.clear();
                        ast_lock.extend(tracks.clone());
                    }
                    let tracks_data: Vec<(String,String,String,String,String)> = tracks.iter()
                        .map(|t| (t.title.clone(), t.artist.clone(), t.artist_id.clone(), t.id.clone(), t.duration_str.clone()))
                        .collect();

                    // 1. Show tracks instantly and dismiss skeleton!
                    let ui_bg = ui_handle_clone.clone();
                    let album_name_clone = album_name.clone();
                    let artist_name_clone = artist_name.clone();
                    let tracks_data_clone = tracks_data.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_bg.upgrade() {
                            let ui_tracks: Vec<UITrack> = tracks_data_clone.iter().map(|(t,a,aid,id,d)| UITrack {
                                title: t.clone().into(), artist: a.clone().into(),
                                id: id.clone().into(), artist_id: aid.clone().into(), duration: d.clone().into(),
                                album_art: slint::Image::default(),
                            }).collect();
                            
                            ui.set_spotify_album_title(album_name_clone.into());
                            ui.set_spotify_album_artist(artist_name_clone.into());
                            ui.set_spotify_album_cover(slint::Image::default());
                            ui.set_spotify_album_tracks(std::rc::Rc::new(slint::VecModel::from(ui_tracks)).into());
                            ui.set_spotify_album_is_loading(false);
                        }
                    });

                    // 2. Fetch cover image in background!
                    let cover_buffer = utils::fetch_and_cache_image(&aid, &cover_url).await;
                    let ui_img = ui_handle_clone.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_img.upgrade() {
                            if let Some(buf) = cover_buffer {
                                let img = slint::Image::from_rgba8(buf);
                                ui.set_spotify_album_cover(img.clone());
                                
                                let tracks_model = ui.get_spotify_album_tracks();
                                for i in 0..tracks_model.row_count() {
                                    if let Some(mut track) = tracks_model.row_data(i) {
                                        track.album_art = img.clone();
                                        tracks_model.set_row_data(i, track);
                                    }
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    println!("ERROR loading album: {}", e);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_handle_clone.upgrade() {
                            ui.set_spotify_album_title("Error loading album".into());
                            ui.set_spotify_album_is_loading(false);
                        }
                    });
                }
            }
        });
    });

    // --- OPEN ARTIST CALLBACK ---
    let spotify_artist = Arc::clone(&spotify);
    let ui_handle_artist = ui_handle.clone();
    let ast_artist = Arc::clone(&active_source_tracks);

    ui.on_open_artist(move |artist_id| {
        let ui_handle_clone = ui_handle_artist.clone();
        let spotify_thread = Arc::clone(&spotify_artist);
        let ast_thread = Arc::clone(&ast_artist);
        let a_id = artist_id.to_string();

        if let Some(ui) = ui_handle_clone.upgrade() {
            ui.set_spotify_artist_is_loading(true);
            ui.set_spotify_artist_name("Loading...".into());
            ui.set_spotify_artist_top_tracks(std::rc::Rc::new(slint::VecModel::from(vec![])).into());
            ui.set_spotify_artist_albums(std::rc::Rc::new(slint::VecModel::from(vec![])).into());
            ui.set_active_view("spotify-artist".into());
        }

        tokio::spawn(async move {
            let provider = &*spotify_thread;
            let result = provider.get_artist(&a_id).await;


            match result {
                Ok((name, image_url, top_tracks, albums)) => {
                    // Populate active_source_tracks
                    {
                        let mut ast_lock = ast_thread.lock().await;
                        ast_lock.clear();
                        ast_lock.extend(top_tracks.clone());
                    }
                    let top_data: Vec<(String,String,String,String,String)> = top_tracks.iter()
                        .map(|t| (t.title.clone(), t.artist.clone(), t.artist_id.clone(), t.id.clone(), t.duration_str.clone()))
                        .collect();
                    let album_data: Vec<(String,String,String)> = albums.iter()
                        .map(|a| (a.title.clone(), a.artist.clone(), a.id.clone()))
                        .collect();

                    let photo_buffer = utils::fetch_and_cache_image(&a_id, &image_url).await;

                    let ui_bg = ui_handle_clone.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_bg.upgrade() {
                            let tracks_init: Vec<UITrack> = top_data.iter().map(|(t,a,aid,id,d)| UITrack {
                                title: t.clone().into(), artist: a.clone().into(),
                                id: id.clone().into(), artist_id: aid.clone().into(), duration: d.clone().into(),
                                album_art: slint::Image::default(),
                            }).collect();
                            let albums_init: Vec<UITrack> = album_data.iter().map(|(t,yr,id)| UITrack {
                                title: t.clone().into(), artist: yr.clone().into(),
                                id: id.clone().into(), artist_id: "".into(), duration: "".into(),
                                album_art: slint::Image::default(),
                            }).collect();

                            ui.set_spotify_artist_name(name.into());
                            if let Some(buf) = photo_buffer {
                                ui.set_spotify_artist_photo(slint::Image::from_rgba8(buf));
                            }
                            ui.set_spotify_artist_top_tracks(std::rc::Rc::new(slint::VecModel::from(tracks_init)).into());
                            ui.set_spotify_artist_albums(std::rc::Rc::new(slint::VecModel::from(albums_init)).into());
                            ui.set_spotify_artist_is_loading(false);
                        }
                    });

                    for (index, track) in top_tracks.into_iter().enumerate() {
                        let ui_img = ui_handle_clone.clone();
                        let t_id = track.id.clone();
                        let url = track.album_url.clone();
                        tokio::spawn(async move {
                            let image_buffer = utils::fetch_and_cache_image(&t_id, &url).await;
                            if let Some(buf) = image_buffer {
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_img.upgrade() {
                                        let m = ui.get_spotify_artist_top_tracks();
                                        if let Some(mut row) = m.row_data(index) {
                                            row.album_art = slint::Image::from_rgba8(buf);
                                            m.set_row_data(index, row);
                                        }
                                    }
                                });
                            }
                        });
                    }
                    for (index, album) in albums.into_iter().enumerate() {
                        let ui_img = ui_handle_clone.clone();
                        let alb_id = album.id.clone();
                        let url = album.album_url.clone();
                        tokio::spawn(async move {
                            let image_buffer = utils::fetch_and_cache_image(&alb_id, &url).await;
                            if let Some(buf) = image_buffer {
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_img.upgrade() {
                                        let m = ui.get_spotify_artist_albums();
                                        if let Some(mut row) = m.row_data(index) {
                                            row.album_art = slint::Image::from_rgba8(buf);
                                            m.set_row_data(index, row);
                                        }
                                    }
                                });
                            }
                        });
                    }
                }
                Err(e) => {
                    println!("ERROR: Failed to fetch artist: {}", e);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_handle_clone.upgrade() {
                            ui.set_spotify_artist_name("Error loading artist".into());
                            ui.set_spotify_artist_is_loading(false);
                        }
                    });
                }
            }
        });
    });

    // --- PLAY ID FUNCTION (Internal) ---
    let play_id = {
        let p = Arc::clone(&player);
        let s = Arc::clone(&spotify);
        let q = Arc::clone(&playback_queue);
        let ui_handle = ui_handle.clone();
        move |track_id: String| {
            println!("[player] play_id called for track: {}", track_id);
            let p = Arc::clone(&p);
            let s = Arc::clone(&s);
            let q = Arc::clone(&q);
            let ui_c = ui_handle.clone();

            let p_play = Arc::clone(&p);
            let t_id_play = track_id.clone();
            tokio::spawn(async move {
                println!("[player] librespot: Loading track {}", t_id_play);
                let p_lock = p_play.lock().await;
                if let Some(ref active_player) = *p_lock {
                    active_player.play(&t_id_play).await;
                    println!("[player] librespot: Play command sent.");
                }
            });

            let ui_active = ui_c.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_active.upgrade() {
                    ui.set_player_is_playing(false);
                    ui.set_player_progress_percent(0.0);
                    ui.set_player_current_time_str("0:00".into());
                    ui.set_player_total_time_str("--:--".into());
                }
            });

            let s_meta = Arc::clone(&s);
            let t_id_meta = track_id.clone();
            let ui_meta = ui_c.clone();
            let ui_handle_outer = ui_handle.clone();
            // 1. Fetch metadata in parallel
            let s_info = Arc::clone(&s_meta);
            let t_id_info = t_id_meta.clone();
            let ui_info = ui_meta.clone();
            let ui_handle_outer_clone = ui_handle_outer.clone();
            let p_meta = Arc::clone(&p);
            tokio::spawn(async move {
                let provider = &*s_info;
                if let Ok(track) = provider.get_track(&t_id_info).await {
                    let dur_ms = crate::providers::spotify_player::parse_duration_str_to_ms(&track.duration_str);
                    if let Some(ref active_player) = *p_meta.lock().await {
                        *active_player.duration_ms.lock().await = dur_ms;
                    }
        
                    let t_title = track.title.clone();
                    let t_artist = track.artist.clone();
                    let album_url = track.album_url.clone();
                    
                    let ui_text = ui_info.clone();
                    let t_id_clone = track.id.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_text.upgrade() {
                            ui.set_player_track_title(t_title.into());
                            ui.set_player_track_artist(t_artist.into());
                            ui.set_player_track_id(t_id_clone.into());
                        }
                    });

                    if !album_url.is_empty() {
                        let ui_img = ui_handle_outer_clone.clone();
                        let t_id = track.id.clone();
                        let t_url = track.album_url.clone();

                        tokio::spawn(async move {
                            if let Some(buf) = utils::fetch_and_cache_image(&t_id, &t_url).await {
                                // Offload CPU-bound colour analysis + gradient build off the tokio runtime.
                                // Move `buf` into the blocking task; clone the SharedPixelBuffer
                                // (Arc-only, O(1)) for the UI closure so no pixel data is copied.
                                let buf_for_ui = buf.clone();
                                let gradient = tokio::task::spawn_blocking(move || {
                                    let (h, s, v) = utils::extract_dominant_hsv(&buf);
                                    utils::build_album_gradient_image(h, s, v)
                                })
                                .await
                                .ok();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_img.upgrade() {
                                        ui.set_player_album_cover(slint::Image::from_rgba8(buf_for_ui));
                                        if let Some(gradient) = gradient {
                                            ui.set_player_bg_image(slint::Image::from_rgba8(gradient));
                                        }
                                    }
                                });
                            }
                        });
                    }
                }
            });

            // 2. Fetch liked/saved status in parallel (completely separate thread!)
            let s_check = Arc::clone(&s_meta);
            let t_id_check = t_id_meta.clone();
            let ui_check = ui_meta.clone();
            tokio::spawn(async move {
                // Initialize clean UI state immediately so we don't carry over checkmarks from previous song!
                let ui_init = ui_check.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_init.upgrade() {
                        ui.set_player_is_liked(false);
                        ui.set_player_is_saved(false);
                        
                        let playlists_model = ui.get_spotify_user_playlists();
                        for i in 0..playlists_model.row_count() {
                            if let Some(mut playlist) = playlists_model.row_data(i) {
                                playlist.artist_id = "".into();
                                playlists_model.set_row_data(i, playlist);
                            }
                        }
                    }
                });

                let provider_lock = &*s_check;
                let is_liked = provider_lock.check_track_liked(&t_id_check).await.unwrap_or(false);
                let containing_playlists = provider_lock.check_playlists_containing_track(&t_id_check).await.unwrap_or_default();

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_check.upgrade() {
                        ui.set_player_is_liked(is_liked);
                        
                        let is_saved = is_liked || !containing_playlists.is_empty();
                        ui.set_player_is_saved(is_saved);

                        let playlists_model = ui.get_spotify_user_playlists();
                        for i in 0..playlists_model.row_count() {
                            if let Some(mut playlist) = playlists_model.row_data(i) {
                                let in_playlist = containing_playlists.contains(&playlist.id.to_string());
                                playlist.artist_id = if in_playlist { "present".into() } else { "".into() };
                                playlists_model.set_row_data(i, playlist);
                            }
                        }
                    }
                });
            });

            let q_next = Arc::clone(&q);
            let s_next = Arc::clone(&s);
            let p_next = Arc::clone(&p);
            tokio::spawn(async move {
                // Let the current song completely load and start playing before preloading the next one
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

                // 1. Check if we need to fetch recommendations from Last.fm
                let (need_recommendations, seed_title, seed_artist, _current) = {
                    let q_lock = q_next.lock().await;
                    let len = q_lock.tracks.len();
                    let current = q_lock.current_index;
                    
                    let mut need_recommendations = false;
                    let mut seed_title = String::new();
                    let mut seed_artist = String::new();
                    
                    if current + 2 >= len {
                        // We are at the last or next-to-last song. Fetch recommendations based on the last song in the queue to continue playback seamlessly!
                        if len > 0 {
                            let last_track = &q_lock.tracks[len - 1];
                            seed_title = last_track.title.clone();
                            seed_artist = last_track.artist.clone();
                            need_recommendations = true;
                        }
                    }
                    
                    (need_recommendations, seed_title, seed_artist, current)
                };
                
                if need_recommendations && !seed_title.is_empty() {
                    println!("[player] Next song is last/none. Auto-prefetching recommendations based on '{}' by '{}'...", seed_title, seed_artist);
                    let provider = &*s_next;
                    match provider.get_recommendations(&seed_title, &seed_artist).await {
                        Ok(recs) => {
                            if !recs.is_empty() {
                                let processed = process_tracks(recs).await;
                                let mut q_lock = q_next.lock().await;
                                let prev_len = q_lock.tracks.len();
                                q_lock.tracks.extend(processed);
                                println!("[player] Auto-prefetched and appended {} recommendations.", q_lock.tracks.len() - prev_len);
                            } else {
                                println!("[player] Last.fm returned 0 recommended tracks.");
                            }
                        }
                        Err(e) => {
                            println!("[player] Last.fm auto-prefetch failed: {}", e);
                        }
                    }
                }

                // 2. Preload the next track if one exists now
                let next_info: Option<(String, String)> = {
                    let q_lock = q_next.lock().await;
                    if q_lock.current_index + 1 < q_lock.tracks.len() {
                        let t = &q_lock.tracks[q_lock.current_index + 1];
                        Some((t.id.clone(), t.title.clone()))
                    } else { None }
                };
                
                if let Some((nid, _)) = next_info {
                    println!("[player] Preloading audio for next track: {}", nid);
                    let p_lock = p_next.lock().await;
                    if let Some(ref active_player) = *p_lock {
                        active_player.preload(&nid).await;
                    }

                    let provider = &*s_next;
                    if let Ok(track) = provider.get_track(&nid).await {
                        let image_url = track.album_url.clone();
            

                        if !image_url.is_empty() {
                            println!("[player] Pre-fetching album art for next track...");
                            utils::fetch_and_cache_image(&nid, &image_url).await;
                        }
                    }
                }
            });
        }
    };

    ui.on_play_track({
        let q = Arc::clone(&playback_queue);
        let s = Arc::clone(&spotify);
        let ui_handle = ui_handle.clone();
        let play_fn = play_id.clone();
        let ast = Arc::clone(&active_source_tracks);
        move |id| {
            let q = Arc::clone(&q);
            let s = Arc::clone(&s);
            let ui_c = ui_handle.clone();
            let track_id: String = id.to_string();
            let play_fn = play_fn.clone();
            let ast_thread = Arc::clone(&ast);

            let mut captured_view = "unknown".to_string();
            if let Some(ui) = ui_c.upgrade() {
                captured_view = ui.get_active_view().to_string();
            }

            tokio::spawn(async move {
                let mut tracks_to_set = Vec::new();
                let mut index_to_set = 0;

                // Asynchronously build the queue in the background using active_source_tracks
                {
                    let source_tracks = ast_thread.lock().await;
                    for (i, t) in source_tracks.iter().enumerate() {
                        if t.id == track_id {
                            index_to_set = i;
                        }
                        tracks_to_set.push(TrackItem {
                            title: t.title.clone(),
                            artist: t.artist.clone(),
                            artist_id: t.artist_id.clone(),
                            id: t.id.clone(),
                            duration: t.duration_str.clone(),
                            image_buffer: None,
                        });
                    }
                }

                if tracks_to_set.is_empty() {
                    println!("[player] on_play_track: No context found in '{}'. Creating single-item queue for track: {}", captured_view, track_id);
                    let provider = &*s;
                    if let Ok(t) = provider.get_track(&track_id).await {
                        tracks_to_set.push(TrackItem {
                            title: t.title,
                            artist: t.artist,
                            artist_id: t.artist_id,
                            id: t.id,
                            duration: t.duration_str,
                            image_buffer: None,
                        });
                        index_to_set = 0;
                    }
                } else {
                    println!("[player] on_play_track: Captured queue from '{}'. Size: {}. Start Index: {}", captured_view, tracks_to_set.len(), index_to_set);
                }

                if !tracks_to_set.is_empty() {
                    let mut q_lock = q.lock().await;
                    q_lock.tracks = tracks_to_set;
                    q_lock.current_index = index_to_set;
                }
                
                play_fn(track_id.clone());
            });
        }
    });

    // --- TOGGLE PLAY/PAUSE ---
    let player_toggle = Arc::clone(&player);
    let ui_toggle = ui_handle.clone();
    ui.on_player_toggle_play(move || {
        let p = Arc::clone(&player_toggle);
        let ui_c = ui_toggle.clone();
        tokio::spawn(async move {
            let p_lock = p.lock().await;
            if let Some(ref active_player) = *p_lock {
                let currently_playing = active_player.get_is_playing().await;
                if currently_playing {
                    active_player.pause().await;
                } else {
                    active_player.resume().await;
                }
                let new_state = !currently_playing;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_c.upgrade() {
                        ui.set_player_is_playing(new_state);
                    }
                });
            }
        });
    });

    // --- SEEK ---
    let player_seek = Arc::clone(&player);
    let ui_seek = ui_handle.clone();
    ui.on_player_seek(move |fraction| {
        let p = Arc::clone(&player_seek);
        let ui_c = ui_seek.clone();
        tokio::spawn(async move {
            let p_lock = p.lock().await;
            if let Some(ref active_player) = *p_lock {
                let dur = *active_player.duration_ms.lock().await;
                if dur > 0 {
                    let pos_ms = (fraction * dur as f32) as u32;
                    active_player.seek(pos_ms).await;
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_c.upgrade() {
                            ui.set_player_progress_percent(fraction);
                            let secs = (pos_ms + 500) / 1000;
                            ui.set_player_current_time_str(format!("{}:{:02}", secs / 60, secs % 60).into());
                        }
                    });
                }
            }
        });
    });
    
    // --- VOLUME ---
    let player_vol = Arc::clone(&player);
    ui.on_player_change_volume(move |vol| {
        let p = Arc::clone(&player_vol);
        tokio::spawn(async move {
            let p_lock = p.lock().await;
            if let Some(ref active_player) = *p_lock {
                active_player.set_volume(vol).await;
            }
        });
    });

    // --- ADD TO LIKED SONGS ---
    let spotify_liked = Arc::clone(&spotify);
    let ui_liked = ui_handle.clone();
    ui.on_player_add_to_liked_songs(move |track_id| {
        let spotify_thread = Arc::clone(&spotify_liked);
        let tid = track_id.to_string();
        let ui_thread = ui_liked.clone();
        tokio::spawn(async move {
            println!("[main] Adding track {} to Liked Songs...", tid);
            let provider = &*spotify_thread;
            match provider.add_track_to_liked_songs(&tid).await {
                Ok(()) => {
                    println!("[main] Successfully added track {} to Liked Songs!", tid);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_thread.upgrade() {
                            ui.set_player_is_liked(true);
                            ui.set_player_is_saved(true);
                        }
                    });
                }
                Err(e) => {
                    println!("[main] Failed to add track {} to Liked Songs: {}", tid, e);
                }
            }
        });
    });

    // --- ADD TO PLAYLIST ---
    let spotify_playlist_add = Arc::clone(&spotify);
    let ui_playlist = ui_handle.clone();
    ui.on_player_add_to_playlist(move |track_id, playlist_id| {
        let spotify_thread = Arc::clone(&spotify_playlist_add);
        let tid = track_id.to_string();
        let pid = playlist_id.to_string();
        let ui_thread = ui_playlist.clone();
        tokio::spawn(async move {
            println!("[main] Adding track {} to playlist {}...", tid, pid);
            let provider = &*spotify_thread;
            match provider.add_track_to_playlist(&tid, &pid).await {
                Ok(()) => {
                    println!("[main] Successfully added track {} to playlist {}!", tid, pid);
                    let pid_clone = pid.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_thread.upgrade() {
                            ui.set_player_is_saved(true);
                            
                            let playlists_model = ui.get_spotify_user_playlists();
                            for i in 0..playlists_model.row_count() {
                                if let Some(mut playlist) = playlists_model.row_data(i) {
                                    if playlist.id.to_string() == pid_clone {
                                        playlist.artist_id = "present".into();
                                        playlists_model.set_row_data(i, playlist);
                                        break;
                                    }
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    println!("[main] Failed to add track {} to playlist {}: {}", tid, pid, e);
                }
            }
        });
    });

    // --- REMOVE FROM LIKED SONGS ---
    let spotify_liked_remove = Arc::clone(&spotify);
    let ui_liked_remove = ui_handle.clone();
    ui.on_player_remove_from_liked_songs(move |track_id| {
        let spotify_thread = Arc::clone(&spotify_liked_remove);
        let tid = track_id.to_string();
        let ui_thread = ui_liked_remove.clone();
        tokio::spawn(async move {
            println!("[main] Removing track {} from Liked Songs...", tid);
            let provider = &*spotify_thread;
            match provider.remove_track_from_liked_songs(&tid).await {
                Ok(()) => {
                    println!("[main] Successfully removed track {} from Liked Songs!", tid);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_thread.upgrade() {
                            ui.set_player_is_liked(false);
                            ui.set_player_is_saved(false);
                        }
                    });
                }
                Err(e) => {
                    println!("[main] Failed to remove track {} from Liked Songs: {}", tid, e);
                }
            }
        });
    });

    // --- REMOVE FROM PLAYLIST ---
    let spotify_playlist_remove = Arc::clone(&spotify);
    let ui_playlist_remove = ui_handle.clone();
    ui.on_player_remove_from_playlist(move |track_id, playlist_id| {
        let spotify_thread = Arc::clone(&spotify_playlist_remove);
        let tid = track_id.to_string();
        let pid = playlist_id.to_string();
        let ui_thread = ui_playlist_remove.clone();
        tokio::spawn(async move {
            println!("[main] Removing track {} from playlist {}...", tid, pid);
            let provider = &*spotify_thread;
            match provider.remove_track_from_playlist(&tid, &pid).await {
                Ok(()) => {
                    println!("[main] Successfully removed track {} from playlist {}!", tid, pid);
                    let pid_clone = pid.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_thread.upgrade() {
                            let playlists_model = ui.get_spotify_user_playlists();
                            for i in 0..playlists_model.row_count() {
                                if let Some(mut playlist) = playlists_model.row_data(i) {
                                    if playlist.id.to_string() == pid_clone {
                                        playlist.artist_id = "".into();
                                        playlists_model.set_row_data(i, playlist);
                                        break;
                                    }
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    println!("[main] Failed to remove track {} from playlist {}: {}", tid, pid, e);
                }
            }
        });
    });

    // --- PLAY NEXT INTERNAL ---
    let go_next = {
        let q = Arc::clone(&playback_queue);
        let play_fn = play_id.clone();
        move || {
            println!("[player] go_next triggered.");
            let q = Arc::clone(&q);
            let play_fn = play_fn.clone();
            tokio::spawn(async move {
                let mut q_lock = q.lock().await;
                println!("[player] go_next: queue size = {}, current index = {}", q_lock.tracks.len(), q_lock.current_index);
                if q_lock.tracks.is_empty() { 
                    println!("[player] go_next: queue is empty, stopping.");
                    return; 
                }
                
                if q_lock.current_index + 1 >= q_lock.tracks.len() {
                    println!("[player] End of queue reached. Looping back to first track...");
                    q_lock.current_index = 0;
                    let next_id = q_lock.tracks[0].id.clone();
                    drop(q_lock);
                    play_fn(next_id);
                } else {
                    q_lock.current_index += 1;
                    let next_track_id = q_lock.tracks[q_lock.current_index].id.clone();
                    drop(q_lock);
                    play_fn(next_track_id);
                }
            });
        }
    };

    ui.on_player_next_track({
        let go_next = go_next.clone();
        move || { go_next(); }
    });

    ui.on_player_prev_track({
        let q = Arc::clone(&playback_queue);
        let play_fn = play_id.clone();
        move || {
            let q = Arc::clone(&q);
            let play_fn = play_fn.clone();
            tokio::spawn(async move {
                let mut q_lock = q.lock().await;
                if q_lock.tracks.is_empty() { return; }

                if q_lock.current_index > 0 {
                    q_lock.current_index -= 1;
                } else {
                    q_lock.current_index = q_lock.tracks.len() - 1;
                }
                
                let prev_id = q_lock.tracks[q_lock.current_index].id.clone();
                drop(q_lock);
                play_fn(prev_id);
            });
        }
    });

    // --- SETUP CANCEL PAIRING CALLBACK ---
    ui.on_spotify_cancel_pairing({
        let ui_cancel = ui.as_weak();
        move || {
            if let Some(ui) = ui_cancel.upgrade() {
                ui.set_active_view("home".into());
            }
        }
    });

    // --- OOBE: GET STARTED (welcome → wifi setup) ---
    ui.on_oobe_get_started({
        let ui_h = ui_handle.clone();
        let sp = Arc::clone(&spotify);
        let pl = Arc::clone(&player);
        let gn = go_next.clone();
        move || {
            let ui_h = ui_h.clone();
            let sp = Arc::clone(&sp);
            let pl = Arc::clone(&pl);
            let gn = gn.clone();
            tokio::spawn(async move {
                start_oobe_wifi(ui_h, sp, pl, gn).await;
            });
        }
    });

    // --- OOBE: SKIP WIFI (go straight to Spotify pairing) ---
    ui.on_oobe_skip_wifi({
        let ui_h = ui_handle.clone();
        let sp = Arc::clone(&spotify);
        let pl = Arc::clone(&player);
        let gn = go_next.clone();
        move || {
            let ui_h = ui_h.clone();
            let sp = Arc::clone(&sp);
            let pl = Arc::clone(&pl);
            let gn = gn.clone();
            tokio::spawn(async move {
                oobe_stop_hotspot();
                start_pairing_flow(ui_h, sp, pl, gn);
            });
        }
    });

    // --- SPOTIFY ACCOUNT MANAGEMENT CALLBACKS ---
    
    // Select/switch account callback
    let spotify_select = Arc::clone(&spotify);
    let player_select = Arc::clone(&player);
    let go_next_select = go_next.clone();
    let ui_select = ui_handle.clone();
    ui.on_spotify_select_account(move |name| {
        let spotify = Arc::clone(&spotify_select);
        let player = Arc::clone(&player_select);
        let go_next = go_next_select.clone();
        let ui = ui_select.clone();
        let name_str = name.to_string();

        tokio::spawn(async move {
            let already_active = {
                let provider = &*spotify;
                provider.auth.active_user.lock().await.as_ref() == Some(&name_str)
            };

            if already_active {
                println!("[main] Account '{}' is already active. Navigating directly to dashboard.", name_str);
                let ui_clone = ui.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_clone.upgrade() {
                        ui.set_active_view("spotify-dashboard".into());
                    }
                });
                return;
            }

            println!("[main] Switching active Spotify account to: {}", name_str);
            // 1. Set active_user in multi-user cache
            {
                let mut cache = crate::providers::spotify::auth::SpotifyAuthManager::load_multi_cache();
                cache.active_user = Some(name_str.clone());
                crate::providers::spotify::auth::SpotifyAuthManager::save_multi_cache(&cache);
            }
            
            // 2. Set active_user in memory
            {
                let provider = &*spotify;
                *provider.auth.active_user.lock().await = Some(name_str.clone());
            }

            // 3. Verify session and run startup
            let is_valid = {
                let provider = &*spotify;
                provider.auth.is_session_valid().await
            };

            if is_valid {
                run_authenticated_startup(ui, spotify, player, go_next, true).await;
            } else {
                println!("[main] Failed to switch/authenticate for user: {}", name_str);
            }
        });
    });

    // Logout account callback
    let spotify_logout = Arc::clone(&spotify);
    let player_logout = Arc::clone(&player);
    let ui_logout = ui_handle.clone();
    ui.on_spotify_logout_account(move |name| {
        let spotify = Arc::clone(&spotify_logout);
        let player = Arc::clone(&player_logout);
        let ui = ui_logout.clone();
        let name_str = name.to_string();

        tokio::spawn(async move {
            println!("[main] Logging out Spotify account: {}", name_str);
            
            // 1. Load cache and remove user
            let mut cache = crate::providers::spotify::auth::SpotifyAuthManager::load_multi_cache();
            cache.users.remove(&name_str);
            
            let is_current = cache.active_user.as_ref() == Some(&name_str);
            if is_current {
                // Clear active user since they are logging out
                cache.active_user = None;
                
                // Pause and clear in-memory player
                let mut p_lock = player.lock().await;
                if let Some(ref active_player) = *p_lock {
                    active_player.pause().await;
                }
                *p_lock = None;
                
                // Clear in-memory token in auth client
                let provider = &*spotify;
                let client = provider.auth.get_client().await;
                let token_arc = client.lock().await.get_token();
                if let Ok(mut token_guard) = token_arc.lock().await {
                    *token_guard = None;
                }
                *provider.auth.active_user.lock().await = None;
            }
            
            // If there are other connected users, set one as active
            if cache.active_user.is_none() && !cache.users.is_empty() {
                if let Some(first_user) = cache.users.keys().next() {
                    cache.active_user = Some(first_user.clone());
                    println!("[main] Automatically switched active user to: {}", first_user);
                }
            }
            
            // Save modified cache
            crate::providers::spotify::auth::SpotifyAuthManager::save_multi_cache(&cache);

            // 2. Transition view
            let ui_clone = ui.clone();
            let has_remaining_users = !cache.users.is_empty();
            let active_user_opt = cache.active_user.clone();
            
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui_win) = ui_clone.upgrade() {
                    if has_remaining_users {
                        // Stay on accounts screen and refresh accounts list
                        refresh_accounts_view(ui_clone.clone());
                        if is_current {
                            if let Some(ref next_user) = active_user_opt {
                                ui_win.set_spotify_user_name(next_user.clone().into());
                            }
                        }
                    } else {
                        // No remaining users, return to home view
                        ui_win.set_active_view("home".into());
                        ui_win.set_player_track_title("".into()); // Hide player bar
                    }
                }
            });
        });
    });

    // Add account callback
    let spotify_add = Arc::clone(&spotify);
    let player_add = Arc::clone(&player);
    let go_next_add = go_next.clone();
    let ui_add = ui_handle.clone();
    ui.on_spotify_add_account(move || {
        start_pairing_flow(ui_add.clone(), spotify_add.clone(), player_add.clone(), go_next_add.clone());
    });

    // Switch account view callback
    let ui_switch = ui_handle.clone();
    ui.on_spotify_switch_account(move || {
        if let Some(ui_win) = ui_switch.upgrade() {
            ui_win.set_active_view("spotify-accounts".into());
        }
        refresh_accounts_view(ui_switch.clone());
    });

    // --- AUTHENTICATION STARTUP CHECK ---
    let init_ui = ui.as_weak();
    let init_spotify = Arc::clone(&spotify);
    let init_player = Arc::clone(&player);
    let go_next_clone = go_next.clone();

    tokio::spawn(async move {
        if is_first_run() {
            println!("[main] First run detected — launching OOBE.");
            let _ = slint::invoke_from_event_loop({
                let h = init_ui.clone();
                move || {
                    if let Some(ui) = h.upgrade() {
                        ui.set_active_view("oobe-welcome".into());
                    }
                }
            });
            return;
        }

        let is_valid = {
            let provider = &*init_spotify;
            provider.auth.is_session_valid().await
        };

        if is_valid {
            run_authenticated_startup(init_ui, init_spotify, init_player, go_next_clone, false).await;
        } else {
            println!("[main] Session is invalid/empty. User is on home/landing screen.");
        }
    });

    // --- OPEN SPOTIFY PROVIDER CALLBACK ---
    let ui_handle_provider = ui_handle.clone();
    let spotify_provider_clone = Arc::clone(&spotify);
    let player_provider_clone = Arc::clone(&player);
    let go_next_provider_clone = go_next.clone();

    ui.on_open_spotify_provider(move || {
        let ui_handle = ui_handle_provider.clone();
        let spotify = Arc::clone(&spotify_provider_clone);
        let player = Arc::clone(&player_provider_clone);
        let go_next = go_next_provider_clone.clone();

        tokio::spawn(async move {
            let cache = crate::providers::spotify::auth::SpotifyAuthManager::load_multi_cache();
            if cache.users.is_empty() {
                start_pairing_flow(ui_handle.clone(), spotify.clone(), player.clone(), go_next.clone());
            } else {
                // Check if we already have an active authenticated session
                let active_opt = {
                    let provider = &*spotify;
                    provider.auth.active_user.lock().await.clone()
                };

                if let Some(active_user) = active_opt {
                    println!("[main] Session already authenticated for user '{}'. Navigating directly to dashboard.", active_user);
                    let ui_clone = ui_handle.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_clone.upgrade() {
                            ui.set_active_view("spotify-dashboard".into());
                        }
                    });
                } else {
                    println!("[main] Accounts exist. Navigating to Accounts selection view.");
                    // Populate accounts view and transition to accounts
                    let ui_clone = ui_handle.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_clone.upgrade() {
                            ui.set_active_view("spotify-accounts".into());
                        }
                    });
                    refresh_accounts_view(ui_handle);
                }
            }
        });
    });

    ui.run()
}