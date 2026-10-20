# Search evaluation

Knode searches a space three ways: by words (SQLite FTS5), by meaning (the local
multilingual-e5-small model with sqlite-vec) and hybrid, which fuses both lists with reciprocal
rank fusion (`1 / (60 + rank)`, up to 50 results from each list). This page measures how well
each mode finds the right note.

## The set

- **Corpus**: 160 short synthetic notes in English and Spanish, the kind a second brain holds:
  project decisions, meeting notes, bug write-ups, code snippets, recipes, reading highlights,
  health, money, travel and home. No real user data. See
  [`core/crates/knode-embed/eval/notes.txt`](../core/crates/knode-embed/eval/notes.txt).
- **Queries**: 65 labelled queries with the notes that answer them, in
  [`core/crates/knode-embed/eval/queries.txt`](../core/crates/knode-embed/eval/queries.txt):
  - `exact` (15): ids, names and commands, such as `BUG-431`, `Northwind` or `git reset --soft`.
  - `paraphrase` (20): the same language as the note, different words.
  - `cross-language` (18): an English query for a Spanish note, or the other way round.
  - `typo` (12): misspelled words, such as `paela valenciana`.
- **Metrics**, on the top 10 results: MRR@10 (how high the first right note is), Recall@5 (how
  many right notes are in the first five) and nDCG@10 (both together).

## Run it

```sh
cd core
cargo run -p knode-embed --release --features accelerate --example search-eval
cargo run -p knode-embed --release --example search-eval -- --details   # one line per query
```

The first run downloads the model (about 490 MB) into `$KNODE_HOME/models`; set `KNODE_HOME`
to a temporary folder to keep it apart from the app's copy.

## Results

| Queries | Mode | MRR@10 | Recall@5 | nDCG@10 |
|---|---|---|---|---|
| all (65) | words | 0.231 | 0.223 | 0.225 |
| all (65) | meaning | 0.811 | 0.879 | 0.837 |
| all (65) | **hybrid** | **0.854** | **0.926** | **0.874** |
| exact (15) | words | 1.000 | 0.967 | 0.974 |
| exact (15) | meaning | 0.817 | 0.800 | 0.842 |
| exact (15) | **hybrid** | **1.000** | **1.000** | **1.000** |
| paraphrase (20) | words | 0.000 | 0.000 | 0.000 |
| paraphrase (20) | meaning | 0.912 | 0.983 | 0.927 |
| paraphrase (20) | hybrid | 0.912 | 0.983 | 0.927 |
| cross-language (18) | words | 0.000 | 0.000 | 0.000 |
| cross-language (18) | meaning | 0.569 | 0.750 | 0.625 |
| cross-language (18) | hybrid | 0.569 | 0.750 | 0.625 |
| typo (12) | words | 0.000 | 0.000 | 0.000 |
| typo (12) | meaning | 1.000 | 1.000 | 1.000 |
| typo (12) | hybrid | 1.000 | 1.000 | 1.000 |

Hybrid is better than either mode alone overall, and never worse than the better of the two
for any kind of query:

- **Exact terms** are where words win and meaning slips: `ADR-007` is not in meaning's top 10,
  and `BUG-431` and `BUG-478` come eighth. Hybrid keeps every one of them first.
- **Paraphrases, other languages and typos** are where words find nothing, because a note
  must contain every word of the query. Hybrid then ranks exactly as meaning does.
- **Cross-language** is the hardest kind. Two queries are not found in the top 10 by any
  mode (`which sign-in method did we choose`, `how do we keep backups safe`), and a few come
  in at eighth or ninth.

## Limits

- The corpus is small and every note fits in one chunk, so it is easier than a real space of
  thousands of long notes. The numbers say how the modes compare, not how a given space will
  do.
- The queries were written by the same author as the notes, which tends to favour them.
