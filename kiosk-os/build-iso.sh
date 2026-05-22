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
SETUP_BINARY_PATH="../target/release/r-audio-setup"

echo "=== R-Audio Kiosk ISO Build Utility ==="

# 1. Verification checks
if [ "$EUID" -ne 0 ]; then
  echo "ERROR: Please run this script with sudo or as root."
  exit 1
fi

if [ ! -f "$BINARY_PATH" ] || [ ! -f "$SETUP_BINARY_PATH" ]; then
  echo "ERROR: Compiled release binaries not found."
  echo "Please build the release binaries first by running: cargo build --release"
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
  --debian-installer live \
  --debian-installer-gui false \
  --memtest none \
  --linux-flavours amd64 \
  --mirror-bootstrap "http://deb.debian.org/debian/" \
  --mirror-chroot "http://deb.debian.org/debian/" \
  --mirror-chroot-security "http://security.debian.org/debian-security/" \
  --mirror-binary "http://deb.debian.org/debian/" \
  --mirror-binary-security "http://security.debian.org/debian-security/"

# 3. Configure packages to be pre-installed inside the Chroot
echo "Configuring pre-installed packages list..."
cat <<EOF > config/package-lists/kiosk.list.chroot
# Display Server & barebones WM
xserver-xorg
xinit
openbox
xserver-xorg-input-libinput

# OpenGL (required by Slint's Skia renderer)
libgl1-mesa-dri

# Audio Framework
alsa-utils
pulseaudio

# Local Networking, mDNS pairing & Wifi management
avahi-daemon
libavahi-compat-libdnssd1
dbus-x11
network-manager
iw
wpasupplicant

# System essentials
ca-certificates
curl
wget
sudo
debootstrap

# Surface Go 2 hardware firmware (Wi-Fi, touchscreen)
firmware-misc-nonfree
firmware-iwlwifi

# Bluetooth support
bluez
bluez-tools
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

# Copy the Wi-Fi setup Rust binary
cp "../$SETUP_BINARY_PATH" "$INCLUDES/usr/local/bin/r-audio-setup"
chmod +x "$INCLUDES/usr/local/bin/r-audio-setup"

# Copy the launcher orchestrator script
cp ../r-audio-launcher.sh "$INCLUDES/usr/local/bin/r-audio-launcher"
chmod +x "$INCLUDES/usr/local/bin/r-audio-launcher"

# Copy the custom systemd player service
cp ../r-audio.service "$INCLUDES/etc/systemd/system/r-audio.service"

# Copy the environment file template as the active configuration
cp ../r-audio.env.template "$INCLUDES/etc/default/r-audio"

# Configure custom system hostname (r-audio.local resolving)
echo "r-audio" > "$INCLUDES/etc/hostname"
cat <<EOF > "$INCLUDES/etc/hosts"
127.0.0.1   localhost r-audio
::1         localhost ip6-localhost ip6-loopback
ff02::1     ip6-allnodes
ff02::2     ip6-allrouters
EOF


# Copy the X11 bare startup script
cp ../xinitrc "$INCLUDES/home/kiosk/.xinitrc"

# 5. Configure TTY1 Auto-Login without passwords (standard kiosk practice)
cat <<EOF > "$INCLUDES/etc/systemd/system/getty@tty1.service.d/override.conf"
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin kiosk --noclear %I \$TERM
EOF

# 6. Configure Bash Profile to start X Server dynamically on login
cat <<EOF > "$INCLUDES/home/kiosk/.bash_profile"
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

# Binary-stage hook: patch every bootloader config in the tree to auto-boot.
# Runs late (9999-) so it executes after any live-build / d-i hooks that might
# regenerate these files. Diagnostic output is printed so we can confirm in CI logs.
cat <<'EOF' > config/hooks/normal/9999-autoboot.hook.binary
#!/bin/sh
set -e

echo "=========================================="
echo "AUTOBOOT HOOK: forcing immediate boot"
echo "=========================================="

# --- GRUB (UEFI — Surface Go 2 path) -------------------------------------
# Find every grub.cfg in the binary tree and force timeout=0 + hidden menu.
# Any pre-existing timeout/timeout_style/default lines are stripped first,
# then known-good values are injected at the very top.
find binary -type f -name "grub.cfg" 2>/dev/null | while read -r cfg; do
    echo "  [grub]    $cfg"
    sed -i -e '/^[[:space:]]*set[[:space:]]\+timeout[[:space:]]*=/d' \
           -e '/^[[:space:]]*set[[:space:]]\+timeout_style[[:space:]]*=/d' \
           -e '/^[[:space:]]*set[[:space:]]\+default[[:space:]]*=/d' \
           "$cfg"
    # Prepend a single block at the very top so it can't be overridden later
    { printf 'set default=0\nset timeout=0\nset timeout_style=hidden\n'; cat "$cfg"; } > "$cfg.new"
    mv "$cfg.new" "$cfg"
done

# --- ISOLINUX / SYSLINUX (legacy BIOS path) ------------------------------
find binary \( -name "isolinux.cfg" -o -name "syslinux.cfg" -o -name "live.cfg" -o -name "menu.cfg" -o -name "stdmenu.cfg" \) -type f 2>/dev/null | while read -r cfg; do
    echo "  [syslinux] $cfg"
    if grep -qi '^[[:space:]]*timeout' "$cfg"; then
        sed -i 's/^[[:space:]]*[Tt][Ii][Mm][Ee][Oo][Uu][Tt].*/TIMEOUT 1/' "$cfg"
    else
        echo 'TIMEOUT 1' >> "$cfg"
    fi
    if grep -qi '^[[:space:]]*prompt' "$cfg"; then
        sed -i 's/^[[:space:]]*[Pp][Rr][Oo][Mm][Pp][Tt].*/PROMPT 0/' "$cfg"
    fi
done

echo "=========================================="
echo "AUTOBOOT HOOK: complete"
echo "=========================================="
EOF
chmod +x config/hooks/normal/9999-autoboot.hook.binary

cat <<'EOF' > config/hooks/normal/0900-create-kiosk-user.hook.chroot
#!/bin/sh
# Add the dedicated kiosk user and assign proper sound/video groups
useradd -m -s /bin/bash -g users -G sudo,audio,video,input kiosk
chown -R kiosk:users /home/kiosk
chmod +x /home/kiosk/.xinitrc

# 2 GB swap file — prevents OOM during heavy workloads
fallocate -l 2G /swapfile
chmod 600 /swapfile
mkswap /swapfile
echo '/swapfile none swap sw 0 0' >> /etc/fstab

# Enable services (r-audio is NOT enabled here — the getty autologin
# restart loop handles that: getty -> bash_profile -> startx -> r-audio)
systemctl enable avahi-daemon
systemctl enable NetworkManager
EOF
chmod +x config/hooks/normal/0900-create-kiosk-user.hook.chroot

# 8. Compile the ISO
echo "Compiling the bootable hybrid Kiosk ISO..."
lb build

echo "=== SUCCESS! ==="
echo "Your custom bootable kiosk ISO has been created:"
ls -lh live-image-amd64.hybrid.iso
