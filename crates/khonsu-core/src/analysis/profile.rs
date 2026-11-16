//! Relationship profiles: what each node does, counted, so one node can be
//! compared with another.
//!
//! Once one record is known to matter — a compromised host, a shell company,
//! an account that cheats — the next question is which others look like it.
//! A node's profile counts its relationships by direction, type and the
//! label at the other end (`out:CONNECTED_TO:Ip`, `in:RAN:Host`). Two
//! profiles are compared by cosine distance over `ln(1 + count)`: 0 when the
//! mix is the same, 1 when they share nothing. The log keeps one very busy
//! relationship type from hiding the rest.
//!
//! Nothing here is learned or approximate. Every candidate is scored, the
//! same graph gives the same answer, and each distance comes with the counts
//! behind it, so a result can be cited and run again. "Similar" is a
//! statement about these counts and nothing more.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use super::load::ident;
use super::{CypherReader, LoadSpec};
use crate::QueryError;

/// Nodes per page when reading them whole for their labels. Smaller than the
/// relationship page because a node carries its properties.
const NODE_PAGE: usize = 5_000;
const EDGE_PAGE: usize = 50_000;

/// How the distance is defined, stated in every result.
pub const MEASURE: &str = "cosine distance over ln(1 + count) of each node's relationships, counted by direction, type and the label at the other end";

/// A label group needs this many profiled nodes before "typical" means
/// anything; smaller groups are named and left out.
pub const MIN_GROUP: usize = 5;

/// Every node's relationship profile.
#[derive(Debug, Clone, Default)]
pub struct Profiles {
    ids: Vec<i64>,
    index: HashMap<i64, usize>,
    /// A node's labels joined with `:`; empty when it has none.
    labels: Vec<String>,
    features: Vec<String>,
    /// Per node: `(feature, count)`, by feature.
    rows: Vec<Vec<(usize, u32)>>,
    relationships: usize,
}

/// One feature two profiles are compared on, with the counts on each side.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FeatureCount {
    pub feature: String,
    /// The node being described.
    pub node: f64,
    /// What it is compared with: the reference node's count, or the mean
    /// count in the node's group.
    pub other: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub node: usize,
    pub distance: f64,
}

/// The nodes most like a reference node.
#[derive(Debug, Clone, PartialEq)]
pub struct Similar {
    /// Nearest first; ties by node ID.
    pub rows: Vec<Scored>,
    /// Nodes compared: every one was scored.
    pub candidates: usize,
    /// Nodes that qualified but have no relationships, so no profile.
    pub without_profile: usize,
}

/// A label group too small to say what is typical of it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SmallGroup {
    pub labels: String,
    pub size: usize,
}

/// A node unlike the rest of its label.
#[derive(Debug, Clone, PartialEq)]
pub struct Outlier {
    pub node: usize,
    pub distance: f64,
    pub group_size: usize,
    pub differs: Vec<FeatureCount>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outliers {
    /// Furthest from typical first; ties by node ID.
    pub rows: Vec<Outlier>,
    /// Nodes scored.
    pub scored: usize,
    pub without_profile: usize,
    pub skipped: Vec<SmallGroup>,
}

impl Profiles {
    /// From `(id, labels)` and `(source, target, type)` records. A
    /// relationship whose end is not among the nodes still counts for the
    /// end that is, with `?` for the label it cannot see.
    pub fn from_records<'a>(
        nodes: impl IntoIterator<Item = (i64, String)>,
        edges: impl IntoIterator<Item = (i64, i64, &'a str)>,
    ) -> Self {
        let mut out = Self::default();
        for (id, labels) in nodes {
            if !out.index.contains_key(&id) {
                out.index.insert(id, out.ids.len());
                out.ids.push(id);
                out.labels.push(labels);
            }
        }
        let mut names: BTreeMap<String, ()> = BTreeMap::new();
        let mut counts: Vec<BTreeMap<String, u32>> = vec![BTreeMap::new(); out.ids.len()];
        for (source, target, rel_type) in edges {
            out.relationships += 1;
            let (s, t) = (
                out.index.get(&source).copied(),
                out.index.get(&target).copied(),
            );
            let label = |end: Option<usize>| {
                end.map(|v| out.labels[v].as_str())
                    .filter(|l| !l.is_empty())
                    .unwrap_or("?")
            };
            if let Some(s) = s {
                let name = format!("out:{rel_type}:{}", label(t));
                names.insert(name.clone(), ());
                *counts[s].entry(name).or_default() += 1;
            }
            if let Some(t) = t {
                let name = format!("in:{rel_type}:{}", label(s));
                names.insert(name.clone(), ());
                *counts[t].entry(name).or_default() += 1;
            }
        }
        // Features in name order, so the same graph numbers them the same way.
        out.features = names.into_keys().collect();
        let position: HashMap<&str, usize> = out
            .features
            .iter()
            .enumerate()
            .map(|(i, f)| (f.as_str(), i))
            .collect();
        out.rows = counts
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|(name, count)| (position[name.as_str()], count))
                    .collect()
            })
            .collect();
        out
    }

    pub fn node_count(&self) -> usize {
        self.ids.len()
    }

    pub fn relationship_count(&self) -> usize {
        self.relationships
    }

    pub fn feature_count(&self) -> usize {
        self.features.len()
    }

    pub fn id(&self, node: usize) -> i64 {
        self.ids[node]
    }

    pub fn index_of(&self, id: i64) -> Option<usize> {
        self.index.get(&id).copied()
    }

    pub fn labels(&self, node: usize) -> &str {
        &self.labels[node]
    }

    /// A node's counts, largest first.
    pub fn profile(&self, node: usize) -> Vec<(String, u32)> {
        let mut row: Vec<(String, u32)> = self.rows[node]
            .iter()
            .map(|&(f, c)| (self.features[f].clone(), c))
            .collect();
        row.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        row
    }

    /// The nodes most like `reference`, among those with its labels, or with
    /// any of `labels` when given. `None` when the reference has no
    /// relationships to compare.
    pub fn similar(&self, reference: usize, labels: &[String]) -> Option<Similar> {
        if self.rows[reference].is_empty() {
            return None;
        }
        let wanted = |v: usize| {
            if labels.is_empty() {
                self.labels[v] == self.labels[reference]
            } else {
                self.labels[v]
                    .split(':')
                    .any(|l| labels.iter().any(|w| w == l))
            }
        };
        let mut out = Similar {
            rows: Vec::new(),
            candidates: 0,
            without_profile: 0,
        };
        for v in (0..self.node_count()).filter(|&v| v != reference && wanted(v)) {
            if self.rows[v].is_empty() {
                out.without_profile += 1;
                continue;
            }
            out.candidates += 1;
            out.rows.push(Scored {
                node: v,
                distance: distance(&self.rows[reference], &self.rows[v]),
            });
        }
        out.rows.sort_by(|a, b| {
            a.distance
                .total_cmp(&b.distance)
                .then_with(|| self.ids[a.node].cmp(&self.ids[b.node]))
        });
        Some(out)
    }

    /// The counts behind a distance: the features either node has, heaviest
    /// first, at most `top`, with each side's count.
    pub fn compare(&self, node: usize, other: usize, top: usize) -> Vec<FeatureCount> {
        let (a, b) = (dense(&self.rows[node]), dense(&self.rows[other]));
        let mut list: Vec<(u32, usize, u32, u32)> = a
            .iter()
            .map(|(&feature, &count)| (feature, count, b.get(&feature).copied().unwrap_or(0)))
            .chain(
                b.iter()
                    .filter(|(f, _)| !a.contains_key(*f))
                    .map(|(&feature, &theirs)| (feature, 0, theirs)),
            )
            .map(|(feature, count, theirs)| (count.max(theirs), feature, count, theirs))
            .collect();
        list.sort_by(|x, y| {
            y.0.cmp(&x.0)
                .then_with(|| self.features[x.1].cmp(&self.features[y.1]))
        });
        list.truncate(top);
        list.into_iter()
            .map(|(_, feature, node, other)| FeatureCount {
                feature: self.features[feature].clone(),
                node: node as f64,
                other: other as f64,
            })
            .collect()
    }

    /// Nodes furthest from what is typical of their labels: the distance
    /// between a node's profile and the mean profile of its group. Only
    /// groups named in `labels` when given.
    pub fn outliers(&self, labels: &[String], top: usize, explain: usize) -> Outliers {
        let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        let mut out = Outliers {
            rows: Vec::new(),
            scored: 0,
            without_profile: 0,
            skipped: Vec::new(),
        };
        for v in 0..self.node_count() {
            let key = self.labels[v].as_str();
            if !labels.is_empty() && !key.split(':').any(|l| labels.iter().any(|w| w == l)) {
                continue;
            }
            if self.rows[v].is_empty() {
                out.without_profile += 1;
            } else {
                groups.entry(key).or_default().push(v);
            }
        }
        for (key, members) in groups {
            if members.len() < MIN_GROUP {
                out.skipped.push(SmallGroup {
                    labels: key.to_string(),
                    size: members.len(),
                });
                continue;
            }
            let size = members.len() as f64;
            // Mean scaled weight (what the distance is taken from) and mean
            // count (what a person reads) per feature.
            let mut typical: BTreeMap<usize, (f64, f64)> = BTreeMap::new();
            for &v in &members {
                for &(feature, count) in &self.rows[v] {
                    let slot = typical.entry(feature).or_default();
                    slot.0 += scale(count) / size;
                    slot.1 += count as f64 / size;
                }
            }
            let norm = typical.values().map(|(w, _)| w * w).sum::<f64>().sqrt();
            out.scored += members.len();
            for &v in &members {
                let own = dense(&self.rows[v]);
                let (mut dot, mut length) = (0.0, 0.0);
                for (feature, &count) in &own {
                    let w = scale(count);
                    dot += w * typical[feature].0;
                    length += w * w;
                }
                let distance = (1.0 - dot / (length.sqrt() * norm)).max(0.0);
                if distance <= 1e-12 {
                    continue;
                }
                let mut differs: Vec<(f64, usize, u32, f64)> = typical
                    .iter()
                    .map(|(&feature, &(weight, mean))| {
                        let count = own.get(&feature).copied().unwrap_or(0);
                        ((scale(count) - weight).abs(), feature, count, mean)
                    })
                    .collect();
                differs.sort_by(|x, y| {
                    y.0.total_cmp(&x.0)
                        .then_with(|| self.features[x.1].cmp(&self.features[y.1]))
                });
                differs.truncate(explain);
                out.rows.push(Outlier {
                    node: v,
                    distance,
                    group_size: members.len(),
                    differs: differs
                        .into_iter()
                        .map(|(_, feature, count, mean)| FeatureCount {
                            feature: self.features[feature].clone(),
                            node: count as f64,
                            other: mean,
                        })
                        .collect(),
                });
            }
        }
        out.rows.sort_by(|a, b| {
            b.distance
                .total_cmp(&a.distance)
                .then_with(|| self.ids[a.node].cmp(&self.ids[b.node]))
        });
        out.rows.truncate(top);
        out
    }
}

fn scale(count: u32) -> f64 {
    (count as f64).ln_1p()
}

fn dense(row: &[(usize, u32)]) -> BTreeMap<usize, u32> {
    row.iter().copied().collect()
}

/// `1 - cosine similarity` of two non-empty profiles, in `[0, 1]`: counts
/// are never negative, so neither is the similarity.
fn distance(a: &[(usize, u32)], b: &[(usize, u32)]) -> f64 {
    let length = |row: &[(usize, u32)]| {
        row.iter()
            .map(|&(_, c)| scale(c).powi(2))
            .sum::<f64>()
            .sqrt()
    };
    let (mut i, mut j, mut dot) = (0, 0, 0.0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                dot += scale(a[i].1) * scale(b[j].1);
                i += 1;
                j += 1;
            }
        }
    }
    (1.0 - dot / (length(a) * length(b))).max(0.0)
}

/// Reads the profiles of a graph: the whole of it, only relationships of
/// `spec.types`, or what `spec.cypher` returns. `spec.labels` does not
/// narrow what is read — a node's relationships to other labels are what
/// describe it — and is for the caller to choose candidates with.
pub fn load_profiles(
    reader: &mut dyn CypherReader,
    spec: &LoadSpec,
) -> Result<Profiles, QueryError> {
    if let Some(text) = &spec.cypher {
        let graph = reader.cypher(text)?.graph.unwrap_or_default();
        return Ok(Profiles::from_records(
            graph.nodes.iter().map(|n| (n.id, n.labels.join(":"))),
            graph
                .edges
                .iter()
                .map(|e| (e.source, e.target, e.rel_type.as_str())),
        ));
    }
    let mut nodes = Vec::new();
    let mut skip = 0;
    loop {
        let page = reader.cypher(&format!(
            "MATCH (n) RETURN id(n) AS id, n ORDER BY id SKIP {skip} LIMIT {NODE_PAGE}"
        ))?;
        let read = page.rows.len();
        nodes.extend(
            page.graph
                .unwrap_or_default()
                .nodes
                .into_iter()
                .map(|n| (n.id, n.labels.join(":"))),
        );
        if read < NODE_PAGE {
            break;
        }
        skip += NODE_PAGE;
    }
    let patterns: Vec<String> = if spec.types.is_empty() {
        vec!["(a)-[r]->(b)".into()]
    } else {
        spec.types
            .iter()
            .map(|t| format!("(a)-[r:{}]->(b)", ident(t)))
            .collect()
    };
    let mut edges: Vec<(i64, i64, String)> = Vec::new();
    for pattern in patterns {
        let mut skip = 0;
        loop {
            let page = reader.cypher(&format!(
                "MATCH {pattern} RETURN id(r) AS rid, id(a) AS s, id(b) AS t, type(r) AS ty ORDER BY rid SKIP {skip} LIMIT {EDGE_PAGE}"
            ))?;
            let col = |name: &str| page.columns.iter().position(|c| c == name);
            let (Some(s), Some(t), Some(ty)) = (col("s"), col("t"), col("ty")) else {
                break;
            };
            for row in &page.rows {
                if let (Some(source), Some(target), Some(rel_type)) =
                    (row[s].as_i64(), row[t].as_i64(), row[ty].as_str())
                {
                    edges.push((source, target, rel_type.to_string()));
                }
            }
            if page.rows.len() < EDGE_PAGE {
                break;
            }
            skip += EDGE_PAGE;
        }
    }
    Ok(Profiles::from_records(
        nodes,
        edges.iter().map(|(s, t, ty)| (*s, *t, ty.as_str())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hosts 1..=6 each run a process and resolve a name; host 3 also opens
    /// 40 connections outward, host 7 has no relationships.
    fn hosts() -> Profiles {
        let mut nodes: Vec<(i64, String)> = (1..=7).map(|id| (id, "Host".to_string())).collect();
        nodes.extend([
            (100, "Process".to_string()),
            (200, "Domain".to_string()),
            (300, "Ip".to_string()),
        ]);
        let mut edges: Vec<(i64, i64, &str)> = Vec::new();
        for host in 1..=6 {
            edges.push((host, 100, "RAN"));
            edges.push((host, 200, "RESOLVED"));
        }
        edges.push((2, 100, "RAN"));
        edges.extend(std::iter::repeat_n((3, 300, "CONNECTED_TO"), 40));
        Profiles::from_records(nodes, edges)
    }

    #[test]
    fn a_profile_counts_relationships_by_direction_type_and_the_other_label() {
        let profiles = hosts();
        let host = profiles.index_of(3).unwrap();
        assert_eq!(
            profiles.profile(host),
            vec![
                ("out:CONNECTED_TO:Ip".to_string(), 40),
                ("out:RAN:Process".to_string(), 1),
                ("out:RESOLVED:Domain".to_string(), 1)
            ]
        );
        let process = profiles.index_of(100).unwrap();
        assert_eq!(
            profiles.profile(process),
            vec![("in:RAN:Host".to_string(), 7)]
        );
        assert_eq!(profiles.relationship_count(), 53);
    }

    #[test]
    fn similar_ranks_every_node_of_the_same_label_nearest_first() {
        let profiles = hosts();
        let similar = profiles
            .similar(profiles.index_of(1).unwrap(), &[])
            .unwrap();
        let ids: Vec<i64> = similar.rows.iter().map(|r| profiles.id(r.node)).collect();
        // 4, 5, 6 match exactly (ties by ID), 2 ran one more process, 3 is the odd one.
        assert_eq!(ids, [4, 5, 6, 2, 3]);
        assert_eq!(similar.rows[0].distance, 0.0);
        assert!(
            similar.rows[3].distance > 0.0 && similar.rows[3].distance < similar.rows[4].distance
        );
        assert_eq!((similar.candidates, similar.without_profile), (5, 1));
        // Other labels are asked for by name.
        let any = profiles
            .similar(profiles.index_of(1).unwrap(), &["Process".into()])
            .unwrap();
        assert_eq!(any.rows.len(), 1);
        assert_eq!(any.rows[0].distance, 1.0);
    }

    #[test]
    fn a_node_without_relationships_has_nothing_to_compare() {
        let profiles = hosts();
        assert_eq!(profiles.similar(profiles.index_of(7).unwrap(), &[]), None);
    }

    #[test]
    fn compare_names_the_counts_behind_a_distance() {
        let profiles = hosts();
        let features = profiles.compare(
            profiles.index_of(3).unwrap(),
            profiles.index_of(2).unwrap(),
            5,
        );
        let read: Vec<(&str, f64, f64)> = features
            .iter()
            .map(|f| (f.feature.as_str(), f.node, f.other))
            .collect();
        assert_eq!(
            read,
            [
                ("out:CONNECTED_TO:Ip", 40.0, 0.0),
                ("out:RAN:Process", 1.0, 2.0),
                ("out:RESOLVED:Domain", 1.0, 1.0)
            ]
        );
        assert_eq!(
            profiles
                .compare(
                    profiles.index_of(3).unwrap(),
                    profiles.index_of(2).unwrap(),
                    1
                )
                .len(),
            1
        );
    }

    #[test]
    fn outliers_put_the_node_unlike_its_label_first_and_name_small_groups() {
        let profiles = hosts();
        let outliers = profiles.outliers(&[], 10, 3);
        assert_eq!(profiles.id(outliers.rows[0].node), 3);
        assert_eq!(outliers.rows[0].group_size, 6);
        assert_eq!(outliers.rows[0].differs[0].feature, "out:CONNECTED_TO:Ip");
        assert_eq!(outliers.rows[0].differs[0].node, 40.0);
        assert!((outliers.rows[0].differs[0].other - 40.0 / 6.0).abs() < 1e-12);
        assert!(outliers
            .rows
            .iter()
            .all(|r| profiles.labels(r.node) == "Host"));
        assert_eq!((outliers.scored, outliers.without_profile), (6, 1));
        assert_eq!(
            outliers
                .skipped
                .iter()
                .map(|g| g.labels.as_str())
                .collect::<Vec<_>>(),
            ["Domain", "Ip", "Process"]
        );
        // The same graph gives the same answer.
        assert_eq!(outliers, hosts().outliers(&[], 10, 3));
        assert!(profiles
            .outliers(&["Process".into()], 10, 3)
            .rows
            .is_empty());
    }
}
