# Hugr Vision

Hugr is a project memory and intelligence system for agents.

It gives agents a durable, structured understanding of a project: what the code does, what changed, what was learned, what failed before, what decisions still matter, and what context is needed for the next task.

## Product Principle

Hugr should not just store memories. It should decide what an agent needs to know right now.

The flagship primitive is:

```bash
hugr context "task"
```

That command should compile task-relevant project memory, code structure, git state, prior sessions, and operational facts into concise, cited context an agent can act on.

## Deployment Model

Hugr is deployment-flexible:

- Local mode for private repos, personal agents, and fast indexing.
- Cloud mode for hosted agents, remote workers, and always-on memory.
- Hybrid mode when code intelligence should run near the repository while memory and orchestration run elsewhere.

The same core concepts should work in every mode.

## Core Capabilities

### Memory Lifecycle

- Remember facts, decisions, sessions, preferences, and observations.
- Recall relevant context for a task, file, symbol, branch, or session.
- Improve memory by merging duplicates, resolving contradictions, and promoting useful session notes.
- Forget stale, wrong, sensitive, or intentionally removed memory.

### Structured Project Memory

- Store typed entities and relationships rather than raw text alone.
- Link facts to sources, files, symbols, commits, sessions, commands, and tests.
- Track confidence, recency, provenance, and validity.
- Allow facts to become stale, superseded, or commit-scoped.

### Code Intelligence

- Discover files quickly.
- Extract symbols and relationships.
- Track callers, callees, imports, dependencies, tests, and changed files.
- Understand branch and worktree state.
- Estimate impact before an agent edits code.
- Surface code health signals such as complexity, coupling, dead code, and risky paths.

### Semantic Operations

- Find exact symbols and references.
- Use diagnostics to guide safe changes.
- Support targeted symbol-aware edits where they reduce agent mistakes.
- Prefer precise structural operations over brittle text replacement.

### Agent Hooks

- Start sessions with relevant project context.
- Observe file edits, shell commands, git operations, and test outcomes.
- Keep indexes fresh as the project changes.
- Summarize useful discoveries at session end.
- Promote durable learnings into long-term memory.

### Context Compiler

The context compiler is the product center.

Given a task, it should:

1. Interpret the task.
2. Find relevant files, symbols, memories, sessions, tests, and risks.
3. Expand through graph relationships.
4. Remove stale or low-confidence facts.
5. Compress the result into an agent-ready context pack.
6. Cite sources so the agent can verify assumptions.

## First User Experience

```bash
hugr init
hugr index
hugr remember "plugin hooks run after configuration is loaded"
hugr recall "plugin hooks"
hugr context "add lifecycle hooks to plugins"
hugr impact src/plugins/manager.ts
hugr improve
hugr forget "plugin hooks"
hugr doctor
```

## Context Pack Shape

A good context pack should include:

- Relevant files.
- Important symbols.
- Prior memories.
- Recent session facts.
- Current branch changes.
- Affected tests.
- Risks and stale facts.
- Suggested path.
- Citations.

## Architecture

```text
hugr daemon
  memory engine
    remember / recall / improve / forget
    graph + vector + provenance
    temporal facts
    session consolidation

  code intelligence engine
    fast file discovery
    symbol graph
    git and branch awareness
    impact and affected tests
    code health signals

  semantic operation engine
    symbol lookup
    references
    diagnostics
    safe edits

  context compiler
    task understanding
    retrieval
    graph expansion
    ranking
    compression
    citations
```

## Status

All capabilities in this vision have a first shipped implementation. See the [README](README.md) for the shipped surface and [docs/ROADMAP.md](docs/ROADMAP.md) for what comes next.
