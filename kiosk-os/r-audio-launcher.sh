#!/bin/bash
# ==============================================================================
# R-Audio Kiosk OS Launcher Orchestrator
# ==============================================================================
# This script handles booting state orchestration inside the X11 session.
# It checks for active network connectivity first. If offline, it runs the
# standalone setup wizard ('r-audio-setup') to configure network settings.
# Finally, it starts the main player client ('r-audio').
# ==============================================================================

# Append to /tmp/r-audio.log so xinitrc's lines (it truncates the file at
# the top of its run) are preserved. The Slint Debug tab in Settings reads
# the same file, so everything from openbox/matchbox-keyboard launch
# diagnostics through r-audio's runtime logs is visible on-screen — the
# only diagnostic option on a Surface Go 2 with no keyboard or SSH.
exec >> /tmp/r-audio.log 2>&1

echo "[launcher] Starting R-Audio Kiosk Boot Manager..."

# Load environment (credentials + display settings)
if [ -f /etc/default/r-audio ]; then
    set -a
    source /etc/default/r-audio
    set +a
fi

# Function to check connectivity.
# Spotify's edge sits behind a CDN that routinely drops ICMP, so `ping` gives
# false negatives even when the network is fully up — which used to force the
# whole retry loop below on every single boot. Check DNS resolution plus a TCP
# connect to the HTTPS port instead: that's what the app actually needs, and it
# returns the instant the connection is refused/accepted rather than waiting
# out a ping timeout.
check_connectivity() {
    getent hosts api.spotify.com >/dev/null 2>&1 || return 1
    timeout 2 bash -c '</dev/tcp/api.spotify.com/443' >/dev/null 2>&1
}

# 1. Retry connectivity up to 3 times (5s apart) to handle slow DHCP
# Note: the main app handles Wi-Fi setup and Spotify pairing via its built-in
# OOBE flow on first run. The launcher just needs to reach the network eventually.
CONNECTED=false
for attempt in 1 2 3; do
    if check_connectivity; then
        CONNECTED=true
        break
    fi
    echo "[launcher] Network not ready (attempt $attempt/3), retrying in 5s..."
    sleep 5
done

if [ "$CONNECTED" = false ]; then
    echo "[launcher] Network not available after retries — starting player anyway (OOBE will handle Wi-Fi setup)."
else
    echo "[launcher] System is online."
fi

# 2. Start the main R-Audio player (handles OOBE on first run)
echo "[launcher] Launching R-Audio player..."
exec /usr/local/bin/r-audio
