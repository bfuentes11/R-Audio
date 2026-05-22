# R-Audio Debian Kiosk OS Build Guide

This directory contains the configurations and scripts required to package the `R-Audio` player into a standalone, custom, bootable Debian-based **Kiosk OS ISO** (`.iso`).

The target system boots in **under 10 seconds** directly into the glassmorphic player without heavy Desktop Environments (like GNOME or KDE), using bare X.org and a lightweight window manager (`openbox`).

---

## Technical Flow Chart

### First Boot — Unattended Install (one-time, from USB)
```
[Boot USB] ──► [GRUB picks unattended entry (timeout 0, hidden)]
       │
       ▼
[Debian Installer reads /cdrom/preseed.cfg]
       │
       ▼
[Auto-detects first disk (eMMC on Surface Go 2) ──► WIPES + partitions]
       │
       ▼
[Installs base Debian + kiosk packages from deb.debian.org]
       │
       ▼
[late_command runs postinstall.sh ──► copies R-Audio + autologin override]
       │
       ▼
[d-i ejects USB and reboots into installed system]
```

### Every Boot After — Kiosk Startup (from internal disk)
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
* [preseed.cfg](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/preseed.cfg) - Debian Installer answer file for unattended install to internal disk.
* [postinstall.sh](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/postinstall.sh) - Runs from preseed's `late_command`; installs R-Audio binaries + autologin into the freshly installed target.
* [r-audio.service](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/r-audio.service) - systemd unit file that handles process monitoring and restarts.
* [r-audio.env.template](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/r-audio.env.template) - Template for credentials and system keys (maps to `/etc/default/r-audio`).
* [r-audio-launcher.sh](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/r-audio-launcher.sh) - X11 startup orchestrator that retries network before launching the player.
* [xinitrc](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/xinitrc) - Direct bare-metal X11 startup orchestrator.

---

## Step-by-Step ISO Build Instructions

### Prerequisites
You must run the build script on a **Debian or Ubuntu host** (or within a VM). Sudo/root permissions are required because it runs kernel `chroot` commands to mount and package the root squash filesystem.

> **Developing on Windows?** The ISO builder needs a Linux binary, so you must cross-compile first. The easiest path is WSL2:
> ```bash
> # In WSL2 (Ubuntu/Debian)
> rustup target add x86_64-unknown-linux-gnu
> sudo apt install gcc libasound2-dev libssl-dev pkg-config
> cargo build --release --target x86_64-unknown-linux-gnu
> # The binary lands at target/x86_64-unknown-linux-gnu/release/R-Audio
> # Copy it to target/release/R-Audio before running the ISO builder
> cp target/x86_64-unknown-linux-gnu/release/R-Audio target/release/R-Audio
> cp target/x86_64-unknown-linux-gnu/release/r-audio-setup target/release/r-audio-setup
> ```

### Step 1: Compile the R-Audio Release Binary
On a Linux host (or WSL2), compile the R-Audio application in release mode:
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

> ⚠️ **This ISO is an unattended installer, not a Live USB.** Booting it on a real machine
> will **wipe the first detected disk** and install Debian + R-Audio on it. No confirmation
> prompt — that's the whole point of the preseed. Use QEMU (below) for any test where you
> don't want to lose data.

### Flashing to a USB Drive
You can flash this ISO to a USB flash drive using standard tools:
* **Windows**: Use [Rufus](https://rufus.ie/) (select "DD Image" mode if prompted) or [Ventoy](https://www.ventoy.net/).
* **Linux / macOS**: Use `dd`:
  ```bash
  sudo dd if=live-image-amd64.hybrid.iso of=/dev/sdX bs=4M status=progress && sync
  ```
  *(Replace `/dev/sdX` with the path to your USB drive).*

### Surface Go 2 — One-time UEFI Setup
Before first boot from the kiosk USB, enter Surface UEFI (hold **Vol+** while pressing **Power**):
1. **Secure Boot** → Disabled (simplest path; shim *should* work but Surface UEFI is finicky)
2. **Boot order** → Move USB Storage above Windows Boot Manager
3. After install, `force-efi-extra-removable=true` (set in [preseed.cfg](file:///c:/Users/Bryan/OneDrive/Bryant%20Desktop/Documents/GitHub/R-Audio/kiosk-os/preseed.cfg)) writes a fallback at `/EFI/BOOT/BOOTX64.EFI` that the Surface firmware will always find — no further UEFI tweaks needed after the first install.

> 💡 **Networking during install:** d-i has to reach `deb.debian.org` to fetch packages.
> The kiosk's own Wi-Fi OOBE flow doesn't run until *after* install. Easiest path:
> plug a USB-C dock with Ethernet into the Surface for the first install only.
> After that, the kiosk handles Wi-Fi pairing itself.

### Local VM Testing (QEMU)
Test the full unattended install + first kiosk boot **without flashing real hardware**.

**Step 1 — Create an empty virtual disk** (one-time):
```bash
qemu-img create -f qcow2 fake.qcow2 16G
```
This is the "fake eMMC" that the installer will wipe. qcow2 is sparse — the file only grows as data is written.

**Step 2 — Run the unattended install:**
```bash
sudo apt install qemu-system-x86 ovmf
qemu-system-x86_64 -enable-kvm -m 2G \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive file=fake.qcow2,format=qcow2 \
    -cdrom live-image-amd64.hybrid.iso
```
The installer runs through unattended (no prompts). When it finishes, close the QEMU window. `-bios OVMF.fd` boots QEMU in UEFI mode to exercise the same code path as the Surface (without it, you'd test the legacy BIOS / isolinux path instead).

**Step 3 — Boot just the installed disk** (no `-cdrom`):
```bash
qemu-system-x86_64 -enable-kvm -m 2G \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive file=fake.qcow2,format=qcow2
```
You should see GRUB → kernel → systemd → getty autologin → startx → R-Audio. If anything breaks in that chain, you'll catch it here.

To start over from scratch: `rm fake.qcow2 && qemu-img create -f qcow2 fake.qcow2 16G`.

---

## Seamless Kiosk Pairing Workflow

Once the Kiosk boots for the first time, it enters **Pairing Mode** automatically:
1. The screen displays a gorgeous high-contrast **QR Code** and instructions pointing to `http://r-audio.local:8888`.
2. Connect your phone or laptop to the **same Wi-Fi network** as the kiosk.
3. Scan the QR code or type `http://r-audio.local:8888` in your browser.
4. Log in to your Spotify account and authorize R-Audio.
5. The pairing server will handle the token cache swap, save it to persistent storage, and transition the kiosk screen to the **Music Dashboard** instantly!
