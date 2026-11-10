//! Results to canvas items: captions, groups, and the look a dashboard
//! `graph` gives its nodes.

use super::*;

const CAPTION_KEYS: &[&str] = &["name", "title", "label", "display_name", "id", "key", "khonsu_id"];

/// Edge property carrying the HawDB relationship id, so a selected edge maps back.
const EDGE_ID: &str = "khonsu_edge_id";

pub(super) fn caption(node: &GraphNode, preferred: Option<&str>) -> String {
    for key in preferred.into_iter().chain(CAPTION_KEYS.iter().copied()) {
        match node.properties.get(key) {
            Some(Json::String(s)) => return truncate(s, 26),
            Some(v @ (Json::Number(_) | Json::Bool(_))) => return v.to_string(),
            _ => {}
        }
    }
    format!("{} #{}", node.labels.first().map(String::as_str).unwrap_or("node"), node.id)
}

pub(super) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

pub(super) fn first_label(node: &GraphNode) -> &str {
    node.labels.first().map(String::as_str).unwrap_or("")
}

/// At most this many groups are offered; more is a name, not a grouping.
const MAX_GROUPS: usize = 60;

/// `node`'s value of `key` as a group id, when it is a plain value.
pub(super) fn group_value(node: &GraphNode, key: &str) -> Option<String> {
    match node.properties.get(key)? {
        Json::String(s) if !s.is_empty() => Some(s.clone()),
        v @ (Json::Number(_) | Json::Bool(_)) => Some(v.to_string()),
        _ => None,
    }
}

/// Node properties worth grouping by: plain values that several nodes
/// share, fewer distinct ones than nodes. Provenance is left out.
pub(super) fn group_keys<'a>(nodes: impl Iterator<Item = &'a GraphNode>) -> Vec<String> {
    let mut seen: BTreeMap<&str, (usize, BTreeSet<String>)> = BTreeMap::new();
    let mut total = 0;
    for node in nodes {
        total += 1;
        for key in node.properties.keys().filter(|k| !k.starts_with("khonsu_")) {
            if let Some(value) = group_value(node, key) {
                let (count, values) = seen.entry(key).or_default();
                *count += 1;
                if values.len() <= MAX_GROUPS {
                    values.insert(value);
                }
            }
        }
    }
    seen.into_iter()
        .filter(|(_, (count, values))| {
            values.len() >= 2 && values.len() <= MAX_GROUPS && values.len() * 2 <= *count && *count * 2 >= total
        })
        .map(|(key, _)| key.to_string())
        .collect()
}

/// Radii a sized node takes, in world units: the smallest value gets the
/// first, the largest the second, and area follows the value between them.
const SIZED_RADIUS: (f32, f32) = (6., 26.);

/// A named or `#rrggbb` color from a `graph`'s `colors`.
pub(super) fn named_color(name: &str, cx: &App) -> Option<Hsla> {
    match name {
        "muted" => Some(cx.theme().muted_foreground),
        hex if hex.starts_with('#') => u32::from_str_radix(&hex[1..], 16).ok().map(|v| rgb(v).into()),
        tone => palette::tone(tone, cx).map(|(rule, _)| rule),
    }
}

pub(super) fn number(node: &GraphNode, key: &str) -> Option<f64> {
    match node.properties.get(key)? {
        Json::Number(n) => n.as_f64(),
        Json::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The component's node for `node`, colored like its label in the sidebar,
/// or by its group when grouping and it has a value for it. A color property
/// wins over both: the group still gathers the node, the color says its state.
/// A node without a value for the color property is muted.
pub(super) fn to_node(node: &GraphNode, look: &Look, cx: &App) -> Node {
    let label = first_label(node);
    let mut converted = Node::new(node.id.to_string(), caption(node, look.caption), label.to_string());
    let grouped = look.group.and_then(|key| group_value(node, key));
    let has_group = grouped.is_some();
    if let Some(value) = grouped {
        converted = converted.group(value);
    }
    if let Some(key) = look.style.color.as_deref() {
        let color = match group_value(node, key) {
            Some(value) => look
                .style
                .colors
                .iter()
                .find(|(fixed, _)| *fixed == value)
                .and_then(|(_, name)| named_color(name, cx))
                .unwrap_or_else(|| {
                    let slot = look.color_slots.iter().position(|v| *v == value).unwrap_or(0);
                    palette::color(slot, cx)
                }),
            None => cx.theme().muted_foreground,
        };
        converted = converted.color(color);
    } else if !has_group {
        converted = converted.color(palette::label_color(label, cx));
    }
    if let (Some(key), Some((low, high))) = (look.style.size.as_deref(), look.size_range) {
        if let Some(value) = number(node, key).filter(|_| high > low) {
            let share = ((value - low) / (high - low)).clamp(0., 1.).sqrt() as f32;
            converted = converted.size(SIZED_RADIUS.0 + share * (SIZED_RADIUS.1 - SIZED_RADIUS.0));
        }
    }
    converted
}

pub(super) fn to_edge(edge: &GraphEdge) -> Edge {
    Edge::new(edge.source.to_string(), edge.target.to_string(), edge.rel_type.clone())
        .property(EDGE_ID, edge.id.to_string())
}

pub(super) fn edge_id(edge: &Edge) -> Option<i64> {
    edge.properties.iter().find(|(k, _)| k.as_ref() == EDGE_ID)?.1.parse().ok()
}
