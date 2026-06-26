# Hugr Storage

Hugr uses libSQL/Turso Vector as its primary storage layer.

The goal is to keep the early product simple and powerful: memory records, provenance, full-text search, graph edges, temporal fields, and embeddings live in one SQLite-compatible database.

## Local Path

```text
.hugr/hugr.db
```

## Why This Fits

- It is SQLite-compatible, so local mode stays simple.
- It supports remote libSQL/Turso deployments, so cloud and hybrid modes do not require a storage rewrite.
- It has built-in vector columns and vector search, so Hugr does not need a separate vector database.
- It lets Hugr join semantic search results back to memories, sources, sessions, entities, and graph edges with normal SQL.

## Initial Schema Direction

Memory rows are stored in `memories`.

Embedding rows are stored in `memory_embeddings`:

```sql
CREATE TABLE memory_embeddings (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL DEFAULT 1536,
    embedding F32_BLOB(1536)
);
```

The first implementation creates the vector-ready table before embedding generation exists. The first implementation also creates `memory_embeddings_vector_idx` with `libsql_vector_idx(embedding)`. When embeddings are wired in, Hugr should query it with `vector_top_k` and use vector search as one signal in recall and context-pack generation.

## Retrieval Plan

Recall should rank evidence from multiple signals:

1. Full-text memory search.
2. Vector similarity over memory embeddings.
3. Graph expansion from linked files, symbols, sources, sessions, and entities.
4. Recency, confidence, validity windows, and stale-fact penalties.

The context compiler should merge those signals into cited context instead of exposing vector search as the main user interface.
