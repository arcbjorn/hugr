use crate::code::CodeSymbol;
use crate::discovery::FileCandidate;
use crate::store::{Memory, SessionFact, StaleMemoryCandidate};
use crate::testmap::TestCandidate;
use crate::worktree::WorktreeState;
use std::fmt::Write;

const DEFAULT_CONTEXT_TOKEN_BUDGET: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPack {
    pub task: String,
    pub budget: ContextBudget,
    pub relevant_files: Vec<ContextFile>,
    pub important_symbols: Vec<ContextSymbol>,
    pub affected_tests: Vec<ContextTest>,
    pub relevant_memories: Vec<ContextMemory>,
    pub stale_memory_risks: Vec<ContextStaleMemoryRisk>,
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
                let (newer_score, newer_reason) =
                    memory_evidence(&candidate.newer_memory, &terms, None);
                let (older_score, older_reason) =
                    memory_evidence(&candidate.older_memory, &terms, None);
                let newer_memory =
                    context_memory_from(candidate.newer_memory, newer_score, newer_reason);
                let older_memory =
                    context_memory_from(candidate.older_memory, older_score, older_reason);
                let (evidence_score, evidence_reason) =
                    stale_memory_evidence(&candidate, &newer_memory, &older_memory, &terms);
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
        let mut pack = Self {
            task: task.to_string(),
            budget: ContextBudget {
                max_tokens: DEFAULT_CONTEXT_TOKEN_BUDGET,
                estimated_tokens: 0,
                truncated_sections: Vec::new(),
            },
            relevant_files,
            important_symbols,
            affected_tests,
            relevant_memories,
            stale_memory_risks,
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
            let _ = write!(
                rendered,
                "{{\"id\":{},\"created_at_ms\":{},\"kind\":{},\"text\":{},\"citation_id\":{},\"evidence_score\":{},\"evidence_reason\":{}}}",
                json_string(&memory.id),
                memory.created_at_ms,
                json_string(&memory.kind),
                json_string(&memory.text),
                json_string(&memory.citation_id),
                memory.evidence_score,
                json_string(&memory.evidence_reason)
            );
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

        rendered.push_str("\"recent_sessions\":[");
        for (index, fact) in self.recent_sessions.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"session_id\":{},\"kind\":{},\"detail\":{},\"created_at_ms\":{},\"citation_id\":{}}}",
                json_string(&fact.session_id),
                json_string(&fact.kind),
                json_string(&fact.detail),
                fact.created_at_ms,
                json_string(&fact.citation_id)
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
}

fn context_memory_from(memory: Memory) -> ContextMemory {
    ContextMemory {
        citation_id: memory.id.clone(),
        id: memory.id,
        created_at_ms: memory.created_at_ms,
        kind: memory.kind,
        text: memory.text,
    }
}

fn render_context_memory_json(memory: &ContextMemory) -> String {
    format!(
        "{{\"id\":{},\"created_at_ms\":{},\"kind\":{},\"text\":{},\"citation_id\":{}}}",
        json_string(&memory.id),
        memory.created_at_ms,
        json_string(&memory.kind),
        json_string(&memory.text),
        json_string(&memory.citation_id)
    )
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
    if pack.recent_sessions.pop().is_some() {
        record_truncation(truncated_sections, "recent_sessions");
        return true;
    }
    if pack.affected_tests.pop().is_some() {
        record_truncation(truncated_sections, "affected_tests");
        return true;
    }
    if pack.relevant_files.pop().is_some() {
        record_truncation(truncated_sections, "relevant_files");
        return true;
    }
    if pack.important_symbols.pop().is_some() {
        record_truncation(truncated_sections, "important_symbols");
        return true;
    }
    if pack.stale_memory_risks.pop().is_some() {
        record_truncation(truncated_sections, "stale_memory_risks");
        return true;
    }
    if pack.relevant_memories.pop().is_some() {
        record_truncation(truncated_sections, "relevant_memories");
        return true;
    }
    false
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
    3 + estimate_tokens(&file.path) + estimate_tokens(&file.citation_id)
}

fn estimate_symbol_tokens(symbol: &ContextSymbol) -> usize {
    8 + estimate_tokens(&symbol.path)
        + symbol
            .language
            .as_ref()
            .map(|language| estimate_tokens(language))
            .unwrap_or(0)
        + estimate_tokens(&symbol.name)
        + estimate_tokens(&symbol.kind)
        + estimate_tokens(&symbol.signature)
        + estimate_tokens(&symbol.citation_id)
}

fn estimate_test_tokens(test: &ContextTest) -> usize {
    3 + estimate_tokens(&test.path)
        + estimate_tokens(&test.reason)
        + estimate_tokens(&test.citation_id)
}

fn estimate_memory_tokens(memory: &ContextMemory) -> usize {
    5 + estimate_tokens(&memory.id)
        + estimate_tokens(&memory.kind)
        + estimate_tokens(&memory.text)
        + estimate_tokens(&memory.citation_id)
}

fn estimate_stale_risk_tokens(risk: &ContextStaleMemoryRisk) -> usize {
    10 + estimate_tokens(&risk.reason)
        + estimate_tokens(&risk.signal)
        + risk
            .shared_terms
            .iter()
            .map(|term| estimate_tokens(term))
            .sum::<usize>()
        + estimate_memory_tokens(&risk.newer_memory)
        + estimate_memory_tokens(&risk.older_memory)
        + estimate_tokens(&risk.citation_id)
}

fn estimate_session_tokens(fact: &ContextSessionFact) -> usize {
    5 + estimate_tokens(&fact.session_id)
        + estimate_tokens(&fact.kind)
        + estimate_tokens(&fact.detail)
        + estimate_tokens(&fact.citation_id)
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
    use crate::store::{Memory, StaleMemoryCandidate};
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
            }],
        );

        let markdown = pack.render_markdown();

        assert!(markdown.contains("- src/plugin.rs [file:src/plugin.rs]"));
        assert!(
            markdown.contains("- mem_1 [memory]: plugin hooks run after configuration is loaded")
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
                },
                Memory {
                    id: "mem_2".to_string(),
                    created_at_ms: 20,
                    kind: "fact".to_string(),
                    text: "plugin hooks now run before configuration is loaded".to_string(),
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
                },
                Memory {
                    id: "mem_old".to_string(),
                    created_at_ms: 10,
                    kind: "fact".to_string(),
                    text: "plugin hooks run after configuration is loaded".to_string(),
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
                },
                older_memory: Memory {
                    id: "mem_old".to_string(),
                    created_at_ms: 10,
                    kind: "fact".to_string(),
                    text: "plugin hooks run after configuration is loaded".to_string(),
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
