use crate::game::{points_for, Round};
use crate::highlight;
use crate::stats::SessionStats;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::io;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const MAX_WIDTH: u16 = 110;
const MAX_SNIPPET_ROWS: u16 = 11; // up to 10 code lines + redaction note
const ACTION_ROWS: u16 = 4;
const RECENT_ROWS: u16 = 3;
const SURVIVAL_TIME: Duration = Duration::from_secs(5);
const TIMER_BAR_WIDTH: usize = 24;
const MODES: [(&str, &str); 2] = [
    ("classic", "endless rounds — cash out anytime"),
    ("survival", "5s per guess — one miss ends the run"),
];

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Classic,
    Survival,
}

struct RecentEntry {
    correct: bool,
    answer: String,
    file_path: String,
    points: u64,
    guess: Option<String>,
}

/// A round plus its pre-highlighted snippet (styling once, not every frame).
struct ActiveRound {
    round: Round,
    styled: Vec<Vec<Span<'static>>>,
}

impl ActiveRound {
    fn new(round: Round) -> Self {
        let expanded: Vec<String> = round.lines.iter().map(|l| l.replace('\t', "  ")).collect();
        let styled = highlight::highlight(&expanded, round.extension.as_deref());
        ActiveRound { round, styled }
    }
}

enum Phase {
    ModeSelect {
        selected: usize,
    },
    Loading,
    Guess {
        active: ActiveRound,
        selected: usize,
        deadline: Option<Instant>,
    },
    Reveal {
        active: ActiveRound,
        correct: bool,
        points: u64,
        guess: Option<String>,
    },
}

enum Action {
    Quit,
    Up,
    Down,
    Digit(usize),
    Enter,
    Other,
}

struct App<'a> {
    phase: Phase,
    mode: Mode,
    spinner: usize,
    session: &'a mut SessionStats,
    high_score: u64,
    org: &'a str,
    recent: Vec<RecentEntry>,
}

/// Run the game loop. Returns Ok(Some(message)) when round preparation
/// failed, Ok(None) when the player cashed out.
pub fn run(
    terminal: &mut DefaultTerminal,
    rounds: Receiver<Result<Round, String>>,
    session: &mut SessionStats,
    high_score: u64,
    org: &str,
) -> io::Result<Option<String>> {
    let mut app = App {
        phase: Phase::ModeSelect { selected: 0 },
        mode: Mode::Classic,
        spinner: 0,
        session,
        high_score,
        org,
        recent: Vec::new(),
    };

    loop {
        terminal.draw(|frame| app.render(frame))?;

        match &app.phase {
            Phase::ModeSelect { .. } => {
                let Some(action) = poll_action(Duration::from_millis(80))? else {
                    app.spinner += 1;
                    continue;
                };
                let Phase::ModeSelect { selected } = &mut app.phase else {
                    unreachable!()
                };
                let chosen = match action {
                    Action::Quit => return Ok(None),
                    Action::Up => {
                        *selected = (*selected + MODES.len() - 1) % MODES.len();
                        None
                    }
                    Action::Down => {
                        *selected = (*selected + 1) % MODES.len();
                        None
                    }
                    Action::Digit(d) if (1..=MODES.len()).contains(&d) => Some(d - 1),
                    Action::Enter => Some(*selected),
                    _ => None,
                };
                if let Some(idx) = chosen {
                    app.mode = if idx == 1 { Mode::Survival } else { Mode::Classic };
                    app.phase = Phase::Loading;
                }
            }
            Phase::Loading => match rounds.try_recv() {
                Ok(Ok(round)) => {
                    app.phase = Phase::Guess {
                        active: ActiveRound::new(round),
                        selected: 0,
                        deadline: (app.mode == Mode::Survival)
                            .then(|| Instant::now() + SURVIVAL_TIME),
                    };
                }
                Ok(Err(message)) => return Ok(Some(message)),
                Err(TryRecvError::Disconnected) => {
                    return Ok(Some("round preparation stopped unexpectedly".into()))
                }
                Err(TryRecvError::Empty) => {
                    if let Some(action) = poll_action(Duration::from_millis(80))? {
                        if matches!(action, Action::Quit) {
                            return Ok(None);
                        }
                    }
                    app.spinner += 1;
                }
            },
            Phase::Guess { deadline, .. } => {
                let deadline = *deadline;
                let poll_timeout = match deadline {
                    Some(d) => d
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(80)),
                    None => Duration::from_millis(250),
                };
                let action = poll_action(poll_timeout)?;
                if matches!(action, Some(Action::Quit)) {
                    return Ok(None);
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    // time's up — any non-quit key this tick loses the race
                    app.resolve_guess(None);
                    continue;
                }
                let Some(action) = action else {
                    continue;
                };
                let Phase::Guess {
                    active, selected, ..
                } = &mut app.phase
                else {
                    unreachable!()
                };
                let n_options = active.round.options.len();
                let locked_in = match action {
                    Action::Quit => return Ok(None),
                    Action::Up => {
                        *selected = (*selected + n_options - 1) % n_options;
                        None
                    }
                    Action::Down => {
                        *selected = (*selected + 1) % n_options;
                        None
                    }
                    Action::Digit(d) if (1..=n_options).contains(&d) => {
                        *selected = d - 1;
                        Some(d - 1)
                    }
                    Action::Enter => Some(*selected),
                    _ => None,
                };
                if let Some(choice) = locked_in {
                    app.resolve_guess(Some(choice));
                }
            }
            Phase::Reveal { correct, .. } => {
                let game_over = app.mode == Mode::Survival && !*correct;
                let Some(action) = poll_action(Duration::from_millis(250))? else {
                    continue;
                };
                match action {
                    Action::Quit => return Ok(None),
                    _ if game_over => return Ok(None),
                    _ => app.phase = Phase::Loading,
                }
            }
        }
    }
}

/// Wait up to `timeout` for a key press, normalized to an Action.
fn poll_action(timeout: Duration) -> io::Result<Option<Action>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    let Event::Key(key) = event::read()? else {
        return Ok(None);
    };
    if key.kind != KeyEventKind::Press {
        return Ok(None);
    }
    let action = match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Enter => Action::Enter,
        KeyCode::Char(c @ '1'..='9') => Action::Digit(c as usize - '0' as usize),
        _ => Action::Other,
    };
    Ok(Some(action))
}

impl App<'_> {
    /// Resolve the round; `None` means the survival timer ran out (no guess).
    fn resolve_guess(&mut self, choice: Option<usize>) {
        let Phase::Guess { active, .. } = std::mem::replace(&mut self.phase, Phase::Loading) else {
            unreachable!("resolve_guess is only called from the guess phase")
        };
        let guess = choice.map(|c| active.round.options[c].clone());
        let correct = guess.as_deref() == Some(active.round.answer.as_str());

        self.session.rounds += 1;
        self.session.history.push(correct);
        let mut points = 0;
        if correct {
            self.session.correct += 1;
            self.session.streak += 1;
            self.session.best_streak = self.session.best_streak.max(self.session.streak);
            points = points_for(self.session.streak);
            self.session.score += points;
        } else {
            self.session.streak = 0;
        }

        self.recent.insert(
            0,
            RecentEntry {
                correct,
                answer: active.round.answer.clone(),
                file_path: active.round.file_path.clone(),
                points,
                guess: guess.clone(),
            },
        );
        self.recent.truncate(RECENT_ROWS as usize);
        self.phase = Phase::Reveal {
            active,
            correct,
            points,
            guess,
        };
    }

    fn render(&self, frame: &mut Frame) {
        let full = frame.area();
        let area = Rect {
            width: full.width.min(MAX_WIDTH),
            ..full
        };

        // header(1) + snippet borders(2) + action(2 + ACTION_ROWS) + footer(1)
        let base_overhead = 1 + 2 + 2 + ACTION_ROWS + 1;
        let show_recent = area.height >= base_overhead + MAX_SNIPPET_ROWS + 2 + RECENT_ROWS;
        let recent_overhead = if show_recent { 2 + RECENT_ROWS } else { 0 };
        let snippet_rows = area
            .height
            .saturating_sub(base_overhead + recent_overhead)
            .clamp(5, MAX_SNIPPET_ROWS);

        let mut constraints = vec![
            Constraint::Length(1),
            Constraint::Length(snippet_rows + 2),
            Constraint::Length(2 + ACTION_ROWS),
        ];
        if show_recent {
            constraints.push(Constraint::Length(2 + RECENT_ROWS));
        }
        constraints.push(Constraint::Length(1));
        let chunks = Layout::vertical(constraints).split(area);

        self.render_header(frame, chunks[0]);
        self.render_snippet(frame, chunks[1], snippet_rows as usize);
        self.render_action(frame, chunks[2]);
        if show_recent {
            self.render_recent(frame, chunks[3]);
        }
        self.render_footer(frame, *chunks.last().expect("footer chunk exists"));
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let round_no = self.session.rounds
            + if matches!(self.phase, Phase::Reveal { .. }) {
                0
            } else {
                1
            };
        let mut bits = vec![
            format!(" CODEGUESSER"),
            self.org.to_string(),
        ];
        if self.mode == Mode::Survival && !matches!(self.phase, Phase::ModeSelect { .. }) {
            bits.push("survival".to_string());
        }
        bits.push(format!("round {round_no}"));
        bits.push(format!("score {}", self.session.score));
        if self.session.streak >= 2 {
            bits.push(format!("🔥 {}", self.session.streak));
        }
        bits.push(format!("high {}", self.high_score.max(self.session.score)));

        let mut spans = Vec::new();
        for (i, bit) in bits.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(
                    " · ",
                    Style::new().add_modifier(Modifier::DIM),
                ));
            }
            let style = if i == 0 {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            spans.push(Span::styled(bit.clone(), style));
        }
        let header = Paragraph::new(Line::from(spans))
            .style(Style::new().bg(Color::Magenta).fg(Color::White));
        frame.render_widget(header, area);
    }

    fn render_snippet(&self, frame: &mut Frame, area: Rect, max_rows: usize) {
        let inner_width = area.width.saturating_sub(4) as usize;

        let (title, lines) = match self.phase_round() {
            None => {
                let mut lines = vec![Line::default(); max_rows];
                let frame_ch = SPINNER_FRAMES[self.spinner % SPINNER_FRAMES.len()];
                lines[max_rows / 2] = Line::from(Span::styled(
                    format!("  {frame_ch} fetching a fresh snippet…"),
                    Style::new().fg(Color::LightMagenta),
                ));
                ("mystery file".to_string(), lines)
            }
            Some(active) => {
                let round = &active.round;
                let ext = round
                    .extension
                    .as_deref()
                    .map_or("???".into(), |e| format!(".{e}"));
                let gutter = round.end_line.to_string().len();
                let body_width = inner_width.saturating_sub(gutter + 1).max(20);
                let note_rows = usize::from(round.redactions > 0);
                let budget = max_rows - note_rows;
                let shown = round.lines.len().min(budget);
                let hidden = round.lines.len() - shown;

                let dim = Style::new().add_modifier(Modifier::DIM);
                let mut lines: Vec<Line> = Vec::with_capacity(max_rows);
                for i in 0..shown {
                    if hidden > 0 && i == shown - 1 {
                        lines.push(Line::from(Span::styled(
                            format!("{} … (+{} more lines)", " ".repeat(gutter), hidden + 1),
                            dim,
                        )));
                        break;
                    }
                    let number = format!("{:>gutter$} ", round.start_line + i);
                    let mut spans = vec![Span::styled(number, dim)];
                    spans.extend(clip_spans(active.styled[i].clone(), body_width));
                    lines.push(Line::from(spans));
                }
                if round.redactions > 0 {
                    let plural = if round.redactions == 1 { "" } else { "s" };
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{} ({} potential giveaway{plural} redacted: ████)",
                            " ".repeat(gutter),
                            round.redactions
                        ),
                        dim,
                    )));
                }
                (format!("mystery {ext} file"), lines)
            }
        };

        self.render_panel(frame, area, &title, lines);
    }

    fn render_action(&self, frame: &mut Frame, area: Rect) {
        let question = "which repo did this code crawl out of?";
        let inner_width = area.width.saturating_sub(4) as usize;
        let (title, lines): (&str, Vec<Line>) = match &self.phase {
            Phase::ModeSelect { selected } => {
                let lines = MODES
                    .iter()
                    .enumerate()
                    .map(|(i, (name, blurb))| {
                        if i == *selected {
                            Line::from(Span::styled(
                                format!("❯ {}. {name} — {blurb}", i + 1),
                                Style::new()
                                    .fg(Color::LightMagenta)
                                    .add_modifier(Modifier::BOLD),
                            ))
                        } else {
                            Line::from(format!("  {}. {name} — {blurb}", i + 1))
                        }
                    })
                    .collect();
                ("choose your mode", lines)
            }
            Phase::Loading => (question, Vec::new()),
            Phase::Guess {
                active,
                selected,
                deadline,
            } => {
                let mut lines: Vec<Line> = Vec::with_capacity(ACTION_ROWS as usize);
                if let Some(deadline) = deadline {
                    lines.push(timer_line(*deadline));
                }
                lines.extend(active.round.options.iter().enumerate().map(|(i, name)| {
                    if i == *selected {
                        Line::from(Span::styled(
                            format!("❯ {}. {name}", i + 1),
                            Style::new()
                                .fg(Color::LightMagenta)
                                .add_modifier(Modifier::BOLD),
                        ))
                    } else {
                        Line::from(format!("  {}. {name}", i + 1))
                    }
                }));
                (question, lines)
            }
            Phase::Reveal {
                active,
                correct,
                points,
                guess,
            } => {
                let round = &active.round;
                let dim = Style::new().add_modifier(Modifier::DIM);
                let verdict = if *correct {
                    let mut spans = vec![
                        Span::styled(
                            "✓ correct!",
                            Style::new()
                                .fg(Color::LightGreen)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!(" +{points}"), Style::new().fg(Color::Green)),
                    ];
                    if self.session.streak >= 2 {
                        spans.push(Span::styled(
                            format!("  🔥 ×{}", self.session.streak),
                            Style::new().fg(Color::LightYellow),
                        ));
                    }
                    Line::from(spans)
                } else {
                    match guess {
                        Some(guess) => Line::from(vec![
                            Span::styled(
                                "✗ nope",
                                Style::new()
                                    .fg(Color::LightRed)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!(" — you guessed {guess}"),
                                Style::new().fg(Color::Red),
                            ),
                        ]),
                        None => Line::from(vec![
                            Span::styled(
                                "⏱ time's up",
                                Style::new()
                                    .fg(Color::LightRed)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(" — no guess locked in", Style::new().fg(Color::Red)),
                        ]),
                    }
                };
                let source = Line::from(vec![
                    Span::styled("it was ", Style::new().add_modifier(Modifier::BOLD)),
                    Span::styled(
                        round.answer.clone(),
                        Style::new()
                            .fg(Color::LightCyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" · {}:{}", round.file_path, round.start_line), dim),
                ]);
                let url = Line::from(Span::styled(
                    round.url.clone(),
                    Style::new().add_modifier(Modifier::DIM | Modifier::UNDERLINED),
                ));
                let title = if *correct {
                    "nice one"
                } else if guess.is_none() {
                    "time's up"
                } else {
                    "unlucky"
                };
                (title, vec![verdict, source, url])
            }
        };

        let clipped = lines
            .into_iter()
            .map(|l| Line::from(clip_spans(l.spans, inner_width)))
            .collect();
        self.render_panel(frame, area, title, clipped);
    }

    fn render_recent(&self, frame: &mut Frame, area: Rect) {
        let inner_width = area.width.saturating_sub(4) as usize;
        let dim = Style::new().add_modifier(Modifier::DIM);
        let mut lines: Vec<Line> = self
            .recent
            .iter()
            .map(|entry| {
                let mark = if entry.correct {
                    Span::styled("✓ ", Style::new().fg(Color::Green))
                } else {
                    Span::styled("✗ ", Style::new().fg(Color::Red))
                };
                let tail = if entry.correct {
                    format!(" (+{})", entry.points)
                } else {
                    match &entry.guess {
                        Some(guess) => format!(" (guessed {guess})"),
                        None => " (timed out)".into(),
                    }
                };
                let spans = vec![
                    mark,
                    Span::styled(
                        entry.answer.clone(),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" · {}", entry.file_path), dim),
                    Span::styled(tail, dim),
                ];
                Line::from(clip_spans(spans, inner_width))
            })
            .collect();
        if lines.is_empty() {
            lines.push(Line::from(Span::styled("nothing yet — no pressure", dim)));
        }
        self.render_panel(frame, area, "recent rounds", lines);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let hints = match &self.phase {
            Phase::ModeSelect { .. } => " ↑↓ / 1-2 choose · enter start · q quit",
            Phase::Loading => " q cash out",
            Phase::Guess { .. } => " ↑↓ / 1-3 choose · enter lock in · q cash out",
            Phase::Reveal { correct, .. } if self.mode == Mode::Survival && !correct => {
                " any key → final tab · q quit"
            }
            Phase::Reveal { .. } => " any key → next round · q cash out",
        };
        frame.render_widget(
            Paragraph::new(hints).style(Style::new().add_modifier(Modifier::DIM)),
            area,
        );
    }

    fn render_panel(&self, frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line>) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().add_modifier(Modifier::DIM))
            .title(Span::styled(
                format!(" {title} "),
                Style::new()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            ));
        let padded: Vec<Line> = lines
            .into_iter()
            .map(|mut line| {
                line.spans.insert(0, Span::raw(" "));
                line
            })
            .collect();
        frame.render_widget(Paragraph::new(padded).block(block), area);
    }

    fn phase_round(&self) -> Option<&ActiveRound> {
        match &self.phase {
            Phase::ModeSelect { .. } | Phase::Loading => None,
            Phase::Guess { active, .. } | Phase::Reveal { active, .. } => Some(active),
        }
    }
}

/// Countdown bar for survival mode: seconds remaining plus a block-char
/// bar that drains and shifts green → yellow → red.
fn timer_line(deadline: Instant) -> Line<'static> {
    let secs = deadline
        .saturating_duration_since(Instant::now())
        .as_secs_f32();
    let frac = (secs / SURVIVAL_TIME.as_secs_f32()).clamp(0.0, 1.0);
    let filled = (frac * TIMER_BAR_WIDTH as f32).round() as usize;
    let color = if secs > 2.5 {
        Color::LightGreen
    } else if secs > 1.0 {
        Color::LightYellow
    } else {
        Color::LightRed
    };
    Line::from(vec![
        Span::styled(
            format!("⏱ {secs:>3.1}s "),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled("█".repeat(filled), Style::new().fg(color)),
        Span::styled(
            "░".repeat(TIMER_BAR_WIDTH - filled),
            Style::new().add_modifier(Modifier::DIM),
        ),
    ])
}

/// Truncate styled spans to a display width, appending an ellipsis when
/// anything was cut.
fn clip_spans(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| s.content.width()).sum();
    if total <= max_width {
        return spans;
    }

    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let span_width = span.content.width();
        if used + span_width < max_width {
            used += span_width;
            out.push(span);
            continue;
        }
        let mut taken = String::new();
        for ch in span.content.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if used + ch_width > max_width.saturating_sub(1) {
                break;
            }
            used += ch_width;
            taken.push(ch);
        }
        taken.push('…');
        out.push(Span::styled(taken, span.style));
        break;
    }
    out
}
