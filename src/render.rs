use crate::engine::Event;
use std::fmt::Write;

/// Minimal stdout: batch text and immediate watch literals, with no framing.
/// Watch signals may repeat text that later appears in a selected batch.
pub fn raw(event: &Event) -> String {
    match event.kind {
        "batch" => event.text.clone().unwrap_or_default(),
        "watch" => format!("{}\n", event.watch.as_deref().unwrap_or_default()),
        "end" => String::new(),
        _ => unreachable!("the engine emits only batch, watch, and end events"),
    }
}

/// Presentation only: the engine and hook schema remain independent of stdout.
pub fn markdown(event: &Event) -> String {
    let mut out = String::new();
    match event.kind {
        "batch" => {
            let continuation = if event.continued == Some(true) {
                ", continues"
            } else {
                ""
            };
            writeln!(
                out,
                "### Batch {} ({}{continuation})\n",
                event.seq, event.reason
            )
            .unwrap();
            code_block(&mut out, event.text.as_deref().unwrap_or_default());
        }
        "watch" => {
            writeln!(
                out,
                "### Watch {}: {}\n",
                event.seq,
                escape_heading(event.watch.as_deref().unwrap_or_default())
            )
            .unwrap();
            code_block(&mut out, event.text.as_deref().unwrap_or_default());
            if event.excerpt_truncated == Some(true) {
                out.push_str("Excerpt truncated.\n\n");
            }
        }
        "end" => {
            let outcome = if let Some(signal) = event.signal {
                format!("signal {signal}")
            } else if let Some(code) = event.exit_code {
                format!("exit {code}")
            } else {
                "input closed".into()
            };
            writeln!(
                out,
                "### End ({outcome})\n\n{} input lines; {} included; {} excluded.\n",
                event.input_lines, event.included_lines, event.excluded_lines
            )
            .unwrap();
        }
        _ => unreachable!("the engine emits only batch, watch, and end events"),
    }
    out
}

fn code_block(out: &mut String, text: &str) {
    // Choose the shorter safe fence. Even log lines containing Markdown fences
    // remain literal content rather than closing the surrounding code block.
    let longest = |delimiter| {
        let mut longest = 0;
        let mut run = 0;
        for byte in text.bytes() {
            run = if byte == delimiter { run + 1 } else { 0 };
            longest = longest.max(run);
        }
        longest
    };
    let ticks = longest(b'`');
    let tildes = longest(b'~');
    let (delimiter, length) = if ticks <= tildes {
        ('`', ticks)
    } else {
        ('~', tildes)
    };
    let fence: String = std::iter::repeat_n(delimiter, (length + 1).max(3)).collect();
    writeln!(out, "{fence}text").unwrap();
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    writeln!(out, "{fence}\n").unwrap();
}

fn escape_heading(text: &str) -> String {
    let mut out = String::new();
    for character in text.chars() {
        if character.is_control() {
            out.extend(character.escape_default());
        } else {
            if matches!(
                character,
                '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '~' | '&'
            ) {
                out.push('\\');
            }
            out.push(character);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_fences_are_preserved_inside_a_longer_safe_fence() {
        let text = "```\n~~~\n# raw log\n";
        let mut rendered = String::new();
        code_block(&mut rendered, text);
        assert_eq!(rendered, "````text\n```\n~~~\n# raw log\n````\n\n");
    }

    #[test]
    fn shorter_alternative_fence_avoids_large_framing() {
        let mut rendered = String::new();
        code_block(&mut rendered, "``````````");
        assert_eq!(rendered, "~~~text\n``````````\n~~~\n\n");
    }

    #[test]
    fn watch_heading_cannot_insert_markdown_or_control_lines() {
        assert_eq!(
            escape_heading("[done](x)\r# é &copy;"),
            "\\[done\\](x)\\r\\# é \\&copy;"
        );
    }
}
