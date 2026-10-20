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

## A real space

The same three modes on a real, private space, measured with
[`examples/space-eval.rs`](../core/crates/knode-embed/examples/space-eval.rs) (#147). Only the
numbers are published: the notes and the queries stay on the owner's machine.

```sh
cd core
cargo run -p knode-embed --release --features accelerate --example space-eval -- \
    ~/Knode/Work.knode --queries ~/private/queries.txt --details --words-any
```

The space is copied first, so the one given is never changed. A query line is
`kind | targets | query`, where targets are node ids, `markdown.path` values or exact titles.

### The space

- An Obsidian vault imported with `knode import --vault`: **2,733 notes**, about a third in
  Spanish and the rest in English. Project notes, decisions and session logs, plus the README and
  docs of code repositories kept in the vault.
- Long notes: median 400 words, mean 810, the longest 90% under 1,906 words, the longest 24,462.
  **20,372 chunks**, 7.5 per note.
- 42 labelled queries written by the owner of the notes, each with the one note that answers it:
  10 `exact`, 12 `paraphrase` (Spanish), 12 `cross-language` (English for Spanish notes and the
  other way round), 8 `typo`.

Measured on an Apple M5 Mac with `--features accelerate`, model multilingual-e5-small.

### Indexing and memory

| | |
|---|---|
| Import (`knode import`, words only) | 2.9 s |
| Indexing by meaning, from nothing | **1,127 s (19 min), 2.4 notes/s** |
| Memory: model loaded / after indexing | 844 MB / 1,975 MB |
| `knode.db` before / after indexing | 64 MB / 99 MB |

### Latency

Three runs of each of the 42 queries after a warm-up. Embedding the query takes 6.3 ms (p50),
7.8 ms (p95) and is not in the table.

| Mode | p50 | p95 |
|---|---|---|
| words | 0.19 ms | 1.0 ms |
| meaning | 41 ms | 50 ms |
| hybrid | 41 ms | 52 ms |

Before #174 a search by meaning took **170 s** on this space: SQLite ran the vector search once
for every chunk.

### Quality

| Queries | Mode | MRR@10 | Recall@5 | nDCG@10 |
|---|---|---|---|---|
| all (42) | words | 0.159 | 0.238 | 0.179 |
| all (42) | meaning | 0.588 | 0.762 | 0.648 |
| all (42) | **hybrid** | **0.600** | **0.786** | **0.658** |
| exact (10) | words | 0.583 | 0.800 | 0.639 |
| exact (10) | meaning | 0.553 | 0.800 | 0.636 |
| exact (10) | **hybrid** | **0.633** | **0.800** | **0.676** |
| paraphrase (12) | words | 0.042 | 0.083 | 0.053 |
| paraphrase (12) | meaning | 0.522 | 0.667 | 0.598 |
| paraphrase (12) | hybrid | 0.495 | 0.750 | 0.598 |
| cross-language (12) | words | 0.000 | 0.000 | 0.000 |
| cross-language (12) | meaning | 0.549 | 0.750 | 0.599 |
| cross-language (12) | hybrid | 0.549 | 0.750 | 0.599 |
| typo (8) | words | 0.042 | 0.125 | 0.062 |
| typo (8) | meaning | 0.792 | 0.875 | 0.812 |
| typo (8) | hybrid | 0.792 | 0.875 | 0.812 |

- **Hybrid is still the best mode overall**, as on the synthetic set, but every score is lower:
  MRR@10 0.60 against 0.85. A real space is harder: long notes split into many chunks, and many
  notes about the same project share its words.
- **Cross-language holds up** (0.55, against 0.57 on the synthetic set) even with two thirds of
  the space in English.
- **Exact terms lose their edge**: words find the note first less often (0.58 against 1.00).
  Names of projects appear in many sibling notes, which outrank the one labelled.
- **File names are not searchable**: a query for a note's file name, such as `DEPLOY.md`, finds
  nothing, because `markdown.path` is not in the full-text index.
- Several misses are near-duplicates: a project note and the README of the code repository
  kept next to it say the same thing, and the other one comes first. One right note per query
  undercounts these.

### Matching any word (`--words-any`)

The words list of hybrid search needs every word of the query. Matching any word instead:

| Queries | hybrid | hybrid, any word |
|---|---|---|
| all (42) | **0.600** | 0.536 |
| exact (10) | 0.633 | 0.633 |
| paraphrase (12) | **0.495** | 0.473 |
| cross-language (12) | **0.549** | 0.272 |
| typo (8) | 0.792 | **0.906** |

(MRR@10.) It helps typos, where some words still match, and hurts cross-language queries badly:
common words of the query's language fill the words list with notes in that language. **Keep
every word.**

### What it means for Knode

- Search by meaning was unusable on a real space before #174.
- **Latency**: 41 ms for 20,000 chunks with a brute-force vector search. The v0.9 goal of
  10,000 nodes in under 10 ms needs an approximate index or a smaller vector type (#176).
- **Indexing** at 2.4 notes/s takes about 20 minutes for 2,700 long notes, at about 2 GB of
  memory. Fine in the background, slow the first time (#177).
- **File names** should be found by words: worth indexing the file name of imported notes (#175).
