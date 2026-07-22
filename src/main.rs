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
use prompt::{dim, red};
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

    let mut terminal = ratatui::init();
    let outcome = tui::run(
        &mut terminal,
        ready.client,
        ready.org,
        ready.repos,
        persisted,
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

    println!("{}", dim("\n  thanks for playing codeguesser 👋\n"));
}
