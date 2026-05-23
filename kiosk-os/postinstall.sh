#!/bin/sh
# ==============================================================================
# R-Audio Kiosk — Post-Install Configuration
# ==============================================================================
# Invoked from preseed.cfg's late_command after d-i has finished installing
# the base system. Runs in the INSTALLER environment (NOT chrooted into the
# target), so paths use:
#   /target/...   = the freshly installed system's root
#   /cdrom/...    = the install media (still mounted)
# Use `in-target` to run commands inside the target chroot when needed
# (for things like systemctl enable that need correct symlink paths, or
# usermod where group lookups must hit the installed /etc/group).
# ==============================================================================
set -e

PAYLOAD=/target/tmp/r-audio-payload

echo "[postinstall] Installing R-Audio binaries..."
cp "$PAYLOAD/r-audio"                  /target/usr/local/bin/r-audio
cp "$PAYLOAD/r-audio-setup"            /target/usr/local/bin/r-audio-setup
cp "$PAYLOAD/r-audio-launcher"         /target/usr/local/bin/r-audio-launcher
cp "$PAYLOAD/install-kiosk-packages.sh" /target/usr/local/bin/r-audio-install-packages
chmod +x /target/usr/local/bin/r-audio \
         /target/usr/local/bin/r-audio-setup \
         /target/usr/local/bin/r-audio-launcher \
         /target/usr/local/bin/r-audio-install-packages

echo "[postinstall] Installing systemd unit and environment file..."
cp "$PAYLOAD/r-audio.service" /target/etc/systemd/system/r-audio.service
cp "$PAYLOAD/r-audio.env"     /target/etc/default/r-audio

echo "[postinstall] Installing kiosk user X11 startup..."
cp "$PAYLOAD/xinitrc" /target/home/kiosk/.xinitrc
chmod +x /target/home/kiosk/.xinitrc

cat > /target/home/kiosk/.bash_profile <<'EOF'
# Auto-launch bare Xorg on TTY1 (kiosk path)
if [ -z "$DISPLAY" ] && [ "$(tty)" = "/dev/tty1" ]; then
    # Log X session errors so they can be read from another TTY or SSH
    startx -- -nocursor 2>/home/kiosk/startx-error.log
    EXIT_CODE=$?
    # Don't loop on crash — show the error and pause so it's readable
    echo ""
    echo "=== startx exited (code $EXIT_CODE) ==="
    echo "Error log: /home/kiosk/startx-error.log"
    echo ""
    cat /home/kiosk/startx-error.log
    echo ""
    echo "=== /home/kiosk/.xsession-errors ==="
    cat /home/kiosk/.xsession-errors 2>/dev/null || echo "(no xsession-errors file)"
    echo ""
    echo "Press Enter to retry, or Ctrl+C to stay at shell."
    read _
    exec "$BASH" --login
fi
EOF

echo "[postinstall] Configuring TTY1 autologin for the kiosk user..."
mkdir -p /target/etc/systemd/system/getty@tty1.service.d
cat > /target/etc/systemd/system/getty@tty1.service.d/override.conf <<'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin kiosk --noclear %I $TERM
EOF

echo "[postinstall] Granting passwordless sudo to kiosk..."
# This is a single-purpose kiosk device. The kiosk user needs unrestricted
# sudo for nmcli (Wi-Fi), apt (package install on first boot), and reboot.
echo 'kiosk ALL=(ALL) NOPASSWD: ALL' > /target/etc/sudoers.d/kiosk
chmod 440 /target/etc/sudoers.d/kiosk

echo "[postinstall] Setting hostname..."
echo 'r-audio' > /target/etc/hostname
cat > /target/etc/hosts <<'EOF'
127.0.0.1   localhost r-audio
::1         localhost ip6-localhost ip6-loopback
ff02::1     ip6-allnodes
ff02::2     ip6-allrouters
EOF

echo "[postinstall] Adding kiosk to hardware groups, fixing /home ownership..."
in-target usermod -aG sudo,audio,video,input kiosk
in-target chown -R kiosk:kiosk /home/kiosk

echo "[postinstall] Selecting multi-user.target (kiosk uses getty->startx, no DM)..."
in-target systemctl set-default multi-user.target

echo "[postinstall] Writing /etc/apt/sources.list..."
# The 50mirror apt-setup generator was disabled during install to prevent a
# blocking "bad archive mirror" dialog (no network during d-i). Write the
# sources.list manually here so apt works normally once Wi-Fi is paired.
cat > /target/etc/apt/sources.list <<'EOF'
deb http://deb.debian.org/debian trixie main contrib non-free non-free-firmware
deb http://security.debian.org/debian-security trixie-security main contrib non-free non-free-firmware
deb http://deb.debian.org/debian trixie-updates main contrib non-free non-free-firmware
EOF

echo "[postinstall] Enabling network and mDNS services..."
in-target systemctl enable avahi-daemon
in-target systemctl enable NetworkManager

echo "[postinstall] Staging R-Audio Plymouth theme assets..."
# Plymouth itself isn't on the Debian DVD1 — it's apt-installed by
# r-audio-install-packages after Wi-Fi connects. We pre-stage the theme files
# now so they're already in place when Plymouth installs and gets activated.
mkdir -p /target/usr/share/plymouth/themes/r-audio
cp "$PAYLOAD/plymouth-theme/r-audio.plymouth" /target/usr/share/plymouth/themes/r-audio/r-audio.plymouth
cp "$PAYLOAD/plymouth-theme/r-audio.script"   /target/usr/share/plymouth/themes/r-audio/r-audio.script
cp "$PAYLOAD/plymouth-theme/rust-logo.png"    /target/usr/share/plymouth/themes/r-audio/rust-logo.png

# Clean up the payload copy so the installed system isn't carrying it around
rm -rf "$PAYLOAD" /target/tmp/postinstall.sh

echo "[postinstall] Done. System will reboot into the kiosk on next boot."
