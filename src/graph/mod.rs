use std::collections::HashMap;
use std::path::PathBuf;

pub struct Graph {
    adjacency_list: HashMap<usize, Vec<usize>>,
    paths: Vec<PathBuf>,
    path_to_index: HashMap<PathBuf, usize>,
}

impl Graph {
    pub fn new() -> Self {
        Graph {
            adjacency_list: HashMap::new(),
            paths: Vec::new(),
            path_to_index: HashMap::new(),
        }
    }

    pub fn add_node(&mut self, path: PathBuf) -> usize {
        if let Some(&index) = self.path_to_index.get(&path) {
            return index;
        }

        let index = self.paths.len();
        self.paths.push(path.clone());
        self.path_to_index.insert(path, index);
        self.adjacency_list.insert(index, Vec::new());
        index
    }

    pub fn add_edge(&mut self, from: usize, to: usize) {
        if let Some(neighbors) = self.adjacency_list.get_mut(&from) {
            neighbors.push(to);
        }
    }

    pub fn to_dot(&self) -> String {
        let mut dot = String::from("digraph Dependencies {\n");
        dot.push_str("  rankdir=LR;\n");
        dot.push_str("  node [shape=box, style=rounded];\n\n");

        // Add nodes with labels
        for (index, path) in self.paths.iter().enumerate() {
            let label = path.to_string_lossy();
            dot.push_str(&format!("  {} [label=\"{}\"];\n", index, label));
        }

        dot.push_str("\n");

        // Add edges
        for (&from, neighbors) in &self.adjacency_list {
            for &to in neighbors {
                dot.push_str(&format!("  {} -> {};\n", from, to));
            }
        }

        dot.push_str("}\n");
        dot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topological_sort() {
        let mut graph = Graph::new();

        let a = graph.add_node(PathBuf::from("a"));
        let b = graph.add_node(PathBuf::from("b"));
        let c = graph.add_node(PathBuf::from("c"));
        let d = graph.add_node(PathBuf::from("d"));

        graph.add_edge(a, b);
        graph.add_edge(b, c);
        graph.add_edge(c, d);

        graph.to_dot();
    }
}
