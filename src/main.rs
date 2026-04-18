use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use clap::Parser;

static RUNNING: AtomicBool = AtomicBool::new(true);

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
    #[arg(long, default_value_t = 24)]
    fps: u64,

    /// Maximum trail length in rows; minimum is ~37% of this, chosen randomly
    /// per column (default: 60)
    #[arg(long, default_value_t = 60)]
    trail_length: usize,

    /// Chance (0–100) per tick that a trail glyph flashes bright (default: 5)
    #[arg(long, default_value_t = 5.0)]
    flash_chance: f64,

    /// Glyph rotation speed multiplier; affects both fast and slow zones
    /// (default: 1.0)
    #[arg(long, default_value_t = 1.0)]
    rotation_speed: f32,
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
}

impl Column {
    fn new(x: u16, height: u16, cfg: &Config, rng: &mut impl rand::RngExt) -> Self {
        let speed = (rng.random_range(3..=8) as f32 * 0.02 + 0.04) * cfg.speed;
        let head_y = -(rng.random_range(0..height) as f32);
        let trail_min = ((cfg.trail_length as f32 * 0.37) as usize).max(1).min(cfg.trail_length);
        let trail_rows = rng.random_range(trail_min..=cfg.trail_length);
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
        }
    }

    fn tick(&mut self, cfg: &Config, rng: &mut impl rand::RngExt) {
        if !self.head_active {
            self.delay_counter += 1;
            self.update_cells(cfg, rng);
            if self.delay_counter >= self.delay && self.cells.iter().all(|c| c.is_none()) {
                self.restart(cfg, rng);
            }
            return;
        }

        let prev_row = self.head_y as i32;
        self.head_y += self.speed;
        let curr_row = self.head_y as i32;

        // Place a new cell at every integer row the head has just crossed
        let fps = cfg.fps as f32;
        let slow_min = (fps * 8.0) as u32;
        let slow_max = (fps * 10.0) as u32;
        for row in (prev_row + 1)..=(curr_row) {
            if row >= 0 && row < self.height as i32 {
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
                });
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
                    cell.ch = random_glyph(rng);
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

    fn restart(&mut self, cfg: &Config, rng: &mut impl rand::RngExt) {
        self.speed = (rng.random_range(3..=8) as f32 * 0.02 + 0.04) * cfg.speed;
        self.head_y = -(rng.random_range(0..self.height / 2) as f32);
        let trail_min = ((cfg.trail_length as f32 * 0.37) as usize).max(1).min(cfg.trail_length);
        self.trail_rows = rng.random_range(trail_min..=cfg.trail_length);
        self.cells.iter_mut().for_each(|c| *c = None);
        self.head_active = true;
        self.delay = rng.random_range(10..60);
        self.delay_counter = 0;
        self.fast_threshold = rng.random_range(3..=5);
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
                if row >= area.height as usize {
                    continue;
                }
                let cy = area.y + row as u16;
                let is_head = head_row == row as i32;

                let (base_r, base_g, base_b) = if is_head {
                    (200u8, 255u8, 225u8)
                } else {
                    let g = (cell.brightness.powf(1.5) * 220.0) as u8;
                    let g = g.max(18);
                    (0u8, g, (g as f32 * 0.10) as u8)
                };

                let color = if cell.flash > 0.0 && !is_head {
                    let f = cell.flash;
                    Color::Rgb(
                        (base_r as f32 + (200.0 - base_r as f32) * f) as u8,
                        (base_g as f32 + (255.0 - base_g as f32) * f) as u8,
                        (base_b as f32 + (225.0 - base_b as f32) * f) as u8,
                    )
                } else {
                    Color::Rgb(base_r, base_g, base_b)
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
}

impl App {
    fn new(width: u16, height: u16, config: Config) -> Self {
        let mut rng = rand::rng();
        let columns = (0..width)
            .map(|x| Column::new(x, height, &config, &mut rng))
            .collect();
        Self { columns, width, height, rng, config }
    }

    fn resize(&mut self, width: u16, height: u16) {
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        let cfg = &self.config;
        let rng = &mut self.rng;
        self.columns = (0..width)
            .map(|x| Column::new(x, height, cfg, rng))
            .collect();
    }

    fn tick(&mut self) {
        let cfg = &self.config;
        for col in &mut self.columns {
            col.tick(cfg, &mut self.rng);
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

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    execute!(stdout, crossterm::cursor::Hide)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let size = terminal.size()?;
    let mut app = App::new(size.width, size.height, config);
    let mut last_tick = Instant::now();

    while RUNNING.load(Ordering::SeqCst) {
        terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Rain { columns: &app.columns }, area);
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

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::cursor::Show,
    )?;

    Ok(())
}