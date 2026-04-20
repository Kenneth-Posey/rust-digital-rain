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

fn parse_comma_separated(s: &str) -> Vec<String> {
    s.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Deserializes a YAML map whose values are comma-delimited strings into
/// a `HashMap<String, Vec<String>>`.  Accepts null/missing values gracefully.
fn deserialize_comma_map<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<HashMap<String, String>> = Option::deserialize(deserializer)?;
    Ok(raw
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, parse_comma_separated(&v)))
        .collect())
}

#[derive(Deserialize, Clone)]
pub struct PathEntry {
    pub path: PathBuf,
    #[serde(default)]
    pub highlight_keywords: bool,
}

#[derive(Deserialize, Default)]
pub struct SourceConfig {
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub paths: Vec<PathEntry>,
    /// Keywords per language (comma-delimited string in YAML)
    #[serde(default, deserialize_with = "deserialize_comma_map")]
    pub keywords: HashMap<String, Vec<String>>,
    /// Doc-comment line prefixes per language (comma-delimited string in YAML).
    /// Lines whose trimmed content starts with any of these prefixes are skipped.
    #[serde(default, deserialize_with = "deserialize_comma_map")]
    pub doc_comments: HashMap<String, Vec<String>>,
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
    doc_prefixes: &[String],
    glyph_set: &HashSet<char>,
) -> Option<(Vec<char>, Vec<bool>)> {
    let trimmed = raw.trim();
    // Skip lines that are purely documentation
    if doc_prefixes.iter().any(|p| trimmed.starts_with(p.as_str())) {
        return None;
    }
    if trimmed.len() < MIN_LINE_LEN {
        return None;
    }

    let lower = trimmed.to_lowercase();

    // Keyword detection on the lowercased, pre-compression string
    let mut kw_mask = vec![false; lower.len()];
    if highlight_keywords && let Some(lang) = language && let Some(kw_set) = keywords.get(lang) {
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
    doc_comments: &HashMap<String, Vec<String>>,
    glyph_set: &HashSet<char>,
) -> Vec<SourceLine> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let language = ext_to_language(ext);
    let empty: Vec<String> = vec![];
    let doc_prefixes: &[String] = language
        .and_then(|l| doc_comments.get(l))
        .map(Vec::as_slice)
        .unwrap_or(&empty);

    text.lines()
        .filter_map(|raw| {
            let (chars, is_keyword) = preprocess_line(
                raw,
                language,
                keywords,
                entry.highlight_keywords,
                doc_prefixes,
                glyph_set,
            )?;
            Some(SourceLine {
                chars,
                is_keyword,
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
                    // Merge doc_comments per-language (union, no duplicates)
                    for (lang, prefixes) in s.doc_comments {
                        let existing = cfg.doc_comments.entry(lang).or_default();
                        for p in prefixes {
                            if !existing.contains(&p) {
                                existing.push(p);
                            }
                        }
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
    doc_comments: HashMap<String, Vec<String>>,
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
            doc_comments: config.doc_comments,
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
            let lines = process_file(path, entry, &self.keywords, &self.doc_comments, &self.glyph_set);
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn glyph_set() -> HashSet<char> {
        build_glyph_set()
    }

    fn no_kw() -> HashMap<String, HashSet<String>> {
        HashMap::new()
    }

    // --- preprocess_line ---

    #[test]
    fn preprocess_rejects_short_lines() {
        let gs = glyph_set();
        assert!(preprocess_line("fn f()", None, &no_kw(), false, &[], &gs).is_none());
        assert!(preprocess_line("", None, &no_kw(), false, &[], &gs).is_none());
        assert!(preprocess_line("       ", None, &no_kw(), false, &[], &gs).is_none());
    }

    #[test]
    fn preprocess_lowercases_and_keeps_valid_glyphs() {
        let gs = glyph_set();
        let (chars, _) = preprocess_line("pub fn main() {", None, &no_kw(), false, &[], &gs)
            .expect("should produce output");
        // All chars must be lowercase or valid symbols
        for ch in &chars {
            assert!(
                ch.is_lowercase() || ch.is_ascii_digit() || !ch.is_alphabetic(),
                "unexpected char: {ch:?}"
            );
        }
        // The string should start with 'p' (no leading space after trim)
        assert_eq!(chars[0], 'p');
    }

    #[test]
    fn preprocess_compresses_whitespace() {
        let gs = glyph_set();
        let (chars, _) = preprocess_line("    let   x  =  1;", None, &no_kw(), false, &[], &gs)
            .expect("should produce output");
        // Multiple spaces should be collapsed to one
        let s: String = chars.iter().collect();
        assert!(!s.contains("  "), "adjacent spaces found: {s:?}");
    }

    #[test]
    fn preprocess_keyword_detection() {
        let gs = glyph_set();
        let mut kw_map: HashMap<String, HashSet<String>> = HashMap::new();
        kw_map.insert(
            "rust".to_string(),
            ["fn", "pub", "let"].iter().map(|s| s.to_string()).collect(),
        );
        let (chars, is_kw) = preprocess_line("pub fn hello_world(x: i32) {", Some("rust"), &kw_map, true, &[], &gs)
            .expect("should produce output");
        let s: String = chars.iter().collect();
        // Find 'p' 'u' 'b' in chars and verify they are marked as keyword
        if let Some(pos) = s.find("pub") {
            assert!(is_kw[pos], "first char of 'pub' should be keyword");
            assert!(is_kw[pos + 1], "middle char of 'pub' should be keyword");
            assert!(is_kw[pos + 2], "last char of 'pub' should be keyword");
        } else {
            panic!("'pub' not found in chars: {s:?}");
        }
    }

    #[test]
    fn preprocess_keyword_no_partial_match() {
        // "public" should NOT match keyword "pub"
        let gs = glyph_set();
        let mut kw_map: HashMap<String, HashSet<String>> = HashMap::new();
        kw_map.insert(
            "rust".to_string(),
            ["pub"].iter().map(|s| s.to_string()).collect(),
        );
        let (chars, is_kw) = preprocess_line("public static void main_func(x)", Some("rust"), &kw_map, true, &[], &gs)
            .expect("should produce output");
        let s: String = chars.iter().collect();
        // "pub" at start of "public" must NOT be flagged
        if let Some(pos) = s.find("public") {
            assert!(!is_kw[pos], "'pub' inside 'public' must not be a keyword hit");
        }
    }

    #[test]
    fn preprocess_skips_doc_comment_lines() {
        let gs = glyph_set();
        let prefixes: Vec<String> = vec!["///".into(), "//!".into()];
        // Pure doc-comment line must be rejected
        assert!(preprocess_line("/// This is a doc comment for a function", None, &no_kw(), false, &prefixes, &gs).is_none());
        assert!(preprocess_line("//! Module-level doc comment here", None, &no_kw(), false, &prefixes, &gs).is_none());
        // Indented doc comment still starts with the prefix after trim
        assert!(preprocess_line("    /// indented doc comment line here", None, &no_kw(), false, &prefixes, &gs).is_none());
        // Regular code line must pass through
        assert!(preprocess_line("pub fn main() -> Result<()> {", None, &no_kw(), false, &prefixes, &gs).is_some());
    }

    // --- load_config ---

    #[test]
    fn load_config_reads_default_config_yml() {
        // Config is relative to this source tree
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg_path = manifest_dir.join("config.yml");
        if !cfg_path.exists() {
            return; // Skip in environments without the file
        }
        let cfg = load_config(&cfg_path).expect("config.yml should parse");
        assert!(!cfg.extensions.is_empty(), "extensions should be non-empty");
        assert!(!cfg.paths.is_empty(), "paths should be non-empty");
        assert!(!cfg.keywords.is_empty(), "keywords should be non-empty");
    }

    // --- LineActor / spawn ---

    #[test]
    fn actor_returns_lines_from_src_directory() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg_path = manifest_dir.join("config.yml");
        if !cfg_path.exists() {
            return;
        }
        let cfg = load_config(&cfg_path).expect("config.yml should parse");
        let handle = spawn(cfg);

        // Request 20 lines — at least some should be real source lines
        let mut got_line = false;
        let mut got_fallback = false;
        for _ in 0..20 {
            match handle.next_line() {
                Some(line) => {
                    got_line = true;
                    assert!(!line.chars.is_empty(), "SourceLine.chars must not be empty");
                    assert_eq!(
                        line.chars.len(),
                        line.is_keyword.len(),
                        "chars and is_keyword must have equal length"
                    );
                    // All chars must be in the glyph set (including space)
                    let gs = build_glyph_set();
                    for ch in &line.chars {
                        assert!(gs.contains(ch), "char {ch:?} not in glyph set");
                    }
                }
                None => {
                    got_fallback = true;
                }
            }
        }
        assert!(got_line, "actor should produce at least one real source line");
        let _ = got_fallback; // Fallback is allowed; just checking we got real lines
    }

    #[test]
    fn actor_with_no_paths_returns_fallback() {
        let cfg = SourceConfig::default();
        let handle = spawn(cfg);
        assert!(handle.next_line().is_none(), "empty config should always return None");
    }

    #[test]
    fn actor_cycles_through_files_continuously() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg_path = manifest_dir.join("config.yml");
        if !cfg_path.exists() {
            return;
        }
        let cfg = load_config(&cfg_path).expect("config.yml should parse");
        let handle = spawn(cfg);

        // Request enough lines to exhaust one file and start the next
        let lines: Vec<_> = (0..200).filter_map(|_| handle.next_line()).collect();
        assert!(!lines.is_empty(), "should get continuous lines over 200 requests");
    }

    // --- secret config merge ---

    fn write_temp_config(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn secret_config_extensions_are_union_merged() {
        let tmp = std::env::temp_dir().join("rain_test_ext");
        std::fs::create_dir_all(&tmp).unwrap();

        write_temp_config(&tmp, "config.yml", "extensions: [rs]\npaths: []\nkeywords: {}");
        write_temp_config(&tmp, "config.secret.yml", "extensions: [py]\npaths: []\nkeywords: {}");

        let cfg = load_config(&tmp.join("config.yml")).unwrap();
        assert!(cfg.extensions.contains(&"rs".to_string()), "must keep main extension");
        assert!(cfg.extensions.contains(&"py".to_string()), "must add secret extension");
        assert_eq!(cfg.extensions.len(), 2, "no extra extensions");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn secret_config_duplicate_extensions_not_added_twice() {
        let tmp = std::env::temp_dir().join("rain_test_dedup");
        std::fs::create_dir_all(&tmp).unwrap();

        write_temp_config(&tmp, "config.yml", "extensions: [rs, py]\npaths: []\nkeywords: {}");
        write_temp_config(&tmp, "config.secret.yml", "extensions: [rs]\npaths: []\nkeywords: {}");

        let cfg = load_config(&tmp.join("config.yml")).unwrap();
        let rs_count = cfg.extensions.iter().filter(|e| e.as_str() == "rs").count();
        assert_eq!(rs_count, 1, "'rs' must not be duplicated after merge");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn secret_config_paths_appended() {
        let tmp = std::env::temp_dir().join("rain_test_paths");
        std::fs::create_dir_all(&tmp).unwrap();

        write_temp_config(&tmp, "config.yml",
            "extensions: []\npaths:\n  - path: /path/a\nkeywords: {}"
        );
        write_temp_config(&tmp, "config.secret.yml",
            "extensions: []\npaths:\n  - path: /path/b\nkeywords: {}"
        );

        let cfg = load_config(&tmp.join("config.yml")).unwrap();
        let paths: Vec<_> = cfg.paths.iter().map(|p| p.path.to_str().unwrap()).collect();
        assert!(paths.iter().any(|p| p.ends_with("path/a")), "main path /path/a must be present");
        assert!(paths.iter().any(|p| p.ends_with("path/b")), "secret path /path/b must be present");
        assert_eq!(cfg.paths.len(), 2);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn secret_config_keywords_merged_per_language() {
        let tmp = std::env::temp_dir().join("rain_test_kw");
        std::fs::create_dir_all(&tmp).unwrap();

        write_temp_config(&tmp, "config.yml",
            "extensions: []\npaths: []\nkeywords:\n  rust: \"fn, let\""
        );
        write_temp_config(&tmp, "config.secret.yml",
            "extensions: []\npaths: []\nkeywords:\n  rust: \"pub\"\n  python: \"def\""
        );

        let cfg = load_config(&tmp.join("config.yml")).unwrap();
        let rust_kw = cfg.keywords.get("rust").expect("rust keywords must exist");
        assert!(rust_kw.contains(&"fn".to_string()), "must keep 'fn' from main");
        assert!(rust_kw.contains(&"let".to_string()), "must keep 'let' from main");
        assert!(rust_kw.contains(&"pub".to_string()), "must merge 'pub' from secret");
        let python_kw = cfg.keywords.get("python").expect("python keywords added from secret");
        assert!(python_kw.contains(&"def".to_string()), "must add new language from secret");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn secret_config_relative_paths_resolved_from_main_config_dir() {
        let tmp = std::env::temp_dir().join("rain_test_relpath");
        std::fs::create_dir_all(&tmp).unwrap();

        write_temp_config(&tmp, "config.yml",
            "extensions: [rs]\npaths:\n  - path: ./src\nkeywords: {}"
        );

        let cfg = load_config(&tmp.join("config.yml")).unwrap();
        let resolved = &cfg.paths[0].path;
        assert!(resolved.is_absolute(), "relative path must be resolved to absolute");
        assert!(
            resolved.ends_with("src"),
            "resolved path must end with 'src', got: {resolved:?}"
        );
        assert_eq!(
            resolved.parent().unwrap(),
            tmp,
            "resolved path must be anchored to config file directory"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn secret_config_missing_is_silently_ignored() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg_path = manifest_dir.join("config.yml");
        if !cfg_path.exists() {
            return;
        }
        // The real config.yml has no secret beside it in CI — should load fine
        let result = load_config(&cfg_path);
        assert!(result.is_ok(), "load_config must succeed when no secret file exists");
    }
}
