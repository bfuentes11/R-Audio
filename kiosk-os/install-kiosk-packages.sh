#!/bin/sh
# ==============================================================================
# R-Audio Kiosk — Post-Wi-Fi Package Installer
# ==============================================================================
# Installs packages that are NOT present on the Debian trixie DVD1 pool.
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
    onboard

echo "[r-audio-install] All kiosk packages installed successfully."
