//! Measures search quality on a labelled corpus with the default model: words, meaning and
//! hybrid, scored with MRR@10, Recall@5 and nDCG@10, overall and per kind of query.
//!
//! The corpus (`eval/notes.txt`) is about 160 synthetic notes in English and Spanish, and
//! `eval/queries.txt` holds the labelled queries. Downloads about 490 MB on first run into
//! `$KNODE_HOME/models`.
//!
//! ```sh
//! cargo run -p knode-embed --release --features accelerate --example search-eval
//! cargo run -p knode-embed --release --example search-eval -- --details
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::ControlFlow;
use std::time::Instant;

use knode_embed::{Embedder, Indexer, ModelSpec, Progress, ensure_model};
use knode_format::{NewNode, NodeId, Provenance, SearchMode, Space};

const NOTES: &str = include_str!("../eval/notes.txt");
const QUERIES: &str = include_str!("../eval/queries.txt");
const MODES: [(SearchMode, &str); 3] = [
    (SearchMode::Words, "words"),
    (SearchMode::Meaning, "meaning"),
    (SearchMode::Hybrid, "hybrid"),
];
const LIMIT: usize = 10;

struct Query {
    kind: String,
    relevant: HashSet<String>,
    text: String,
}

/// Lines that are not blank or comments, split on `|` into `n` trimmed fields.
fn records(text: &str, n: usize) -> impl Iterator<Item = Vec<String>> + '_ {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(move |l| {
            let fields: Vec<String> = l.splitn(n, '|').map(|f| f.trim().to_string()).collect();
            assert_eq!(fields.len(), n, "malformed line: {l}");
            fields
        })
}

#[derive(Default, Clone, Copy)]
struct Scores {
    mrr: f64,
    recall: f64,
    ndcg: f64,
    count: usize,
}

impl Scores {
    fn add(&mut self, ranked: &[String], relevant: &HashSet<String>) {
        let first = ranked.iter().take(10).position(|id| relevant.contains(id));
        self.mrr += first.map_or(0.0, |i| 1.0 / (i as f64 + 1.0));
        let found = ranked
            .iter()
            .take(5)
            .filter(|id| relevant.contains(*id))
            .count();
        self.recall += found as f64 / relevant.len() as f64;
        let dcg: f64 = ranked
            .iter()
            .take(10)
            .enumerate()
            .filter(|(_, id)| relevant.contains(*id))
            .map(|(i, _)| 1.0 / (i as f64 + 2.0).log2())
            .sum();
        let ideal: f64 = (0..relevant.len().min(10))
            .map(|i| 1.0 / (i as f64 + 2.0).log2())
            .sum();
        self.ndcg += dcg / ideal;
        self.count += 1;
    }

    fn row(&self) -> [f64; 3] {
        let n = self.count.max(1) as f64;
        [self.mrr / n, self.recall / n, self.ndcg / n]
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let details = std::env::args().any(|a| a == "--details");
    let dir = ensure_model(&ModelSpec::default_model(), |p| {
        if let Progress::Started { file, total } = p {
            eprintln!("downloading {file} ({total} bytes)");
        }
    })?;
    let embedder = Embedder::load(&dir)?;

    let tmp = tempfile::tempdir()?;
    let mut space = Space::create(tmp.path().join("Eval.knode"), "Eval")?;
    let by = Provenance::person("Evaluation");
    let mut ids: HashMap<NodeId, String> = HashMap::new();
    for f in records(NOTES, 3) {
        let node = space.create_node(NewNode::note(&f[1], &f[2]), &by)?;
        ids.insert(node.id, f[0].clone());
    }
    let queries: Vec<Query> = records(QUERIES, 3)
        .map(|f| Query {
            kind: f[0].clone(),
            relevant: f[1].split(',').map(|s| s.trim().to_string()).collect(),
            text: f[2].clone(),
        })
        .collect();
    for q in &queries {
        for id in &q.relevant {
            assert!(
                ids.values().any(|v| v == id),
                "unknown note {id} in {:?}",
                q.text
            );
        }
    }

    let started = Instant::now();
    let report = Indexer::new(&embedder).run(&mut space, |_| ControlFlow::Continue(()))?;
    eprintln!(
        "indexed {} notes ({} chunks) in {:.1?}",
        report.stored,
        report.chunks,
        started.elapsed()
    );

    // (mode, kind) -> scores; kind "all" holds every query.
    let mut table: BTreeMap<(usize, String), Scores> = BTreeMap::new();
    for q in &queries {
        let vector = embedder.embed_query(&q.text)?;
        let mut line = format!("{:<15} {:<52}", q.kind, q.text);
        for (m, (mode, name)) in MODES.iter().enumerate() {
            let results = space.hybrid_search(&q.text, Some(&vector), *mode, LIMIT)?;
            assert_eq!(results.mode, *mode, "{name} search fell back");
            let ranked: Vec<String> = results
                .hits
                .iter()
                .map(|h| ids[&h.node.id].clone())
                .collect();
            for kind in ["all", q.kind.as_str()] {
                table
                    .entry((m, kind.to_string()))
                    .or_default()
                    .add(&ranked, &q.relevant);
            }
            let rank = ranked.iter().position(|id| q.relevant.contains(id));
            line.push_str(&format!(
                " {name}={}",
                rank.map_or("-".to_string(), |r| (r + 1).to_string())
            ));
        }
        if details {
            println!("{line}");
        }
    }

    let mut kinds: Vec<String> = vec!["all".into()];
    for q in &queries {
        if !kinds.contains(&q.kind) {
            kinds.push(q.kind.clone());
        }
    }
    println!(
        "\n{} notes, {} queries, model {}\n",
        ids.len(),
        queries.len(),
        embedder.model_id()
    );
    println!("| Queries | Mode | MRR@10 | Recall@5 | nDCG@10 |");
    println!("|---|---|---|---|---|");
    for kind in &kinds {
        for (m, (_, name)) in MODES.iter().enumerate() {
            let s = table[&(m, kind.clone())];
            let [mrr, recall, ndcg] = s.row();
            let label = if kind == "all" {
                format!("all ({})", s.count)
            } else {
                format!("{kind} ({})", s.count)
            };
            println!("| {label} | {name} | {mrr:.3} | {recall:.3} | {ndcg:.3} |");
        }
    }
    Ok(())
}
