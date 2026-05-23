#!/bin/sh
# ==============================================================================
# R-Audio Kiosk — Post-Wi-Fi Package Installer
# ==============================================================================
# Installs packages that are NOT present on the Debian trixie DVD1 pool, plus
# activates the R-Audio Plymouth boot splash (theme files are pre-staged by
# postinstall.sh; we just install the plymouth binaries and flip the switch).
#
# Called by the r-audio OOBE immediately after Wi-Fi connects on first boot,
# before Spotify pairing begins.
#
# Must run as root (called via sudo from r-audio).
# Requires: network connectivity (Wi-Fi already connected).
# ==============================================================================

set -e

echo "[r-audio-install] Updating package lists..."
apt-get update -qq

echo "[r-audio-install] Installing kiosk packages..."
DEBIAN_FRONTEND=noninteractive apt-get install -y \
    openbox \
    pulseaudio \
    libavahi-compat-libdnssd1 \
    bluez-tools \
    onboard \
    xserver-xorg-legacy \
    plymouth \
    plymouth-themes

# ── Plymouth: enable graphical boot splash with the R-Audio theme ──────────
echo "[r-audio-install] Activating R-Audio Plymouth boot splash..."

# Add `splash` + quiet flags to GRUB so kernel/systemd text is hidden behind
# the splash on subsequent boots. Idempotent — won't double-add if already set.
if ! grep -q '^GRUB_CMDLINE_LINUX_DEFAULT=.*splash' /etc/default/grub; then
    sed -i 's|^GRUB_CMDLINE_LINUX_DEFAULT=.*|GRUB_CMDLINE_LINUX_DEFAULT="quiet splash loglevel=3 vt.global_cursor_default=0"|' /etc/default/grub
fi

# Activate the pre-staged theme (postinstall.sh dropped the files into
# /usr/share/plymouth/themes/r-audio/ during install).
if [ -d /usr/share/plymouth/themes/r-audio ]; then
    plymouth-set-default-theme r-audio
else
    echo "[r-audio-install] WARN: r-audio theme not staged; falling back to spinner."
    plymouth-set-default-theme spinner
fi

# Regenerate initramfs (bundles Plymouth + theme into early boot) and GRUB
# config (picks up the new cmdline).
update-initramfs -u
update-grub

# ── xserver-xorg-legacy: allow non-root kiosk user to start X with iopl ─────
# This package ships /etc/X11/Xwrapper.config. Re-write it to our settings in
# case the package's postinst dropped a default that doesn't suit the kiosk.
cat > /etc/X11/Xwrapper.config <<'EOF'
allowed_users=anybody
needs_root_rights=yes
EOF

echo "[r-audio-install] All kiosk packages installed and Plymouth activated."
