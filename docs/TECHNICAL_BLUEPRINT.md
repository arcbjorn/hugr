# Hugr Technical Blueprint

Hugr is a project memory and intelligence system for agents. Its core should be fast, portable, inspectable, and deployment-flexible.

## Language Strategy

Use Rust for the core product.

Rust owns:

- CLI and daemon.
- MCP server.
- File discovery and indexing.
- Code graph extraction.
- Git and branch awareness.
- Local storage access.
- Context-pack assembly.
- Background workers.
- Cloud/hybrid agent service runtime.

Use Python only for optional memory intelligence workers and experiments.

Python can own:

- LLM extraction prototypes.
- Embedding adapters.
- Memory consolidation experiments.
- Evaluation scripts.
- Importers for external data sources.

Use TypeScript only where it clearly helps.

TypeScript can own:

- Web UI.
- VS Code or browser integrations.
- Thin SDKs.

The stable system boundary should be Rust-first. Optional workers communicate over local HTTP, gRPC, stdio, or durable queues.

## Deployment Modes

Hugr should use the same conceptual model in every deployment mode.

### Local Mode

- Runs as a local daemon and CLI.
- Stores indexes and memory on the developer machine.
- Keeps private source code local.
- Serves MCP tools to coding agents.

### Cloud Mode

- Runs as a hosted service.
- Supports remote agents and always-on memory.
- Stores project memory, indexes, and context packs centrally.
- Works best for cloud workspaces, remote runners, and managed agent fleets.

### Hybrid Mode

- Runs code intelligence near the repository.
- Runs memory, orchestration, API, and optional UI elsewhere.
- Syncs metadata, summaries, embeddings, facts, graph edges, and context artifacts.
- Avoids uploading full source unless explicitly configured.

## Process Model

```text
hugr cli
  talks to hugr daemon over local socket or HTTP

hugr daemon
  owns project registry, indexing, memory, MCP, and background jobs

workers
  optional memory extraction, embedding, summarization, and evaluation jobs

cloud service
  optional hosted API, orchestration, sync, and multi-project memory
```

The daemon should be useful without the cloud service.

## Storage Model

Start with embedded local storage. Keep the schema portable enough for a future server deployment.

Recommended initial stack:

- libSQL/Turso Vector as the primary relational, FTS, and vector store.
- `F32_BLOB(...)` columns for embeddings inside normal database tables.
- `libsql_vector_idx(...)` vector indexes and `vector_top_k` for semantic recall once embeddings are wired in.
- Embedded graph representation in relational tables first.
- Optional graph database later only if graph traversal becomes the bottleneck.

Do not introduce a separate vector database or heavy graph database on day one. Hugr should keep memory, provenance, full-text search, graph edges, and embeddings in one portable SQLite-compatible database first. The first win is correct memory shape and context compilation, not database purity.


## Vector Storage

Hugr uses libSQL/Turso Vector as the default storage foundation.

This keeps core memory data in one SQLite-compatible database:

- relational provenance tables
- full-text indexes
- graph edges
- temporal memory fields
- embedding vectors
- future remote sync

Initial local path:

```text
.hugr/hugr.db
```

Initial vector-ready table shape:

```sql
CREATE TABLE memory_embeddings (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL DEFAULT 1536,
    embedding F32_BLOB(1536)
);
```

Once embedding generation is implemented, semantic recall should combine:

1. full-text search over memory text
2. vector search over `memory_embeddings.embedding`
3. graph expansion through source/entity/edge relationships
4. freshness, confidence, and validity scoring

## Core Entities

### Project

A root directory or repository Hugr knows how to index.

Fields:

- id
- name
- root_path or remote locator
- default_branch
- created_at
- updated_at

### Source

A provenance object for any memory or fact.

Examples:

- file
- symbol
- commit
- command
- session
- user note
- documentation chunk
- issue or task
- test result

### Memory

A durable remembered item.

Fields:

- id
- project_id
- kind
- text
- structured_payload
- confidence
- created_at
- updated_at
- valid_from
- valid_to
- superseded_by
- sensitivity

Kinds:

- fact
- decision
- preference
- procedure
- warning
- failed_attempt
- task_state
- architecture_note
- test_observation

### Entity

A typed object extracted from code or memory.

Examples:

- file
- symbol
- module
- package
- test
- command
- branch
- commit
- task
- user
- tool

### Edge

A relationship between entities, memories, and sources.

Examples:

- mentions
- defines
- calls
- imports
- tests
- affected_by
- derived_from
- contradicts
- supersedes
- belongs_to
- changed_in
- observed_during

### Session

An agent or human work session.

Tracks:

- prompt/task
- active branch
- files viewed
- files edited
- commands run
- tests run
- failures
- discoveries
- final summary
- promoted memories

### Context Pack

The compiled output Hugr gives to an agent for a task.

Fields:

- id
- task
- project_id
- created_at
- cited_memories
- cited_files
- cited_symbols
- affected_tests
- risks
- stale_facts
- suggested_path
- token_budget
- rendered_text

## Memory Lifecycle

### Remember

Stores a new memory with source and confidence.

```bash
hugr remember "plugin hooks run after configuration is loaded"
```

Should support:

- raw text
- structured JSON
- source attachment
- project scoping
- sensitivity tags
- validity hints

### Recall

Retrieves relevant memory for a query.

```bash
hugr recall "plugin hooks"
```

Recall should combine:

- full-text search
- vector similarity
- graph expansion
- recency
- confidence
- current project state
- source reliability

### Improve

Consolidates memory.

```bash
hugr improve
```

Improve should:

- merge duplicates
- mark contradictions
- promote session notes
- decay stale facts
- create procedures from repeated behavior
- attach memories to code entities

### Forget

Removes or invalidates memory.

```bash
hugr forget --stale
```

Forget should support:

- deletion
- redaction
- invalidation
- supersession
- project-level wipe
- source-level wipe

## Code Intelligence

The code engine should answer operational questions agents need before editing.

Questions:

- Which files are relevant?
- Which symbols matter?
- What calls this?
- What does this call?
- What imports this module?
- What tests cover this path?
- What changed on this branch?
- What is stale after recent edits?
- What is risky to change?

Index layers:

1. Fast file discovery.
2. Text and path index.
3. Syntax-aware symbol extraction.
4. Relationship extraction.
5. Git-aware freshness.
6. Test and impact mapping.
7. Code health scoring.

## Semantic Operations

Hugr should not become a full IDE, but it should expose exact semantic capabilities where they improve agent reliability.

Capabilities:

- find symbol
- find references
- goto definition
- diagnostics for file or project
- replace symbol body
- insert before or after symbol
- rename when backed by language tooling

These operations should be treated as precise tools, not the main product identity.

## Context Compiler

The context compiler is the center of Hugr.

Input:

- task text
- project
- branch/worktree state
- optional token budget
- optional focused files or symbols

Pipeline:

1. Parse task intent.
2. Discover candidate files quickly.
3. Retrieve memories.
4. Retrieve symbols and graph neighborhoods.
5. Check current git state.
6. Attach related sessions, commands, and test outcomes.
7. Remove stale or contradicted facts.
8. Rank evidence by usefulness.
9. Compress into an agent-facing context pack.
10. Include citations and uncertainty.

Output sections:

- task understanding
- relevant files
- important symbols
- relevant memories
- prior attempts
- branch state
- affected tests
- risks
- suggested path
- citations

## MCP Surface

Start with a small high-value tool surface.

Required tools:

- `hugr_context`
- `hugr_remember`
- `hugr_recall`
- `hugr_forget`
- `hugr_project_status`
- `hugr_index`
- `hugr_impact`
- `hugr_session_start`
- `hugr_session_event`
- `hugr_session_end`

Avoid exposing dozens of low-level tools as the default interface. Add advanced tools only when the context compiler cannot cover the use case well.

## CLI Surface

```bash
hugr init
hugr daemon
hugr index
hugr status
hugr remember <text>
hugr recall <query>
hugr context <task>
hugr impact <file-or-symbol>
hugr session start
hugr session event
hugr session end
hugr improve
hugr forget
hugr doctor
```

## Sync Boundary

Design sync around data classes.

Can sync by default:

- project metadata
- memory records
- source references
- entity IDs
- graph edges
- summaries
- embeddings if configured
- context packs
- session summaries

Should require explicit opt-in:

- full source code
- raw command output
- secrets or environment values
- private user notes
- shell history

## MVP Phases

### Phase 1: Local Memory Core

- Rust CLI.
- Project registry.
- libSQL/Turso Vector storage.
- Vector-ready memory schema with `F32_BLOB(...)` embedding columns.
- `remember`, `recall`, `forget`.
- Session table.
- Basic MCP server.

### Phase 2: Fast Project Index

- File discovery.
- Ignore rules.
- FTS index.
- Git state detection.
- Incremental freshness.

### Phase 3: Context Packs

- `hugr context`.
- Memory + file retrieval.
- Citations.
- Token budget.
- Rendered agent output.

### Phase 4: Code Graph

- Symbol extraction.
- Definitions and references.
- Imports and module relationships.
- Basic impact radius.

### Phase 5: Sessions and Improve

- Agent session hooks.
- Command/test/file event capture.
- Session summarization.
- Memory promotion.
- Duplicate and contradiction handling.

### Phase 6: Tests and Risk

- Test discovery.
- Affected test mapping.
- Complexity and coupling signals.
- Risk section in context packs.

### Phase 7: Cloud and Hybrid

- Remote API.
- Auth.
- Sync protocol.
- Worker separation.
- Hosted memory service.

## Non-Goals For The First Build

- Full IDE replacement.
- Large dashboard-first UX.
- Heavy graph database dependency.
- Cloud-only architecture.
- Generic assistant personality memory.
- Exposing every internal primitive as a first-class user command.

## Product Test

Hugr is working when this command is obviously useful inside a real project:

```bash
hugr context "make this change"
```

The output should help an agent act with fewer files opened, fewer wrong assumptions, fewer repeated mistakes, and better tests.
