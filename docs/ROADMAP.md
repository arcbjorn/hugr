# Hugr Roadmap

All planned product systems have a first shipped implementation. This roadmap tracks what comes next; the shipped surface is documented in the [README](../README.md) and [ARCHITECTURE.md](ARCHITECTURE.md).

## Now

- **Ranking quality.** The eval baseline on this repository (30 commits, from a freshly indexed database) is file recall ~0.80, hit rate ~0.95, MRR ~0.72. MRR was 0.274 before the lexical/embedding blend landed; ranking order is no longer the standout weakness, so the remaining headroom is in recall.
- **Eval stability.** `hugr eval` is deterministic for a fixed database but mutates the one it scores — each case persists a context pack and refreshes candidate indexes, so consecutive runs drift. Across clean-database runs the metrics move by ~0.02 recall and ~0.03 hit rate (one commit flipping is worth 0.033), which is wider than most single ranking changes. Score a change against a rebuilt database, and treat differences below that band as noise.
- **Eval portability.** Run `hugr eval` on two or three foreign repositories before turning the CI job into a `--min-hit-rate` gate, so thresholds are not overfitted to this repository's commit style. The gate needs a threshold below the noise band above, or it will fail on unchanged code.

## Next

- Replace the curl subprocess with a proper HTTP client (`ureq` or async `reqwest`) in the embedding, LLM, and API sync transports.
- Move the daemon's hand-parsed HTTP handling onto `axum`.
- Add a Codex `hugr install` target and an install-time `hugr index` kick-off.
- Decide whether `local` (in-process ONNX) replaces `deterministic` as the default embedding provider once first-run download UX (progress reporting, offline behavior) is settled.

## Later

- A `scip` importer for LSP-grade reference graphs.
- Broader dynamic CommonJS export patterns in `move-symbol` rewrites (needs a deeper JavaScript module model).
- Go and Swift moves in monorepos with generated manifests or custom build systems.
