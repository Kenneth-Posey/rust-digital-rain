#!/bin/bash
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cargo build --release --manifest-path "$DIR/Cargo.toml"

# Detect the primary screen resolution; fall back to 1920x1080
read -r SCREEN_W SCREEN_H < <(
  xrandr --current \
    | grep ' connected primary' \
    | grep -oP '\d+x\d+\+\d+\+\d+' \
    | head -1 \
    | cut -d'+' -f1 \
    | tr 'x' ' '
)
if [ -z "$SCREEN_W" ] || [ "$SCREEN_W" -eq 0 ] 2>/dev/null; then
  # No primary display labelled — fall back to first connected screen
  read -r SCREEN_W SCREEN_H < <(
    xrandr --current \
      | grep ' connected' \
      | grep -oP '\d+x\d+\+\d+\+\d+' \
      | head -1 \
      | cut -d'+' -f1 \
      | tr 'x' ' '
  )
fi
SCREEN_W=${SCREEN_W:-1920}
SCREEN_H=${SCREEN_H:-1080}

# Character cell dimensions for "Matrix Code NFI" at font.size=16.
# Derived from: 1920/210 ≈ 9 px/col, 1080/58 ≈ 19 px/line.
# Update these if you change font family or font size.
CELL_W=9
CELL_H=19

COLS=$(( SCREEN_W / CELL_W ))
LINES=$(( SCREEN_H / CELL_H ))

nohup xwinwrap -ni -fdt -un -g "${SCREEN_W}x${SCREEN_H}+0+0" -s -st -sp -b -nf -- \
  alacritty \
    --embed WID \
    --option 'colors.primary.background="#001900"' \
    --option 'window.decorations="None"' \
    --option "window.opacity=1.0" \
    --option 'font.normal.family="Matrix Code NFI"' \
    --option "font.size=16" \
    --option "window.dimensions.columns=${COLS}" \
    --option "window.dimensions.lines=${LINES}" \
    -e "$DIR/target/release/rust-digital-rain" \
        --speed 1.0 \
        --fps 24 \
        --trail-length 60 \
        --flash-chance 5 \
        --rotation-speed 1.0 \
  > /dev/null 2>&1 &

disown
echo "digital-rain started (PID $!) at ${SCREEN_W}x${SCREEN_H} → ${COLS}x${LINES} cells"
