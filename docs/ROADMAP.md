# Hugr Roadmap

All product systems from the [technical blueprint](TECHNICAL_BLUEPRINT.md) have a first shipped implementation. This roadmap tracks what comes next; the shipped surface is documented in the [README](../README.md) and the blueprint.

## Now

- **Ranking quality.** The eval baseline on this repository (30 commits) is file recall 0.925, hit rate 0.933, MRR 0.274. The right files are almost always retrieved but rarely ranked near the top, so ranking-order work can move MRR without risking recall.
- **Eval portability.** Run `hugr eval` on two or three foreign repositories before turning the CI job into a `--min-hit-rate` gate, so thresholds are not overfitted to this repository's commit style.

## Next

- Replace the curl subprocess with a proper HTTP client (`ureq` or async `reqwest`) in the embedding, LLM, and API sync transports.
- Move the daemon's hand-parsed HTTP handling onto `axum`.
- Replace hand-escaped JSON rendering in context packs with serde derive.
- Add a Codex `hugr install` target and an install-time `hugr index` kick-off.
- Decide whether `local` (in-process ONNX) replaces `deterministic` as the default embedding provider once first-run download UX (progress reporting, offline behavior) is settled.

## Later

- A `scip` importer for LSP-grade reference graphs.
- Broader dynamic CommonJS export patterns in `move-symbol` rewrites (needs a deeper JavaScript module model).
- Go and Swift moves in monorepos with generated manifests or custom build systems.
