//! `filter` blocks: a click on one plot narrows the queries of the others.
//!
//! ```text
//! filter "host" {
//!   plot   = plot.hosts      // a click on a bar, a table row or a graph node
//!   column = name            // picks the value of this column (or property)
//!   match  = "h.name"        // what a Cypher query compares it with
//! }
//! query "blast" {
//!   cypher = "MATCH (h:Host)-[r]->(s) WHERE $host RETURN h, r, s"
//! }
//! ```
//!
//! `$host` is a predicate. In SQL it is `("name" = 'web-1')` while a value
//! is picked and `TRUE` while none is, as in DuckLocal. HawDB's Cypher has no
//! boolean literal, so in Cypher a picked filter is `(h.name = 'web-1')` and
//! an unpicked one is taken out of its `WHERE` together with the `AND` that
//! joins it — and the `WHERE` itself when it was the only condition. For that
//! to stay well formed a Cypher placeholder must be one whole condition of a
//! `WHERE`, joined by `AND` (`cypher_placement_problems` says where not).
//! Either way a dashboard with nothing picked runs exactly the statement it
//! would without the filter.
//!
//! The picking plot's own queries see nothing picked: the filter narrows
//! everything else, never the plot it is picked on — otherwise one click
//! would collapse the bars to the one just clicked, and there would be
//! nothing left to click next. This is the cross-filter of Mosaic and
//! crossfilter.js.
//!
//! A picked number goes in as a number; anything else as a string literal,
//! quotes doubled (and backslashes doubled for Cypher), so nothing the data
//! holds can end the literal early and a read-only query stays read-only.
//!
//! Finding `$name` skips string literals, quoted identifiers, comments and
//! dollar-quoted strings, so a `'$5'` in the data or a `$$ … $$` body is never
//! mistaken for one.

use std::collections::HashMap;
use std::ops::Range;

use khonsu_core::Language;
use serde_json::Value as Json;

/// One `$name` in a query's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    pub name: String,
    /// Byte range of the whole `$name`, dollar included.
    pub range: Range<usize>,
}

/// A value picked on a plot, for the queries that name its filter.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    /// The column (or node property) picked, as named on the filter. SQL
    /// compares it as a quoted identifier.
    pub column: String,
    /// The Cypher expression compared, from the filter's `match`.
    pub expr: Option<String>,
    /// The picked value; `Json::Null` for a cell holding NULL.
    pub value: Json,
}

fn literal(value: &Json, language: Language) -> String {
    match value {
        Json::Number(n) => n.to_string(),
        Json::Bool(b) if language == Language::Cypher => b.to_string(),
        Json::String(s) => quote(s, language),
        other => quote(&other.to_string(), language),
    }
}

fn quote(text: &str, language: Language) -> String {
    match language {
        Language::Sql => format!("'{}'", text.replace('\'', "''")),
        Language::Cypher => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
    }
}

impl Pick {
    /// The predicate that stands in for the placeholder.
    pub fn predicate(&self, language: Language) -> String {
        let target = match language {
            Language::Sql => format!("\"{}\"", self.column.replace('"', "\"\"")),
            Language::Cypher => self.expr.clone().unwrap_or_else(|| self.column.clone()),
        };
        match &self.value {
            Json::Null => format!("({target} IS NULL)"),
            value => format!("({target} = {})", literal(value, language)),
        }
    }
}
/// Every `$name` outside strings, quoted identifiers and comments, in order.
/// `$1`-style positional parameters are not names and are left alone.
pub fn placeholders(sql: &str) -> Vec<Placeholder> {
    let bytes = sql.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"') => {
                // A doubled quote inside is an escaped one; either way the
                // scan resumes after the closing quote.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if bytes.get(i + 1) == Some(&quote) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = sql[i..].find('\n').map_or(bytes.len(), |end| i + end + 1);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| i + 2 + end + 2);
            }
            b'$' => {
                let start = i;
                let mut end = i + 1;
                while end < bytes.len() && ident(bytes[end]) {
                    end += 1;
                }
                let tag = &sql[start..end];
                if bytes.get(end) == Some(&b'$') {
                    // `$$ … $$` or `$tag$ … $tag$`: a string; skip past its end.
                    let close = format!("{tag}$");
                    i = sql[end + 1..]
                        .find(&close)
                        .map_or(bytes.len(), |at| end + 1 + at + close.len());
                } else if end > start + 1 && !bytes[start + 1].is_ascii_digit() {
                    found.push(Placeholder {
                        name: sql[start + 1..end].to_string(),
                        range: start..end,
                    });
                    i = end;
                } else {
                    i = end.max(start + 1);
                }
            }
            _ => i += 1,
        }
    }
    found
}

/// `text` with each `$name` replaced by the picked value's predicate when
/// `picks` has one for that name. An unpicked one is taken out with the
/// `AND` that joins it, and a `WHERE` left with no condition goes too: HawDB
/// accepts no `TRUE` in a predicate, in Cypher or in SQL.
pub fn apply(text: &str, language: Language, picks: &HashMap<String, Pick>) -> String {
    // Work from the last placeholder back, so earlier ranges stay valid.
    let mut out = text.to_string();
    for placeholder in placeholders(text).into_iter().rev() {
        let range = placeholder.range.clone();
        if let Some(pick) = picks.get(&placeholder.name) {
            out.replace_range(range, &pick.predicate(language));
            continue;
        }
        // Drop `AND $x` when something precedes it, else `$x AND`, else the
        // whole `WHERE $x`.
        let (start, end) = match (word_before(&out, range.start), word_after(&out, range.end)) {
            (Some((start, word)), _) if word == "AND" => (start, range.end),
            (_, Some((end, word))) if word == "AND" => (range.start, end),
            (Some((start, word)), _) if word == "WHERE" => (start, range.end),
            _ => (range.start, range.end),
        };
        cut(&mut out, start, end);
    }
    out
}

/// Removes `start..end` and the whitespace around it, leaving one space
/// between what was on either side.
fn cut(out: &mut String, start: usize, end: usize) {
    let left = out[..start].trim_end().len();
    let right = end + (out[end..].len() - out[end..].trim_start().len());
    let joint = if left == 0 || right == out.len() { "" } else { " " };
    out.replace_range(left..right, joint);
}

/// The statement as it runs with nothing picked: what `check` judges.
pub fn neutral(text: &str, language: Language) -> String {
    apply(text, language, &HashMap::new())
}

/// Words that end a `WHERE` clause.
fn clause_words(language: Language) -> &'static [&'static str] {
    match language {
        Language::Cypher => &[
            "RETURN", "WITH", "MATCH", "OPTIONAL", "UNWIND", "ORDER", "SKIP", "LIMIT", "UNION", "CALL",
        ],
        Language::Sql => &[
            "GROUP", "ORDER", "LIMIT", "OFFSET", "HAVING", "UNION", "EXCEPT", "INTERSECT", "WINDOW", "FETCH",
            "RETURNING",
        ],
    }
}

/// The word that ends just before `at` (skipping whitespace), and where it starts.
fn word_before(text: &str, at: usize) -> Option<(usize, String)> {
    let head = text[..at].trim_end();
    let start = head
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    let word = &head[start..];
    (!word.is_empty()).then(|| (start, word.to_ascii_uppercase()))
}

/// The word that starts just after `at` (skipping whitespace), and where it ends.
fn word_after(text: &str, at: usize) -> Option<(usize, String)> {
    let rest = &text[at..];
    let start = at + (rest.len() - rest.trim_start().len());
    let len = text[start..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len() - start);
    (len > 0).then(|| (start + len, text[start..start + len].to_ascii_uppercase()))
}

/// Where a placeholder cannot be taken out cleanly: it must stand as one
/// condition of a `WHERE`, joined to the others by `AND`.
pub fn placement_problems(text: &str, language: Language) -> Vec<String> {
    let mut problems = Vec::new();
    for placeholder in placeholders(text) {
        let before = word_before(text, placeholder.range.start).map(|(_, w)| w);
        let after = word_after(text, placeholder.range.end).map(|(_, w)| w);
        let before_ok = matches!(before.as_deref(), Some("WHERE" | "AND"));
        let after_ok = match after.as_deref() {
            None => {
                let rest = text[placeholder.range.end..].trim_start();
                rest.is_empty() || rest.starts_with([')', ';'])
            }
            Some(word) => word == "AND" || clause_words(language).contains(&word),
        };
        if !(before_ok && after_ok) {
            let name = &placeholder.name;
            problems.push(format!(
                "${name} must be a whole condition of a WHERE, joined by AND (WHERE ${name} AND x > 1): while nothing is picked it is taken out, since HawDB has no TRUE to stand in"
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(sql: &str) -> Vec<String> {
        placeholders(sql).into_iter().map(|p| p.name).collect()
    }

    #[test]
    fn placeholders_are_found_outside_strings_and_comments() {
        assert_eq!(
            names("SELECT * FROM t WHERE $channel AND ($region OR x = 1)"),
            ["channel", "region"]
        );
        assert_eq!(
            names("SELECT '$not', \"$nor\", $$ $body $$, $t$ $x $t$ -- $no\n/* $none */ FROM t WHERE $yes"),
            ["yes"]
        );
        assert_eq!(names("SELECT 'it''s $x' WHERE $ok"), ["ok"]);
        assert!(names("SELECT $1, $ FROM t").is_empty());
    }

    #[test]
    fn sql_nothing_picked_is_left_out_and_a_pick_is_a_comparison() {
        let sql = "FROM t WHERE $channel AND $day AND $n";
        assert_eq!(neutral(sql, Language::Sql), "FROM t");
        assert_eq!(
            neutral("SELECT a FROM t WHERE $x AND a > 1 GROUP BY a", Language::Sql),
            "SELECT a FROM t WHERE a > 1 GROUP BY a"
        );
        let mut picks = HashMap::new();
        picks.insert(
            "channel".to_string(),
            Pick { column: "Sales \"Channel\"".into(), expr: None, value: json!("it's") },
        );
        picks.insert("day".to_string(), Pick { column: "day".into(), expr: None, value: Json::Null });
        picks.insert("n".to_string(), Pick { column: "n".into(), expr: None, value: json!(3) });
        assert_eq!(
            apply(sql, Language::Sql, &picks),
            "FROM t WHERE (\"Sales \"\"Channel\"\"\" = 'it''s') AND (\"day\" IS NULL) AND (\"n\" = 3)"
        );
    }

    #[test]
    fn cypher_unpicked_conditions_are_taken_out_with_their_and() {
        let only = "MATCH (h:Host)-[r]->(s) WHERE $host RETURN h, r, s";
        assert_eq!(neutral(only, Language::Cypher), "MATCH (h:Host)-[r]->(s) RETURN h, r, s");
        let first = "MATCH (h:Host) WHERE $host AND h.cpu > 4 RETURN h";
        assert_eq!(neutral(first, Language::Cypher), "MATCH (h:Host) WHERE h.cpu > 4 RETURN h");
        let last = "MATCH (h:Host) WHERE h.cpu > 4 AND $host RETURN h";
        assert_eq!(neutral(last, Language::Cypher), "MATCH (h:Host) WHERE h.cpu > 4 RETURN h");
        let both = "MATCH (h:Host) WHERE $a AND $b RETURN h";
        assert_eq!(neutral(both, Language::Cypher), "MATCH (h:Host) RETURN h");
    }

    #[test]
    fn cypher_picks_compare_the_match_expression() {
        let mut picks = HashMap::new();
        picks.insert(
            "host".to_string(),
            Pick { column: "name".into(), expr: Some("h.name".into()), value: json!("o'brien\\x") },
        );
        assert_eq!(
            apply("MATCH (h:Host) WHERE $host AND $other RETURN h", Language::Cypher, &picks),
            "MATCH (h:Host) WHERE (h.name = 'o\\'brien\\\\x') RETURN h"
        );
    }

    #[test]
    fn placement_must_be_a_whole_condition() {
        assert!(placement_problems("MATCH (n) WHERE $a AND n.x = 1 RETURN n", Language::Cypher).is_empty());
        assert!(placement_problems("MATCH (n) WHERE n.x = 1 AND $a", Language::Cypher).is_empty());
        assert_eq!(placement_problems("MATCH (n) WHERE $a OR n.x = 1 RETURN n", Language::Cypher).len(), 1);
        assert_eq!(placement_problems("MATCH (n) RETURN n, $a", Language::Cypher).len(), 1);
    }

    #[test]
    fn an_unterminated_string_ends_the_scan_without_panicking() {
        assert!(names("SELECT 'open $x").is_empty());
        assert!(names("SELECT /* open $x").is_empty());
        assert!(names("SELECT $$ open $x").is_empty());
        assert_eq!(names("SELECT $x"), ["x"]);
    }
}
