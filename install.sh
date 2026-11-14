#!/bin/sh
# circular-scroll installer
#   ./install.sh                install (or update) and calibrate if needed
#   ./install.sh --recalibrate  re-run touchpad calibration
#   ./install.sh --uninstall    remove everything
set -e

REPO=wagidevnull/circular-scroll
BIN=/usr/local/bin/circular-scroll
UNIT=/etc/systemd/system/circular-scroll.service
CONF=/etc/circular-scroll.toml
MODLOAD=/etc/modules-load.d/circular-scroll-uinput.conf
CAL_SECONDS=15

cd "$(dirname "$0")"

say() { printf '\n\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\n\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

case "$1" in
    ""|--install|--recalibrate|--uninstall) ;;
    *) die "unknown option: $1" ;;
esac

# ------------------------------------------------------------------ get the program
# Done before switching to root, so cargo builds as you (with your toolchain).
# Without cargo, a prebuilt binary is downloaded from GitHub Releases.
fetch_binary() {
    arch=$(uname -m)
    out=target/circular-scroll-$arch
    url=https://github.com/$REPO/releases/latest/download/circular-scroll-$arch
    say "Downloading circular-scroll for $arch"
    mkdir -p target
    if command -v curl >/dev/null; then
        get() { curl -fsSL -o "$2" "$1"; }
    elif command -v wget >/dev/null; then
        get() { wget -qO "$2" "$1"; }
    else
        die "need cargo (to build) or curl/wget (to download)"
    fi
    get "$url" "$out" || die "no prebuilt binary for $arch - install Rust (cargo) and re-run to build it"
    get "$url.sha256" "$out.sha256" || die "couldn't download the checksum"
    (cd target && sed "s|  .*|  circular-scroll-$arch|" "circular-scroll-$arch.sha256" | sha256sum -c --quiet -) \
        || die "checksum mismatch - download corrupted?"
    chmod +x "$out"
    echo "ok"
    NEW_BIN=$out
}

if [ -z "$NEW_BIN" ] && [ "$1" != "--uninstall" ] && [ "$1" != "--recalibrate" ]; then
    if command -v cargo >/dev/null && [ -f Cargo.toml ]; then
        say "Building circular-scroll"
        cargo build --release --locked
        NEW_BIN=target/release/circular-scroll
    else
        fetch_binary
    fi
fi

# ------------------------------------------------------------------ switch to root
if [ "$(id -u)" -ne 0 ]; then
    echo "Root access is needed to install a system service."
    exec sudo NEW_BIN="$NEW_BIN" sh "$0" "$@"
fi

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
    until "$BIN" --calibrate "$CAL_SECONDS" --write </dev/tty; do
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
esac

command -v systemctl >/dev/null || die "systemd is required"
[ -n "$NEW_BIN" ] && [ -x "$NEW_BIN" ] || die "no binary to install"

say "Installing files"
systemctl stop circular-scroll 2>/dev/null || true
install -Dm755 "$NEW_BIN" "$BIN"
install -Dm644 circular-scroll.service "$UNIT"
echo uinput > "$MODLOAD"
modprobe uinput
systemctl daemon-reload
"$BIN" --version

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
