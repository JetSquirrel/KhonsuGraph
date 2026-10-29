//! The algorithms, each after networkx's definition of the same name.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use serde::Serialize;

use super::Network;

/// Adjacency as the path algorithms walk it.
type Adjacency = Vec<Vec<(usize, f64)>>;

fn adjacency(net: &Network, directed: bool) -> Adjacency {
    if directed {
        net.directed_simple()
    } else {
        net.undirected()
    }
}

/// In-, out- and total degree per node, counting parallel relationships and
/// self-loops as networkx's MultiDiGraph does.
pub fn degrees(net: &Network) -> Vec<(usize, usize)> {
    (0..net.node_count()).map(|v| (net.in_edges(v).len(), net.out_edges(v).len())).collect()
}

/// `degree_centrality`: total degree over `n - 1`.
pub fn degree_centrality(net: &Network) -> Vec<f64> {
    let n = net.node_count();
    let scale = if n > 1 { 1.0 / (n - 1) as f64 } else { 1.0 };
    degrees(net).into_iter().map(|(i, o)| (i + o) as f64 * scale).collect()
}

/// `pagerank`: the stationary visit share of a random walk that follows a
/// relationship (in proportion to its weight) with probability `alpha` and
/// otherwise jumps anywhere; a node with no way out jumps anywhere. Returns
/// the scores and whether they converged within `max_iter` rounds.
pub fn pagerank(net: &Network, alpha: f64, directed: bool, max_iter: usize, tol: f64) -> (Vec<f64>, bool) {
    let n = net.node_count();
    if n == 0 {
        return (Vec::new(), true);
    }
    // Every relationship, both ways when undirected, as networkx turns an
    // undirected graph into a directed one.
    let mut out: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for v in 0..n {
        for &(t, w) in net.out_edges(v) {
            out[v].push((t, w));
            if !directed {
                out[t].push((v, w));
            }
        }
    }
    let out_weight: Vec<f64> = out.iter().map(|e| e.iter().map(|&(_, w)| w).sum()).collect();
    let uniform = 1.0 / n as f64;
    let mut x = vec![uniform; n];
    for _ in 0..max_iter {
        let last = std::mem::replace(&mut x, vec![0.0; n]);
        let dangling: f64 = (0..n).filter(|&v| out_weight[v] == 0.0).map(|v| last[v]).sum();
        for v in 0..n {
            if out_weight[v] > 0.0 {
                for &(t, w) in &out[v] {
                    x[t] += alpha * last[v] * w / out_weight[v];
                }
            }
        }
        let jump = (alpha * dangling + (1.0 - alpha)) * uniform;
        x.iter_mut().for_each(|value| *value += jump);
        let err: f64 = x.iter().zip(&last).map(|(a, b)| (a - b).abs()).sum();
        if err < n as f64 * tol {
            return (x, true);
        }
    }
    (x, false)
}

/// A node's distance from the source, its shortest-path count and the
/// nodes it is reached through: one source's share of Brandes' algorithm.
struct Paths {
    order: Vec<usize>,
    sigma: Vec<f64>,
    preds: Vec<Vec<usize>>,
}

fn single_source_paths(adj: &Adjacency, source: usize, weighted: bool) -> Paths {
    let n = adj.len();
    let mut sigma = vec![0.0; n];
    let mut preds = vec![Vec::new(); n];
    let mut dist = vec![f64::INFINITY; n];
    let mut order = Vec::new();
    sigma[source] = 1.0;
    dist[source] = 0.0;
    if weighted {
        let mut heap = BinaryHeap::new();
        heap.push(State { cost: 0.0, node: source });
        let mut done = vec![false; n];
        while let Some(State { cost, node: v }) = heap.pop() {
            if done[v] || cost > dist[v] {
                continue;
            }
            done[v] = true;
            order.push(v);
            for &(w, len) in &adj[v] {
                let alt = cost + len;
                if alt < dist[w] - 1e-12 {
                    dist[w] = alt;
                    sigma[w] = sigma[v];
                    preds[w] = vec![v];
                    heap.push(State { cost: alt, node: w });
                } else if (alt - dist[w]).abs() <= 1e-12 && !done[w] {
                    sigma[w] += sigma[v];
                    preds[w].push(v);
                }
            }
        }
    } else {
        let mut queue = VecDeque::from([source]);
        while let Some(v) = queue.pop_front() {
            order.push(v);
            for &(w, _) in &adj[v] {
                if dist[w].is_infinite() {
                    dist[w] = dist[v] + 1.0;
                    queue.push_back(w);
                }
                if dist[w] == dist[v] + 1.0 {
                    sigma[w] += sigma[v];
                    preds[w].push(v);
                }
            }
        }
    }
    Paths { order, sigma, preds }
}

/// `betweenness_centrality`, normalized: the share of shortest paths
/// between other pairs that pass through each node. With `sample`, only
/// that many sources (spread evenly over the nodes) are walked and the sum
/// scaled up, as networkx's `k` does — exact when `sample` covers every node.
pub fn betweenness(net: &Network, directed: bool, weighted: bool, sample: Option<usize>) -> Vec<f64> {
    let n = net.node_count();
    let adj = adjacency(net, directed);
    let mut score = vec![0.0; n];
    let sources = spread(n, sample);
    for &s in &sources {
        let Paths { order, sigma, preds } = single_source_paths(&adj, s, weighted);
        let mut delta = vec![0.0; n];
        for &w in order.iter().rev() {
            for &v in &preds[w] {
                delta[v] += sigma[v] / sigma[w] * (1.0 + delta[w]);
            }
            if w != s {
                score[w] += delta[w];
            }
        }
    }
    if n > 2 {
        let mut scale = 1.0 / ((n - 1) * (n - 2)) as f64;
        if sources.len() < n {
            scale *= n as f64 / sources.len() as f64;
        }
        score.iter_mut().for_each(|value| *value *= scale);
    }
    score
}

/// `closeness_centrality` with Wasserman and Faust's correction: how near a
/// node is to those that can reach it (by incoming distance, when directed),
/// scaled by the share of the graph that can.
pub fn closeness(net: &Network, directed: bool, weighted: bool) -> Vec<f64> {
    let n = net.node_count();
    let adj = if directed { net.reversed_simple() } else { net.undirected() };
    (0..n)
        .map(|u| {
            let dist = distances(&adj, u, weighted);
            let reached: Vec<f64> = dist.into_iter().filter(|d| d.is_finite()).collect();
            let total: f64 = reached.iter().sum();
            let r = reached.len();
            if total > 0.0 && n > 1 {
                let c = (r - 1) as f64 / total;
                c * (r - 1) as f64 / (n - 1) as f64
            } else {
                0.0
            }
        })
        .collect()
}

fn distances(adj: &Adjacency, source: usize, weighted: bool) -> Vec<f64> {
    let mut dist = vec![f64::INFINITY; adj.len()];
    dist[source] = 0.0;
    if weighted {
        let mut heap = BinaryHeap::from([State { cost: 0.0, node: source }]);
        while let Some(State { cost, node }) = heap.pop() {
            if cost > dist[node] {
                continue;
            }
            for &(next, len) in &adj[node] {
                if cost + len < dist[next] {
                    dist[next] = cost + len;
                    heap.push(State { cost: cost + len, node: next });
                }
            }
        }
    } else {
        let mut queue = VecDeque::from([source]);
        while let Some(v) = queue.pop_front() {
            for &(w, _) in &adj[v] {
                if dist[w].is_infinite() {
                    dist[w] = dist[v] + 1.0;
                    queue.push_back(w);
                }
            }
        }
    }
    dist
}

/// A shortest path from `source` to `target` (node indexes, both ends
/// included) and its length; `None` when none exists.
pub fn shortest_path(net: &Network, source: usize, target: usize, directed: bool, weighted: bool) -> Option<(Vec<usize>, f64)> {
    let adj = adjacency(net, directed);
    let n = adj.len();
    let mut dist = vec![f64::INFINITY; n];
    let mut prev = vec![usize::MAX; n];
    dist[source] = 0.0;
    if weighted {
        let mut heap = BinaryHeap::from([State { cost: 0.0, node: source }]);
        while let Some(State { cost, node }) = heap.pop() {
            if node == target {
                break;
            }
            if cost > dist[node] {
                continue;
            }
            for &(next, len) in &adj[node] {
                if cost + len < dist[next] {
                    dist[next] = cost + len;
                    prev[next] = node;
                    heap.push(State { cost: cost + len, node: next });
                }
            }
        }
    } else {
        let mut queue = VecDeque::from([source]);
        while let Some(v) = queue.pop_front() {
            if v == target {
                break;
            }
            for &(w, _) in &adj[v] {
                if dist[w].is_infinite() {
                    dist[w] = dist[v] + 1.0;
                    prev[w] = v;
                    queue.push_back(w);
                }
            }
        }
    }
    if dist[target].is_infinite() {
        return None;
    }
    let mut path = vec![target];
    while let Some(&last) = path.last() {
        if last == source {
            break;
        }
        path.push(prev[last]);
    }
    path.reverse();
    Some((path, dist[target]))
}

/// Groups of nodes, largest first: `groups[i]` is a group's members, and
/// `of[v]` the group node `v` is in.
#[derive(Debug, Clone, Default)]
pub struct Partition {
    pub of: Vec<usize>,
    pub groups: Vec<Vec<usize>>,
}

impl Partition {
    /// Number groups by size, largest first, ties by their first member.
    fn from_labels(labels: &[usize]) -> Self {
        let mut by_label: HashMap<usize, Vec<usize>> = HashMap::new();
        for (v, &label) in labels.iter().enumerate() {
            by_label.entry(label).or_default().push(v);
        }
        let mut groups: Vec<Vec<usize>> = by_label.into_values().collect();
        groups.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
        let mut of = vec![0; labels.len()];
        for (g, members) in groups.iter().enumerate() {
            for &v in members {
                of[v] = g;
            }
        }
        Self { of, groups }
    }
}

/// `weakly_connected_components` (`connected_components` when undirected):
/// nodes joined by relationships in either direction.
pub fn weak_components(net: &Network) -> Partition {
    let n = net.node_count();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut v: usize) -> usize {
        while parent[v] != v {
            parent[v] = parent[parent[v]];
            v = parent[v];
        }
        v
    }
    for v in 0..n {
        for &(t, _) in net.out_edges(v) {
            let (a, b) = (find(&mut parent, v), find(&mut parent, t));
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let labels: Vec<usize> = (0..n).map(|v| find(&mut parent, v)).collect();
    Partition::from_labels(&labels)
}

/// `strongly_connected_components`: nodes that can each reach the others
/// following relationships' direction. Tarjan's algorithm, without
/// recursion so a long chain cannot overflow the stack.
pub fn strong_components(net: &Network) -> Partition {
    let n = net.node_count();
    let adj = net.directed_simple();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut labels = vec![0; n];
    let mut next_index = 0;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        // (node, position in its adjacency)
        let mut work = vec![(root, 0usize)];
        index[root] = next_index;
        low[root] = next_index;
        next_index += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut pos)) = work.last_mut() {
            if *pos < adj[v].len() {
                let w = adj[v][*pos].0;
                *pos += 1;
                if index[w] == usize::MAX {
                    index[w] = next_index;
                    low[w] = next_index;
                    next_index += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    low[parent] = low[parent].min(low[v]);
                }
                if low[v] == index[v] {
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        labels[w] = v;
                        if w == v {
                            break;
                        }
                    }
                }
            }
        }
    }
    Partition::from_labels(&labels)
}

/// `louvain_communities`: groups that maximize modularity, every
/// relationship read both ways and parallel ones adding their weights. Moves
/// each node to the neighboring group that gains most, then merges groups
/// into single nodes and repeats until nothing moves. `seed` fixes the
/// order nodes are visited in, so a result repeats. Returns the groups and
/// their modularity.
pub fn louvain(net: &Network, resolution: f64, seed: u64) -> (Partition, f64) {
    let n = net.node_count();
    // Symmetric weighted adjacency, parallel relationships summed.
    let mut adj: Vec<HashMap<usize, f64>> = vec![HashMap::new(); n];
    for v in 0..n {
        for &(t, w) in net.out_edges(v) {
            *adj[v].entry(t).or_default() += w;
            if t != v {
                *adj[t].entry(v).or_default() += w;
            }
        }
    }
    let original: Vec<Vec<(usize, f64)>> = adj.iter().map(|m| m.iter().map(|(&k, &w)| (k, w)).collect()).collect();
    let mut level: Vec<Vec<(usize, f64)>> = original.clone();
    // Which current-level node each original node is in.
    let mut member: Vec<usize> = (0..n).collect();
    let mut rng = seed.max(1);
    loop {
        let (comm, moved) = louvain_level(&level, resolution, &mut rng);
        if !moved {
            break;
        }
        // Renumber the communities 0.. and fold the level graph into them.
        let mut renumber: HashMap<usize, usize> = HashMap::new();
        for &c in &comm {
            let next = renumber.len();
            renumber.entry(c).or_insert(next);
        }
        let k = renumber.len();
        let mut folded: Vec<HashMap<usize, f64>> = vec![HashMap::new(); k];
        for (v, edges) in level.iter().enumerate() {
            let cv = renumber[&comm[v]];
            for &(t, w) in edges {
                *folded[cv].entry(renumber[&comm[t]]).or_default() += w;
            }
        }
        member.iter_mut().for_each(|m| *m = renumber[&comm[*m]]);
        level = folded.into_iter().map(|m| m.into_iter().collect()).collect();
        if k == 1 {
            break;
        }
    }
    let partition = Partition::from_labels(&member);
    let q = modularity(&original, &partition.of, resolution);
    (partition, q)
}

/// One pass of Louvain's local moves over a level graph; returns each node's
/// community and whether any node moved.
fn louvain_level(adj: &[Vec<(usize, f64)>], resolution: f64, rng: &mut u64) -> (Vec<usize>, bool) {
    let n = adj.len();
    let k: Vec<f64> = adj.iter().map(|e| e.iter().map(|&(_, w)| w).sum()).collect();
    let m2: f64 = k.iter().sum();
    let mut comm: Vec<usize> = (0..n).collect();
    if m2 == 0.0 {
        return (comm, false);
    }
    let mut tot = k.clone();
    let mut order: Vec<usize> = (0..n).collect();
    // Fisher–Yates with a small xorshift: deterministic for a seed.
    for i in (1..n).rev() {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 7;
        *rng ^= *rng << 17;
        order.swap(i, (*rng % (i as u64 + 1)) as usize);
    }
    let mut any = false;
    loop {
        let mut moved = false;
        for &v in &order {
            let cv = comm[v];
            let mut to: HashMap<usize, f64> = HashMap::new();
            for &(t, w) in &adj[v] {
                if t != v {
                    *to.entry(comm[t]).or_default() += w;
                }
            }
            tot[cv] -= k[v];
            let gain = |c: usize, w_to: f64| w_to - resolution * tot[c] * k[v] / m2;
            let mut best = cv;
            let mut best_gain = gain(cv, to.get(&cv).copied().unwrap_or(0.0));
            let mut candidates: Vec<_> = to.iter().map(|(&c, &w)| (c, w)).collect();
            candidates.sort_by_key(|&(c, _)| c);
            for (c, w) in candidates {
                let g = gain(c, w);
                if g > best_gain + 1e-12 {
                    best = c;
                    best_gain = g;
                }
            }
            tot[best] += k[v];
            if best != cv {
                comm[v] = best;
                moved = true;
                any = true;
            }
        }
        if !moved {
            break;
        }
    }
    (comm, any)
}

/// Newman's modularity of grouping `of` over symmetric adjacency `adj`.
fn modularity(adj: &[Vec<(usize, f64)>], of: &[usize], resolution: f64) -> f64 {
    let k: Vec<f64> = adj.iter().map(|e| e.iter().map(|&(_, w)| w).sum()).collect();
    let m2: f64 = k.iter().sum();
    if m2 == 0.0 {
        return 0.0;
    }
    let groups = of.iter().max().map_or(0, |m| m + 1);
    let (mut inside, mut total) = (vec![0.0; groups], vec![0.0; groups]);
    for (v, edges) in adj.iter().enumerate() {
        total[of[v]] += k[v];
        for &(t, w) in edges {
            if of[t] == of[v] {
                inside[of[v]] += w;
            }
        }
    }
    (0..groups).map(|c| inside[c] / m2 - resolution * (total[c] / m2).powi(2)).sum()
}

/// `triangles` and `clustering` (undirected, unweighted): the triangles
/// through each node, and the share of its neighbor pairs that are joined.
pub fn clustering(net: &Network) -> (Vec<usize>, Vec<f64>) {
    let adj = net.undirected();
    let sets: Vec<HashSet<usize>> = adj.iter().map(|e| e.iter().map(|&(t, _)| t).collect()).collect();
    let mut triangles = vec![0usize; adj.len()];
    let mut coefficient = vec![0.0; adj.len()];
    for v in 0..adj.len() {
        let mut links = 0;
        for &(a, _) in &adj[v] {
            for &(b, _) in &adj[a] {
                if b != v && sets[v].contains(&b) {
                    links += 1;
                }
            }
        }
        triangles[v] = links / 2;
        let d = adj[v].len();
        if d > 1 {
            coefficient[v] = links as f64 / (d * (d - 1)) as f64;
        }
    }
    (triangles, coefficient)
}

/// `core_number`: the largest k for which the node is in a subgraph where
/// every node has at least k neighbors (Batagelj and Zaversnik).
pub fn core_number(net: &Network) -> Vec<usize> {
    let adj = net.undirected();
    let n = adj.len();
    let mut degree: Vec<usize> = adj.iter().map(Vec::len).collect();
    let max = degree.iter().copied().max().unwrap_or(0);
    let mut bins: Vec<Vec<usize>> = vec![Vec::new(); max + 1];
    for v in 0..n {
        bins[degree[v]].push(v);
    }
    let mut core = vec![0; n];
    let mut done = vec![false; n];
    let mut d = 0;
    let mut remaining = n;
    while remaining > 0 {
        let Some(v) = bins.get_mut(d).and_then(Vec::pop) else {
            d += 1;
            continue;
        };
        if done[v] || degree[v] != d {
            continue;
        }
        done[v] = true;
        core[v] = d;
        remaining -= 1;
        for &(w, _) in &adj[v] {
            if !done[w] && degree[w] > d {
                degree[w] -= 1;
                bins[degree[w]].push(w);
            }
        }
        // A neighbor may now sit in a lower bin than `d`; it still belongs
        // to this core, so keep `d`.
    }
    core
}

/// What `khonsu analyze summary` reports.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Summary {
    pub nodes: usize,
    pub relationships: usize,
    /// Relationships over the `n(n-1)` a directed graph could hold.
    pub density: f64,
    pub self_loops: usize,
    pub weak_components: usize,
    pub largest_weak_component: usize,
    pub strong_components: usize,
    pub isolated_nodes: usize,
    pub average_degree: f64,
    pub average_clustering: f64,
    pub max_core: usize,
    /// The longest shortest path in the largest weak component, every
    /// relationship read both ways.
    pub diameter: usize,
    /// True when `diameter` is a lower bound from repeated sweeps rather
    /// than every pair: the component is too large to walk from each node.
    pub diameter_estimated: bool,
}

/// Largest component walked from every node for an exact diameter.
const EXACT_DIAMETER: usize = 2_000;

pub fn summary(net: &Network) -> Summary {
    let n = net.node_count();
    let m = net.edge_count();
    let weak = weak_components(net);
    let strong = strong_components(net);
    let (_, coefficient) = clustering(net);
    let self_loops = (0..n).map(|v| net.out_edges(v).iter().filter(|&&(t, _)| t == v).count()).sum();
    let isolated = (0..n).filter(|&v| net.out_edges(v).is_empty() && net.in_edges(v).is_empty()).count();
    let largest: &[usize] = weak.groups.first().map(Vec::as_slice).unwrap_or(&[]);
    let adj = net.undirected();
    let (diameter, estimated) = diameter(&adj, largest);
    Summary {
        nodes: n,
        relationships: m,
        density: if n > 1 { m as f64 / (n * (n - 1)) as f64 } else { 0.0 },
        self_loops,
        weak_components: weak.groups.len(),
        largest_weak_component: largest.len(),
        strong_components: strong.groups.len(),
        isolated_nodes: isolated,
        average_degree: if n > 0 { 2.0 * m as f64 / n as f64 } else { 0.0 },
        average_clustering: if n > 0 { coefficient.iter().sum::<f64>() / n as f64 } else { 0.0 },
        max_core: core_number(net).into_iter().max().unwrap_or(0),
        diameter,
        diameter_estimated: estimated,
    }
}

fn diameter(adj: &Adjacency, component: &[usize]) -> (usize, bool) {
    let eccentricity = |from: usize| -> (usize, usize) {
        let dist = distances(adj, from, false);
        component
            .iter()
            .map(|&v| (dist[v] as usize, v))
            .max()
            .unwrap_or((0, from))
    };
    if component.len() <= EXACT_DIAMETER {
        return (component.iter().map(|&v| eccentricity(v).0).max().unwrap_or(0), false);
    }
    // Double sweeps from a few starts: each finds a far pair; the longest is
    // a lower bound, usually the diameter itself.
    let mut best = 0;
    for &start in component.iter().step_by((component.len() / 4).max(1)).take(4) {
        let (_, far) = eccentricity(start);
        best = best.max(eccentricity(far).0);
    }
    (best, true)
}

/// `count` node indexes spread evenly over `0..n`, or all of them.
fn spread(n: usize, count: Option<usize>) -> Vec<usize> {
    match count {
        Some(k) if k > 0 && k < n => (0..k).map(|i| i * n / k).collect(),
        _ => (0..n).collect(),
    }
}

#[derive(Copy, Clone, PartialEq)]
struct State {
    cost: f64,
    node: usize,
}

impl Eq for State {}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        // Smallest cost first.
        other.cost.partial_cmp(&self.cost).unwrap_or(Ordering::Equal).then(self.node.cmp(&other.node))
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    //! Expected values are networkx 3.x's for the same graphs.
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-4
    }

    /// Zachary's karate club, networkx's `karate_club_graph`, undirected.
    fn karate() -> Network {
        const EDGES: &[(i64, i64)] = &[
            (0, 1), (0, 2), (0, 3), (0, 4), (0, 5), (0, 6), (0, 7), (0, 8), (0, 10), (0, 11), (0, 12), (0, 13), (0, 17),
            (0, 19), (0, 21), (0, 31), (1, 2), (1, 3), (1, 7), (1, 13), (1, 17), (1, 19), (1, 21), (1, 30), (2, 3),
            (2, 7), (2, 8), (2, 9), (2, 13), (2, 27), (2, 28), (2, 32), (3, 7), (3, 12), (3, 13), (4, 6), (4, 10),
            (5, 6), (5, 10), (5, 16), (6, 16), (8, 30), (8, 32), (8, 33), (9, 33), (13, 33), (14, 32), (14, 33),
            (15, 32), (15, 33), (18, 32), (18, 33), (19, 33), (20, 32), (20, 33), (22, 32), (22, 33), (23, 25),
            (23, 27), (23, 29), (23, 32), (23, 33), (24, 25), (24, 27), (24, 31), (25, 31), (26, 29), (26, 33),
            (27, 33), (28, 31), (28, 33), (29, 32), (29, 33), (30, 32), (30, 33), (31, 32), (31, 33), (32, 33),
        ];
        let mut net = Network::new();
        for v in 0..34 {
            net.add_node(v);
        }
        for &(a, b) in EDGES {
            net.add_edge(a, b, 1.0);
        }
        net
    }

    #[test]
    fn karate_pagerank_matches_networkx() {
        let net = karate();
        let (pr, converged) = pagerank(&net, 0.85, false, 100, 1e-6);
        assert!(converged);
        // nx.pagerank(nx.karate_club_graph(), weight=None)
        assert!(close(pr[33], 0.10091), "{}", pr[33]);
        assert!(close(pr[0], 0.09700), "{}", pr[0]);
        assert!(close(pr.iter().sum::<f64>(), 1.0));
    }

    #[test]
    fn karate_betweenness_matches_networkx() {
        let b = betweenness(&karate(), false, false, None);
        // nx.betweenness_centrality(nx.karate_club_graph())
        assert!(close(b[0], 0.43763), "{}", b[0]);
        assert!(close(b[33], 0.30407), "{}", b[33]);
        assert!(close(b[11], 0.0));
    }

    #[test]
    fn karate_closeness_and_clustering_match_networkx() {
        let net = karate();
        let c = closeness(&net, false, false);
        assert!(close(c[0], 0.56897), "{}", c[0]);
        let (triangles, coefficient) = clustering(&net);
        assert_eq!(triangles[0], 18);
        assert!(close(coefficient[0], 0.15), "{}", coefficient[0]);
        assert!(close(coefficient.iter().sum::<f64>() / 34.0, 0.57064));
        assert_eq!(core_number(&net).into_iter().max(), Some(4));
    }

    #[test]
    fn karate_louvain_finds_its_communities() {
        let (partition, q) = louvain(&karate(), 1.0, 42);
        // networkx's louvain lands between 0.41 and 0.42 depending on seed.
        assert!(q > 0.40 && q < 0.43, "{q}");
        assert!((3..=5).contains(&partition.groups.len()), "{}", partition.groups.len());
        // The two leaders end up apart.
        assert_ne!(partition.of[0], partition.of[33]);
    }

    #[test]
    fn components_split_by_direction() {
        // 1 → 2 → 3 → 1 is one strong component; 4 → 1 hangs off it; 5 – 6 apart.
        let net = Network::from_pairs(&[(1, 2), (2, 3), (3, 1), (4, 1), (5, 6)]);
        let weak = weak_components(&net);
        assert_eq!(weak.groups.iter().map(Vec::len).collect::<Vec<_>>(), vec![4, 2]);
        let strong = strong_components(&net);
        assert_eq!(strong.groups.len(), 4);
        assert_eq!(strong.groups[0].len(), 3);
    }

    #[test]
    fn directed_paths_follow_direction_and_weights() {
        let mut net = Network::new();
        net.add_edge(1, 2, 1.0);
        net.add_edge(2, 3, 1.0);
        net.add_edge(1, 3, 5.0);
        let (a, c) = (net.index_of(1).unwrap(), net.index_of(3).unwrap());
        let (path, len) = shortest_path(&net, a, c, true, true).unwrap();
        assert_eq!(path.iter().map(|&v| net.id(v)).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(len, 2.0);
        let (hops, _) = shortest_path(&net, a, c, true, false).unwrap();
        assert_eq!(hops.len(), 2);
        assert!(shortest_path(&net, c, a, true, false).is_none());
        assert!(shortest_path(&net, c, a, false, false).is_some());
    }

    #[test]
    fn directed_pagerank_sends_dangling_mass_everywhere() {
        // networkx: nx.pagerank(nx.DiGraph([(1, 2), (2, 3)]))
        let net = Network::from_pairs(&[(1, 2), (2, 3)]);
        let (pr, _) = pagerank(&net, 0.85, true, 100, 1e-6);
        assert!(close(pr[0], 0.18442), "{}", pr[0]);
        assert!(close(pr[2], 0.47441), "{}", pr[2]);
    }

    #[test]
    fn summary_of_a_path() {
        let s = summary(&Network::from_pairs(&[(1, 2), (2, 3), (3, 4)]));
        assert_eq!((s.nodes, s.relationships, s.diameter, s.weak_components, s.strong_components), (4, 3, 3, 1, 4));
        assert!(!s.diameter_estimated);
    }
}
