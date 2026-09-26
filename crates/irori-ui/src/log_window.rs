//! What an extension has said for itself lately, in a window of its own.
//!
//! The guideline this exists to serve (`docs/specs/extensions.md` §8): a failure a person can see
//! should be one they can act on. A card's reason line carries the extension's last words, which
//! is usually enough; when it isn't, the rest of what it said is one press away rather than in a
//! terminal they may not have open.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;

const REFRESH: Duration = Duration::from_secs(2);

#[component]
pub fn LogWindow(id: String, #[prop(into)] on_close: Callback<()>) -> impl IntoView {
    let lines = RwSignal::new(Vec::<String>::new());
    let trouble = RwSignal::new(None::<String>);
    let asked = RwSignal::new(false);

    // Helper to detect and format log line elements using simple string patterns
    fn format_log_line(line: String) -> String {
        let upper = line.to_uppercase();
        
        // Priority 1: ERROR messages (word boundary check)
        if find_word_boundary(&upper, "ERROR").is_some() {
            return format!(
                "<span class=\"log-error\">{}</span>",
                escape_html(&line)
            );
        }

        // Priority 2: WARNING/WARN messages (word boundary check)  
        if find_word_boundary(&upper, "WARNING").is_some() {
            return format!(
                "<span class=\"log-warn\">{}</span>",
                escape_html(&line)
            );
        }

        // Priority 3: Timestamps at start of line (HH:MM:SS.ddd or HH:MM:SS)
        if let Some(ts_pos) = find_timestamp_start(&line) {
            return format!(
                "<span class=\"log-time\">[{}]</span><span class=\"log-info\">{}</span>",
                escape_html(&line[..ts_pos]),
                escape_html(&line[ts_pos..])
            );
        }

        // Priority 4: Windows paths (C:\ or C:/ at start of line, not just bare drive letter)
        if let Some(pos) = find_path_start(&line) {
            if pos > 0 || line[pos] != ':' {
                let rest = &line[pos..];
                
                return format!(
                    "<span class=\"log-path\">{}</span>{rest}",
                    escape_html(rest.trim_start_matches(['\\', '/', ' ', '\t']))
                );
            }
        }

        // No pattern matched - return raw escaped line
        escape_html(&line)
    }

    // Find position where a word pattern starts (word boundary check)
    fn find_word_boundary(text: &str, pattern: &str) -> Option<usize> {
        text.find(pattern).and_then(|pos| {
            if pos == 0 || text[pos - 1].is_whitespace() {
                // Check if pattern is at end of string or followed by non-alphabetic char
                let next_pos = pos + pattern.len();
                if next_pos >= text.len() {
                    Some(pos) // At end of string, it's a word boundary
                } else {
                    let next_char = text[next_pos];
                    if next_char == '\n' 
                        || next_char == ' ' 
                        || !next_char.is_alphabetic() {
                        Some(pos)
                    } else {
                        None
                    }
                }
            } else {
                None
            }
        })
    }

    // Find timestamp start (HH:MM:SS.ddd or HH:MM:SS at start of line)
    fn find_timestamp_start(line: &str) -> Option<usize> {
        if line.len() < 12 { return None; }
        
        // Check first 13 chars for HH:MM:SS pattern
        let first_chars: Vec<char> = line.chars().take(13).collect();
        
        // Need at least "HH:MM:" (6 chars) plus either SS or SS.something (2+ chars)
        if first_chars.len() < 8 { return None; }
        
        // Check for colons at positions 2 and 5
        if first_chars[2] != ':' || first_chars[5] != ':' { return None; }
        
        // Check HH are digits
        let hh = first_chars[0..2].collect::<String>();
        if !hh.chars().all(|c| c.is_ascii_digit()) { return None; }
        
        // Check MM are digits
        let mm = first_chars[3..5].collect::<String>();
        if !mm.chars().all(|c| c.is_ascii_digit()) { return None; }
        
        // Check SS are digits
        let ss = first_chars[6..8].collect::<String>();
        if !ss.chars().all(|c| c.is_ascii_digit()) { return None; }
        
        // Find where the timestamp ends (either at position 8, or with optional .ddd)
        let mut ts_end = 8;

        if ts_end < first_chars.len() && first_chars[ts_end] == '.' {
            // Check for milliseconds (.ddd format) - need exactly 3 digits after dot
            if ts_end + 4 <= first_chars.len() {
                let ms_bytes: &[u8] = &first_chars.as_bytes()[ts_end+1..ts_end+4];
                let ms_str: String = ms_bytes.iter().collect();
                if ms_str.chars().all(|c| c.is_ascii_digit()) && ms_str.len() == 3 {
                    ts_end += 4; // include dot + milliseconds
                }
            }
        }

        Some(ts_end) // Return the actual end position of the timestamp
    }

    // Find Windows path start (first \ or / character)
    fn find_path_start(line: &str) -> Option<usize> {
        line.chars().enumerate().find(|(_, c)| matches!(c, '\\' | '/')).map(|(pos, _)| pos)
    }

    let load = {
        let id = id.clone();
        move || {
            let id = id.clone();
            spawn_local(async move {
                match crate::api::fetch_extension_log(&id).await {
                    Ok(said) => {
                        lines.set(said);
                        trouble.set(None);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
                asked.set(true);
            });
        }
    };
    load();

    let handle = set_interval_with_handle(load, REFRESH).ok();
    on_cleanup(move || {
        if let Some(handle) = handle {
            handle.clear();
        }
    });

    // Copy button feedback state
    let copy_feedback: CopyFeedback = RwSignal::new(None);
    let copy_btn = move |_| {
        let lines = lines.read();
        let text = lines.join("\n");
        
        leptos_use_navigator::navigator()
            .map(|nav| {
                let _ = navigator::clipboard::write(&text);
                copy_feedback.set(Some("Copied!".to_string()));
                set_timeout(Duration::from_millis(1500), || {
                    copy_feedback.set(None);
                });
            })
            .unwrap_or_else(|_| {
                spawn_local(async move {
                    let _ = navigator::clipboard::write(&text);
                    copy_feedback.set(Some("Copied!".to_string()));
                    set_timeout(Duration::from_millis(1500), || {
                        copy_feedback.set(None);
                    });
                })
            });

        view! {}
    };

    view! {
        <crate::modal::Modal title=format!("{id} log") on_close=on_close>
            <p class="muted small">
                "What this extension has written to its own output, oldest first. Irori keeps the "
                "most recent lines only, and starts again each time the extension is reinstalled."
            </p>
            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            {move || {
                let said = lines.get();
                if said.is_empty() {
                    (asked.get())
                        .then(|| view! {
                            <p class="muted">
                                "Nothing yet. A built-in extension has no output of its own, and "
                                "one that hasn't started has nothing to say."
                            </p>
                        })
                        .into_any()
                } else {
                    view! {
                        <pre class="ext-log">
                            <button class="copy-btn" onclick=move |_| copy_btn() title="Copy all text">📋</button>
                            <span class="copy-feedback">{ move || copy_feedback.get().as_deref().unwrap_or_default() }</span>
                            {said.iter().map(format_log_line).collect::<Vec<_>>().into_view()}
                        </pre>
                    }
                        .into_any()
                }
            }}
        </crate::modal::Modal>
    }
}

/// Escape HTML special characters to prevent XSS when displaying log content
fn escape_html(s: &str) -> String {
    let mut escaped = s
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;");
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("Hello <World>"), "Hello &lt;World&gt;");
        assert_eq!(escape_html("A&B"), "A&amp;B");
        assert_eq!(escape_html(""), "");
    }

    #[test]
    fn test_find_word_boundary() {
        // Should find ERROR with word boundary
        assert!(find_word_boundary("My ERROR message", "ERROR").is_some());
        
        // Should NOT find ERROR without word boundary (part of another word)
        assert!(find_word_boundary("MyERRORmessage", "ERROR").is_none());

        // Should find ERROR at start
        assert!(find_word_boundary("ERROR occurred", "ERROR").is_some());

        // ERROR at end of line (no trailing character)
        assert!(find_word_boundary("This has ERROR", "ERROR").is_some());
    }

    #[test]
    fn test_find_timestamp_start() {
        // Basic HH:MM:SS format
        assert_eq!(find_timestamp_start("12:34:56 message"), Some(8));
        
        // HH:MM:SS with milliseconds
        assert_eq!(find_timestamp_start("12:34:56.123 message"), Some(12));
        
        // No timestamp (too short)
        assert_eq!(find_timestamp_start("short"), None);
        
        // Valid time but not at start
        assert_eq!(find_timestamp_start("text 12:34:56"), None);

        // HH:MM format only
        assert_eq!(find_timestamp_start("09:00 started"), Some(5));
    }

    #[test]
    fn test_format_log_line_error() {
        let result = format_log_line(String::from("ERROR something bad"));
        assert!(result.contains("<span class=\"log-error\""));
    }

    #[test]
    fn test_format_log_line_timestamp() {
        let result = format_log_line(String::from("12:34:56 Error message"));
        assert!(result.contains("[12:34:56]"));
        assert!(result.contains("<span class=\"log-info\""));
    }

    #[test]
    fn test_format_log_line_milliseconds() {
        let result = format_log_line(String::from("12:34:56.999 Warning here"));
        assert!(result.contains("[12:34:56.999]"));
    }

    #[test]
    fn test_format_log_line_empty() {
        let result = format_log_line(String::from(""));
        assert_eq!(result, "");
    }

    #[test]
    fn test_find_path_start() {
        // Windows path with backslash
        assert_eq!(find_path_start("C:\\Users\\test"), Some(1));
        
        // Unix path with forward slash
        assert_eq!(find_path_start("/home/user/test"), Some(0));

        // No path
        assert_eq!(find_path_start("hello world"), None);
    }
}
