use std::collections::{HashMap, VecDeque};
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

    pub fn topological_sort(&self) -> Option<Vec<PathBuf>> {
        let mut in_degree: HashMap<usize, usize> = HashMap::new();
        let mut queue = VecDeque::new();
        let mut result = Vec::new();

        // Calculate in-degrees
        for &node in self.adjacency_list.keys() {
            in_degree.insert(node, 0);
        }
        for neighbors in self.adjacency_list.values() {
            for &neighbor in neighbors {
                *in_degree.entry(neighbor).or_insert(0) += 1;
            }
        }

        // Find nodes with no incoming edges
        for (&node, &degree) in &in_degree {
            if degree == 0 {
                queue.push_back(node);
            }
        }

        // Process nodes
        while let Some(node) = queue.pop_front() {
            result.push(self.paths[node].clone());

            if let Some(neighbors) = self.adjacency_list.get(&node) {
                for &neighbor in neighbors {
                    if let Some(degree) = in_degree.get_mut(&neighbor) {
                        *degree -= 1;
                        if *degree == 0 {
                            queue.push_back(neighbor);
                        }
                    }
                }
            }
        }

        if result.len() == self.adjacency_list.len() {
            Some(result)
        } else {
            None // Cycle detected
        }
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

        let result = graph.topological_sort();
        assert_eq!(
            result,
            Some(vec![
                PathBuf::from("a"),
                PathBuf::from("b"),
                PathBuf::from("c"),
                PathBuf::from("d")
            ])
        );
    }
}
