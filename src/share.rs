use crate::stats::SessionStats;
use std::io::Write;
use std::process::{Command, Stdio};
use unicode_width::UnicodeWidthStr;

const GRID_PER_ROW: usize = 10;

/// A plain-text (no ANSI) stats card, sized to its content — safe to paste
/// into Slack as a code block.
pub fn build_share_card(session: &SessionStats, new_high: bool, org: &str) -> String {
    let accuracy = (session.correct * 100)
        .checked_div(session.rounds)
        .unwrap_or(0);
    let now = time::OffsetDateTime::now_utc().date();
    let date = format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    );

    let mut rows: Vec<String> = vec![
        "C O D E G U E S S E R".into(),
        format!("{org} edition · {date}"),
        String::new(),
        format!(
            "score        {}{}",
            session.score,
            if new_high { "  🏆 new high!" } else { "" }
        ),
        format!(
            "rounds       {} · {} correct ({accuracy}%)",
            session.rounds, session.correct
        ),
        format!(
            "best streak  {}{}",
            session.best_streak,
            if session.best_streak >= 3 {
                " 🔥"
            } else {
                ""
            }
        ),
    ];

    if !session.history.is_empty() {
        rows.push(String::new());
        for chunk in session.history.chunks(GRID_PER_ROW) {
            rows.push(
                chunk
                    .iter()
                    .map(|hit| if *hit { "🟩" } else { "🟥" })
                    .collect(),
            );
        }
    }

    let inner = rows.iter().map(|r| r.width()).max().unwrap_or(0) + 2;
    let mut card = format!("╔{}╗\n", "═".repeat(inner));
    for row in &rows {
        card.push_str(&format!(
            "║ {}{}║\n",
            row,
            " ".repeat(inner - row.width() - 1)
        ));
    }
    card.push_str(&format!("╚{}╝", "═".repeat(inner)));
    card
}

/// Best-effort copy; returns whether it (probably) worked.
pub fn copy_to_clipboard(text: &str) -> bool {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[("xclip", &["-selection", "clipboard"]), ("wl-copy", &[])]
    };

    for (cmd, args) in candidates {
        let child = Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            let ok = child
                .stdin
                .take()
                .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
            if ok && child.wait().is_ok_and(|s| s.success()) {
                return true;
            }
        }
    }
    false
}
