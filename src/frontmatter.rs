//! Small, deliberately narrow YAML-frontmatter model for the note header.
//!
//! The three fields the UI owns are parsed as YAML-compatible scalar strings.
//! Unknown lines are preserved verbatim so switching through card view does not
//! discard tags, dates, or other metadata another tool added.

pub const DEFAULT_COVER: &str = "#cccc";

/// A cover value after classifying the frontmatter scalar.
///
/// Keeping remote images distinct gives the UI a stable hand-off point for a
/// future Kitty graphics renderer without changing the stored frontmatter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverSource<'a> {
    Color(u8, u8, u8),
    Url(&'a str),
    Unsupported(&'a str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub cover: String,
    pub title: String,
    pub description: String,
    pub extra: Vec<String>,
}

impl Metadata {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            cover: DEFAULT_COVER.to_string(),
            title: title.into(),
            description: String::new(),
            extra: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub metadata: Metadata,
    pub body: Vec<String>,
    /// Row where the body begins in the source representation.
    pub body_start: usize,
}

/// Open any Markdown document in card view. A document without frontmatter
/// receives in-memory defaults; they are written only when the user saves.
pub fn parse(text: &str, fallback_title: &str) -> Document {
    let lines = text_lines(text);
    parse_source_lines(&lines, fallback_title).unwrap_or_else(|| Document {
        metadata: Metadata::new(fallback_title),
        body: nonempty(lines),
        body_start: 0,
    })
}

/// Parse only a literal frontmatter source representation. This is used when
/// returning from source view, where a missing closing fence should keep the
/// user in source view rather than silently reinterpret the document.
pub fn parse_source_lines(lines: &[String], fallback_title: &str) -> Option<Document> {
    if lines.first()?.trim() != "---" {
        return None;
    }
    let closing = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(row, line)| (line.trim() == "---").then_some(row))?;

    let mut cover = None;
    let mut title = None;
    let mut description = None;
    let mut extra = Vec::new();
    for line in &lines[1..closing] {
        let Some((key, value)) = line.split_once(':') else {
            extra.push(line.clone());
            continue;
        };
        match key.trim() {
            "cover" => cover = Some(decode_scalar(value)),
            "title" => title = Some(decode_scalar(value)),
            "description" => description = Some(decode_scalar(value)),
            _ => extra.push(line.clone()),
        }
    }

    let body_start = closing + 1;
    Some(Document {
        metadata: Metadata {
            cover: cover.unwrap_or_else(|| DEFAULT_COVER.to_string()),
            title: title.unwrap_or_else(|| fallback_title.to_string()),
            description: description.unwrap_or_default(),
            extra,
        },
        body: nonempty(lines[body_start..].to_vec()),
        body_start,
    })
}

/// Canonical source form. JSON strings are valid YAML strings and safely keep
/// `#`, colons, quotes, and Unicode intact without bringing in a full YAML
/// dependency for three scalar fields.
pub fn compose_lines(metadata: &Metadata, body: &[String]) -> Vec<String> {
    let mut lines = Vec::with_capacity(5 + metadata.extra.len() + body.len());
    lines.push("---".to_string());
    lines.push(format!("cover: {}", encode_scalar(&metadata.cover)));
    lines.push(format!("title: {}", encode_scalar(&metadata.title)));
    lines.push(format!(
        "description: {}",
        encode_scalar(&metadata.description)
    ));
    lines.extend(metadata.extra.iter().cloned());
    lines.push("---".to_string());
    lines.extend(nonempty(body.to_vec()));
    lines
}

pub fn body_start(metadata: &Metadata) -> usize {
    5 + metadata.extra.len()
}

/// CSS-like cover colors: #rgb, #rgba, #rrggbb, and #rrggbbaa. Alpha is
/// ignored because terminal cells have no alpha channel.
pub fn cover_rgb(value: &str) -> Option<(u8, u8, u8)> {
    let hex = value.trim().strip_prefix('#')?;
    if !hex.is_ascii() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        3 | 4 => {
            let mut digits = hex.chars();
            let r = expand_hex(digits.next()?)?;
            let g = expand_hex(digits.next()?)?;
            let b = expand_hex(digits.next()?)?;
            Some((r, g, b))
        }
        6 | 8 => Some((
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        )),
        _ => None,
    }
}

/// Recognize the cover formats understood by the card renderer. HTTP(S) URLs
/// are preserved as-is; the terminal UI currently draws a placeholder for
/// them and can later paint the same cover rectangle with Kitty graphics.
pub fn cover_source(value: &str) -> CoverSource<'_> {
    let value = value.trim();
    if let Some((r, g, b)) = cover_rgb(value) {
        return CoverSource::Color(r, g, b);
    }
    if is_http_url(value) {
        CoverSource::Url(value)
    } else {
        CoverSource::Unsupported(value)
    }
}

fn is_http_url(value: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        value
            .get(..scheme.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
            && value
                .get(scheme.len()..)
                .is_some_and(|remainder| !remainder.trim().is_empty())
    })
}

fn expand_hex(digit: char) -> Option<u8> {
    let value = digit.to_digit(16)? as u8;
    Some(value * 17)
}

fn text_lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

fn nonempty(mut lines: Vec<String>) -> Vec<String> {
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn decode_scalar(value: &str) -> String {
    let value = value.trim();
    if let Ok(decoded) = serde_json::from_str::<String>(value) {
        return decoded;
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].replace("''", "'");
    }
    value.to_string()
}

fn encode_scalar(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_markdown_gets_defaults_without_losing_body() {
        let document = parse("# Existing title\n\nBody\n", "note");
        assert_eq!(document.metadata, Metadata::new("note"));
        assert_eq!(document.body, ["# Existing title", "", "Body"]);
        assert_eq!(document.body_start, 0);
    }

    #[test]
    fn known_fields_round_trip_and_unknown_fields_survive() {
        let source = [
            "---".to_string(),
            "cover:#abc".to_string(),
            "title: 'A title: with punctuation'".to_string(),
            "description: \"A short description\"".to_string(),
            "tags: [one, two]".to_string(),
            "---".to_string(),
            "Body".to_string(),
        ];
        let document = parse_source_lines(&source, "fallback").unwrap();
        assert_eq!(document.metadata.cover, "#abc");
        assert_eq!(document.metadata.title, "A title: with punctuation");
        assert_eq!(document.metadata.description, "A short description");
        assert_eq!(document.metadata.extra, ["tags: [one, two]"]);
        assert_eq!(document.body, ["Body"]);

        let round_trip = compose_lines(&document.metadata, &document.body);
        let reparsed = parse_source_lines(&round_trip, "fallback").unwrap();
        assert_eq!(reparsed.metadata, document.metadata);
        assert_eq!(reparsed.body, document.body);
    }

    #[test]
    fn cover_colors_accept_css_short_and_long_forms() {
        assert_eq!(cover_rgb("#cccc"), Some((204, 204, 204)));
        assert_eq!(cover_rgb("#123"), Some((17, 34, 51)));
        assert_eq!(cover_rgb("#12ab34"), Some((18, 171, 52)));
        assert_eq!(cover_rgb("not-a-color"), None);
        assert_eq!(cover_rgb("#aébcd"), None);
    }

    #[test]
    fn cover_sources_distinguish_colors_urls_and_unknown_values() {
        assert_eq!(cover_source(" #abc "), CoverSource::Color(170, 187, 204));
        assert_eq!(
            cover_source("https://example.com/cover image.jpg"),
            CoverSource::Url("https://example.com/cover image.jpg")
        );
        assert_eq!(
            cover_source("HTTP://example.com/cover.png"),
            CoverSource::Url("HTTP://example.com/cover.png")
        );
        assert_eq!(
            cover_source("https://"),
            CoverSource::Unsupported("https://")
        );
        assert_eq!(
            cover_source("https://   "),
            CoverSource::Unsupported("https://")
        );
        assert_eq!(cover_source("blue"), CoverSource::Unsupported("blue"));
    }
}
