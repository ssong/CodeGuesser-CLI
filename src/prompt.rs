//! Plain-terminal output helpers and prompts, used outside the TUI
//! (banner, onboarding, final summary).

use std::io::{self, BufRead, Write};

pub fn bold(text: &str) -> String {
    format!("\x1b[1m{text}\x1b[22m")
}

pub fn dim(text: &str) -> String {
    format!("\x1b[2m{text}\x1b[22m")
}

pub fn magenta(text: &str) -> String {
    format!("\x1b[95m{text}\x1b[39m")
}

pub fn green(text: &str) -> String {
    format!("\x1b[32m{text}\x1b[39m")
}

pub fn red(text: &str) -> String {
    format!("\x1b[31m{text}\x1b[39m")
}

pub fn banner() {
    let art = [
        "╔═╗╔═╗╔╦╗╔═╗╔═╗╦ ╦╔═╗╔═╗╔═╗╔═╗╦═╗",
        "║  ║ ║ ║║║╣ ║ ╦║ ║║╣ ╚═╗╚═╗║╣ ╠╦╝",
        "╚═╝╚═╝═╩╝╚═╝╚═╝╚═╝╚═╝╚═╝╚═╝╚═╝╩╚═",
    ];
    println!();
    for line in art {
        println!("{}", magenta(&bold(line)));
    }
    println!(
        "{}\n",
        dim("guess which repo the code came from · endless · quit any time")
    );
}

/// One line of free text; empty input falls back to the default.
pub fn ask_line(question: &str, default: Option<&str>) -> io::Result<String> {
    let suffix = default.map(|d| dim(&format!(" ({d})"))).unwrap_or_default();
    print!("  {}{suffix} ", bold(question));
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    let answer = answer.trim();
    Ok(if answer.is_empty() {
        default.unwrap_or("").to_string()
    } else {
        answer.to_string()
    })
}

/// One line of secret text, not echoed.
pub fn ask_hidden(question: &str) -> io::Result<String> {
    rpassword::prompt_password(format!("  {} ", bold(question)))
}

/// y/N confirmation; anything but y/Y counts as no.
pub fn ask_yes_no(question: &str) -> bool {
    print!("  {} {} ", bold(question), dim("[y/N]"));
    let _ = io::stdout().flush();
    let mut answer = String::new();
    if io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    answer.trim().eq_ignore_ascii_case("y")
}
