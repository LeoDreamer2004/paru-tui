use super::catalog::Package;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Installed runtime dependencies, resolved by libalpm (including versioned provides).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Graph {
    pub edges: Vec<Vec<usize>>,
    pub roots: Vec<usize>,
}
impl Graph {
    pub fn installed(alpm: &alpm::Alpm, packages: &[Package]) -> Self {
        let indices: HashMap<_, _> = packages
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.as_str(), i))
            .collect();
        let local = alpm.localdb().pkgs();
        let mut resolved = HashMap::new();
        let edges = packages
            .iter()
            .map(|p| {
                let mut children: Vec<_> = p
                    .dependencies
                    .iter()
                    .filter_map(|dep| {
                        *resolved.entry(dep.as_str()).or_insert_with(|| {
                            local
                                .find_satisfier(dep.as_str())
                                .and_then(|p| indices.get(p.name()).copied())
                        })
                    })
                    .collect();
                children.sort_unstable();
                children.dedup();
                children
            })
            .collect();
        Self::new(edges)
    }

    pub fn new(edges: Vec<Vec<usize>>) -> Self {
        // Iterative Kosaraju: dependencies may contain cycles. Each source SCC
        // contributes its alphabetically first package, so no rootless group vanishes.
        let mut reverse = vec![vec![]; edges.len()];
        for (p, children) in edges.iter().enumerate() {
            for &child in children {
                reverse[child].push(p);
            }
        }
        let mut seen = vec![false; edges.len()];
        let mut order = Vec::with_capacity(edges.len());
        for root in 0..edges.len() {
            if seen[root] {
                continue;
            }
            seen[root] = true;
            let mut stack = vec![(root, 0)];
            while let Some((node, next)) = stack.last_mut() {
                if let Some(&child) = edges[*node].get(*next) {
                    *next += 1;
                    if !seen[child] {
                        seen[child] = true;
                        stack.push((child, 0));
                    }
                } else {
                    order.push(*node);
                    stack.pop();
                }
            }
        }
        let mut component = vec![usize::MAX; edges.len()];
        let mut representatives = vec![];
        for &root in order.iter().rev() {
            if component[root] != usize::MAX {
                continue;
            }
            let id = representatives.len();
            let mut representative = root;
            component[root] = id;
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                representative = representative.min(node);
                for &parent in &reverse[node] {
                    if component[parent] == usize::MAX {
                        component[parent] = id;
                        stack.push(parent);
                    }
                }
            }
            representatives.push(representative);
        }
        let mut incoming = vec![false; representatives.len()];
        for (p, children) in edges.iter().enumerate() {
            for &child in children {
                if component[p] != component[child] {
                    incoming[component[child]] = true;
                }
            }
        }
        let mut roots: Vec<_> = representatives
            .into_iter()
            .enumerate()
            .filter_map(|(i, p)| (!incoming[i]).then_some(p))
            .collect();
        roots.sort_unstable();
        Self { edges, roots }
    }
}

pub struct Row {
    pub package: usize,
    pub parent: Option<usize>,
    pub cycle: bool,
    pub open: bool,
    pub expandable: bool,
    path: Vec<usize>,
    // Whether each ancestor below the root has another sibling.
    branches: Vec<bool>,
}
impl Row {
    pub fn prefix(&self, max_width: usize) -> String {
        let mut prefix = String::new();
        // Keep the package name readable even after many levels of expansion.
        let capacity = max_width.saturating_sub(2) / 2;
        let skip = if self.branches.len() > capacity {
            self.branches
                .len()
                .saturating_sub(max_width.saturating_sub(4) / 2)
        } else {
            0
        };
        if skip > 0 {
            prefix.push_str("… ");
        }
        for (i, &more) in self.branches.iter().enumerate().skip(skip) {
            prefix.push_str(if i + 1 == self.branches.len() {
                if more { "├─" } else { "└─" }
            } else if more {
                "│ "
            } else {
                "  "
            });
        }
        prefix.push_str(if self.cycle {
            "↩ "
        } else if self.open {
            "▾ "
        } else if self.expandable {
            "▸ "
        } else {
            "  "
        });
        prefix
    }
}

#[derive(Default)]
pub struct Tree {
    pub rows: Vec<Row>,
    expanded: HashSet<Vec<usize>>,
    names: Vec<String>,
}
impl Tree {
    pub fn sync(&mut self, packages: &[Package]) {
        if !self
            .names
            .iter()
            .map(String::as_str)
            .eq(packages.iter().map(|p| p.name.as_str()))
        {
            self.expanded.clear();
            self.names = packages.iter().map(|p| p.name.clone()).collect();
        }
    }
    pub fn rebuild(&mut self, graph: &Graph, roots: &[usize]) {
        self.rows.clear();
        let mut stack: Vec<_> = roots
            .iter()
            .rev()
            .map(|&p| (p, None, vec![], vec![]))
            .collect();
        while let Some((package, parent, mut path, branches)) = stack.pop() {
            let cycle = path.contains(&package);
            path.push(package);
            let children = graph
                .edges
                .get(package)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let expandable = !cycle && !children.is_empty();
            let open = expandable && self.expanded.contains(&path);
            let index = self.rows.len();
            if open {
                for (i, &child) in children.iter().enumerate().rev() {
                    let mut branches = branches.clone();
                    branches.push(i + 1 < children.len());
                    stack.push((child, Some(index), path.clone(), branches));
                }
            }
            self.rows.push(Row {
                package,
                parent,
                cycle,
                open,
                expandable,
                path,
                branches,
            });
        }
    }
    pub fn set_open(&mut self, row: usize, open: bool) {
        if let Some(row) = self.rows.get(row).filter(|r| r.expandable) {
            if open {
                self.expanded.insert(row.path.clone());
            } else {
                self.expanded.remove(&row.path);
            }
        }
    }
}
