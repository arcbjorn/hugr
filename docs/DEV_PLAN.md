# Hugr Development Plan

This plan is meant to make the next session productive without freezing the design too early. Hugr should stay ambitious, but every milestone should produce a useful agent-facing behavior.

## Current State

Hugr is a Rust CLI with a libSQL/Turso Vector-backed local store.

Implemented:

- `hugr init`
- `hugr status`
- `hugr remember <text>`
- `hugr recall <query>`
- `hugr context <task>`
- `hugr doctor`
- `.hugr/hugr.db` local database
- vector-ready `memory_embeddings` table with `F32_BLOB(1536)`
- `memory_embeddings_vector_idx` using `libsql_vector_idx(embedding)`
- basic full-text-ready schema
- `schema_migrations` tracking for the initial schema
- FTS-backed memory recall with deterministic reranking
- structured `ContextPack` generation and Markdown rendering
- JSON output for `hugr recall --json` and `hugr context --json`
- deterministic local embedding provider for tests and offline development
- optional OpenAI-compatible embedding provider selected through environment config
- synchronous embedding persistence on `hugr remember`
- `vector_top_k` recall over stored deterministic embeddings
- combined FTS and vector recall ranking
- single-project registry with root/name/git metadata
- `hugr project status`
- dedicated file discovery layer with Git and walking adapters
- `.gitignore`-aware fallback discovery with generated/vendor/build skips
- `discovered_files` table populated by `hugr context`
- durable session tables and CLI workflow
- daemon-observed file-change and git/worktree session events for active sessions
- `hugr run <command>` records shell/test command outcomes for active sessions
- `hugr shell-hook <bash|zsh>` emits shell integration for automatic command-status observation
- `hugr session promote [--json]` summarizes latest session facts into a durable memory
- daemon background promotion turns ended, unpromoted sessions into durable memories
- promoted session memories preserve structured provenance payloads and expose them through CLI, MCP, and context JSON
- `hugr remember --source <kind:locator>` and MCP `hugr_remember.source` attach manual source provenance to memories
- `hugr remember` and MCP `hugr_remember` accept confidence, sensitivity, and validity metadata for structured memory writes
- structured memory payloads include current project id, name, root, remote, and default branch provenance
- recent session facts in context packs
- stdio MCP server with core Hugr tools
- `hugr daemon` local HTTP runtime with `/health`, `/status`, file watching, debounced background indexing, and periodic memory-maintenance audits
- daemon background indexing records discovery session events with indexed file/symbol summaries
- `hugr index`, MCP `hugr_index`, and daemon discovery facts include file-role, language, and symbol-kind classifications
- `hugr index` for explicit project indexing
- best-effort code symbol extraction stored in `code_symbols`
- best-effort direct reference/call/import extraction stored in `code_references`
- tree-sitter-backed Rust, Python, TypeScript, JavaScript/JSX, Go, Java, and Swift symbol extraction with line ranges
- important symbol citations in context packs
- `hugr impact <file-or-symbol>` for direct indexed impact reports
- local branch, upstream, ahead/behind, and worktree changes in context packs
- environment-driven cloud/hybrid/remote storage config with redacted status output
- safe default and explicit opt-in cloud/hybrid sync class policy
- `hugr sync status` execution-plan output for cloud/hybrid/remote sync decisions
- remote-only direct libSQL/Turso storage execution when `HUGR_STORAGE_MODE=remote` and credentials are explicit
- Hugr API sync backend contract metadata in `hugr sync status`, including endpoint, contract version, and route surface
- Hugr API sync client transport for explicit push, pull, and history requests against the `/v1/sync/*` contract
- `hugr daemon` exposes authenticated Hugr API sync routes for status, push, pull, and history, and records accepted API sync runs
- `hugr daemon` exposes authenticated `GET /v1/memories` and `POST /v1/memories` storage routes for hosted memory reads and writes without recording sync runs
- Hugr API sync push and pull transfer and reconcile memory row payloads with the same overwrite/preserve behavior used by direct sync
- Hugr API sync push and pull transfer and reconcile all currently syncable safe row payload classes: project metadata, memories, embeddings, source references, discovered files, entities, code symbols, graph edges, code references, and finalized sessions
- remote-mode Hugr API memory operations for `remember`, `recall`, memory counts, `forget`, and `improve` memory maintenance use hosted memory rows instead of a local libSQL connection
- ignored live integration coverage for remote-mode Hugr API memory commands spawning a daemon and client against localhost
- guarded `hugr sync push` dry-run and explicit execution path for safe sync classes
- guarded `hugr sync pull` dry-run and explicit execution path for safe sync classes
- `hugr sync history` with per-table sync counters and conflict summaries
- `hugr forget [--json] <query>` soft-retires active memories by setting `valid_to`
- `hugr improve [--json]` reports active/retired memory counts and exact duplicate active-memory groups
- `hugr improve --execute --duplicates [--json]` retires older duplicate memories and points them at the kept fact
- `hugr improve [--json]` reports deterministic stale candidates for active memories with opposing terms
- `hugr improve --execute --stale [--json]` retires older stale candidates and points them at newer evidence
- `hugr context` and `hugr_context` surface relevant unresolved stale-memory risks with citations
- `hugr context` and `hugr_context` include deterministic token budget metadata and trim lower-priority context items before rendering
- `hugr context` and `hugr_context` include deterministic evidence scores and reasons for files, symbols, tests, memories, stale-memory risks, and session facts
- code-reference edges distinguish imports, calls, member calls, implementations, inheritance, instantiations, type references, and generic references
- initial vision, storage, and technical blueprint docs

Not implemented yet:

- tree-sitter-backed Kotlin parsing; the current published `tree-sitter-kotlin` crate resolves to `tree-sitter <0.23` and conflicts with Hugr's `tree-sitter 0.26` parser stack
- ordinary API-backed storage operations for non-memory remote-mode commands such as project status, indexing, context assembly, and sessions
- default CI wiring for live client/server Hugr API contract tests beyond the ignored localhost smoke

## Completion Gap Review

The near-term plan is close to complete, but the broader vision and technical blueprint still require several product systems before Hugr is complete:

- Daemon/runtime service: `hugr daemon` local HTTP transport, file watching, debounced background indexing, and periodic memory-maintenance audits exist.
- Remote/cloud execution: direct remote libSQL/Turso storage mode exists; `hugr sync status` exposes the Hugr API contract metadata; explicit API sync push, pull, and history requests can use a configured hosted endpoint; `hugr daemon` exposes authenticated `/v1/sync/*` routes that accept and reconcile all currently syncable safe row payload classes, then persist accepted runs; remote-mode Hugr API memory commands use authenticated `/v1/memories` storage routes; and an ignored live localhost integration test covers remote memory command behavior. Default CI wiring for live API tests and ordinary API-backed storage for non-memory commands are still missing.
- Context pack persistence: durable `context_packs` storage and real sync behavior for the `context_packs` sync class.
- Context graph expansion: use code/source/entity graph neighborhoods during `hugr context`, not only direct file/symbol/memory retrieval.
- Structured memory provenance: promoted session memories preserve structured payloads in JSON, and CLI/MCP memory writes support manual source attachments plus confidence, sensitivity, validity metadata, and project scope.
- Automatic session observation: daemon captures file-change and git/worktree events for active sessions, `hugr run <command>` captures command/test outcomes, `hugr shell-hook <bash|zsh>` can observe ordinary shell command statuses, and daemon indexing captures classified discovery summaries.
- Session summarization and memory promotion: manual `hugr session promote` summarizes latest session facts into long-term memory, and the daemon periodically promotes ended, unpromoted sessions.
- Risk and health signals: complexity, coupling, dead code, diagnostics, risky paths, stale-after-edit detection, and richer risk sections in context packs.
- Semantic operations: symbol lookup/edit helpers, diagnostics integration, and safe structural operations where they reduce brittle text edits.
- Incremental freshness: watcher-driven invalidation and refresh for file discovery, symbols, graph edges, tests, and context evidence.

## Runtime Decision

Tokio is the right default runtime for the current architecture.

Reasons:

- The libSQL Rust API is async.
- Hugr will need a long-running daemon.
- MCP servers are naturally async IO services.
- File watching, indexing, remote sync, and background memory jobs fit Tokio well.
- Tokio is the safest ecosystem choice for Rust networking and service code.

This should remain a contained dependency, not a product identity. Keep Tokio at the boundary:

- `main`
- daemon/server runtime
- storage calls
- background jobs
- future MCP transport

Keep pure logic synchronous where possible:

- ranking
- query parsing
- context-pack assembly
- scoring
- schema-independent memory logic
- code graph transformations

If Hugr later needs a different runtime in a narrow environment, the core logic should still be reusable.

## Product Direction

Hugr should become a project memory and intelligence system for agents.

The flagship behavior is:

```bash
hugr context "task"
```

That command should compile what an agent needs right now:

- relevant memories
- relevant files
- relevant symbols
- prior attempts
- current git state
- affected tests
- stale or contradicted facts
- risks
- suggested path
- citations

The product should avoid becoming a generic note database or a dashboard-first app. The context compiler is the center.

## Design Principles

1. Make the useful path obvious.
2. Store provenance for every durable fact.
3. Prefer structured memory over raw text when the structure is known.
4. Keep local, cloud, and hybrid modes compatible.
5. Do not require cloud for the core developer experience.
6. Do not require local-only assumptions for the architecture.
7. Keep source-code upload explicit, not accidental.
8. Use one high-level context tool before exposing many low-level tools.
9. Make memory inspectable and correctable.
10. Treat stale facts as normal, not exceptional.

## Next Session Goal

Build deterministic stale-fact detection.

Target outcome:

```bash
hugr remember "plugin hooks run after configuration is loaded"
hugr remember "plugin hooks now run before configuration is loaded"
hugr improve --json
```

The report should identify likely contradictory memories so a later execution path can retire stale facts with clear evidence.

## Phase 1: Make Storage Real

Tasks:

- Add schema version tracking.
- Add a `schema_migrations` table.
- Move migration SQL into a dedicated module or file.
- Add integration tests that create a temp `.hugr/hugr.db`.
- Add tests for:
  - init creates expected tables
  - remember writes a memory
  - recall reads from libSQL
  - FTS table stays synchronized
  - vector index exists

Open questions:

- Keep migrations embedded as Rust strings, or load SQL files?
- Use a temporary directory helper dependency for tests, or keep tests std-only?
- Should the local DB path be configurable before project registry exists?

## Phase 2: Real Recall

Tasks:

- Query `memories_fts` for full-text recall.
- Keep the current term-scoring as a fallback/reranker.
- Add ranking fields:
  - text score
  - recency
  - confidence
  - validity
  - source reliability placeholder
- Return citation metadata with recall results.
- Add `hugr recall --json` for agent consumption.

Open questions:

- Should recall return memories only, or evidence objects that can include files, symbols, sessions, and sources?
- How soon should `Memory` split into `MemoryRecord` and `Evidence`?

## Phase 3: Embeddings and Turso Vector

Tasks:

- Add an embedding provider trait.
- Start with a deterministic local fake embedding for tests.
- Add optional real embedding provider later.
- Store embeddings in `memory_embeddings`.
- Add vector search using `vector_top_k`.
- Combine FTS and vector results in recall.
- Add a `hugr embed` or background embedding path.

Implemented provider support:

- Deterministic provider remains the default for local/offline work and tests.
- `HUGR_EMBEDDING_PROVIDER=openai` enables an OpenAI-compatible embeddings endpoint through `curl`.
- `HUGR_OPENAI_API_KEY` or `OPENAI_API_KEY` supplies credentials.
- `HUGR_OPENAI_EMBEDDING_MODEL`, `HUGR_OPENAI_EMBEDDING_URL`, and `HUGR_EMBEDDING_DIMENSIONS` configure the provider.
- `hugr doctor` reports the selected embedding provider without exposing secrets.

Open questions:

- Which embedding model should be the default?
- Should Hugr support local embedding models first, API embedding models first, or both behind config?
- Should embeddings be generated synchronously on `remember`, or asynchronously by the daemon?

## Phase 4: Project Registry

Tasks:

- Add `projects` table.
- Track project root, name, git remote, default branch, and created time.
- Make `.hugr/hugr.db` support one project first.
- Leave room for global Hugr config later.
- Add `hugr project status`.

Open questions:

- Should each repo have its own `.hugr/hugr.db`, or should there also be a global database?
- How should cloud/hybrid project IDs map to local project IDs?

## Phase 5: File Discovery

Tasks:

- Replace the temporary recursive scanner with a dedicated discovery layer.
- Respect `.gitignore`.
- Skip generated/vendor/build directories.
- Add fast path-based candidate ranking.
- Keep an adapter boundary for FFF or another fast file finder.
- Store discovered files in libSQL.

Open questions:

- Should FFF be a subprocess integration first or a library integration later?
- What is the fallback when FFF is not installed?
- Should file discovery run during `hugr context` or only during `hugr index`?

## Phase 6: Sessions

Tasks:

- Add session tables.
- Add `hugr session start`.
- Add `hugr session event`.
- Add `hugr session end`.
- Store:
  - task
  - branch
  - files viewed
  - files edited
  - commands run
  - tests run
  - failures
  - final summary
- Let `hugr context` include recent relevant session facts.

Open questions:

- How much raw command output should be stored by default?
- What is the redaction model for secrets and environment values?
- Should session summaries require an LLM, or start as structured event logs?

## Phase 7: MCP Server

Tasks:

- Add `hugr mcp` command.
- Expose a minimal tool surface:
  - `hugr_context`
  - `hugr_remember`
  - `hugr_recall`
  - `hugr_project_status`
  - `hugr_session_start`
  - `hugr_session_event`
  - `hugr_session_end`
- Return structured JSON with citations.
- Keep low-level tools private until the high-level context tool proves insufficient.

Open questions:

- Which Rust MCP crate is mature enough?
- Should MCP use stdio first, HTTP later, or both immediately?
- How should agent hooks be installed for Codex, Claude Code, Cursor, and other clients?

## Phase 8: Code Intelligence

Tasks:

- Add a code indexing module.
- Start with files and simple language detection.
- Add tree-sitter symbol extraction.
- Store symbols as entities.
- Store imports/calls/references as edges where available.
- Add `hugr impact <file-or-symbol>`.

Implemented first slice:

- Best-effort dependency-free symbol extraction for common source files.
- `code_symbols` table with path, kind, name, language, line, and signature.
- `hugr index` to populate discovered files and symbols.
- `hugr_context` and `hugr context` include important symbols when available.
- `code_references` table for direct references, calls, and imports.
- Code-reference extraction classifies imports, calls, member calls, implementations, inheritance, instantiations, type references, and generic references.
- `hugr impact` and `hugr_impact` report matched symbols, direct references, and affected files.
- Symbol ranges are stored and surfaced in context and impact JSON.
- Impact reports include inbound references and outbound references from matched symbol or file scope.
- Likely test files are mapped from discovered project paths and surfaced in context and impact output.
- Context packs include local branch, upstream, ahead/behind counts, and worktree changes.
- Rust, Python, TypeScript, JavaScript/JSX, Go, Java, and Swift symbol extraction use tree-sitter when parsing succeeds, with line-scanner fallback.

Open questions:

- Which languages should be first-class first?
- Should syntax indexing be best-effort per language or strict?
- How much should rely on tree-sitter versus LSP?

## Phase 9: Context Compiler

Tasks:

- Split context generation from command printing.
- Define a `ContextPack` struct.
- Add token budgeting.
- Add evidence ranking.
- Add stale-fact filtering.
- Add citations for every section.
- Add Markdown and JSON renderers.

Implemented first slices:

- `hugr context` and `hugr_context` rank files, symbols, tests, memories, session facts, and stale-memory risks with deterministic evidence scores before token budgeting.
- `hugr context` and `hugr_context` include deterministic token budget metadata and remove lower-priority items when a pack exceeds the default budget.
- `hugr context` and `hugr_context` include unresolved stale-memory risks relevant to recalled task memories.
- Stale-memory risks render in Markdown and JSON with newer/older memory evidence, shared terms, deterministic signal, and `stale_memory` citations.

Open questions:

- Should the context compiler be deterministic first, or should LLM compression be introduced early?
- What is the minimum citation format that agents can reliably use?

## Phase 10: Cloud and Hybrid Boundary

Tasks:

- Keep local database schema compatible with remote libSQL/Turso.
- Define syncable data classes:
  - memories
  - sources
  - entities
  - edges
  - embeddings
  - context packs
  - session summaries
- Define explicit opt-in classes:
  - full source
  - raw command output
  - secrets
  - private notes
- Add config placeholders for remote URL and auth token.

Implemented first slice:

- `HUGR_STORAGE_MODE=local|hybrid|remote` is parsed and validated.
- `HUGR_REMOTE_DATABASE_URL` and `HUGR_REMOTE_AUTH_TOKEN` are recognized, with common libSQL/Turso aliases.
- `HUGR_API_URL` and `HUGR_API_TOKEN` are recognized for the Hugr API backend contract.
- CLI status, project status, doctor, and MCP project status expose redacted storage configuration.
- Remote mode opens direct remote libSQL/Turso storage when explicitly configured; Hugr API remote mode routes memory commands through hosted memory storage and still fails non-memory local database commands with a hosted-storage-operations error instead of silently writing locally.
- Hybrid mode keeps local storage active while guarded direct remote sync remains opt-in.
- `HUGR_SYNC_CLASSES` represents safe default sync classes and explicit opt-ins for full source, raw command output, secrets, and private notes.
- `HUGR_SYNC_BACKEND=direct_libsql|hugr_api` records the execution strategy decision, defaulting to direct libSQL/Turso.
- `hugr sync status [--json]` renders the current sync execution plan, with guarded remote reads and writes available for explicitly configured hybrid or remote direct libSQL/Turso storage.
- For `HUGR_SYNC_BACKEND=hugr_api`, `hugr sync status [--json]` exposes the configured endpoint, `hugr-api-v1` contract version, and the planned `/v1/memories` plus `/v1/sync/*` route surface.
- For explicit API sync execution, Hugr posts contract JSON to `POST /v1/sync/push` and `POST /v1/sync/pull`, fetches `GET /v1/sync/history`, and parses hosted table counters plus memory row payloads into the existing sync result types.
- `hugr daemon` serves authenticated `GET /v1/memories`, `POST /v1/memories`, `GET /v1/sync/status`, `POST /v1/sync/push`, `POST /v1/sync/pull`, and `GET /v1/sync/history` routes using `HUGR_API_TOKEN` or `HUGR_REMOTE_AUTH_TOKEN` as the bearer token.
- Hosted API push and pull validate the contract version, transfer project/memory/embedding/source/discovered-file/entity/code-symbol/edge/code-reference/finalized-session row payloads, apply supported rows server-side on push, return supported rows on pull, and persist accepted runs into sync history.
- Hosted memory storage validates the contract version, applies project/memory/embedding payloads through the same row reconciliation path without recording sync runs, and returns memory rows for remote-mode recall, memory counts, forget, and improve operations.
- `tests/hugr_api_live.rs` provides an ignored live daemon/client contract test for remote-mode memory commands and verifies hosted memory storage does not create client-local `.hugr` state or sync-history rows.
- `hugr sync push [--dry-run|--execute] [--json]` counts configured sync-class tables locally by default, writes to direct libSQL/Turso when `--execute` and hybrid remote config are explicit, or posts to the Hugr API backend when selected.
- `hugr sync pull [--dry-run|--execute] [--json]` counts configured sync-class tables locally by default, reads from direct libSQL/Turso when `--execute` and hybrid remote config are explicit, or posts to the Hugr API backend when selected.
- Sync push currently covers project metadata, memories, embeddings, source references, entity/code-symbol indexes, graph/code-reference edges, and finalized session summaries. It does not sync raw session events, shell output, secrets, private notes, or full source without future explicit data sources.
- Sync pull reconciles the same safe table set with conservative merge rules: local memories are not clobbered, project and code-index rows update only when the remote record is newer, dependent embeddings are skipped until their memory exists, and finalized sessions only replace open or older local summaries.
- Sync push and pull report per-table inserted, updated, skipped, and conflict counts.
- Executed sync runs are recorded in `sync_runs`, `sync_table_runs`, and `sync_table_conflicts`.
- `hugr sync history [--json]` renders the last recorded sync runs with table counters and grouped conflict reasons.

Open questions:

- Should hosted cloud mode use only the Hugr API service, or continue supporting direct remote libSQL/Turso for advanced users?
- How long should sync history be retained before pruning?

## Phase 11: Memory Maintenance

Tasks:

- Keep ordinary recall and context generation limited to active memories.
- Soft-retire memories instead of physically deleting rows.
- Add `hugr forget [--json] <query>` for term-based memory retirement.
- Add `hugr improve [--json]` for maintenance inspection.
- Surface active and retired memory counts.
- Detect exact duplicate active-memory groups.
- Detect likely stale or contradictory memory pairs with deterministic signals.
- Add `hugr improve --execute --stale` for explicit stale-candidate retirement.
- Wire `hugr_forget` through MCP.

Implemented first slice:

- `hugr forget [--json] <query>` matches active memories by query terms, sets `valid_to`, and returns the retired rows.
- Retired memories stay in `memories` for audit/sync but are excluded from `memories()`, FTS recall, vector recall, and context packs.
- `hugr improve [--json]` renders active count, retired count, and exact duplicate active-memory groups.
- `hugr improve --execute --duplicates [--json]` keeps the newest duplicate in each exact group, retires older duplicates, and writes `superseded_by`.
- `hugr improve [--json]` reports stale candidates when active memories share meaningful terms but contain opposing deterministic signal terms such as `after` versus `before`.
- `hugr improve --execute --stale [--json]` retires older stale candidates and writes `superseded_by` to the newer evidence memory.
- `hugr_forget` now uses the same soft-retire behavior through MCP.
- Hybrid pull can propagate remote memory retirement metadata without overwriting local memory text.

Open questions:

- Should `hugr improve --execute` support multiple maintenance actions at once, or require one explicit action flag per run?
- Should retired memories remain syncable forever, or be pruned after a retention window?
- Which additional stale-candidate signals should be executable without a human confirmation step?

## Near-Term Implementation Order

Recommended next commits:

1. Done: `feat(storage): add schema migrations`
2. Done: `feat(recall): query memory fts`
3. Done: `feat(context): return structured context packs`
4. Done: `feat(memory): add embedding provider trait`
5. Done: `feat(vector): add vector recall`
6. Done: `feat(project): add project registry`
7. Done: `feat(index): add file discovery`
8. Done: `feat(session): record agent sessions`
9. Done: `feat(mcp): expose core tools`
10. Done: `feat(code): index symbols`
11. Done: `feat(impact): trace symbol impact`
12. Done: `feat(graph): enrich code relationships`
13. Done: `feat(testmap): suggest affected tests`
14. Done: `feat(git): add worktree context`
15. Done: `feat(embed): add openai provider`
16. Done: `feat(parser): use tree-sitter rust`
17. Done: `feat(parser): use tree-sitter python`
18. Done: `feat(parser): use tree-sitter typescript`
19. Done: `feat(parser): use tree-sitter go`
20. Done: `feat(config): add storage mode placeholders`
21. Done: `feat(sync): define sync class policy`
22. Done: `feat(sync): expose sync execution plan`
23. Done: `feat(sync): push safe sync classes`
24. Done: `feat(sync): pull safe sync classes`
25. Done: `feat(sync): report conflicts and history`
26. Done: `feat(memory): soft forget memories`
27. Done: `feat(memory): consolidate duplicates`
28. Done: `feat(memory): detect stale candidates`
29. Done: `feat(memory): retire stale candidates`
30. Done: `feat(context): surface stale memory risks`
31. Done: `feat(context): add token budgeting`
32. Done: `feat(context): rank context evidence`
33. Done: `feat(graph): classify richer symbol edges`
34. Done: `feat(parser): use tree-sitter javascript`
35. Done: `feat(parser): use tree-sitter java`
36. Done: `feat(parser): use tree-sitter swift`
37. Done: `feat(daemon): add local runtime skeleton`
38. Done: `feat(daemon): index on file changes`
39. Done: `feat(daemon): audit memory in background`
40. Done: `feat(session): observe daemon file changes`
41. Done: `feat(session): observe command outcomes`
42. Done: `feat(session): add shell observation hooks`
43. Done: `feat(session): promote session memory`
44. Done: `feat(session): auto-promote ended sessions`
45. Done: `feat(session): capture indexing discoveries`
46. Done: `feat(memory): preserve session provenance`
47. Done: `feat(memory): attach manual sources`
48. Done: `feat(memory): add write metadata`
49. Done: `feat(index): classify discoveries`
50. Done: `feat(memory): add project provenance`
51. Done: `feat(sync): define Hugr API contract`
52. Done: `feat(sync): add Hugr API client`
53. Done: `feat(daemon): serve Hugr API sync routes`
54. Done: `feat(sync): reconcile API memory rows`
55. Done: `feat(sync): reconcile API graph rows`
56. Done: `feat(sync): reconcile API index rows`
57. Done: `feat(api): add remote memory storage`
58. Done: `test(api): cover live remote memory`

Each commit should leave the CLI usable.

## Verification Checklist

Before ending each future session:

- Run `cargo fmt --check`.
- Run `cargo test`.
- Run a smoke test in `/private/tmp`.
- Verify `.hugr/hugr.db` schema when migrations change.
- Check `git status --short --branch`.
- Update this plan if priorities change.

## Current Best Next Step

Add API-backed storage for non-memory remote-mode commands.

That is the right next step because all currently syncable safe row payload classes and remote-mode memory commands now move through authenticated Hugr API routes, with an ignored live daemon/client test covering the memory path. The remaining remote/cloud gap is that project status, indexing, context assembly, and session workflows still depend on local database access in Hugr API remote mode.
