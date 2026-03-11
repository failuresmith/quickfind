use crate::config::load_config;
use crate::db;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use eyre::Result;
use glob::Pattern;
use rusqlite::Connection;
use std::io::{self};
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Span, Spans, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};

enum Focus {
    Search,
    Results,
}

const RESULT_PAGE_SIZE: usize = 10;

pub fn run_tui(conn: &Connection, initial_search: Option<String>) -> Result<()> {
    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let tick_rate = Duration::from_millis(250);
    let res = run_app(&mut terminal, conn, initial_search, tick_rate);

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
    }

    Ok(())
}

fn parse_color(s: &str) -> Option<Color> {
    match s.to_lowercase().as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "gray" => Some(Color::Gray),
        "darkgray" => Some(Color::DarkGray),
        "lightred" => Some(Color::LightRed),
        "lightgreen" => Some(Color::LightGreen),
        "lightyellow" => Some(Color::LightYellow),
        "lightblue" => Some(Color::LightBlue),
        "lightmagenta" => Some(Color::LightMagenta),
        "lightcyan" => Some(Color::LightCyan),
        _ => None,
    }
}

fn handle_file_opening(path: &str, error_message: &mut Option<String>) {
    match opener::open(path) {
        Ok(_) => {
            *error_message = None; // Clear error on successful open
        }
        Err(e) => {
            *error_message = Some(format!("Error opening file: {}", path));
            eprintln!("Failed to open file: {}. Error: {:?}", path, e);
        }
    }
}

fn current_selection_range(
    cursor_position: usize,
    selection_anchor: Option<usize>,
) -> Option<(usize, usize)> {
    selection_anchor.and_then(|anchor| {
        if anchor == cursor_position {
            None
        } else if anchor < cursor_position {
            Some((anchor, cursor_position))
        } else {
            Some((cursor_position, anchor))
        }
    })
}

fn delete_selection_if_any(
    search_input: &mut String,
    cursor_position: &mut usize,
    selection_anchor: &mut Option<usize>,
) -> bool {
    if let Some((start, end)) = current_selection_range(*cursor_position, *selection_anchor) {
        search_input.replace_range(start..end, "");
        *cursor_position = start;
        *selection_anchor = None;
        return true;
    }
    false
}

fn delete_previous_word(
    search_input: &mut String,
    cursor_position: &mut usize,
    selection_anchor: &mut Option<usize>,
) {
    if delete_selection_if_any(search_input, cursor_position, selection_anchor)
        || *cursor_position == 0
    {
        return;
    }

    let bytes = search_input.as_bytes();
    let mut idx = *cursor_position;

    while idx > 0 && bytes[idx - 1].is_ascii_whitespace() {
        idx -= 1;
    }
    while idx > 0 && !bytes[idx - 1].is_ascii_whitespace() {
        idx -= 1;
    }

    search_input.replace_range(idx..*cursor_position, "");
    *cursor_position = idx;
}

fn previous_word_boundary(search_input: &str, cursor_position: usize) -> usize {
    let bytes = search_input.as_bytes();
    let mut idx = cursor_position.min(search_input.len());

    while idx > 0 && bytes[idx - 1].is_ascii_whitespace() {
        idx -= 1;
    }
    while idx > 0 && !bytes[idx - 1].is_ascii_whitespace() {
        idx -= 1;
    }

    idx
}

fn next_word_boundary(search_input: &str, cursor_position: usize) -> usize {
    let bytes = search_input.as_bytes();
    let mut idx = cursor_position.min(search_input.len());
    let len = search_input.len();

    if idx >= len {
        return len;
    }

    if bytes[idx].is_ascii_whitespace() {
        while idx < len && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
    } else {
        while idx < len && !bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        while idx < len && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
    }

    idx
}

fn update_results(conn: &Connection, search_input: &str, results_state: &mut ListState) -> Vec<String> {
    let results = db::search_files(conn, search_input).unwrap_or_default();
    if results.is_empty() {
        results_state.select(None);
    } else {
        results_state.select(Some(0));
    }
    results
}

fn create_input_spans(search_input: &str, selected_range: Option<(usize, usize)>) -> Vec<Span<'static>> {
    if let Some((start, end)) = selected_range {
        let mut spans = Vec::new();
        if start > 0 {
            spans.push(Span::raw(search_input[..start].to_string()));
        }
        spans.push(Span::styled(
            search_input[start..end].to_string(),
            Style::default().bg(Color::Blue).fg(Color::White),
        ));
        if end < search_input.len() {
            spans.push(Span::raw(search_input[end..].to_string()));
        }
        spans
    } else {
        vec![Span::raw(search_input.to_string())]
    }
}

fn copy_selection_to_clipboard(
    search_input: &str,
    cursor_position: usize,
    selection_anchor: Option<usize>,
    clipboard: &mut Option<String>,
) {
    if let Some((start, end)) = current_selection_range(cursor_position, selection_anchor) {
        *clipboard = Some(search_input[start..end].to_string());
    }
}

fn cut_selection_to_clipboard(
    search_input: &mut String,
    cursor_position: &mut usize,
    selection_anchor: &mut Option<usize>,
    clipboard: &mut Option<String>,
) -> bool {
    if let Some((start, end)) = current_selection_range(*cursor_position, *selection_anchor) {
        *clipboard = Some(search_input[start..end].to_string());
        search_input.replace_range(start..end, "");
        *cursor_position = start;
        *selection_anchor = None;
        return true;
    }
    false
}

fn paste_from_clipboard(
    search_input: &mut String,
    cursor_position: &mut usize,
    selection_anchor: &mut Option<usize>,
    clipboard: &mut Option<String>,
) -> bool {
    let Some(mut pasted) = clipboard.clone() else {
        return false;
    };

    if pasted.is_empty() {
        return false;
    }

    pasted = pasted.replace(['\n', '\r'], " ");
    delete_selection_if_any(search_input, cursor_position, selection_anchor);
    search_input.insert_str(*cursor_position, &pasted);
    *cursor_position += pasted.len();
    *selection_anchor = None;
    true
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    conn: &Connection,
    initial_search: Option<String>,
    tick_rate: Duration,
) -> io::Result<()> {
    let config = load_config().unwrap_or_default();
    let highlight_color = config
        .highlight_color
        .as_ref()
        .and_then(|s| parse_color(s))
        .unwrap_or(Color::DarkGray);
    let preferred_editor = config.editor.clone();

    let mut last_tick = Instant::now();
    let mut search_input = initial_search.clone().unwrap_or_default();
    let mut cursor_position = search_input.len();
    let mut selection_anchor: Option<usize> = None;
    let mut error_message: Option<String> = None;
    let mut clipboard: Option<String> = None;

    let mut search_results = if let Some(term) = initial_search {
        db::search_files(conn, &term).unwrap_or_default()
    } else {
        vec![]
    };

    let mut results_state = ListState::default();
    if search_results.is_empty() {
        results_state.select(None);
    } else {
        results_state.select(Some(0));
    }
    let mut focus = Focus::Search;

    loop {
        let selected_range = current_selection_range(cursor_position, selection_anchor);
        terminal.draw(|f| {
            ui(
                f,
                &search_input,
                cursor_position,
                selected_range,
                &search_results,
                &mut results_state,
                &focus,
                &highlight_color,
                &error_message, // Pass the error_message
            )
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match focus {
                    Focus::Search => match key.code {
                        KeyCode::Enter => {
                            if !search_input.is_empty() {
                                search_results =
                                    update_results(conn, &search_input, &mut results_state);
                                focus = Focus::Results;
                                selection_anchor = None;
                                if let Some(path) = search_results.get(0) {
                                    handle_file_opening(path, &mut error_message);
                                }
                            }
                        }
                        KeyCode::Down => {
                            if !search_results.is_empty() {
                                focus = Focus::Results;
                                results_state.select(Some(0));
                                selection_anchor = None;
                            }
                        }
                        KeyCode::Backspace => {
                            let mut edited = false;
                            if key.modifiers.contains(KeyModifiers::ALT) {
                                let previous = search_input.clone();
                                delete_previous_word(
                                    &mut search_input,
                                    &mut cursor_position,
                                    &mut selection_anchor,
                                );
                                edited = previous != search_input;
                            } else if delete_selection_if_any(
                                &mut search_input,
                                &mut cursor_position,
                                &mut selection_anchor,
                            ) {
                                edited = true;
                            } else if cursor_position > 0 {
                                search_input.remove(cursor_position - 1);
                                cursor_position -= 1;
                                selection_anchor = None;
                                edited = true;
                            }

                            if edited {
                                search_results =
                                    update_results(conn, &search_input, &mut results_state);
                                error_message = None;
                            }
                        }
                        KeyCode::Left => {
                            let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
                            let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                            if is_shift {
                                if selection_anchor.is_none() {
                                    selection_anchor = Some(cursor_position);
                                }
                                cursor_position = if is_ctrl {
                                    previous_word_boundary(&search_input, cursor_position)
                                } else if cursor_position > 0 {
                                    cursor_position - 1
                                } else {
                                    0
                                };
                            } else {
                                cursor_position = if is_ctrl {
                                    previous_word_boundary(&search_input, cursor_position)
                                } else if cursor_position > 0 {
                                    cursor_position - 1
                                } else {
                                    0
                                };
                                selection_anchor = None;
                            }
                        }
                        KeyCode::Right => {
                            let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
                            let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                            if is_shift {
                                if selection_anchor.is_none() {
                                    selection_anchor = Some(cursor_position);
                                }
                                cursor_position = if is_ctrl {
                                    next_word_boundary(&search_input, cursor_position)
                                } else if cursor_position < search_input.len() {
                                    cursor_position + 1
                                } else {
                                    search_input.len()
                                };
                            } else {
                                cursor_position = if is_ctrl {
                                    next_word_boundary(&search_input, cursor_position)
                                } else if cursor_position < search_input.len() {
                                    cursor_position + 1
                                } else {
                                    search_input.len()
                                };
                                selection_anchor = None;
                            }
                        }
                        KeyCode::Home => {
                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                if selection_anchor.is_none() {
                                    selection_anchor = Some(cursor_position);
                                }
                            } else {
                                selection_anchor = None;
                            }
                            cursor_position = 0;
                        }
                        KeyCode::End => {
                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                if selection_anchor.is_none() {
                                    selection_anchor = Some(cursor_position);
                                }
                            } else {
                                selection_anchor = None;
                            }
                            cursor_position = search_input.len();
                        }
                        KeyCode::Delete => {
                            let edited = if delete_selection_if_any(
                                &mut search_input,
                                &mut cursor_position,
                                &mut selection_anchor,
                            ) {
                                true
                            } else if cursor_position < search_input.len() {
                                search_input.remove(cursor_position);
                                selection_anchor = None;
                                true
                            } else {
                                false
                            };

                            if edited {
                                search_results =
                                    update_results(conn, &search_input, &mut results_state);
                                error_message = None;
                            }
                        }
                        KeyCode::Esc => {
                            return Ok(());
                        }
                        KeyCode::Tab => {
                            if !search_results.is_empty() {
                                focus = Focus::Results;
                                if results_state.selected().is_none() {
                                    results_state.select(Some(0));
                                }
                            }
                        }
                        KeyCode::Char(c) => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                match c.to_ascii_lowercase() {
                                    'c' => {
                                        copy_selection_to_clipboard(
                                            &search_input,
                                            cursor_position,
                                            selection_anchor,
                                            &mut clipboard,
                                        );
                                    }
                                    'x' => {
                                        if cut_selection_to_clipboard(
                                            &mut search_input,
                                            &mut cursor_position,
                                            &mut selection_anchor,
                                            &mut clipboard,
                                        ) {
                                            search_results = update_results(
                                                conn,
                                                &search_input,
                                                &mut results_state,
                                            );
                                            error_message = None;
                                        }
                                    }
                                    'v' => {
                                        if paste_from_clipboard(
                                            &mut search_input,
                                            &mut cursor_position,
                                            &mut selection_anchor,
                                            &mut clipboard,
                                        ) {
                                            search_results = update_results(
                                                conn,
                                                &search_input,
                                                &mut results_state,
                                            );
                                            error_message = None;
                                        }
                                    }
                                    _ => {}
                                }
                                continue;
                            }

                            delete_selection_if_any(
                                &mut search_input,
                                &mut cursor_position,
                                &mut selection_anchor,
                            );
                            search_input.insert(cursor_position, c);
                            cursor_position += 1;
                            selection_anchor = None;
                            search_results = update_results(conn, &search_input, &mut results_state);
                            error_message = None;
                        }
                        _ => {}
                    },
                    Focus::Results => match key.code {
                        KeyCode::Enter => {
                            if let Some(selected) = results_state.selected() {
                                if let Some(path) = search_results.get(selected) {
                                    // Attempt to open the file
                                    match opener::open(path) {
                                        Ok(_) => {}
                                        Err(e) => {
                                            // Handle file not found or other errors
                                            error_message =
                                                Some(format!("Error opening file: {}", path));
                                            eprintln!(
                                                "Failed to open file: {}. Error: {:?}",
                                                path, e
                                            );
                                            // If the error is indeed a file not found, we'd ideally want to re-index.
                                            // For now, we'll just log the error.
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('o') => {
                            if let Some(selected) = results_state.selected() {
                                if let Some(path) = search_results.get(selected) {
                                    handle_file_opening(path, &mut error_message);
                                }
                            }
                        }
                        KeyCode::Char('e') => {
                            if let Some(selected) = results_state.selected() {
                                if let Some(path) = search_results.get(selected) {
                                    disable_raw_mode()?;
                                    execute!(io::stdout(), LeaveAlternateScreen)?;
                                    let editor_result =
                                        open_file_with_editor(path, preferred_editor.clone());
                                    enable_raw_mode()?;
                                    execute!(io::stdout(), EnterAlternateScreen)?;
                                    terminal.clear()?;
                                    if let Err(e) = editor_result {
                                        error_message = Some(format!("Error opening file: {}", e));
                                        eprintln!("Failed to open file: {}. Error: {:?}", path, e);
                                    }
                                }
                            }
                        }
                        KeyCode::Down => {
                            if !search_results.is_empty() {
                                let i = match results_state.selected() {
                                    Some(i) => (i + 1) % search_results.len(),
                                    None => 0,
                                };
                                results_state.select(Some(i));
                            }
                        }
                        KeyCode::Up => {
                            if !search_results.is_empty() {
                                let i = match results_state.selected() {
                                    Some(0) => {
                                        focus = Focus::Search;
                                        selection_anchor = None;
                                        0
                                    }
                                    Some(i) => {
                                        (i + search_results.len() - 1) % search_results.len()
                                    }
                                    None => 0,
                                };
                                results_state.select(Some(i));
                            }
                        }
                        KeyCode::PageDown => {
                            if !search_results.is_empty() {
                                let current = results_state.selected().unwrap_or(0);
                                let next =
                                    (current + RESULT_PAGE_SIZE).min(search_results.len() - 1);
                                results_state.select(Some(next));
                            }
                        }
                        KeyCode::PageUp => {
                            if !search_results.is_empty() {
                                let current = results_state.selected().unwrap_or(0);
                                let next = current.saturating_sub(RESULT_PAGE_SIZE);
                                results_state.select(Some(next));
                            }
                        }
                        KeyCode::Home => {
                            if !search_results.is_empty() {
                                results_state.select(Some(0));
                            }
                        }
                        KeyCode::End => {
                            if !search_results.is_empty() {
                                results_state.select(Some(search_results.len() - 1));
                            }
                        }
                        KeyCode::Tab => {
                            focus = Focus::Search;
                            selection_anchor = None;
                        }
                        KeyCode::Esc => {
                            return Ok(());
                        }
                        KeyCode::Char('d') => {
                            if let Some(selected) = results_state.selected() {
                                if let Some(path) = search_results.get(selected) {
                                    let file_path = PathBuf::from(path);
                                    if let Some(dir_path) = file_path.parent() {
                                        if let Some(dir_str) = dir_path.to_str() {
                                            opener::open(dir_str).unwrap_or_else(|e| {
                                                eprintln!("Failed to open directory: {}", e);
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    },
                }
            }
        }
        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }
}

fn open_file_with_editor(path: &str, preferred_editor: Option<String>) -> Result<()> {
    let editors = if let Some(editor) = preferred_editor {
        vec![
            editor,
            "nvim".to_string(),
            "vim".to_string(),
            "vi".to_string(),
        ]
    } else {
        vec!["nvim".to_string(), "vim".to_string(), "vi".to_string()]
    };

    for editor in editors {
        match Command::new(&editor).arg(path).status() {
            Ok(status) if status.success() => return Ok(()),
            _ => continue,
        }
    }
    eyre::bail!("Could not open file with any editor: nvim, vim, or vi.")
}

// Helper function to create styled spans for highlighting search terms
fn create_highlighted_spans(text: &str, term: &str, highlight_color: &Color) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    if term.is_empty() {
        spans.push(Span::raw(text.to_string()));
        return spans;
    }

    let words: Vec<&str> = term.split_whitespace().collect();
    if words.is_empty() {
        spans.push(Span::raw(text.to_string()));
        return spans;
    }

    let mut matches: Vec<(usize, usize)> = Vec::new();
    let text_lower = text.to_lowercase();
    let basename_start = text_lower.rfind('/').map_or(0, |idx| idx + 1);
    let basename_lower = &text_lower[basename_start..];

    for word in words {
        let word_lower = word.to_lowercase();
        if has_glob_meta_for_highlight(&word_lower) {
            let highlight_pattern = if word_lower.starts_with('*') && word_lower.len() > 1 {
                word_lower.trim_start_matches('*').to_string()
            } else {
                word_lower.clone()
            };

            if word_lower.contains('/') {
                collect_wildcard_matches(&text_lower, &highlight_pattern, 0, &mut matches);
            } else {
                collect_wildcard_matches(
                    basename_lower,
                    &highlight_pattern,
                    basename_start,
                    &mut matches,
                );
            }
        } else {
            for (start, _) in text_lower.match_indices(&word_lower) {
                matches.push((start, start + word_lower.len()));
            }
        }
    }

    // Sort by start index then end index for deterministic rendering.
    matches.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut last_end = 0;
    for (start, end) in matches {
        // Skip if this match is completely contained within a previous match
        if start >= last_end {
            // Add text before the current match
            if start > last_end {
                spans.push(Span::raw(text[last_end..start].to_string()));
            }
            // Add the highlighted match
            spans.push(Span::styled(
                text[start..end].to_string(),
                Style::default().bg(*highlight_color),
            ));
            last_end = end;
        }
    }

    // Add any remaining text after the last match
    if last_end < text.len() {
        spans.push(Span::raw(text[last_end..].to_string()));
    }

    spans
}

fn has_glob_meta_for_highlight(token: &str) -> bool {
    token.contains('*') || token.contains('?') || token.contains('[')
}

fn collect_wildcard_matches(
    target: &str,
    pattern: &str,
    offset: usize,
    matches: &mut Vec<(usize, usize)>,
) {
    if pattern.is_empty() {
        return;
    }

    let Ok(glob) = Pattern::new(pattern) else {
        return;
    };

    let boundaries = char_boundaries(target);
    for start_idx in 0..boundaries.len().saturating_sub(1) {
        let start = boundaries[start_idx];
        for end_idx in (start_idx + 1)..boundaries.len() {
            let end = boundaries[end_idx];
            if glob.matches(&target[start..end]) {
                matches.push((offset + start, offset + end));
                break;
            }
        }
    }
}

fn char_boundaries(s: &str) -> Vec<usize> {
    let mut boundaries: Vec<usize> = s.char_indices().map(|(idx, _)| idx).collect();
    boundaries.push(s.len());
    boundaries
}

fn ui<B: Backend>(
    f: &mut Frame<B>,
    search_input: &str,
    cursor_position: usize,
    selected_range: Option<(usize, usize)>,
    search_results: &[String],
    results_state: &mut ListState,
    focus: &Focus,
    highlight_color: &Color,
    error_message: &Option<String>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(1), // New chunk for error message
            ]
            .as_ref(),
        )
        .split(f.size());

    let search_style = match focus {
        Focus::Search => Style::default().fg(Color::Green),
        _ => Style::default(),
    };
    let input = Paragraph::new(Text::from(Spans::from(create_input_spans(
        search_input,
        selected_range,
    ))))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Search")
            .border_style(search_style),
    );
    f.render_widget(input, chunks[0]);

    if let Focus::Search = focus {
        f.set_cursor(chunks[0].x + cursor_position as u16 + 1, chunks[0].y + 1)
    }

    let results_style = match focus {
        Focus::Results => Style::default().fg(Color::Green),
        _ => Style::default(),
    };
    let results: Vec<ListItem> = search_results
        .iter()
        .map(|item| {
            // Use the search_input for highlighting, not the whole item
            let spans = create_highlighted_spans(item, search_input, highlight_color);
            ListItem::new(Text::from(Spans::from(spans)))
        })
        .collect();

    let results_list = List::new(results)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Results")
                .border_style(results_style),
        )
        .highlight_style(Style::default().bg(*highlight_color));
    f.render_stateful_widget(results_list, chunks[1], results_state);

    let mut summary_text = if search_results.is_empty() {
        "0 items".to_string()
    } else {
        format!(
            "{}/{} items",
            results_state.selected().map_or(0, |i| i + 1),
            search_results.len()
        )
    };

    // Add shortcuts based on focus
    let shortcuts_text = match focus {
        Focus::Search => {
            " | Ctrl+C/X/V: Clipboard | Ctrl+Shift+←/→: Select Word | Alt+Backspace: Del Word | Shift+Home/End: Select | Esc: Quit"
        }
        Focus::Results => {
            " | Enter/o: Open | e: Edit | d: Dir | PgUp/PgDn/Home/End: Navigate | Tab: Search | Esc: Quit"
        }
    };
    summary_text.push_str(shortcuts_text);

    let summary = Paragraph::new(summary_text).style(Style::default().fg(Color::Gray));
    f.render_widget(summary, chunks[2]);

    // Render error message if present
    if let Some(err) = error_message {
        let error_style = Style::default().fg(Color::Red);
        // Use err.as_str() to convert String to &str for Paragraph::new
        let error_paragraph = Paragraph::new(err.as_str()).style(error_style);
        f.render_widget(error_paragraph, chunks[3]);
    }
}
