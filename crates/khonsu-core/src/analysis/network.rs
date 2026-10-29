use std::collections::HashMap;

/// A graph as the algorithms read it: nodes by dense index, with HawDB's
/// IDs beside them, and weighted adjacency both ways. Parallel
/// relationships stay separate (PageRank and degree count each); the
/// algorithms that want a simple graph collapse them.
#[derive(Debug, Clone, Default)]
pub struct Network {
    ids: Vec<i64>,
    index: HashMap<i64, usize>,
    out: Vec<Vec<(usize, f64)>>,
    inn: Vec<Vec<(usize, f64)>>,
    edges: usize,
    /// True when some relationship carried a weight other than 1.
    weighted: bool,
}

impl Network {
    pub fn new() -> Self {
        Self::default()
    }

    /// The node with HawDB ID `id`, added if new.
    pub fn add_node(&mut self, id: i64) -> usize {
        if let Some(&ix) = self.index.get(&id) {
            return ix;
        }
        let ix = self.ids.len();
        self.ids.push(id);
        self.index.insert(id, ix);
        self.out.push(Vec::new());
        self.inn.push(Vec::new());
        ix
    }

    pub fn add_edge(&mut self, source: i64, target: i64, weight: f64) {
        let (s, t) = (self.add_node(source), self.add_node(target));
        self.out[s].push((t, weight));
        self.inn[t].push((s, weight));
        self.edges += 1;
        if weight != 1.0 {
            self.weighted = true;
        }
    }

    pub fn node_count(&self) -> usize {
        self.ids.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges
    }

    pub fn is_weighted(&self) -> bool {
        self.weighted
    }

    /// HawDB's ID for node `ix`.
    pub fn id(&self, ix: usize) -> i64 {
        self.ids[ix]
    }

    pub fn ids(&self) -> &[i64] {
        &self.ids
    }

    pub fn index_of(&self, id: i64) -> Option<usize> {
        self.index.get(&id).copied()
    }

    pub fn out_edges(&self, v: usize) -> &[(usize, f64)] {
        &self.out[v]
    }

    pub fn in_edges(&self, v: usize) -> &[(usize, f64)] {
        &self.inn[v]
    }

    /// Neighbors when every relationship reads both ways, self-loops and
    /// parallel relationships collapsed; a pair joined both ways keeps the
    /// smaller weight, as a shortest path would.
    pub fn undirected(&self) -> Vec<Vec<(usize, f64)>> {
        let mut adj: Vec<HashMap<usize, f64>> = vec![HashMap::new(); self.node_count()];
        for (s, edges) in self.out.iter().enumerate() {
            for &(t, w) in edges {
                if s == t {
                    continue;
                }
                for (a, b) in [(s, t), (t, s)] {
                    let slot = adj[a].entry(b).or_insert(w);
                    *slot = slot.min(w);
                }
            }
        }
        adj.into_iter()
            .map(|m| {
                let mut v: Vec<_> = m.into_iter().collect();
                v.sort_by_key(|&(n, _)| n);
                v
            })
            .collect()
    }

    /// Outgoing neighbors with parallel relationships and self-loops
    /// collapsed, keeping the smallest weight: the simple directed graph
    /// paths run on.
    pub fn directed_simple(&self) -> Vec<Vec<(usize, f64)>> {
        self.out
            .iter()
            .enumerate()
            .map(|(s, edges)| {
                let mut m: HashMap<usize, f64> = HashMap::new();
                for &(t, w) in edges {
                    if s != t {
                        let slot = m.entry(t).or_insert(w);
                        *slot = slot.min(w);
                    }
                }
                let mut v: Vec<_> = m.into_iter().collect();
                v.sort_by_key(|&(n, _)| n);
                v
            })
            .collect()
    }

    /// Every relationship reversed, simple: what "distance to" a node runs on.
    pub fn reversed_simple(&self) -> Vec<Vec<(usize, f64)>> {
        let mut rev: Vec<HashMap<usize, f64>> = vec![HashMap::new(); self.node_count()];
        for (s, edges) in self.out.iter().enumerate() {
            for &(t, w) in edges {
                if s != t {
                    let slot = rev[t].entry(s).or_insert(w);
                    *slot = slot.min(w);
                }
            }
        }
        rev.into_iter()
            .map(|m| {
                let mut v: Vec<_> = m.into_iter().collect();
                v.sort_by_key(|&(n, _)| n);
                v
            })
            .collect()
    }

    /// Build one from `(source, target)` pairs, for tests.
    pub fn from_pairs(pairs: &[(i64, i64)]) -> Self {
        let mut net = Self::new();
        for &(s, t) in pairs {
            net.add_edge(s, t, 1.0);
        }
        net
    }
}
