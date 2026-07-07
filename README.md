# Hugr

[![CI](https://github.com/arcbjorn/hugr/actions/workflows/ci.yml/badge.svg)](https://github.com/arcbjorn/hugr/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Hugr is a project memory and intelligence system for coding agents**, built on a local semantic code graph and a temporal, provenance-tracked memory store. It gives an agent a durable, structured understanding of a codebase — what the code does, what changed, what was learned, what failed before, and which decisions still matter — and compiles that understanding into concise, citation-grounded context for the task at hand.

Hugr is local-first: it runs on the developer machine by default, in the cloud, or as a hybrid service near your code. All state lives in a single SQLite-compatible libSQL database, so memory, full-text search, embeddings, and the code graph need no external services.

## Why Hugr

Coding agents start every session from zero. They re-discover the same files, repeat past mistakes, act on stale assumptions, and lose everything they learned when the session ends. Attaching a generic note database does not fix this: the hard problem is not storing text, it is deciding what an agent needs to know *right now* — and proving where each fact came from.

Hugr solves that with three connected systems:

1. **A temporal memory engine** that stores facts in a provenance-tracked knowledge graph with confidence scores and validity windows, consolidates duplicates, and retires contradicted facts by supersession rather than deletion — memory stays auditable and correctable.
2. **A code intelligence engine** that builds a local semantic code graph — AST-derived symbols, typed reference edges, test mappings, and git state across eight languages — incrementally and without an IDE or language server.
3. **A context compiler** that runs hybrid retrieval (full-text, vector, and graph expansion) over both, then emits a ranked, token-budgeted, citation-grounded context pack for a specific task.

## The Flagship Command

```bash
hugr context "add lifecycle hooks to plugins"
```

One command compiles everything an agent needs before touching code:

- relevant memories, with provenance and staleness warnings
- relevant files and symbols, ranked by deterministic evidence scores
- code-graph neighborhoods (callers, callees, imports, implementations)
- affected tests and current branch/worktree state
- structured diagnostics from recent command runs
- deterministic risk signals: stale facts, missing test coverage, high fan-in symbols, public API surfaces, oversized or deeply nested functions, and more
- citations for every item, within a configurable token budget

Output is Markdown for humans or JSON (`--json`) for agents.

## Features

### Durable memory

- `remember`, `recall`, `forget`, `improve` — with source attachment, confidence, sensitivity, and validity metadata on every write.
- Soft retirement: outdated memories are superseded, never silently deleted, and stay auditable.
- Deterministic duplicate and contradiction detection with explicit, opt-in retirement.
- A device-local global scope (`--global`) for cross-project user memories that never sync.
- Secret redaction at the storage boundary before anything can reach sync or LLM synthesis.

### Code intelligence

- Tree-sitter symbol and reference extraction for Rust, Python, TypeScript, JavaScript, Go, Java, Kotlin, and Swift.
- A typed reference graph: imports, calls, member calls, implementations, inheritance, instantiations, and type references.
- Incremental re-indexing of changed paths plus their inbound references; deleted files are pruned everywhere.
- `hugr impact <file-or-symbol>` for blast-radius reports before an edit.

### Safe structural edits

- `replace-symbol`, `rename-symbol`, and `move-symbol` perform parser-validated edits that refuse ambiguous targets and re-index immediately.
- `move-symbol --rewrite-references` rewrites imports and qualified references across files, packages (Go, Java, Kotlin), and modules (Swift), conservatively.

### Session observation

- A local daemon watches file changes, git state, and worktree events for active sessions.
- `hugr run <command>` captures command outcomes and parses structured diagnostics; `hugr shell-hook` observes ordinary shell commands.
- Ended sessions are automatically summarized and promoted into durable memories — episodic observations consolidated into semantic memory — optionally distilled through a local or hosted LLM.

### Hybrid retrieval and embeddings

- Recall fuses FTS5 full-text search, vector similarity, and graph expansion with deterministic reranking — GraphRAG-style retrieval with no LLM in the loop.
- Embedding providers: `deterministic` (offline default), `local` (in-process ONNX, no API key, no sidecar), `ollama`, and `openai`.

### Agent integration

- A stdio MCP server exposing 14 tools, sharing the exact compile pipeline with the CLI.
- `hugr install claude-code|cursor` writes idempotent MCP registration and session hooks in one command.

### Deployment and sync

- Local, hybrid, and remote storage modes with the same schema and commands.
- Opt-in sync of safe data classes to remote libSQL/Turso or a hosted Hugr API; full source, raw output, and secrets never sync without explicit opt-in.
- `hugr eval` scores the context compiler against your own git history (recall, hit rate, MRR), so context quality is measurable — and measured on every push in CI.

## Quick Start

```bash
# Build and install (Rust 1.85+ / edition 2024)
git clone https://github.com/arcbjorn/hugr && cd hugr
cargo install --path .

# Set up a project
cd /path/to/your/project
hugr init
hugr index

# Wire up your agent (Claude Code or Cursor)
hugr install claude-code

# Use it
hugr remember "plugin hooks run after configuration is loaded"
hugr recall "plugin hooks"
hugr context "add lifecycle hooks to plugins"
```

## CLI Overview

| Area | Commands |
| --- | --- |
| Setup | `init`, `status`, `doctor`, `install <agent>` |
| Memory | `remember`, `recall`, `forget`, `improve` |
| Context | `context <task>`, `eval` |
| Code | `index`, `symbols`, `impact` |
| Edits | `replace-symbol`, `rename-symbol`, `move-symbol` |
| Sessions | `session start\|event\|end\|promote`, `run`, `shell-hook` |
| Services | `daemon`, `mcp` |
| Sync | `sync status\|push\|pull\|history` |

Most commands accept `--json`. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full surface with flags.

## Configuration

Everything is configured through environment variables; nothing is required for local use.

| Variable | Purpose |
| --- | --- |
| `HUGR_STORAGE_MODE` | `local` (default), `hybrid`, or `remote` |
| `HUGR_EMBEDDING_PROVIDER` | `deterministic` (default), `local`, `ollama`, or `openai` |
| `HUGR_LOCAL_EMBEDDING_MODEL` | ONNX model for the `local` provider (default `bge-small-en-v1.5`) |
| `HUGR_LLM_PROVIDER` | `ollama` or `openai`, for optional session synthesis |
| `HUGR_CONTEXT_TOKEN_BUDGET` | Default token budget for context packs |
| `HUGR_REMOTE_DATABASE_URL` / `HUGR_REMOTE_AUTH_TOKEN` | Direct remote libSQL/Turso storage |
| `HUGR_API_URL` / `HUGR_API_TOKEN` | Hosted Hugr API backend |
| `HUGR_SYNC_BACKEND` | `direct_libsql` (default) or `hugr_api` |
| `HUGR_SYNC_CLASSES` | Which data classes may sync; unsafe classes require explicit opt-in |
| `HUGR_GLOBAL_DIR` | Location of the global memory store (default `~/.hugr`) |

`hugr doctor` reports the active configuration with secrets redacted.

## Architecture

```text
hugr cli / MCP server
  └── context compiler        ranking, budgeting, risks, citations
        ├── memory engine     remember / recall / improve / forget, provenance, staleness
        ├── code engine       discovery, symbols, references, tests, git state
        └── session engine    observation, diagnostics, promotion

hugr daemon                   file watching, background indexing, memory audits,
                              hosted /v1 API (memories, storage, sync)

storage                       one libSQL database: relational + FTS + vector + graph
```

The daemon is useful without the cloud; the cloud needs no local state. The same schema and commands work in every mode.

## Terminology

| Term | Definition |
| --- | --- |
| **Context pack** | The compiled output of `hugr context`: ranked, cited, token-budgeted evidence for one task. |
| **Semantic code graph** | The AST-derived index of symbols and the typed reference edges between them (calls, imports, inheritance, implementations, instantiations, type references). |
| **Knowledge graph** | The linked network of memories, sources, entities, and edges that grounds every fact in its origin. Hugr's is temporal: facts carry validity windows and supersession links. |
| **Hybrid retrieval** | Fusing full-text search, vector similarity, and graph expansion into one ranked result, rather than relying on any single signal. |
| **GraphRAG** | Graph-augmented retrieval: expanding retrieved evidence through knowledge-graph relationships before ranking. Hugr does this deterministically, without an LLM in the retrieval loop. |
| **Memory** | A durable remembered fact with provenance, confidence, sensitivity, and a validity window. |
| **Provenance** | The recorded origin of a fact: file, symbol, commit, command, session, or user note. |
| **Episodic / semantic memory** | Cognitive-science distinction Hugr follows: sessions capture episodic observations (what happened); promotion consolidates them into semantic memories (what is durably true). |
| **Consolidation** | Memory maintenance over time: merging duplicates, resolving contradictions, and promoting session observations (`hugr improve`, session promotion). |
| **Supersession** | Retiring a fact by linking it to the newer fact that replaces it (`superseded_by`), instead of deleting it. Keeps memory auditable. |
| **Temporal validity** | Every memory carries `valid_from`/`valid_to` bounds, so facts can expire, be time-scoped, or be retired without loss of history. |
| **Session** | One observed unit of agent or human work: files touched, commands run, outcomes, and a summary. |
| **Promotion** | Consolidating an ended session's episodic facts into a durable semantic memory. |
| **Impact analysis** | Computing the blast radius of a change from the semantic code graph before editing (`hugr impact`). |
| **Risk signal** | A deterministic warning attached to a context pack, such as a stale fact, an untested file, or a high fan-in symbol. |
| **Local-first** | The full system — storage, indexing, embeddings, retrieval — works on the developer machine with no network dependency; cloud and sync are opt-in. |
| **AST** | Abstract syntax tree — the parsed structure of source code, produced by tree-sitter, from which symbols and references are extracted. |
| **MCP** | Model Context Protocol — the open standard agents use to call external tools. |
| **FTS5** | SQLite's full-text search engine, used natively by the storage layer for lexical recall. |
| **ONNX** | Open Neural Network Exchange — the model format used for in-process local embeddings. |
| **libSQL** | An open-source SQLite fork with native vector search, used embedded locally and server-side by Turso. |
| **MRR** | Mean reciprocal rank — the ranking-quality metric reported by `hugr eval`. |
| **SCIP** | SCIP Code Intelligence Protocol — a code-indexing format planned for LSP-grade reference import. |

## Development

```bash
cargo fmt --check          # formatting
cargo test                 # full test suite
cargo test -- --ignored    # live Hugr API client/server contract tests
```

The `local-embeddings` cargo feature (in-process ONNX) is on by default; build with `--no-default-features` for a slim binary — the `deterministic`, `ollama`, and `openai` providers remain available. CI runs formatting, tests, the live API smoke, and a report-only context eval against the last 30 commits on every push.

## Documentation

- [VISION.md](VISION.md) — product principle and product test
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how it is built: storage, entities, engines, surfaces
- [docs/ROADMAP.md](docs/ROADMAP.md) — what comes next

## Acknowledgements

Hugr draws inspiration from [Cognee](https://github.com/topoteretes/cognee), [TraceDecay](https://github.com/ScriptedAlchemy/tracedecay), [Serena](https://github.com/oraios/serena), [Graphiti](https://github.com/getzep/graphiti), [Mem0](https://github.com/mem0ai/mem0), [LangMem](https://github.com/langchain-ai/langmem), and FastContext.

## License

[MIT](LICENSE)
