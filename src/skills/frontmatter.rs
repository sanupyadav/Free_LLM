//! SKILL.md frontmatter parsing and generation.
//!
//! Uses a minimal YAML-like style (single-line key-value pairs only, no YAML dependency):
//!
//! ```text
//! ---
//! name: Example Skill
//! description: One-line description
//! version: 0.1
//! triggers: git, rebase
//! ---
//! Body...
//! ```

/// Skill metadata (SKILL.md frontmatter)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frontmatter {
    pub name: String,
    pub description: String,
    pub version: String,
    pub triggers: Vec<String>,
}

/// Parse SKILL.md, returning `(metadata, body)`.
///
/// Without frontmatter, heuristically fills name/description from the body's first lines; the body is returned as-is.
pub fn parse(content: &str) -> (Frontmatter, String) {
    let trimmed = content.strip_prefix('\u{feff}').unwrap_or(content);
    if let Some(rest) = strip_open(trimmed) {
        if let Some((head, tail)) = split_close(rest) {
            // The first newline after the separator line is not part of the body
            let body = tail.strip_prefix('\n').unwrap_or(tail);
            return (parse_fields(head), body.to_string());
        }
    }
    heuristic(trimmed)
}

/// Generate the full SKILL.md text. Satisfies `parse(compose(fm, body)) == (fm, body)` (the body is kept as-is).
pub fn compose(fm: &Frontmatter, body: &str) -> String {
    let triggers = fm
        .triggers
        .iter()
        .map(|t| sanitize(t))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {}\n", sanitize(&fm.name)));
    out.push_str(&format!("description: {}\n", sanitize(&fm.description)));
    out.push_str(&format!("version: {}\n", sanitize(&fm.version)));
    out.push_str(&format!("triggers: {triggers}\n"));
    out.push_str("---\n");
    out.push_str(body);
    out
}

/// Strip the leading `---` separator line (handles CRLF and BOM).
fn strip_open(s: &str) -> Option<&str> {
    s.strip_prefix("---\r\n")
        .or_else(|| s.strip_prefix("---\n"))
}

/// Find the closing `---` line, returning (header field block, remaining content after the separator line).
fn split_close(rest: &str) -> Option<(&str, &str)> {
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']).trim_end() == "---" {
            let head = &rest[..offset];
            let tail = &rest[offset + line.len()..];
            return Some((head, tail));
        }
        offset += line.len();
    }
    None
}

/// Parse `key: value` lines; unknown keys are ignored, version defaults to 0.1.
fn parse_fields(head: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut has_version = false;
    for line in head.lines() {
        let Some((raw_key, raw_val)) = line.split_once(':') else {
            continue;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        let val = raw_val
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_string();
        match key.as_str() {
            "name" => fm.name = val,
            "description" => fm.description = val,
            "version" => {
                has_version = true;
                fm.version = val;
            }
            "triggers" => fm.triggers = split_triggers(&val),
            _ => {}
        }
    }
    if !has_version {
        fm.version = "0.1".to_string();
    }
    fm
}

fn split_triggers(val: &str) -> Vec<String> {
    val.trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Heuristic for when there's no frontmatter: the first non-empty line becomes the name, the second line the description (falls back to the name if absent).
fn heuristic(content: &str) -> (Frontmatter, String) {
    let mut lines = content.lines().map(str::trim).filter(|l| !l.is_empty());
    let name = clean_heading(lines.next().unwrap_or(""));
    let description = lines
        .next()
        .map(clean_heading)
        .unwrap_or_else(|| name.clone());
    let fm = Frontmatter {
        name,
        description,
        version: "0.1".to_string(),
        triggers: Vec::new(),
    };
    (fm, content.to_string())
}

/// Strip Markdown heading markers
fn clean_heading(line: &str) -> String {
    line.trim_start_matches('#').trim().to_string()
}

/// Frontmatter values must be a single line
fn sanitize(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_then_parse_round_trip() {
        let fm = Frontmatter {
            name: "Git Expert".to_string(),
            description: "Handles branches, rebase, and conflicts".to_string(),
            version: "0.2".to_string(),
            triggers: vec!["git".to_string(), "rebase".to_string()],
        };
        let body = "First line\n\n```rust\nfn main() {}\n```\n";
        let text = compose(&fm, body);
        let (parsed_fm, parsed_body) = parse(&text);
        assert_eq!(parsed_fm, fm);
        assert_eq!(parsed_body, body);
    }

    #[test]
    fn round_trip_with_default_frontmatter() {
        let fm = Frontmatter::default();
        let (parsed_fm, parsed_body) = parse(&compose(&fm, "Body"));
        assert_eq!(parsed_fm, fm);
        assert_eq!(parsed_body, "Body");
    }

    #[test]
    fn parses_crlf_and_optional_keys() {
        let text = "---\r\nname: x\r\nversion: 1.0\r\n---\r\nbody";
        let (fm, body) = parse(text);
        assert_eq!(fm.name, "x");
        assert_eq!(fm.version, "1.0");
        assert_eq!(fm.description, "");
        assert_eq!(fm.triggers, Vec::<String>::new());
        assert_eq!(body, "body");
    }

    #[test]
    fn without_frontmatter_uses_first_lines() {
        let content = "# My Skill\n\nThis is the description line\nBody content";
        let (fm, body) = parse(content);
        assert_eq!(fm.name, "My Skill");
        assert_eq!(fm.description, "This is the description line");
        assert_eq!(fm.version, "0.1");
        assert_eq!(body, content);
    }

    #[test]
    fn single_line_content_falls_back_to_name_as_description() {
        let (fm, body) = parse("Only one line");
        assert_eq!(fm.name, "Only one line");
        assert_eq!(fm.description, "Only one line");
        assert_eq!(body, "Only one line");
    }
}
