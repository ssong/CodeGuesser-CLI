use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::Deserialize;
use std::time::Duration;

const MIN_FILE_BYTES: u64 = 200;
const MAX_FILE_BYTES: u64 = 150_000;

/// Safe-to-keep characters when encoding a path segment for a URL.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.');

#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub name: String,
    pub default_branch: String,
}

#[derive(Debug, Clone)]
pub struct CodeFile {
    pub path: String,
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    auth_header: String,
}

#[derive(Deserialize)]
struct ApiRepo {
    name: String,
    default_branch: Option<String>,
    fork: bool,
    size: Option<u64>,
}

#[derive(Deserialize)]
struct ApiTree {
    #[serde(default)]
    tree: Vec<ApiTreeEntry>,
}

#[derive(Deserialize)]
struct ApiTreeEntry {
    path: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    size: Option<u64>,
}

fn describe(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(401, _) => "GitHub said 401 — the token is invalid or expired".into(),
        ureq::Error::Status(403, _) => {
            "GitHub said 403 — rate limited, or the token lacks access".into()
        }
        ureq::Error::Status(404, _) => {
            "GitHub said 404 — not found (bad org name, or no access)".into()
        }
        ureq::Error::Status(code, _) => format!("GitHub said {code}"),
        ureq::Error::Transport(t) => format!("network error: {t}"),
    }
}

impl Client {
    pub fn new(token: &str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .user_agent("codeguesser")
            .timeout(Duration::from_secs(30))
            .build();
        Client {
            agent,
            auth_header: format!("Bearer {token}"),
        }
    }

    fn get(&self, url: &str) -> ureq::Request {
        self.agent
            .get(url)
            .set("Authorization", &self.auth_header)
            .set("X-GitHub-Api-Version", "2022-11-28")
    }

    /// All non-fork, non-empty repos in the org (archived included — history is history).
    pub fn fetch_repos(&self, org: &str) -> Result<Vec<RepoInfo>, String> {
        let mut repos = Vec::new();
        for page in 1.. {
            let batch: Vec<ApiRepo> = self
                .get(&format!("https://api.github.com/orgs/{org}/repos"))
                .query("type", "all")
                .query("per_page", "100")
                .query("page", &page.to_string())
                .call()
                .map_err(describe)?
                .into_json()
                .map_err(|e| format!("bad response from GitHub: {e}"))?;
            let done = batch.len() < 100;
            repos.extend(
                batch
                    .into_iter()
                    .filter(|r| !r.fork && r.size.unwrap_or(0) > 0)
                    .map(|r| RepoInfo {
                        name: r.name,
                        default_branch: r.default_branch.unwrap_or_else(|| "main".into()),
                    }),
            );
            if done {
                break;
            }
        }
        Ok(repos)
    }

    /// List guessable code files on the repo's default branch.
    pub fn fetch_code_files(&self, org: &str, repo: &RepoInfo) -> Result<Vec<CodeFile>, String> {
        let tree: ApiTree = self
            .get(&format!(
                "https://api.github.com/repos/{org}/{}/git/trees/{}",
                repo.name, repo.default_branch
            ))
            .query("recursive", "1")
            .call()
            .map_err(describe)?
            .into_json()
            .map_err(|e| format!("bad response from GitHub: {e}"))?;

        Ok(tree
            .tree
            .into_iter()
            .filter_map(|entry| {
                let path = entry.path?;
                let size = entry.size.unwrap_or(0);
                if entry.kind.as_deref() != Some("blob")
                    || !(MIN_FILE_BYTES..=MAX_FILE_BYTES).contains(&size)
                    || !crate::snippet::is_code_file(&path)
                {
                    return None;
                }
                Some(CodeFile { path })
            })
            .collect())
    }

    pub fn fetch_file_text(
        &self,
        org: &str,
        repo: &str,
        path: &str,
        git_ref: &str,
    ) -> Result<String, String> {
        let encoded = encode_path(path);
        self.get(&format!(
            "https://api.github.com/repos/{org}/{repo}/contents/{encoded}"
        ))
        .query("ref", git_ref)
        .set("Accept", "application/vnd.github.raw+json")
        .call()
        .map_err(describe)?
        .into_string()
        .map_err(|e| format!("could not read file body: {e}"))
    }
}

pub fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|seg| utf8_percent_encode(seg, PATH_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}
