use crate::config;
use crate::game::{self, points_for, Round};
use crate::github::{Client, RepoInfo};
use crate::highlight;
use crate::onboarding;
use crate::share;
use crate::stats::{self, PersistedStats, SessionStats};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::io;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const MAX_WIDTH: u16 = 110;
const MAX_SNIPPET_ROWS: u16 = 11; // up to 10 code lines + redaction note
const ACTION_ROWS: u16 = 4;
const RECENT_ROWS: u16 = 3;
const SURVIVAL_TIME: Duration = Duration::from_secs(5);
const TIMER_BAR_WIDTH: usize = 24;
const MAX_ORG_LEN: usize = 60;
// How long the reveal lingers before the next round loads on its own.
const REVEAL_CORRECT: Duration = Duration::from_millis(2000);
const REVEAL_WRONG: Duration = Duration::from_millis(3500);
const MENU: [(&str, &str); 3] = [
    ("classic", "endless rounds — cash out anytime"),
    ("survival", "5s per guess — one miss ends the run"),
    ("settings", "point snippets at a different GitHub org"),
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
    Menu {
        selected: usize,
    },
    Settings(SettingsView),
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
        /// When to auto-advance to the next round (None on a survival death).
        until: Option<Instant>,
    },
    GameOver {
        new_high: bool,
    },
    ShareCard {
        card: String,
        copied: bool,
    },
}

/// The settings screen: pick the snippet org from the ones the logged-in
/// user belongs to, with free-text entry as a fallback.
enum SettingsView {
    /// Fetching the user's org list on a background thread.
    LoadingOrgs {
        rx: Receiver<Result<Vec<String>, String>>,
    },
    /// Choosing from the fetched list.
    Pick {
        orgs: Vec<String>,
        selected: usize,
        error: Option<String>,
    },
    /// Validating the picked org (repo count) on a background thread.
    Checking {
        orgs: Vec<String>,
        selected: usize,
        rx: Receiver<Result<Vec<RepoInfo>, String>>,
    },
    /// Typing an org name by hand (list unavailable, or org not listed).
    Manual {
        input: String,
        error: Option<String>,
        pending: Option<Receiver<Result<Vec<RepoInfo>, String>>>,
    },
}

enum Action {
    HardQuit,
    Quit,
    Up,
    Down,
    Digit(usize),
    Enter,
    Other,
}

struct App {
    phase: Phase,
    mode: Mode,
    spinner: usize,
    session: SessionStats,
    persisted: PersistedStats,
    client: Client,
    org: String,
    repos: Vec<RepoInfo>,
    rounds: Receiver<Result<Round, String>>,
    recent: Vec<RecentEntry>,
}

/// Run the game loop. Returns Ok(Some(message)) when round preparation
/// failed, Ok(None) when the player quit.
pub fn run(
    terminal: &mut DefaultTerminal,
    client: Client,
    org: String,
    repos: Vec<RepoInfo>,
    persisted: PersistedStats,
) -> io::Result<Option<String>> {
    let rounds = game::spawn_prefetcher(client.clone(), org.clone(), repos.clone());
    App {
        phase: Phase::Menu { selected: 0 },
        mode: Mode::Classic,
        spinner: 0,
        session: SessionStats::default(),
        persisted,
        client,
        org,
        repos,
        rounds,
        recent: Vec::new(),
    }
    .run_loop(terminal)
}

/// Wait up to `timeout` for a key press.
fn poll_key(timeout: Duration) -> io::Result<Option<KeyEvent>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    let Event::Key(key) = event::read()? else {
        return Ok(None);
    };
    if key.kind != KeyEventKind::Press {
        return Ok(None);
    }
    Ok(Some(key))
}

fn action_of(key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::HardQuit,
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Enter => Action::Enter,
        KeyCode::Char(c @ '1'..='9') => Action::Digit(c as usize - '0' as usize),
        _ => Action::Other,
    }
}

impl App {
    fn run_loop(mut self, terminal: &mut DefaultTerminal) -> io::Result<Option<String>> {
        loop {
            self.spinner = self.spinner.wrapping_add(1);
            terminal.draw(|frame| self.render(frame))?;

            match &self.phase {
                Phase::Menu { .. } => {
                    let Some(key) = poll_key(Duration::from_millis(80))? else {
                        continue;
                    };
                    let action = action_of(key);
                    let Phase::Menu { selected } = &mut self.phase else {
                        unreachable!()
                    };
                    let chosen = match action {
                        Action::Quit | Action::HardQuit => return Ok(None),
                        Action::Up => {
                            *selected = (*selected + MENU.len() - 1) % MENU.len();
                            None
                        }
                        Action::Down => {
                            *selected = (*selected + 1) % MENU.len();
                            None
                        }
                        Action::Digit(d) if (1..=MENU.len()).contains(&d) => Some(d - 1),
                        Action::Enter => Some(*selected),
                        _ => None,
                    };
                    match chosen {
                        Some(2) => {
                            let (tx, rx) = channel();
                            let client = self.client.clone();
                            thread::spawn(move || {
                                let _ = tx.send(client.fetch_accounts());
                            });
                            self.phase = Phase::Settings(SettingsView::LoadingOrgs { rx });
                        }
                        Some(idx) => {
                            self.mode = if idx == 1 {
                                Mode::Survival
                            } else {
                                Mode::Classic
                            };
                            self.phase = Phase::Loading;
                        }
                        None => {}
                    }
                }
                Phase::Settings { .. } => {
                    if self.settings_poll_background() {
                        continue;
                    }
                    let Some(key) = poll_key(Duration::from_millis(80))? else {
                        continue;
                    };
                    if matches!(action_of(key), Action::HardQuit) {
                        return Ok(None);
                    }
                    let Phase::Settings(view) = std::mem::replace(&mut self.phase, Phase::Loading)
                    else {
                        unreachable!()
                    };
                    self.phase = self.settings_handle_key(view, key);
                }
                Phase::Loading => match self.rounds.try_recv() {
                    Ok(Ok(round)) => {
                        self.phase = Phase::Guess {
                            active: ActiveRound::new(round),
                            selected: 0,
                            deadline: (self.mode == Mode::Survival)
                                .then(|| Instant::now() + SURVIVAL_TIME),
                        };
                    }
                    Ok(Err(message)) => return Ok(Some(message)),
                    Err(TryRecvError::Disconnected) => {
                        return Ok(Some("round preparation stopped unexpectedly".into()))
                    }
                    Err(TryRecvError::Empty) => {
                        match poll_key(Duration::from_millis(80))?.map(action_of) {
                            Some(Action::HardQuit) => {
                                self.finalize_stats();
                                return Ok(None);
                            }
                            Some(Action::Quit) => self.finish_game(),
                            _ => {}
                        }
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
                    let action = poll_key(poll_timeout)?.map(action_of);
                    if matches!(action, Some(Action::HardQuit)) {
                        self.finalize_stats();
                        return Ok(None);
                    }
                    if matches!(action, Some(Action::Quit)) {
                        self.finish_game();
                        continue;
                    }
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        // time's up — any non-quit key this tick loses the race
                        self.resolve_guess(None);
                        continue;
                    }
                    let Some(action) = action else {
                        continue;
                    };
                    let Phase::Guess {
                        active, selected, ..
                    } = &mut self.phase
                    else {
                        unreachable!()
                    };
                    let n_options = active.round.options.len();
                    let locked_in = match action {
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
                        self.resolve_guess(Some(choice));
                    }
                }
                Phase::Reveal { correct, until, .. } => {
                    let game_over = self.mode == Mode::Survival && !*correct;
                    let until = *until;
                    let poll_timeout = match until {
                        Some(u) => u
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(80)),
                        None => Duration::from_millis(250),
                    };
                    match poll_key(poll_timeout)?.map(action_of) {
                        Some(Action::HardQuit) => {
                            self.finalize_stats();
                            return Ok(None);
                        }
                        Some(Action::Quit) => self.finish_game(),
                        Some(_) if game_over => self.finish_game(),
                        Some(_) => self.phase = Phase::Loading,
                        None => {
                            if until.is_some_and(|u| Instant::now() >= u) {
                                self.phase = Phase::Loading;
                            }
                        }
                    }
                }
                Phase::GameOver { new_high } => {
                    let new_high = *new_high;
                    let Some(key) = poll_key(Duration::from_millis(250))? else {
                        continue;
                    };
                    match key.code {
                        _ if matches!(action_of(key), Action::HardQuit) => return Ok(None),
                        KeyCode::Char('q') => return Ok(None),
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            let card = share::build_share_card(&self.session, new_high, &self.org);
                            let copied = share::copy_to_clipboard(&card);
                            self.phase = Phase::ShareCard { card, copied };
                        }
                        _ => self.go_menu(),
                    }
                }
                Phase::ShareCard { .. } => {
                    let Some(key) = poll_key(Duration::from_millis(250))? else {
                        continue;
                    };
                    match action_of(key) {
                        Action::HardQuit | Action::Quit => return Ok(None),
                        _ => self.go_menu(),
                    }
                }
            }
        }
    }

    /// Drain any finished background work in the settings screen. Returns
    /// true when the phase changed and the loop should restart.
    fn settings_poll_background(&mut self) -> bool {
        fn drain<T>(rx: &Receiver<Result<T, String>>) -> Option<Result<T, String>> {
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("background check stopped unexpectedly".into()))
                }
            }
        }

        match &self.phase {
            Phase::Settings(SettingsView::LoadingOrgs { rx }) => match drain(rx) {
                Some(Ok(orgs)) if !orgs.is_empty() => {
                    let selected = orgs.iter().position(|o| *o == self.org).unwrap_or(0);
                    self.phase = Phase::Settings(SettingsView::Pick {
                        orgs,
                        selected,
                        error: None,
                    });
                    true
                }
                Some(Ok(_)) => {
                    self.settings_manual(Some(
                        "couldn't find any accounts — type an org or user instead".into(),
                    ));
                    true
                }
                Some(Err(message)) => {
                    self.settings_manual(Some(format!(
                        "couldn't list your accounts ({message}) — type one instead"
                    )));
                    true
                }
                None => false,
            },
            Phase::Settings(SettingsView::Checking { rx, .. }) => {
                let Some(outcome) = drain(rx) else {
                    return false;
                };
                let Phase::Settings(SettingsView::Checking { orgs, selected, .. }) =
                    std::mem::replace(&mut self.phase, Phase::Loading)
                else {
                    unreachable!()
                };
                match outcome {
                    Ok(repos) => {
                        let org = orgs[selected].clone();
                        self.apply_new_org(org, repos);
                    }
                    Err(message) => {
                        self.phase = Phase::Settings(SettingsView::Pick {
                            orgs,
                            selected,
                            error: Some(message),
                        });
                    }
                }
                true
            }
            Phase::Settings(SettingsView::Manual {
                pending: Some(rx), ..
            }) => {
                let Some(outcome) = drain(rx) else {
                    return false;
                };
                match outcome {
                    Ok(repos) => {
                        let Phase::Settings(SettingsView::Manual { input, .. }) =
                            std::mem::replace(&mut self.phase, Phase::Loading)
                        else {
                            unreachable!()
                        };
                        self.apply_new_org(input.trim().to_string(), repos);
                    }
                    Err(message) => {
                        let Phase::Settings(SettingsView::Manual { error, pending, .. }) =
                            &mut self.phase
                        else {
                            unreachable!()
                        };
                        *error = Some(message);
                        *pending = None;
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Handle one key press on the settings screen, returning the next phase.
    fn settings_handle_key(&mut self, view: SettingsView, key: KeyEvent) -> Phase {
        match view {
            SettingsView::LoadingOrgs { rx } => match action_of(key) {
                Action::Quit => Phase::Menu { selected: 0 },
                _ => Phase::Settings(SettingsView::LoadingOrgs { rx }),
            },
            SettingsView::Pick {
                orgs,
                mut selected,
                error,
            } => match action_of(key) {
                Action::Quit => Phase::Menu { selected: 0 },
                Action::Up => {
                    selected = (selected + orgs.len() - 1) % orgs.len();
                    Phase::Settings(SettingsView::Pick {
                        orgs,
                        selected,
                        error: None,
                    })
                }
                Action::Down => {
                    selected = (selected + 1) % orgs.len();
                    Phase::Settings(SettingsView::Pick {
                        orgs,
                        selected,
                        error: None,
                    })
                }
                Action::Digit(d) if (1..=orgs.len().min(9)).contains(&d) => {
                    self.spawn_org_check(orgs, d - 1)
                }
                Action::Enter => self.spawn_org_check(orgs, selected),
                _ if key.code == KeyCode::Char('o') => Phase::Settings(SettingsView::Manual {
                    input: String::new(),
                    error: None,
                    pending: None,
                }),
                _ => Phase::Settings(SettingsView::Pick {
                    orgs,
                    selected,
                    error,
                }),
            },
            SettingsView::Checking { orgs, selected, rx } => match action_of(key) {
                // dropping the receiver abandons the in-flight check
                Action::Quit => Phase::Settings(SettingsView::Pick {
                    orgs,
                    selected,
                    error: None,
                }),
                _ => Phase::Settings(SettingsView::Checking { orgs, selected, rx }),
            },
            SettingsView::Manual {
                mut input,
                mut error,
                pending,
            } => {
                if key.code == KeyCode::Esc {
                    return Phase::Menu { selected: 0 };
                }
                if pending.is_none() {
                    match key.code {
                        KeyCode::Enter => {
                            let org = input.trim().to_string();
                            if org.is_empty() {
                                error = Some("type an org name first".into());
                            } else {
                                let (tx, rx) = channel();
                                let client = self.client.clone();
                                thread::spawn(move || {
                                    let _ = tx.send(onboarding::validate_org(&client, &org));
                                });
                                return Phase::Settings(SettingsView::Manual {
                                    input,
                                    error: None,
                                    pending: Some(rx),
                                });
                            }
                        }
                        KeyCode::Backspace => {
                            input.pop();
                        }
                        KeyCode::Char(c) if !c.is_control() && input.len() < MAX_ORG_LEN => {
                            input.push(c);
                        }
                        _ => {}
                    }
                }
                Phase::Settings(SettingsView::Manual {
                    input,
                    error,
                    pending,
                })
            }
        }
    }

    /// Kick off a background repo-count check for the picked org.
    fn spawn_org_check(&self, orgs: Vec<String>, selected: usize) -> Phase {
        let org = orgs[selected].clone();
        let (tx, rx) = channel();
        let client = self.client.clone();
        thread::spawn(move || {
            let _ = tx.send(onboarding::validate_org(&client, &org));
        });
        Phase::Settings(SettingsView::Checking { orgs, selected, rx })
    }

    /// Drop into free-text org entry, optionally with a note about why.
    fn settings_manual(&mut self, error: Option<String>) {
        self.phase = Phase::Settings(SettingsView::Manual {
            input: String::new(),
            error,
            pending: None,
        });
    }

    /// Switch to the validated org: persist it, restart the prefetcher, and
    /// head back to the menu.
    fn apply_new_org(&mut self, org: String, repos: Vec<RepoInfo>) {
        self.org = org;
        self.repos = repos;
        let mut cfg = config::load();
        cfg.org = Some(self.org.clone());
        let _ = config::save(&cfg); // best-effort — the session switches either way
        self.rounds =
            game::spawn_prefetcher(self.client.clone(), self.org.clone(), self.repos.clone());
        self.go_menu();
    }

    /// Reset per-game state and return to the main menu.
    fn go_menu(&mut self) {
        self.session = SessionStats::default();
        self.recent.clear();
        self.phase = Phase::Menu { selected: 0 };
    }

    /// Fold the session into lifetime stats and save them. Returns whether
    /// this run set a new high score. No-op for a session with no rounds.
    fn finalize_stats(&mut self) -> bool {
        if self.session.rounds == 0 {
            return false;
        }
        let new_high = self.session.score > self.persisted.high_score;
        self.persisted = PersistedStats {
            high_score: self.persisted.high_score.max(self.session.score),
            best_streak: self.persisted.best_streak.max(self.session.best_streak),
            games_played: self.persisted.games_played + 1,
            total_rounds: self.persisted.total_rounds + self.session.rounds,
            total_correct: self.persisted.total_correct + self.session.correct,
        };
        stats::save(&self.persisted);
        new_high
    }

    /// End the current game: show the final tab, or skip straight back to
    /// the menu when nothing was played.
    fn finish_game(&mut self) {
        if self.session.rounds == 0 {
            self.go_menu();
            return;
        }
        let new_high = self.finalize_stats();
        self.phase = Phase::GameOver { new_high };
    }

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
        let survival_death = self.mode == Mode::Survival && !correct;
        let until = (!survival_death).then(|| {
            Instant::now()
                + if correct {
                    REVEAL_CORRECT
                } else {
                    REVEAL_WRONG
                }
        });
        self.phase = Phase::Reveal {
            active,
            correct,
            points,
            guess,
            until,
        };
    }

    fn render(&self, frame: &mut Frame) {
        let full = frame.area();
        let area = Rect {
            width: full.width.min(MAX_WIDTH),
            ..full
        };

        if matches!(
            self.phase,
            Phase::Settings { .. } | Phase::GameOver { .. } | Phase::ShareCard { .. }
        ) {
            self.render_screen(frame, area);
            return;
        }

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

    /// Full-height single-panel layout for settings / final tab / share card.
    fn render_screen(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
        self.render_header(frame, chunks[0]);
        let max_rows = chunks[1].height.saturating_sub(2) as usize;
        let (title, lines) = match &self.phase {
            Phase::Settings { .. } => self.settings_lines(max_rows),
            Phase::GameOver { new_high } => self.game_over_lines(*new_high),
            Phase::ShareCard { .. } => self.share_lines(),
            _ => unreachable!("render_screen is only called for screen phases"),
        };
        let inner_width = chunks[1].width.saturating_sub(4) as usize;
        let clipped = lines
            .into_iter()
            .map(|l| Line::from(clip_spans(l.spans, inner_width)))
            .collect();
        self.render_panel(frame, chunks[1], title, clipped);
        self.render_footer(frame, chunks[2]);
    }

    fn settings_lines(&self, max_rows: usize) -> (&'static str, Vec<Line<'static>>) {
        let Phase::Settings(view) = &self.phase else {
            unreachable!()
        };
        let dim = Style::new().add_modifier(Modifier::DIM);
        let spin = Style::new().fg(Color::LightMagenta);
        let frame_ch = SPINNER_FRAMES[self.spinner % SPINNER_FRAMES.len()];
        let mut lines = vec![
            Line::default(),
            Line::from(vec![
                Span::raw("snippets currently come from "),
                Span::styled(
                    self.org.clone(),
                    Style::new()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("."),
            ]),
            Line::from(Span::styled(
                "switching orgs saves to your config and takes effect immediately.",
                dim,
            )),
            Line::default(),
        ];
        match view {
            SettingsView::LoadingOrgs { .. } => {
                lines.push(Line::from(Span::styled(
                    format!("{frame_ch} listing accounts you can pull from…"),
                    spin,
                )));
            }
            SettingsView::Pick {
                orgs,
                selected,
                error,
            } => {
                let budget = max_rows.saturating_sub(lines.len() + 3).max(3);
                push_org_list(&mut lines, orgs, *selected, &self.org, budget);
                lines.push(Line::default());
                if let Some(error) = error {
                    lines.push(Line::from(Span::styled(
                        format!("✖ {error}"),
                        Style::new().fg(Color::LightRed),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        "enter switches to the highlighted org · o to type a name instead",
                        dim,
                    )));
                }
            }
            SettingsView::Checking { orgs, selected, .. } => {
                let budget = max_rows.saturating_sub(lines.len() + 3).max(3);
                push_org_list(&mut lines, orgs, *selected, &self.org, budget);
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    format!("{frame_ch} checking {}…", orgs[*selected]),
                    spin,
                )));
            }
            SettingsView::Manual {
                input,
                error,
                pending,
            } => {
                lines.push(Line::from(vec![
                    Span::styled("new org: ", Style::new().add_modifier(Modifier::BOLD)),
                    Span::raw(input.clone()),
                    Span::styled("▏", Style::new().fg(Color::LightMagenta)),
                ]));
                lines.push(Line::default());
                if pending.is_some() {
                    lines.push(Line::from(Span::styled(
                        format!("{frame_ch} checking {}…", input.trim()),
                        spin,
                    )));
                } else if let Some(error) = error {
                    lines.push(Line::from(Span::styled(
                        format!("✖ {error}"),
                        Style::new().fg(Color::LightRed),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        "enter checks the org and saves it · esc goes back",
                        dim,
                    )));
                }
            }
        }
        ("settings", lines)
    }

    fn game_over_lines(&self, new_high: bool) -> (&'static str, Vec<Line<'static>>) {
        let s = &self.session;
        let accuracy = (s.correct * 100).checked_div(s.rounds).unwrap_or(0);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let mut score_spans = vec![
            Span::raw("score        "),
            Span::styled(
                s.score.to_string(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
        ];
        if new_high {
            score_spans.push(Span::styled(
                "  🏆 new high score!",
                Style::new().fg(Color::LightYellow),
            ));
        } else {
            score_spans.push(Span::styled(
                format!("  (high score: {})", self.persisted.high_score),
                dim,
            ));
        }
        let lines = vec![
            Line::default(),
            Line::from(score_spans),
            Line::from(format!(
                "rounds       {} · {} correct · {accuracy}%",
                s.rounds, s.correct
            )),
            Line::from(format!("best streak  {}", s.best_streak)),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    "generate a share card for your run? ",
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::styled("[y/n]", dim),
            ]),
        ];
        ("final tab", lines)
    }

    fn share_lines(&self) -> (&'static str, Vec<Line<'static>>) {
        let Phase::ShareCard { card, copied } = &self.phase else {
            unreachable!()
        };
        let mut lines = vec![Line::default()];
        lines.extend(card.lines().map(|l| Line::raw(l.to_string())));
        lines.push(Line::default());
        if *copied {
            lines.push(Line::from(Span::styled(
                "✔ copied to clipboard — paste it in slack",
                Style::new().fg(Color::LightGreen),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "couldn't reach the clipboard — copy it by hand",
                Style::new().add_modifier(Modifier::DIM),
            )));
        }
        ("share card", lines)
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let in_round = matches!(
            self.phase,
            Phase::Loading | Phase::Guess { .. } | Phase::Reveal { .. }
        );
        let mut bits = vec![" CODEGUESSER".to_string(), self.org.clone()];
        if in_round {
            if self.mode == Mode::Survival {
                bits.push("survival".to_string());
            }
            let round_no = self.session.rounds
                + if matches!(self.phase, Phase::Reveal { .. }) {
                    0
                } else {
                    1
                };
            bits.push(format!("round {round_no}"));
        }
        bits.push(format!("score {}", self.session.score));
        if self.session.streak >= 2 {
            bits.push(format!("🔥 {}", self.session.streak));
        }
        bits.push(format!(
            "high {}",
            self.persisted.high_score.max(self.session.score)
        ));

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
            Phase::Menu { selected } => {
                let lines = MENU
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
                ("main menu", lines)
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
                ..
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
            _ => (question, Vec::new()),
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
            Phase::Menu { .. } => " ↑↓ / 1-3 choose · enter select · q quit",
            Phase::Settings(SettingsView::LoadingOrgs { .. }) => " loading accounts… · esc back",
            Phase::Settings(SettingsView::Pick { .. }) => {
                " ↑↓ / 1-9 choose · enter switch · o type a name · esc back"
            }
            Phase::Settings(SettingsView::Checking { .. }) => " checking org… · esc cancel",
            Phase::Settings(SettingsView::Manual {
                pending: Some(_), ..
            }) => " checking org… · esc back",
            Phase::Settings(SettingsView::Manual { .. }) => {
                " type an org name · enter check & save · esc back"
            }
            Phase::Loading => " q cash out",
            Phase::Guess { .. } => " ↑↓ / 1-3 choose · enter lock in · q cash out",
            Phase::Reveal { correct, .. } if self.mode == Mode::Survival && !correct => {
                " any key → final tab"
            }
            Phase::Reveal { .. } => " next round auto-loads · any key to skip · q cash out",
            Phase::GameOver { .. } => " y share card · any other key → menu · q quit",
            Phase::ShareCard { .. } => " any key → menu · q quit",
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
            Phase::Guess { active, .. } | Phase::Reveal { active, .. } => Some(active),
            _ => None,
        }
    }
}

/// Append the selectable org list, windowed around the selection when it
/// doesn't all fit in `budget` rows.
fn push_org_list(
    lines: &mut Vec<Line<'static>>,
    orgs: &[String],
    selected: usize,
    current: &str,
    budget: usize,
) {
    let shown = orgs.len().min(budget);
    let start = selected.saturating_sub(shown / 2).min(orgs.len() - shown);
    for (i, org) in orgs.iter().enumerate().skip(start).take(shown) {
        let digit = if i < 9 {
            format!("{}. ", i + 1)
        } else {
            "   ".to_string()
        };
        let marker = if org == current { " (current)" } else { "" };
        if i == selected {
            lines.push(Line::from(Span::styled(
                format!("❯ {digit}{org}{marker}"),
                Style::new()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(format!("  {digit}{org}{marker}")));
        }
    }
    if shown < orgs.len() {
        lines.push(Line::from(Span::styled(
            format!("  … {} more (scroll with ↑↓)", orgs.len() - shown),
            Style::new().add_modifier(Modifier::DIM),
        )));
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
