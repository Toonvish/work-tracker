//! MR descriptions for the detail pane: GitLab markdown split into text and images, and the
//! text rendered to styled lines. Pure and unit-tested.

use std::sync::LazyLock;

use ratatui::text::{Line, Span};
use regex::Regex;

/// Where an image of a description comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImageRef {
    /// A file uploaded to the MR's project: `/uploads/<secret>/<filename>`.
    Upload { secret: String, filename: String },
    /// Anything else. Not downloaded; shown as its address.
    External(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// Markdown without images.
    Text(String),
    Image {
        alt: String,
        source: ImageRef,
    },
}

/// `![alt](target "title"){attributes}` or `<img ... src="target" ...>`.
static IMAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"!\[(?P<alt>[^\]]*)\]\(\s*<?(?P<md>[^)\s>]+)>?[^)]*\)(?:\{[^}\n]*\})?|<img\b[^>]*?\bsrc\s*=\s*["'](?P<html>[^"']+)["'][^>]*>"#,
    )
    .expect("valid regex")
});

/// The `/uploads/<32 hex>/<filename>` tail of a target, relative or absolute.
static UPLOAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|/)uploads/(?P<secret>[0-9a-f]{32})/(?P<file>[^/?#]+)$").expect("valid regex")
});

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| s.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn image_ref(target: &str) -> ImageRef {
    match UPLOAD.captures(target) {
        Some(c) => ImageRef::Upload {
            secret: c["secret"].to_string(),
            filename: percent_decode(&c["file"]),
        },
        None => ImageRef::External(target.to_string()),
    }
}

/// Control characters other than newlines would reach the terminal as they are.
fn clean(s: &str) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .map(|c| match c {
            '\n' => '\n',
            '\t' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

/// Splits a description into markdown text and the images between it, in order.
pub fn split(description: &str) -> Vec<Part> {
    fn push_text(parts: &mut Vec<Part>, text: &str) {
        if !text.trim().is_empty() {
            parts.push(Part::Text(text.trim_matches('\n').to_string()));
        }
    }

    let text = clean(description);
    let mut parts = Vec::new();
    let mut at = 0;
    for c in IMAGE.captures_iter(&text) {
        let whole = c.get(0).expect("group 0 always exists");
        push_text(&mut parts, &text[at..whole.start()]);
        at = whole.end();
        let target = c
            .name("md")
            .or_else(|| c.name("html"))
            .map_or("", |m| m.as_str());
        parts.push(Part::Image {
            alt: c.name("alt").map_or("", |m| m.as_str()).trim().to_string(),
            source: image_ref(target),
        });
    }
    push_text(&mut parts, &text[at..]);
    parts
}

/// Renders markdown (without images) to styled lines. Tables are laid out for `width`.
pub fn render(markdown: &str, width: u16) -> Vec<Line<'static>> {
    let options = tui_markdown::Options::default().table_width(width.max(20));
    let markdown = clean(markdown);
    tui_markdown::from_str_with_options(&markdown, &options)
        .lines
        .into_iter()
        .map(|line| {
            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|s| Span::styled(s.content.into_owned(), s.style))
                .collect();
            let mut owned = Line::from(spans).style(line.style);
            owned.alignment = line.alignment;
            owned
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(secret: &str, filename: &str) -> ImageRef {
        ImageRef::Upload {
            secret: secret.into(),
            filename: filename.into(),
        }
    }

    const S1: &str = "b2259262421e0480821f75988977bd75";
    const S2: &str = "7ad0cfc8714ea185d347001db6d4d377";

    #[test]
    fn splits_text_and_images_in_order() {
        let md = format!(
            "## Shots\n\nBefore:\n\n![before](/uploads/{S1}/before.png)\n\nAfter:\n\n![after shot](/uploads/{S2}/after%20shot.png){{width=600}}\n\nDone."
        );
        assert_eq!(
            split(&md),
            vec![
                Part::Text("## Shots\n\nBefore:".into()),
                Part::Image {
                    alt: "before".into(),
                    source: upload(S1, "before.png")
                },
                Part::Text("After:".into()),
                Part::Image {
                    alt: "after shot".into(),
                    source: upload(S2, "after shot.png")
                },
                Part::Text("Done.".into()),
            ]
        );
    }

    #[test]
    fn html_images_absolute_uploads_and_external_images() {
        let md = format!(
            "<img src=\"/uploads/{S1}/a.png\" width=\"300\">\n![x](https://gl.example.com/g/p/uploads/{S2}/b.png \"title\")\n![logo](https://example.com/logo.svg)"
        );
        let images: Vec<ImageRef> = split(&md)
            .into_iter()
            .filter_map(|p| match p {
                Part::Image { source, .. } => Some(source),
                Part::Text(_) => None,
            })
            .collect();
        assert_eq!(
            images,
            vec![
                upload(S1, "a.png"),
                upload(S2, "b.png"),
                ImageRef::External("https://example.com/logo.svg".into()),
            ]
        );
    }

    #[test]
    fn no_images_and_empty() {
        assert_eq!(split("just *text*"), vec![Part::Text("just *text*".into())]);
        assert_eq!(split(""), vec![]);
        assert_eq!(split("  \n\n"), vec![]);
        // A link is not an image.
        assert_eq!(
            split("[doc](/uploads/x/doc.pdf)"),
            vec![Part::Text("[doc](/uploads/x/doc.pdf)".into())]
        );
    }

    #[test]
    fn control_characters_never_reach_the_terminal() {
        let parts = split("a\x1b[31mred\r\nb\tc\x07");
        assert_eq!(parts, vec![Part::Text("a [31mred\nb c ".into())]);
        for line in render("x\x1b[2Jy", 80) {
            for span in line.spans {
                assert!(!span.content.contains('\x1b'), "{:?}", span.content);
            }
        }
    }

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn renders_headings_lists_code_and_tables() {
        let md = "## What changed\n\n- one `code`\n- **two**\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let lines = render(md, 60);
        let text = plain(&lines).join("\n");
        assert!(text.contains("What changed"), "{text}");
        assert!(text.contains("one code"), "{text}");
        assert!(text.contains("two"), "{text}");
        assert!(!text.contains("**"), "{text}");
        assert!(text.contains('\u{2502}'), "table borders: {text}");
        // Emphasis is a style, not markup.
        assert!(
            lines
                .iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.content == "two"
                    && s.style
                        .add_modifier
                        .contains(ratatui::style::Modifier::BOLD)),
            "{lines:?}"
        );
    }
}
