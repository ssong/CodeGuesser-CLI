mod auth;
mod config;
mod game;
mod github;
mod highlight;
mod onboarding;
mod prompt;
mod share;
mod snippet;
mod stats;
mod tui;

use onboarding::ReadyContext;
use prompt::{bold, dim, red, yellow};
use stats::{PersistedStats, SessionStats};
use std::io::IsTerminal;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let smoke = args.iter().any(|a| a == "--smoke");
    let force_setup = args.iter().any(|a| a == "--setup");
    let interactive = std::io::stdin().is_terminal() && !smoke;

    if !smoke && !interactive {
        eprintln!("codeguesser needs an interactive terminal (or run with --smoke for a dry run)");
        std::process::exit(1);
    }

    prompt::banner();

    let ready = match onboarding::ensure_setup(interactive, force_setup) {
        Ok(ready) => ready,
        Err(message) => {
            eprintln!("{}", red(&message));
            std::process::exit(1);
        }
    };

    if smoke {
        if let Err(message) = smoke_test(&ready) {
            eprintln!("{}", red(&message));
            std::process::exit(1);
        }
        return;
    }

    play(ready);
}

/// Non-interactive dry run: fetch one round and print it, answer included.
fn smoke_test(ready: &ReadyContext) -> Result<(), String> {
    let mut dead = std::collections::HashSet::new();
    let mut cache = std::collections::HashMap::new();
    let round = game::prepare_round(
        &ready.client,
        &ready.org,
        &ready.repos,
        &mut dead,
        &mut cache,
    )?;
    println!("answer:     {}", round.answer);
    println!("options:    {}", round.options.join(", "));
    println!(
        "file:       {} (L{}–L{})",
        round.file_path, round.start_line, round.end_line
    );
    println!(
        "extension:  {}",
        round.extension.as_deref().unwrap_or("(none)")
    );
    println!("redactions: {}", round.redactions);
    println!("url:        {}", round.url);
    println!("snippet:");
    for line in &round.lines {
        println!("    {line}");
    }
    println!("\nsmoke test OK");
    Ok(())
}

fn play(ready: ReadyContext) {
    let persisted = stats::load();
    let mut session = SessionStats::default();
    let rounds =
        game::spawn_prefetcher(ready.client.clone(), ready.org.clone(), ready.repos.clone());

    let mut terminal = ratatui::init();
    let outcome = tui::run(
        &mut terminal,
        rounds,
        &mut session,
        persisted.high_score,
        &ready.org,
    );
    ratatui::restore();

    match outcome {
        Ok(Some(message)) => eprintln!(
            "\n{}",
            red(&format!("  round preparation failed: {message}"))
        ),
        Ok(None) => {}
        Err(err) => eprintln!("\n{}", red(&format!("  terminal error: {err}"))),
    }

    summary(&session, &persisted);

    if session.rounds > 0 {
        stats::save(&PersistedStats {
            high_score: persisted.high_score.max(session.score),
            best_streak: persisted.best_streak.max(session.best_streak),
            games_played: persisted.games_played + 1,
            total_rounds: persisted.total_rounds + session.rounds,
            total_correct: persisted.total_correct + session.correct,
        });

        if prompt::ask_yes_no("generate a share card for your run?") {
            let card =
                share::build_share_card(&session, session.score > persisted.high_score, &ready.org);
            println!("\n{card}");
            if share::copy_to_clipboard(&card) {
                println!("{}", dim("\n  (copied to clipboard — paste it in slack)"));
            }
        }
    }
    println!("{}", dim("\n  thanks for playing codeguesser 👋\n"));
}

fn summary(session: &SessionStats, persisted: &PersistedStats) {
    let accuracy = (session.correct * 100)
        .checked_div(session.rounds)
        .unwrap_or(0);
    let new_high = session.rounds > 0 && session.score > persisted.high_score;
    println!("\n{}", bold(&format!("──── final tab {}", "─".repeat(25))));
    let score_note = if new_high {
        yellow("  🏆 new high score!")
    } else {
        dim(&format!(
            "  (high score: {})",
            persisted.high_score.max(session.score)
        ))
    };
    println!(
        "  score        {}{score_note}",
        bold(&session.score.to_string())
    );
    println!(
        "  rounds       {} · {} correct · {accuracy}%",
        session.rounds, session.correct
    );
    println!("  best streak  {}", session.best_streak);
}
