//! Measures search on a real space: indexing time and memory, latency per mode (p50 and p95),
//! and, with labelled queries, the same quality scores as `search-eval`.
//!
//! The space is copied to a temporary folder first, so the one given is never changed. Queries
//! come from `--queries <file>`, one per line, as `kind | targets | query`: targets are node
//! ids, `markdown.path` values (`Projects/Plan.md`) or exact titles, separated by commas. Keep
//! that file outside the repository when the space is private; only the scores are printed.
//! Without it, the latency runs on a few generic queries.
//!
//! `--words-any` adds an experimental hybrid where the words list matches any word of the
//! query instead of every word, to measure whether that helps natural-language queries.
//!
//! ```sh
//! cargo run -p knode-embed --release --features accelerate --example space-eval -- \
//!     ~/Knode/Work.knode --queries ~/private/work-queries.txt --details --words-any
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use knode_embed::{Embedder, Indexer, ModelSpec, Progress, ensure_model};
use knode_format::{NodeId, SearchMode, Space};

const MODES: [(SearchMode, &str); 3] = [
    (SearchMode::Words, "words"),
    (SearchMode::Meaning, "meaning"),
    (SearchMode::Hybrid, "hybrid"),
];
const LIMIT: usize = 10;
/// The size of each list hybrid search fuses, and its reciprocal rank constant (19.6).
const LIST: usize = 50;
const RRF_K: f64 = 60.0;
/// Each query runs this many times per mode for the latency; the first run is a warm-up.
const RUNS: usize = 4;
const GENERIC_QUERIES: &[&str] = &[
    "meeting notes",
    "what did we decide about the database",
    "next steps",
    "pricing",
    "how to deploy",
    "ideas for the app",
    "bug",
    "notas de la reunión",
];

struct Query {
    kind: String,
    relevant: HashSet<NodeId>,
    text: String,
}

#[derive(Default, Clone, Copy)]
struct Scores {
    mrr: f64,
    recall: f64,
    ndcg: f64,
    count: usize,
}

impl Scores {
    fn add(&mut self, ranked: &[NodeId], relevant: &HashSet<NodeId>) {
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

/// Resident memory of this process, in MB (from `ps`, so macOS and Linux).
fn rss_mb() -> f64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map_or(f64::NAN, |kb| kb / 1024.0)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// Nodes by id, title key and `markdown.path`, to read the targets of the queries.
struct Targets {
    ids: HashSet<NodeId>,
    titles: HashMap<String, Vec<NodeId>>,
    paths: HashMap<String, NodeId>,
}

impl Targets {
    fn load(db: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let conn =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut ids = HashSet::new();
        let mut titles: HashMap<String, Vec<NodeId>> = HashMap::new();
        let mut stmt = conn.prepare("SELECT id, title_key FROM nodes ORDER BY seq")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, key) = row?;
            let id = NodeId::parse(&id)?;
            ids.insert(id);
            titles.entry(key).or_default().push(id);
        }
        let mut paths = HashMap::new();
        let mut stmt =
            conn.prepare("SELECT node_id, value FROM properties WHERE key = 'markdown.path'")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, path) = row?;
            paths.insert(path.to_lowercase(), NodeId::parse(&id)?);
        }
        Ok(Targets { ids, titles, paths })
    }

    fn resolve(&self, target: &str) -> Result<Vec<NodeId>, String> {
        if let Ok(id) = NodeId::parse(target)
            && self.ids.contains(&id)
        {
            return Ok(vec![id]);
        }
        if let Some(id) = self.paths.get(&target.to_lowercase()) {
            return Ok(vec![*id]);
        }
        let key = target
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        match self.titles.get(&key) {
            Some(ids) if ids.len() == 1 => Ok(ids.clone()),
            Some(ids) => Err(format!(
                "{} notes are titled {target:?}: give its path or id",
                ids.len()
            )),
            None => Err(format!(
                "no note is titled {target:?}, nor has that path or id"
            )),
        }
    }
}

fn read_queries(file: &Path, targets: &Targets) -> Result<Vec<Query>, Box<dyn std::error::Error>> {
    let mut queries = Vec::new();
    for line in std::fs::read_to_string(file)?.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.splitn(3, '|').map(str::trim).collect();
        if fields.len() != 3 {
            return Err(format!("expected `kind | targets | query`: {line}").into());
        }
        let mut relevant = HashSet::new();
        for target in fields[1]
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            relevant.extend(
                targets
                    .resolve(target)
                    .map_err(|e| format!("{e} (in {line:?})"))?,
            );
        }
        queries.push(Query {
            kind: fields[0].to_string(),
            relevant,
            text: fields[2].to_string(),
        });
    }
    Ok(queries)
}

/// The words list of the `--words-any` experiment: notes with any word of the query, ranked
/// by FTS5, the last word also as a prefix, as `Space::search` treats it.
fn words_any(db: &rusqlite::Connection, query: &str) -> rusqlite::Result<Vec<NodeId>> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect();
    let Some((last, rest)) = words.split_last() else {
        return Ok(Vec::new());
    };
    let mut terms: Vec<String> = rest.iter().map(|w| format!("\"{w}\"")).collect();
    terms.push(format!("\"{last}\"*"));
    let mut stmt = db.prepare_cached(
        "SELECT n.id FROM nodes_fts JOIN nodes n ON n.seq = nodes_fts.rowid
         WHERE nodes_fts MATCH ?1 ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![terms.join(" OR "), LIST as i64], |r| {
        r.get::<_, String>(0)
    })?;
    let mut ids = Vec::new();
    for id in rows {
        if let Ok(id) = NodeId::parse(&id?) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Reciprocal rank fusion of two lists, ties to the better rank, as hybrid search does.
fn fuse(words: &[NodeId], meaning: &[NodeId]) -> Vec<NodeId> {
    let mut scores: HashMap<NodeId, (f64, usize)> = HashMap::new();
    for list in [words, meaning] {
        for (i, id) in list.iter().enumerate() {
            let entry = scores.entry(*id).or_insert((0.0, usize::MAX));
            entry.0 += 1.0 / (RRF_K + i as f64 + 1.0);
            entry.1 = entry.1.min(i);
        }
    }
    let mut ranked: Vec<(NodeId, (f64, usize))> = scores.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.0
            .total_cmp(&a.1.0)
            .then(a.1.1.cmp(&b.1.1))
            .then(a.0.cmp(&b.0))
    });
    ranked.into_iter().map(|(id, _)| id).take(LIMIT).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let details = args.iter().any(|a| a == "--details");
    let any = args.iter().any(|a| a == "--words-any");
    let queries_file = args
        .iter()
        .position(|a| a == "--queries")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let source = args
        .iter()
        .find(|a| a.ends_with(".knode") || a.ends_with(".knode/"))
        .map(PathBuf::from)
        .ok_or("give the path of a .knode space")?;

    let tmp = tempfile::tempdir()?;
    let copy = tmp.path().join("Eval.knode");
    copy_dir(&source, &copy)?;

    let dir = ensure_model(&ModelSpec::default_model(), |p| {
        if let Progress::Started { file, total } = p {
            eprintln!("downloading {file} ({total} bytes)");
        }
    })?;
    let rss_start = rss_mb();
    let embedder = Embedder::load(&dir)?;
    let mut space = Space::open(&copy)?;
    let targets = Targets::load(&copy.join("knode.db"))?;

    let rss_loaded = rss_mb();
    let started = Instant::now();
    let report = Indexer::new(&embedder).run(&mut space, |_| ControlFlow::Continue(()))?;
    let indexing = started.elapsed();
    let size_mb = std::fs::metadata(copy.join("knode.db"))?.len() as f64 / 1_048_576.0;
    println!(
        "{} notes, {} indexed ({} chunks) in {:.1?}: {:.1} notes/s; model {}",
        targets.ids.len(),
        report.stored,
        report.chunks,
        indexing,
        report.stored as f64 / indexing.as_secs_f64(),
        embedder.model_id()
    );
    println!(
        "memory: {rss_start:.0} MB at start, {rss_loaded:.0} MB with the model, {:.0} MB after \
         indexing; knode.db {size_mb:.1} MB",
        rss_mb()
    );

    let queries = match &queries_file {
        Some(file) => read_queries(file, &targets)?,
        None => GENERIC_QUERIES
            .iter()
            .map(|q| Query {
                kind: "generic".into(),
                relevant: HashSet::new(),
                text: (*q).to_string(),
            })
            .collect(),
    };
    let db = rusqlite::Connection::open_with_flags(
        copy.join("knode.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    let mut latency: Vec<Vec<Duration>> = vec![Vec::new(); MODES.len()];
    let mut embed_times = Vec::new();
    let mut table: BTreeMap<(usize, String), Scores> = BTreeMap::new();
    let names: Vec<&str> = MODES
        .iter()
        .map(|(_, n)| *n)
        .chain(any.then_some("hybrid-any"))
        .collect();
    for q in &queries {
        let mut vector = Vec::new();
        for run in 0..RUNS {
            let started = Instant::now();
            vector = embedder.embed_query(&q.text)?;
            if run > 0 {
                embed_times.push(started.elapsed());
            }
        }
        let mut line = format!("{:<15} {:<52}", q.kind, q.text);
        let mut rankings: Vec<Vec<NodeId>> = Vec::new();
        for (m, (mode, name)) in MODES.iter().enumerate() {
            let mut results = None;
            for run in 0..RUNS {
                let started = Instant::now();
                let r = space.hybrid_search(&q.text, Some(&vector), *mode, LIMIT)?;
                if run > 0 {
                    latency[m].push(started.elapsed());
                }
                results = Some(r);
            }
            let results = results.expect("RUNS > 0");
            assert_eq!(results.mode, *mode, "{name} search fell back");
            rankings.push(results.hits.iter().map(|h| h.node.id).collect());
        }
        if any {
            let meaning = space.hybrid_search(&q.text, Some(&vector), SearchMode::Meaning, LIST)?;
            let meaning: Vec<NodeId> = meaning.hits.iter().map(|h| h.node.id).collect();
            rankings.push(fuse(&words_any(&db, &q.text)?, &meaning));
        }
        if q.relevant.is_empty() {
            continue;
        }
        for (m, ranked) in rankings.iter().enumerate() {
            for kind in ["all", q.kind.as_str()] {
                table
                    .entry((m, kind.to_string()))
                    .or_default()
                    .add(ranked, &q.relevant);
            }
            let rank = ranked.iter().position(|id| q.relevant.contains(id));
            line.push_str(&format!(
                " {}={}",
                names[m],
                rank.map_or("-".to_string(), |r| (r + 1).to_string())
            ));
        }
        if details {
            println!("{line}");
        }
    }

    embed_times.sort();
    println!(
        "\nlatency over {} queries × {} runs (query embedding p50 {:.1?}, p95 {:.1?}):\n",
        queries.len(),
        RUNS - 1,
        percentile(&embed_times, 0.5),
        percentile(&embed_times, 0.95)
    );
    println!("| Mode | p50 | p95 |");
    println!("|---|---|---|");
    for (m, (_, name)) in MODES.iter().enumerate() {
        latency[m].sort();
        println!(
            "| {name} | {:.2?} | {:.2?} |",
            percentile(&latency[m], 0.5),
            percentile(&latency[m], 0.95)
        );
    }

    if table.is_empty() {
        return Ok(());
    }
    let mut kinds: Vec<String> = vec!["all".into()];
    for q in &queries {
        if !kinds.contains(&q.kind) {
            kinds.push(q.kind.clone());
        }
    }
    println!("\n| Queries | Mode | MRR@10 | Recall@5 | nDCG@10 |");
    println!("|---|---|---|---|---|");
    for kind in &kinds {
        for (m, name) in names.iter().enumerate() {
            let Some(s) = table.get(&(m, kind.clone())) else {
                continue;
            };
            let [mrr, recall, ndcg] = s.row();
            println!(
                "| {kind} ({}) | {name} | {mrr:.3} | {recall:.3} | {ndcg:.3} |",
                s.count
            );
        }
    }
    Ok(())
}
