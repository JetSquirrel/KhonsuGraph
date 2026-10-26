//! The column order a Cypher query asked for.
//!
//! HawDB returns Cypher columns sorted by name (SQL keeps its SELECT order),
//! so `RETURN service, cpu` comes back as `cpu, service`. The final `RETURN`
//! clause says the order meant: each item is named by its alias, or by its
//! expression text when it has none, which is how HawDB names the column.

/// The column names of `text`'s final top-level `RETURN`, in order, or
/// `None` when there is no such clause. Strings, backquoted names, comments
/// and anything inside brackets are skipped, so a `RETURN` or comma there
/// does not count.
pub(crate) fn return_columns(text: &str) -> Option<Vec<String>> {
    let text = &without_comments(text);
    let tokens = top_level(text);
    let start = tokens.iter().rposition(|t| matches!(t, Token::Word(at, len) if keyword(text, *at, *len, "RETURN")))?;
    let Token::Word(at, len) = tokens[start] else { unreachable!() };
    let mut begin = at + len;
    let mut rest = &tokens[start + 1..];
    if let Some(&Token::Word(at, len)) = rest.first() {
        if keyword(text, at, len, "DISTINCT") {
            begin = at + len;
            rest = &rest[1..];
        }
    }
    let mut end = text.len();
    let mut commas = Vec::new();
    for token in rest {
        match *token {
            Token::Word(at, len) if ["ORDER", "SKIP", "LIMIT", "UNION"].iter().any(|k| keyword(text, at, len, k)) => {
                end = at;
                break;
            }
            Token::Comma(at) => commas.push(at),
            Token::Word(..) => {}
        }
    }
    let mut items = Vec::new();
    let mut from = begin;
    for comma in commas.into_iter().chain(std::iter::once(end)) {
        items.push(column_name(text, from, comma, &tokens)?);
        from = comma + 1;
    }
    Some(items)
}

/// One RETURN item's column: the alias after its last top-level `AS`, or
/// the expression itself.
fn column_name(text: &str, from: usize, to: usize, tokens: &[Token]) -> Option<String> {
    let alias = tokens.iter().rev().find_map(|t| match *t {
        Token::Word(at, len) if at >= from && at + len <= to && keyword(text, at, len, "AS") => Some(at + len),
        _ => None,
    });
    let name = match alias {
        Some(at) => text[at..to].trim().trim_matches('`'),
        None => text[from..to].trim(),
    };
    (!name.is_empty()).then(|| name.to_string())
}

/// Puts `columns` and each row in `wanted` order. Does nothing unless
/// `wanted` names every column exactly once (spaces ignored, as HawDB may
/// print an expression more tightly than it was written).
pub(crate) fn reorder<T>(columns: &mut Vec<String>, rows: &mut [Vec<T>], wanted: &[String]) {
    let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    if wanted.len() != columns.len() {
        return;
    }
    let have: Vec<String> = columns.iter().map(|c| squeeze(c)).collect();
    let mut order = Vec::with_capacity(wanted.len());
    for name in wanted {
        let name = squeeze(name);
        match have.iter().position(|c| *c == name) {
            Some(i) if !order.contains(&i) => order.push(i),
            _ => return,
        }
    }
    if order.iter().enumerate().all(|(i, &j)| i == j) {
        return;
    }
    *columns = order.iter().map(|&i| columns[i].clone()).collect();
    for row in rows {
        let mut cells: Vec<Option<T>> = row.drain(..).map(Some).collect();
        row.extend(order.iter().map(|&i| cells[i].take().expect("each column is taken once")));
    }
}

#[derive(Debug, Clone, Copy)]
enum Token {
    /// A bare word outside any bracket: start and length.
    Word(usize, usize),
    /// A comma outside any bracket.
    Comma(usize),
}

/// `text` with each `//` comment turned into spaces, so offsets still match.
fn without_comments(text: &str) -> String {
    let mut bytes = text.as_bytes().to_vec();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = past_quoted(&bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    bytes[i] = b' ';
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    // Whole comments were blanked, so no multi-byte character was split.
    String::from_utf8(bytes).expect("still UTF-8")
}

/// The offset just past the string or backquoted name opening at `at`.
fn past_quoted(bytes: &[u8], at: usize) -> usize {
    let quote = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() && bytes[i] != quote {
        i += if bytes[i] == b'\\' { 2 } else { 1 };
    }
    i + 1
}

/// The words and commas at bracket depth zero, outside strings.
fn top_level(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = past_quoted(bytes, i),
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b',' => {
                if depth == 0 {
                    tokens.push(Token::Comma(i));
                }
                i += 1;
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                // `n.name`: a property, never a keyword.
                let after_dot = start > 0 && bytes[start - 1] == b'.';
                if depth == 0 && !after_dot {
                    tokens.push(Token::Word(start, i - start));
                }
            }
            _ => i += 1,
        }
    }
    tokens
}

fn keyword(text: &str, at: usize, len: usize, word: &str) -> bool {
    text[at..at + len].eq_ignore_ascii_case(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(text: &str) -> Option<Vec<String>> {
        return_columns(text)
    }

    #[test]
    fn items_are_named_by_alias_or_expression() {
        assert_eq!(
            cols("MATCH (p:P) RETURN p.name, count(p), p.n AS zz, 'a,b' AS s ORDER BY p.name LIMIT 1").unwrap(),
            ["p.name", "count(p)", "zz", "s"]
        );
        assert_eq!(cols("MATCH (s) RETURN DISTINCT s.tier AS tier, s.name AS `name`").unwrap(), ["tier", "name"]);
        assert_eq!(cols("MATCH (n) RETURN n SKIP 2").unwrap(), ["n"]);
    }

    #[test]
    fn only_the_final_top_level_return_counts() {
        assert_eq!(
            cols("MATCH (a) WITH a, [x IN a.tags WHERE x <> 'RETURN' | x] AS t RETURN t, a // RETURN b, c").unwrap(),
            ["t", "a"]
        );
        assert_eq!(cols("MATCH (a) RETURN a.x AS x UNION MATCH (b) RETURN b.x AS x").unwrap(), ["x"]);
        assert_eq!(cols("MATCH (n) RETURN {a: 1, b: 2} AS m, n.returned").unwrap(), ["m", "n.returned"]);
        assert_eq!(cols("MATCH (n) RETURN n.url = 'http://x' AS web // the site
").unwrap(), ["web"]);
        assert_eq!(cols("CREATE (n:P {name: 'x'})"), None);
    }

    #[test]
    fn rows_follow_the_wanted_order() {
        let mut columns = vec!["cpu".to_string(), "count(p)".to_string(), "service".to_string()];
        let mut rows = vec![vec![1, 2, 3], vec![4, 5, 6]];
        reorder(&mut columns, &mut rows, &["service".into(), "count( p )".into(), "cpu".into()]);
        assert_eq!(columns, ["service", "count(p)", "cpu"]);
        assert_eq!(rows, [vec![3, 2, 1], vec![6, 5, 4]]);
    }

    #[test]
    fn an_unmatched_name_leaves_the_order_alone() {
        let mut columns = vec!["a".to_string(), "b".to_string()];
        let mut rows = vec![vec![1, 2]];
        reorder(&mut columns, &mut rows, &["b".into(), "c".into()]);
        reorder(&mut columns, &mut rows, &["b".into(), "b".into()]);
        reorder(&mut columns, &mut rows, &["b".into()]);
        assert_eq!((columns, rows), (vec!["a".to_string(), "b".to_string()], vec![vec![1, 2]]));
    }
}
