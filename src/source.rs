use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    thread,
};

use serde::Deserialize;
use walkdir::WalkDir;

use crate::GLYPHS;

// ---------------------------------------------------------------------------
// Config structs
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
pub struct PathEntry {
    pub path: PathBuf,
    #[serde(default)]
    pub show_file_path: bool,
    #[serde(default)]
    pub highlight_keywords: bool,
}

#[derive(Deserialize, Default)]
pub struct SourceConfig {
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub paths: Vec<PathEntry>,
    #[serde(default)]
    pub keywords: HashMap<String, Vec<String>>,
}

// ---------------------------------------------------------------------------
// Static extension → language map
// ---------------------------------------------------------------------------

fn ext_to_language(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "cs" => Some("csharp"),
        "fs" | "fsi" | "fsx" => Some("fsharp"),
        "hs" | "lhs" => Some("haskell"),
        "swift" => Some("swift"),
        "clj" | "cljs" | "cljc" | "edn" => Some("clojure"),
        "php" => Some("php"),
        "cob" | "cbl" => Some("cobol"),
        "vb" => Some("visualbasic"),
        "sql" => Some("sql"),
        "cpp" | "cxx" | "cc" | "c++" | "hpp" | "hxx" | "h++" => Some("cpp"),
        "c" | "h" => Some("c"),
        "rb" => Some("ruby"),
        "dart" => Some("dart"),
        "r" => Some("r"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Preprocessed output
// ---------------------------------------------------------------------------

pub struct SourceLine {
    pub chars: Vec<char>,
    pub is_keyword: Vec<bool>,
    pub file: Arc<PathBuf>,
    pub show_file_path: bool,
    pub highlight_keywords: bool,
}

// ---------------------------------------------------------------------------
// Preprocessing
// ---------------------------------------------------------------------------

const MIN_LINE_LEN: usize = 8;

/// Set of chars valid in GLYPHS (plus space for whitespace slots)
fn build_glyph_set() -> HashSet<char> {
    let mut s: HashSet<char> = GLYPHS.iter().copied().collect();
    s.insert(' ');
    s
}

fn preprocess_line(
    raw: &str,
    language: Option<&str>,
    keywords: &HashMap<String, HashSet<String>>,
    highlight_keywords: bool,
    glyph_set: &HashSet<char>,
) -> Option<(Vec<char>, Vec<bool>)> {
    let trimmed = raw.trim();
    if trimmed.len() < MIN_LINE_LEN {
        return None;
    }

    let lower = trimmed.to_lowercase();

    // Keyword detection on the lowercased, pre-compression string
    let mut kw_mask = vec![false; lower.len()];
    if highlight_keywords {
        if let Some(lang) = language {
            if let Some(kw_set) = keywords.get(lang) {
                for kw in kw_set {
                    let mut start = 0;
                    while let Some(pos) = lower[start..].find(kw.as_str()) {
                        let abs = start + pos;
                        let end = abs + kw.len();
                        // Whole-word check
                        let before_ok = abs == 0
                            || !lower.as_bytes()[abs - 1].is_ascii_alphanumeric()
                                && lower.as_bytes()[abs - 1] != b'_';
                        let after_ok = end >= lower.len()
                            || !lower.as_bytes()[end].is_ascii_alphanumeric()
                                && lower.as_bytes()[end] != b'_';
                        if before_ok && after_ok {
                            for b in kw_mask[abs..end].iter_mut() {
                                *b = true;
                            }
                        }
                        start = abs + 1;
                    }
                }
            }
        }
    }

    // Compress whitespace, remap keyword mask
    let mut chars: Vec<char> = Vec::with_capacity(lower.len());
    let mut is_kw: Vec<bool> = Vec::with_capacity(lower.len());
    let mut in_ws = false;
    let mut byte_idx = 0usize;

    for ch in lower.chars() {
        let ch_len = ch.len_utf8();
        let is_ws = ch.is_whitespace();
        if is_ws {
            if !in_ws {
                chars.push(' ');
                is_kw.push(false);
                in_ws = true;
            }
        } else {
            let kw_flag = if byte_idx < kw_mask.len() { kw_mask[byte_idx] } else { false };
            if glyph_set.contains(&ch) {
                chars.push(ch);
                is_kw.push(kw_flag);
            }
            in_ws = false;
        }
        byte_idx += ch_len;
    }

    if chars.len() < MIN_LINE_LEN {
        return None;
    }

    Some((chars, is_kw))
}

fn process_file(
    path: &Path,
    entry: &PathEntry,
    keywords: &HashMap<String, HashSet<String>>,
    glyph_set: &HashSet<char>,
) -> Vec<SourceLine> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let language = ext_to_language(ext);

    let file = Arc::new(path.to_path_buf());

    text.lines()
        .filter_map(|raw| {
            let (chars, is_keyword) = preprocess_line(
                raw,
                language,
                keywords,
                entry.highlight_keywords,
                glyph_set,
            )?;
            Some(SourceLine {
                chars,
                is_keyword,
                file: Arc::clone(&file),
                show_file_path: entry.show_file_path,
                highlight_keywords: entry.highlight_keywords,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

pub fn load_config(config_path: &Path) -> Result<SourceConfig, String> {
    let text = std::fs::read_to_string(config_path)
        .map_err(|e| format!("cannot read config: {e}"))?;
    let mut cfg: SourceConfig =
        serde_yml::from_str(&text).map_err(|e| format!("config parse error: {e}"))?;

    // Attempt to load and merge secret config from same directory
    if let Some(dir) = config_path.parent() {
        let secret = dir.join("config.secret.yml");
        if secret.exists() {
            match std::fs::read_to_string(&secret)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_yml::from_str::<SourceConfig>(&t).map_err(|e| e.to_string()))
            {
                Ok(s) => {
                    // Merge extensions (union)
                    for ext in s.extensions {
                        if !cfg.extensions.contains(&ext) {
                            cfg.extensions.push(ext);
                        }
                    }
                    // Append path entries as-is
                    cfg.paths.extend(s.paths);
                    // Merge keywords per-language (union)
                    for (lang, kws) in s.keywords {
                        cfg.keywords.entry(lang).or_default().extend(kws);
                    }
                }
                Err(e) => {
                    eprintln!("warning: could not load config.secret.yml: {e}");
                }
            }
        }
    }

    // Resolve paths relative to the config file's directory
    if let Some(dir) = config_path.parent() {
        for entry in &mut cfg.paths {
            if entry.path.is_relative() {
                entry.path = dir.join(&entry.path);
            }
        }
    }

    Ok(cfg)
}

// ---------------------------------------------------------------------------
// LineActor
// ---------------------------------------------------------------------------

enum Request {
    NextLine,
}

pub enum Response {
    Line(SourceLine),
    Fallback,
}

pub struct LineActorHandle {
    tx: mpsc::SyncSender<Request>,
    rx: mpsc::Receiver<Response>,
}

impl LineActorHandle {
    /// Returns the next source line, or None if the actor signals fallback.
    pub fn next_line(&self) -> Option<SourceLine> {
        self.tx.send(Request::NextLine).ok()?;
        match self.rx.recv().ok()? {
            Response::Line(line) => Some(line),
            Response::Fallback => None,
        }
    }
}

struct LineActor {
    files: Vec<(PathBuf, Arc<PathEntry>)>,
    file_cursor: usize,
    line_buffer: VecDeque<SourceLine>,
    keywords: Arc<HashMap<String, HashSet<String>>>,
    glyph_set: HashSet<char>,
}

impl LineActor {
    fn new(config: SourceConfig) -> Self {
        use rand::seq::SliceRandom;

        // Build keyword sets (lowercased)
        let keywords: HashMap<String, HashSet<String>> = config
            .keywords
            .into_iter()
            .map(|(lang, kws)| {
                (
                    lang.to_lowercase(),
                    kws.into_iter().map(|k| k.to_lowercase()).collect(),
                )
            })
            .collect();

        let ext_set: HashSet<String> = config.extensions.iter().map(|e| e.to_lowercase()).collect();

        // Discover all matching files
        let mut files: Vec<(PathBuf, Arc<PathEntry>)> = Vec::new();
        for entry in config.paths {
            let entry = Arc::new(entry);
            for result in WalkDir::new(&entry.path).follow_links(true) {
                let Ok(de) = result else { continue };
                if !de.file_type().is_file() {
                    continue;
                }
                let path = de.path();
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();
                if ext_set.contains(&ext) {
                    files.push((path.to_path_buf(), Arc::clone(&entry)));
                }
            }
        }

        let mut rng = rand::rng();
        files.shuffle(&mut rng);

        Self {
            files,
            file_cursor: 0,
            line_buffer: VecDeque::new(),
            keywords: Arc::new(keywords),
            glyph_set: build_glyph_set(),
        }
    }

    fn fill_buffer_from_next_file(&mut self) -> bool {
        if self.files.is_empty() {
            return false;
        }
        // Try each file at most one full cycle
        for _ in 0..self.files.len() {
            let idx = self.file_cursor % self.files.len();
            self.file_cursor += 1;
            let (path, entry) = &self.files[idx];
            let lines = process_file(path, entry, &self.keywords, &self.glyph_set);
            if !lines.is_empty() {
                self.line_buffer.extend(lines);
                return true;
            }
        }
        false
    }

    fn next_line(&mut self) -> Option<SourceLine> {
        if self.line_buffer.is_empty() && !self.fill_buffer_from_next_file() {
            return None;
        }
        self.line_buffer.pop_front()
    }

    fn run(mut self, tx: mpsc::SyncSender<Response>, rx: mpsc::Receiver<Request>) {
        for req in rx {
            match req {
                Request::NextLine => {
                    let resp = match self.next_line() {
                        Some(line) => Response::Line(line),
                        None => Response::Fallback,
                    };
                    if tx.send(resp).is_err() {
                        break;
                    }
                }
            }
        }
    }
}

pub fn spawn(config: SourceConfig) -> LineActorHandle {
    let (req_tx, req_rx) = mpsc::sync_channel::<Request>(0);
    let (resp_tx, resp_rx) = mpsc::sync_channel::<Response>(0);

    let actor = LineActor::new(config);
    thread::spawn(move || actor.run(resp_tx, req_rx));

    LineActorHandle { tx: req_tx, rx: resp_rx }
}
