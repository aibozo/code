use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Node {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub props: Json,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub src: String,
    pub rel: String,
    pub dst: String,
    #[serde(default)]
    pub props: Json,
}

pub trait GraphStore: Send + Sync {
    fn put_node(&self, node: &Node) -> std::io::Result<()>;
    fn put_edge(&self, edge: &Edge) -> std::io::Result<()>;
    fn get_node(&self, id: &str) -> std::io::Result<Option<Node>>;
    fn neighbors(&self, id: &str, rel: Option<&str>) -> std::io::Result<Vec<Edge>>;
}

/// File-backed graph using JSONL for nodes and edges.
pub struct FileGraph {
    nodes_path: PathBuf,
    edges_path: PathBuf,
}

impl FileGraph {
    pub fn new(home: &Path) -> std::io::Result<Self> {
        let base = home.join("graph");
        std::fs::create_dir_all(&base)?;
        let nodes_path = base.join("nodes.jsonl");
        let edges_path = base.join("edges.jsonl");
        Ok(Self { nodes_path, edges_path })
    }

    fn append_jsonl<T: Serialize>(path: &Path, rec: &T) -> std::io::Result<()> {
        let mut opts = OpenOptions::new();
        opts.create(true).append(true).read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        let mut line = serde_json::to_string(rec)
            .map_err(|e| std::io::Error::other(format!("serialize failed: {e}")))?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.flush()?;
        fs2::FileExt::unlock(&file)?;
        Ok(())
    }
}

impl GraphStore for FileGraph {
    fn put_node(&self, node: &Node) -> std::io::Result<()> {
        Self::append_jsonl(&self.nodes_path, node)
    }

    fn put_edge(&self, edge: &Edge) -> std::io::Result<()> {
        Self::append_jsonl(&self.edges_path, edge)
    }

    fn get_node(&self, id: &str) -> std::io::Result<Option<Node>> {
        let file = match OpenOptions::new().read(true).open(&self.nodes_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        fs2::FileExt::lock_shared(&file)?;
        let reader = BufReader::new(&file);
        for line in reader.lines() {
            let Ok(s) = line else { continue };
            if let Ok(node) = serde_json::from_str::<Node>(&s) {
                if node.id == id {
                    fs2::FileExt::unlock(&file)?;
                    return Ok(Some(node));
                }
            }
        }
        fs2::FileExt::unlock(&file)?;
        Ok(None)
    }

    fn neighbors(&self, id: &str, rel: Option<&str>) -> std::io::Result<Vec<Edge>> {
        let file = match OpenOptions::new().read(true).open(&self.edges_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        fs2::FileExt::lock_shared(&file)?;
        let reader = BufReader::new(&file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let Ok(s) = line else { continue };
            if let Ok(edge) = serde_json::from_str::<Edge>(&s) {
                if (edge.src == id || edge.dst == id)
                    && rel.map(|r| r == edge.rel.as_str()).unwrap_or(true)
                {
                    out.push(edge);
                }
            }
        }
        fs2::FileExt::unlock(&file)?;
        Ok(out)
    }
}

pub mod ingest;
pub mod retrieve;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn file_graph_round_trip() {
        let dir = tempdir().unwrap();
        let g = FileGraph::new(dir.path()).unwrap();
        let n = Node { kind: "Episode".into(), id: "20250101-000000".into(), props: Json::Null };
        g.put_node(&n).unwrap();
        let got = g.get_node(&n.id).unwrap();
        assert_eq!(Some(n), got);
        let e = Edge { src: "A".into(), rel: "contains".into(), dst: "B".into(), props: Json::Null };
        g.put_edge(&e).unwrap();
        let neigh = g.neighbors("A", Some("contains")).unwrap();
        assert!(!neigh.is_empty());
    }
}
