# Hugr Storage

Hugr uses libSQL/Turso Vector as its primary storage layer.

The goal is to keep the early product simple and powerful: memory records, provenance, full-text search, graph edges, temporal fields, and embeddings live in one SQLite-compatible database.

## Local Paths

```text
.hugr/hugr.db    project store
~/.hugr/         global memory store and local embedding model cache
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

`memory_embeddings_vector_idx` is created with `libsql_vector_idx(embedding)` and queried with `vector_top_k`. Embeddings are generated synchronously on `remember` by the configured provider (`HUGR_EMBEDDING_PROVIDER=deterministic|openai|ollama|local`); all vectors normalize to the 1536-wide columns so provider dimensions can differ.

## Retrieval

Recall ranks evidence from multiple signals:

1. Full-text memory search.
2. Vector similarity over memory embeddings.
3. Graph expansion from linked files, symbols, sources, sessions, and entities.
4. Recency, confidence, validity windows, and stale-fact penalties.

The context compiler should merge those signals into cited context instead of exposing vector search as the main user interface.
