#!/bin/bash
# ==============================================================================
# R-Audio Kiosk OS Launcher Orchestrator
# ==============================================================================
# This script handles booting state orchestration inside the X11 session.
# It checks for active network connectivity first. If offline, it runs the
# standalone setup wizard ('r-audio-setup') to configure network settings.
# Finally, it starts the main player client ('r-audio').
# ==============================================================================

# Redirect ALL output (this script + the r-audio binary we exec into at the
# end) to /tmp/r-audio.log so the Slint Debug tab in Settings can display it
# on the kiosk screen — the Surface Go 2 has no keyboard, no easy SSH path,
# so on-screen log viewing is the only diagnostic option. tmpfs clears on
# reboot, which is fine for live debugging.
exec > /tmp/r-audio.log 2>&1

echo "[launcher] Starting R-Audio Kiosk Boot Manager..."

# Load environment (credentials + display settings)
if [ -f /etc/default/r-audio ]; then
    set -a
    source /etc/default/r-audio
    set +a
fi

# Function to check connectivity (resolve/ping Spotify API)
check_connectivity() {
    ping -c 1 -W 2 api.spotify.com &>/dev/null
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
