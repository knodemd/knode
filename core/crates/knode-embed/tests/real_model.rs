//! Runs the default model for real. Downloads about 490 MB on first run into
//! `$KNODE_HOME/models`, so it is ignored by default:
//!
//! ```sh
//! cargo test -p knode-embed --release -- --ignored
//! ```

use knode_embed::{Embedder, ModelSpec, ensure_model};

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[test]
#[ignore = "downloads the real model"]
fn the_default_model_finds_by_meaning_across_languages() {
    let spec = ModelSpec::default_model();
    let dir = ensure_model(&spec, |_| {}).expect("model downloads");
    let embedder = Embedder::load(&dir).expect("model loads");
    assert_eq!(embedder.model_id(), "multilingual-e5-small");
    assert_eq!(embedder.dimension(), 384);

    let passages = [
        "To cook rice, rinse it, then simmer one cup with two cups of water for 18 minutes.",
        "Para cocinar arroz, lávalo y hiérvelo a fuego lento con el doble de agua.",
        "The quarterly report shows revenue grew 12 percent while costs stayed flat.",
        "We decided to store embeddings in sqlite-vec next to the full-text index.",
    ];
    let vectors = embedder.embed(&passages).unwrap();
    for vector in &vectors {
        assert_eq!(vector.len(), 384);
        assert!((dot(vector, vector) - 1.0).abs() < 1e-4);
    }

    let scores = |query: &str| {
        let q = embedder.embed_query(query).unwrap();
        vectors.iter().map(|v| dot(&q, v)).collect::<Vec<_>>()
    };

    let rice = scores("how long does rice take to boil?");
    assert!(rice[0] > rice[2] && rice[0] > rice[3], "{rice:?}");
    assert!(rice[1] > rice[2] && rice[1] > rice[3], "{rice:?}");

    let spanish = scores("¿dónde guardamos los vectores?");
    let best = spanish
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap()
        .0;
    assert_eq!(best, 3, "{spanish:?}");
}
