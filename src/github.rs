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
struct ApiOrg {
    login: String,
}

#[derive(Deserialize)]
struct ApiUser {
    login: String,
}

/// A repo-listing failure, keeping "no such owner" separate so callers can
/// retry against a different endpoint.
enum FetchError {
    NotFound,
    Other(String),
}

impl FetchError {
    fn into_message(self) -> String {
        match self {
            FetchError::NotFound => {
                "GitHub said 404 — no org or user with that name (or no access)".into()
            }
            FetchError::Other(message) => message,
        }
    }
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

    /// All non-fork, non-empty repos of an org or user account (archived
    /// included — history is history). Org repos are tried first; a 404
    /// means `owner` is a user account, so fall back to the user endpoints
    /// (the authenticated user's own account includes private repos).
    pub fn fetch_repos(&self, owner: &str) -> Result<Vec<RepoInfo>, String> {
        let org_url = format!("https://api.github.com/orgs/{owner}/repos");
        match self.fetch_repos_at(&org_url, &[("type", "all")]) {
            Err(FetchError::NotFound) => {}
            other => return other.map_err(FetchError::into_message),
        }
        let own_account = self
            .fetch_login()
            .is_ok_and(|login| login.eq_ignore_ascii_case(owner));
        let outcome = if own_account {
            self.fetch_repos_at(
                "https://api.github.com/user/repos",
                &[("affiliation", "owner")],
            )
        } else {
            let user_url = format!("https://api.github.com/users/{owner}/repos");
            self.fetch_repos_at(&user_url, &[])
        };
        outcome.map_err(FetchError::into_message)
    }

    /// One paginated repo listing, keeping 404 distinguishable.
    fn fetch_repos_at(
        &self,
        url: &str,
        extra: &[(&str, &str)],
    ) -> Result<Vec<RepoInfo>, FetchError> {
        let mut repos = Vec::new();
        for page in 1.. {
            let mut request = self
                .get(url)
                .query("per_page", "100")
                .query("page", &page.to_string());
            for (key, value) in extra {
                request = request.query(key, value);
            }
            let batch: Vec<ApiRepo> = request
                .call()
                .map_err(|err| match err {
                    ureq::Error::Status(404, _) => FetchError::NotFound,
                    other => FetchError::Other(describe(other)),
                })?
                .into_json()
                .map_err(|e| FetchError::Other(format!("bad response from GitHub: {e}")))?;
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

    /// The authenticated user's login.
    pub fn fetch_login(&self) -> Result<String, String> {
        let user: ApiUser = self
            .get("https://api.github.com/user")
            .call()
            .map_err(describe)?
            .into_json()
            .map_err(|e| format!("bad response from GitHub: {e}"))?;
        Ok(user.login)
    }

    /// Accounts the user can pull snippets from: their own login first,
    /// then the orgs they belong to (best-effort — an org listing failure
    /// still yields the personal account).
    pub fn fetch_accounts(&self) -> Result<Vec<String>, String> {
        let login = self.fetch_login()?;
        let mut accounts = vec![login];
        accounts.extend(self.fetch_user_orgs().unwrap_or_default());
        Ok(accounts)
    }

    /// Orgs the authenticated user belongs to.
    pub fn fetch_user_orgs(&self) -> Result<Vec<String>, String> {
        let mut orgs = Vec::new();
        for page in 1.. {
            let batch: Vec<ApiOrg> = self
                .get("https://api.github.com/user/orgs")
                .query("per_page", "100")
                .query("page", &page.to_string())
                .call()
                .map_err(describe)?
                .into_json()
                .map_err(|e| format!("bad response from GitHub: {e}"))?;
            let done = batch.len() < 100;
            orgs.extend(batch.into_iter().map(|o| o.login));
            if done {
                break;
            }
        }
        Ok(orgs)
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
