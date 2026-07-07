# Hugr Architecture

Hugr is a project memory and intelligence system for agents. The core is fast, portable, inspectable, and deployment-flexible: one Rust binary provides the CLI, MCP server, and daemon, backed by one SQLite-compatible database.

## Process Model

```text
hugr cli / MCP server
  talks to storage directly, or to the hugr daemon over local HTTP

hugr daemon
  owns file watching, background indexing, memory audits, session
  promotion, and the hosted /v1 API (memories, storage, sync)

cloud service (optional)
  the same daemon serving hosted storage and sync for remote-mode clients
```

The daemon is useful without the cloud; the cloud needs no local state.

## Language Strategy

Rust owns the stable system boundary: CLI, daemon, MCP server, discovery, indexing, code graph, git awareness, storage, context compilation, and background workers. Python and TypeScript are reserved for optional edges — experiments, importers, and future UI — communicating over local HTTP or stdio.

## Deployment Modes

The same schema, concepts, and commands work in every mode.

- **Local** (default): everything on the developer machine; private source stays local.
- **Remote**: all commands run against hosted rows over the authenticated `/v1` API or direct remote libSQL/Turso, with no client-local `.hugr` state.
- **Hybrid**: local storage stays active while guarded sync pushes safe data classes to a remote.

## Storage

Hugr uses libSQL/Turso Vector as its only storage layer. Memory records, provenance, full-text search, graph edges, temporal fields, embeddings, and sync history live in one SQLite-compatible database.

Why this fits:

- SQLite-compatible, so local mode stays simple.
- The same schema deploys to remote libSQL/Turso, so cloud and hybrid modes need no storage rewrite.
- Built-in vector columns and search, so no separate vector database.
- Semantic search results join back to memories, sources, sessions, entities, and edges with normal SQL.

Paths:

```text
.hugr/hugr.db    project store
~/.hugr/         global memory store and local embedding model cache
```

Embeddings are stored inline:

```sql
CREATE TABLE memory_embeddings (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL DEFAULT 1536,
    embedding F32_BLOB(1536)
);
```

`memory_embeddings_vector_idx` is created with `libsql_vector_idx(embedding)` and queried with `vector_top_k`. Embeddings are generated synchronously on `remember` by the configured provider; all vectors normalize to the 1536-wide columns so provider dimensions can differ. Schema changes are tracked in `schema_migrations`.

Retrieval is hybrid: recall fuses four signals — FTS5 full-text search, vector similarity, graph expansion through linked files, symbols, sources, sessions, and entities, and recency/confidence/validity scoring with stale-fact penalties. This is GraphRAG-style retrieval with no LLM in the loop; the context compiler merges the signals into cited context, and vector search is never the user interface.

## Core Entities

Together these form a temporal knowledge graph: every durable fact links back to its origin, carries a validity window, and can be superseded without losing history.

- **Project** — a root directory or repository Hugr indexes: id, name, root path or remote locator, default branch, timestamps.
- **Source** — a provenance object for any fact: file, symbol, commit, command, session, user note, documentation chunk, test result.
- **Memory** — a durable remembered item: project, kind (fact, decision, preference, procedure, warning, failed attempt, task state, architecture note, test observation), text, structured payload, confidence, sensitivity, validity window (`valid_from`/`valid_to`), `superseded_by`.
- **Entity** — a typed object extracted from code or memory: file, symbol, module, package, test, command, branch, commit, task, tool.
- **Edge** — a relationship between entities, memories, and sources: mentions, defines, calls, imports, tests, affected_by, derived_from, contradicts, supersedes, belongs_to, changed_in, observed_during.
- **Session** — an agent or human work session: task, branch, files viewed and edited, commands and tests run, failures, discoveries, final summary, promoted memories.
- **Context pack** — the compiled output for a task: cited memories, files, symbols, tests, risks, stale facts, token budget, rendered text. Persisted packs are audit and sync artifacts; packs are always recompiled, never re-served.

## Memory Lifecycle

- **Remember** stores a fact with source attachment, confidence, sensitivity, validity hints, and project provenance; `--global` writes to the device-local user store instead.
- **Recall** retrieves ranked, cited memories for a query using the four retrieval signals above.
- **Improve** performs consolidation: it reports active/retired counts, exact duplicate groups, and deterministic stale/contradiction candidates; `--execute --duplicates|--stale` retires losers and records `superseded_by`.
- **Forget** soft-retires matching memories by setting `valid_to`. Retired rows leave recall and context but stay auditable and syncable.
- **Promote** consolidates episodic session observations into durable semantic memories with structured provenance — manually (`hugr session promote`, optionally LLM-distilled) or automatically by the daemon for ended, unpromoted sessions.
- Session events, summaries, and diagnostics are secret-redacted at the storage boundary (API keys, tokens, JWTs, PEM blocks, URL credentials) before they can reach sync or LLM synthesis.

## Code Intelligence

The code engine builds a local semantic code graph and answers the operational questions agents need before editing: which files are relevant, which symbols matter, what calls this, what does this call, what tests cover this path, what changed on this branch, what is stale, what is risky to change.

Index layers:

1. `.gitignore`-aware file discovery with generated/vendor/build skips.
2. Tree-sitter symbol extraction (line-scanner fallback) for Rust, Python, TypeScript, JavaScript/JSX, Go, Java, Kotlin, and Swift, with line ranges and signatures.
3. Typed reference edges: imports, calls, member calls, implementations, inheritance, instantiations, type and generic references — extracted in linear time via hashed declaration lookups.
4. Per-file ambiguity resolution: local definitions shadow foreign ones, import-line evidence narrows the rest, member calls keep every candidate.
5. Git and branch awareness: upstream, ahead/behind, worktree changes.
6. Heuristic test mapping, including Rust inline `#[cfg(test)]` modules.
7. Code health scoring from indexed symbol bodies.

Freshness is incremental: `hugr index --paths` and daemon file events re-index only changed paths plus their inbound-reference sources, and deleted files are pruned from every table including dangling edges.

## Semantic Operations

Hugr is not an IDE, but it exposes exact semantic capabilities where they improve agent reliability:

- symbol lookup (`hugr symbols`) and reference/impact tracing (`hugr impact`)
- structured diagnostics captured from `hugr run` output
- parser-validated symbol body replacement (`hugr replace-symbol`)
- reference-aware rename (`hugr rename-symbol`)
- reference-aware move across files, packages, and modules (`hugr move-symbol --rewrite-references`), covering Rust import forms, Python imports, TypeScript/JavaScript ES modules and conservative CommonJS, same-package Go/Java/Kotlin and same-module Swift validation, Java/Kotlin cross-package import rewrites, Go moves via nearest `go.mod`, and Swift moves via nearest `Package.swift`

Every edit refuses ambiguous targets, validates the result parses, re-indexes immediately, and records a session edit event when a session is active. These operations are precise tools, not the product identity.

## Context Compiler

The context compiler is the center of Hugr. Input: task text, project, branch state, optional token budget.

Pipeline:

1. Discover candidate files and refresh their index entries.
2. Retrieve memories, symbols, graph neighborhoods, tests, session facts, and diagnostics.
3. Score everything with deterministic evidence reasons.
4. Derive risk signals: stale-memory conflicts, changed files, missing test or symbol coverage, graph coupling, failure facts, index freshness, stale-after-edit and stale-context invalidation, structured diagnostics, and code health (`large_symbol`, `long_parameter_list`, `high_fan_in`, `deep_nesting`, `cyclomatic_complexity`, `refactor_surface`, `public_api_surface`, `unreferenced_private_symbol`).
5. Trim lower-priority items to the token budget.
6. Render Markdown or JSON with citations on every item, and persist the pack.

The CLI and MCP tool share this pipeline exactly.

## MCP Surface

The tool surface stays small and high-value; low-level tools are added only when the context compiler cannot cover the use case.

`hugr_context`, `hugr_remember`, `hugr_recall`, `hugr_forget`, `hugr_project_status`, `hugr_index`, `hugr_symbols`, `hugr_impact`, `hugr_replace_symbol`, `hugr_rename_symbol`, `hugr_move_symbol`, `hugr_session_start`, `hugr_session_event`, `hugr_session_end`.

## CLI Surface

```bash
hugr init
hugr status
hugr doctor
hugr mcp
hugr daemon [--addr <host:port>]
hugr index [--paths <p1,p2,...>]
hugr remember <text> [--global] [--source <kind:locator>] [--confidence ...] [--sensitivity ...]
hugr recall <query> [--global]
hugr forget <query> [--global]
hugr improve [--execute --duplicates|--stale]
hugr context <task> [--budget <tokens>]
hugr symbols <query>
hugr impact <file-or-symbol>
hugr replace-symbol <path> <symbol>
hugr rename-symbol <path> <symbol> <new-symbol>
hugr move-symbol <source-path> <symbol> <destination-path> [--rewrite-references]
hugr project status
hugr session start|event|end
hugr session promote [--llm]
hugr run <command>
hugr shell-hook <bash|zsh>
hugr sync status|push|pull|history
hugr eval [--from-git <n>] [--min-hit-rate <f>]
hugr install <claude-code|cursor> [--shared]
```

Most commands accept `--json` for agent consumption.

## Sync Boundary

Sync is designed around data classes, configured with `HUGR_SYNC_CLASSES` and executed by `HUGR_SYNC_BACKEND=direct_libsql|hugr_api`.

Syncs by default: project metadata, memory records, embeddings, source references, entities, graph edges, code symbols and references, test mappings, finalized session summaries, context packs.

Requires explicit opt-in: full source code, raw command output, secrets or environment values, private user notes, shell history. Global memories never sync.

Pull merges conservatively: local memories are never clobbered, other rows update only when the remote record is newer, and executed runs are recorded with per-table counters and conflict reasons (`hugr sync history`).

## Non-Goals

- Full IDE replacement.
- Dashboard-first UX.
- Heavy graph database dependency.
- Cloud-only architecture.
- Generic assistant personality memory.
- Exposing every internal primitive as a first-class user command.
