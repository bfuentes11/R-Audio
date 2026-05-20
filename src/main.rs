#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod providers;
mod utils;
mod spectrum_visualizer;

use providers::spotify::SpotifyProvider;
use providers::{AudioProvider, Track};
use std::sync::Arc;
use tokio::sync::Mutex;
use slint::Model;
use rspotify::prelude::*;

use crate::providers::spotify_player::LibrespotPlayer;

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
}

async fn process_tracks(
    tracks: Vec<Track>,
) -> Vec<TrackItem> {
    let mut join_set = tokio::task::JoinSet::new();

    for (index, track) in tracks.into_iter().enumerate() {
        let t_id = track.id.clone();
        let t_url = track.album_url.clone();
        
        join_set.spawn(async move {
            let image_buffer = utils::fetch_and_cache_image(&t_id, &t_url).await;
            (index, track, image_buffer)
        });
    }

    let mut results = Vec::new();
    while let Some(res) = join_set.join_next().await {
        if let Ok((index, track, image_buffer)) = res {
            results.push((index, track, image_buffer));
        }
    }

    // Sort by original index to guarantee perfect sequence ordering!
    results.sort_by_key(|(idx, _, _)| *idx);

    results.into_iter().map(|(_, track, image_buffer)| TrackItem {
        title: track.title,
        artist: track.artist,
        artist_id: track.artist_id,
        id: track.id,
        duration: track.duration_str,
        image_buffer,
    }).collect()
}

async fn initialize_audio_player(
    user_name: String,
    token: String,
    player_container: Arc<Mutex<Option<Arc<LibrespotPlayer>>>>,
    ui_handle: slint::Weak<MainWindow>,
    go_next: impl Fn() + Clone + Send + 'static,
) {
    println!("[player] Initializing bare-metal Audio Engine...");
    match LibrespotPlayer::new(&user_name, &token).await {
        Ok((audio_player, mut player_events)) => {
            let player_arc = Arc::new(audio_player);
            *player_container.lock().await = Some(Arc::clone(&player_arc));

            let ui_events = ui_handle.clone();
            let go_next_clone = go_next.clone();
            tokio::spawn(async move {
                while let Some(event) = player_events.recv().await {
                    let ui_ref = ui_events.clone();
                    let go_next_inner = go_next_clone.clone();
                    match event {
                        crate::providers::spotify_player::PlaybackEvent::Playing { position_ms, duration_ms } => {
                            println!("[main] Event: Playing {}/{}", position_ms, duration_ms);
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
                            go_next_inner();
                        }
                    }
                }
            });
        }
        Err(e) => {
            println!("[player] Failed to initialize Audio Engine: {}", e);
        }
    }
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
        initialize_audio_player(user_id, librespot_token, player_container.clone(), ui_handle.clone(), go_next).await;
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

#[tokio::main]
async fn main() -> Result<(), slint::PlatformError> {
    dotenvy::dotenv().ok();
    println!("Booting R-Audio Bare-Metal UI...");
    
    let ui = MainWindow::new()?;
    let spotify = Arc::new(SpotifyProvider::new());
    let player: Arc<Mutex<Option<Arc<LibrespotPlayer>>>> = Arc::new(Mutex::new(None));

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
                
                for (index, track) in tracks.into_iter().enumerate() {
                    let ui_handle_image = ui_handle_clone.clone();
                    let t_id = track.id.clone();
                    let track_album_url = track.album_url.clone();
                    
                    tokio::spawn(async move {
                        let image_buffer = utils::fetch_and_cache_image(&t_id, &track_album_url).await;
                        
                        if let Some(buf) = image_buffer {
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = ui_handle_image.upgrade() {
                                    let tracks_model = ui.get_spotify_album_tracks();
                                    if let Some(mut ui_track) = tracks_model.row_data(index) {
                                        ui_track.album_art = slint::Image::from_rgba8(buf);
                                        tracks_model.set_row_data(index, ui_track);
                                    }
                                }
                            });
                        }
                    });
                }

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

            let seed = spectrum_visualizer::get_spectrum_seed(&track_id);

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
            let seed_clone = seed;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_active.upgrade() {
                    ui.set_player_is_playing(false);
                    ui.set_player_progress_percent(0.0);
                    ui.set_player_current_time_str("0:00".into());
                    ui.set_player_total_time_str("--:--".into());
                    ui.set_player_spectrum_seed(seed_clone);
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
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_img.upgrade() {
                                        ui.set_player_album_cover(slint::Image::from_rgba8(buf));
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
                
                if let Some((nid, _nurl)) = next_info {
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