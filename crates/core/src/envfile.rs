//! Parse the text of a `.env` file: `KEY=value` lines pasted or uploaded by a person.

use crate::validate_env_key;

/// Most variables one import accepts.
pub const MAX_IMPORT_ENTRIES: usize = 500;
/// Longest value of one variable, in bytes.
pub const MAX_ENV_VALUE_BYTES: usize = 16_384;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvParseError {
    /// 1-based line the problem starts on.
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvParse {
    /// Entries in file order. A key that appears twice keeps its last value, at its first position.
    pub entries: Vec<EnvEntry>,
    pub errors: Vec<EnvParseError>,
}

/// Parse `.env` text.
///
/// Accepted: `KEY=value`, `export KEY=value`, blank lines, `#` comments, a trailing
/// ` # comment` after an unquoted value, `'single'` quotes (literal), `"double"`
/// quotes (`\n`, `\r`, `\t`, `\"`, `\\` are unescaped) and quoted values that continue
/// over several lines. Problems are reported per line and do not stop the parse.
pub fn parse_env_text(text: &str) -> EnvParse {
    let mut parse = EnvParse::default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let number = index + 1;
        let line = lines[index].trim();
        index += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export")
            .filter(|rest| rest.starts_with([' ', '\t']))
            .map_or(line, str::trim_start);
        let Some((key, raw)) = line.split_once('=') else {
            parse.errors.push(EnvParseError {
                line: number,
                message: "expected KEY=value".into(),
            });
            continue;
        };
        let key = key.trim();
        if let Err(error) = validate_env_key(key) {
            parse.errors.push(EnvParseError {
                line: number,
                message: error.to_string(),
            });
            continue;
        }
        let raw = raw.trim_start();
        let value = match raw.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let mut body = raw[1..].to_owned();
                let closed = loop {
                    if let Some(end) = closing_quote(&body, quote) {
                        body.truncate(end);
                        break true;
                    }
                    if index >= lines.len() {
                        break false;
                    }
                    body.push('\n');
                    body.push_str(lines[index]);
                    index += 1;
                };
                if !closed {
                    parse.errors.push(EnvParseError {
                        line: number,
                        message: format!("the {quote} quote is never closed"),
                    });
                    continue;
                }
                if quote == '"' {
                    unescape(&body)
                } else {
                    body
                }
            }
            _ => strip_comment(raw).trim_end().to_owned(),
        };
        if value.len() > MAX_ENV_VALUE_BYTES || value.contains('\0') {
            parse.errors.push(EnvParseError {
                line: number,
                message: format!("{key}: a value must be at most 16 KiB and cannot contain NUL"),
            });
            continue;
        }
        match parse.entries.iter_mut().find(|entry| entry.key == key) {
            Some(existing) => existing.value = value,
            None => parse.entries.push(EnvEntry {
                key: key.to_owned(),
                value,
            }),
        }
    }
    if parse.entries.len() > MAX_IMPORT_ENTRIES {
        parse.errors.push(EnvParseError {
            line: 1,
            message: format!("at most {MAX_IMPORT_ENTRIES} variables can be imported at once"),
        });
    }
    parse
}

/// Byte offset of the first unescaped `quote` in `body`, where `body` starts after the opening quote.
fn closing_quote(body: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (offset, ch) in body.char_indices() {
        if quote == '"' && escaped {
            escaped = false;
        } else if quote == '"' && ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return Some(offset);
        }
    }
    None
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other @ ('"' | '\\' | '$')) => out.push(other),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// An unquoted value ends at a ` #` (a `#` after whitespace).
fn strip_comment(raw: &str) -> &str {
    let mut previous_space = false;
    for (offset, ch) in raw.char_indices() {
        if ch == '#' && previous_space {
            return &raw[..offset];
        }
        previous_space = ch.is_whitespace();
    }
    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(parse: &EnvParse) -> Vec<(&str, &str)> {
        parse
            .entries
            .iter()
            .map(|entry| (entry.key.as_str(), entry.value.as_str()))
            .collect()
    }

    #[test]
    fn reads_the_usual_forms() {
        let parse = parse_env_text(
            "# comment\n\nA=1\nexport B = two words # trailing\nC='it\"s $HOME'\nD=\"line\\nbreak \\\"q\\\"\"\nE=\nF=a#b\r\nG=\"multi\nline\"\n",
        );
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert_eq!(
            pairs(&parse),
            vec![
                ("A", "1"),
                ("B", "two words"),
                ("C", "it\"s $HOME"),
                ("D", "line\nbreak \"q\""),
                ("E", ""),
                ("F", "a#b"),
                ("G", "multi\nline"),
            ]
        );
    }

    #[test]
    fn a_repeated_key_keeps_the_last_value_in_its_first_place() {
        let parse = parse_env_text("A=1\nB=2\nA=3\n");
        assert_eq!(pairs(&parse), vec![("A", "3"), ("B", "2")]);
    }

    #[test]
    fn bad_lines_are_reported_without_stopping() {
        let parse = parse_env_text("GOOD=1\nnot a pair\n1BAD=x\nOPEN=\"never closed\nNEXT=2\n");
        assert_eq!(pairs(&parse), vec![("GOOD", "1")]);
        let lines: Vec<usize> = parse.errors.iter().map(|error| error.line).collect();
        assert_eq!(lines, vec![2, 3, 4]);
        assert!(parse.errors[2].message.contains("never closed"));
    }

    #[test]
    fn limits_are_enforced() {
        let big = format!("A={}\n", "x".repeat(MAX_ENV_VALUE_BYTES + 1));
        assert_eq!(parse_env_text(&big).errors.len(), 1);
        let many: String = (0..=MAX_IMPORT_ENTRIES)
            .map(|n| format!("K{n}=1\n"))
            .collect();
        assert!(!parse_env_text(&many).errors.is_empty());
        assert_eq!(parse_env_text("\u{feff}A=1").entries.len(), 1);
    }
}
