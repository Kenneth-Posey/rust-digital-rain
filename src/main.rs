use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use clap::Parser;

mod source;

static RUNNING: AtomicBool = AtomicBool::new(true);

fn nonzero_usize(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    if n == 0 {
        Err("value must be at least 1".into())
    } else {
        Ok(n)
    }
}

extern "C" fn handle_signal(_: libc::c_int) {
    RUNNING.store(false, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// CLI configuration
// ---------------------------------------------------------------------------

/// Matrix-style digital rain desktop wallpaper
#[derive(Parser)]
#[command(about)]
struct Config {
    /// Column fall speed multiplier (default: 1.0 → 0.10–0.20 rows/tick at 24 fps)
    #[arg(long, default_value_t = 1.0)]
    speed: f32,

    /// Target frame rate in frames per second (default: 24)
    #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u64).range(1..))]
    fps: u64,

    /// Maximum trail length in rows; minimum is ~37% of this, chosen randomly
    /// per column (default: 60)
    #[arg(long, default_value_t = 60, value_parser = nonzero_usize)]
    trail_length: usize,

    /// Chance (0–100) per tick that a trail glyph flashes bright (default: 5)
    #[arg(long, default_value_t = 5.0)]
    flash_chance: f64,

    /// Glyph rotation speed multiplier; affects both fast and slow zones
    /// (default: 1.0)
    #[arg(long, default_value_t = 1.0)]
    rotation_speed: f32,

    /// Path to a YAML config file enabling source-code rain mode
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
}

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
    Terminal,
};

// ---------------------------------------------------------------------------
// Glyph pool
// ---------------------------------------------------------------------------

const GLYPHS: &[char] = &[
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm',
    'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
    '$', '+', '-', '*', '/', '=', '%', '"', '|', '\'', '#', '&', '_',
    '(', ')', ',', '.', ';', ':', '?', '!', '\\', '{', '}', '<', '>',
    '[', ']', '^', '~', '`',
];

fn random_glyph(rng: &mut impl rand::RngExt) -> char {
    GLYPHS[rng.random_range(0..GLYPHS.len())]
}

// ---------------------------------------------------------------------------
// Cell — a character placed at a fixed screen row
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Cell {
    ch: char,
    /// 1.0 = freshly placed, 0.0 = fully faded
    brightness: f32,
    /// Brightness decrease per tick
    fade_rate: f32,
    /// Rotation speed while within fast_threshold rows of the head
    fast_rotate_speed: f32,
    /// Rotation speed beyond fast_threshold (changes every 1–3 seconds)
    slow_rotate_speed: f32,
    accum: f32,
    /// Flash intensity: 1.0 = head colour, 0.0 = normal trail colour
    flash: f32,
    flash_decay: f32,
    /// The stable target character for source-code rain columns; None in random mode
    target_ch: Option<char>,
    /// True once the cell has snapped to its target_ch
    settled: bool,
    /// True if this character falls inside a keyword token
    is_keyword: bool,
}
// ---------------------------------------------------------------------------
// Column state
// ---------------------------------------------------------------------------

struct Column {
    x: u16,
    height: u16,
    /// Falling head position in rows (can be negative = above screen)
    head_y: f32,
    speed: f32,
    /// Fixed-position cells indexed by row; characters stay put when placed
    cells: Vec<Option<Cell>>,
    /// How many rows the trail lasts before fully fading
    trail_rows: usize,
    /// False once the head has exited the bottom of the screen
    head_active: bool,
    delay: u32,
    delay_counter: u32,
    /// Rows behind the head below which rotation slows to 1–3 sec intervals
    fast_threshold: u16,
    // Source-code rain fields
    source_chars: Option<Vec<char>>,
    source_kw: Option<Vec<bool>>,
    char_index: usize,
    source_file: Option<Arc<PathBuf>>,
    show_file_path: bool,
    highlight_keywords: bool,
}

impl Column {
    fn new(
        x: u16,
        height: u16,
        cfg: &Config,
        rng: &mut impl rand::RngExt,
        actor: Option<&source::LineActorHandle>,
    ) -> Self {
        let speed = (rng.random_range(3..=8) as f32 * 0.02 + 0.04) * cfg.speed;
        let head_y = -(rng.random_range(0..height) as f32);

        let (source_chars, source_kw, trail_rows, source_file, show_file_path, highlight_keywords) =
            Self::pick_line(cfg, actor);

        Self {
            x,
            height,
            head_y,
            speed,
            cells: vec![None; height as usize],
            trail_rows,
            head_active: true,
            delay: rng.random_range(10..60),
            delay_counter: 0,
            fast_threshold: rng.random_range(3..=5),
            source_chars,
            source_kw,
            char_index: 0,
            source_file,
            show_file_path,
            highlight_keywords,
        }
    }

    /// Ask the actor for the next line; fall back to random if unavailable.
    fn pick_line(
        cfg: &Config,
        actor: Option<&source::LineActorHandle>,
    ) -> (Option<Vec<char>>, Option<Vec<bool>>, usize, Option<Arc<PathBuf>>, bool, bool) {
        if let Some(a) = actor {
            if let Some(line) = a.next_line() {
                let trail = line.chars.len().max(1);
                return (
                    Some(line.chars),
                    Some(line.is_keyword),
                    trail,
                    Some(line.file),
                    line.show_file_path,
                    line.highlight_keywords,
                );
            }
        }
        // Random mode
        let _trail_min =
            ((cfg.trail_length as f32 * 0.37) as usize).max(1).min(cfg.trail_length);
        // trail_rows computed at call site when rng is available; use default here
        (None, None, cfg.trail_length, None, false, false)
    }

    fn tick(
        &mut self,
        cfg: &Config,
        rng: &mut impl rand::RngExt,
        actor: Option<&source::LineActorHandle>,
    ) {
        if !self.head_active {
            self.delay_counter += 1;
            self.update_cells(cfg, rng);
            if self.delay_counter >= self.delay && self.cells.iter().all(|c| c.is_none()) {
                self.restart(cfg, rng, actor);
            }
            return;
        }

        let prev_row = self.head_y as i32;
        self.head_y += self.speed;
        let curr_row = self.head_y as i32;

        let fps = cfg.fps as f32;
        let slow_min = (fps * 8.0) as u32;
        let slow_max = (fps * 10.0) as u32;
        for row in (prev_row + 1)..=(curr_row) {
            if row >= 0 && row < self.height as i32 {
                // Skip the last row when showing the file path overlay
                if self.show_file_path && row == self.height as i32 - 1 {
                    continue;
                }
                let (target_ch, is_keyword) = if let Some(ref sc) = self.source_chars {
                    let ch = sc.get(self.char_index).copied();
                    let kw = self
                        .source_kw
                        .as_ref()
                        .and_then(|v| v.get(self.char_index).copied())
                        .unwrap_or(false);
                    self.char_index += 1;
                    (ch, kw)
                } else {
                    (None, false)
                };

                // A space target means an empty slot — don't place a visible cell
                if target_ch == Some(' ') {
                    // char_index already incremented above
                } else {
                    self.cells[row as usize] = Some(Cell {
                        ch: random_glyph(rng),
                        brightness: 1.0,
                        fade_rate: self.speed / self.trail_rows as f32,
                        fast_rotate_speed: self.speed * rng.random_range(2..=3) as f32
                            * cfg.rotation_speed,
                        slow_rotate_speed: cfg.rotation_speed
                            / rng.random_range(slow_min..=slow_max) as f32,
                        accum: 0.0,
                        flash: 0.0,
                        flash_decay: 0.0,
                        target_ch,
                        settled: false,
                        is_keyword,
                    });
                }
            }
        }

        if self.head_y >= self.height as f32 {
            self.head_active = false;
            self.delay_counter = 0;
        }

        self.update_cells(cfg, rng);
    }

    fn update_cells(&mut self, cfg: &Config, rng: &mut impl rand::RngExt) {
        let head_row = self.head_y as i32;
        let mut flash_candidates: Vec<usize> = Vec::new();
        let highlight_keywords = self.highlight_keywords;

        for (row, cell_opt) in self.cells.iter_mut().enumerate() {
            if let Some(cell) = cell_opt.as_mut() {
                let dist = (head_row - row as i32).max(0) as u16;
                let rot = if dist == 0 {
                    cell.fast_rotate_speed * 4.0
                } else if dist < self.fast_threshold {
                    cell.fast_rotate_speed
                } else {
                    cell.slow_rotate_speed
                };
                cell.accum += rot;
                if cell.accum >= 1.0 {
                    cell.accum -= 1.0;
                    if dist >= self.fast_threshold as u16 {
                        if let Some(tc) = cell.target_ch {
                            if !cell.settled {
                                // First time in slow zone: snap to target
                                cell.ch = if tc == ' ' { tc } else { tc };
                                cell.settled = true;
                            } else if highlight_keywords {
                                // Locked — no drift when keyword highlighting is on
                            } else {
                                // Drift: alternate random then snap back
                                if cell.ch == tc {
                                    cell.ch = random_glyph(rng);
                                } else {
                                    cell.ch = tc;
                                }
                            }
                        } else {
                            cell.ch = random_glyph(rng);
                        }
                    } else {
                        cell.ch = random_glyph(rng);
                    }
                }

                cell.brightness = (cell.brightness - cell.fade_rate).max(0.0);

                if cell.flash > 0.0 {
                    cell.flash = (cell.flash - cell.flash_decay).max(0.0);
                }

                if dist > 0
                    && dist <= (self.trail_rows / 2) as u16
                    && cell.flash == 0.0
                    && cell.brightness > 0.3
                {
                    flash_candidates.push(row);
                }
            }

            if matches!(cell_opt, Some(c) if c.brightness == 0.0) {
                *cell_opt = None;
            }
        }

        let chance = (cfg.flash_chance / 100.0).clamp(0.0, 1.0);
        if !flash_candidates.is_empty() && rng.random_bool(chance) {
            let pick = flash_candidates[rng.random_range(0..flash_candidates.len())];
            if let Some(cell) = &mut self.cells[pick] {
                cell.flash = 1.0;
                let fps = cfg.fps as f32;
                cell.flash_decay =
                    1.0 / rng.random_range((fps * 5.0) as u32..=(fps * 8.0) as u32) as f32;
            }
        }
    }

    fn restart(
        &mut self,
        cfg: &Config,
        rng: &mut impl rand::RngExt,
        actor: Option<&source::LineActorHandle>,
    ) {
        self.speed = (rng.random_range(3..=8) as f32 * 0.02 + 0.04) * cfg.speed;
        self.head_y = -(rng.random_range(0..self.height / 2) as f32);
        self.cells.iter_mut().for_each(|c| *c = None);
        self.head_active = true;
        self.delay = rng.random_range(10..60);
        self.delay_counter = 0;
        self.fast_threshold = rng.random_range(3..=5);
        self.char_index = 0;

        let (source_chars, source_kw, trail_rows, source_file, show_file_path, highlight_keywords) =
            Self::pick_line(cfg, actor);

        self.source_chars = source_chars;
        self.source_kw = source_kw;
        self.trail_rows = if self.source_chars.is_some() {
            trail_rows
        } else {
            let trail_min =
                ((cfg.trail_length as f32 * 0.37) as usize).max(1).min(cfg.trail_length);
            rng.random_range(trail_min..=cfg.trail_length)
        };
        self.source_file = source_file;
        self.show_file_path = show_file_path;
        self.highlight_keywords = highlight_keywords;
    }
}

// ---------------------------------------------------------------------------
// Rain widget
// ---------------------------------------------------------------------------

struct Rain<'a> {
    columns: &'a [Column],
    /// If Some, the last row is reserved for the file path overlay and skipped
    safe_rows: u16,
}

impl<'a> Widget for Rain<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let max_row = area.height.saturating_sub(self.safe_rows);
        for col in self.columns {
            let cx = area.x + col.x;
            if cx >= area.right() {
                continue;
            }

            let head_row = col.head_y as i32;

            for (row, cell_opt) in col.cells.iter().enumerate() {
                let Some(cell) = cell_opt else { continue };
                if row >= max_row as usize {
                    continue;
                }
                let cy = area.y + row as u16;
                let is_head = head_row == row as i32;

                let (base_r, base_g, base_b) = if is_head {
                    (200u8, 255u8, 225u8)
                } else if col.highlight_keywords && cell.settled && cell.is_keyword && cell.flash == 0.0 {
                    // Bright keyword colour
                    let g = (cell.brightness.powf(1.5) * 255.0) as u8;
                    let g = g.max(18);
                    (0u8, g, (g as f32 * 0.10) as u8)
                } else {
                    let g = (cell.brightness.powf(1.5) * 220.0) as u8;
                    let g = g.max(18);
                    (0u8, g, (g as f32 * 0.10) as u8)
                };

                // Keyword-settled cells get a brighter, more saturated green
                let base_color = if col.highlight_keywords && cell.settled && cell.is_keyword && !is_head && cell.flash == 0.0 {
                    let g = (cell.brightness.powf(1.2) * 255.0).min(255.0) as u8;
                    let g = g.max(60);
                    Color::Rgb(0, g, (g as f32 * 0.15) as u8)
                } else {
                    Color::Rgb(base_r, base_g, base_b)
                };

                let color = if cell.flash > 0.0 && !is_head {
                    let f = cell.flash;
                    Color::Rgb(
                        (base_r as f32 + (200.0 - base_r as f32) * f) as u8,
                        (base_g as f32 + (255.0 - base_g as f32) * f) as u8,
                        (base_b as f32 + (225.0 - base_b as f32) * f) as u8,
                    )
                } else {
                    base_color
                };

                buf[(cx, cy)]
                    .set_char(cell.ch)
                    .set_bg(Color::Rgb(0, 25, 0))
                    .set_style(if is_head {
                        Style::default().fg(color).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(color)
                    });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct App {
    columns: Vec<Column>,
    width: u16,
    height: u16,
    rng: rand::rngs::ThreadRng,
    config: Config,
    line_actor: Option<source::LineActorHandle>,
    display_file: Option<Arc<PathBuf>>,
}

impl App {
    fn new(
        width: u16,
        height: u16,
        config: Config,
        line_actor: Option<source::LineActorHandle>,
    ) -> Self {
        let mut rng = rand::rng();
        let actor_ref = line_actor.as_ref();
        let columns = (0..width)
            .map(|x| Column::new(x, height, &config, &mut rng, actor_ref))
            .collect();
        Self { columns, width, height, rng, config, line_actor, display_file: None }
    }

    fn resize(&mut self, width: u16, height: u16) {
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        let cfg = &self.config;
        let rng = &mut self.rng;
        let actor_ref = self.line_actor.as_ref();
        self.columns = (0..width)
            .map(|x| Column::new(x, height, cfg, rng, actor_ref))
            .collect();
    }

    fn tick(&mut self) {
        let cfg = &self.config;
        let actor_ref = self.line_actor.as_ref();
        for col in &mut self.columns {
            col.tick(cfg, &mut self.rng, actor_ref);
            // Update display file when a column has show_file_path and a source file
            if col.show_file_path {
                if let Some(ref f) = col.source_file {
                    self.display_file = Some(Arc::clone(f));
                }
            }
        }
    }

    fn show_file_path_active(&self) -> bool {
        self.columns.iter().any(|c| c.show_file_path)
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> io::Result<()> {
    // Set process name visible in htop/ps
    proctitle::set_title("digital-rain");
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_NAME, c"digital-rain".as_ptr(), 0, 0, 0);
        libc::signal(libc::SIGTERM, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT,  handle_signal as *const () as libc::sighandler_t);
    }

    let config = Config::parse();
    let tick_rate = Duration::from_millis(1000 / config.fps);

    // Optionally start the LineActor from a config file
    let line_actor = config.config.as_deref().and_then(|path| {
        match source::load_config(path) {
            Ok(src_cfg) => {
                let handle = source::spawn(src_cfg);
                Some(handle)
            }
            Err(e) => {
                eprintln!("warning: could not load source config: {e} — using random mode");
                None
            }
        }
    });

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    execute!(stdout, crossterm::cursor::Hide)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let size = terminal.size()?;
    let mut app = App::new(size.width, size.height, config, line_actor);
    let mut last_tick = Instant::now();

    while RUNNING.load(Ordering::SeqCst) {
        let show_overlay = app.show_file_path_active() && app.display_file.is_some();
        let safe_rows = if show_overlay { 1 } else { 0 };
        let display_file = app.display_file.clone();

        terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Rain { columns: &app.columns, safe_rows }, area);

            // File path overlay — bottom-right, last row
            if show_overlay {
                if let Some(ref fpath) = display_file {
                    let label = format_file_label(fpath);
                    let label_len = label.chars().count() as u16;
                    let row = area.height.saturating_sub(1);
                    let col_start = area.width.saturating_sub(label_len);
                    let overlay_style = Style::default()
                        .fg(Color::Rgb(140, 200, 140))
                        .bg(Color::Rgb(0, 15, 0));
                    let buf = frame.buffer_mut();
                    for (i, ch) in label.chars().enumerate() {
                        let x = col_start + i as u16;
                        if x < area.width {
                            buf[(x, row)].set_char(ch).set_style(overlay_style);
                        }
                    }
                }
            }
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            if let Event::Resize(w, h) = event::read()? {
                app.resize(w, h);
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.tick();
            last_tick = Instant::now();
        }
    }

    let r1 = disable_raw_mode();
    let r2 = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::cursor::Show,
    );
    r1.and(r2)
}

fn format_file_label(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let display = path.display().to_string();
    format!(" {name} — {display} ")
}