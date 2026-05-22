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
# --debian-installer true (was: live) → ship the standard text-mode d-i,
# which honors preseed cleanly. The "live" installer is calamares-style and
# does not respect d-i preseed answers the same way.
lb config \
  --binary-images iso-hybrid \
  --architectures amd64 \
  --distribution "$DEBIAN_VERSION" \
  --archive-areas "main contrib non-free non-free-firmware" \
  --debian-installer true \
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

# 7. Unattended-install payload: drop preseed + R-Audio binaries onto the
#    ISO root so the Debian Installer can read them at /cdrom/... and the
#    preseed late_command can copy them into the freshly installed system.
echo "Staging unattended-install preseed and R-Audio payload..."
BIN_INCLUDES="config/includes.binary"
PAYLOAD_DIR="$BIN_INCLUDES/r-audio-payload"
mkdir -p "$PAYLOAD_DIR"

# preseed.cfg goes at the ISO root → /cdrom/preseed.cfg from the installer's view
cp ../preseed.cfg "$BIN_INCLUDES/preseed.cfg"

# Everything the post-install script needs to wire up the installed system
cp "../$BINARY_PATH"        "$PAYLOAD_DIR/r-audio"
cp "../$SETUP_BINARY_PATH"  "$PAYLOAD_DIR/r-audio-setup"
cp ../r-audio-launcher.sh   "$PAYLOAD_DIR/r-audio-launcher"
cp ../r-audio.service       "$PAYLOAD_DIR/r-audio.service"
cp ../r-audio.env.template  "$PAYLOAD_DIR/r-audio.env"
cp ../xinitrc               "$PAYLOAD_DIR/xinitrc"
cp ../postinstall.sh        "$PAYLOAD_DIR/postinstall.sh"
chmod +x "$PAYLOAD_DIR/r-audio" \
         "$PAYLOAD_DIR/r-audio-setup" \
         "$PAYLOAD_DIR/r-audio-launcher" \
         "$PAYLOAD_DIR/postinstall.sh"

# 8. Bootloader overrides — replace live-build's templates with a self-contained
#    config whose default entry is the unattended installer (timeout 0, hidden).
#    Live mode and a manual installer entry are kept as menu fallbacks.
#
#    IMPORTANT: live-build's binary_grub_cfg / binary_syslinux steps do sed
#    substitution on these files, replacing @KERNEL_DI@ / @INITRD_DI@ /
#    @KERNEL_LIVE@ / @INITRD_LIVE@ / @APPEND_INSTALL@ / @APPEND_LIVE@ with
#    the real on-ISO paths and args. Use the placeholders — hardcoding paths
#    like /install.amd/vmlinuz breaks because current live-build emits
#    /install/vmlinuz instead.
#
#    `grub-pc` controls BOTH the BIOS GRUB menu and the UEFI menu — the
#    EFI grub.cfg inside efi.img is a tiny stub that just redirects to
#    /boot/grub/grub.cfg (which is generated from this directory).
echo "Writing bootloader overrides (default = unattended installer)..."
mkdir -p config/bootloaders/grub-pc config/bootloaders/isolinux

cat <<'GRUBCFG' > config/bootloaders/grub-pc/grub.cfg
# R-Audio Kiosk GRUB config — overrides live-build template.
# Default entry is the unattended installer; menu is hidden with timeout 0.
# Hold Esc/Shift during boot to interrupt and pick a different entry.
# @KERNEL_*@ / @INITRD_*@ / @APPEND_*@ are replaced at build time by
# live-build's binary_grub_cfg step with real ISO paths and args.

if loadfont $prefix/font.pf2 ; then
    set gfxmode=auto
    insmod all_video
    insmod gfxterm
    terminal_output gfxterm
fi

set default=0
set timeout=0
set timeout_style=hidden

menuentry "R-Audio Kiosk - Auto-Install (WIPES DISK)" {
    linux   @KERNEL_DI@ auto=true priority=critical preseed/file=/cdrom/preseed.cfg vga=788 @APPEND_INSTALL@ --- quiet
    initrd  @INITRD_DI@
}

menuentry "Live system (debug fallback)" {
    linux   @KERNEL_LIVE@ boot=live components @APPEND_LIVE@ quiet splash
    initrd  @INITRD_LIVE@
}

menuentry "Manual install (interactive)" {
    linux   @KERNEL_DI@ vga=788 @APPEND_INSTALL@ --- quiet
    initrd  @INITRD_DI@
}
GRUBCFG

# Legacy BIOS path — Surface Go 2 boots UEFI, but include this so the ISO
# is also bootable on plain BIOS hardware for development. Same @VAR@
# placeholders apply; binary_syslinux does the substitution.
cat <<'ISOCFG' > config/bootloaders/isolinux/isolinux.cfg
default unattended
prompt 0
timeout 1

label unattended
    linux  @KERNEL_DI@
    initrd @INITRD_DI@
    append vga=788 auto=true priority=critical preseed/file=/cdrom/preseed.cfg @APPEND_INSTALL@ --- quiet

label live
    linux  @KERNEL_LIVE@
    initrd @INITRD_LIVE@
    append boot=live components @APPEND_LIVE@ quiet splash

label install
    linux  @KERNEL_DI@
    initrd @INITRD_DI@
    append vga=788 @APPEND_INSTALL@ --- quiet
ISOCFG

# 9. Post-configuration setup (create kiosk user during image creation)
mkdir -p config/hooks/normal

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

# 10. Compile the ISO
echo "Compiling the bootable hybrid Kiosk ISO..."
lb build

echo "=== SUCCESS! ==="
echo "Your custom bootable kiosk ISO has been created:"
ls -lh live-image-amd64.hybrid.iso
