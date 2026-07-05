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
- `hugr index` and daemon background indexing prune discovered-file, symbol, and code-reference rows for files that no longer exist on disk, removing dangling reference edges and reporting pruned counts
- `hugr index` for explicit project indexing
- best-effort code symbol extraction stored in `code_symbols`
- best-effort direct reference/call/import extraction stored in `code_references`
- tree-sitter-backed Rust, Python, TypeScript, JavaScript/JSX, Go, Java, Kotlin, and Swift symbol extraction with line ranges
- important symbol citations in context packs
- `hugr impact <file-or-symbol>` for direct indexed impact reports
- local branch, upstream, ahead/behind, and worktree changes in context packs
- durable `context_packs` storage and sync behavior for compiled context-pack JSON payloads
- environment-driven cloud/hybrid/remote storage config with redacted status output
- safe default and explicit opt-in cloud/hybrid sync class policy
- `hugr sync status` execution-plan output for cloud/hybrid/remote sync decisions
- remote-only direct libSQL/Turso storage execution when `HUGR_STORAGE_MODE=remote` and credentials are explicit
- Hugr API sync backend contract metadata in `hugr sync status`, including endpoint, contract version, and route surface
- Hugr API sync client transport for explicit push, pull, and history requests against the `/v1/sync/*` contract
- `hugr daemon` exposes authenticated Hugr API sync routes for status, push, pull, and history, and records accepted API sync runs
- `hugr daemon` exposes authenticated `GET /v1/memories` and `POST /v1/memories` storage routes for hosted memory reads and writes without recording sync runs
- `hugr daemon` exposes authenticated `GET /v1/storage` and `POST /v1/storage` storage routes for hosted project, index, code graph, session, session-event, and session-promotion rows without recording sync runs
- Hugr API sync push and pull transfer and reconcile memory row payloads with the same overwrite/preserve behavior used by direct sync
- Hugr API sync push and pull transfer and reconcile all currently syncable safe row payload classes: project metadata, memories, memory/source embeddings, source references, discovered files, test mappings, entities, code symbols, graph edges, code references, and finalized sessions
- remote-mode Hugr API memory operations for `remember`, `recall`, memory counts, `forget`, and `improve` memory maintenance use hosted memory rows instead of a local libSQL connection
- remote-mode Hugr API project status, indexing, context assembly, impact analysis, session start/event/end/promotion, `hugr run`, and shell-observation event writes use hosted storage rows instead of a local libSQL connection
- ignored live integration coverage for remote-mode Hugr API memory, project, index, context, impact, session lifecycle, and session promotion commands spawning a daemon and client against localhost
- GitHub Actions CI runs `cargo fmt --check`, `cargo test`, and the ignored live Hugr API client/server contract smoke explicitly
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
- `hugr context` and `hugr_context` include deterministic evidence scores and reasons for files, symbols, graph neighbors, tests, memories, stale-memory risks, and session facts
- `hugr context` and `hugr_context` include graph neighborhoods from code references plus source/entity/edge rows in local and hosted Hugr API storage modes
- `hugr context` and `hugr_context` include deterministic risk signals for stale memory conflicts, changed relevant files, missing test mappings, missing symbol coverage, graph coupling, and recent failure session facts
- `hugr context` and `hugr_context` include index-freshness risk evidence for relevant files whose Hugr index timestamps are missing or older than local file modification time
- `hugr context` and `hugr_context` derive recent diagnostic risk evidence from captured command/session output terms such as compiler errors, warnings, panics, and unresolved imports
- `hugr run <command>` parses structured diagnostics from command output and stores file, line, severity, code, message, command, and timestamp in local or hosted Hugr API storage
- `hugr context` and `hugr_context` include structured diagnostics with exact locations, messages, citations, and risk signals
- `hugr symbols [--json] <query>` and MCP `hugr_symbols` provide a stable read-only symbol lookup surface over exact file/name targets or ranked symbol search
- `hugr replace-symbol [--json] [--kind <kind>] <path> <symbol>` and MCP `hugr_replace_symbol` safely replace one local indexed symbol after refusing ambiguous targets, renames, kind changes, and replacement bodies that fail to parse
- `hugr replace-symbol` refreshes the index immediately after a successful edit and records a session edit event when a session is active
- `hugr rename-symbol [--json] [--kind <kind>] <path> <symbol> <new-symbol>` and MCP `hugr_rename_symbol` safely rename one local symbol plus indexed inbound references after refusing ambiguous targets, invalid identifiers, stale reference lines, collisions, and files that fail to parse after the refactor
- `hugr rename-symbol` refreshes the index immediately after a successful refactor and records a session edit event when a session is active
- `hugr move-symbol [--json] [--kind <kind>] [--rewrite-references] <source-path> <symbol> <destination-path>` and MCP `hugr_move_symbol` safely move one local symbol between files after refusing unsupported inbound references, language mismatches, destination collisions, and files that fail to parse after the move
- `hugr move-symbol --rewrite-references` supports conservative Rust rewrites for indexed inbound references using exact module paths, simple and nested braced `use` imports, symbol aliases in imports, module aliases, and module-qualified call/reference lines
- `hugr move-symbol --rewrite-references` supports conservative Python rewrites for indexed inbound references using `from module import symbol`, symbol aliases, module imports, and module-qualified call/reference lines
- `hugr move-symbol --rewrite-references` supports conservative TypeScript and JavaScript ES-module rewrites for indexed inbound references using relative named imports/exports, symbol aliases, namespace imports, and extension-preserving module specifiers
- `hugr move-symbol --rewrite-references` supports same-package Go moves between files in one package directory by validating indexed references remain in the same directory and require no textual rewrite
- `hugr move-symbol --rewrite-references` supports same-package Java type moves by validating package declarations and indexed references for class, interface, enum, annotation, and record declarations that require no textual rewrite
- `hugr move-symbol --rewrite-references` supports same-package Kotlin type moves by validating package declarations and indexed references for class, interface, enum, annotation, object, and type-alias declarations that require no textual rewrite
- `hugr move-symbol --rewrite-references` supports same-module Swift type moves by validating module directories and indexed references for class, struct, enum, actor, protocol, extension, and type-alias declarations that require no textual rewrite
- `hugr move-symbol --rewrite-references` supports cross-package Kotlin and Java type moves by rewriting qualified imports in referencing files (inserting a fresh import in source-package files, rewriting the path in foreign-package files, and dropping the now-redundant import in destination-package files), refusing wildcard and aliased imports it cannot safely rewrite
- `hugr move-symbol --rewrite-references` supports exported Go cross-package moves by resolving the nearest enclosing `go.mod`, including nested module roots, and rewriting import paths plus package-qualified references
- `hugr move-symbol --rewrite-references` supports public/open Swift type moves across modules by resolving the nearest enclosing SwiftPM `Package.swift`, including nested packages, and inserting destination-module imports
- `hugr move-symbol` refreshes the index immediately after a successful move and records a session edit event when a session is active
- `hugr context` and `hugr_context` include a first code-health risk signal for large indexed symbols using deterministic symbol line ranges
- `hugr context` and `hugr_context` include cross-file refactor-surface risks when code graph references span multiple files
- `hugr context` and `hugr_context` include public/exported API surface risks using indexed symbol signatures and incoming reference evidence
- `hugr context` and `hugr_context` include low-severity unreferenced-private-symbol risks when selected private functions or methods have no incoming indexed references
- `hugr context` and `hugr_context` include stale-after-edit risks when Hugr edit events for selected files are newer than the latest persisted context pack
- `hugr context` and `hugr_context` include `stale_context` risks when a relevant file's on-disk modification time is newer than the latest persisted context pack, catching edits made outside Hugr edit commands
- `hugr context` and `hugr_context` merge persisted source-embedding vector matches into relevant-file selection and render source-embedding evidence ranks
- `hugr index --paths <p1,p2,...>` and daemon file events perform incremental re-indexing of only changed paths plus their inbound-reference sources
- code-reference edges distinguish imports, calls, member calls, implementations, inheritance, instantiations, type references, and generic references
- linear-time reference extraction using per-line identifier tokenization and hashed declaration lookups instead of per-target scans
- Rust `#[cfg(test)]` inline test modules map their own file as a test candidate in local, hosted, sync-record, and persisted mapping paths
- symbol retrieval and context evidence rank identifier-word and bounded-stem name matches above signature and path hits, prefer callable kinds on ties, and report which field matched
- graph neighborhoods collapse repeated reference sites per (kind, path, target) into one entry with a structured `site_count`, rank cross-file relationships above same-file ones, and name the defining file for cross-file targets in labels and citation ids
- initial vision, storage, and technical blueprint docs

Near-term parser and hosted API checklist is complete. Remaining broader product gaps are tracked below.

## Completion Gap Review

The near-term plan is close to complete, but the broader vision and technical blueprint still require several product systems before Hugr is complete:

- Daemon/runtime service: `hugr daemon` local HTTP transport, file watching, debounced background indexing, and periodic memory-maintenance audits exist.
- Remote/cloud execution: direct remote libSQL/Turso storage mode exists; `hugr sync status` exposes the Hugr API contract metadata; explicit API sync push, pull, and history requests can use a configured hosted endpoint; `hugr daemon` exposes authenticated `/v1/sync/*` routes that accept and reconcile all currently syncable safe row payload classes, then persist accepted runs; remote-mode Hugr API memory commands use authenticated `/v1/memories` storage routes; remote-mode project/index/context/impact/session lifecycle/session-promotion workflows use authenticated `/v1/storage` rows; an ignored live localhost integration test covers those hosted client/server paths; and GitHub Actions runs that live smoke explicitly.
- Context graph expansion: `hugr context` and `hugr_context` use code-reference plus source/entity/edge graph neighborhoods in local and hosted Hugr API storage modes.
- Structured memory provenance: promoted session memories preserve structured payloads in JSON, and CLI/MCP memory writes support manual source attachments plus confidence, sensitivity, validity metadata, and project scope.
- Automatic session observation: daemon captures file-change and git/worktree events for active sessions, `hugr run <command>` captures command/test outcomes, `hugr shell-hook <bash|zsh>` can observe ordinary shell command statuses, and daemon indexing captures classified discovery summaries.
- Session summarization and memory promotion: manual `hugr session promote` summarizes latest session facts into long-term memory, and the daemon periodically promotes ended, unpromoted sessions.
- Risk and health signals: context packs now include deterministic risks for stale memory conflicts, changed relevant files, missing tests/symbols, graph coupling, public/exported API surfaces, cross-file refactor surfaces, unreferenced private symbols, recent failure facts, index freshness, stale-after-edit and stale-context invalidation, recent diagnostic output, structured diagnostics with source locations, large indexed symbols, long parameter lists, high fan-in blast radius, deep nesting, and cyclomatic branching from indexed symbol bodies.
- Semantic operations: read-only CLI/MCP symbol lookup exists; safe local symbol replacement exists with parse and identity checks; the first reference-aware local rename exists for definitions plus indexed inbound references; reference-aware moves rewrite Rust, Python, TypeScript, JavaScript ES-module imports, and supported CommonJS `require` forms while rewriting CommonJS `module.exports` object/property exports; validate same-package Go/Java/Kotlin and same-module Swift moves; rewrite Java and Kotlin imports across packages; rewrite exported Go symbols across package directories using nearest `go.mod` module paths; and rewrite public/open Swift type moves across nearest `Package.swift` module directories by inserting destination-module imports.
- Incremental freshness: full and daemon indexing prune discovered-file, symbol, code-reference, test-mapping, and source-embedding rows (including dangling target edges) for files that no longer exist on disk; the daemon and `hugr index --paths` now re-index only changed paths plus their inbound-reference sources instead of the whole project, keeping cross-file references correct; indexing refreshes persisted heuristic test mappings and metadata-only source embeddings for changed source paths; context packs use persisted source embeddings as a semantic file-ranking signal; and context packs surface a `stale_context` risk when a cited file's on-disk modification time is newer than the latest persisted pack, closing the direct-edit staleness gap (persisted packs are audit/sync artifacts and are always recompiled, never re-served).

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
- Rust, Python, TypeScript, JavaScript/JSX, Go, Java, Kotlin, and Swift symbol extraction use tree-sitter when parsing succeeds, with line-scanner fallback.
- `hugr replace-symbol` and `hugr_replace_symbol` use indexed symbols plus parser validation to perform the first safe local structural edit.
- `hugr rename-symbol` and `hugr_rename_symbol` use indexed symbols and code references to safely rename a local definition plus inbound reference lines, then re-index.
- `hugr move-symbol` and `hugr_move_symbol` safely move an unreferenced local symbol between files with parser validation and destination collision checks, and can opt into Rust module-path, nested import, symbol-alias, and module-alias rewrites, Python import/call rewrites, TypeScript/JavaScript ES-module and CommonJS rewrites, same-package Go reference validation, same-package Java type reference validation, same-package Kotlin type reference validation, same-module Swift type reference validation, Java/Kotlin cross-package import rewrites, Go nearest-`go.mod` package rewrites, and SwiftPM nearest-`Package.swift` module import insertion for supported inbound references.

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
- Add deterministic risk signals.
- Add citations for every section.
- Add Markdown and JSON renderers.

Implemented first slices:

- `hugr context` and `hugr_context` rank files, symbols, graph neighbors, tests, memories, session facts, and stale-memory risks with deterministic evidence scores before token budgeting.
- `hugr context` and `hugr_context` include deterministic token budget metadata and remove lower-priority items when a pack exceeds the default budget.
- `hugr context` and `hugr_context` include unresolved stale-memory risks relevant to recalled task memories.
- Stale-memory risks render in Markdown and JSON with newer/older memory evidence, shared terms, deterministic signal, and `stale_memory` citations.
- `hugr context` persists each compiled pack as an opaque JSON payload in `context_packs` with project/task/timestamps for later audit and sync.
- Graph neighborhoods render in Markdown and JSON with `graph` citations, drawing from code references plus source/entity/edge rows in local and hosted API storage.
- Risk signals render in Markdown and JSON with `risk` citations, covering stale-memory conflicts, changed relevant files, missing test mappings, missing symbol coverage, graph coupling, and recent failure session facts.
- Index freshness checks compare relevant files against local or hosted Hugr index timestamps and surface missing/stale index evidence as risk signals.
- Recent command/session output with diagnostic terms is compacted into `recent_diagnostics` risk signals.
- `hugr run` stores parsed structured diagnostics from stdout/stderr, and context packs render matching diagnostics with `diagnostic` citations plus `structured_diagnostics` risk signals.
- `hugr symbols` and `hugr_symbols` refresh the index, then return matching symbol paths, languages, kinds, ranges, and signatures.
- Context packs derive `large_symbol` code-health risks from indexed symbol line ranges so agents see large edit targets before changing them.
- Context packs derive `refactor_surface` risks from code graph neighbors when incoming, outgoing, or path references span multiple files.
- Context packs derive `public_api_surface` risks from public/exported symbol signatures and incoming reference counts.
- Context packs derive `unreferenced_private_symbol` risks when selected private functions or methods have no incoming indexed references.
- Context packs derive `stale_after_edit` risks when selected files have Hugr edit session events newer than the latest persisted context pack, in both local and hosted API storage paths.
- Context packs derive `long_parameter_list` code-health risks from indexed callable signatures whose top-level parameter count exceeds a deterministic threshold.
- Context packs derive `high_fan_in` blast-radius risks when a symbol is referenced from many distinct files in the code graph, independent of visibility.

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
- Remote mode opens direct remote libSQL/Turso storage when explicitly configured; Hugr API remote mode routes memory commands through hosted memory storage and project/index/context/impact/session lifecycle/session-promotion commands through hosted general storage, while unsupported local database commands still fail with a hosted-storage-operations error instead of silently writing locally.
- Hybrid mode keeps local storage active while guarded direct remote sync remains opt-in.
- `HUGR_SYNC_CLASSES` represents safe default sync classes and explicit opt-ins for full source, raw command output, secrets, and private notes.
- `HUGR_SYNC_BACKEND=direct_libsql|hugr_api` records the execution strategy decision, defaulting to direct libSQL/Turso.
- `hugr sync status [--json]` renders the current sync execution plan, with guarded remote reads and writes available for explicitly configured hybrid or remote direct libSQL/Turso storage.
- For `HUGR_SYNC_BACKEND=hugr_api`, `hugr sync status [--json]` exposes the configured endpoint, `hugr-api-v1` contract version, and the planned `/v1/memories` plus `/v1/sync/*` route surface.
- For explicit API sync execution, Hugr posts contract JSON to `POST /v1/sync/push` and `POST /v1/sync/pull`, fetches `GET /v1/sync/history`, and parses hosted table counters plus memory row payloads into the existing sync result types.
- `hugr daemon` serves authenticated `GET /v1/memories`, `POST /v1/memories`, `GET /v1/storage`, `POST /v1/storage`, `GET /v1/sync/status`, `POST /v1/sync/push`, `POST /v1/sync/pull`, and `GET /v1/sync/history` routes using `HUGR_API_TOKEN` or `HUGR_REMOTE_AUTH_TOKEN` as the bearer token.
- Hosted API push and pull validate the contract version, transfer project/memory/memory-embedding/source/source-embedding/discovered-file/test-mapping/entity/code-symbol/edge/code-reference/finalized-session/context-pack row payloads, apply supported rows server-side on push, return supported rows on pull, and persist accepted runs into sync history.
- Hosted memory storage validates the contract version, applies project/memory/embedding payloads through the same row reconciliation path without recording sync runs, and returns memory rows for remote-mode recall, memory counts, forget, and improve operations.
- Hosted general storage validates the contract version, applies project/source/source-embedding/discovered-file/test-mapping/entity/code-symbol/edge/code-reference/session/context-pack rows plus private session-event and session-promotion rows without recording sync runs, and supports remote-mode project status, indexing, context assembly, impact analysis, session lifecycle, session promotion, command observation, and shell observation.
- `tests/hugr_api_live.rs` provides an ignored live daemon/client contract test for remote-mode memory, project, index, context, impact, session lifecycle, and session promotion commands and verifies hosted storage does not create client-local `.hugr` state or sync-history rows.
- `.github/workflows/ci.yml` runs formatting, the full Rust test suite, and the ignored live Hugr API contract smoke on push and pull request.
- `hugr sync push [--dry-run|--execute] [--json]` counts configured sync-class tables locally by default, writes to direct libSQL/Turso when `--execute` and hybrid remote config are explicit, or posts to the Hugr API backend when selected.
- `hugr sync pull [--dry-run|--execute] [--json]` counts configured sync-class tables locally by default, reads from direct libSQL/Turso when `--execute` and hybrid remote config are explicit, or posts to the Hugr API backend when selected.
- Sync push currently covers project metadata, memories, memory/source embeddings, source references, discovered files, test mappings, entity/code-symbol indexes, graph/code-reference edges, finalized session summaries, and context packs. It does not sync raw session events, shell output, secrets, private notes, or full source without future explicit data sources.
- Sync pull reconciles the same safe table set with conservative merge rules: local memories are not clobbered, project, code-index, and context-pack rows update only when the remote record is newer, dependent embeddings are skipped until their memory exists, and finalized sessions only replace open or older local summaries.
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
59. Done: `feat(api): add remote project storage`
60. Done: `feat(api): add remote session promotion`
61. Done: `ci(api): run live contract smoke`
62. Done: `feat(parser): use tree-sitter kotlin`
63. Done: `feat(context): persist context packs`
64. Done: `feat(context): add graph neighborhoods`
65. Done: `feat(context): add risk signals`
66. Done: `feat(context): add index freshness risks`
67. Done: `feat(context): derive diagnostic risks`
68. Done: `feat(context): ingest structured diagnostics`
69. Done: `feat(symbols): add lookup surface`
70. Done: `feat(edit): replace symbols safely`
71. Done: `feat(context): flag large symbols`
72. Done: `feat(context): flag refactor surfaces`
73. Done: `feat(context): flag public api surfaces`
74. Done: `feat(context): flag unreferenced private symbols`
75. Done: `feat(context): flag edits after context`
76. Done: `feat(edit): rename symbols with references`
77. Done: `feat(edit): move unreferenced symbols`
78. Done: `feat(edit): rewrite references on move`
79. Done: `feat(edit): broaden Rust move rewrites`
80. Done: `feat(edit): rewrite Python move references`
81. Done: `feat(edit): rewrite TS and JS move references`
82. Done: `feat(edit): allow same-package Go moves`
83. Done: `feat(edit): allow same-package Java type moves`
84. Done: `feat(edit): allow same-package Kotlin and Swift moves`
85. Done: `feat(index): prune deleted file rows`
86. Done: `feat(index): add incremental path refresh`
87. Done: `feat(context): flag stale context packs`
88. Done: `feat(edit): rewrite Kotlin cross-package moves`
89. Done: `feat(edit): rewrite Java cross-package moves`
90. Done: `feat(context): flag long parameter lists`
91. Done: `feat(context): flag high fan-in symbols`
92. Done: `feat(context): flag body complexity`
93. Done: `feat(edit): handle move source references`
94. Done: `feat(context): rank source embeddings`
95. Done: `feat(edit): resolve nested manifests`
96. Done: `perf(code): index reference targets by name`
97. Done: `feat(testmap): map rust inline test modules`
98. Done: `feat(context): rank symbols by name relevance`
99. Done: `feat(context): collapse graph reference sites`

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

A dogfooding review of `hugr context` on this repository fixed four output-quality gaps: reference extraction was accidentally quadratic (a full index of this repo never finished; it now takes seconds), Rust files with inline `#[cfg(test)]` modules produced false `missing_test_mapping` risks, symbol ranking listed top-of-file declarations because every symbol in a path-matched file scored identically, and graph neighborhoods spent most of their token budget on repeated same-file reference lines with ambiguous labels. Context packs on this repository now surface the task-relevant functions first, map inline tests, and compress the graph section into distinct cross-file relationships.

The planned product systems are complete. Incremental freshness (deletion pruning, watcher-scoped partial refresh, persisted test-map/source-embedding refresh, semantic source-embedding file ranking, and `stale_context` invalidation), broader structural edits (Kotlin/Java import rewriting, manifest-resolved Go package moves, manifest-resolved Swift module moves, CommonJS `require` and export rewrites), safer source/destination-file reference handling during moves, and deeper deterministic risk signals (`long_parameter_list`, `high_fan_in`, `deep_nesting`, `cyclomatic_complexity`) all landed with unit and live coverage.

The remaining items are optional deep extensions, each opening a substantial new design space rather than closing a committed gap:

- CommonJS support remains intentionally conservative for one-line `require` and export assignments; broader dynamic export patterns would need a deeper JavaScript module model.
- Go and Swift manifest support now resolves nearest nested `go.mod` and SwiftPM `Sources/<Module>` roots; monorepos with generated manifests or custom build systems remain future work.

None of these is required for the core `hugr context` experience, which is complete: it compiles ranked memories, files, symbols, graph neighborhoods, tests, git state, structured diagnostics, and a broad deterministic risk surface with citations, in local and hosted API storage modes.
