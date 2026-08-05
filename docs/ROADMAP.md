# Hugr Roadmap

All planned product systems have a first shipped implementation. This roadmap tracks what comes next; the shipped surface is documented in the [README](../README.md) and [ARCHITECTURE.md](ARCHITECTURE.md).

## Now

- **Ranking quality.** The eval baseline on this repository (30 commits, from a freshly indexed database) is file recall ~0.80, hit rate ~0.95, MRR ~0.72. MRR was 0.274 before the lexical/embedding blend landed; ranking order is no longer the standout weakness, so the remaining headroom is in recall.
- **Eval stability.** `hugr eval` is deterministic for a fixed database but mutates the one it scores — each case persists a context pack and refreshes candidate indexes, so consecutive runs drift. Across clean-database runs the metrics move by ~0.02 recall and ~0.03 hit rate (one commit flipping is worth 0.033), which is wider than most single ranking changes. Score a change against a rebuilt database, and treat differences below that band as noise.
- **Eval portability.** Measured on four foreign repositories. The metrics are heavily overfitted to this one, and they degrade with repository size:

  | repository | commits | file recall | hit rate | MRR |
  | --- | --- | --- | --- | --- |
  | hugr | 0.1k | ~0.75 | ~0.90 | ~0.72 |
  | repo A (TypeScript + Go app) | 0.9k | 0.505 | 0.567 | 0.444 |
  | repo B (Go + Python services) | 1.0k | 0.421 | 0.500 | 0.383 |
  | repo C (Go service) | 1.8k | 0.368 | 0.533 | 0.378 |
  | repo D (large TypeScript app) | 12.6k | 0.151 | 0.267 | 0.183 |

  A `--min-hit-rate` gate must be set from foreign-repository numbers, not this one, and below the noise band above. Retrieval quality on other people's repositories — not this one — is the real remaining headroom, and the largest repository is where it is worst by a wide margin.

- **Ranking on foreign layouts.** The exact-name and test-file work in `discovery::candidate_for` came out of the measurement above and moved the Go repository a long way (recall 0.260 → 0.368, hit rate 0.333 → 0.533), at a cost of ~0.03 recall on the TypeScript app.

  The obvious follow-up — treating the parent directory as the stem when the file name is generic (`index.ts`, `main.go`), so a `components/Thing/index.ts` entry point competes on its directory — was **implemented and measured, and it loses**: repo B 0.421 → 0.387 recall, repo D 0.151 → 0.147 recall and 0.267 → 0.233 hit rate. It was reverted. The plausible reading is that promoting a directory name to an exact match creates ties across every sibling file in that directory, which costs more than the occasional correct entry-point hit gains. Do not re-attempt without a different tie-break.

  The candidate-set size this previously pointed at was **measured and ruled out**. Sweeping the `discover_candidate_files` limit from its hardcoded 12 up to 96 on repo D leaves file recall and hit rate completely flat (0.144 / 0.267 at 24, 48, and 96); the mid-size repo C gains only ~0.01 recall and ~0.03 hit rate going 12 → 24, then plateaus. Widening the candidate set is not where the loss is.

- **Prose commit subjects, and the `retrievable_rate` reference line.** Measuring what repo D's subjects contain explains its low scores better than any ranking change: **12 of its 30 eval commits share no term at all with the paths they touch** — subjects like `fix`, `fix ci`, `events`, `change tabs`, `cancel query`. Filename ranking has nothing to work with there.

  `hugr eval` now reports `retrievable_rate`: the share of cases whose subject shares a term with an expected path. It is a reference line rather than a hard cap — embedding and graph evidence can retrieve a file with no lexical overlap — but comparing it against `hit_rate` separates the two failure modes:

  | repository | hit rate | retrievable rate | reading |
  | --- | --- | --- | --- |
  | hugr | 0.900 | 0.767 | ranking already exceeds the lexical signal; little headroom |
  | repo D | 0.267 | 0.600 | ~0.33 of genuinely addressable gap, ~0.40 unretrievable |

  So repo D's real target is ~0.60, not 1.0, and a `--min-hit-rate` gate must be calibrated against that or it encodes an impossible number.

- **Symbol matches now rank files, which closed a third of that gap.** `symbol_file_hit_rate` had been running above `hit_rate` on every repository measured — the symbol index resolved the right file while file ranking, which sees only names and paths, did not. Those symbol paths were computed *after* files were ranked and never fed back in.

  Recalling symbols first and boosting any candidate that defines one (`discovery::promote_symbol_paths`) moved repo D substantially:

  | | before | after |
  | --- | --- | --- |
  | file recall | 0.151 | 0.186 |
  | hit rate | 0.267 | 0.367 |
  | MRR | 0.183 | 0.267 |

  hugr itself gains MRR (0.773 → 0.823) and holds recall and hit rate, so this is not a trade. `symbol_file_hit_rate` is unchanged, confirming the win comes from using an existing signal rather than changing symbol search.

  The bonus is 6, deliberately large enough to draw level with an exact filename match. A value of 3 — which guarantees the filename always wins — was measured and retrieved less (hit rate 0.333, recall 0.169).

  A follow-up went further: a symbol file absent from the candidate set entirely is now *inserted*, not just boosted, because `symbol_file_hit_rate` exceeded even `candidate_hit_rate` on repo D. The two repositories responded very differently:

  | | hugr before | hugr after | repo D before | repo D after |
  | --- | --- | --- | --- | --- |
  | file recall | 0.788 | **0.844** | 0.186 | 0.186 |
  | hit rate | 0.900 | **0.967–1.000** | 0.367 | 0.367 |
  | MRR | 0.823 | **0.853** | 0.244 | 0.250 |
  | candidate hit rate | 0.900 | — | 0.400 | **0.500** |

  On hugr this is a large, reproducible gain across three clean-database runs. On repo D the insertion demonstrably works — candidate hit rate rose 0.400 → 0.500 — but only **1 of 30 commits** improved its rank (3 → 2), so the headline metrics did not move. Getting a file into the candidate set is necessary and not sufficient there; it still has to out-rank a crowded field and survive the token budget.

  That "necessary but not sufficient" turned out to be a budget problem, not a ranking one. Eviction compared raw evidence scores, ignoring that pack items differ in size: a file entry costs ~35 tokens (a path and a short reason) against ~72–78 for a symbol or graph neighbour, *and* files carry the lowest base score. So the cheapest, highest-value evidence lost every comparison — on repo D the budget kept 8 symbols and 7 graph neighbours while truncating 11 of 14 files down to the retention floor.

- **Evicting by value per token.** `cost_adjusted_score` divides an item's evidence score by its estimated token cost, so the choice becomes "what is worth its space" rather than "what scores highest". Within an unchanged 4000-token budget the same pack went from 3 files to 14, trading away three graph neighbours.

  | | hugr before | hugr after | repo D before | repo D after |
  | --- | --- | --- | --- | --- |
  | file recall | 0.844 | **0.880** | 0.186 | **0.283** |
  | hit rate | 0.967 | 0.967 | 0.367 | **0.500** |
  | MRR | 0.853 | 0.853 | 0.250 | **0.288** |

  This is the largest single retrieval gain measured so far, and the first change that moved repo D substantially. `hit_rate` now equals `candidate_hit_rate` (0.500) there: every file that reaches the candidate set survives into the pack, so the budget is no longer the bottleneck and the remaining loss is candidate generation.

  Caveat on confidence: the hugr numbers are two clean-database runs (recall 0.880 twice), but repo D is a **single** run — repeat attempts were cut short by the eval's runtime on a 12.6k-commit repository. The section-count change (3 files → 14 within an unchanged token total) is direct mechanism evidence and does not depend on that run, but the exact repo D figures should be re-measured before they are quoted as a baseline.

  Remaining headroom on repo D is ~0.10 against its ~0.60 retrievability ceiling. The next evidence worth trying is what neither names nor symbols carry: commit-message-to-diff history, or a real embedding model.

## Next

- Replace the curl subprocess with a proper HTTP client (`ureq` or async `reqwest`) in the embedding, LLM, and API sync transports.
- Move the daemon's hand-parsed HTTP handling onto `axum`.
- Add a Codex `hugr install` target and an install-time `hugr index` kick-off.
- Decide whether `local` (in-process ONNX) replaces `deterministic` as the default embedding provider once first-run download UX (progress reporting, offline behavior) is settled.

## Later

- A `scip` importer for LSP-grade reference graphs.
- Broader dynamic CommonJS export patterns in `move-symbol` rewrites (needs a deeper JavaScript module model).
- Go and Swift moves in monorepos with generated manifests or custom build systems.
