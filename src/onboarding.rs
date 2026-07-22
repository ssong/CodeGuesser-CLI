use crate::auth::detect_token;
use crate::config;
use crate::github::{Client, RepoInfo};
use crate::prompt::{ask_hidden, ask_line, ask_yes_no, bold, dim, green, red};

pub struct ReadyContext {
    pub client: Client,
    pub org: String,
    pub repos: Vec<RepoInfo>,
}

const MIN_REPOS: usize = 3;
const ORG_ATTEMPTS: usize = 3;
const MAX_LISTED_ORGS: usize = 20;

/// List an org's repos and require enough of them to build a round.
pub fn validate_org(client: &Client, org: &str) -> Result<Vec<RepoInfo>, String> {
    let repos = client.fetch_repos(org)?;
    if repos.len() < MIN_REPOS {
        return Err(format!(
            "need at least {MIN_REPOS} non-empty, non-fork repos in \"{org}\" — found {}",
            repos.len()
        ));
    }
    Ok(repos)
}

/// Resolve credentials + org scope, prompting interactively for whatever is
/// missing (or for everything, with `force`), and validate by listing the
/// org's repos. Choices are persisted to the config file.
pub fn ensure_setup(interactive: bool, force: bool) -> Result<ReadyContext, String> {
    let mut cfg = config::load();
    let config_path = config::config_path();
    let config_path = config_path.display();

    // ── token ──
    let token = if let Some(detected) = detect_token() {
        println!(
            "{}",
            dim(&format!(
                "✔ using GitHub credentials from {}",
                detected.source
            ))
        );
        detected.token
    } else if let Some(saved) = cfg.token.clone() {
        println!(
            "{}",
            dim(&format!("✔ using GitHub token from {config_path}"))
        );
        saved
    } else {
        if !interactive {
            return Err(
                "no GitHub credentials found — run `gh auth login`, set GITHUB_TOKEN / GH_TOKEN, or run codeguesser once interactively"
                    .into(),
            );
        }
        println!("\n  {}", bold("first, credentials."));
        println!(
            "{}",
            dim("  no GitHub login found (checked GH_TOKEN / GITHUB_TOKEN and the gh CLI).")
        );
        println!(
            "{}\n",
            dim("  paste a personal access token with repo read access — or ctrl+c out and run `gh auth login`.")
        );
        let token = ask_hidden("token:").map_err(|e| format!("could not read token: {e}"))?;
        if token.trim().is_empty() {
            return Err("setup cancelled — no token provided".into());
        }
        let token = token.trim().to_string();
        if ask_yes_no(&format!("save it to {config_path} for next time?")) {
            cfg.token = Some(token.clone());
            config::save(&cfg).map_err(|e| format!("could not save config: {e}"))?;
            println!(
                "{}",
                dim("  saved (file is chmod 600 — delete the \"token\" field to forget it)")
            );
        }
        println!();
        token
    };
    let client = Client::new(&token);

    // ── org scope ──
    let mut org = cfg.org.clone().unwrap_or_default();
    let mut must_ask = force || cfg.org.is_none();
    let mut user_orgs: Vec<String> = Vec::new();
    let mut listed = false;

    for attempt in 1..=ORG_ATTEMPTS {
        if must_ask {
            if !interactive {
                return Err(
                    "no org configured — run codeguesser once interactively to set one".into(),
                );
            }
            if !listed {
                listed = true;
                print!("{}", dim("… listing accounts you can pull from"));
                use std::io::Write;
                let _ = std::io::stdout().flush();
                user_orgs = client.fetch_accounts().unwrap_or_default();
                print!("\r\x1b[2K");
                if !user_orgs.is_empty() {
                    println!("  {}", bold("your accounts:"));
                    for (i, name) in user_orgs.iter().take(MAX_LISTED_ORGS).enumerate() {
                        println!("    {}. {name}", i + 1);
                    }
                    if user_orgs.len() > MAX_LISTED_ORGS {
                        println!(
                            "{}",
                            dim(&format!(
                                "    … and {} more (type its name)",
                                user_orgs.len() - MAX_LISTED_ORGS
                            ))
                        );
                    }
                }
            }
            let default = if org.is_empty() {
                None
            } else {
                Some(org.as_str())
            };
            let question = if user_orgs.is_empty() {
                "which GitHub org should snippets come from?"
            } else {
                "which org should snippets come from? (number or name)"
            };
            let answer =
                ask_line(question, default).map_err(|e| format!("could not read org: {e}"))?;
            org = match answer.parse::<usize>() {
                Ok(n) if (1..=user_orgs.len()).contains(&n) => user_orgs[n - 1].clone(),
                _ => answer,
            };
            if org.is_empty() {
                must_ask = true;
                continue;
            }
        }

        print!("{}", dim(&format!("… loading repos from {org}")));
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let outcome = validate_org(&client, &org);
        print!("\r\x1b[2K");

        match outcome {
            Ok(repos) => {
                println!(
                    "{}",
                    green(&format!("✔ loaded {} repos from {org}", repos.len()))
                );
                if cfg.org.as_deref() != Some(org.as_str()) {
                    cfg.org = Some(org.clone());
                    config::save(&cfg).map_err(|e| format!("could not save config: {e}"))?;
                    println!(
                        "{}",
                        dim(&format!(
                            "  org saved to {config_path} (re-run with --setup to change it)"
                        ))
                    );
                }
                return Ok(ReadyContext { client, org, repos });
            }
            Err(err) => {
                println!("{}", red(&format!("✖ {err}")));
                if !interactive || attempt == ORG_ATTEMPTS {
                    return Err(format!("couldn't set up org \"{org}\" — check the name and that your token can see it"));
                }
                must_ask = true; // give the player another shot at the org name
            }
        }
    }
    unreachable!("loop returns or errors within ORG_ATTEMPTS")
}
