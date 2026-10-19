# Speed up the slow test

## User

Why is the test slow?

## Gemini CLI

- `run_shell_command`: `cargo test --release` (failed)
- `read_file`: `/Users/example/project/tests/slow.rs`

It sleeps for two seconds.

## User

Make it fast

## Gemini CLI

Done: the sleep is gone.
