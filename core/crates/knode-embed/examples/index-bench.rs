//! Measures indexing by meaning on a large synthetic space: notes per second and peak memory.
//!
//! It writes a space of long notes in English and Spanish, made up from a fixed list of
//! sentences (no real data), with lengths spread like a real second brain: median about 400
//! words, mean about 800, a few very long ones. Then it indexes the space from nothing.
//!
//! ```sh
//! cargo run -p knode-embed --release --features accelerate --example index-bench
//! cargo run -p knode-embed --release --features accelerate --example index-bench -- --notes 500
//! ```
//!
//! `--notes <n>` sets the number of notes (3,000 by default) and `--batch <n>` the nodes
//! embedded at once (the indexer's default otherwise).

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use knode_embed::{Embedder, Indexer, ModelSpec, Progress, ensure_model};
use knode_format::{NewNode, Provenance, Space};

const ENGLISH: &[&str] = &[
    "We decided to keep the database local so that search never leaves the device.",
    "The meeting moved the launch to the second week of March, after the security review.",
    "Remember to benchmark ten thousand nodes before the public beta opens.",
    "The bug only shows up when two agents write to the same note within a second.",
    "Ideas for the map view: saved positions, typed links and backlinks on hover.",
    "Next steps are to write the migration, test it on a copy, and then ship it.",
    "The recipe needs two cups of rice, a pinch of saffron and a slow fire.",
    "Reading highlight: the best systems are boring to operate and easy to explain.",
    "Budget for the trip covers flights, three nights in Lisbon and a rental car.",
    "The service is the only process that opens a space; everyone else talks to it.",
    "Run the tests with the release profile, the debug build is far too slow.",
    "Pricing stays free for individuals; teams pay for shared spaces and sync.",
];

const SPANISH: &[&str] = &[
    "La reunión del martes decidió mover la base de datos a SQLite en modo WAL.",
    "Hay que revisar el presupuesto antes de firmar el contrato con el proveedor.",
    "El error aparece cuando dos agentes escriben en la misma nota a la vez.",
    "Para la paella hace falta arroz, azafrán, judías verdes y un fuego lento.",
    "Los próximos pasos son escribir la migración, probarla en una copia y publicarla.",
    "La idea es que cada nota guarde quién la escribió y desde qué sesión.",
    "El viaje a Granada incluye dos noches en el Albaicín y una visita a la Alhambra.",
    "Apunte de lectura: un buen sistema se explica en una página y se opera sin miedo.",
    "La copia de seguridad se hace cada noche y se guarda cifrada fuera de casa.",
    "El servicio es el único proceso que abre un espacio; los demás hablan con él.",
    "Conviene medir el consumo de memoria con un espacio grande antes de la beta.",
    "La búsqueda combina palabras y significado con fusión de rangos recíprocos.",
];

const TOPICS: &[&str] = &[
    "Northwind",
    "Atlas",
    "Borealis",
    "Quartz",
    "Lisbon",
    "Granada",
    "pricing",
    "sync",
    "backup",
    "onboarding",
    "search",
    "release",
];

/// A small deterministic random generator (xorshift64*), so every run writes the same space.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A normal sample (Box-Muller).
    fn normal(&mut self) -> f64 {
        let u = self.unit().max(1e-12);
        let v = self.unit();
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
}

/// One note: a third in Spanish, words drawn from a log-normal with median 400.
fn note(rng: &mut Rng, i: usize) -> (String, String) {
    let spanish = i % 3 == 0;
    let sentences = if spanish { SPANISH } else { ENGLISH };
    let words = (400.0 * (1.15 * rng.normal()).exp()).clamp(20.0, 25_000.0) as usize;
    let topic = TOPICS[rng.below(TOPICS.len())];
    let title = if spanish {
        format!("Notas sobre {topic} {i}")
    } else {
        format!("Notes on {topic} {i}")
    };
    let mut body = String::new();
    let mut count = 0;
    let mut section = 0;
    while count < words {
        if count >= section * 180 {
            section += 1;
            let heading = if spanish { "Sección" } else { "Section" };
            body.push_str(&format!("\n## {heading} {section}\n\n"));
        }
        let sentence = sentences[rng.below(sentences.len())];
        // A number and a topic in every sentence, so no two notes are the same text.
        body.push_str(&format!("{sentence} ({topic} {}) ", rng.below(10_000)));
        count += sentence.split_whitespace().count() + 2;
        if rng.below(4) == 0 {
            body.push_str("\n\n");
        }
    }
    (title, body)
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

fn arg(name: &str) -> Option<usize> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let notes = arg("--notes").unwrap_or(3_000);
    let dir = ensure_model(&ModelSpec::default_model(), |p| {
        if let Progress::Started { file, total } = p {
            eprintln!("downloading {file} ({total} bytes)");
        }
    })?;

    let tmp = tempfile::tempdir()?;
    let mut space = Space::create(tmp.path().join("Bench.knode"), "Bench")?;
    let by = Provenance::person("Benchmark");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut total_words = 0;
    let started = Instant::now();
    for i in 0..notes {
        let (title, body) = note(&mut rng, i);
        total_words += body.split_whitespace().count();
        space.create_node(NewNode::note(title, body), &by)?;
    }
    println!(
        "{notes} notes, {} words on average, written in {:.1?}",
        total_words / notes.max(1),
        started.elapsed()
    );

    // Peak memory, sampled every 100 ms while indexing.
    let peak = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (peak, stop) = (peak.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(rss_mb() as u64, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };

    let rss_start = rss_mb();
    let embedder = Embedder::load(&dir)?;
    let rss_loaded = rss_mb();
    let mut indexer = Indexer::new(&embedder);
    if let Some(batch) = arg("--batch") {
        indexer = indexer.with_batch_size(batch);
    }
    let started = Instant::now();
    let report = indexer.run(&mut space, |_| ControlFlow::Continue(()))?;
    let secs = started.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    sampler.join().expect("sampler");
    assert!(report.complete);
    println!(
        "indexed {} notes ({} chunks) in {secs:.1} s: {:.1} notes/s, {:.1} chunks/s",
        report.stored,
        report.chunks,
        report.stored as f64 / secs,
        report.chunks as f64 / secs
    );
    println!(
        "memory: {rss_start:.0} MB at start, {rss_loaded:.0} MB with the model, peak {} MB, \
         {:.0} MB after indexing",
        peak.load(Ordering::Relaxed),
        rss_mb()
    );
    Ok(())
}
