# Assistant V2

This directory contains the new implementation of the assistant using OpenAI's Assistants API. The goal is to reach feature parity with the original project.

Internet search is supported via DuckDuckGo.

## Features to Port

See `FEATURE_PROGRESS.md` for the migration checklist. Features listed there come from the original README and represent the functionality that should exist in the new version.

## Proof-of-Concept

A basic example demonstrating OpenAI's Assistants API is provided in `src/main.rs`.
To run it make sure the `OPENAI_API_KEY` environment variable is set and execute:

```bash
cargo run --manifest-path assistant_v2/Cargo.toml
```

The program creates a temporary assistant that answers a simple weather query using function calling.
