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

## Run as a background wallpaper

```bash
bash launch.sh
```

The script builds the binary (if not already built), then launches xwinwrap with alacritty embedded as a borderless, non-interactive desktop background window. The process will appear as `digital-rain` in your process explorer.

## Stop the wallpaper

```bash
bash stop.sh
```

## Tuning the animation

`launch.sh` passes all animation parameters as explicit CLI flags — edit the values there to adjust the defaults:

| Flag | Default | Description |
|---|---|---|
| `--speed` | `1.0` | Column fall speed multiplier |
| `--fps` | `24` | Target frame rate |
| `--trail-length` | `60` | Maximum trail length in rows |
| `--flash-chance` | `5` | Chance (0–100) per tick a glyph flashes bright |
| `--rotation-speed` | `1.0` | Glyph rotation speed multiplier |
| `--config` | config.yml | Configuration file path 

---

## Source Code Rain

Instead of random glyphs, the rain can display real lines of source code read from your own projects. Each column is assigned one line from a source file; characters spin randomly as the head falls, then snap to the correct character once they settle. Keyword characters (e.g. `fn`, `class`, `return`) are highlighted in brighter green when keyword highlighting is enabled.

## Config file format

```yaml
# Extensions to include when scanning directories (global)
extensions:
  - rs
  - py
  - ts

# One entry per directory; options are per-path
paths:
  - path: /path/to/my/project/src
    show_file_path: true      # display filename in bottom-right corner
    highlight_keywords: true  # brighter colour for language keywords

  - path: /path/to/other/project
    show_file_path: false
    highlight_keywords: false

# Keyword lists — applied when highlight_keywords is true.
# Keys are language names (rust, python, javascript, etc.)
keywords:
  rust:
    - fn
    - pub
    - async
    # … see config.yml for the full list
```

### Supported languages for keyword highlighting

Java, Kotlin, C#, Rust, Python, JavaScript, TypeScript, F#, Haskell, Swift, Clojure, PHP, COBOL, Visual Basic / VB.NET, SQL, C++, C, Ruby, Dart, R.

### Adding private paths with `config.secret.yml`

To add directories from private projects without committing them, create a `config.secret.yml` file **in the same directory as `config.yml`**. It uses the exact same format and is automatically merged at startup. It is already listed in `.gitignore`.

#### 1. Create the file next to `config.yml`

   ```bash touch config.secret.yml
   ```

#### 2. Add your private paths (and optionally extra extensions or keywords)

   ```yaml
   paths:
     - path: /home/you/work/my-private-repo/src
       show_file_path: true
       highlight_keywords: true
     - path: /home/you/personal/side-project
       show_file_path: true
       highlight_keywords: false
   ```

#### 3. Run as normal — your private paths will be included in the file rotation alongside the public ones

   ```bash launch.sh
   ```
