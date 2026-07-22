use crate::github::{encode_path, Client, CodeFile, RepoInfo};
use crate::snippet;
use rand::seq::SliceRandom;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{sync_channel, Receiver};
use std::thread;

const BASE_POINTS: u64 = 100;
const STREAK_BONUS: u64 = 25;
const MAX_STREAK_BONUS: u64 = 100;

/// Points for a correct answer at the given (already incremented) streak.
pub fn points_for(streak: u32) -> u64 {
    BASE_POINTS
        + (u64::from(streak) - 1)
            .saturating_mul(STREAK_BONUS)
            .min(MAX_STREAK_BONUS)
}

#[derive(Debug, Clone)]
pub struct Round {
    /// Three repo names, shuffled.
    pub options: Vec<String>,
    /// The correct repo name.
    pub answer: String,
    pub file_path: String,
    /// File extension, used to pick a syntax for highlighting.
    pub extension: Option<String>,
    /// 1-based line numbers of the snippet within the file.
    pub start_line: usize,
    pub end_line: usize,
    /// Snippet lines, with giveaway names redacted.
    pub lines: Vec<String>,
    pub url: String,
    pub redactions: usize,
}

const REPO_ATTEMPTS: usize = 12;
const FILE_ATTEMPTS: usize = 4;

pub fn prepare_round(
    client: &Client,
    org: &str,
    repos: &[RepoInfo],
    dead: &mut HashSet<String>,
    tree_cache: &mut HashMap<String, Vec<CodeFile>>,
) -> Result<Round, String> {
    let mut rng = rand::thread_rng();

    for _ in 0..REPO_ATTEMPTS {
        let usable: Vec<&RepoInfo> = repos.iter().filter(|r| !dead.contains(&r.name)).collect();
        if usable.len() < 3 {
            return Err(format!(
                "fewer than 3 usable repos left in {org} — can't build a round"
            ));
        }
        let answer = *usable.choose(&mut rng).expect("usable is non-empty");

        if !tree_cache.contains_key(&answer.name) {
            let files = client.fetch_code_files(org, answer).unwrap_or_default();
            tree_cache.insert(answer.name.clone(), files);
        }
        let files = &tree_cache[&answer.name];
        if files.is_empty() {
            dead.insert(answer.name.clone());
            continue;
        }

        for _ in 0..FILE_ATTEMPTS {
            let file = files.choose(&mut rng).expect("files is non-empty");
            let Ok(text) =
                client.fetch_file_text(org, &answer.name, &file.path, &answer.default_branch)
            else {
                continue;
            };

            let Some(window) = snippet::pick_window(&text) else {
                continue;
            };

            let mut decoys: Vec<&str> = usable
                .iter()
                .map(|r| r.name.as_str())
                .filter(|n| *n != answer.name)
                .collect();
            decoys.shuffle(&mut rng);
            let mut options: Vec<String> = [answer.name.as_str(), decoys[0], decoys[1]]
                .iter()
                .map(|s| s.to_string())
                .collect();
            options.shuffle(&mut rng);

            let mut redact_names: Vec<&str> = options.iter().map(String::as_str).collect();
            redact_names.push(org);
            let (lines, redactions) = snippet::redact(&window.lines, &redact_names);

            let end_line = window.start_line + lines.len() - 1;
            let url = format!(
                "https://github.com/{org}/{}/blob/{}/{}#L{}-L{}",
                answer.name,
                answer.default_branch,
                encode_path(&file.path),
                window.start_line,
                end_line
            );

            return Ok(Round {
                options,
                answer: answer.name.clone(),
                extension: snippet::extension(&file.path).map(|e| e.to_ascii_lowercase()),
                file_path: file.path.clone(),
                start_line: window.start_line,
                end_line,
                lines,
                url,
                redactions,
            });
        }
        // no luck with this repo's files this time — try another repo
    }
    Err("could not prepare a round after several attempts (unlucky picks or rate limiting)".into())
}

/// Prepare rounds on a background thread, always keeping the next one ready
/// while the player stares at the current one. Dropping the receiver stops
/// the thread.
pub fn spawn_prefetcher(
    client: Client,
    org: String,
    repos: Vec<RepoInfo>,
) -> Receiver<Result<Round, String>> {
    let (tx, rx) = sync_channel(0);
    thread::spawn(move || {
        let mut dead = HashSet::new();
        let mut tree_cache = HashMap::new();
        loop {
            let round = prepare_round(&client, &org, &repos, &mut dead, &mut tree_cache);
            let failed = round.is_err();
            if tx.send(round).is_err() || failed {
                break;
            }
        }
    });
    rx
}
