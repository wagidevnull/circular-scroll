#!/bin/sh
# circular-scroll installer
#   ./install.sh                install (or update) and calibrate if needed
#   ./install.sh --recalibrate  re-run touchpad calibration
#   ./install.sh --uninstall    remove everything
set -e

BIN=/usr/local/bin/circular-scroll
UNIT=/etc/systemd/system/circular-scroll.service
CONF=/etc/circular-scroll.toml
MODLOAD=/etc/modules-load.d/circular-scroll-uinput.conf
CAL_SECONDS=15

cd "$(dirname "$0")"

# Re-run as root if needed
if [ "$(id -u)" -ne 0 ]; then
    echo "Root access is needed to install a system service."
    exec sudo sh "$0" "$@"
fi

say() { printf '\n\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\n\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

uninstall() {
    say "Removing circular-scroll"
    systemctl disable --now circular-scroll 2>/dev/null || true
    rm -f "$BIN" "$UNIT" "$CONF" "$MODLOAD"
    systemctl daemon-reload
    echo "Done. Your touchpad is back to normal."
    exit 0
}

calibrate() {
    # The daemon grabs the touchpad, so it must be stopped while calibrating.
    systemctl stop circular-scroll 2>/dev/null || true
    say "Calibrating your touchpad"
    until python3 "$BIN" --calibrate "$CAL_SECONDS" --write </dev/tty; do
        printf 'Calibration failed. Try again? [Y/n] '
        read -r ans </dev/tty
        case "$ans" in [nN]*) die "calibration skipped; re-run with --recalibrate" ;; esac
    done
}

case "$1" in
    --uninstall) uninstall ;;
    --recalibrate)
        [ -x "$BIN" ] || die "not installed yet; run ./install.sh first"
        calibrate
        systemctl restart circular-scroll
        echo "Recalibrated and restarted."
        exit 0 ;;
    ""|--install) ;;
    *) die "unknown option: $1" ;;
esac

command -v systemctl >/dev/null || die "systemd is required"
command -v python3 >/dev/null || die "python3 is required"
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' \
    || die "python 3.11 or newer is required"

say "Installing python-evdev"
if python3 -c 'import evdev' 2>/dev/null; then
    echo "already installed"
elif command -v pacman >/dev/null; then
    pacman -S --needed --noconfirm python-evdev
elif command -v apt-get >/dev/null; then
    apt-get install -y python3-evdev
elif command -v dnf >/dev/null; then
    dnf install -y python3-evdev
elif command -v zypper >/dev/null; then
    zypper --non-interactive install python3-evdev
else
    die "unknown package manager - install python-evdev yourself and re-run"
fi

say "Installing files"
install -Dm755 circular-scroll.py "$BIN"
install -Dm644 circular-scroll.service "$UNIT"
echo uinput > "$MODLOAD"
modprobe uinput
systemctl daemon-reload
echo "ok"

# Calibrate on first install (or if geometry was never recorded)
if ! [ -f "$CONF" ] || ! grep -qE '^center_x *= *[1-9]' "$CONF"; then
    calibrate
else
    echo "Existing calibration kept (use --recalibrate to redo it)."
fi

say "Starting service"
systemctl enable circular-scroll >/dev/null 2>&1
systemctl restart circular-scroll
sleep 2
if systemctl is-active --quiet circular-scroll; then
    journalctl -u circular-scroll -n 1 --no-pager -o cat
    cat <<EOF

Installed! Start a touch near the edge of the touchpad and circle around it
to scroll (clockwise = down). Touches elsewhere behave normally.

  Settings:      $CONF   (then: sudo systemctl restart circular-scroll)
  Recalibrate:   ./install.sh --recalibrate
  Pause:         sudo systemctl stop circular-scroll
  Uninstall:     ./install.sh --uninstall
EOF
else
    journalctl -u circular-scroll -n 15 --no-pager -o cat
    die "service failed to start (log above)"
fi
