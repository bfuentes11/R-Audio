# R-Audio Debian Kiosk OS Build Guide

Bootable Debian **trixie** (13) ISO that turns a Surface Go 2 (or any x86_64 UEFI machine) into a dedicated R-Audio kiosk. Boots in under 10 seconds straight into a fullscreen Slint UI — no GNOME, no desktop environment, just bare X11 + openbox + the player.

The ISO is built by **remastering the official Debian DVD1** (xorriso graft of our preseed + payload). No live-build, no Docker for the build itself, no initrd patching. The d-i install runs fully offline from the DVD pool; anything not on DVD1 is installed after Wi-Fi connects on first boot.

---

## Boot Flow

### First boot ever (from USB)
```
[GRUB picks unattended entry, timeout 0, hidden]
     │
     ▼
[d-i reads /cdrom/preseed.cfg ──► fully unattended install]
     │
     ▼
[Auto-detects first disk (eMMC on Surface Go 2) ──► WIPES + partitions]
     │
     ▼
[Installs minimal Debian + base kiosk deps from the DVD pool only — NO network]
     │
     ▼
[late_command → postinstall.sh: copies r-audio binaries, autologin, sudoers,
                                stages Plymouth theme files for later]
     │
     ▼
[d-i ejects USB, reboots into installed system]
```

### First boot from internal disk (one-time OOBE)
```
[autologin kiosk on tty1 → startx → r-audio launches in non-fullscreen]
     │
     ▼
[r-audio detects first run → starts Wi-Fi hotspot "R-Audio-Setup"]
     │
     ▼
[User scans QR, connects phone to hotspot, enters home Wi-Fi creds in web form]
     │
     ▼
[r-audio connects to home Wi-Fi → runs /usr/local/bin/r-audio-install-packages
                                    (apt-installs openbox, plymouth, pulseaudio,
                                     onboard, bluez-tools, etc.)]
     │
     ▼
[Spotify pairing QR flow → mark setup complete → REBOOT]
```

### Every boot after that
```
[Plymouth pulsing-Rust-logo splash → autologin → startx]
     │
     ▼
[xinitrc starts openbox + onboard + r-audio fullscreen]
     │
     ▼
[r-audio loads cached Spotify token, goes straight to player]
```

---

## Files in this directory

| File | Purpose |
|---|---|
| `build-iso.sh` | Downloads Debian DVD1 (or uses cache), runs xorriso to graft our payload onto it |
| `preseed.cfg` | d-i answer file — fully unattended install with DVD-only package set |
| `postinstall.sh` | Runs from preseed `late_command`; installs r-audio binaries, configures autologin, stages Plymouth theme |
| `install-kiosk-packages.sh` | Apt-installs everything not on DVD1 — called by r-audio OOBE after Wi-Fi connects |
| `xinitrc` | X11 startup — runs openbox + onboard + r-audio-launcher |
| `r-audio-launcher.sh` | Pre-flight connectivity check, then exec's r-audio |
| `r-audio.service` | systemd unit (manual dev use only; the autologin/startx chain is the kiosk path) |
| `r-audio.env.template` | Env vars (Spotify keys, Last.fm key, Slint settings) — copied to `/etc/default/r-audio` |
| `plymouth-theme/` | Source SVG + `.plymouth` + `.script` for the pulsing-Rust-logo boot splash |

---

## Package strategy: what goes where

**On the Debian DVD1 → installed by d-i during preseed install:**
- `xserver-xorg`, `xinit`, `xserver-xorg-input-libinput`, `xauth`, `x11-xserver-utils`
- `libgl1-mesa-dri`
- `alsa-utils`
- `avahi-daemon`, `dbus-x11`
- `network-manager`, `wpasupplicant`
- `ca-certificates`, `curl`, `wget`, `sudo`
- `firmware-misc-nonfree`, `firmware-iwlwifi`
- `fonts-dejavu-core`

**Not on DVD1 → apt-installed by `install-kiosk-packages.sh` after first-boot Wi-Fi:**
- `openbox` — window manager (GNOME's DVD uses mutter, not openbox)
- `onboard` — touchscreen on-screen keyboard
- `pulseaudio` — audio server (trixie's GNOME defaults to pipewire)
- `libavahi-compat-libdnssd1` — Bonjour compat lib for librespot
- `bluez`, `bluez-tools` — Bluetooth stack + diagnostics
- `iw`, `rfkill` — Wi-Fi diagnostics
- `plymouth`, `plymouth-themes` — graphical boot splash
- `xserver-xorg-legacy` — setuid X wrapper so non-root kiosk user can start X

**Rule:** if a package isn't in the GNOME-task dependency chain on DVD1, it goes in `install-kiosk-packages.sh`. Adding speculative packages to `preseed.cfg` breaks the "Select and install software" d-i step.

---

## Building the ISO

### Option 1 — GitHub Actions (recommended)

The workflow at [.github/workflows/build-kiosk-iso.yml](../.github/workflows/build-kiosk-iso.yml) builds on `ubuntu-latest` (x86_64) on every push to `main` or `librespot-0.4.2`. After ~10 minutes it uploads a `r-audio-kiosk-iso` artifact you can download from the run page.

This is the path most contributors should use. Pushing your changes is faster than maintaining a local cross-compile environment.

### Option 2 — Native build on an x86_64 Linux host

If you have an x86_64 Linux machine (NOT ARM — this won't work on Apple Silicon or Snapdragon WSL without cross-compilation):

```bash
# Dependencies
sudo apt-get install -y gcc libasound2-dev libssl-dev pkg-config \
    libdbus-1-dev libfontconfig1-dev xorriso wget librsvg2-bin

# Build binaries
cargo build --release --target x86_64-unknown-linux-gnu
mkdir -p target/release
cp target/x86_64-unknown-linux-gnu/release/R-Audio       target/release/R-Audio
cp target/x86_64-unknown-linux-gnu/release/r-audio-setup target/release/r-audio-setup

# Build ISO
cd kiosk-os
sudo bash build-iso.sh
# Output: r-audio-kiosk-trixie.iso
```

First run downloads the Debian DVD1 (~4.7 GB, one-time). Subsequent builds skip the download — total time ~2-3 minutes.

---

## Flashing

> ⚠️ **This ISO is an unattended installer, not a Live USB.** Booting it on a real machine **wipes the first detected disk** with no confirmation.

| OS | Tool |
|---|---|
| Windows | [Rufus](https://rufus.ie/) — select **DD Image mode** when prompted |
| Linux/macOS | `sudo dd if=r-audio-kiosk-trixie.iso of=/dev/sdX bs=4M status=progress && sync` |

### Surface Go 2 UEFI setup (one-time, before first install)

Hold **Vol+** while pressing **Power** to enter UEFI.

1. **Security → Secure Boot → Disabled**
   The custom GRUB EFI binary isn't signed with Microsoft's key, so Surface firmware rejects it with Secure Boot on.
2. **Boot order → USB Storage above Windows Boot Manager**
3. Save & Exit. Plug in the USB and turn the device back on.

The preseed sets `force-efi-extra-removable=true`, which installs a fallback bootloader at `/EFI/BOOT/BOOTX64.EFI` — Surface firmware will always find it, so no further UEFI tweaks needed after the install completes.

---

## Local VM testing (QEMU)

Verify the unattended install without flashing real hardware:

```bash
sudo apt install qemu-system-x86 ovmf

# One-time: create empty 16 GB disk
qemu-img create -f qcow2 fake.qcow2 16G

# Run the install (UEFI mode = OVMF — matches Surface firmware)
qemu-system-x86_64 -enable-kvm -m 2G \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive file=fake.qcow2,format=qcow2 \
    -cdrom kiosk-os/r-audio-kiosk-trixie.iso

# After install, boot the installed disk alone (no -cdrom)
qemu-system-x86_64 -enable-kvm -m 2G \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive file=fake.qcow2,format=qcow2
```

To start over: `rm fake.qcow2 && qemu-img create -f qcow2 fake.qcow2 16G`.

---

## Customization

### r-audio.env (Spotify / Last.fm credentials)

`r-audio.env.template` is copied to `/etc/default/r-audio` on the installed system. Edit the template **before** building the ISO if you want custom keys baked in, or SSH into the kiosk and edit it post-install.

```ini
LASTFM_API_KEY=your_lastfm_key
RSPOTIFY_CLIENT_ID=your_spotify_client_id
RSPOTIFY_CLIENT_SECRET=your_spotify_client_secret
SLINT_FULLSCREEN=1
SLINT_SCALE_FACTOR=2.0
```

### Plymouth theme

The pulsing Rust logo lives at `plymouth-theme/`. Edit `r-audio.script` to tune the animation (`progress = progress + 0.04` controls speed; the `0.275 * Math.Sin(...)` term controls pulse depth). The SVG is rasterized to PNG at build time by `build-iso.sh` via `rsvg-convert`.

---

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| "Installation step failed — Select and install software" | A package in `preseed.cfg` pkgsel/include isn't on DVD1. Move it to `install-kiosk-packages.sh`. |
| Boot loop: terminal flashes then black, repeats | startx crashing; the modified `.bash_profile` now pauses with the error log visible — read it from the screen. |
| OOBE shows but hotspot isn't visible from phone | `oobe_start_hotspot()` will display the actual nmcli error on the kiosk screen. Common causes: rfkill block, adapter doesn't support AP mode, NetworkManager not ready. |
| Secure Boot error on Surface | Disable Secure Boot in UEFI (see flashing section above). |
| `sha384` error in GRUB at boot | Cosmetic GRUB warning. Non-fatal — boot continues. |
