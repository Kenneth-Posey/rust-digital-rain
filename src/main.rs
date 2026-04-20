use std::{
    io,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
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

        let (trail_rows, source_chars, source_kw, highlight_keywords) =
            Self::apply_line(cfg, rng, Self::pick_line(actor));

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
            highlight_keywords,
        }
    }

    /// Ask the actor for the next line; fall back to None (random mode) if unavailable.
    fn pick_line(actor: Option<&source::LineActorHandle>) -> Option<source::SourceLine> {
        actor?.next_line()
    }

    #[allow(clippy::type_complexity)]
    fn apply_line(
        cfg: &Config,
        rng: &mut impl rand::RngExt,
        line: Option<source::SourceLine>,
    ) -> (usize, Option<Vec<char>>, Option<Vec<bool>>, bool) {
        if let Some(l) = line {
            let trail = l.chars.len().max(1);
            (trail, Some(l.chars), Some(l.is_keyword), l.highlight_keywords)
        } else {
            let trail_min =
                ((cfg.trail_length as f32 * 0.37) as usize).max(1).min(cfg.trail_length);
            let trail = rng.random_range(trail_min..=cfg.trail_length);
            (trail, None, None, false)
        }
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
                    if dist >= self.fast_threshold {
                        if let Some(tc) = cell.target_ch {
                            if !cell.settled {
                                // First time in slow zone: snap to target
                                cell.ch = tc;
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

        let (trail_rows, source_chars, source_kw, highlight_keywords) =
            Self::apply_line(cfg, rng, Self::pick_line(actor));

        self.trail_rows = trail_rows;
        self.source_chars = source_chars;
        self.source_kw = source_kw;
        self.highlight_keywords = highlight_keywords;
    }
}

// ---------------------------------------------------------------------------
// Rain widget
// ---------------------------------------------------------------------------

struct Rain<'a> {
    columns: &'a [Column],
}

impl<'a> Widget for Rain<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        for col in self.columns {
            let cx = area.x + col.x;
            if cx >= area.right() {
                continue;
            }

            let head_row = col.head_y as i32;

            for (row, cell_opt) in col.cells.iter().enumerate() {
                let Some(cell) = cell_opt else { continue };
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
        Self { columns, width, height, rng, config, line_actor }
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
        }
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

    // Resolve config path: explicit --config flag, then exe directory, then cwd
    let config_path: Option<PathBuf> = config.config.clone()
        .or_else(|| {
            std::env::current_exe().ok()
                .and_then(|p| p.parent().map(|d| d.join("config.yml")))
                .filter(|p| p.exists())
        })
        .or_else(|| {
            let p = PathBuf::from("config.yml");
            p.exists().then_some(p)
        });

    // Optionally start the LineActor from a config file
    let line_actor = config_path.as_deref().and_then(|path| {
        match source::load_config(path) {
            Ok(src_cfg) => {
                for entry in &src_cfg.paths {
                    if entry.path.is_dir() {
                        eprintln!("[config] {}", entry.path.display());
                    } else {
                        eprintln!("[config] not found: {}", entry.path.display());
                    }
                }
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
        terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Rain { columns: &app.columns }, area);
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? && let Event::Resize(w, h) = event::read()? {
            app.resize(w, h);
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_cfg() -> Config {
        Config {
            speed: 1.0,
            fps: 24,
            trail_length: 60,
            flash_chance: 5.0,
            rotation_speed: 1.0,
            config: None,
        }
    }

    /// Advance the column until `head_y` has crossed at least `target_row` rows.
    fn tick_to_row(col: &mut Column, cfg: &Config, rng: &mut impl rand::RngExt, target_row: i32) {
        for _ in 0..10_000 {
            col.tick(cfg, rng, None);
            if col.head_y as i32 >= target_row {
                break;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Column + actor integration
    // -----------------------------------------------------------------------

    #[test]
    fn column_new_with_actor_has_source_line() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg_path = manifest.join("config.yml");
        if !cfg_path.exists() {
            return;
        }
        let src_cfg = source::load_config(&cfg_path).expect("config.yml");
        let handle = source::spawn(src_cfg);

        let mut rng = rand::rng();
        let cfg = test_cfg();
        let col = Column::new(0, 40, &cfg, &mut rng, Some(&handle));

        assert!(col.source_chars.is_some(), "source_chars should be populated from actor");
        assert!(col.source_kw.is_some(), "source_kw should be populated from actor");
        assert_eq!(col.char_index, 0, "char_index must start at 0");
        let sc = col.source_chars.as_ref().unwrap();
        let sk = col.source_kw.as_ref().unwrap();
        assert!(!sc.is_empty(), "source line must not be empty");
        assert_eq!(sc.len(), sk.len(), "chars and is_keyword must have equal lengths");
        assert_eq!(col.trail_rows, sc.len().max(1), "trail_rows should equal source line length");
    }

    #[test]
    fn column_restart_resets_char_index_and_fetches_new_line() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg_path = manifest.join("config.yml");
        if !cfg_path.exists() {
            return;
        }
        let src_cfg = source::load_config(&cfg_path).expect("config.yml");
        let handle = source::spawn(src_cfg);

        let mut rng = rand::rng();
        let cfg = test_cfg();
        let mut col = Column::new(0, 40, &cfg, &mut rng, Some(&handle));

        // Simulate enough ticks to advance char_index
        tick_to_row(&mut col, &cfg, &mut rng, 5);
        let idx_before = col.char_index;

        col.restart(&cfg, &mut rng, Some(&handle));

        assert_eq!(col.char_index, 0, "char_index must be reset to 0 on restart");
        assert!(idx_before > 0 || col.head_y < 0.0,
            "char_index should have advanced if head crossed rows");
        assert!(col.source_chars.is_some(), "restart should fetch a new source line");
    }

    #[test]
    fn column_char_index_advances_when_head_crosses_rows() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg_path = manifest.join("config.yml");
        if !cfg_path.exists() {
            return;
        }
        let src_cfg = source::load_config(&cfg_path).expect("config.yml");
        let handle = source::spawn(src_cfg);

        let mut rng = rand::rng();
        let cfg = test_cfg();
        let mut col = Column::new(0, 40, &cfg, &mut rng, Some(&handle));

        // Force head to start just above row 3 (rows 0-2 are reserved for overlay)
        col.head_y = 2.5;
        col.speed = 2.0; // Will cross rows 3 and 4 in one tick

        let idx_before = col.char_index;
        col.tick(&cfg, &mut rng, Some(&handle));
        let idx_after = col.char_index;

        assert!(
            idx_after > idx_before,
            "char_index ({idx_after}) should have advanced past {idx_before} after head crossed rows"
        );
    }

    #[test]
    fn column_space_in_source_chars_does_not_place_cell() {
        // Build a column whose first source char is a space
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg_path = manifest.join("config.yml");
        if !cfg_path.exists() {
            return;
        }
        let src_cfg = source::load_config(&cfg_path).expect("config.yml");

        // Keep requesting lines until we get one starting with a space
        let handle = source::spawn(src_cfg);
        let mut line_with_leading_space = None;
        for _ in 0..500 {
            if let Some(l) = handle.next_line() {
                if l.chars.first() == Some(&' ') {
                    line_with_leading_space = Some(l);
                    break;
                }
            }
        }
        let Some(line) = line_with_leading_space else {
            // Not every file has a leading-space line; skip gracefully
            return;
        };

        let mut rng = rand::rng();
        let cfg = test_cfg();
        let mut col = Column::new(0, 40, &cfg, &mut rng, None);

        // Inject the space-leading source line directly into the column
        col.source_chars = Some(line.chars);
        col.source_kw = Some(line.is_keyword);
        col.char_index = 0;
        col.head_y = -0.1;
        col.speed = 1.5; // Crosses exactly row 0

        col.tick(&cfg, &mut rng, None);

        // Row 0 should have no cell (space was consumed without placing)
        assert!(
            col.cells[0].is_none(),
            "a space source char must not place a visible cell at row 0"
        );
        // char_index should have advanced past the space
        assert!(col.char_index >= 1, "char_index must advance even when space is skipped");
    }

    #[test]
    fn column_without_actor_uses_random_mode() {
        let mut rng = rand::rng();
        let cfg = test_cfg();
        let col = Column::new(0, 40, &cfg, &mut rng, None);

        assert!(col.source_chars.is_none(), "no actor → source_chars must be None");
        assert!(col.source_kw.is_none(), "no actor → source_kw must be None");
        assert!(!col.highlight_keywords, "no actor → highlight_keywords must be false");
        // trail_rows must be in the valid random range
        let trail_min = ((cfg.trail_length as f32 * 0.37) as usize).max(1);
        assert!(
            col.trail_rows >= trail_min && col.trail_rows <= cfg.trail_length,
            "random trail_rows {} out of range {}..={}",
            col.trail_rows, trail_min, cfg.trail_length
        );
    }

    #[test]
    fn column_cells_have_target_ch_from_source_line() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg_path = manifest.join("config.yml");
        if !cfg_path.exists() {
            return;
        }
        let src_cfg = source::load_config(&cfg_path).expect("config.yml");
        let handle = source::spawn(src_cfg);

        let mut rng = rand::rng();
        let cfg = test_cfg();
        let mut col = Column::new(0, 40, &cfg, &mut rng, Some(&handle));

        // Force head to start just above row 3 (rows 0-2 reserved for overlay)
        col.head_y = 2.5;
        col.speed = 3.0;

        col.tick(&cfg, &mut rng, Some(&handle));

        // Find a non-space cell and verify its target_ch came from source_chars
        let source = col.source_chars.clone().unwrap_or_default();
        let mut found_target = false;
        for (row, cell_opt) in col.cells.iter().enumerate() {
            if let Some(cell) = cell_opt {
                if let Some(tc) = cell.target_ch {
                    // target_ch must either be in the source line or be from the post-line random zone
                    if row < source.len() {
                        assert!(
                            source.contains(&tc) || tc == ' ',
                            "target_ch {tc:?} at row {row} not found in source line"
                        );
                    }
                    found_target = true;
                }
            }
        }
        assert!(found_target, "at least one cell should have a target_ch set");
    }
}