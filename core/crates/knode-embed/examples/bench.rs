//! Measures the default model on this machine: download, load time, throughput and memory.
//!
//! ```sh
//! cargo run -p knode-embed --release --example bench
//! cargo run -p knode-embed --release --features accelerate --example bench
//! cargo run -p knode-embed --release --features metal --example bench
//! ```

use std::time::Instant;

use knode_embed::{Embedder, ModelSpec, Progress, chunk, ensure_model};

const SENTENCES: &[&str] = &[
    "We chose a local model so that search by meaning never leaves the device.",
    "La reunión del martes decidió mover la base de datos a SQLite en modo WAL.",
    "Les agents écrivent dans le même espace que la personne, avec la provenance de chaque note.",
    "Die Suche kombiniert Volltext und Vektoren mit reziproker Rangfusion.",
    "The service is the only process that opens a space; everyone else talks to it.",
    "Ideas for the map view: saved positions, typed links and backlinks on hover.",
    "日本語のメモも同じモデルで検索できるようにする。",
    "Remember to benchmark ten thousand nodes before the public beta.",
];

fn passage(words: usize, seed: usize) -> String {
    let mut text = String::new();
    let mut i = seed;
    while text.split_whitespace().count() < words {
        text.push_str(SENTENCES[i % SENTENCES.len()]);
        text.push(' ');
        i += 3;
    }
    text
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = ModelSpec::default_model();
    let started = Instant::now();
    let dir = ensure_model(&spec, |p| match p {
        Progress::Started { file, total } => eprintln!("downloading {file} ({total} bytes)"),
        Progress::Verified { file } => eprintln!("verified {file}"),
        _ => {}
    })?;
    println!("model ready in {:.2?}", started.elapsed());

    let rss_before = rss_mb();
    let started = Instant::now();
    let embedder = Embedder::load(&dir)?;
    println!(
        "load: {:.2?} ({embedder:?}), RSS {rss_before:.0} MB -> {:.0} MB",
        started.elapsed(),
        rss_mb()
    );

    embedder.embed(&["warm up"])?;

    let started = Instant::now();
    let runs = 20;
    for i in 0..runs {
        embedder.embed_query(SENTENCES[i % SENTENCES.len()])?;
    }
    println!(
        "query: {:.1} ms each",
        started.elapsed().as_secs_f64() * 1000.0 / runs as f64
    );

    for words in [32, 128, 320] {
        let texts: Vec<String> = (0..64).map(|i| passage(words, i)).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let tokens = refs.iter().map(|t| embedder.count_tokens(t)).sum::<usize>() / refs.len();
        let started = Instant::now();
        let vectors = embedder.embed(&refs)?;
        let secs = started.elapsed().as_secs_f64();
        assert_eq!(vectors.len(), refs.len());
        println!(
            "passages of ~{tokens} tokens: {:.1} per second ({} in {secs:.2} s), RSS {:.0} MB",
            refs.len() as f64 / secs,
            refs.len(),
            rss_mb()
        );
    }

    let note = (0..30)
        .map(|i| format!("## Section {i}\n\n{}\n", passage(120, i)))
        .collect::<String>();
    let started = Instant::now();
    let chunks = chunk(&note);
    let tokens = chunks
        .iter()
        .map(|c| embedder.count_tokens(&c.text))
        .max()
        .unwrap_or(0);
    println!(
        "chunking a {} KB note: {} chunks in {:.2?}, the longest {tokens} tokens",
        note.len() / 1024,
        chunks.len(),
        started.elapsed()
    );
    Ok(())
}
