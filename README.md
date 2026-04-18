# rust-digital-rain

A Matrix-style digital rain animated desktop wallpaper written in Rust. A falling stream of characters renders in each column of the screen, leaving glyphs behind at fixed positions as the lead character descends. Characters near the head rotate quickly, fade gradually, and occasionally flash bright before dimming back into the trail. Runs as a non-interactive background window via xwinwrap and alacritty at 24 fps.

---

![digital rain demonstration](demo.gif)

---

## Installation

### 1. Rust

These instructions assume Rust is already installed on your system. If not, visit [rust-lang.org](https://www.rust-lang.org/tools/install) and follow the instructions there.

### 2. xwinwrap

xwinwrap hosts the terminal emulator as a desktop-level window that sits behind all other windows and ignores mouse/keyboard input. Build it from source:

```bash
sudo apt install libx11-dev libxrender-dev libxext-dev
git clone https://github.com/mmhobi7/xwinwrap
cd xwinwrap
make
sudo make install
```

### 3. Alacritty

Alacritty is the terminal emulator used to render the animation. Visit [alacritty.org](https://alacritty.org) for full documentation.

```bash
sudo add-apt-repository ppa:aslatter/ppa
sudo apt update
sudo apt install alacritty
```

### 4. Matrix Code NFI font

The wallpaper uses the **Matrix Code NFI** font.

1. Download it from [dafont.com/matrix-code-nfi.font](https://www.dafont.com/matrix-code-nfi.font)
2. Extract the zip and double-click the `.ttf` file
3. Click **Install** in the font preview window that opens

### 5. Clone and build

```bash
git clone https://github.com/Kenneth-Posey/rust-digital-rain.git
cd rust-digital-rain
cargo build --release
```

### 6. Run as a background wallpaper

```bash
bash launch.sh
```

The script builds the binary (if not already built), then launches xwinwrap with alacritty embedded as a borderless, non-interactive desktop background window. The process will appear as `digital-rain` in your process explorer.

### Tuning the animation

`launch.sh` passes all animation parameters as explicit CLI flags — edit the values there to adjust the defaults:

| Flag | Default | Description |
|---|---|---|
| `--speed` | `1.0` | Column fall speed multiplier |
| `--fps` | `24` | Target frame rate |
| `--trail-length` | `60` | Maximum trail length in rows |
| `--flash-chance` | `5` | Chance (0–100) per tick a glyph flashes bright |
| `--rotation-speed` | `1.0` | Glyph rotation speed multiplier |

You can also pass flags directly when running the binary: `./target/release/rust-digital-rain --speed 0.5 --fps 30`

To stop it:

```bash
bash stop.sh
```
