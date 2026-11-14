//! Text-formatting commands for the note editor. Pure functions over char indices so they are easy to test.

#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub text: String,
    /// Selection after the edit, as char indices (start, end).
    pub sel: (usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    Paragraph,
    Heading(u8),
    Bullet,
    Numbered,
    Task,
    Quote,
}

fn count_run(chars: &[char], from: usize, forward: bool, ch: char) -> usize {
    let mut n = 0;
    if forward {
        while from + n < chars.len() && chars[from + n] == ch {
            n += 1;
        }
    } else {
        while n < from && chars[from - 1 - n] == ch {
            n += 1;
        }
    }
    n
}

/// Wraps the selection in `open`/`close`, or removes them if already wrapped.
/// With an empty selection the markers are inserted and the cursor is placed between them.
pub fn toggle_wrap(text: &str, sel: (usize, usize), open: &str, close: &str) -> Edit {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let (a, b) = (sel.0.min(sel.1).min(len), sel.0.max(sel.1).min(len));
    let (o, c): (Vec<char>, Vec<char>) = (open.chars().collect(), close.chars().collect());
    let (oc, cc) = (o.len(), c.len());
    let is_italic_marker = open == "*" && close == "*";

    let wrapped_outside = a >= oc && b + cc <= len && chars[a - oc..a] == o[..] && chars[b..b + cc] == c[..] && {
        // A single `*` is only italic when the run of stars is odd (`**` is bold).
        !is_italic_marker || (count_run(&chars, a, false, '*') % 2 == 1 && count_run(&chars, b, true, '*') % 2 == 1)
    };
    let mk = |chars: Vec<char>, s: usize, e: usize| Edit { text: chars.into_iter().collect(), sel: (s, e) };

    if wrapped_outside {
        let mut out = chars[..a - oc].to_vec();
        out.extend_from_slice(&chars[a..b]);
        out.extend_from_slice(&chars[b + cc..]);
        return mk(out, a - oc, b - oc);
    }
    if b - a >= oc + cc && chars[a..a + oc] == o[..] && chars[b - cc..b] == c[..] && !is_italic_marker {
        let mut out = chars[..a].to_vec();
        out.extend_from_slice(&chars[a + oc..b - cc]);
        out.extend_from_slice(&chars[b..]);
        return mk(out, a, b - oc - cc);
    }
    let mut out = chars[..a].to_vec();
    out.extend_from_slice(&o);
    out.extend_from_slice(&chars[a..b]);
    out.extend_from_slice(&c);
    out.extend_from_slice(&chars[b..]);
    mk(out, a + oc, b + oc)
}

/// Strips a list, quote or heading marker from the start of a line.
pub fn strip_block_prefix(line: &str) -> &str {
    let t = line;
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
        return &t[hashes + 1..];
    }
    for p in ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "> "] {
        if let Some(r) = t.strip_prefix(p) {
            return r;
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && t[digits..].starts_with(". ") {
        return &t[digits + 2..];
    }
    t
}

fn block_prefix(block: Block, n: usize) -> String {
    match block {
        Block::Paragraph => String::new(),
        Block::Heading(l) => format!("{} ", "#".repeat(l.clamp(1, 6) as usize)),
        Block::Bullet => "- ".into(),
        Block::Numbered => format!("{n}. "),
        Block::Task => "- [ ] ".into(),
        Block::Quote => "> ".into(),
    }
}

/// Applies a block style to every line the selection touches. Applying the same style again removes it.
pub fn set_block(text: &str, sel: (usize, usize), block: Block) -> Edit {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let (a, b) = (sel.0.min(sel.1).min(len), sel.0.max(sel.1).min(len));
    let start = chars[..a].iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1);
    let end = chars[b..].iter().position(|c| *c == '\n').map_or(len, |i| b + i);
    let region: String = chars[start..end].iter().collect();
    let lines: Vec<&str> = region.split('\n').collect();

    let non_empty = lines.iter().filter(|l| !l.trim().is_empty()).count();
    let already = block != Block::Paragraph
        && non_empty > 0
        && lines.iter().enumerate().all(|(i, l)| l.trim().is_empty() || l.starts_with(&block_prefix(block, i + 1)));
    let mut n = 0;
    let new_lines: Vec<String> = lines
        .iter()
        .map(|l| {
            let body = strip_block_prefix(l);
            if already || block == Block::Paragraph || (l.trim().is_empty() && lines.len() > 1) {
                body.to_string()
            } else {
                n += 1;
                format!("{}{}", block_prefix(block, n), body)
            }
        })
        .collect();
    let new_region = new_lines.join("\n");
    let new_len = new_region.chars().count();
    let mut out: Vec<char> = chars[..start].to_vec();
    out.extend(new_region.chars());
    out.extend_from_slice(&chars[end..]);
    Edit { text: out.into_iter().collect(), sel: (start, start + new_len) }
}

/// Inserts a horizontal rule on its own line after the selection.
pub fn insert_rule(text: &str, sel: (usize, usize)) -> Edit {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let b = sel.0.max(sel.1).min(len);
    let end = chars[b..].iter().position(|c| *c == '\n').map_or(len, |i| b + i);
    let mut out: Vec<char> = chars[..end].to_vec();
    let ins: Vec<char> = "\n\n---\n\n".chars().collect();
    out.extend_from_slice(&ins);
    out.extend_from_slice(&chars[end..]);
    let pos = end + ins.len();
    Edit { text: out.into_iter().collect(), sel: (pos, pos) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_unwraps_selection() {
        let e = toggle_wrap("hello world", (6, 11), "**", "**");
        assert_eq!(e.text, "hello **world**");
        assert_eq!(e.sel, (8, 13));
        let back = toggle_wrap(&e.text, e.sel, "**", "**");
        assert_eq!(back.text, "hello world");
        assert_eq!(back.sel, (6, 11));
    }

    #[test]
    fn unwraps_when_markers_are_selected() {
        let e = toggle_wrap("a ~~b~~ c", (2, 7), "~~", "~~");
        assert_eq!(e.text, "a b c");
    }

    #[test]
    fn empty_selection_puts_cursor_between_markers() {
        let e = toggle_wrap("ab", (1, 1), "*", "*");
        assert_eq!(e.text, "a**b");
        assert_eq!(e.sel, (2, 2));
    }

    #[test]
    fn italic_inside_bold_adds_a_star_pair() {
        let e = toggle_wrap("**x**", (2, 3), "*", "*");
        assert_eq!(e.text, "***x***");
        let back = toggle_wrap(&e.text, e.sel, "*", "*");
        assert_eq!(back.text, "**x**");
    }

    #[test]
    fn handles_multibyte_text() {
        let e = toggle_wrap("café £5", (0, 4), "**", "**");
        assert_eq!(e.text, "**café** £5");
        assert_eq!(toggle_wrap(&e.text, e.sel, "**", "**").text, "café £5");
    }

    #[test]
    fn headings_replace_and_toggle() {
        let e = set_block("Title", (2, 2), Block::Heading(2));
        assert_eq!(e.text, "## Title");
        assert_eq!(set_block(&e.text, e.sel, Block::Heading(1)).text, "# Title");
        assert_eq!(set_block(&e.text, e.sel, Block::Heading(2)).text, "Title");
        assert_eq!(set_block(&e.text, e.sel, Block::Paragraph).text, "Title");
    }

    #[test]
    fn lists_apply_to_each_selected_line() {
        let e = set_block("one\ntwo\nthree", (0, 13), Block::Numbered);
        assert_eq!(e.text, "1. one\n2. two\n3. three");
        let b = set_block(&e.text, e.sel, Block::Bullet);
        assert_eq!(b.text, "- one\n- two\n- three");
        assert_eq!(set_block(&b.text, b.sel, Block::Bullet).text, "one\ntwo\nthree");
    }

    #[test]
    fn only_touched_lines_change() {
        let e = set_block("a\nb\nc", (2, 2), Block::Quote);
        assert_eq!(e.text, "a\n> b\nc");
    }

    #[test]
    fn rule_goes_after_the_current_line() {
        let e = insert_rule("one\ntwo", (1, 1));
        assert_eq!(e.text, "one\n\n---\n\n\ntwo");
    }
}
