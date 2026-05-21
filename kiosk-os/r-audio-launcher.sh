#!/bin/bash
# ==============================================================================
# R-Audio Kiosk OS Launcher Orchestrator
# ==============================================================================
# This script handles booting state orchestration inside the X11 session.
# It checks for active network connectivity first. If offline, it runs the
# standalone setup wizard ('r-audio-setup') to configure network settings.
# Finally, it starts the main player client ('r-audio').
# ==============================================================================

echo "[launcher] Starting R-Audio Kiosk Boot Manager..."

# Function to check connectivity (resolve/ping Spotify API)
check_connectivity() {
    ping -c 1 -W 2 api.spotify.com &>/dev/null
}

# 1. Run internet check
if ! check_connectivity; then
    echo "[launcher] System is offline. Spawning Wi-Fi configuration wizard..."
    # Launch Wi-Fi configuration utility (Wait until configured or skipped)
    /usr/local/bin/r-audio-setup
else
    echo "[launcher] System is online."
fi

# 2. Start the main R-Audio player
echo "[launcher] Launching R-Audio player..."
exec /usr/local/bin/r-audio
