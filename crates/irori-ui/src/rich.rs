//! A model's answer, read for the little formatting it uses: paragraphs, lists, headings,
//! **bold**, and `values`. It becomes elements, never markup: what a model writes, and the
//! device names it repeats, are text and stay text.

use leptos::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Para(Vec<Span>),
    Heading(Vec<Span>),
    /// A list line. `mark` is its number when it has one.
    Item {
        mark: Option<String>,
        spans: Vec<Span>,
    },
    Code(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Text(String),
    Bold(String),
    /// A value or a state, which is what a model is asked to put in backticks.
    Value(String),
}

pub fn parse(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para = String::new();
    let mut code: Option<String> = None;
    let close = |para: &mut String, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Para(spans(para)));
            para.clear();
        }
    };
    for line in text.lines() {
        if let Some(open) = code.as_mut() {
            if line.trim_start().starts_with("```") {
                blocks.push(Block::Code(code.take().unwrap_or_default()));
            } else {
                if !open.is_empty() {
                    open.push('\n');
                }
                open.push_str(line);
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            close(&mut para, &mut blocks);
            code = Some(String::new());
        } else if trimmed.is_empty() {
            close(&mut para, &mut blocks);
        } else if let Some(rest) = trimmed.trim_start_matches('#').strip_prefix(' ')
            && trimmed.starts_with('#')
        {
            close(&mut para, &mut blocks);
            blocks.push(Block::Heading(spans(rest)));
        } else if let Some((mark, rest)) = item(trimmed) {
            close(&mut para, &mut blocks);
            blocks.push(Block::Item {
                mark,
                spans: spans(rest),
            });
        } else {
            if !para.is_empty() {
                para.push('\n');
            }
            para.push_str(trimmed);
        }
    }
    // An answer still being written can end inside a fence.
    if let Some(open) = code {
        blocks.push(Block::Code(open));
    }
    close(&mut para, &mut blocks);
    blocks
}

/// `- lamp`, `* lamp`, `• lamp`, `1. lamp`, or `2) lamp`.
fn item(line: &str) -> Option<(Option<String>, &str)> {
    for bullet in ["- ", "* ", "• "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return Some((None, rest.trim_start()));
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if (1..=3).contains(&digits) {
        let rest = &line[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((Some(line[..digits].to_owned()), rest.trim_start()));
        }
    }
    None
}

/// A marker with no partner is left as the characters it is.
fn spans(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let found = [("**", true), ("`", false)]
            .into_iter()
            .find_map(|(mark, bold)| {
                let inner = rest.strip_prefix(mark)?;
                let end = inner.find(mark)?;
                (end > 0 && !inner[..end].contains('\n'))
                    .then(|| (bold, &inner[..end], &inner[end + mark.len()..]))
            });
        if let Some((bold, inner, after)) = found {
            if !plain.is_empty() {
                out.push(Span::Text(std::mem::take(&mut plain)));
            }
            out.push(if bold {
                Span::Bold(inner.to_owned())
            } else {
                Span::Value(inner.to_owned())
            });
            rest = after;
        } else {
            let mut chars = rest.chars();
            if let Some(c) = chars.next() {
                plain.push(c);
            }
            rest = chars.as_str();
        }
    }
    if !plain.is_empty() {
        out.push(Span::Text(plain));
    }
    out
}

fn inline(spans: Vec<Span>) -> impl IntoView {
    spans
        .into_iter()
        .map(|span| match span {
            Span::Text(text) => text.into_any(),
            Span::Bold(text) => view! { <strong>{text}</strong> }.into_any(),
            Span::Value(text) => view! { <code class="value">{text}</code> }.into_any(),
        })
        .collect_view()
}

/// The answer, as elements.
pub fn render(text: &str) -> impl IntoView + use<> {
    parse(text)
        .into_iter()
        .map(|block| match block {
            Block::Para(spans) => view! { <p>{inline(spans)}</p> }.into_any(),
            Block::Heading(spans) => view! { <p class="rich-heading">{inline(spans)}</p> }.into_any(),
            Block::Item { mark, spans } => view! {
                <p class="rich-item">
                    <span class="rich-mark">{mark.map_or("•".to_owned(), |n| format!("{n}."))}</span>
                    <span>{inline(spans)}</span>
                </p>
            }
            .into_any(),
            Block::Code(code) => view! { <pre class="rich-code">{code}</pre> }.into_any(),
        })
        .collect_view()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Span {
        Span::Text(text.to_owned())
    }

    #[test]
    fn values_and_names_are_picked_out_of_a_sentence() {
        assert_eq!(
            parse("The **Desk lamp** is `on` at `40 %`."),
            vec![Block::Para(vec![
                text("The "),
                Span::Bold("Desk lamp".into()),
                text(" is "),
                Span::Value("on".into()),
                text(" at "),
                Span::Value("40 %".into()),
                text("."),
            ])]
        );
    }

    #[test]
    fn a_marker_without_its_partner_is_just_characters() {
        assert_eq!(
            parse("2 * 3 ** 4 and a ` tick"),
            vec![Block::Para(vec![text("2 * 3 ** 4 and a ` tick")])]
        );
    }

    #[test]
    fn lists_headings_and_paragraphs_are_told_apart() {
        assert_eq!(
            parse("## Lights\n- Lamp: `on`\n2. Strip\n\nTwo lines\nof one thought."),
            vec![
                Block::Heading(vec![text("Lights")]),
                Block::Item {
                    mark: None,
                    spans: vec![text("Lamp: "), Span::Value("on".into())],
                },
                Block::Item {
                    mark: Some("2".into()),
                    spans: vec![text("Strip")],
                },
                Block::Para(vec![text("Two lines\nof one thought.")]),
            ]
        );
    }

    #[test]
    fn markup_in_an_answer_stays_text() {
        assert_eq!(
            parse("<img src=x onerror=alert(1)>"),
            vec![Block::Para(vec![text("<img src=x onerror=alert(1)>")])]
        );
    }

    #[test]
    fn a_fence_still_open_shows_what_has_arrived() {
        assert_eq!(
            parse("```\nlet x = 1;"),
            vec![Block::Code("let x = 1;".into())]
        );
    }
}
