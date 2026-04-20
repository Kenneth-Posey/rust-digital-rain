#!/bin/bash
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cargo build --release --manifest-path "$DIR/Cargo.toml"

# Detect the primary screen resolution; fall back to 1920x1080
read -r SCREEN_W SCREEN_H < <(
  xrandr --current \
    | awk '/connected primary/ {
        match($0, /([0-9]+)x([0-9]+)\+[0-9]+\+[0-9]+/, m)
        if (m[1]) { print m[1], m[2]; exit }
      }' \
)
if [ -z "$SCREEN_W" ] || [ "$SCREEN_W" -eq 0 ] 2>/dev/null; then
  # No primary display labelled — fall back to first connected screen
  read -r SCREEN_W SCREEN_H < <(
    xrandr --current \
      | awk '/connected/ {
          match($0, /([0-9]+)x([0-9]+)\+[0-9]+\+[0-9]+/, m)
          if (m[1]) { print m[1], m[2]; exit }
        }'
  )
fi
SCREEN_W=${SCREEN_W:-1920}
SCREEN_H=${SCREEN_H:-1080}

nohup xwinwrap -ni -fdt -un -g "${SCREEN_W}x${SCREEN_H}+0+0" -s -st -sp -b -nf -- \
  alacritty \
    --embed WID \
    --option 'colors.primary.background="#001900"' \
    --option 'window.decorations="None"' \
    --option "window.opacity=1.0" \
    --option 'font.normal.family="Matrix Code NFI"' \
    --option "font.size=16" \
    -e "$DIR/target/release/rust-digital-rain" \
        --speed 1.0 \
        --fps 24 \
        --trail-length 60 \
        --flash-chance 5 \
        --rotation-speed 1.0 \
  > /dev/null 2>&1 &

disown
echo "digital-rain started (PID $!) at ${SCREEN_W}x${SCREEN_H}"
