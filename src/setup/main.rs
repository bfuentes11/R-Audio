use std::net::ToSocketAddrs;
use std::thread;
use std::time::Duration;

slint::include_modules!();

// Check if we can resolve Spotify API or standard hostname
fn check_internet() -> bool {
    match "api.spotify.com:80".to_socket_addrs() {
        Ok(mut addrs) => addrs.next().is_some(),
        Err(_) => false,
    }
}

// Start NetworkManager Hotspot
fn start_hotspot() -> Result<(), String> {
    if cfg!(target_os = "linux") {
        let status = std::process::Command::new("sudo")
            .args(&["nmcli", "device", "wifi", "hotspot", "ssid", "R-Audio-Setup"])
            .status()
            .map_err(|e| format!("Failed to execute nmcli hotspot: {}", e))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("nmcli hotspot returned status code {:?}", status.code()))
        }
    } else {
        println!("[wifi] Simulating hotspot start (R-Audio-Setup) on non-Linux platform.");
        Ok(())
    }
}

// Stop Hotspot connection
fn stop_hotspot() -> Result<(), String> {
    if cfg!(target_os = "linux") {
        let status = std::process::Command::new("sudo")
            .args(&["nmcli", "connection", "down", "Hotspot"])
            .status()
            .map_err(|e| format!("Failed to execute nmcli down: {}", e))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("nmcli down returned status code {:?}", status.code()))
        }
    } else {
        println!("[wifi] Simulating hotspot connection shutdown on non-Linux platform.");
        Ok(())
    }
}

// Connect to Client Wi-Fi
fn connect_to_wifi(ssid: &str, password: &str) -> Result<(), String> {
    if cfg!(target_os = "linux") {
        let mut args = vec!["nmcli", "device", "wifi", "connect", ssid];
        if !password.is_empty() {
            args.push("password");
            args.push(password);
        }
        let status = std::process::Command::new("sudo")
            .args(&args)
            .status()
            .map_err(|e| format!("Failed to execute nmcli connect: {}", e))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("nmcli connect failed with status code {:?}", status.code()))
        }
    } else {
        println!("[wifi] Simulating Wi-Fi connection to SSID: '{}', Password: '{}'", ssid, password);
        thread::sleep(Duration::from_secs(2));
        Ok(())
    }
}

// Scan visible networks
fn scan_wifi() -> Vec<(String, String)> {
    if cfg!(target_os = "linux") {
        let output = match std::process::Command::new("sudo")
            .args(&["nmcli", "-t", "-f", "SSID,SIGNAL,SECURITY", "device", "wifi", "list"])
            .output()
        {
            Ok(out) => out,
            Err(_) => return vec![],
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut networks = vec![];
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                let ssid = parts[0].trim().to_string();
                let signal = parts[1].trim().to_string();
                if !ssid.is_empty() && !networks.iter().any(|(s, _)| s == &ssid) {
                    networks.push((ssid, signal));
                }
            }
        }
        networks
    } else {
        vec![
            ("Home-WiFi-5G".to_string(), "95".to_string()),
            ("CoffeeShop_Guest".to_string(), "82".to_string()),
            ("Kiosk_Internal".to_string(), "64".to_string()),
        ]
    }
}

// Parse urlencoded POST body
fn parse_form(body: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for pair in body.split('&') {
        let mut parts = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
            let key = urlencoding::decode(k).unwrap_or_else(|_| std::borrow::Cow::Borrowed(k)).into_owned();
            let val = urlencoding::decode(v).unwrap_or_else(|_| std::borrow::Cow::Borrowed(v)).into_owned();
            map.insert(key, val);
        }
    }
    map
}

fn generate_qr_code(url: &str) -> slint::Image {
    match qrcode_generator::to_png_to_vec(url, qrcode_generator::QrCodeEcc::Low, 250) {
        Ok(png_bytes) => {
            if let Ok(img) = image::load_from_memory(&png_bytes) {
                let buffer = img.to_rgba8();
                slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
                    buffer.as_raw(),
                    buffer.width(),
                    buffer.height(),
                ))
            } else {
                slint::Image::default()
            }
        }
        Err(_) => slint::Image::default(),
    }
}

#[tokio::main]
async fn main() -> Result<(), slint::PlatformError> {
    println!("Booting R-Audio Wi-Fi Setup Wizard...");

    // 1. Initial Internet check
    if check_internet() {
        println!("[setup] Internet connection already established. Exiting setup wizard...");
        std::process::exit(0);
    }

    let ui = SetupWindow::new()?;
    let ui_handle = ui.as_weak();

    // 2. Generate QR Code
    let setup_url = "http://10.42.0.1:8888/wifi";
    let qr_image = generate_qr_code(setup_url);
    ui.set_wifi_setup_qr_code(qr_image);
    ui.set_wifi_setup_ssid("R-Audio-Setup".into());
    ui.set_wifi_setup_url(setup_url.into());
    ui.set_wifi_setup_status("Initializing Hotspot...".into());

    // 3. Skip callback
    ui.on_wifi_skip_setup({
        let ui_handle = ui_handle.clone();
        move || {
            println!("[setup] User skipped network configuration. Exiting...");
            let _ = stop_hotspot();
            if let Some(ui) = ui_handle.upgrade() {
                ui.hide().unwrap();
            }
            std::process::exit(0);
        }
    });

    // Fullscreen callback
    ui.on_toggle_fullscreen({
        let ui_handle = ui_handle.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                let is_fs = ui.window().is_fullscreen();
                ui.window().set_fullscreen(!is_fs);
            }
        }
    });

    // Start Hostspot
    if let Err(e) = start_hotspot() {
        eprintln!("[setup] Failed to start Wi-Fi Hotspot: {}", e);
        ui.set_wifi_setup_status(format!("Hotspot Error: {}", e).into());
    } else {
        ui.set_wifi_setup_status("Hotspot active. Connect and scan QR.".into());
    }

    // 4. Communication channel between HTTP Server and Main Event Loop
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, String)>(1);

    // 5. Spin up tiny_http background server
    thread::spawn(move || {
        let server = match tiny_http::Server::http("0.0.0.0:8888") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[setup] HTTP Server failed to bind: {}", e);
                return;
            }
        };

        println!("[setup] HTTP Server listening on http://0.0.0.0:8888");

        for mut request in server.incoming_requests() {
            let url = request.url().to_string();
            
            if url == "/wifi" || url == "/" {
                let networks = scan_wifi();
                let mut options = String::new();
                for (ssid, signal) in networks {
                    let escaped_ssid = ssid.replace("\"", "&quot;").replace("<", "&lt;").replace(">", "&gt;");
                    options.push_str(&format!(
                        "<option value=\"{}\">{} ({}% signal)</option>\n",
                        escaped_ssid, escaped_ssid, signal
                    ));
                }

                let html = format!(r#"<!DOCTYPE html>
<html>
<head>
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>R-Audio Wi-Fi Setup</title>
<style>
body {{
    background: linear-gradient(135deg, #102a4e 0%, #0a182d 100%);
    color: #ffffff;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    margin: 0;
    padding: 20px;
    display: flex;
    justify-content: center;
    align-items: center;
    min-height: 100vh;
    box-sizing: border-box;
}}
.card {{
    background: rgba(255, 255, 255, 0.07);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 16px;
    padding: 30px;
    width: 100%;
    max-width: 400px;
    box-shadow: 0 8px 32px 0 rgba(0, 0, 0, 0.37);
    backdrop-filter: blur(8px);
    -webkit-backdrop-filter: blur(8px);
}}
h2 {{
    margin-top: 0;
    font-weight: 800;
    color: #4b9beb;
    letter-spacing: 1px;
}}
p {{
    color: rgba(255, 255, 255, 0.7);
    font-size: 14px;
    line-height: 1.5;
}}
.form-group {{
    margin-bottom: 20px;
}}
label {{
    display: block;
    margin-bottom: 8px;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 1px;
    color: rgba(255, 255, 255, 0.5);
}}
select, input {{
    width: 100%;
    padding: 12px;
    border-radius: 8px;
    border: 1px solid rgba(255, 255, 255, 0.15);
    background: rgba(0, 0, 0, 0.2);
    color: white;
    font-size: 16px;
    box-sizing: border-box;
}}
select:focus, input:focus {{
    outline: none;
    border-color: #4b9beb;
}}
button {{
    width: 100%;
    padding: 14px;
    border: none;
    border-radius: 8px;
    background: #2b7bc5;
    color: white;
    font-weight: 700;
    font-size: 16px;
    cursor: pointer;
    transition: background 0.2s;
}}
button:hover {{
    background: #4b9beb;
}}
</style>
</head>
<body>
<div class="card">
    <h2>R-Audio</h2>
    <p>Select your home Wi-Fi network and enter the password to connect the kiosk.</p>
    <form action="/connect" method="POST">
        <div class="form-group">
            <label for="ssid">WI-FI NETWORK</label>
            <select name="ssid" id="ssid" required>
                {}
            </select>
        </div>
        <div class="form-group">
            <label for="password">PASSWORD</label>
            <input type="password" name="password" id="password" placeholder="Enter network password">
        </div>
        <button type="submit">Connect Kiosk</button>
    </form>
</div>
</body>
</html>"#, options);

                let response = tiny_http::Response::from_string(html)
                    .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap());
                let _ = request.respond(response);

            } else if request.method() == &tiny_http::Method::Post && url == "/connect" {
                let mut body = String::new();
                if request.as_reader().read_to_string(&mut body).is_ok() {
                    let params = parse_form(&body);
                    let ssid = params.get("ssid").cloned().unwrap_or_default();
                    let password = params.get("password").cloned().unwrap_or_default();

                    println!("[setup] Received connection request for SSID: {}", ssid);

                    // Send HTML response back immediately before deactivating the hotspot interface
                    let html = r#"<!DOCTYPE html>
<html>
<head>
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Connecting...</title>
<style>
body {
    background: linear-gradient(135deg, #102a4e 0%, #0a182d 100%);
    color: #ffffff;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    margin: 0;
    padding: 20px;
    display: flex;
    justify-content: center;
    align-items: center;
    min-height: 100vh;
    box-sizing: border-box;
}
.card {
    background: rgba(255, 255, 255, 0.07);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 16px;
    padding: 30px;
    width: 100%;
    max-width: 400px;
    text-align: center;
    box-shadow: 0 8px 32px 0 rgba(0, 0, 0, 0.37);
    backdrop-filter: blur(8px);
    -webkit-backdrop-filter: blur(8px);
}
h2 {
    margin-top: 0;
    color: #4b9beb;
}
.spinner {
    width: 50px;
    height: 50px;
    border: 3px solid rgba(255,255,255,0.1);
    border-radius: 50%;
    border-top-color: #4b9beb;
    animation: spin 1s ease-in-out infinite;
    margin: 30px auto;
}
@keyframes spin {
    to { transform: rotate(360deg); }
}
</style>
</head>
<body>
<div class="card">
    <h2>Connecting...</h2>
    <p>The kiosk is attempting to connect. The setup hotspot will close. You can close this tab and check the kiosk screen.</p>
    <div class="spinner"></div>
</div>
</body>
</html>"#;

                    let response = tiny_http::Response::from_string(html)
                        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap());
                    let _ = request.respond(response);

                    // Send parameters to the orchestrator channel
                    let _ = tx.blocking_send((ssid, password));
                }
            } else {
                let response = tiny_http::Response::from_string("Not Found")
                    .with_status_code(404);
                let _ = request.respond(response);
            }
        }
    });

    // 6. Monitor channels and events in an async context
    let orchestrator_ui = ui_handle.clone();
    tokio::spawn(async move {
        while let Some((ssid, password)) = rx.recv().await {
            println!("[setup] Initiating connection flow on main thread...");
            
            let _ = slint::invoke_from_event_loop({
                let ui = orchestrator_ui.clone();
                let ssid = ssid.clone();
                move || {
                    if let Some(ui_win) = ui.upgrade() {
                        ui_win.set_wifi_setup_status(format!("Connecting to {}...", ssid).into());
                    }
                }
            });

            // A: Deactivate Hotspot
            println!("[setup] Deactivating Setup Hotspot...");
            let _ = stop_hotspot();

            // B: Attempt Wi-Fi Connection
            println!("[setup] Connecting to SSID: {}...", ssid);
            match connect_to_wifi(&ssid, &password) {
                Ok(_) => {
                    // Check if connectivity is really established
                    thread::sleep(Duration::from_secs(2));
                    if check_internet() {
                        println!("[setup] Internet check succeeded! Network configured successfully.");
                        let _ = slint::invoke_from_event_loop({
                            let ui = orchestrator_ui.clone();
                            move || {
                                if let Some(ui_win) = ui.upgrade() {
                                    ui_win.set_wifi_setup_status("Connected! Loading R-Audio...".into());
                                }
                            }
                        });
                        thread::sleep(Duration::from_secs(2));
                        let _ = slint::invoke_from_event_loop({
                            let ui = orchestrator_ui.clone();
                            move || {
                                if let Some(ui_win) = ui.upgrade() {
                                    ui_win.hide().unwrap();
                                }
                            }
                        });
                        std::process::exit(0);
                    } else {
                        println!("[setup] Connected to AP but failed to reach internet.");
                    }
                }
                Err(e) => {
                    println!("[setup] Connection failed: {}", e);
                }
            }

            // Connection failed or no internet: Restart Hotspot
            println!("[setup] Connection failed or no internet. Restoring Setup Hotspot...");
            let _ = slint::invoke_from_event_loop({
                let ui = orchestrator_ui.clone();
                move || {
                    if let Some(ui_win) = ui.upgrade() {
                        ui_win.set_wifi_setup_status("Connection failed. Re-starting hotspot...".into());
                    }
                }
            });

            thread::sleep(Duration::from_secs(1));
            let _ = start_hotspot();

            let _ = slint::invoke_from_event_loop({
                let ui = orchestrator_ui.clone();
                move || {
                    if let Some(ui_win) = ui.upgrade() {
                        ui_win.set_wifi_setup_status("Hotspot active. Connect and scan QR.".into());
                    }
                }
            });
        }
    });

    // Run Slint Event Loop
    ui.run()?;

    // Post Event Loop Cleanup
    println!("[setup] Cleaning up network states...");
    let _ = stop_hotspot();

    Ok(())
}
