#!/bin/bash
# ==============================================================================
# R-Audio Kiosk ISO Builder — Official Debian DVD Remaster
# ==============================================================================
# Downloads the official Debian trixie DVD1 (offline-capable, includes
# non-free firmware) and remasters it with:
#
#   /preseed.cfg          — unattended install config (at ISO root)
#   /r-audio-payload/     — R-Audio binaries + post-install scripts
#   /boot/grub/grub.cfg   — replaced to auto-boot the unattended installer
#
# Uses xorriso to graft our files onto the original ISO while preserving
# the exact boot structure (EFI + BIOS). No live-build, no initrd patching.
#
# The official Debian DVD's d-i is already configured for offline use —
# it mounts the disc early, uses it as the apt source, and doesn't require
# a reachable network mirror. This sidesteps all the "bad archive mirror"
# issues that plagued the live-build approach.
#
# Requirements:
#   - Run as root (xorriso needs it for some ISO operations)
#   - cargo build --release must have been run first
#   - ~10 GB free disk space (4.7 GB source ISO + output ISO + working files)
# ==============================================================================

set -e

# ------------------------------------------------------------------------------
# Configuration
# ------------------------------------------------------------------------------
DEBIAN_VERSION="trixie"
MIRROR="https://cdimage.debian.org/debian-cd/current/amd64/iso-dvd"
BINARY_PATH="../target/release/R-Audio"
SETUP_BINARY_PATH="../target/release/r-audio-setup"
WORK_DIR="r-audio-iso-work"
OUTPUT_ISO="r-audio-kiosk-${DEBIAN_VERSION}.iso"

echo "=== R-Audio Kiosk ISO Builder (Remaster) ==="

# ------------------------------------------------------------------------------
# Preflight checks
# ------------------------------------------------------------------------------
if [ "$EUID" -ne 0 ]; then
    echo "ERROR: Run with sudo or as root."
    exit 1
fi

if [ ! -f "$BINARY_PATH" ] || [ ! -f "$SETUP_BINARY_PATH" ]; then
    echo "ERROR: Release binaries not found."
    echo "       Run: cargo build --release"
    exit 1
fi

# ------------------------------------------------------------------------------
# Dependencies
# ------------------------------------------------------------------------------
echo "Installing dependencies (xorriso, wget)..."
apt-get update -qq
apt-get install -y -qq xorriso wget

# ------------------------------------------------------------------------------
# Locate or download the official Debian DVD1
# ------------------------------------------------------------------------------
echo "Locating Debian ${DEBIAN_VERSION} DVD1..."
DVD_ISO=""

# Re-use a previously downloaded copy if present in the current directory.
# In CI, cache this file between runs to avoid the 4.7 GB download every time.
for f in debian-*-amd64-DVD-1.iso; do
    if [ -f "$f" ]; then
        DVD_ISO="$f"
        echo "  Using cached ISO: $DVD_ISO"
        break
    fi
done

if [ -z "$DVD_ISO" ]; then
    echo "  Fetching directory listing from $MIRROR ..."
    ISO_NAME=$(wget -q -O- "$MIRROR/" \
        | grep -oP 'debian-[0-9]+\.[0-9.]+-amd64-DVD-1\.iso(?=")' \
        | head -1)

    if [ -z "$ISO_NAME" ]; then
        echo "ERROR: Could not find DVD1 ISO at $MIRROR"
        echo "       Check https://cdimage.debian.org/debian-cd/current/amd64/iso-dvd/"
        exit 1
    fi

    echo "  Downloading $ISO_NAME (~4.7 GB) ..."
    echo "  Tip: cache this file to skip the download on future builds."
    wget -c --progress=bar:force "$MIRROR/$ISO_NAME" -O "$ISO_NAME"
    DVD_ISO="$ISO_NAME"
fi

echo "  Source ISO: $DVD_ISO ($(du -h "$DVD_ISO" | cut -f1))"

# ------------------------------------------------------------------------------
# Stage files to inject into the ISO
# ------------------------------------------------------------------------------
echo "Staging R-Audio payload..."
rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR/payload"

# R-Audio runtime binaries
cp "$BINARY_PATH"           "$WORK_DIR/payload/r-audio"
cp "$SETUP_BINARY_PATH"     "$WORK_DIR/payload/r-audio-setup"
cp r-audio-launcher.sh         "$WORK_DIR/payload/r-audio-launcher"
cp r-audio.service             "$WORK_DIR/payload/r-audio.service"
cp r-audio.env.template        "$WORK_DIR/payload/r-audio.env"
cp xinitrc                     "$WORK_DIR/payload/xinitrc"
cp postinstall.sh              "$WORK_DIR/payload/postinstall.sh"
chmod +x \
    "$WORK_DIR/payload/r-audio" \
    "$WORK_DIR/payload/r-audio-setup" \
    "$WORK_DIR/payload/r-audio-launcher" \
    "$WORK_DIR/payload/postinstall.sh"

# ------------------------------------------------------------------------------
# Inject sensitive runtime secrets from the build environment.
#
# r-audio.env.template ships ONLY non-sensitive defaults (client IDs,
# redirect URIs, Slint settings). The actual secrets are injected here at
# build time from env vars sourced from GitHub Actions secrets (or local
# `export` for off-CI builds). This keeps secrets out of git while still
# baking them into the offline ISO.
#
# Required (or warned) for full functionality:
#   SPOTIFY_APP_SECRET     — X-App-Secret header for the Cloudflare worker
#                            on /auth, /wait, /callback. 401 if missing.
#   RSPOTIFY_CLIENT_SECRET — Spotify app's OAuth client secret. Required for
#                            librespot/rspotify token refresh.
#   LASTFM_API_KEY         — Last.fm API key for autoplay recommendations.
# ------------------------------------------------------------------------------
inject_secret() {
    local name="$1"
    local value="$2"
    if [ -n "$value" ]; then
        echo "  Injecting $name into baked r-audio.env"
        echo "${name}=${value}" >> "$WORK_DIR/payload/r-audio.env"
    else
        echo "  WARNING: $name not set — kiosk will be missing this credential."
    fi
}

echo "Injecting runtime secrets into r-audio.env..."
inject_secret SPOTIFY_APP_SECRET     "${SPOTIFY_APP_SECRET:-}"
inject_secret RSPOTIFY_CLIENT_SECRET "${RSPOTIFY_CLIENT_SECRET:-}"
inject_secret LASTFM_API_KEY         "${LASTFM_API_KEY:-}"

# ------------------------------------------------------------------------------
# Pre-fetch the full dependency tree of every package the kiosk needs beyond
# what d-i installs from DVD1. We spin up a Debian trixie container so the
# package versions exactly match the target's, then apt-download every .deb
# (and recursive deps) into the ISO's payload at /r-audio-debs/. postinstall.sh
# later runs `dpkg -i` on the whole pile during install — fully offline.
#
# This is the same strategy FAI uses internally: resolve deps at build time
# on a known-good environment, ship the resulting .debs.
# ------------------------------------------------------------------------------
echo "Pre-fetching extra .deb packages with complete dependency tree..."
DEBS_DIR="$WORK_DIR/payload/r-audio-debs"
mkdir -p "$DEBS_DIR"

# Packages the kiosk needs that aren't on Debian DVD1, plus any r-audio
# runtime libs we want to bundle defensively.
# Must be ONE line — newlines in the value break the `bash -c` string below.
KIOSK_PKGS="openbox onboard pulseaudio libavahi-compat-libdnssd1 bluez bluez-tools iw rfkill xserver-xorg-legacy libfontconfig1 libfreetype6 libxkbcommon0 libxkbcommon-x11-0 libegl1 libgles2 libgl1 libglib2.0-0 libglib2.0-bin libssl3 dnsmasq-base dbus wmctrl xdotool at-spi2-core dconf-gsettings-backend mousetweaks fonts-dejavu-core python3 python3-gi python3-gi-cairo"

# Check for a pre-populated cache (used by CI to skip the docker download).
# Set R_AUDIO_DEBS_CACHE to a directory path to enable.
if [ -n "${R_AUDIO_DEBS_CACHE:-}" ] && [ -d "$R_AUDIO_DEBS_CACHE" ] && \
   ls "$R_AUDIO_DEBS_CACHE"/*.deb >/dev/null 2>&1; then
    echo "  Using cached .debs from $R_AUDIO_DEBS_CACHE ($(ls "$R_AUDIO_DEBS_CACHE" | wc -l) files)"
    cp "$R_AUDIO_DEBS_CACHE"/*.deb "$DEBS_DIR/"
else
    # shellcheck disable=SC2086
    # NOTE: deliberately NOT using --no-install-recommends here. Complex
    # Python/GTK packages like onboard need their Recommends bundled too —
    # otherwise dpkg -i during postinstall configures them only partially
    # and the binary may end up missing/non-functional. The +30-50MB ISO
    # size hit is worth the install reliability.
    docker run --rm \
        -v "$(realpath "$DEBS_DIR")":/debs \
        debian:trixie-slim bash -c "
            set -e
            apt-get update -qq
            DEBIAN_FRONTEND=noninteractive apt-get install -y --download-only $KIOSK_PKGS
            cp /var/cache/apt/archives/*.deb /debs/
            echo \"Downloaded \$(ls /debs/ | wc -l) .deb files (\$(du -sh /debs/ | cut -f1) total)\"
        "

    # If a cache dir was specified, populate it for the next run.
    if [ -n "${R_AUDIO_DEBS_CACHE:-}" ]; then
        echo "  Populating cache at $R_AUDIO_DEBS_CACHE"
        mkdir -p "$R_AUDIO_DEBS_CACHE"
        cp "$DEBS_DIR"/*.deb "$R_AUDIO_DEBS_CACHE/"
    fi
fi

# Preseed (placed at the ISO root — d-i reads it as /cdrom/preseed.cfg)
cp preseed.cfg "$WORK_DIR/preseed.cfg"

# ------------------------------------------------------------------------------
# Custom GRUB config — default entry is the unattended installer
# timeout=0 + hidden so the Surface boots straight in; hold Shift for menu
# ------------------------------------------------------------------------------
cat > "$WORK_DIR/grub.cfg" <<'GRUBEOF'
if loadfont $prefix/font.pf2; then
    set gfxmode=auto
    insmod all_video
    insmod gfxterm
    terminal_output gfxterm
fi

set default=0
set timeout=0
set timeout_style=hidden

menuentry "R-Audio Kiosk - Auto-Install (WIPES DISK)" {
    linux  /install.amd/vmlinuz auto=true priority=critical preseed/file=/cdrom/preseed.cfg vga=788 --- quiet
    initrd /install.amd/initrd.gz
}

menuentry "Install (interactive fallback)" {
    linux  /install.amd/vmlinuz vga=788 --- quiet
    initrd /install.amd/initrd.gz
}

menuentry "Graphical Install (interactive fallback)" {
    linux  /install.amd/vmlinuz video=vesa:ywrap,mtrr vga=788 --- quiet
    initrd /install.amd/gtk/initrd.gz
}
GRUBEOF

# ------------------------------------------------------------------------------
# Build the remastered ISO
#
# xorriso -indev / -outdev copies the original ISO and applies our changes:
#   -map src dst   — graft a local file/dir onto the ISO at the given path
#   -boot_image any replay — preserve the original EFI + BIOS boot setup
#                            exactly as Debian shipped it
# ------------------------------------------------------------------------------
echo "Building remastered ISO: $OUTPUT_ISO ..."
xorriso \
    -indev  "$DVD_ISO" \
    -outdev "$OUTPUT_ISO" \
    -boot_image any replay \
    -map "$WORK_DIR/preseed.cfg"  /preseed.cfg \
    -map "$WORK_DIR/grub.cfg"     /boot/grub/grub.cfg \
    -map "$WORK_DIR/payload"      /r-audio-payload \
    --

echo ""
echo "=== SUCCESS ==="
echo "Remastered ISO: $OUTPUT_ISO"
ls -lh "$OUTPUT_ISO"
echo ""
echo "Write to USB with:"
echo "  dd if=$OUTPUT_ISO of=/dev/sdX bs=4M status=progress && sync"
