use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let mut themes = ThemeSet::load_defaults();
        themes
            .themes
            .remove("base16-eighties.dark")
            .expect("default theme exists")
    })
}

/// Turn one snippet into per-line styled spans. Falls back to unstyled text
/// when the extension isn't recognised.
pub fn highlight(lines: &[String], extension: Option<&str>) -> Vec<Vec<Span<'static>>> {
    let set = syntax_set();
    let syntax = extension.and_then(|ext| set.find_syntax_by_extension(ext));

    let Some(syntax) = syntax else {
        return lines.iter().map(|l| vec![Span::raw(l.clone())]).collect();
    };

    let mut highlighter = HighlightLines::new(syntax, theme());
    lines
        .iter()
        .map(|line| match highlighter.highlight_line(line, set) {
            Ok(regions) => regions
                .into_iter()
                .map(|(style, text)| Span::styled(text.to_string(), convert(style)))
                .collect(),
            Err(_) => vec![Span::raw(line.clone())],
        })
        .collect()
}

fn convert(style: syntect::highlighting::Style) -> Style {
    let fg = style.foreground;
    let mut out = Style::new().fg(Color::Rgb(fg.r, fg.g, fg.b));
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    out
}
