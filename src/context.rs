use crate::code::CodeSymbol;
use crate::discovery::FileCandidate;
use crate::store::{
    Diagnostic, FreshnessSignal, GraphNeighbor, Memory, SessionFact, StaleMemoryCandidate,
};
use crate::testmap::TestCandidate;
use crate::worktree::WorktreeState;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

const DEFAULT_CONTEXT_TOKEN_BUDGET: usize = 4000;
const LARGE_SYMBOL_LINE_THRESHOLD: i64 = 80;
const VERY_LARGE_SYMBOL_LINE_THRESHOLD: i64 = 200;
const REFACTOR_SURFACE_FILE_THRESHOLD: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPack {
    pub task: String,
    pub budget: ContextBudget,
    pub relevant_files: Vec<ContextFile>,
    pub important_symbols: Vec<ContextSymbol>,
    pub graph_neighbors: Vec<ContextGraphNeighbor>,
    pub affected_tests: Vec<ContextTest>,
    pub relevant_memories: Vec<ContextMemory>,
    pub stale_memory_risks: Vec<ContextStaleMemoryRisk>,
    pub diagnostics: Vec<ContextDiagnostic>,
    pub risk_signals: Vec<ContextRiskSignal>,
    pub recent_sessions: Vec<ContextSessionFact>,
    pub branch_state: Option<ContextBranchState>,
    pub suggested_path: Vec<String>,
    pub citations: Vec<Citation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudget {
    pub max_tokens: usize,
    pub estimated_tokens: usize,
    pub truncated_sections: Vec<ContextBudgetTruncation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudgetTruncation {
    pub section: String,
    pub removed_items: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    pub path: String,
    pub citation_id: String,
    pub evidence_score: usize,
    pub evidence_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSymbol {
    pub path: String,
    pub language: Option<String>,
    pub name: String,
    pub kind: String,
    pub line_start: i64,
    pub line_end: Option<i64>,
    pub signature: String,
    pub citation_id: String,
    pub evidence_score: usize,
    pub evidence_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextGraphNeighbor {
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub path: Option<String>,
    pub target_path: Option<String>,
    pub target_name: Option<String>,
    pub line_start: Option<i64>,
    pub citation_id: String,
    pub evidence_score: usize,
    pub evidence_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextTest {
    pub path: String,
    pub reason: String,
    pub citation_id: String,
    pub evidence_score: usize,
    pub evidence_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMemory {
    pub id: String,
    pub created_at_ms: i64,
    pub kind: String,
    pub text: String,
    pub structured_payload: Option<String>,
    pub citation_id: String,
    pub evidence_score: usize,
    pub evidence_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextStaleMemoryRisk {
    pub reason: String,
    pub signal: String,
    pub shared_terms: Vec<String>,
    pub newer_memory: ContextMemory,
    pub older_memory: ContextMemory,
    pub citation_id: String,
    pub evidence_score: usize,
    pub evidence_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDiagnostic {
    pub id: String,
    pub source: String,
    pub path: Option<String>,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub severity: String,
    pub code: Option<String>,
    pub message: String,
    pub command: Option<String>,
    pub created_at_ms: i64,
    pub citation_id: String,
    pub evidence_score: usize,
    pub evidence_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRiskSignal {
    pub severity: String,
    pub kind: String,
    pub summary: String,
    pub citation_id: String,
    pub evidence_score: usize,
    pub evidence_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSessionFact {
    pub session_id: String,
    pub kind: String,
    pub detail: String,
    pub created_at_ms: i64,
    pub citation_id: String,
    pub evidence_score: usize,
    pub evidence_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBranchState {
    pub root_path: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub changed_files: Vec<ContextChangedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextChangedFile {
    pub path: String,
    pub original_path: Option<String>,
    pub staged_status: Option<String>,
    pub unstaged_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub id: String,
    pub source_type: String,
    pub label: String,
}

impl ContextPack {
    #[cfg(test)]
    pub fn new(task: &str, files: Vec<String>, memories: Vec<Memory>) -> Self {
        Self::with_sessions(task, files, memories, Vec::new())
    }

    #[cfg(test)]
    pub fn with_sessions(
        task: &str,
        files: Vec<String>,
        memories: Vec<Memory>,
        sessions: Vec<SessionFact>,
    ) -> Self {
        Self::with_sessions_symbols_tests_and_branch(
            task,
            files,
            memories,
            sessions,
            Vec::new(),
            Vec::new(),
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_sessions_symbols_tests_and_branch(
        task: &str,
        files: Vec<String>,
        memories: Vec<Memory>,
        sessions: Vec<SessionFact>,
        symbols: Vec<CodeSymbol>,
        tests: Vec<TestCandidate>,
        branch_state: Option<WorktreeState>,
    ) -> Self {
        Self::with_sessions_symbols_tests_branch_and_stale_risks(
            task,
            files,
            memories,
            sessions,
            symbols,
            tests,
            branch_state,
            Vec::new(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_sessions_symbols_tests_branch_and_stale_risks(
        task: &str,
        files: Vec<String>,
        memories: Vec<Memory>,
        sessions: Vec<SessionFact>,
        symbols: Vec<CodeSymbol>,
        tests: Vec<TestCandidate>,
        branch_state: Option<WorktreeState>,
        stale_candidates: Vec<StaleMemoryCandidate>,
    ) -> Self {
        let file_candidates = files
            .into_iter()
            .map(|path| FileCandidate {
                path,
                score: 0,
                language: None,
                size_bytes: None,
            })
            .collect::<Vec<_>>();
        Self::with_file_candidates_sessions_symbols_tests_branch_and_stale_risks(
            task,
            file_candidates,
            memories,
            sessions,
            symbols,
            tests,
            branch_state,
            stale_candidates,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_file_candidates_sessions_symbols_tests_branch_and_stale_risks(
        task: &str,
        file_candidates: Vec<FileCandidate>,
        memories: Vec<Memory>,
        sessions: Vec<SessionFact>,
        symbols: Vec<CodeSymbol>,
        tests: Vec<TestCandidate>,
        branch_state: Option<WorktreeState>,
        stale_candidates: Vec<StaleMemoryCandidate>,
    ) -> Self {
        Self::with_file_candidates_sessions_symbols_tests_branch_stale_risks_and_graph(
            task,
            file_candidates,
            memories,
            sessions,
            symbols,
            tests,
            branch_state,
            stale_candidates,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    pub(crate) fn with_file_candidates_sessions_symbols_tests_branch_stale_risks_and_graph(
        task: &str,
        file_candidates: Vec<FileCandidate>,
        memories: Vec<Memory>,
        sessions: Vec<SessionFact>,
        symbols: Vec<CodeSymbol>,
        tests: Vec<TestCandidate>,
        branch_state: Option<WorktreeState>,
        stale_candidates: Vec<StaleMemoryCandidate>,
        graph_neighbors: Vec<GraphNeighbor>,
        freshness_signals: Vec<FreshnessSignal>,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        let terms = context_query_terms(task);
        let relevant_files = file_candidates
            .into_iter()
            .map(|candidate| {
                let (evidence_score, evidence_reason) = file_evidence(&candidate, &terms);
                ContextFile {
                    citation_id: format!("file:{}", candidate.path),
                    path: candidate.path,
                    evidence_score,
                    evidence_reason,
                }
            })
            .collect::<Vec<_>>();
        let important_symbols = symbols
            .into_iter()
            .map(|symbol| {
                let (evidence_score, evidence_reason) = symbol_evidence(&symbol, &terms);
                ContextSymbol {
                    citation_id: format!(
                        "symbol:{}:{}:{}",
                        symbol.path, symbol.line_start, symbol.name
                    ),
                    path: symbol.path,
                    language: symbol.language,
                    name: symbol.name,
                    kind: symbol.kind,
                    line_start: symbol.line_start,
                    line_end: symbol.line_end,
                    signature: symbol.signature,
                    evidence_score,
                    evidence_reason,
                }
            })
            .collect::<Vec<_>>();
        let graph_neighbors = graph_neighbors
            .into_iter()
            .map(|neighbor| {
                let (evidence_score, evidence_reason) = graph_evidence(&neighbor, &terms);
                ContextGraphNeighbor {
                    citation_id: graph_neighbor_citation_id(&neighbor),
                    kind: neighbor.kind,
                    label: neighbor.label,
                    detail: neighbor.detail,
                    path: neighbor.path,
                    target_path: neighbor.target_path,
                    target_name: neighbor.target_name,
                    line_start: neighbor.line_start,
                    evidence_score,
                    evidence_reason,
                }
            })
            .collect::<Vec<_>>();
        let affected_tests = tests
            .into_iter()
            .map(|test| ContextTest {
                citation_id: format!("test:{}", test.path),
                evidence_score: 500 + test.score * 5,
                evidence_reason: format!("test-map score {}: {}", test.score, test.reason),
                path: test.path,
                reason: test.reason,
            })
            .collect::<Vec<_>>();
        let memory_count = memories.len();
        let relevant_memories = memories
            .into_iter()
            .enumerate()
            .map(|(index, memory)| {
                let (evidence_score, evidence_reason) =
                    memory_evidence(&memory, &terms, Some((index, memory_count)));
                context_memory_from(memory, evidence_score, evidence_reason)
            })
            .collect::<Vec<_>>();
        let stale_memory_risks = stale_candidates
            .into_iter()
            .map(|candidate| {
                let (evidence_score, evidence_reason) = stale_memory_evidence(&candidate, &terms);
                let (newer_score, newer_reason) =
                    memory_evidence(&candidate.newer_memory, &terms, None);
                let (older_score, older_reason) =
                    memory_evidence(&candidate.older_memory, &terms, None);
                let newer_memory =
                    context_memory_from(candidate.newer_memory, newer_score, newer_reason);
                let older_memory =
                    context_memory_from(candidate.older_memory, older_score, older_reason);
                ContextStaleMemoryRisk {
                    citation_id: format!("stale:{}:{}", older_memory.id, newer_memory.id),
                    reason: candidate.reason,
                    signal: candidate.signal,
                    shared_terms: candidate.shared_terms,
                    newer_memory,
                    older_memory,
                    evidence_score,
                    evidence_reason,
                }
            })
            .collect::<Vec<_>>();
        let session_count = sessions.len();
        let recent_sessions = sessions
            .into_iter()
            .enumerate()
            .map(|(index, fact)| {
                let (evidence_score, evidence_reason) =
                    session_evidence(&fact, &terms, index, session_count);
                ContextSessionFact {
                    citation_id: format!("session:{}", fact.session_id),
                    session_id: fact.session_id,
                    kind: fact.kind,
                    detail: fact.detail,
                    created_at_ms: fact.created_at_ms,
                    evidence_score,
                    evidence_reason,
                }
            })
            .collect::<Vec<_>>();
        let diagnostics = diagnostics
            .into_iter()
            .map(|diagnostic| {
                let (evidence_score, evidence_reason) = diagnostic_evidence(&diagnostic, &terms);
                ContextDiagnostic {
                    citation_id: format!("diagnostic:{}", diagnostic.id),
                    id: diagnostic.id,
                    source: diagnostic.source,
                    path: diagnostic.path,
                    line_start: diagnostic.line_start,
                    line_end: diagnostic.line_end,
                    severity: diagnostic.severity,
                    code: diagnostic.code,
                    message: diagnostic.message,
                    command: diagnostic.command,
                    created_at_ms: diagnostic.created_at_ms,
                    evidence_score,
                    evidence_reason,
                }
            })
            .collect::<Vec<_>>();
        let branch_state = branch_state
            .filter(|state| state.inside_worktree)
            .map(|state| ContextBranchState {
                root_path: state.root_path,
                branch: state.branch,
                upstream: state.upstream,
                ahead: state.ahead,
                behind: state.behind,
                changed_files: state
                    .changed_files
                    .into_iter()
                    .map(|file| ContextChangedFile {
                        path: file.path,
                        original_path: file.original_path,
                        staged_status: file.staged_status,
                        unstaged_status: file.unstaged_status,
                    })
                    .collect(),
            });
        let risk_signals = build_risk_signals(
            &relevant_files,
            &important_symbols,
            &graph_neighbors,
            &affected_tests,
            &stale_memory_risks,
            &diagnostics,
            &freshness_signals,
            &recent_sessions,
            branch_state.as_ref(),
        );
        let mut pack = Self {
            task: task.to_string(),
            budget: ContextBudget {
                max_tokens: DEFAULT_CONTEXT_TOKEN_BUDGET,
                estimated_tokens: 0,
                truncated_sections: Vec::new(),
            },
            relevant_files,
            important_symbols,
            graph_neighbors,
            affected_tests,
            relevant_memories,
            stale_memory_risks,
            diagnostics,
            risk_signals,
            recent_sessions,
            branch_state,
            suggested_path: vec![
                "Inspect the relevant files and symbols.".to_string(),
                "Check whether any memories are stale before relying on them.".to_string(),
                "Make the smallest change that satisfies the task.".to_string(),
                "Run the narrowest useful tests, then broaden if risk is unclear.".to_string(),
            ],
            citations: Vec::new(),
        };
        pack.rank_context_sections();
        pack.apply_token_budget(DEFAULT_CONTEXT_TOKEN_BUDGET);
        pack
    }

    pub fn render_markdown(&self) -> String {
        let mut rendered = String::new();

        rendered.push_str("# Hugr Context Pack\n\n");
        rendered.push_str("## Task\n");
        rendered.push_str(&self.task);
        rendered.push_str("\n\n");

        rendered.push_str("## Budget\n");
        let _ = writeln!(rendered, "- max_tokens: {}", self.budget.max_tokens);
        let _ = writeln!(
            rendered,
            "- estimated_tokens: {}",
            self.budget.estimated_tokens
        );
        if self.budget.truncated_sections.is_empty() {
            rendered.push_str("- truncated: none\n");
        } else {
            for truncation in &self.budget.truncated_sections {
                let _ = writeln!(
                    rendered,
                    "- truncated {}: {} item(s)",
                    truncation.section, truncation.removed_items
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Relevant Files\n");
        if self.relevant_files.is_empty() {
            rendered.push_str("No file candidates found yet.\n");
        } else {
            for file in &self.relevant_files {
                let _ = writeln!(
                    rendered,
                    "- {} [{}] (score {}: {})",
                    file.path, file.citation_id, file.evidence_score, file.evidence_reason
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Important Symbols\n");
        if self.important_symbols.is_empty() {
            rendered.push_str("No matching symbols indexed yet.\n");
        } else {
            for symbol in &self.important_symbols {
                let _ = writeln!(
                    rendered,
                    "- {} {} at {} [{}] (score {}: {})",
                    symbol.kind,
                    symbol.name,
                    symbol_location(symbol),
                    symbol.citation_id,
                    symbol.evidence_score,
                    symbol.evidence_reason
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Graph Neighborhood\n");
        if self.graph_neighbors.is_empty() {
            rendered.push_str("No graph neighbors found yet.\n");
        } else {
            for neighbor in &self.graph_neighbors {
                let _ = writeln!(
                    rendered,
                    "- {}: {} [{}] (score {}: {})",
                    neighbor.kind,
                    neighbor.label,
                    neighbor.citation_id,
                    neighbor.evidence_score,
                    neighbor.evidence_reason
                );
                if !neighbor.detail.is_empty() {
                    let _ = writeln!(rendered, "  - {}", neighbor.detail);
                }
            }
        }
        rendered.push('\n');

        rendered.push_str("## Affected Tests\n");
        if self.affected_tests.is_empty() {
            rendered.push_str("No likely tests mapped yet.\n");
        } else {
            for test in &self.affected_tests {
                let _ = writeln!(
                    rendered,
                    "- {} ({}) [{}] (score {}: {})",
                    test.path,
                    test.reason,
                    test.citation_id,
                    test.evidence_score,
                    test.evidence_reason
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Relevant Memories\n");
        if self.relevant_memories.is_empty() {
            rendered.push_str("No matching memories yet.\n");
        } else {
            for memory in &self.relevant_memories {
                let _ = writeln!(
                    rendered,
                    "- {} [{}]: {} (score {}: {})",
                    memory.id,
                    memory.kind,
                    memory.text,
                    memory.evidence_score,
                    memory.evidence_reason
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Stale Memory Risks\n");
        if self.stale_memory_risks.is_empty() {
            rendered.push_str("No stale memory risks detected for this task.\n");
        } else {
            for risk in &self.stale_memory_risks {
                let _ = writeln!(
                    rendered,
                    "- {} [{}]: older {} may be stale; newer {} shares {} (score {}: {})",
                    risk.signal,
                    risk.citation_id,
                    risk.older_memory.id,
                    risk.newer_memory.id,
                    risk.shared_terms.join(","),
                    risk.evidence_score,
                    risk.evidence_reason
                );
                let _ = writeln!(
                    rendered,
                    "  - older [{}]: {}",
                    risk.older_memory.kind, risk.older_memory.text
                );
                let _ = writeln!(
                    rendered,
                    "  - newer [{}]: {}",
                    risk.newer_memory.kind, risk.newer_memory.text
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Diagnostics\n");
        if self.diagnostics.is_empty() {
            rendered.push_str("No structured diagnostics matched this task.\n");
        } else {
            for diagnostic in &self.diagnostics {
                let location = diagnostic_location(diagnostic);
                let code = diagnostic.code.as_deref().unwrap_or("none");
                let _ = writeln!(
                    rendered,
                    "- {} {} at {} [{}]: {} (score {}: {})",
                    diagnostic.severity,
                    code,
                    location,
                    diagnostic.citation_id,
                    diagnostic.message,
                    diagnostic.evidence_score,
                    diagnostic.evidence_reason
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Risk Signals\n");
        if self.risk_signals.is_empty() {
            rendered.push_str("No deterministic risk signals detected.\n");
        } else {
            for risk in &self.risk_signals {
                let _ = writeln!(
                    rendered,
                    "- {} {}: {} [{}] (score {}: {})",
                    risk.severity,
                    risk.kind,
                    risk.summary,
                    risk.citation_id,
                    risk.evidence_score,
                    risk.evidence_reason
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Recent Sessions\n");
        if self.recent_sessions.is_empty() {
            rendered.push_str("No matching session facts yet.\n");
        } else {
            for fact in &self.recent_sessions {
                let _ = writeln!(
                    rendered,
                    "- {} [{}]: {} (score {}: {})",
                    fact.session_id,
                    fact.kind,
                    fact.detail,
                    fact.evidence_score,
                    fact.evidence_reason
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Branch State\n");
        if let Some(branch) = &self.branch_state {
            let branch_name = branch.branch.as_deref().unwrap_or("unknown");
            let upstream = branch.upstream.as_deref().unwrap_or("none");
            let _ = writeln!(
                rendered,
                "- branch: {branch_name}\n- upstream: {upstream}\n- ahead: {}\n- behind: {}",
                branch.ahead, branch.behind
            );
            if branch.changed_files.is_empty() {
                rendered.push_str("- changes: clean\n");
            } else {
                rendered.push_str("- changes:\n");
                for file in &branch.changed_files {
                    let _ = writeln!(rendered, "  - {} [{}]", file.path, change_label(file));
                }
            }
        } else {
            rendered.push_str("No git worktree detected.\n");
        }
        rendered.push('\n');

        rendered.push_str("## Suggested Path\n");
        if self.suggested_path.is_empty() {
            rendered.push_str("No suggested path within budget.\n");
        } else {
            for (index, step) in self.suggested_path.iter().enumerate() {
                let _ = writeln!(rendered, "{}. {}", index + 1, step);
            }
        }
        rendered.push('\n');

        rendered.push_str("## Citations\n");
        if self.citations.is_empty() {
            rendered.push_str("- No citations yet.\n");
        } else {
            for citation in &self.citations {
                let _ = writeln!(
                    rendered,
                    "- {} [{}]: {}",
                    citation.id, citation.source_type, citation.label
                );
            }
        }

        rendered
    }

    pub fn render_json(&self) -> String {
        let mut rendered = String::new();

        rendered.push('{');
        let _ = write!(rendered, "\"task\":{},", json_string(&self.task));

        rendered.push_str("\"budget\":{");
        let _ = write!(
            rendered,
            "\"max_tokens\":{},\"estimated_tokens\":{},\"truncated_sections\":[",
            self.budget.max_tokens, self.budget.estimated_tokens
        );
        for (index, truncation) in self.budget.truncated_sections.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"section\":{},\"removed_items\":{}}}",
                json_string(&truncation.section),
                truncation.removed_items
            );
        }
        rendered.push_str("]},");

        rendered.push_str("\"relevant_files\":[");
        for (index, file) in self.relevant_files.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
                json_string(&file.path),
                json_string(&file.citation_id),
                file.evidence_score,
                json_string(&file.evidence_reason)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"important_symbols\":[");
        for (index, symbol) in self.important_symbols.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"language\":{},\"name\":{},\"kind\":{},\"line_start\":{},\"line_end\":{},\"signature\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
                json_string(&symbol.path),
                json_option_string(symbol.language.as_deref()),
                json_string(&symbol.name),
                json_string(&symbol.kind),
                symbol.line_start,
                json_optional_i64(symbol.line_end),
                json_string(&symbol.signature),
                json_string(&symbol.citation_id),
                symbol.evidence_score,
                json_string(&symbol.evidence_reason)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"graph_neighbors\":[");
        for (index, neighbor) in self.graph_neighbors.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"kind\":{},\"label\":{},\"detail\":{},\"path\":{},\"target_path\":{},\"target_name\":{},\"line_start\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
                json_string(&neighbor.kind),
                json_string(&neighbor.label),
                json_string(&neighbor.detail),
                json_option_string(neighbor.path.as_deref()),
                json_option_string(neighbor.target_path.as_deref()),
                json_option_string(neighbor.target_name.as_deref()),
                json_optional_i64(neighbor.line_start),
                json_string(&neighbor.citation_id),
                neighbor.evidence_score,
                json_string(&neighbor.evidence_reason)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"affected_tests\":[");
        for (index, test) in self.affected_tests.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"reason\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
                json_string(&test.path),
                json_string(&test.reason),
                json_string(&test.citation_id),
                test.evidence_score,
                json_string(&test.evidence_reason)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"relevant_memories\":[");
        for (index, memory) in self.relevant_memories.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            rendered.push_str(&render_context_memory_json(memory));
        }
        rendered.push_str("],");

        rendered.push_str("\"stale_memory_risks\":[");
        for (index, risk) in self.stale_memory_risks.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let shared_terms = risk
                .shared_terms
                .iter()
                .map(|term| json_string(term))
                .collect::<Vec<_>>()
                .join(",");
            let _ = write!(
                rendered,
                "{{\"reason\":{},\"signal\":{},\"shared_terms\":[{}],\"newer_memory\":{},\"older_memory\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
                json_string(&risk.reason),
                json_string(&risk.signal),
                shared_terms,
                render_context_memory_json(&risk.newer_memory),
                render_context_memory_json(&risk.older_memory),
                json_string(&risk.citation_id),
                risk.evidence_score,
                json_string(&risk.evidence_reason)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"diagnostics\":[");
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"id\":{},\"source\":{},\"path\":{},\"line_start\":{},\"line_end\":{},\"severity\":{},\"code\":{},\"message\":{},\"command\":{},\"created_at_ms\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
                json_string(&diagnostic.id),
                json_string(&diagnostic.source),
                json_option_string(diagnostic.path.as_deref()),
                json_optional_i64(diagnostic.line_start),
                json_optional_i64(diagnostic.line_end),
                json_string(&diagnostic.severity),
                json_option_string(diagnostic.code.as_deref()),
                json_string(&diagnostic.message),
                json_option_string(diagnostic.command.as_deref()),
                diagnostic.created_at_ms,
                json_string(&diagnostic.citation_id),
                diagnostic.evidence_score,
                json_string(&diagnostic.evidence_reason)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"risk_signals\":[");
        for (index, risk) in self.risk_signals.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"severity\":{},\"kind\":{},\"summary\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
                json_string(&risk.severity),
                json_string(&risk.kind),
                json_string(&risk.summary),
                json_string(&risk.citation_id),
                risk.evidence_score,
                json_string(&risk.evidence_reason)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"recent_sessions\":[");
        for (index, fact) in self.recent_sessions.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"session_id\":{},\"kind\":{},\"detail\":{},\"created_at_ms\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
                json_string(&fact.session_id),
                json_string(&fact.kind),
                json_string(&fact.detail),
                fact.created_at_ms,
                json_string(&fact.citation_id),
                fact.evidence_score,
                json_string(&fact.evidence_reason)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"branch_state\":");
        if let Some(branch) = &self.branch_state {
            rendered.push('{');
            let _ = write!(
                rendered,
                "\"root_path\":{},\"branch\":{},\"upstream\":{},\"ahead\":{},\"behind\":{},\"changed_files\":[",
                json_option_string(branch.root_path.as_deref()),
                json_option_string(branch.branch.as_deref()),
                json_option_string(branch.upstream.as_deref()),
                branch.ahead,
                branch.behind
            );
            for (index, file) in branch.changed_files.iter().enumerate() {
                if index > 0 {
                    rendered.push(',');
                }
                let _ = write!(
                    rendered,
                    "{{\"path\":{},\"original_path\":{},\"staged_status\":{},\"unstaged_status\":{}}}",
                    json_string(&file.path),
                    json_option_string(file.original_path.as_deref()),
                    json_option_string(file.staged_status.as_deref()),
                    json_option_string(file.unstaged_status.as_deref())
                );
            }
            rendered.push_str("]},");
        } else {
            rendered.push_str("null,");
        }

        rendered.push_str("\"suggested_path\":[");
        for (index, step) in self.suggested_path.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            rendered.push_str(&json_string(step));
        }
        rendered.push_str("],");

        rendered.push_str("\"citations\":[");
        for (index, citation) in self.citations.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"id\":{},\"source_type\":{},\"label\":{}}}",
                json_string(&citation.id),
                json_string(&citation.source_type),
                json_string(&citation.label)
            );
        }
        rendered.push_str("]}");

        rendered
    }

    fn apply_token_budget(&mut self, max_tokens: usize) {
        let mut truncated_sections = Vec::new();

        loop {
            self.citations = build_citations(self);
            let estimated_tokens = estimate_context_pack_tokens(self, &truncated_sections);
            if estimated_tokens <= max_tokens
                || !remove_lowest_priority_context_item(self, &mut truncated_sections)
            {
                self.budget = ContextBudget {
                    max_tokens,
                    estimated_tokens,
                    truncated_sections,
                };
                return;
            }
        }
    }

    fn rank_context_sections(&mut self) {
        self.relevant_files.sort_by(|left, right| {
            right
                .evidence_score
                .cmp(&left.evidence_score)
                .then_with(|| left.path.cmp(&right.path))
        });
        self.important_symbols.sort_by(|left, right| {
            right
                .evidence_score
                .cmp(&left.evidence_score)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.line_start.cmp(&right.line_start))
                .then_with(|| left.name.cmp(&right.name))
        });
        self.graph_neighbors.sort_by(|left, right| {
            right
                .evidence_score
                .cmp(&left.evidence_score)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.label.cmp(&right.label))
        });
        self.affected_tests.sort_by(|left, right| {
            right
                .evidence_score
                .cmp(&left.evidence_score)
                .then_with(|| left.path.cmp(&right.path))
        });
        self.relevant_memories.sort_by(|left, right| {
            right
                .evidence_score
                .cmp(&left.evidence_score)
                .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.stale_memory_risks.sort_by(|left, right| {
            right
                .evidence_score
                .cmp(&left.evidence_score)
                .then_with(|| left.signal.cmp(&right.signal))
                .then_with(|| left.older_memory.id.cmp(&right.older_memory.id))
                .then_with(|| left.newer_memory.id.cmp(&right.newer_memory.id))
        });
        self.diagnostics.sort_by(|left, right| {
            right
                .evidence_score
                .cmp(&left.evidence_score)
                .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.risk_signals.sort_by(|left, right| {
            right
                .evidence_score
                .cmp(&left.evidence_score)
                .then_with(|| left.severity.cmp(&right.severity))
                .then_with(|| left.kind.cmp(&right.kind))
        });
        self.recent_sessions.sort_by(|left, right| {
            right
                .evidence_score
                .cmp(&left.evidence_score)
                .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
    }
}

fn context_memory_from(
    memory: Memory,
    evidence_score: usize,
    evidence_reason: String,
) -> ContextMemory {
    ContextMemory {
        citation_id: memory.id.clone(),
        id: memory.id,
        created_at_ms: memory.created_at_ms,
        kind: memory.kind,
        text: memory.text,
        structured_payload: memory.structured_payload,
        evidence_score,
        evidence_reason,
    }
}

fn graph_neighbor_citation_id(neighbor: &GraphNeighbor) -> String {
    let anchor = neighbor
        .path
        .as_deref()
        .or(neighbor.target_path.as_deref())
        .unwrap_or(neighbor.label.as_str());
    let line = neighbor
        .line_start
        .map(|line| format!(":{line}"))
        .unwrap_or_default();
    let target = neighbor
        .target_name
        .as_deref()
        .unwrap_or(neighbor.kind.as_str());

    format!("graph:{}:{}{}:{}", neighbor.kind, anchor, line, target)
}

fn render_context_memory_json(memory: &ContextMemory) -> String {
    format!(
        "{{\"id\":{},\"created_at_ms\":{},\"kind\":{},\"text\":{},\"structured_payload\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
        json_string(&memory.id),
        memory.created_at_ms,
        json_string(&memory.kind),
        json_string(&memory.text),
        render_optional_json_payload(memory.structured_payload.as_deref()),
        json_string(&memory.citation_id),
        memory.evidence_score,
        json_string(&memory.evidence_reason)
    )
}

fn render_optional_json_payload(payload: Option<&str>) -> String {
    match payload {
        Some(payload) => serde_json::from_str::<serde_json::Value>(payload)
            .map(|value| value.to_string())
            .unwrap_or_else(|_| json_string(payload)),
        None => "null".to_string(),
    }
}

fn file_evidence(candidate: &FileCandidate, terms: &[String]) -> (usize, String) {
    let language_bonus = candidate
        .language
        .as_ref()
        .filter(|language| terms.iter().any(|term| term == &language.to_lowercase()))
        .map(|_| 30)
        .unwrap_or(0);
    let size_bonus = candidate
        .size_bytes
        .map(|bytes| if bytes <= 128_000 { 10 } else { 0 })
        .unwrap_or(0);
    let score = 400 + candidate.score * 20 + language_bonus + size_bonus;
    let reason = if candidate.score > 0 {
        format!("file discovery score {}", candidate.score)
    } else {
        "provided file candidate".to_string()
    };
    (score, reason)
}

fn symbol_evidence(symbol: &CodeSymbol, terms: &[String]) -> (usize, String) {
    let searchable = format!(
        "{} {} {} {}",
        symbol.path, symbol.kind, symbol.name, symbol.signature
    );
    let score = 600 + text_match_score(&searchable, terms);
    (score, "symbol path/name/signature matched task".to_string())
}

fn memory_evidence(
    memory: &Memory,
    terms: &[String],
    recall_rank: Option<(usize, usize)>,
) -> (usize, String) {
    let rank_bonus = recall_rank
        .map(|(index, total)| total.saturating_sub(index) * 20)
        .unwrap_or(0);
    let score = 700 + rank_bonus + text_match_score(&memory.text, terms);
    let reason = recall_rank
        .map(|(index, _)| format!("memory recall rank {}", index + 1))
        .unwrap_or_else(|| "memory included as stale evidence".to_string());
    (score, reason)
}

fn stale_memory_evidence(candidate: &StaleMemoryCandidate, terms: &[String]) -> (usize, String) {
    let shared_term_score = candidate.shared_terms.len() * 20;
    let text_score = text_match_score(&candidate.newer_memory.text, terms)
        + text_match_score(&candidate.older_memory.text, terms);
    let score = 900 + shared_term_score + text_score;
    (
        score,
        format!("unresolved stale-memory signal {}", candidate.signal),
    )
}

fn session_evidence(
    fact: &SessionFact,
    terms: &[String],
    index: usize,
    total: usize,
) -> (usize, String) {
    let rank_bonus = total.saturating_sub(index) * 20;
    let searchable = format!("{} {}", fact.kind, fact.detail);
    let score = 300 + rank_bonus + text_match_score(&searchable, terms);
    (score, format!("session fact recall rank {}", index + 1))
}

fn graph_evidence(neighbor: &GraphNeighbor, terms: &[String]) -> (usize, String) {
    let searchable = format!(
        "{} {} {} {} {} {}",
        neighbor.kind,
        neighbor.label,
        neighbor.detail,
        neighbor.path.as_deref().unwrap_or(""),
        neighbor.target_path.as_deref().unwrap_or(""),
        neighbor.target_name.as_deref().unwrap_or("")
    );
    let base = match neighbor.kind.as_str() {
        "incoming_reference" => 650,
        "outgoing_reference" => 630,
        "path_reference" => 590,
        "entity" => 520,
        "edge" => 500,
        "source" => 480,
        _ => 450,
    };
    let match_score = text_match_score(&searchable, terms);
    let reason = if match_score > 0 {
        format!("{} matched task terms", neighbor.kind)
    } else {
        format!("{} connected through selected context", neighbor.kind)
    };

    (base + match_score, reason)
}

fn diagnostic_evidence(diagnostic: &Diagnostic, terms: &[String]) -> (usize, String) {
    let searchable = format!(
        "{} {} {} {} {}",
        diagnostic.source,
        diagnostic.severity,
        diagnostic.code.as_deref().unwrap_or(""),
        diagnostic.path.as_deref().unwrap_or(""),
        diagnostic.message
    );
    let severity_bonus = match diagnostic.severity.as_str() {
        "error" => 120,
        "warning" => 60,
        _ => 20,
    };
    let location_bonus = diagnostic.path.as_ref().map(|_| 40).unwrap_or(0);
    let score = 760 + severity_bonus + location_bonus + text_match_score(&searchable, terms);
    let reason = if diagnostic.path.is_some() {
        "structured diagnostic with source location".to_string()
    } else {
        "structured diagnostic matched task evidence".to_string()
    };
    (score, reason)
}

fn build_risk_signals(
    files: &[ContextFile],
    symbols: &[ContextSymbol],
    graph_neighbors: &[ContextGraphNeighbor],
    tests: &[ContextTest],
    stale_memory_risks: &[ContextStaleMemoryRisk],
    diagnostics: &[ContextDiagnostic],
    freshness_signals: &[FreshnessSignal],
    sessions: &[ContextSessionFact],
    branch_state: Option<&ContextBranchState>,
) -> Vec<ContextRiskSignal> {
    let mut signals = Vec::new();

    if !stale_memory_risks.is_empty() {
        signals.push(context_risk_signal(
            "high",
            "stale_memory_conflict",
            format!(
                "{} unresolved stale-memory risk(s) are relevant to this task",
                stale_memory_risks.len()
            ),
            900 + stale_memory_risks.len() * 25,
            "stale-memory evidence remains unresolved",
        ));
    }

    if let Some(branch) = branch_state {
        let changed_paths = branch
            .changed_files
            .iter()
            .flat_map(|file| [Some(file.path.as_str()), file.original_path.as_deref()])
            .flatten()
            .collect::<HashSet<_>>();
        let relevant_paths = files
            .iter()
            .map(|file| file.path.as_str())
            .chain(symbols.iter().map(|symbol| symbol.path.as_str()))
            .collect::<HashSet<_>>();
        let mut changed_relevant = relevant_paths
            .into_iter()
            .filter(|path| changed_paths.contains(path))
            .map(str::to_string)
            .collect::<Vec<_>>();
        changed_relevant.sort();

        if !changed_relevant.is_empty() {
            signals.push(context_risk_signal(
                "medium",
                "changed_relevant_files",
                format!(
                    "relevant files already have worktree changes: {}",
                    summarize_values(&changed_relevant, 4)
                ),
                740 + changed_relevant.len() * 20,
                "worktree changes overlap selected context",
            ));
        }
    }

    let source_files = files
        .iter()
        .filter(|file| context_file_likely_source(&file.path))
        .collect::<Vec<_>>();

    if !source_files.is_empty() && tests.is_empty() {
        signals.push(context_risk_signal(
            "medium",
            "missing_test_mapping",
            format!(
                "no likely tests were mapped for source file candidates: {}",
                summarize_values(
                    &source_files
                        .iter()
                        .map(|file| file.path.clone())
                        .collect::<Vec<_>>(),
                    4,
                )
            ),
            660 + source_files.len() * 10,
            "source files were selected but no affected tests were found",
        ));
    }

    if !source_files.is_empty() && symbols.is_empty() {
        signals.push(context_risk_signal(
            "low",
            "missing_symbol_index",
            "source file candidates have no matching indexed symbols".to_string(),
            460 + source_files.len() * 10,
            "symbol recall did not find task-relevant symbols",
        ));
    }

    if let Some((node, count)) = most_connected_graph_node(graph_neighbors) {
        if count >= 4 {
            signals.push(context_risk_signal(
                "medium",
                "high_graph_coupling",
                format!("{node} has {count} nearby graph reference(s) in this context"),
                680 + count * 15,
                "graph neighborhood has concentrated references",
            ));
        }
    }

    if let Some(signal) = refactor_surface_signal(graph_neighbors) {
        signals.push(signal);
    }

    if let Some(signal) = large_symbol_health_signal(symbols) {
        signals.push(signal);
    }

    let stale_index_paths = freshness_signals
        .iter()
        .filter(|signal| signal.kind == "stale_index")
        .map(|signal| signal.path.clone())
        .collect::<Vec<_>>();
    if !stale_index_paths.is_empty() {
        signals.push(context_risk_signal(
            "medium",
            "stale_index",
            format!(
                "Hugr index timestamps may be stale for relevant files: {}",
                summarize_values(&stale_index_paths, 4)
            ),
            720 + stale_index_paths.len() * 20,
            "file modification time is newer than latest indexed evidence",
        ));
    }

    let missing_index_paths = freshness_signals
        .iter()
        .filter(|signal| signal.kind == "missing_index")
        .map(|signal| signal.path.clone())
        .collect::<Vec<_>>();
    if !missing_index_paths.is_empty() {
        signals.push(context_risk_signal(
            "low",
            "missing_index",
            format!(
                "relevant files have no Hugr index timestamp: {}",
                summarize_values(&missing_index_paths, 4)
            ),
            500 + missing_index_paths.len() * 15,
            "no discovered-file, symbol, or reference timestamp was found",
        ));
    }

    if !diagnostics.is_empty() {
        let highest_severity = if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == "error")
        {
            "high"
        } else {
            "medium"
        };
        let diagnostic_summaries = diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{} at {}: {}",
                    diagnostic.severity,
                    diagnostic_location(diagnostic),
                    diagnostic.message
                )
            })
            .collect::<Vec<_>>();
        signals.push(context_risk_signal(
            highest_severity,
            "structured_diagnostics",
            format!(
                "structured diagnostics are relevant: {}",
                summarize_values(&diagnostic_summaries, 2)
            ),
            840 + diagnostics.len() * 30,
            "durable diagnostic records match selected context",
        ));
    }

    let diagnostics = sessions
        .iter()
        .filter_map(session_fact_diagnostic_snippet)
        .collect::<Vec<_>>();
    if !diagnostics.is_empty() {
        signals.push(context_risk_signal(
            "high",
            "recent_diagnostics",
            format!(
                "recent session output includes diagnostic evidence: {}",
                summarize_values(&diagnostics, 2)
            ),
            780 + diagnostics.len() * 25,
            "recent command or session output contains diagnostic terms",
        ));
    }

    let failure_facts = sessions
        .iter()
        .filter(|fact| session_fact_mentions_failure(fact))
        .count();
    if failure_facts > 0 {
        signals.push(context_risk_signal(
            "medium",
            "recent_failure_history",
            format!("{failure_facts} recent session fact(s) mention failures or errors"),
            700 + failure_facts * 20,
            "recent session evidence contains failure terms",
        ));
    }

    signals.sort_by(|left, right| {
        right
            .evidence_score
            .cmp(&left.evidence_score)
            .then_with(|| left.kind.cmp(&right.kind))
    });
    signals
}

fn refactor_surface_signal(graph_neighbors: &[ContextGraphNeighbor]) -> Option<ContextRiskSignal> {
    let reference_neighbors = graph_neighbors
        .iter()
        .filter(|neighbor| {
            matches!(
                neighbor.kind.as_str(),
                "incoming_reference" | "outgoing_reference" | "path_reference"
            )
        })
        .collect::<Vec<_>>();
    if reference_neighbors.is_empty() {
        return None;
    }

    let mut files = reference_neighbors
        .iter()
        .flat_map(|neighbor| [neighbor.path.as_deref(), neighbor.target_path.as_deref()])
        .flatten()
        .filter(|path| !path.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();

    if files.len() < REFACTOR_SURFACE_FILE_THRESHOLD {
        return None;
    }

    let reference_labels = reference_neighbors
        .iter()
        .map(|neighbor| neighbor.label.clone())
        .collect::<Vec<_>>();
    let severity = if files.len() >= 5 || reference_neighbors.len() >= 8 {
        "high"
    } else {
        "medium"
    };

    Some(context_risk_signal(
        severity,
        "refactor_surface",
        format!(
            "code graph references span {} files: {}; sample references: {}",
            files.len(),
            summarize_values(&files, 4),
            summarize_values(&reference_labels, 2)
        ),
        700 + files.len() * 25 + reference_neighbors.len() * 10,
        "code graph neighbors cross multiple files",
    ))
}

fn large_symbol_health_signal(symbols: &[ContextSymbol]) -> Option<ContextRiskSignal> {
    let mut large_symbols = symbols
        .iter()
        .filter_map(|symbol| {
            let line_end = symbol.line_end?;
            let span = line_end.checked_sub(symbol.line_start)?.checked_add(1)?;
            (span >= LARGE_SYMBOL_LINE_THRESHOLD).then_some((span, symbol))
        })
        .collect::<Vec<_>>();

    large_symbols.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.path.cmp(&right.1.path))
            .then_with(|| left.1.name.cmp(&right.1.name))
            .then_with(|| left.1.kind.cmp(&right.1.kind))
    });

    let highest_span = large_symbols.first().map(|(span, _)| *span)?;
    let summaries = large_symbols
        .iter()
        .map(|(span, symbol)| {
            let line_end = symbol.line_end.unwrap_or(symbol.line_start);
            format!(
                "{} {} spans {span} lines at {}:{}-{}",
                symbol.kind, symbol.name, symbol.path, symbol.line_start, line_end
            )
        })
        .collect::<Vec<_>>();
    let severity = if highest_span >= VERY_LARGE_SYMBOL_LINE_THRESHOLD {
        "high"
    } else {
        "medium"
    };
    let bounded_span = highest_span.min(250) as usize;

    Some(context_risk_signal(
        severity,
        "large_symbol",
        format!(
            "large indexed symbols may need careful edits: {}",
            summarize_values(&summaries, 3)
        ),
        690 + bounded_span + large_symbols.len() * 15,
        "indexed symbol line ranges exceed deterministic size threshold",
    ))
}

fn context_risk_signal(
    severity: &str,
    kind: &str,
    summary: String,
    evidence_score: usize,
    evidence_reason: &str,
) -> ContextRiskSignal {
    ContextRiskSignal {
        severity: severity.to_string(),
        kind: kind.to_string(),
        summary,
        citation_id: format!("risk:{kind}"),
        evidence_score,
        evidence_reason: evidence_reason.to_string(),
    }
}

fn context_file_likely_source(path: &str) -> bool {
    let lower = path.to_lowercase();
    if lower.starts_with("tests/") || lower.contains("/tests/") || lower.contains("__tests__") {
        return false;
    }

    [
        ".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".java", ".kt", ".kts", ".swift", ".c",
        ".cc", ".cpp", ".h", ".hpp", ".cs", ".rb", ".php",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

fn most_connected_graph_node(graph_neighbors: &[ContextGraphNeighbor]) -> Option<(String, usize)> {
    let mut counts = HashMap::<String, usize>::new();
    for neighbor in graph_neighbors.iter().filter(|neighbor| {
        matches!(
            neighbor.kind.as_str(),
            "incoming_reference" | "outgoing_reference" | "path_reference"
        )
    }) {
        if let Some(target_path) = &neighbor.target_path {
            *counts.entry(target_path.clone()).or_insert(0) += 1;
        }
        if let Some(path) = &neighbor.path {
            *counts.entry(path.clone()).or_insert(0) += 1;
        }
    }

    counts
        .into_iter()
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
}

fn session_fact_mentions_failure(fact: &ContextSessionFact) -> bool {
    let lower = format!("{} {}", fact.kind, fact.detail).to_lowercase();
    ["fail", "error", "panic", "timeout", "regression"]
        .iter()
        .any(|term| lower.contains(term))
}

fn session_fact_diagnostic_snippet(fact: &ContextSessionFact) -> Option<String> {
    let lower = format!("{} {}", fact.kind, fact.detail).to_lowercase();
    let diagnostic_terms = [
        "error:",
        "error[",
        "warning:",
        "panicked at",
        "failed to compile",
        "cannot find",
        "mismatched types",
        "unresolved import",
        "thread '",
    ];
    if !diagnostic_terms.iter().any(|term| lower.contains(term)) {
        return None;
    }

    Some(compact_snippet(&fact.detail, 140))
}

fn compact_snippet(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }

    let mut rendered = compact.chars().take(max_chars).collect::<String>();
    rendered.push_str("...");
    rendered
}

fn summarize_values(values: &[String], max_items: usize) -> String {
    let mut rendered = values
        .iter()
        .take(max_items)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let remaining = values.len().saturating_sub(max_items);
    if remaining > 0 {
        let _ = write!(rendered, " (+{remaining} more)");
    }
    rendered
}

fn context_query_terms(query: &str) -> Vec<String> {
    query
        .split(|char: char| !char.is_alphanumeric() && char != '_' && char != '-')
        .filter(|term| term.len() > 2)
        .map(|term| term.to_lowercase())
        .collect()
}

fn text_match_score(value: &str, terms: &[String]) -> usize {
    if terms.is_empty() {
        return 0;
    }

    let lower = value.to_lowercase();
    let tokens = context_query_terms(value);
    terms
        .iter()
        .map(|term| {
            if tokens.iter().any(|token| token == term) {
                20
            } else if lower.contains(term) {
                10
            } else {
                0
            }
        })
        .sum()
}

fn build_citations(pack: &ContextPack) -> Vec<Citation> {
    let mut citations = pack
        .relevant_files
        .iter()
        .map(|file| Citation {
            id: file.citation_id.clone(),
            source_type: "file".to_string(),
            label: file.path.clone(),
        })
        .collect::<Vec<_>>();
    citations.extend(pack.important_symbols.iter().map(|symbol| Citation {
        id: symbol.citation_id.clone(),
        source_type: "symbol".to_string(),
        label: format!(
            "{} {} at {}:{}",
            symbol.kind, symbol.name, symbol.path, symbol.line_start
        ),
    }));
    citations.extend(pack.graph_neighbors.iter().map(|neighbor| Citation {
        id: neighbor.citation_id.clone(),
        source_type: "graph".to_string(),
        label: neighbor.label.clone(),
    }));
    citations.extend(pack.affected_tests.iter().map(|test| Citation {
        id: test.citation_id.clone(),
        source_type: "test".to_string(),
        label: test.path.clone(),
    }));
    citations.extend(pack.relevant_memories.iter().map(|memory| Citation {
        id: memory.citation_id.clone(),
        source_type: "memory".to_string(),
        label: memory.text.clone(),
    }));
    citations.extend(pack.stale_memory_risks.iter().map(|risk| Citation {
        id: risk.citation_id.clone(),
        source_type: "stale_memory".to_string(),
        label: format!(
            "{}: older {} conflicts with newer {}",
            risk.signal, risk.older_memory.id, risk.newer_memory.id
        ),
    }));
    citations.extend(pack.diagnostics.iter().map(|diagnostic| Citation {
        id: diagnostic.citation_id.clone(),
        source_type: "diagnostic".to_string(),
        label: format!(
            "{} at {}: {}",
            diagnostic.severity,
            diagnostic_location(diagnostic),
            diagnostic.message
        ),
    }));
    citations.extend(pack.risk_signals.iter().map(|risk| Citation {
        id: risk.citation_id.clone(),
        source_type: "risk".to_string(),
        label: risk.summary.clone(),
    }));
    citations.extend(pack.recent_sessions.iter().map(|fact| Citation {
        id: fact.citation_id.clone(),
        source_type: "session".to_string(),
        label: fact.detail.clone(),
    }));
    citations
}

fn remove_lowest_priority_context_item(
    pack: &mut ContextPack,
    truncated_sections: &mut Vec<ContextBudgetTruncation>,
) -> bool {
    if let Some(branch) = &mut pack.branch_state {
        if branch.changed_files.pop().is_some() {
            record_truncation(truncated_sections, "branch_state.changed_files");
            return true;
        }
    }
    if pack.suggested_path.pop().is_some() {
        record_truncation(truncated_sections, "suggested_path");
        return true;
    }

    let mut weakest = None;
    if let Some(fact) = pack.recent_sessions.last() {
        consider_weakest_evidence(&mut weakest, "recent_sessions", fact.evidence_score, 0);
    }
    if let Some(test) = pack.affected_tests.last() {
        consider_weakest_evidence(&mut weakest, "affected_tests", test.evidence_score, 1);
    }
    if let Some(neighbor) = pack.graph_neighbors.last() {
        consider_weakest_evidence(&mut weakest, "graph_neighbors", neighbor.evidence_score, 2);
    }
    if let Some(file) = pack.relevant_files.last() {
        consider_weakest_evidence(&mut weakest, "relevant_files", file.evidence_score, 3);
    }
    if let Some(symbol) = pack.important_symbols.last() {
        consider_weakest_evidence(&mut weakest, "important_symbols", symbol.evidence_score, 4);
    }
    if let Some(risk) = pack.stale_memory_risks.last() {
        consider_weakest_evidence(&mut weakest, "stale_memory_risks", risk.evidence_score, 5);
    }
    if let Some(diagnostic) = pack.diagnostics.last() {
        consider_weakest_evidence(&mut weakest, "diagnostics", diagnostic.evidence_score, 6);
    }
    if let Some(risk) = pack.risk_signals.last() {
        consider_weakest_evidence(&mut weakest, "risk_signals", risk.evidence_score, 7);
    }
    if let Some(memory) = pack.relevant_memories.last() {
        consider_weakest_evidence(&mut weakest, "relevant_memories", memory.evidence_score, 8);
    }

    match weakest.map(|(_, _, section)| section) {
        Some("recent_sessions") => {
            pack.recent_sessions.pop();
            record_truncation(truncated_sections, "recent_sessions");
            true
        }
        Some("affected_tests") => {
            pack.affected_tests.pop();
            record_truncation(truncated_sections, "affected_tests");
            true
        }
        Some("graph_neighbors") => {
            pack.graph_neighbors.pop();
            record_truncation(truncated_sections, "graph_neighbors");
            true
        }
        Some("relevant_files") => {
            pack.relevant_files.pop();
            record_truncation(truncated_sections, "relevant_files");
            true
        }
        Some("important_symbols") => {
            pack.important_symbols.pop();
            record_truncation(truncated_sections, "important_symbols");
            true
        }
        Some("stale_memory_risks") => {
            pack.stale_memory_risks.pop();
            record_truncation(truncated_sections, "stale_memory_risks");
            true
        }
        Some("diagnostics") => {
            pack.diagnostics.pop();
            record_truncation(truncated_sections, "diagnostics");
            true
        }
        Some("risk_signals") => {
            pack.risk_signals.pop();
            record_truncation(truncated_sections, "risk_signals");
            true
        }
        Some("relevant_memories") => {
            pack.relevant_memories.pop();
            record_truncation(truncated_sections, "relevant_memories");
            true
        }
        _ => false,
    }
}

fn consider_weakest_evidence(
    weakest: &mut Option<(usize, usize, &'static str)>,
    section: &'static str,
    evidence_score: usize,
    tie_breaker: usize,
) {
    match weakest {
        Some((current_score, current_tie_breaker, _))
            if evidence_score > *current_score
                || (evidence_score == *current_score && tie_breaker >= *current_tie_breaker) => {}
        _ => *weakest = Some((evidence_score, tie_breaker, section)),
    }
}

fn record_truncation(truncated_sections: &mut Vec<ContextBudgetTruncation>, section: &str) {
    if let Some(truncation) = truncated_sections
        .iter_mut()
        .find(|truncation| truncation.section == section)
    {
        truncation.removed_items += 1;
    } else {
        truncated_sections.push(ContextBudgetTruncation {
            section: section.to_string(),
            removed_items: 1,
        });
    }
}

fn estimate_context_pack_tokens(
    pack: &ContextPack,
    truncated_sections: &[ContextBudgetTruncation],
) -> usize {
    let mut total = 24 + estimate_tokens(&pack.task);
    total += pack
        .relevant_files
        .iter()
        .map(estimate_file_tokens)
        .sum::<usize>();
    total += pack
        .important_symbols
        .iter()
        .map(estimate_symbol_tokens)
        .sum::<usize>();
    total += pack
        .graph_neighbors
        .iter()
        .map(estimate_graph_neighbor_tokens)
        .sum::<usize>();
    total += pack
        .affected_tests
        .iter()
        .map(estimate_test_tokens)
        .sum::<usize>();
    total += pack
        .relevant_memories
        .iter()
        .map(estimate_memory_tokens)
        .sum::<usize>();
    total += pack
        .stale_memory_risks
        .iter()
        .map(estimate_stale_risk_tokens)
        .sum::<usize>();
    total += pack
        .diagnostics
        .iter()
        .map(estimate_diagnostic_tokens)
        .sum::<usize>();
    total += pack
        .risk_signals
        .iter()
        .map(estimate_risk_signal_tokens)
        .sum::<usize>();
    total += pack
        .recent_sessions
        .iter()
        .map(estimate_session_tokens)
        .sum::<usize>();
    if let Some(branch) = &pack.branch_state {
        total += estimate_branch_tokens(branch);
    }
    total += pack
        .suggested_path
        .iter()
        .map(|step| 2 + estimate_tokens(step))
        .sum::<usize>();
    total += pack
        .citations
        .iter()
        .map(|citation| {
            3 + estimate_tokens(&citation.id)
                + estimate_tokens(&citation.source_type)
                + estimate_tokens(&citation.label)
        })
        .sum::<usize>();
    total + 12 + truncated_sections.len() * 8
}

fn estimate_file_tokens(file: &ContextFile) -> usize {
    4 + estimate_tokens(&file.path)
        + estimate_tokens(&file.citation_id)
        + estimate_tokens(&file.evidence_reason)
}

fn estimate_symbol_tokens(symbol: &ContextSymbol) -> usize {
    9 + estimate_tokens(&symbol.path)
        + symbol
            .language
            .as_ref()
            .map(|language| estimate_tokens(language))
            .unwrap_or(0)
        + estimate_tokens(&symbol.name)
        + estimate_tokens(&symbol.kind)
        + estimate_tokens(&symbol.signature)
        + estimate_tokens(&symbol.citation_id)
        + estimate_tokens(&symbol.evidence_reason)
}

fn estimate_graph_neighbor_tokens(neighbor: &ContextGraphNeighbor) -> usize {
    9 + estimate_tokens(&neighbor.kind)
        + estimate_tokens(&neighbor.label)
        + estimate_tokens(&neighbor.detail)
        + neighbor
            .path
            .as_ref()
            .map(|path| estimate_tokens(path))
            .unwrap_or(0)
        + neighbor
            .target_path
            .as_ref()
            .map(|path| estimate_tokens(path))
            .unwrap_or(0)
        + neighbor
            .target_name
            .as_ref()
            .map(|name| estimate_tokens(name))
            .unwrap_or(0)
        + estimate_tokens(&neighbor.citation_id)
        + estimate_tokens(&neighbor.evidence_reason)
}

fn estimate_test_tokens(test: &ContextTest) -> usize {
    4 + estimate_tokens(&test.path)
        + estimate_tokens(&test.reason)
        + estimate_tokens(&test.citation_id)
        + estimate_tokens(&test.evidence_reason)
}

fn estimate_memory_tokens(memory: &ContextMemory) -> usize {
    6 + estimate_tokens(&memory.id)
        + estimate_tokens(&memory.kind)
        + estimate_tokens(&memory.text)
        + estimate_tokens(&memory.citation_id)
        + estimate_tokens(&memory.evidence_reason)
}

fn estimate_stale_risk_tokens(risk: &ContextStaleMemoryRisk) -> usize {
    11 + estimate_tokens(&risk.reason)
        + estimate_tokens(&risk.signal)
        + risk
            .shared_terms
            .iter()
            .map(|term| estimate_tokens(term))
            .sum::<usize>()
        + estimate_memory_tokens(&risk.newer_memory)
        + estimate_memory_tokens(&risk.older_memory)
        + estimate_tokens(&risk.citation_id)
        + estimate_tokens(&risk.evidence_reason)
}

fn estimate_diagnostic_tokens(diagnostic: &ContextDiagnostic) -> usize {
    10 + estimate_tokens(&diagnostic.id)
        + estimate_tokens(&diagnostic.source)
        + diagnostic
            .path
            .as_ref()
            .map(|path| estimate_tokens(path))
            .unwrap_or(0)
        + estimate_tokens(&diagnostic.severity)
        + diagnostic
            .code
            .as_ref()
            .map(|code| estimate_tokens(code))
            .unwrap_or(0)
        + estimate_tokens(&diagnostic.message)
        + diagnostic
            .command
            .as_ref()
            .map(|command| estimate_tokens(command))
            .unwrap_or(0)
        + estimate_tokens(&diagnostic.citation_id)
        + estimate_tokens(&diagnostic.evidence_reason)
}

fn estimate_risk_signal_tokens(risk: &ContextRiskSignal) -> usize {
    7 + estimate_tokens(&risk.severity)
        + estimate_tokens(&risk.kind)
        + estimate_tokens(&risk.summary)
        + estimate_tokens(&risk.citation_id)
        + estimate_tokens(&risk.evidence_reason)
}

fn estimate_session_tokens(fact: &ContextSessionFact) -> usize {
    6 + estimate_tokens(&fact.session_id)
        + estimate_tokens(&fact.kind)
        + estimate_tokens(&fact.detail)
        + estimate_tokens(&fact.citation_id)
        + estimate_tokens(&fact.evidence_reason)
}

fn estimate_branch_tokens(branch: &ContextBranchState) -> usize {
    let mut total = 8;
    if let Some(root_path) = &branch.root_path {
        total += estimate_tokens(root_path);
    }
    if let Some(branch_name) = &branch.branch {
        total += estimate_tokens(branch_name);
    }
    if let Some(upstream) = &branch.upstream {
        total += estimate_tokens(upstream);
    }
    total
        + branch
            .changed_files
            .iter()
            .map(|file| {
                4 + estimate_tokens(&file.path)
                    + file
                        .original_path
                        .as_ref()
                        .map(|path| estimate_tokens(path))
                        .unwrap_or(0)
                    + file
                        .staged_status
                        .as_ref()
                        .map(|status| estimate_tokens(status))
                        .unwrap_or(0)
                    + file
                        .unstaged_status
                        .as_ref()
                        .map(|status| estimate_tokens(status))
                        .unwrap_or(0)
            })
            .sum::<usize>()
}

fn estimate_tokens(value: &str) -> usize {
    if value.is_empty() {
        return 0;
    }
    let word_estimate = value.split_whitespace().count();
    let char_estimate = (value.chars().count() + 3) / 4;
    word_estimate.max(char_estimate).max(1)
}

pub(crate) fn json_string(value: &str) -> String {
    let mut escaped = String::from("\"");
    for char in value.chars() {
        match char {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            char if char.is_control() => {
                let _ = write!(escaped, "\\u{:04x}", char as u32);
            }
            char => escaped.push(char),
        }
    }
    escaped.push('"');
    escaped
}

fn json_option_string(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_string())
}

fn json_optional_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn symbol_location(symbol: &ContextSymbol) -> String {
    match symbol.line_end {
        Some(line_end) if line_end > symbol.line_start => {
            format!("{}:{}-{}", symbol.path, symbol.line_start, line_end)
        }
        _ => format!("{}:{}", symbol.path, symbol.line_start),
    }
}

fn diagnostic_location(diagnostic: &ContextDiagnostic) -> String {
    match (
        diagnostic.path.as_deref(),
        diagnostic.line_start,
        diagnostic.line_end,
    ) {
        (Some(path), Some(start), Some(end)) if end > start => format!("{path}:{start}-{end}"),
        (Some(path), Some(start), _) => format!("{path}:{start}"),
        (Some(path), None, _) => path.to_string(),
        (None, _, _) => "unknown".to_string(),
    }
}

fn change_label(file: &ContextChangedFile) -> String {
    match (&file.staged_status, &file.unstaged_status) {
        (Some(staged), Some(unstaged)) if staged == unstaged => staged.clone(),
        (Some(staged), Some(unstaged)) => format!("staged {staged}, unstaged {unstaged}"),
        (Some(staged), None) => format!("staged {staged}"),
        (None, Some(unstaged)) => format!("unstaged {unstaged}"),
        (None, None) => "changed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextPack, json_string};
    use crate::code::CodeSymbol;
    use crate::discovery::FileCandidate;
    use crate::store::{
        Diagnostic, FreshnessSignal, GraphNeighbor, Memory, SessionFact, StaleMemoryCandidate,
    };
    use crate::testmap::TestCandidate;
    use crate::worktree::{ChangedFile, WorktreeState};

    #[test]
    fn markdown_includes_citations_for_files_and_memories() {
        let pack = ContextPack::new(
            "add plugin hooks",
            vec!["src/plugin.rs".to_string()],
            vec![Memory {
                id: "mem_1".to_string(),
                created_at_ms: 7,
                kind: "fact".to_string(),
                text: "plugin hooks run after configuration is loaded".to_string(),
                structured_payload: Some(
                    r#"{"source":{"type":"session_promotion","session_id":"ses_1"}}"#.to_string(),
                ),
            }],
        );

        let markdown = pack.render_markdown();
        let json = pack.render_json();
        let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap();

        assert!(markdown.contains("- src/plugin.rs [file:src/plugin.rs]"));
        assert!(
            markdown.contains("- mem_1 [memory]: plugin hooks run after configuration is loaded")
        );
        assert_eq!(
            parsed["relevant_memories"][0]["structured_payload"]["source"]["type"],
            "session_promotion"
        );
        assert_eq!(
            parsed["relevant_memories"][0]["structured_payload"]["source"]["session_id"],
            "ses_1"
        );
    }

    #[test]
    fn markdown_and_json_include_symbols() {
        let pack = ContextPack::with_sessions_symbols_tests_and_branch(
            "add plugin hooks",
            vec!["src/plugin_hooks.rs".to_string()],
            Vec::new(),
            Vec::new(),
            vec![CodeSymbol {
                path: "src/plugin_hooks.rs".to_string(),
                language: Some("rust".to_string()),
                name: "PluginHooks".to_string(),
                kind: "struct".to_string(),
                line_start: 3,
                line_end: Some(8),
                signature: "pub struct PluginHooks".to_string(),
            }],
            vec![TestCandidate {
                path: "tests/plugin_hooks.rs".to_string(),
                reason: "repository tests directory match".to_string(),
                score: 50,
            }],
            None,
        );

        let markdown = pack.render_markdown();
        let json = pack.render_json();

        assert!(markdown.contains("- struct PluginHooks at src/plugin_hooks.rs:3-8"));
        assert!(json.contains("\"important_symbols\""));
        assert!(json.contains("\"name\":\"PluginHooks\""));
        assert!(json.contains("\"line_end\":8"));
        assert!(markdown.contains("- tests/plugin_hooks.rs"));
        assert!(json.contains("\"affected_tests\""));
    }

    #[test]
    fn markdown_and_json_include_graph_neighbors() {
        let pack =
            ContextPack::with_file_candidates_sessions_symbols_tests_branch_stale_risks_and_graph(
                "add plugin hooks",
                vec![FileCandidate {
                    path: "src/plugin_hooks.rs".to_string(),
                    score: 5,
                    language: Some("rust".to_string()),
                    size_bytes: Some(100),
                }],
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                None,
                Vec::new(),
                vec![GraphNeighbor {
                    kind: "incoming_reference".to_string(),
                    label: "src/main.rs:8 references function run_after_config".to_string(),
                    detail: "call reference to function run_after_config: run_after_config();"
                        .to_string(),
                    path: Some("src/main.rs".to_string()),
                    target_path: Some("src/plugin_hooks.rs".to_string()),
                    target_name: Some("run_after_config".to_string()),
                    line_start: Some(8),
                }],
                Vec::new(),
                Vec::new(),
            );

        let markdown = pack.render_markdown();
        let json = pack.render_json();
        let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap();

        assert!(markdown.contains("## Graph Neighborhood"));
        assert!(markdown.contains("incoming_reference: src/main.rs:8 references"));
        assert_eq!(
            parsed["graph_neighbors"][0]["target_name"],
            "run_after_config"
        );
        assert_eq!(parsed["citations"][1]["source_type"], "graph");
    }

    #[test]
    fn cross_file_references_render_as_refactor_surface_risks() {
        let pack =
            ContextPack::with_file_candidates_sessions_symbols_tests_branch_stale_risks_and_graph(
                "refactor plugin hooks",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                None,
                Vec::new(),
                vec![
                    GraphNeighbor {
                        kind: "incoming_reference".to_string(),
                        label: "src/main.rs:8 references function run_after_config".to_string(),
                        detail: "call reference to function run_after_config".to_string(),
                        path: Some("src/main.rs".to_string()),
                        target_path: Some("src/plugin_hooks.rs".to_string()),
                        target_name: Some("run_after_config".to_string()),
                        line_start: Some(8),
                    },
                    GraphNeighbor {
                        kind: "incoming_reference".to_string(),
                        label: "src/worker.rs:14 references function run_after_config".to_string(),
                        detail: "call reference to function run_after_config".to_string(),
                        path: Some("src/worker.rs".to_string()),
                        target_path: Some("src/plugin_hooks.rs".to_string()),
                        target_name: Some("run_after_config".to_string()),
                        line_start: Some(14),
                    },
                ],
                Vec::new(),
                Vec::new(),
            );

        let markdown = pack.render_markdown();
        let parsed = serde_json::from_str::<serde_json::Value>(&pack.render_json()).unwrap();
        let risk_kinds = parsed["risk_signals"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|risk| risk["kind"].as_str())
            .collect::<Vec<_>>();

        assert!(risk_kinds.contains(&"refactor_surface"));
        assert!(markdown.contains("risk:refactor_surface [risk]"));
        assert!(markdown.contains("code graph references span 3 files"));
    }

    #[test]
    fn freshness_signals_render_as_risk_signals() {
        let pack =
            ContextPack::with_file_candidates_sessions_symbols_tests_branch_stale_risks_and_graph(
                "add plugin hooks",
                vec![FileCandidate {
                    path: "src/plugin_hooks.rs".to_string(),
                    score: 5,
                    language: Some("rust".to_string()),
                    size_bytes: Some(100),
                }],
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                None,
                Vec::new(),
                Vec::new(),
                vec![FreshnessSignal {
                    path: "src/plugin_hooks.rs".to_string(),
                    kind: "stale_index".to_string(),
                    detail: "src/plugin_hooks.rs changed after its latest Hugr index timestamp"
                        .to_string(),
                    indexed_at_ms: Some(10),
                    modified_at_ms: Some(20),
                }],
                Vec::new(),
            );

        let json = pack.render_json();
        let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap();
        let risk_kinds = parsed["risk_signals"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|risk| risk["kind"].as_str())
            .collect::<Vec<_>>();

        assert!(pack.render_markdown().contains("risk:stale_index [risk]"));
        assert!(risk_kinds.contains(&"stale_index"));
    }

    #[test]
    fn large_symbols_render_as_code_health_risks() {
        let pack = ContextPack::with_sessions_symbols_tests_and_branch(
            "refactor plugin hooks",
            vec!["src/plugin_hooks.rs".to_string()],
            Vec::new(),
            Vec::new(),
            vec![CodeSymbol {
                path: "src/plugin_hooks.rs".to_string(),
                language: Some("rust".to_string()),
                name: "run_after_config".to_string(),
                kind: "function".to_string(),
                line_start: 12,
                line_end: Some(105),
                signature: "pub fn run_after_config()".to_string(),
            }],
            vec![TestCandidate {
                path: "tests/plugin_hooks.rs".to_string(),
                reason: "repository tests directory match".to_string(),
                score: 50,
            }],
            None,
        );

        let markdown = pack.render_markdown();
        let parsed = serde_json::from_str::<serde_json::Value>(&pack.render_json()).unwrap();
        let risk_kinds = parsed["risk_signals"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|risk| risk["kind"].as_str())
            .collect::<Vec<_>>();

        assert!(risk_kinds.contains(&"large_symbol"));
        assert!(markdown.contains("risk:large_symbol [risk]"));
        assert!(markdown.contains("function run_after_config spans 94 lines"));
    }

    #[test]
    fn structured_diagnostics_render_with_citations_and_risks() {
        let pack =
            ContextPack::with_file_candidates_sessions_symbols_tests_branch_stale_risks_and_graph(
                "fix plugin hooks",
                vec![FileCandidate {
                    path: "src/plugin_hooks.rs".to_string(),
                    score: 5,
                    language: Some("rust".to_string()),
                    size_bytes: Some(100),
                }],
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                vec![Diagnostic {
                    id: "diag_1".to_string(),
                    source: "command_stderr".to_string(),
                    path: Some("src/plugin_hooks.rs".to_string()),
                    line_start: Some(12),
                    line_end: None,
                    severity: "error".to_string(),
                    code: Some("E0425".to_string()),
                    message: "cannot find value hook in this scope".to_string(),
                    command: Some("cargo test".to_string()),
                    created_at_ms: 42,
                }],
            );

        let markdown = pack.render_markdown();
        let parsed = serde_json::from_str::<serde_json::Value>(&pack.render_json()).unwrap();
        let risk_kinds = parsed["risk_signals"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|risk| risk["kind"].as_str())
            .collect::<Vec<_>>();

        assert!(markdown.contains("## Diagnostics"));
        assert!(markdown.contains("error E0425 at src/plugin_hooks.rs:12"));
        assert_eq!(parsed["diagnostics"][0]["path"], "src/plugin_hooks.rs");
        assert_eq!(parsed["diagnostics"][0]["line_start"], 12);
        assert_eq!(parsed["diagnostics"][0]["citation_id"], "diagnostic:diag_1");
        assert!(risk_kinds.contains(&"structured_diagnostics"));
        assert!(markdown.contains("diagnostic:diag_1 [diagnostic]"));
    }

    #[test]
    fn evidence_ranking_orders_files_and_renders_scores() {
        let pack = ContextPack::with_file_candidates_sessions_symbols_tests_branch_and_stale_risks(
            "plugin hooks",
            vec![
                FileCandidate {
                    path: "docs/hooks.md".to_string(),
                    score: 1,
                    language: Some("markdown".to_string()),
                    size_bytes: Some(10),
                },
                FileCandidate {
                    path: "src/plugin_hooks.rs".to_string(),
                    score: 8,
                    language: Some("rust".to_string()),
                    size_bytes: Some(10),
                },
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
        );

        assert_eq!(pack.relevant_files[0].path, "src/plugin_hooks.rs");
        assert!(pack.relevant_files[0].evidence_score > pack.relevant_files[1].evidence_score);

        let markdown = pack.render_markdown();
        let json = pack.render_json();

        assert!(markdown.contains("score"));
        assert!(json.contains("\"evidence_score\""));
        assert!(json.contains("\"evidence_reason\":\"file discovery score 8\""));
    }

    #[test]
    fn json_renderer_escapes_strings() {
        let pack = ContextPack::new("quote \"and\" newline\n", Vec::new(), Vec::new());
        let json = pack.render_json();

        assert!(json.contains("\"task\":\"quote \\\"and\\\" newline\\n\""));
        assert_eq!(json_string("tab\tbackslash\\"), "\"tab\\tbackslash\\\\\"");
    }

    #[test]
    fn markdown_and_json_include_branch_state() {
        let pack = ContextPack::with_sessions_symbols_tests_and_branch(
            "add plugin hooks",
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some(WorktreeState {
                inside_worktree: true,
                root_path: Some("/repo".to_string()),
                branch: Some("feature".to_string()),
                upstream: Some("origin/feature".to_string()),
                ahead: 2,
                behind: 1,
                changed_files: vec![ChangedFile {
                    path: "src/lib.rs".to_string(),
                    original_path: None,
                    staged_status: None,
                    unstaged_status: Some("modified".to_string()),
                }],
            }),
        );

        let markdown = pack.render_markdown();
        let json = pack.render_json();

        assert!(markdown.contains("## Branch State"));
        assert!(markdown.contains("- branch: feature"));
        assert!(markdown.contains("src/lib.rs [unstaged modified]"));
        assert!(json.contains("\"branch_state\""));
        assert!(json.contains("\"ahead\":2"));
        assert!(json.contains("\"unstaged_status\":\"modified\""));
    }

    #[test]
    fn markdown_and_json_include_risk_signals() {
        let pack = ContextPack::with_sessions_symbols_tests_and_branch(
            "add plugin hooks",
            vec!["src/plugin_hooks.rs".to_string()],
            Vec::new(),
            vec![SessionFact {
                session_id: "ses_1".to_string(),
                kind: "test".to_string(),
                detail: "cargo test failed; stderr_tail: error[E0425]: cannot find value hook"
                    .to_string(),
                created_at_ms: 30,
            }],
            Vec::new(),
            Vec::new(),
            Some(WorktreeState {
                inside_worktree: true,
                root_path: Some("/repo".to_string()),
                branch: Some("feature".to_string()),
                upstream: Some("origin/feature".to_string()),
                ahead: 0,
                behind: 0,
                changed_files: vec![ChangedFile {
                    path: "src/plugin_hooks.rs".to_string(),
                    original_path: None,
                    staged_status: None,
                    unstaged_status: Some("modified".to_string()),
                }],
            }),
        );

        let markdown = pack.render_markdown();
        let json = pack.render_json();
        let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap();
        let risk_kinds = parsed["risk_signals"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|risk| risk["kind"].as_str())
            .collect::<Vec<_>>();

        assert!(markdown.contains("## Risk Signals"));
        assert!(risk_kinds.contains(&"changed_relevant_files"));
        assert!(risk_kinds.contains(&"missing_test_mapping"));
        assert!(risk_kinds.contains(&"missing_symbol_index"));
        assert!(risk_kinds.contains(&"recent_diagnostics"));
        assert!(risk_kinds.contains(&"recent_failure_history"));
        assert!(markdown.contains("risk:changed_relevant_files [risk]"));
    }

    #[test]
    fn token_budget_reports_truncation_and_rebuilds_citations() {
        let mut pack = ContextPack::new(
            "tiny budget",
            vec![
                "src/first_large_file.rs".to_string(),
                "src/second_large_file.rs".to_string(),
            ],
            vec![
                Memory {
                    id: "mem_1".to_string(),
                    created_at_ms: 10,
                    kind: "fact".to_string(),
                    text: "plugin hooks run after configuration is loaded".to_string(),
                    structured_payload: None,
                },
                Memory {
                    id: "mem_2".to_string(),
                    created_at_ms: 20,
                    kind: "fact".to_string(),
                    text: "plugin hooks now run before configuration is loaded".to_string(),
                    structured_payload: None,
                },
            ],
        );

        pack.apply_token_budget(1);

        assert_eq!(pack.budget.max_tokens, 1);
        assert!(
            pack.budget
                .truncated_sections
                .iter()
                .any(|truncation| truncation.section == "relevant_files")
        );
        assert!(
            pack.budget
                .truncated_sections
                .iter()
                .any(|truncation| truncation.section == "relevant_memories")
        );
        assert!(pack.relevant_files.is_empty());
        assert!(pack.relevant_memories.is_empty());
        assert!(
            !pack
                .citations
                .iter()
                .any(|citation| citation.id.starts_with("file:src/"))
        );

        let markdown = pack.render_markdown();
        let json = pack.render_json();

        assert!(markdown.contains("## Budget"));
        assert!(markdown.contains("- truncated relevant_files: 2 item(s)"));
        assert!(json.contains("\"budget\""));
        assert!(json.contains("\"section\":\"relevant_files\""));
    }

    #[test]
    fn markdown_and_json_include_stale_memory_risks() {
        let pack = ContextPack::with_sessions_symbols_tests_branch_and_stale_risks(
            "add plugin hooks",
            Vec::new(),
            vec![
                Memory {
                    id: "mem_new".to_string(),
                    created_at_ms: 20,
                    kind: "fact".to_string(),
                    text: "plugin hooks now run before configuration is loaded".to_string(),
                    structured_payload: None,
                },
                Memory {
                    id: "mem_old".to_string(),
                    created_at_ms: 10,
                    kind: "fact".to_string(),
                    text: "plugin hooks run after configuration is loaded".to_string(),
                    structured_payload: None,
                },
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            vec![StaleMemoryCandidate {
                reason: "opposing_terms".to_string(),
                signal: "after_vs_before".to_string(),
                shared_terms: vec!["hooks".to_string(), "plugin".to_string(), "run".to_string()],
                newer_memory: Memory {
                    id: "mem_new".to_string(),
                    created_at_ms: 20,
                    kind: "fact".to_string(),
                    text: "plugin hooks now run before configuration is loaded".to_string(),
                    structured_payload: None,
                },
                older_memory: Memory {
                    id: "mem_old".to_string(),
                    created_at_ms: 10,
                    kind: "fact".to_string(),
                    text: "plugin hooks run after configuration is loaded".to_string(),
                    structured_payload: None,
                },
            }],
        );

        let markdown = pack.render_markdown();
        let json = pack.render_json();

        assert!(markdown.contains("## Stale Memory Risks"));
        assert!(markdown.contains("after_vs_before"));
        assert!(markdown.contains("older mem_old may be stale"));
        assert!(json.contains("\"stale_memory_risks\""));
        assert!(json.contains("\"signal\":\"after_vs_before\""));
        assert!(json.contains("\"citation_id\":\"stale:mem_old:mem_new\""));
        assert!(markdown.contains("stale:mem_old:mem_new [stale_memory]"));
    }
}
