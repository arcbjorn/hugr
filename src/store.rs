use crate::code::{CodeReference, CodeSymbol};
use crate::discovery::FileCandidate;
use crate::embedding::{EmbeddingProvider, SelectedEmbeddingProvider};
use crate::migrations;
use crate::testmap::{self, TestCandidate};
use libsql::{Builder, Connection, Row, params};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};

const HUGR_DIR: &str = ".hugr";
const HUGR_DB: &str = "hugr.db";
const LOCAL_PROJECT_ID: &str = "project_local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageMode {
    Local,
    Hybrid,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StorageConfig {
    mode: StorageMode,
    remote_url: Option<String>,
    auth_token_configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub id: String,
    pub created_at_ms: i64,
    pub kind: String,
    pub text: String,
}

pub struct Store {
    root: PathBuf,
    embedding_provider: Result<SelectedEmbeddingProvider, String>,
    storage_config: Result<StorageConfig, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub git_remote: Option<String>,
    pub default_branch: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub task: String,
    pub branch: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub final_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvent {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub detail: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFact {
    pub session_id: String,
    pub kind: String,
    pub detail: String,
    pub created_at_ms: i64,
}

struct ProjectInput {
    id: String,
    name: String,
    root_path: String,
    git_remote: Option<String>,
    default_branch: Option<String>,
}

impl Store {
    pub fn open_current() -> Self {
        Self {
            root: PathBuf::from(HUGR_DIR),
            embedding_provider: SelectedEmbeddingProvider::from_env(),
            storage_config: StorageConfig::from_env(),
        }
    }

    #[cfg(test)]
    fn open_at(root: PathBuf) -> Self {
        Self {
            root,
            embedding_provider: Ok(SelectedEmbeddingProvider::default()),
            storage_config: Ok(StorageConfig::local()),
        }
    }

    pub async fn init(&self) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.root.join("sessions")).map_err(|error| error.to_string())?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let project = current_project_input()?;
        upsert_project(&conn, project).await?;
        Ok(())
    }

    pub fn exists(&self) -> bool {
        self.db_path().exists()
    }

    pub async fn remember(&self, text: &str) -> Result<Memory, String> {
        self.init().await?;
        let conn = self.connect().await?;
        let created_at_ms = now_ms()?;
        let memory = Memory {
            id: format!("mem_{created_at_ms}"),
            created_at_ms,
            kind: "fact".to_string(),
            text: text.trim().to_string(),
        };

        conn.execute(
            "INSERT INTO memories (id, created_at_ms, kind, text) VALUES (?1, ?2, ?3, ?4)",
            params![
                memory.id.clone(),
                memory.created_at_ms,
                memory.kind.clone(),
                memory.text.clone()
            ],
        )
        .await
        .map_err(|error| error.to_string())?;

        let embedding = self.embedding_provider()?.embed(&memory.text)?;
        let embedding_dimensions =
            i64::try_from(embedding.dimensions()).map_err(|error| error.to_string())?;
        let embedding_vector = embedding.to_vector_literal();
        conn.execute(
            "INSERT INTO memory_embeddings (memory_id, model, dimensions, embedding) VALUES (?1, ?2, ?3, vector(?4))",
            params![
                memory.id.clone(),
                embedding.model,
                embedding_dimensions,
                embedding_vector
            ],
        )
        .await
        .map_err(|error| error.to_string())?;

        Ok(memory)
    }

    pub async fn memories(&self) -> Result<Vec<Memory>, String> {
        if !self.exists() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT id, created_at_ms, kind, text FROM memories ORDER BY created_at_ms DESC",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut memories = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            memories.push(Memory {
                id: row.get::<String>(0).map_err(|error| error.to_string())?,
                created_at_ms: row.get::<i64>(1).map_err(|error| error.to_string())?,
                kind: row.get::<String>(2).map_err(|error| error.to_string())?,
                text: row.get::<String>(3).map_err(|error| error.to_string())?,
            });
        }

        Ok(memories)
    }

    pub async fn sync_current_project(&self) -> Result<Project, String> {
        self.init().await?;
        self.project()
            .await?
            .ok_or_else(|| "project registry is empty after initialization".to_string())
    }

    pub async fn project(&self) -> Result<Option<Project>, String> {
        if !self.exists() {
            return Ok(None);
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        project_from_conn(&conn).await
    }

    pub async fn record_discovered_files(&self, files: &[FileCandidate]) -> Result<(), String> {
        if files.is_empty() {
            return Ok(());
        }

        self.init().await?;
        let conn = self.connect().await?;
        let now = now_ms()?;

        for file in files {
            let size_bytes = file
                .size_bytes
                .map(i64::try_from)
                .transpose()
                .map_err(|error| error.to_string())?;
            conn.execute(
                "
                INSERT INTO discovered_files (
                    project_id,
                    path,
                    language,
                    size_bytes,
                    discovered_at_ms,
                    updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                ON CONFLICT(project_id, path) DO UPDATE SET
                    language = excluded.language,
                    size_bytes = excluded.size_bytes,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![
                    LOCAL_PROJECT_ID,
                    file.path.clone(),
                    file.language.clone(),
                    size_bytes,
                    now
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        }

        Ok(())
    }

    pub async fn record_code_index(
        &self,
        files: &[FileCandidate],
        symbols: &[CodeSymbol],
        references: &[CodeReference],
    ) -> Result<(), String> {
        if files.is_empty() {
            return Ok(());
        }

        self.init().await?;
        let conn = self.connect().await?;
        let now = now_ms()?;
        let mut paths = files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<HashSet<_>>();

        for path in paths.drain() {
            conn.execute(
                "
                DELETE FROM code_symbols
                WHERE project_id = ?1 AND path = ?2
                ",
                params![LOCAL_PROJECT_ID, path],
            )
            .await
            .map_err(|error| error.to_string())?;
            conn.execute(
                "
                DELETE FROM code_references
                WHERE project_id = ?1 AND path = ?2
                ",
                params![LOCAL_PROJECT_ID, path],
            )
            .await
            .map_err(|error| error.to_string())?;
        }

        for symbol in symbols {
            conn.execute(
                "
                INSERT INTO code_symbols (
                    project_id,
                    path,
                    name,
                    kind,
                    language,
                    line_start,
                    line_end,
                    signature,
                    indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ",
                params![
                    LOCAL_PROJECT_ID,
                    symbol.path.clone(),
                    symbol.name.clone(),
                    symbol.kind.clone(),
                    symbol.language.clone(),
                    symbol.line_start,
                    symbol.line_end,
                    symbol.signature.clone(),
                    now
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        }

        for reference in references {
            conn.execute(
                "
                INSERT INTO code_references (
                    project_id,
                    path,
                    target_path,
                    target_name,
                    target_kind,
                    kind,
                    language,
                    line_start,
                    excerpt,
                    indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ",
                params![
                    LOCAL_PROJECT_ID,
                    reference.path.clone(),
                    reference.target_path.clone(),
                    reference.target_name.clone(),
                    reference.target_kind.clone(),
                    reference.kind.clone(),
                    reference.language.clone(),
                    reference.line_start,
                    reference.excerpt.clone(),
                    now
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        }

        Ok(())
    }

    pub async fn symbols_for_target(
        &self,
        target: &str,
        limit: usize,
    ) -> Result<Vec<CodeSymbol>, String> {
        if limit == 0 || !self.exists() {
            return Ok(Vec::new());
        }

        let target = normalize_target(target);
        if target.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let candidate_limit = i64::try_from(limit.max(50)).map_err(|error| error.to_string())?;
        let mut rows = conn
            .query(
                "
                SELECT path, language, name, kind, line_start, line_end, signature
                FROM code_symbols
                WHERE project_id = ?1
                  AND (path = ?2 OR name = ?3)
                ORDER BY path ASC, line_start ASC
                LIMIT ?4
                ",
                params![
                    LOCAL_PROJECT_ID,
                    target.clone(),
                    target.clone(),
                    candidate_limit
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut symbols = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            symbols.push(code_symbol_from_row(&row)?);
        }

        if symbols.is_empty() {
            return self.recall_symbols(&target, limit).await;
        }

        symbols.truncate(limit);
        Ok(symbols)
    }

    pub async fn references_to_symbols(
        &self,
        symbols: &[CodeSymbol],
        limit: usize,
    ) -> Result<Vec<CodeReference>, String> {
        if limit == 0 || symbols.is_empty() || !self.exists() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut rows = conn
            .query(
                "
                SELECT
                    path,
                    language,
                    target_path,
                    target_name,
                    target_kind,
                    kind,
                    line_start,
                    excerpt
                FROM code_references
                WHERE project_id = ?1
                ORDER BY path ASC, line_start ASC
                LIMIT 5000
                ",
                params![LOCAL_PROJECT_ID],
            )
            .await
            .map_err(|error| error.to_string())?;
        let targets = symbols
            .iter()
            .map(|symbol| (symbol.path.as_str(), symbol.name.as_str()))
            .collect::<HashSet<_>>();
        let mut references = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            let reference = code_reference_from_row(&row)?;
            if targets.contains(&(
                reference.target_path.as_str(),
                reference.target_name.as_str(),
            )) {
                references.push(reference);
            }
        }

        references.truncate(limit);
        Ok(references)
    }

    pub async fn references_from_symbols(
        &self,
        symbols: &[CodeSymbol],
        limit: usize,
    ) -> Result<Vec<CodeReference>, String> {
        if limit == 0 || symbols.is_empty() || !self.exists() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut rows = conn
            .query(
                "
                SELECT
                    path,
                    language,
                    target_path,
                    target_name,
                    target_kind,
                    kind,
                    line_start,
                    excerpt
                FROM code_references
                WHERE project_id = ?1
                ORDER BY path ASC, line_start ASC
                LIMIT 5000
                ",
                params![LOCAL_PROJECT_ID],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut references = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            let reference = code_reference_from_row(&row)?;
            if symbols
                .iter()
                .any(|symbol| reference_is_in_symbol(&reference, symbol))
            {
                references.push(reference);
            }
        }

        references.truncate(limit);
        Ok(references)
    }

    pub async fn references_from_path(
        &self,
        path: &str,
        limit: usize,
    ) -> Result<Vec<CodeReference>, String> {
        if limit == 0 || !self.exists() {
            return Ok(Vec::new());
        }

        let path = normalize_target(path);
        if path.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let candidate_limit = i64::try_from(limit.max(50)).map_err(|error| error.to_string())?;
        let mut rows = conn
            .query(
                "
                SELECT
                    path,
                    language,
                    target_path,
                    target_name,
                    target_kind,
                    kind,
                    line_start,
                    excerpt
                FROM code_references
                WHERE project_id = ?1 AND path = ?2
                ORDER BY line_start ASC
                LIMIT ?3
                ",
                params![LOCAL_PROJECT_ID, path, candidate_limit],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut references = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            references.push(code_reference_from_row(&row)?);
        }

        references.truncate(limit);
        Ok(references)
    }

    pub async fn likely_tests_for_files(
        &self,
        files: &[String],
        limit: usize,
    ) -> Result<Vec<TestCandidate>, String> {
        if limit == 0 || files.is_empty() || !self.exists() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut rows = conn
            .query(
                "
                SELECT path
                FROM discovered_files
                WHERE project_id = ?1
                ORDER BY path ASC
                ",
                params![LOCAL_PROJECT_ID],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut known_files = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            known_files.push(row.get::<String>(0).map_err(|error| error.to_string())?);
        }

        Ok(testmap::likely_tests_for_files(files, &known_files, limit))
    }

    pub async fn recall_symbols(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<CodeSymbol>, String> {
        if limit == 0 || !self.exists() {
            return Ok(Vec::new());
        }

        let terms = query_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut rows = conn
            .query(
                "
                SELECT path, language, name, kind, line_start, line_end, signature
                FROM code_symbols
                WHERE project_id = ?1
                ORDER BY indexed_at_ms DESC, path ASC, line_start ASC
                LIMIT 2000
                ",
                params![LOCAL_PROJECT_ID],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut matches = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            let symbol = CodeSymbol {
                path: row.get::<String>(0).map_err(|error| error.to_string())?,
                language: row
                    .get::<Option<String>>(1)
                    .map_err(|error| error.to_string())?,
                name: row.get::<String>(2).map_err(|error| error.to_string())?,
                kind: row.get::<String>(3).map_err(|error| error.to_string())?,
                line_start: row.get::<i64>(4).map_err(|error| error.to_string())?,
                line_end: row
                    .get::<Option<i64>>(5)
                    .map_err(|error| error.to_string())?,
                signature: row.get::<String>(6).map_err(|error| error.to_string())?,
            };
            let score = code_symbol_score(&symbol, &terms, query);
            if score > 0 {
                matches.push((score, symbol));
            }
        }

        matches.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.path.cmp(&right.1.path))
                .then_with(|| left.1.line_start.cmp(&right.1.line_start))
        });
        matches.truncate(limit);
        Ok(matches.into_iter().map(|(_, symbol)| symbol).collect())
    }

    pub async fn start_session(&self, task: &str) -> Result<Session, String> {
        self.init().await?;
        let conn = self.connect().await?;
        let started_at_ms = now_ms()?;
        let session = Session {
            id: format!("ses_{started_at_ms}"),
            task: task.trim().to_string(),
            branch: current_branch().unwrap_or(None),
            started_at_ms,
            ended_at_ms: None,
            final_summary: None,
        };

        conn.execute(
            "
            INSERT INTO sessions (id, project_id, task, branch, started_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                session.id.clone(),
                LOCAL_PROJECT_ID,
                session.task.clone(),
                session.branch.clone(),
                session.started_at_ms
            ],
        )
        .await
        .map_err(|error| error.to_string())?;

        Ok(session)
    }

    pub async fn record_session_event(
        &self,
        kind: &str,
        detail: &str,
    ) -> Result<SessionEvent, String> {
        self.init().await?;
        let conn = self.connect().await?;
        let session_id = active_session_id(&conn).await?;
        let created_at_ms = now_ms()?;
        let event = SessionEvent {
            id: format!("evt_{created_at_ms}"),
            session_id,
            kind: kind.trim().to_string(),
            detail: detail.trim().to_string(),
            created_at_ms,
        };

        conn.execute(
            "
            INSERT INTO session_events (id, session_id, kind, detail, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                event.id.clone(),
                event.session_id.clone(),
                event.kind.clone(),
                event.detail.clone(),
                event.created_at_ms
            ],
        )
        .await
        .map_err(|error| error.to_string())?;

        Ok(event)
    }

    pub async fn end_session(&self, summary: Option<&str>) -> Result<Session, String> {
        self.init().await?;
        let conn = self.connect().await?;
        let session_id = active_session_id(&conn).await?;
        let ended_at_ms = now_ms()?;
        let summary = summary
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .map(str::to_string);

        conn.execute(
            "
            UPDATE sessions
            SET ended_at_ms = ?1, final_summary = ?2
            WHERE id = ?3
            ",
            params![ended_at_ms, summary.clone(), session_id.clone()],
        )
        .await
        .map_err(|error| error.to_string())?;

        session_by_id(&conn, &session_id)
            .await?
            .ok_or_else(|| "ended session was not found".to_string())
    }

    pub async fn recent_session_facts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SessionFact>, String> {
        if limit == 0 || !self.exists() {
            return Ok(Vec::new());
        }

        let terms = query_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut rows = conn
            .query(
                "
                SELECT s.id, 'task', s.task, s.started_at_ms
                FROM sessions AS s
                WHERE s.project_id = ?1
                UNION ALL
                SELECT s.id, e.kind, e.detail, e.created_at_ms
                FROM session_events AS e
                JOIN sessions AS s ON s.id = e.session_id
                WHERE s.project_id = ?1
                UNION ALL
                SELECT s.id, 'summary', s.final_summary, COALESCE(s.ended_at_ms, s.started_at_ms)
                FROM sessions AS s
                WHERE s.project_id = ?1 AND s.final_summary IS NOT NULL
                ORDER BY 4 DESC
                LIMIT 100
                ",
                params![LOCAL_PROJECT_ID],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut facts = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            let fact = SessionFact {
                session_id: row.get::<String>(0).map_err(|error| error.to_string())?,
                kind: row.get::<String>(1).map_err(|error| error.to_string())?,
                detail: row.get::<String>(2).map_err(|error| error.to_string())?,
                created_at_ms: row.get::<i64>(3).map_err(|error| error.to_string())?,
            };
            if session_fact_score(&fact, &terms) > 0 {
                facts.push(fact);
            }
        }

        facts.sort_by(|left, right| {
            session_fact_score(right, &terms)
                .cmp(&session_fact_score(left, &terms))
                .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
        });
        facts.truncate(limit);
        Ok(facts)
    }

    pub async fn recall(&self, query: &str, limit: usize) -> Result<Vec<Memory>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let terms = query_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        if !self.exists() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let fts_matches = self.fts_recall(&conn, &terms, query, limit).await?;
        let vector_matches = self.vector_recall(&conn, query, limit).await?;

        if fts_matches.is_empty() && vector_matches.is_empty() {
            return self.recall_from_memory_scan(query, &terms, limit).await;
        }

        let mut matches = merge_recall_candidates(fts_matches, vector_matches);
        matches.sort_by(|left, right| {
            right
                .ranking_score()
                .total_cmp(&left.ranking_score())
                .then_with(|| right.memory.created_at_ms.cmp(&left.memory.created_at_ms))
        });
        matches.truncate(limit);
        Ok(matches.into_iter().map(|matched| matched.memory).collect())
    }

    async fn fts_recall(
        &self,
        conn: &Connection,
        terms: &[String],
        query: &str,
        limit: usize,
    ) -> Result<Vec<RankedMemory>, String> {
        let search_query = fts_query(terms);
        let candidate_limit = i64::try_from(limit.max(50)).map_err(|error| error.to_string())?;
        let mut rows = conn
            .query(
                "
                SELECT m.id, m.created_at_ms, m.kind, m.text, bm25(memories_fts) AS fts_rank
                FROM memories_fts
                JOIN memories AS m ON m.rowid = memories_fts.rowid
                WHERE memories_fts MATCH ?1
                ORDER BY fts_rank, m.created_at_ms DESC
                LIMIT ?2
                ",
                params![search_query, candidate_limit],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut matches = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            let memory = Memory {
                id: row.get::<String>(0).map_err(|error| error.to_string())?,
                created_at_ms: row.get::<i64>(1).map_err(|error| error.to_string())?,
                kind: row.get::<String>(2).map_err(|error| error.to_string())?,
                text: row.get::<String>(3).map_err(|error| error.to_string())?,
            };
            let term_score = recall_score(&memory, terms, query);
            if term_score > 0 {
                matches.push(RankedMemory {
                    memory,
                    term_score,
                    fts_rank: Some(row.get::<f64>(4).map_err(|error| error.to_string())?),
                    vector_rank: None,
                });
            }
        }

        Ok(matches)
    }

    async fn vector_recall(
        &self,
        conn: &Connection,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RankedMemory>, String> {
        let embedding = self.embedding_provider()?.embed(query)?;
        let query_vector = embedding.to_vector_literal();
        let candidate_limit = i64::try_from(limit.max(50)).map_err(|error| error.to_string())?;
        let mut rows = conn
            .query(
                "
                WITH vector_matches AS (
                    SELECT id, row_number() OVER () AS vector_rank
                    FROM vector_top_k('memory_embeddings_vector_idx', ?1, ?2)
                )
                SELECT m.id, m.created_at_ms, m.kind, m.text, vector_matches.vector_rank
                FROM vector_matches
                JOIN memory_embeddings AS e ON e.rowid = vector_matches.id
                JOIN memories AS m ON m.id = e.memory_id
                ORDER BY vector_matches.vector_rank ASC, m.created_at_ms DESC
                LIMIT ?2
                ",
                params![query_vector, candidate_limit],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut matches = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            matches.push(RankedMemory {
                memory: Memory {
                    id: row.get::<String>(0).map_err(|error| error.to_string())?,
                    created_at_ms: row.get::<i64>(1).map_err(|error| error.to_string())?,
                    kind: row.get::<String>(2).map_err(|error| error.to_string())?,
                    text: row.get::<String>(3).map_err(|error| error.to_string())?,
                },
                term_score: 0,
                fts_rank: None,
                vector_rank: Some(row.get::<i64>(4).map_err(|error| error.to_string())?),
            });
        }

        Ok(matches)
    }

    async fn recall_from_memory_scan(
        &self,
        query: &str,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<Memory>, String> {
        let mut matches = self
            .memories()
            .await?
            .into_iter()
            .filter_map(|memory| {
                let score = recall_score(&memory, &terms, query);
                (score > 0).then_some((score, memory))
            })
            .collect::<Vec<_>>();

        matches.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.created_at_ms.cmp(&left.1.created_at_ms))
        });
        matches.truncate(limit);
        Ok(matches.into_iter().map(|(_, memory)| memory).collect())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join(HUGR_DB)
    }

    pub fn embedding_provider_summary(&self) -> String {
        match self.embedding_provider() {
            Ok(provider) => format!(
                "{} ({} dimensions)",
                provider.model(),
                provider.dimensions()
            ),
            Err(error) => format!("configuration error: {error}"),
        }
    }

    pub fn storage_summary(&self) -> String {
        match self.storage_config() {
            Ok(config) => config.summary(),
            Err(error) => format!("configuration error: {error}"),
        }
    }

    async fn connect(&self) -> Result<Connection, String> {
        let storage_config = self.storage_config()?;
        if matches!(storage_config.mode, StorageMode::Remote) {
            return Err(
                "HUGR_STORAGE_MODE=remote is configured but remote storage is not implemented yet"
                    .to_string(),
            );
        }

        let db = Builder::new_local(self.db_path())
            .build()
            .await
            .map_err(|error| error.to_string())?;
        let conn = db.connect().map_err(|error| error.to_string())?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .map_err(|error| error.to_string())?;
        Ok(conn)
    }

    fn embedding_provider(&self) -> Result<&SelectedEmbeddingProvider, String> {
        self.embedding_provider
            .as_ref()
            .map_err(|error| error.clone())
    }

    fn storage_config(&self) -> Result<&StorageConfig, String> {
        self.storage_config.as_ref().map_err(|error| error.clone())
    }
}

impl StorageMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "hybrid" => Ok(Self::Hybrid),
            "remote" => Ok(Self::Remote),
            unknown => Err(format!(
                "invalid HUGR_STORAGE_MODE '{unknown}', expected local, hybrid, or remote"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Hybrid => "hybrid",
            Self::Remote => "remote",
        }
    }

    fn requires_remote_config(self) -> bool {
        matches!(self, Self::Hybrid | Self::Remote)
    }
}

impl StorageConfig {
    #[cfg(test)]
    fn local() -> Self {
        Self {
            mode: StorageMode::Local,
            remote_url: None,
            auth_token_configured: false,
        }
    }

    fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    fn from_lookup<F>(lookup: F) -> Result<Self, String>
    where
        F: Fn(&str) -> Option<String>,
    {
        let mode = lookup_first(&lookup, &["HUGR_STORAGE_MODE"])
            .map(|value| StorageMode::parse(&value))
            .transpose()?
            .unwrap_or(StorageMode::Local);
        let remote_url = lookup_first(
            &lookup,
            &[
                "HUGR_REMOTE_DATABASE_URL",
                "HUGR_LIBSQL_URL",
                "TURSO_DATABASE_URL",
                "LIBSQL_URL",
            ],
        );
        let auth_token_configured = lookup_first(
            &lookup,
            &[
                "HUGR_REMOTE_AUTH_TOKEN",
                "HUGR_LIBSQL_AUTH_TOKEN",
                "TURSO_AUTH_TOKEN",
                "LIBSQL_AUTH_TOKEN",
            ],
        )
        .is_some();

        if mode.requires_remote_config() {
            if remote_url.is_none() {
                return Err(format!(
                    "HUGR_STORAGE_MODE={} requires HUGR_REMOTE_DATABASE_URL",
                    mode.as_str()
                ));
            }
            if !auth_token_configured {
                return Err(format!(
                    "HUGR_STORAGE_MODE={} requires HUGR_REMOTE_AUTH_TOKEN",
                    mode.as_str()
                ));
            }
        }

        Ok(Self {
            mode,
            remote_url,
            auth_token_configured,
        })
    }

    fn summary(&self) -> String {
        let auth_status = if self.auth_token_configured {
            "auth configured"
        } else {
            "auth missing"
        };
        match self.mode {
            StorageMode::Local if self.remote_url.is_some() => {
                format!("local (remote configured, inactive, {auth_status})")
            }
            StorageMode::Local => "local".to_string(),
            StorageMode::Hybrid => {
                format!("hybrid (local active, remote sync configured but disabled, {auth_status})")
            }
            StorageMode::Remote => format!("remote configured (not implemented, {auth_status})"),
        }
    }
}

fn lookup_first<F>(lookup: &F, names: &[&str]) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    names.iter().find_map(|name| {
        lookup(name)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

async fn upsert_project(conn: &Connection, project: ProjectInput) -> Result<(), String> {
    let now = now_ms()?;

    conn.execute(
        "
        INSERT INTO projects (
            id,
            name,
            root_path,
            git_remote,
            default_branch,
            created_at_ms,
            updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
        ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            root_path = excluded.root_path,
            git_remote = excluded.git_remote,
            default_branch = excluded.default_branch,
            updated_at_ms = excluded.updated_at_ms
        ",
        params![
            project.id,
            project.name,
            project.root_path,
            project.git_remote,
            project.default_branch,
            now
        ],
    )
    .await
    .map_err(|error| error.to_string())?;

    Ok(())
}

async fn project_from_conn(conn: &Connection) -> Result<Option<Project>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, name, root_path, git_remote, default_branch, created_at_ms, updated_at_ms
            FROM projects
            WHERE id = ?1
            LIMIT 1
            ",
            params![LOCAL_PROJECT_ID],
        )
        .await
        .map_err(|error| error.to_string())?;

    let Some(row) = rows.next().await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };

    Ok(Some(Project {
        id: row.get::<String>(0).map_err(|error| error.to_string())?,
        name: row.get::<String>(1).map_err(|error| error.to_string())?,
        root_path: row.get::<String>(2).map_err(|error| error.to_string())?,
        git_remote: row
            .get::<Option<String>>(3)
            .map_err(|error| error.to_string())?,
        default_branch: row
            .get::<Option<String>>(4)
            .map_err(|error| error.to_string())?,
        created_at_ms: row.get::<i64>(5).map_err(|error| error.to_string())?,
        updated_at_ms: row.get::<i64>(6).map_err(|error| error.to_string())?,
    }))
}

fn code_symbol_from_row(row: &Row) -> Result<CodeSymbol, String> {
    Ok(CodeSymbol {
        path: row.get::<String>(0).map_err(|error| error.to_string())?,
        language: row
            .get::<Option<String>>(1)
            .map_err(|error| error.to_string())?,
        name: row.get::<String>(2).map_err(|error| error.to_string())?,
        kind: row.get::<String>(3).map_err(|error| error.to_string())?,
        line_start: row.get::<i64>(4).map_err(|error| error.to_string())?,
        line_end: row
            .get::<Option<i64>>(5)
            .map_err(|error| error.to_string())?,
        signature: row.get::<String>(6).map_err(|error| error.to_string())?,
    })
}

fn code_reference_from_row(row: &Row) -> Result<CodeReference, String> {
    Ok(CodeReference {
        path: row.get::<String>(0).map_err(|error| error.to_string())?,
        language: row
            .get::<Option<String>>(1)
            .map_err(|error| error.to_string())?,
        target_path: row.get::<String>(2).map_err(|error| error.to_string())?,
        target_name: row.get::<String>(3).map_err(|error| error.to_string())?,
        target_kind: row.get::<String>(4).map_err(|error| error.to_string())?,
        kind: row.get::<String>(5).map_err(|error| error.to_string())?,
        line_start: row.get::<i64>(6).map_err(|error| error.to_string())?,
        excerpt: row.get::<String>(7).map_err(|error| error.to_string())?,
    })
}

fn normalize_target(target: &str) -> String {
    target.trim().trim_start_matches("./").replace('\\', "/")
}

fn reference_is_in_symbol(reference: &CodeReference, symbol: &CodeSymbol) -> bool {
    let line_end = symbol.line_end.unwrap_or(symbol.line_start);
    reference.path == symbol.path
        && reference.line_start >= symbol.line_start
        && reference.line_start <= line_end
}

fn current_project_input() -> Result<ProjectInput, String> {
    let root = env::current_dir().map_err(|error| error.to_string())?;
    let root = root.canonicalize().unwrap_or(root);
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("project")
        .to_string();

    Ok(ProjectInput {
        id: LOCAL_PROJECT_ID.to_string(),
        name,
        root_path: root.display().to_string(),
        git_remote: git_output(&root, &["config", "--get", "remote.origin.url"]),
        default_branch: detect_default_branch(&root),
    })
}

fn detect_default_branch(root: &Path) -> Option<String> {
    git_output(
        root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .map(|branch| {
        branch
            .strip_prefix("origin/")
            .unwrap_or(branch.as_str())
            .to_string()
    })
    .or_else(|| git_output(root, &["branch", "--show-current"]))
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn current_branch() -> Result<Option<String>, String> {
    let root = env::current_dir().map_err(|error| error.to_string())?;
    Ok(git_output(&root, &["branch", "--show-current"]))
}

async fn active_session_id(conn: &Connection) -> Result<String, String> {
    let mut rows = conn
        .query(
            "
            SELECT id
            FROM sessions
            WHERE project_id = ?1 AND ended_at_ms IS NULL
            ORDER BY started_at_ms DESC
            LIMIT 1
            ",
            params![LOCAL_PROJECT_ID],
        )
        .await
        .map_err(|error| error.to_string())?;

    let Some(row) = rows.next().await.map_err(|error| error.to_string())? else {
        return Err("no active session; run `hugr session start <task>` first".to_string());
    };

    row.get::<String>(0).map_err(|error| error.to_string())
}

async fn session_by_id(conn: &Connection, session_id: &str) -> Result<Option<Session>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, task, branch, started_at_ms, ended_at_ms, final_summary
            FROM sessions
            WHERE id = ?1
            LIMIT 1
            ",
            params![session_id],
        )
        .await
        .map_err(|error| error.to_string())?;

    let Some(row) = rows.next().await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };

    Ok(Some(Session {
        id: row.get::<String>(0).map_err(|error| error.to_string())?,
        task: row.get::<String>(1).map_err(|error| error.to_string())?,
        branch: row
            .get::<Option<String>>(2)
            .map_err(|error| error.to_string())?,
        started_at_ms: row.get::<i64>(3).map_err(|error| error.to_string())?,
        ended_at_ms: row
            .get::<Option<i64>>(4)
            .map_err(|error| error.to_string())?,
        final_summary: row
            .get::<Option<String>>(5)
            .map_err(|error| error.to_string())?,
    }))
}

fn session_fact_score(fact: &SessionFact, terms: &[String]) -> usize {
    let text = format!("{} {}", fact.kind, fact.detail).to_lowercase();
    terms
        .iter()
        .filter(|term| text.contains(term.as_str()))
        .count()
}

fn code_symbol_score(symbol: &CodeSymbol, terms: &[String], query: &str) -> usize {
    let name = symbol.name.to_lowercase();
    let path = symbol.path.to_lowercase();
    let kind = symbol.kind.to_lowercase();
    let signature = symbol.signature.to_lowercase();
    let query = query.to_lowercase();
    let exact_bonus = if name == query || signature.contains(&query) {
        12
    } else {
        0
    };

    exact_bonus
        + terms
            .iter()
            .map(|term| {
                let mut score = 0;
                if name.contains(term) {
                    score += 5;
                }
                if kind == *term {
                    score += 3;
                }
                if path.contains(term) {
                    score += 2;
                }
                if signature.contains(term) {
                    score += 1;
                }
                score
            })
            .sum::<usize>()
}

struct RankedMemory {
    memory: Memory,
    term_score: usize,
    fts_rank: Option<f64>,
    vector_rank: Option<i64>,
}

impl RankedMemory {
    fn merge(&mut self, other: Self) {
        self.term_score = self.term_score.max(other.term_score);
        self.fts_rank = match (self.fts_rank, other.fts_rank) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left @ Some(_), None) => left,
            (None, right) => right,
        };
        self.vector_rank = match (self.vector_rank, other.vector_rank) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left @ Some(_), None) => left,
            (None, right) => right,
        };
    }

    fn ranking_score(&self) -> f64 {
        let text_score = self.term_score as f64 * 10.0;
        let fts_score = self
            .fts_rank
            .map(|rank| 1.0 / (1.0 + rank.abs()))
            .unwrap_or(0.0);
        let vector_score = self.vector_rank.map(vector_rank_score).unwrap_or(0.0);

        text_score + fts_score + vector_score
    }
}

fn vector_rank_score(rank: i64) -> f64 {
    1.0 / (rank.max(1) as f64)
}

fn merge_recall_candidates(
    fts_matches: Vec<RankedMemory>,
    vector_matches: Vec<RankedMemory>,
) -> Vec<RankedMemory> {
    let mut merged = HashMap::<String, RankedMemory>::new();

    for matched in fts_matches.into_iter().chain(vector_matches) {
        match merged.get_mut(&matched.memory.id) {
            Some(existing) => existing.merge(matched),
            None => {
                merged.insert(matched.memory.id.clone(), matched);
            }
        }
    }

    merged.into_values().collect()
}

fn recall_score(memory: &Memory, terms: &[String], query: &str) -> usize {
    let text = memory.text.to_lowercase();
    let query = query.to_lowercase();
    let exact_bonus = if text.contains(&query) { 10 } else { 0 };
    exact_bonus
        + terms
            .iter()
            .filter(|term| text.contains(term.as_str()))
            .count()
}

fn fts_query(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|char: char| !char.is_alphanumeric() && char != '_' && char != '-')
        .filter(|term| term.len() > 2)
        .map(|term| term.to_lowercase())
        .collect()
}

fn now_ms() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| error.to_string())?;
    i64::try_from(millis).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{Memory, StorageConfig, StorageMode, Store, fts_query, query_terms, recall_score};
    use crate::code::{CodeReference, CodeSymbol};
    use crate::discovery::FileCandidate;
    use crate::embedding::{DEFAULT_EMBEDDING_DIMENSIONS, DETERMINISTIC_MODEL};
    use libsql::{Connection, params};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestStore {
        store: Store,
        workspace: PathBuf,
    }

    impl TestStore {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let workspace = std::env::temp_dir().join(format!("hugr_{name}_{unique}"));
            let store = Store::open_at(workspace.join(".hugr"));

            Self { store, workspace }
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.workspace);
        }
    }

    fn env_lookup<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn recall_scores_query_terms() {
        let memory = Memory {
            id: "mem_1".into(),
            created_at_ms: 1,
            kind: "fact".into(),
            text: "plugin hooks run after configuration is loaded".into(),
        };
        let terms = query_terms("add plugin hooks");
        assert!(recall_score(&memory, &terms, "add plugin hooks") > 0);
    }

    #[test]
    fn fts_query_ors_quoted_terms() {
        let terms = query_terms("add plugin hooks");
        assert_eq!(fts_query(&terms), "\"add\" OR \"plugin\" OR \"hooks\"");
    }

    #[test]
    fn storage_config_defaults_to_local() {
        let config = StorageConfig::from_lookup(|_| None).unwrap();

        assert_eq!(config.mode, StorageMode::Local);
        assert_eq!(config.remote_url, None);
        assert!(!config.auth_token_configured);
        assert_eq!(config.summary(), "local");
    }

    #[test]
    fn storage_config_reads_hybrid_remote_placeholder() {
        let config = StorageConfig::from_lookup(env_lookup(&[
            ("HUGR_STORAGE_MODE", "hybrid"),
            ("HUGR_REMOTE_DATABASE_URL", "libsql://example.turso.io"),
            ("HUGR_REMOTE_AUTH_TOKEN", "secret-token"),
        ]))
        .unwrap();

        assert_eq!(config.mode, StorageMode::Hybrid);
        assert_eq!(
            config.remote_url.as_deref(),
            Some("libsql://example.turso.io")
        );
        assert!(config.auth_token_configured);
        assert_eq!(
            config.summary(),
            "hybrid (local active, remote sync configured but disabled, auth configured)"
        );
    }

    #[test]
    fn storage_config_requires_remote_credentials() {
        let missing_url =
            StorageConfig::from_lookup(env_lookup(&[("HUGR_STORAGE_MODE", "hybrid")])).unwrap_err();
        assert!(missing_url.contains("requires HUGR_REMOTE_DATABASE_URL"));

        let missing_token = StorageConfig::from_lookup(env_lookup(&[
            ("HUGR_STORAGE_MODE", "hybrid"),
            ("HUGR_REMOTE_DATABASE_URL", "libsql://example.turso.io"),
        ]))
        .unwrap_err();
        assert!(missing_token.contains("requires HUGR_REMOTE_AUTH_TOKEN"));
    }

    #[tokio::test]
    async fn remote_storage_mode_does_not_open_local_database() {
        let mut test = TestStore::new("remote_mode");
        test.store.storage_config = Ok(StorageConfig {
            mode: StorageMode::Remote,
            remote_url: Some("libsql://example.turso.io".to_string()),
            auth_token_configured: true,
        });

        let error = test.store.init().await.unwrap_err();

        assert!(error.contains("remote storage is not implemented"));
        assert!(!test.store.db_path().exists());
    }

    #[tokio::test]
    async fn init_records_initial_schema_migration() {
        let test = TestStore::new("migration");
        test.store.init().await.unwrap();

        let conn = test.store.connect().await.unwrap();
        assert!(object_exists(&conn, "table", "memories").await);
        assert!(object_exists(&conn, "table", "schema_migrations").await);
        assert!(object_exists(&conn, "table", "memories_fts").await);
        assert!(object_exists(&conn, "table", "projects").await);
        assert!(object_exists(&conn, "table", "discovered_files").await);
        assert!(object_exists(&conn, "table", "sessions").await);
        assert!(object_exists(&conn, "table", "session_events").await);
        assert!(object_exists(&conn, "table", "code_symbols").await);
        assert!(object_exists(&conn, "table", "code_references").await);
        assert!(object_exists(&conn, "index", "code_symbols_project_name_idx").await);
        assert!(object_exists(&conn, "index", "code_references_target_name_idx").await);
        assert!(object_exists(&conn, "index", "memory_embeddings_vector_idx").await);

        let mut rows = conn
            .query(
                "SELECT version, name FROM schema_migrations ORDER BY version",
                (),
            )
            .await
            .unwrap();
        let mut migrations = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            migrations.push((row.get::<i64>(0).unwrap(), row.get::<String>(1).unwrap()));
        }

        assert_eq!(
            migrations,
            vec![
                (1, "initial_schema".to_string()),
                (2, "project_registry".to_string()),
                (3, "file_discovery".to_string()),
                (4, "sessions".to_string()),
                (5, "code_symbols".to_string()),
                (6, "code_references".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn init_records_current_project() {
        let test = TestStore::new("project");
        test.store.init().await.unwrap();

        let project = test.store.project().await.unwrap().unwrap();
        let current_dir = std::env::current_dir().unwrap();
        let current_dir = current_dir.canonicalize().unwrap_or(current_dir);

        assert_eq!(project.id, "project_local");
        assert_eq!(project.root_path, current_dir.display().to_string());
        assert!(!project.name.is_empty());
        assert!(project.created_at_ms > 0);
        assert!(project.updated_at_ms >= project.created_at_ms);
    }

    #[tokio::test]
    async fn recall_reads_from_fts() {
        let test = TestStore::new("recall");
        let memory = test
            .store
            .remember("plugin hooks run after configuration is loaded")
            .await
            .unwrap();

        let matches = test.store.recall("add plugin hooks", 5).await.unwrap();

        assert_eq!(matches.first(), Some(&memory));
        assert_eq!(fts_row_count(&test.store, "add plugin hooks").await, 1);
    }

    #[tokio::test]
    async fn vector_recall_reads_from_vector_index() {
        let test = TestStore::new("vector_recall");
        let memory = test
            .store
            .remember("plugin hooks run after configuration is loaded")
            .await
            .unwrap();
        let conn = test.store.connect().await.unwrap();

        let matches = test
            .store
            .vector_recall(&conn, "plugin hooks", 5)
            .await
            .unwrap();

        assert_eq!(
            matches.first().map(|matched| &matched.memory),
            Some(&memory)
        );
        assert_eq!(
            matches.first().and_then(|matched| matched.vector_rank),
            Some(1)
        );
    }

    #[tokio::test]
    async fn remember_writes_deterministic_embedding() {
        let test = TestStore::new("embedding");
        let memory = test
            .store
            .remember("plugin hooks run after configuration is loaded")
            .await
            .unwrap();
        let conn = test.store.connect().await.unwrap();
        let mut rows = conn
            .query(
                "
                SELECT model, dimensions, length(embedding)
                FROM memory_embeddings
                WHERE memory_id = ?1
                ",
                params![memory.id],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();

        assert_eq!(row.get::<String>(0).unwrap(), DETERMINISTIC_MODEL);
        assert_eq!(
            row.get::<i64>(1).unwrap(),
            DEFAULT_EMBEDDING_DIMENSIONS as i64
        );
        assert_eq!(
            row.get::<i64>(2).unwrap(),
            (DEFAULT_EMBEDDING_DIMENSIONS * 4) as i64
        );
        assert!(rows.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn records_discovered_files() {
        let test = TestStore::new("discovered_files");
        let file = FileCandidate {
            path: "src/plugin_hooks.rs".to_string(),
            score: 12,
            language: Some("rust".to_string()),
            size_bytes: Some(42),
        };

        test.store
            .record_discovered_files(std::slice::from_ref(&file))
            .await
            .unwrap();

        let conn = test.store.connect().await.unwrap();
        let mut rows = conn
            .query(
                "
                SELECT project_id, path, language, size_bytes
                FROM discovered_files
                WHERE path = ?1
                ",
                params![file.path],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();

        assert_eq!(row.get::<String>(0).unwrap(), "project_local");
        assert_eq!(row.get::<String>(1).unwrap(), "src/plugin_hooks.rs");
        assert_eq!(row.get::<String>(2).unwrap(), "rust");
        assert_eq!(row.get::<i64>(3).unwrap(), 42);
        assert!(rows.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn records_and_recalls_code_symbols() {
        let test = TestStore::new("code_symbols");
        let file = FileCandidate {
            path: "src/plugin_hooks.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(120),
        };
        let symbol = CodeSymbol {
            path: file.path.clone(),
            language: file.language.clone(),
            name: "PluginHooks".to_string(),
            kind: "struct".to_string(),
            line_start: 3,
            line_end: None,
            signature: "pub struct PluginHooks".to_string(),
        };

        test.store
            .record_code_index(
                std::slice::from_ref(&file),
                std::slice::from_ref(&symbol),
                &[],
            )
            .await
            .unwrap();
        let matches = test.store.recall_symbols("plugin hooks", 5).await.unwrap();

        assert_eq!(matches, vec![symbol]);

        test.store
            .record_code_index(&[file], &[], &[])
            .await
            .unwrap();
        assert!(
            test.store
                .recall_symbols("plugin hooks", 5)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn records_code_references_for_impact() {
        let test = TestStore::new("code_references");
        let source = FileCandidate {
            path: "src/main.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(80),
        };
        let target = FileCandidate {
            path: "src/plugin_hooks.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(120),
        };
        let symbol = CodeSymbol {
            path: target.path.clone(),
            language: target.language.clone(),
            name: "run_after_config".to_string(),
            kind: "function".to_string(),
            line_start: 3,
            line_end: Some(10),
            signature: "pub fn run_after_config()".to_string(),
        };
        let reference = CodeReference {
            path: source.path.clone(),
            language: source.language.clone(),
            target_path: symbol.path.clone(),
            target_name: symbol.name.clone(),
            target_kind: symbol.kind.clone(),
            kind: "call".to_string(),
            line_start: 8,
            excerpt: "run_after_config();".to_string(),
        };

        test.store
            .record_code_index(
                &[source.clone(), target],
                std::slice::from_ref(&symbol),
                &[reference],
            )
            .await
            .unwrap();

        let symbols = test
            .store
            .symbols_for_target("run_after_config", 5)
            .await
            .unwrap();
        let references = test.store.references_to_symbols(&symbols, 5).await.unwrap();
        let outbound = test
            .store
            .references_from_symbols(&symbols, 5)
            .await
            .unwrap();
        let file_outbound = test
            .store
            .references_from_path("src/main.rs", 5)
            .await
            .unwrap();

        assert_eq!(symbols, vec![symbol]);
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].path, "src/main.rs");
        assert_eq!(references[0].kind, "call");
        assert!(outbound.is_empty());
        assert_eq!(file_outbound.len(), 1);
        assert_eq!(file_outbound[0].target_name, "run_after_config");
    }

    #[tokio::test]
    async fn maps_likely_tests_from_discovered_files() {
        let test = TestStore::new("likely_tests");
        let files = vec![
            FileCandidate {
                path: "src/plugin_hooks.rs".to_string(),
                score: 0,
                language: Some("rust".to_string()),
                size_bytes: Some(120),
            },
            FileCandidate {
                path: "tests/plugin_hooks.rs".to_string(),
                score: 0,
                language: Some("rust".to_string()),
                size_bytes: Some(90),
            },
        ];

        test.store.record_discovered_files(&files).await.unwrap();
        let tests = test
            .store
            .likely_tests_for_files(&["src/plugin_hooks.rs".to_string()], 5)
            .await
            .unwrap();

        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].path, "tests/plugin_hooks.rs");
    }

    #[tokio::test]
    async fn records_session_lifecycle_and_recent_facts() {
        let test = TestStore::new("sessions");
        let session = test.store.start_session("add plugin hooks").await.unwrap();
        let event = test
            .store
            .record_session_event("test", "cargo test passed for plugin hooks")
            .await
            .unwrap();
        let ended = test
            .store
            .end_session(Some("plugin hooks are wired"))
            .await
            .unwrap();

        assert_eq!(event.session_id, session.id);
        assert_eq!(ended.id, session.id);
        assert!(ended.ended_at_ms.is_some());
        assert_eq!(
            ended.final_summary.as_deref(),
            Some("plugin hooks are wired")
        );

        let facts = test
            .store
            .recent_session_facts("plugin hooks", 5)
            .await
            .unwrap();

        assert!(facts.iter().any(|fact| fact.kind == "test"));
        assert!(facts.iter().any(|fact| fact.kind == "summary"));
    }

    async fn object_exists(conn: &Connection, kind: &str, name: &str) -> bool {
        let mut rows = conn
            .query(
                "SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2 LIMIT 1",
                params![kind, name],
            )
            .await
            .unwrap();

        rows.next().await.unwrap().is_some()
    }

    async fn fts_row_count(store: &Store, query: &str) -> usize {
        let conn = store.connect().await.unwrap();
        let terms = query_terms(query);
        let mut rows = conn
            .query(
                "SELECT count(*) FROM memories_fts WHERE memories_fts MATCH ?1",
                params![fts_query(&terms)],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();

        row.get::<i64>(0).unwrap() as usize
    }
}
