//! Markdown preview with real inline styles (bold, italic, strikethrough, underline, highlight, code, links)
//! in a chosen document font and size.
use crate::fonts::DocFont;
use crate::theme::*;
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontFamily, FontId, Margin, RichText, Stroke, Ui};

pub struct DocStyle {
    pub font: DocFont,
    pub size: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub underline: bool,
    pub mark: bool,
    pub code: bool,
    pub link: bool,
}

fn starts(chars: &[char], i: usize, marker: &str) -> bool {
    let m: Vec<char> = marker.chars().collect();
    chars.len() >= i + m.len() && chars[i..i + m.len()] == m[..]
}

fn find(chars: &[char], from: usize, marker: &str) -> Option<usize> {
    (from..chars.len()).find(|&j| starts(chars, j, marker))
}

/// Splits a line into styled spans. A marker only opens a style if a matching closer follows,
/// so stray `*` or `~~` stay literal.
pub fn spans(s: &str) -> Vec<Span> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<Span> = vec![];
    let mut cur = Span::default();
    let mut buf = String::new();
    let mut i = 0;

    fn flush(out: &mut Vec<Span>, cur: &Span, buf: &mut String) {
        if !buf.is_empty() {
            let mut sp = cur.clone();
            sp.text = std::mem::take(buf);
            out.push(sp);
        }
    }

    while i < chars.len() {
        if cur.code {
            if chars[i] == '`' {
                flush(&mut out, &cur, &mut buf);
                cur.code = false;
            } else {
                buf.push(chars[i]);
            }
            i += 1;
            continue;
        }
        if starts(&chars, i, "[[") {
            if let Some(j) = find(&chars, i + 2, "]]") {
                let inner: String = chars[i + 2..j].iter().collect();
                if !inner.trim().is_empty() && !inner.contains('\\') {
                    flush(&mut out, &cur, &mut buf);
                    let mut sp = cur.clone();
                    sp.text = inner;
                    sp.link = true;
                    out.push(sp);
                    i = j + 2;
                    continue;
                }
            }
        }
        // (marker, current flag, setter)
        let toggles: [(&str, bool); 6] = [("**", cur.bold), ("~~", cur.strike), ("==", cur.mark), ("<u>", cur.underline), ("`", cur.code), ("*", cur.italic)];
        let mut handled = false;
        for (idx, (marker, on)) in toggles.iter().enumerate() {
            if starts(&chars, i, marker) {
                let closer = if *marker == "<u>" { "</u>" } else { marker };
                // A single `*` directly followed by another `*` is part of a longer run, not an italic opener.
                let lone_star_run = *marker == "*" && chars.get(i + 1) == Some(&'*');
                let opening = !*on && !lone_star_run && find(&chars, i + marker.chars().count(), closer).is_some();
                if opening || *on {
                    flush(&mut out, &cur, &mut buf);
                    let v = !*on;
                    match idx {
                        0 => cur.bold = v,
                        1 => cur.strike = v,
                        2 => cur.mark = v,
                        3 => cur.underline = v,
                        4 => cur.code = v,
                        _ => cur.italic = v,
                    }
                    i += marker.chars().count();
                    handled = true;
                    break;
                }
            }
        }
        if handled {
            continue;
        }
        if cur.underline && starts(&chars, i, "</u>") {
            flush(&mut out, &cur, &mut buf);
            cur.underline = false;
            i += 4;
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut out, &cur, &mut buf);
    out
}

fn job(spans: &[Span], style: &DocStyle, size: f32, bold_all: bool, color: Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    for s in spans {
        let family = if s.code {
            FontFamily::Monospace
        } else if s.bold || bold_all {
            style.font.bold.clone()
        } else {
            style.font.regular.clone()
        };
        let mut f = TextFormat { font_id: FontId::new(if s.code { size * 0.92 } else { size }, family), color, italics: s.italic, ..Default::default() };
        if s.strike {
            f.strikethrough = Stroke::new(1.2, color);
        }
        if s.underline || s.link {
            f.underline = Stroke::new(1.0, if s.link { ACCENT } else { color });
        }
        if s.link {
            f.color = ACCENT;
        }
        if s.mark {
            f.background = ACCENT;
            f.color = ON_ACCENT;
        }
        if s.code {
            f.background = RAISED;
        }
        job.append(&s.text, 0.0, f);
    }
    job
}

/// Draws a checkbox glyph with the painter so it does not depend on font coverage.
fn check_glyph(ui: &mut Ui, checked: bool, size: f32) {
    let side = size * 0.85;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side + 2.0), egui::Sense::hover());
    let r = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 3.0), egui::vec2(side, side));
    let col = if checked { ACCENT } else { MUTED };
    ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, col), egui::StrokeKind::Inside);
    if checked {
        let p = |x: f32, y: f32| egui::pos2(r.left() + r.width() * x, r.top() + r.height() * y);
        ui.painter().line_segment([p(0.22, 0.55), p(0.43, 0.76)], Stroke::new(2.0, col));
        ui.painter().line_segment([p(0.43, 0.76), p(0.8, 0.26)], Stroke::new(2.0, col));
    }
}

fn orderd_prefix(t: &str) -> Option<(&str, &str)> {
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    (digits > 0 && t[digits..].starts_with(". ")).then(|| (&t[..digits], &t[digits + 2..]))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    /// Char index of the start of the heading line (for placing the editor cursor and scroll).
    pub char_idx: usize,
}

/// Headings (levels 1 to 3, as the preview draws them), in document order. Skips fenced code.
pub fn headings(src: &str) -> Vec<Heading> {
    let mut out = vec![];
    let mut in_code = false;
    let mut idx = 0usize;
    for line in src.split('\n') {
        let start = idx;
        idx += line.chars().count() + 1;
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let t = line.trim_end();
        let (level, rest) = if let Some(h) = t.strip_prefix("### ") {
            (3, h)
        } else if let Some(h) = t.strip_prefix("## ") {
            (2, h)
        } else if let Some(h) = t.strip_prefix("# ") {
            (1, h)
        } else {
            continue;
        };
        let text: String = spans(rest).iter().map(|s| s.text.as_str()).collect();
        out.push(Heading { level, text, char_idx: start });
    }
    out
}

/// `scroll_to` is the index (in `headings` order) of a heading to bring into view.
pub fn render(ui: &mut Ui, src: &str, style: &DocStyle, scroll_to: Option<usize>) {
    let mut heading_no = 0usize;
    let reveal = |resp: egui::Response, heading_no: &mut usize| {
        if scroll_to == Some(*heading_no) {
            resp.scroll_to_me(Some(egui::Align::Min));
        }
        *heading_no += 1;
    };
    let width = ui.available_width() - 10.0;
    let mut code_block: Option<Vec<&str>> = None;
    for line in src.lines() {
        if line.trim_start().starts_with("```") {
            match code_block.take() {
                Some(lines) => {
                    egui::Frame::new().fill(CARD).corner_radius(8).inner_margin(Margin::same(10)).show(ui, |ui| {
                        ui.set_width(width - 20.0);
                        ui.label(RichText::new(lines.join("\n")).monospace().size(style.size * 0.92));
                    });
                }
                None => code_block = Some(vec![]),
            }
            continue;
        }
        if let Some(lines) = code_block.as_mut() {
            lines.push(line);
            continue;
        }
        let t = line.trim_end();
        let (s, size) = (style.size, style.size);
        if t.is_empty() {
            ui.add_space(s * 0.4);
        } else if let Some(h) = t.strip_prefix("### ") {
            ui.add_space(4.0);
            let resp = ui.label(job(&spans(h), style, size * 1.25, true, TEXT, width));
            reveal(resp, &mut heading_no);
        } else if let Some(h) = t.strip_prefix("## ") {
            ui.add_space(6.0);
            let resp = ui.label(job(&spans(h), style, size * 1.55, true, TEXT, width));
            reveal(resp, &mut heading_no);
        } else if let Some(h) = t.strip_prefix("# ") {
            ui.add_space(8.0);
            let resp = ui.label(job(&spans(h), style, size * 2.0, true, TEXT, width));
            reveal(resp, &mut heading_no);
            ui.add_space(2.0);
        } else if t == "---" || t == "***" {
            ui.add_space(4.0);
            ui.separator();
        } else if let Some(x) = t.strip_prefix("- [x] ").or_else(|| t.strip_prefix("- [X] ")) {
            ui.horizontal_top(|ui| {
                check_glyph(ui, true, size);
                let mut sp = spans(x);
                sp.iter_mut().for_each(|p| p.strike = true);
                ui.add(egui::Label::new(job(&sp, style, size, false, MUTED, width - 44.0)).wrap_mode(egui::TextWrapMode::Wrap));
            });
        } else if let Some(x) = t.strip_prefix("- [ ] ") {
            ui.horizontal_top(|ui| {
                check_glyph(ui, false, size);
                ui.add(egui::Label::new(job(&spans(x), style, size, false, TEXT, width - 44.0)).wrap_mode(egui::TextWrapMode::Wrap));
            });
        } else if let Some(x) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("•").size(size));
                ui.add(egui::Label::new(job(&spans(x), style, size, false, TEXT, width - 40.0)).wrap_mode(egui::TextWrapMode::Wrap));
            });
        } else if let Some((n, x)) = orderd_prefix(t) {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(format!("{n}.")).size(size).color(MUTED));
                ui.add(egui::Label::new(job(&spans(x), style, size, false, TEXT, width - 48.0)).wrap_mode(egui::TextWrapMode::Wrap));
            });
        } else if let Some(x) = t.strip_prefix("> ") {
            let r = egui::Frame::new().inner_margin(Margin { left: 14, ..Default::default() }).show(ui, |ui| {
                let mut sp = spans(x);
                sp.iter_mut().for_each(|p| p.italic = true);
                ui.label(job(&sp, style, size, false, MUTED, width - 30.0));
            });
            ui.painter().vline(r.response.rect.left() + 3.0, r.response.rect.y_range(), Stroke::new(3.0, ACCENT.gamma_multiply(0.6)));
        } else {
            ui.label(job(&spans(t), style, size, false, TEXT, width));
        }
    }
    if let Some(lines) = code_block {
        ui.label(RichText::new(lines.join("\n")).monospace().color(MUTED));
    }
}

/// Titles of `[[links]]` in a page.
pub fn links(src: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = src;
    while let Some(i) = rest.find("[[") {
        let after = &rest[i + 2..];
        match after.find("]]") {
            Some(j) => {
                let t = after[..j].trim();
                if !t.is_empty() && !t.contains('\n') {
                    out.push(t.to_string());
                }
                rest = &after[j + 2..];
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        spans(s).iter().map(|p| p.text.as_str()).collect()
    }

    #[test]
    fn finds_links() {
        assert_eq!(links("a [[One]] b [[Two words]] [[ ]] [[open"), vec!["One", "Two words"]);
    }

    #[test]
    fn styles_are_detected() {
        let sp = spans("a **b** *c* ~~d~~ `e` ==f== <u>g</u> [[h]]");
        let find = |t: &str| sp.iter().find(|s| s.text == t).unwrap().clone();
        assert!(find("b").bold && !find("b").italic);
        assert!(find("c").italic);
        assert!(find("d").strike);
        assert!(find("e").code);
        assert!(find("f").mark);
        assert!(find("g").underline);
        assert!(find("h").link);
        assert_eq!(plain("a **b** *c*"), "a b c");
    }

    #[test]
    fn heading_outline() {
        let src = "# Title\ntext\n```\n# not a heading\n```\n## **Bold** part\n#### too deep\n### Third";
        let h = headings(src);
        assert_eq!(h.iter().map(|x| (x.level, x.text.as_str())).collect::<Vec<_>>(), vec![(1, "Title"), (2, "Bold part"), (3, "Third")]);
        assert_eq!(h[0].char_idx, 0);
        assert_eq!(&src.chars().skip(h[2].char_idx).take(5).collect::<String>(), "### T");
    }

    #[test]
    fn star_runs_and_bold_italic() {
        assert_eq!(plain("**bold** and *it*"), "bold and it");
        let sp = spans("*a* **b**");
        assert!(sp[0].italic && !sp[0].bold);
        assert!(sp.last().unwrap().bold);
    }

    #[test]
    fn nested_styles_combine() {
        let sp = spans("***both***");
        assert!(sp.iter().any(|s| s.text == "both" && s.bold && s.italic));
    }

    #[test]
    fn stray_markers_stay_literal() {
        assert_eq!(plain("2 * 3 = 6"), "2 * 3 = 6");
        assert_eq!(plain("**unclosed"), "**unclosed");
        assert_eq!(plain("a ~~b"), "a ~~b");
    }

    #[test]
    fn code_is_not_styled_inside() {
        let sp = spans("`a **b** c`");
        assert_eq!(sp.len(), 1);
        assert!(sp[0].code && !sp[0].bold);
        assert_eq!(sp[0].text, "a **b** c");
    }

    #[test]
    fn multibyte_is_safe() {
        assert_eq!(plain("£5 **café** ✓"), "£5 café ✓");
    }
}
