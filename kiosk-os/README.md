# R-Audio Debian Kiosk OS Build Guide

This directory contains the configurations and scripts required to package the `R-Audio` player into a standalone, custom, bootable Debian-based **Kiosk OS ISO** (`.iso`).

The target system boots in **under 10 seconds** directly into the glassmorphic player without heavy Desktop Environments (like GNOME or KDE), using bare X.org and a lightweight window manager (`openbox`).

---

## Technical Flow Chart

```
[System Power On] 
       │
       ▼
[systemd TTY1 Auto-Login] ──► logs in "kiosk" user
       │
       ▼
[startx -- -nocursor] ────► initializes bare Xorg display server
       │
       ▼
[~/.xinitrc] ─────────────► starts openbox & spawns r-audio fullscreen
       │
       ▼
[R-Audio Boot Check] ─────► checks if Spotify cache exists
       ├──► YES ──────────► dynamic token refresh ──► loads player dashboard
       └──► NO ───────────► starts tiny-http (8888) ──► displays Pairing QR
```

---

## Directory Structure

* [build-iso.sh](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/build-iso.sh) - Automated live-build shell script wrapper.
* [r-audio.service](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/r-audio.service) - systemd unit file that handles process monitoring and restarts.
* [r-audio.env.template](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/r-audio.env.template) - Template for credentials and system keys (maps to `/etc/default/r-audio`).
* [xinitrc](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/xinitrc) - Direct bare-metal X11 startup orchestrator.

---

## Step-by-Step ISO Build Instructions

### Prerequisites
You must run the build script on a **Debian or Ubuntu host** (or within a VM). Sudo/root permissions are required because it runs kernel `chroot` commands to mount and package the root squash filesystem.

### Step 1: Compile the R-Audio Release Binary
On your development machine, compile the R-Audio application in release mode:
```bash
cargo build --release
```
This optimizes Slint rendering, turns off compiler diagnostics, and produces a highly efficient bare-metal binary inside `target/release/R-Audio`.

### Step 2: Configure Environment Credentials
Open [r-audio.env.template](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/r-audio.env.template) and input your credentials, including the `LASTFM_API_KEY` for recommendations:
```bash
LASTFM_API_KEY=your_lastfm_key_here
```

### Step 3: Run the ISO Compiler
Navigate into the `kiosk-os/` folder on your build host and execute the script:
```bash
chmod +x build-iso.sh
sudo ./build-iso.sh
```
This script will:
1. Fetch and install building utilities (`live-build`, `xorriso`).
2. Download a base minimal Debian Bookworm image.
3. Install core kiosk libraries (Xorg, openbox, sound and network libraries).
4. Inject `R-Audio`, set up systemd auto-login, and compile the final bootable ISO.

Upon completion, you will find a hybrid, bootable ISO file:
📁 **`live-image-amd64.hybrid.iso`**

---

## Flashing & Booting the Kiosk OS

### Flashing to a USB Drive
You can flash this ISO to a USB flash drive using standard tools:
* **Windows**: Use [Rufus](https://rufus.ie/) (select "DD Image" mode if prompted) or [Ventoy](https://www.ventoy.net/).
* **Linux / macOS**: Use `dd`:
  ```bash
  sudo dd if=live-image-amd64.hybrid.iso of=/dev/sdX bs=4M status=progress && sync
  ```
  *(Replace `/dev/sdX` with the path to your USB drive).*

### Local VM Testing (QEMU)
To test the boot sequence and graphics compatibility directly on your development machine, use **QEMU**:
```bash
sudo apt install qemu-system-x86
qemu-system-x86_64 -enable-kvm -m 2G -cdrom live-image-amd64.hybrid.iso
```

---

## Seamless Kiosk Pairing Workflow

Once the Kiosk boots for the first time, it enters **Pairing Mode** automatically:
1. The screen displays a gorgeous high-contrast **QR Code** and instructions pointing to `http://r-audio.local:8888`.
2. Connect your phone or laptop to the **same Wi-Fi network** as the kiosk.
3. Scan the QR code or type `http://r-audio.local:8888` in your browser.
4. Log in to your Spotify account and authorize R-Audio.
5. The pairing server will handle the token cache swap, save it to persistent storage, and transition the kiosk screen to the **Music Dashboard** instantly!
