# Memory workflow evaluation

This is a reproducible fixture and offline results validator, **not measured token savings**.
It never calls a model, estimates tokens from characters, or reads a personal vault.

## Run a fair comparison

Use an authorized runner with a fixed exact model/version, identical task prompts, source corpus,
execution rules and success criteria. Keep model settings and context/cache treatment in an external
run manifest. Start each independent repetition from a fresh agent session and reset writable
memory to the same initial corpus. For the checkpoint task, include the second session's usage.

Evaluate all of these arms, randomizing their order across repetitions:

- `files-full`: all corpus notes loaded at the beginning.
- `files-selective`: a capable file-search/read workflow, loading only relevant files.
- `knode`: the same corpus imported into an isolated space, retrieved through MCP.
- `knode-bounded`: the same setup with read_excerpt when the companion implementation is available.

Import with `knode --space <temporary-space> import <corpus>`. Do not use a personal default space
or enable capture. No paid model runs are authorized merely by running this validator. The fixture
contains only synthetic data. Its project identities deliberately overlap in subject matter, and
an old decision is explicitly superseded. Grade against tasks.json, including contradictory and
missing evidence, before examining token outcomes. Keep the actual answer/tool trace as evidence.

The tiny corpus tests correctness and the measurement pipeline; it cannot establish large-vault
savings. For scaling, version a larger synthetic corpus (and its distractors) and compute its new
hash; never quietly change inputs between arms. Measure setup/import/indexing time and resources
separately from per-task retrieval/work/checkpoint usage.

## Normalized results

Create JSON with schema_version=1, exact `model`, `corpus_sha256` from `evaluate.corpus_hash`, and
`runs`. Each run needs `arm`, `task` (from tasks.json), `trial` (same repetition key across arms),
`scope: "end-to-end"`, `includes_memory_writes: true`, independently graded `success` and an
`evidence` reference. Optional measured fields: latency_ms, tool_calls, returned_chars.

`usage` is null when unavailable, otherwise it contains actual provider input_tokens and
output_tokens, plus cached_input_tokens and reasoning_output_tokens when available. Normalize
input to include cached tokens and output to include reasoning tokens; do not add those subsets
again. Aggregate all turns, searches, instructions, compaction and memory-writing work, including
additional sessions required by the task. Record collection limitations in the run manifest.
Do not submit fixtures/test arithmetic as provider measurements. Read usage from provider/client
records; asking a model to estimate its own usage is not a measurement.

Optional `prices` requires currency=USD, as_of, source, input_per_million,
cached_input_per_million and output_per_million. Supply dated prices yourself; no price is built
in. Unknown cache counts leave cost unknown. Estimated provider cost is not subscription billing.

```sh
cd evaluation/memory
python3 -m unittest -v
python3 -c 'from pathlib import Path; import evaluate; print(evaluate.corpus_hash(Path("corpus")))'
python3 evaluate.py /path/to/authorized-normalized-results.json
```

The report separates missing values from zero, displays success counts alongside usage, and
compares only matching task/trial pairs. Positive tokens_saved means the candidate used fewer
input+output tokens; negative values are regressions. Bootstrap intervals are exploratory and
need enough independent repetitions; repeated runs on one tiny task are not broad evidence.
Report every task, failures, overhead and no-benefit cases. Inspect per-task raw records: pooled
means can conceal a regression. Do not claim savings until real comparable runs exist.

Stored memory remains local, but retrieved text goes to the model provider when used by an agent.
