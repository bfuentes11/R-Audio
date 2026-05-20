#!/bin/bash
# ==============================================================================
# R-Audio Custom Debian Kiosk OS ISO Builder
# ==============================================================================
# This script uses Debian's official 'live-build' system to generate a highly
# optimized, minimal, bootable Live-Installer ISO (.iso) running R-Audio.
#
# Requirements:
# - Run this script on a Debian/Ubuntu host or VM.
# - Root/Sudo privileges are required to run 'debootstrap' and 'live-build'.
# ==============================================================================

set -e

# Configuration
DEBIAN_VERSION="bookworm"
IMAGE_DIR="r-audio-iso-build"
BINARY_PATH="../target/release/R-Audio"

echo "=== R-Audio Kiosk ISO Build Utility ==="

# 1. Verification checks
if [ "$EUID" -ne 0 ]; then
  echo "ERROR: Please run this script with sudo or as root."
  exit 1
fi

if [ ! -f "$BINARY_PATH" ]; then
  echo "ERROR: Compiled release binary not found at $BINARY_PATH."
  echo "Please build the release binary first by running: cargo build --release"
  exit 1
fi

echo "Installing ISO builder dependencies (live-build, xorriso, squashfs-tools)..."
apt-get update && apt-get install -y live-build xorriso squashfs-tools curl wget

# 2. Initialize live-build environment
rm -rf "$IMAGE_DIR"
mkdir -p "$IMAGE_DIR"
cd "$IMAGE_DIR"

echo "Bootstrapping Live-Build configuration..."
lb config \
  --binary-images iso-hybrid \
  --architectures amd64 \
  --distribution "$DEBIAN_VERSION" \
  --archive-areas "main contrib non-free non-free-firmware" \
  --debian-installer false \
  --memtest none \
  --parent-mirror-bootstrap "http://deb.debian.org/debian/"

# 3. Configure packages to be pre-installed inside the Chroot
echo "Configuring pre-installed packages list..."
cat <<EOF > config/package-lists/kiosk.list.chroot
# Display Server & barebones WM
xserver-xorg
xinit
openbox

# Audio Framework
alsa-utils
pulseaudio

# Local Networking & mDNS pairing resolution
avahi-daemon
libavahi-compat-libdnssd1
dbus-x11

# System essentials
ca-certificates
curl
wget
sudo
EOF

# 4. Inject Kiosk OS files directly into the target filesystem
echo "Injecting R-Audio bare-metal configurations..."

# Create includes directories representing the target root directory
INCLUDES="config/includes.chroot"
mkdir -p "$INCLUDES/usr/local/bin"
mkdir -p "$INCLUDES/etc/systemd/system"
mkdir -p "$INCLUDES/etc/default"
mkdir -p "$INCLUDES/etc/systemd/system/getty@tty1.service.d"
mkdir -p "$INCLUDES/home/kiosk"

# Copy our compiled R-Audio Rust binary
cp "../$BINARY_PATH" "$INCLUDES/usr/local/bin/r-audio"
chmod +x "$INCLUDES/usr/local/bin/r-audio"

# Copy the custom systemd player service
cp ../r-audio.service "$INCLUDES/etc/systemd/system/r-audio.service"

# Copy the environment file template as the active configuration
cp ../r-audio.env.template "$INCLUDES/etc/default/r-audio"

# Copy the X11 bare startup script
cp ../xinitrc "$INCLUDES/home/kiosk/.xinitrc"

# 5. Configure TTY1 Auto-Login without passwords (standard kiosk practice)
cat <<EOF > "$INCLUDES/etc/systemd/system/getty@tty1.service.d/override.conf"
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin kiosk --noclear %I \$TERM
EOF

# 6. Configure Bash Profile to start X Server dynamically on login
cat <<EOF > "$INCLUDES/etc/home/kiosk/.bash_profile"
# Automatically launch bare Xorg when booting into TTY1
if [ -z "\$DISPLAY" ] && [ "\$(tty)" = "/dev/tty1" ]; then
    exec startx -- -nocursor
fi
EOF

# Set proper permissions inside the target root filesystem
# Sudoers configuration for kiosk user to allow autostart and network tasks
mkdir -p "$INCLUDES/etc/sudoers.d"
echo "kiosk ALL=(ALL) NOPASSWD: ALL" > "$INCLUDES/etc/sudoers.d/kiosk"
chmod 440 "$INCLUDES/etc/sudoers.d/kiosk"

# 7. Post-configuration setup (create kiosk user during image creation)
mkdir -p config/hooks/normal
cat <<'EOF' > config/hooks/normal/0900-create-kiosk-user.hook.chroot
#!/bin/sh
# Add the dedicated kiosk user and assign proper sound/video groups
useradd -m -s /bin/bash -g users -G sudo,audio,video kiosk
chown -R kiosk:users /home/kiosk
chmod +x /home/kiosk/.xinitrc

# Enable the r-audio and avahi system services
systemctl enable r-audio
systemctl enable avahi-daemon
EOF
chmod +x config/hooks/normal/0900-create-kiosk-user.hook.chroot

# 8. Compile the ISO
echo "Compiling the bootable hybrid Kiosk ISO..."
lb build

echo "=== SUCCESS! ==="
echo "Your custom bootable kiosk ISO has been created:"
ls -lh live-image-amd64.hybrid.iso
