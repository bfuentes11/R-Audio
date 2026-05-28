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
cp "$PAYLOAD/r-audio"          /target/usr/local/bin/r-audio
cp "$PAYLOAD/r-audio-setup"    /target/usr/local/bin/r-audio-setup
cp "$PAYLOAD/r-audio-launcher" /target/usr/local/bin/r-audio-launcher
chmod +x /target/usr/local/bin/r-audio \
         /target/usr/local/bin/r-audio-setup \
         /target/usr/local/bin/r-audio-launcher

echo "[postinstall] Installing systemd unit and environment file..."
cp "$PAYLOAD/r-audio.service" /target/etc/systemd/system/r-audio.service
cp "$PAYLOAD/r-audio.env"     /target/etc/default/r-audio

echo "[postinstall] Installing kiosk user X11 startup..."
cp "$PAYLOAD/xinitrc" /target/home/kiosk/.xinitrc
chmod +x /target/home/kiosk/.xinitrc

# Minimal openbox config — suppresses the "no valid config file" and
# "no menu.xml" warnings that clutter the log on every boot.
mkdir -p /target/home/kiosk/.config/openbox
cat > /target/home/kiosk/.config/openbox/rc.xml <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<openbox_config xmlns="http://openbox.org/3.4/rc">
  <resistance><strength>10</strength><screen_edge_strength>20</screen_edge_strength></resistance>
  <focus><focusNew>yes</focusNew><followMouse>no</followMouse><focusLast>yes</focusLast><underMouse>no</underMouse><focusDelay>200</focusDelay><raiseOnFocus>no</raiseOnFocus></focus>
  <placement><policy>Smart</policy></placement>
  <theme><name>Clearlooks</name><titleLayout>NLIMC</titleLayout></theme>
  <desktops><number>1</number><firstdesk>1</firstdesk><names/><popupTime>875</popupTime></desktops>
  <resize><drawContents>yes</drawContents><popupShow>NonPixel</popupShow></resize>
  <mouse><dragThreshold>8</dragThreshold><doubleClickTime>200</doubleClickTime><screenEdgeWarpTime>400</screenEdgeWarpTime></mouse>
  <keyboard/>
  <applications/>
</openbox_config>
EOF
cat > /target/home/kiosk/.config/openbox/menu.xml <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<openbox_menu xmlns="http://openbox.org/3.4/menu">
  <menu id="root-menu" label="Openbox 3" execute=""/>
</openbox_menu>
EOF

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

# ──────────────────────────────────────────────────────────────────────────────
# Install the bundled .deb packages (no network required).
# build-iso.sh pre-fetched the complete dep tree on a Debian trixie container
# so every package + transitive dep is in /target/tmp/r-audio-payload/r-audio-debs/.
# Calling `dpkg -i` on all of them at once lets dpkg figure out the install
# order via its internal dep graph.
# ──────────────────────────────────────────────────────────────────────────────
echo "[postinstall] Installing bundled .deb packages (offline)..."
mkdir -p /target/var/cache/r-audio-debs
cp "$PAYLOAD"/r-audio-debs/*.deb /target/var/cache/r-audio-debs/

# --auto-deconfigure handles installing in dep order; -E skips already-installed.
# We don't `set -e` around this because some postinst scripts (openbox, etc.)
# can emit non-fatal warnings that would trip set -e. Capture stdout+stderr
# to a log file so we can debug install failures post-boot — without this,
# packages like onboard can silently fail to install and we only notice
# weeks later when the on-screen keyboard doesn't appear.
in-target sh -c 'dpkg -i --auto-deconfigure /var/cache/r-audio-debs/*.deb 2>&1 | tee /var/log/r-audio-dpkg.log' || {
    echo "[postinstall] WARNING: dpkg -i reported issues — see /var/log/r-audio-dpkg.log."
}

# Some packages may have unpacked but failed to configure (postinst error,
# dep ordering issue, etc.). dpkg --configure -a retries every unconfigured
# package, often fixing complex chains like Python+GTK that the initial
# pass couldn't resolve. Output goes to the same log so we can debug.
in-target sh -c 'dpkg --configure -a 2>&1 | tee -a /var/log/r-audio-dpkg.log' || {
    echo "[postinstall] WARNING: dpkg --configure -a still has unconfigured packages."
}

# Explicit sanity check on the critical kiosk binaries. If any of these are
# missing the kiosk will boot but lose a feature, and we want to know.
for bin in /target/usr/bin/onboard /target/usr/bin/wmctrl /target/usr/bin/xdotool /target/usr/bin/openbox-session; do
    if [ -x "$bin" ]; then
        echo "[postinstall] OK: $bin"
    else
        echo "[postinstall] MISSING: $bin (check /var/log/r-audio-dpkg.log on the kiosk)"
    fi
done

# Clean up the deb cache once installed.
rm -rf /target/var/cache/r-audio-debs

# ──────────────────────────────────────────────────────────────────────────────
# Boot appearance — suppress all kernel/systemd console output and hide GRUB.
# No Plymouth: the screen goes black immediately after GRUB and stays black
# until r-audio's own Slint UI takes over. This is cleaner and simpler than
# Plymouth since we own the whole display from the moment the app launches.
# ──────────────────────────────────────────────────────────────────────────────
echo "[postinstall] Configuring silent boot (no Plymouth)..."

# Suppress kernel messages, systemd status, udev noise, and the VT cursor.
sed -i 's|^GRUB_CMDLINE_LINUX_DEFAULT=.*|GRUB_CMDLINE_LINUX_DEFAULT="quiet loglevel=0 rd.systemd.show_status=false systemd.show_status=false rd.udev.log_level=3 vt.global_cursor_default=0 fbcon=nodefer"|' /target/etc/default/grub

# Completely hide the GRUB menu — zero-second timeout, no countdown.
sed -i 's|^GRUB_TIMEOUT=.*|GRUB_TIMEOUT=0|' /target/etc/default/grub
echo 'GRUB_TIMEOUT_STYLE=hidden' >> /target/etc/default/grub
echo 'GRUB_HIDDEN_TIMEOUT=0' >> /target/etc/default/grub

# Rebuild initramfs (picks up firmware/driver changes) and regenerate GRUB config.
in-target update-initramfs -u
in-target update-grub

# X wrapper config — allow non-root kiosk user to start X with proper iopl.
mkdir -p /target/etc/X11
cat > /target/etc/X11/Xwrapper.config <<'EOF'
allowed_users=anybody
needs_root_rights=yes
EOF

# Clean up the payload copy so the installed system isn't carrying it around.
rm -rf "$PAYLOAD" /target/tmp/postinstall.sh

echo "[postinstall] Done. System will reboot into the fully-equipped kiosk."
