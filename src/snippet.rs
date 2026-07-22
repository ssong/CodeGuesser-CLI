use rand::Rng;
use regex::{Regex, RegexBuilder};
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// File extensions that count as guessable code. Shell scripts are excluded —
/// too generic to guess from.
const CODE_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", //
    "py", "rb", "go", "java", "kt", "kts", "cs", "php", "swift", "rs", "scala", //
    "m", "mm", "c", "h", "cc", "cpp", "hpp", "cxx", //
    "sql", "ps1", "tf", "hcl", //
    "groovy", "gradle", "dart", "lua", "r", "pl", "pm", "ex", "exs", "erl", "clj", "cljs", //
    "vue", "svelte", "astro", "graphql", "gql", "proto", //
    "css", "scss", "less", "sass", "html",
];

/// Paths that are boring, generated, or third-party — never quiz on these.
const DENIED_PATH_PATTERNS: &[&str] = &[
    r"(^|/)node_modules/",
    r"(^|/)vendor/",
    r"(^|/)dist/",
    r"(^|/)build/",
    r"(^|/)out/",
    r"(^|/)\.next/",
    r"(^|/)coverage/",
    r"(^|/)__snapshots__/",
    r"(?i)(^|/)third[-_]?party/",
    r"(^|/)Pods/",
    r"(^|/)__generated__(/|\.)",
    r"(?i)(^|/)generated/",
    r"(?i)\.generated\.",
    r"\.min\.(js|css)$",
    r"\.bundle\.js$",
    r"\.d\.ts$",
];

fn denied_paths() -> &'static [Regex] {
    static DENIED: OnceLock<Vec<Regex>> = OnceLock::new();
    DENIED.get_or_init(|| {
        DENIED_PATH_PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("valid pattern"))
            .collect()
    })
}

pub fn extension(path: &str) -> Option<&str> {
    path.rsplit_once('.').map(|(_, ext)| ext)
}

pub fn is_code_file(path: &str) -> bool {
    let Some(ext) = extension(path) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    CODE_EXTENSIONS.contains(&ext.as_str()) && !denied_paths().iter().any(|re| re.is_match(path))
}

const WINDOW_SIZE: usize = 10;
const MIN_FILE_LINES: usize = 6;
const WINDOW_ATTEMPTS: usize = 24;

pub struct SnippetWindow {
    pub lines: Vec<String>,
    /// 1-based line number of lines[0] in the source file.
    pub start_line: usize,
}

/// Pick a readable ~10-line window from a file, or None if nothing decent exists.
pub fn pick_window(content: &str) -> Option<SnippetWindow> {
    if content.contains('\0') {
        return None; // binary sneaking past the extension filter
    }
    let all: Vec<&str> = content
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    if all.len() < MIN_FILE_LINES {
        return None;
    }

    if all.len() <= WINDOW_SIZE {
        if window_score(&all) < 0 {
            return None;
        }
        return trim(&all, 1);
    }

    let mut rng = rand::thread_rng();
    let mut best: Option<(i64, usize)> = None;
    for _ in 0..WINDOW_ATTEMPTS {
        let start = rng.gen_range(0..=all.len() - WINDOW_SIZE);
        let score = window_score(&all[start..start + WINDOW_SIZE]);
        if score < 0 {
            continue;
        }
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, start));
        }
    }
    let (_, start) = best?;
    trim(&all[start..start + WINDOW_SIZE], start + 1)
}

/// Score a candidate window; negative means "reject".
fn window_score(lines: &[&str]) -> i64 {
    let non_empty: Vec<&str> = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if non_empty.len() < 6 {
        return -1;
    }
    if lines.iter().any(|l| l.len() > 400) {
        return -1; // minified or a data blob
    }

    let joined = non_empty.join("\n").to_lowercase();
    if joined.contains("copyright")
        || joined.contains("permission is hereby")
        || joined.contains("license")
    {
        return -1; // license headers look the same everywhere
    }

    const COMMENT_STARTS: &[&str] = &["//", "#", "*", "/*", "--", "<!--", ";;"];
    let commentish = non_empty
        .iter()
        .filter(|l| COMMENT_STARTS.iter().any(|c| l.starts_with(c)))
        .count();
    if commentish * 10 > non_empty.len() * 6 {
        return -1;
    }

    let substantial = non_empty.iter().filter(|l| l.len() >= 16).count();
    if substantial < 3 {
        return -1;
    }

    // prefer denser, more varied windows
    let distinct = non_empty.iter().collect::<BTreeSet<_>>().len();
    (substantial * 2 + non_empty.len() + distinct) as i64
}

/// Drop blank lines at the edges of a window.
fn trim(lines: &[&str], start_line: usize) -> Option<SnippetWindow> {
    let mut start = 0;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    if end - start < 4 {
        return None;
    }
    Some(SnippetWindow {
        lines: lines[start..end].iter().map(|l| l.to_string()).collect(),
        start_line: start_line + start,
    })
}

/// Redact occurrences of the candidate repo names (and org name) so the
/// snippet can't give the answer away. All options are redacted equally, so a
/// redaction itself carries no signal.
pub fn redact(lines: &[String], names: &[&str]) -> (Vec<String>, usize) {
    let mut variants = BTreeSet::new();
    for raw in names {
        let name = raw.trim();
        if name.len() < 3 {
            continue;
        }
        variants.insert(name.to_string());
        variants.insert(name.replace(['-', '_'], ""));
        variants.insert(name.replace('-', "_"));
        variants.insert(name.replace('_', "-"));
        variants.insert(name.replace(['-', '_'], " "));
    }

    let mut sorted: Vec<String> = variants.into_iter().filter(|v| v.len() >= 3).collect();
    // longest first so sub-names don't shred longer matches
    sorted.sort_by_key(|v| std::cmp::Reverse(v.len()));
    let patterns: Vec<Regex> = sorted
        .iter()
        .map(|v| {
            RegexBuilder::new(&regex::escape(v))
                .case_insensitive(true)
                .build()
                .expect("escaped pattern is valid")
        })
        .collect();

    let mut count = 0;
    let redacted = lines
        .iter()
        .map(|line| {
            let mut out = line.clone();
            for pattern in &patterns {
                out = pattern
                    .replace_all(&out, |m: &regex::Captures| {
                        count += 1;
                        "█".repeat(m[0].len().clamp(4, 12))
                    })
                    .into_owned();
            }
            out
        })
        .collect();
    (redacted, count)
}
