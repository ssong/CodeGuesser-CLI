# CodeGuesser

A terminal game: you get a random code snippet from somewhere in your GitHub
org, three repo names, and one question — *which repo did this code crawl out
of?*

Endless rounds, points for correct guesses, streak bonuses, local high score.
Built in Rust with [ratatui](https://ratatui.rs).

## Prereqs

- [Rust](https://rustup.rs) (stable)
- A GitHub login with access to your org, any of:
  - the `gh` CLI, logged in (`gh auth login`) — recommended,
  - a `GITHUB_TOKEN` / `GH_TOKEN` env var with repo read access, or
  - nothing — first run will ask you to paste a token

## Play

```sh
cargo run --release
```

Or install it on your PATH:

```sh
cargo install --path .
codeguesser
```

### First run

The game walks you through setup:

1. **Credentials** — it looks for a token in `GH_TOKEN` / `GITHUB_TOKEN`, then
   the `gh` CLI. If neither exists it asks you to paste a personal access
   token (not echoed), with the option to save it for next time.
2. **Org scope** — it asks which GitHub org snippets should come from and
   validates the answer by listing the org's repos.

Choices are saved to `~/.config/codeguesser/config.json` (chmod 600 since it
can hold a token). Re-run setup any time:

```sh
codeguesser --setup
```

### In the game

The game runs as a fixed full-screen TUI: stats in the header, the mystery
snippet (syntax-highlighted), your three options, and your recent rounds each
in their own panel — no scrolling, no layout shift.

Keys:

- `↑`/`↓` (or `j`/`k`) to choose, `enter` to lock in
- `1`–`3` to answer instantly
- any key advances past the reveal
- `q`, `esc`, or `ctrl+c` to cash out and see your final tab

After a game you can generate a **share card** — a plain-text stats box with
your score, accuracy, streak, and a 🟩🟥 guess grid — which is also copied to
your clipboard, ready to paste into Slack.

## Scoring

- 100 points per correct guess
- +25 per consecutive correct answer beyond the first (bonus caps at +100)
- a wrong guess scores nothing and resets your streak

High score and lifetime stats live in `~/.config/codeguesser/stats.json`.

## Notes

- Repos in play: everything in the org except forks and empty repos (archived
  repos are fair game — history is history).
- Snippets come from real code files; lock files, `node_modules`, minified and
  generated files are filtered out, as are shell scripts (`.sh`/`.bash`/`.zsh`)
  — too generic to guess from. Any occurrence of the candidate repo names
  or the org name is redacted to `████` so the code can't snitch.
- `codeguesser --smoke` does a non-interactive dry run (fetches one round and
  prints it, answer included) — handy for checking auth/org access. It uses
  the saved config but never prompts.
