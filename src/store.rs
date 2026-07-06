use crate::code::{self, CodeReference, CodeSymbol};
use crate::discovery::FileCandidate;
use crate::embedding::{Embedding, EmbeddingProvider, SelectedEmbeddingProvider};
use crate::migrations;
use crate::testmap::{self, TestCandidate};
use libsql::{Builder, Connection, Row, params};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const HUGR_DIR: &str = ".hugr";
const HUGR_DB: &str = "hugr.db";
const LOCAL_PROJECT_ID: &str = "project_local";
pub(crate) const HUGR_API_CONTRACT_VERSION: &str = "hugr-api-v1";
pub(crate) const HUGR_API_ROUTES: &[&str] = &[
    "GET /v1/memories",
    "POST /v1/memories",
    "GET /v1/storage",
    "POST /v1/storage",
    "GET /v1/sync/status",
    "POST /v1/sync/push",
    "POST /v1/sync/pull",
    "GET /v1/sync/history",
];
static SESSION_EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);
static DIAGNOSTIC_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageMode {
    Local,
    Hybrid,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncBackend {
    DirectLibsql,
    HugrApi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncClass {
    Memories,
    Sources,
    Entities,
    Edges,
    Embeddings,
    ContextPacks,
    Diagnostics,
    SessionSummaries,
    FullSource,
    RawCommandOutput,
    Secrets,
    PrivateNotes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SyncTableKind {
    Projects,
    Memories,
    MemoryEmbeddings,
    Sources,
    DiscoveredFiles,
    Entities,
    CodeSymbols,
    Edges,
    CodeReferences,
    TestMappings,
    SourceEmbeddings,
    Sessions,
    ContextPacks,
    Diagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StorageConfig {
    mode: StorageMode,
    backend: SyncBackend,
    remote_url: Option<String>,
    remote_auth_token: Option<String>,
    auth_token_configured: bool,
    sync_classes: Vec<SyncClass>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncExecutionPlan {
    pub storage_mode: String,
    pub backend: String,
    pub local_writes_enabled: bool,
    pub remote_configured: bool,
    pub remote_auth_configured: bool,
    pub remote_reads_enabled: bool,
    pub remote_writes_enabled: bool,
    pub remote_endpoint: Option<String>,
    pub api_contract_version: Option<String>,
    pub api_routes: Vec<String>,
    pub sync_classes: Vec<String>,
    pub explicit_opt_in_classes: Vec<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPushResult {
    pub run_id: Option<String>,
    pub dry_run: bool,
    pub backend: String,
    pub status: String,
    pub tables: Vec<SyncTableResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPullResult {
    pub run_id: Option<String>,
    pub dry_run: bool,
    pub backend: String,
    pub status: String,
    pub tables: Vec<SyncTableResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncTableResult {
    pub class: String,
    pub table: String,
    pub row_count: usize,
    pub inserted_count: usize,
    pub updated_count: usize,
    pub skipped_count: usize,
    pub conflict_count: usize,
    pub executed: bool,
    pub conflicts: Vec<SyncConflictSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConflictSummary {
    pub reason: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRunHistory {
    pub id: String,
    pub operation: String,
    pub backend: String,
    pub status: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub tables: Vec<SyncTableResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SyncApiTablePayload {
    pub result: SyncTableResult,
    pub records: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneSummary {
    pub missing_paths: usize,
    pub discovered_files: u64,
    pub symbols: u64,
    pub references: u64,
    pub test_mappings: u64,
    pub source_embeddings: u64,
}

impl PruneSummary {
    pub fn is_empty(&self) -> bool {
        self.missing_paths == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub id: String,
    pub created_at_ms: i64,
    pub kind: String,
    pub text: String,
    pub structured_payload: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySource {
    pub kind: String,
    pub locator: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryWriteOptions {
    pub source: Option<MemorySource>,
    pub confidence: Option<f64>,
    pub sensitivity: Option<String>,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPromotionResult {
    pub session_id: String,
    pub task: String,
    pub fact_count: usize,
    pub memory: Memory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetResult {
    pub query: String,
    pub forgotten_count: usize,
    pub forgotten_at: String,
    pub memories: Vec<Memory>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryMaintenanceReport {
    pub active_count: usize,
    pub retired_count: usize,
    pub duplicate_groups: Vec<DuplicateMemoryGroup>,
    pub stale_candidates: Vec<StaleMemoryCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateMemoryGroup {
    pub normalized_text: String,
    pub memories: Vec<Memory>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleMemoryCandidate {
    pub reason: String,
    pub signal: String,
    pub shared_terms: Vec<String>,
    pub newer_memory: Memory,
    pub older_memory: Memory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryConsolidationResult {
    pub executed_at: String,
    pub duplicate_groups: Vec<DuplicateMemoryGroup>,
    pub kept_memories: Vec<Memory>,
    pub retired_memories: Vec<Memory>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleRetirementResult {
    pub executed_at: String,
    pub stale_candidates: Vec<StaleMemoryCandidate>,
    pub kept_memories: Vec<Memory>,
    pub retired_memories: Vec<Memory>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNeighbor {
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub path: Option<String>,
    pub target_path: Option<String>,
    pub target_name: Option<String>,
    pub line_start: Option<i64>,
    /// Number of collapsed reference sites this neighbor represents; 1 for a
    /// single line, larger when repeated same-relationship lines were folded
    /// into one entry to save context budget.
    pub site_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshnessSignal {
    pub path: String,
    pub kind: String,
    pub detail: String,
    pub indexed_at_ms: Option<i64>,
    pub modified_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticInput {
    pub source: String,
    pub path: Option<String>,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub severity: String,
    pub code: Option<String>,
    pub message: String,
    pub command: Option<String>,
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
        let storage_config = self.storage_config()?;
        if !matches!(storage_config.mode, StorageMode::Remote) {
            fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
            fs::create_dir_all(self.root.join("sessions")).map_err(|error| error.to_string())?;
        }
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let project = current_project_input()?;
        upsert_project(&conn, project).await?;
        Ok(())
    }

    async fn connect_migrated_without_project_upsert(&self) -> Result<Connection, String> {
        let storage_config = self.storage_config()?;
        if !matches!(storage_config.mode, StorageMode::Remote) {
            fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
            fs::create_dir_all(self.root.join("sessions")).map_err(|error| error.to_string())?;
        }
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        Ok(conn)
    }

    pub fn exists(&self) -> bool {
        self.db_path().exists()
    }

    pub async fn remember(&self, text: &str) -> Result<Memory, String> {
        self.remember_with_source(text, None).await
    }

    pub async fn remember_with_source(
        &self,
        text: &str,
        source: Option<MemorySource>,
    ) -> Result<Memory, String> {
        self.remember_with_options(
            text,
            MemoryWriteOptions {
                source,
                ..MemoryWriteOptions::default()
            },
        )
        .await
    }

    pub async fn remember_with_options(
        &self,
        text: &str,
        options: MemoryWriteOptions,
    ) -> Result<Memory, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return self.remember_via_hugr_api(&storage_config, text, options);
        }

        self.init().await?;
        let conn = self.connect().await?;
        let options = normalize_memory_write_options(options)?;
        let project = project_from_conn(&conn).await?;
        let structured_payload = memory_write_payload(&options, project.as_ref());
        insert_memory(
            &conn,
            self.embedding_provider()?,
            text,
            options.confidence.unwrap_or(1.0),
            options
                .sensitivity
                .clone()
                .unwrap_or_else(|| "normal".to_string()),
            structured_payload,
        )
        .await
    }

    pub async fn memories(&self) -> Result<Vec<Memory>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return hugr_api_active_memories(&storage_config);
        }

        if !self.exists() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        active_memories(&conn).await
    }

    pub async fn forget(&self, query: &str, limit: usize) -> Result<ForgetResult, String> {
        let query = query.trim();
        if query.is_empty() {
            return Err("hugr forget requires a query".to_string());
        }
        let terms = query_terms(query);
        if terms.is_empty() {
            return Err("hugr forget requires at least one searchable term".to_string());
        }

        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return forget_via_hugr_api(&storage_config, query, &terms, limit);
        }

        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut matches = active_memories(&conn)
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
        matches.truncate(limit.max(1));

        let forgotten_at = now_ms()?.to_string();
        let memories = matches
            .into_iter()
            .map(|(_, memory)| memory)
            .collect::<Vec<_>>();

        for memory in &memories {
            conn.execute(
                "
                UPDATE memories
                SET valid_to = ?1
                WHERE id = ?2 AND valid_to IS NULL
                ",
                params![forgotten_at.clone(), memory.id.clone()],
            )
            .await
            .map_err(|error| error.to_string())?;
        }

        Ok(ForgetResult {
            query: query.to_string(),
            forgotten_count: memories.len(),
            forgotten_at,
            memories,
        })
    }

    pub async fn memory_maintenance_report(&self) -> Result<MemoryMaintenanceReport, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return memory_maintenance_report_via_hugr_api(&storage_config);
        }

        if !self.exists() {
            return Ok(MemoryMaintenanceReport {
                active_count: 0,
                retired_count: 0,
                duplicate_groups: Vec::new(),
                stale_candidates: Vec::new(),
            });
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let active = active_memories(&conn).await?;
        let retired_count = table_count_where(&conn, "memories", "valid_to IS NOT NULL").await?;
        let mut grouped = HashMap::<String, Vec<Memory>>::new();

        for memory in &active {
            grouped
                .entry(normalized_memory_text(&memory.text))
                .or_default()
                .push(memory.clone());
        }

        let mut duplicate_groups = grouped
            .into_iter()
            .filter_map(|(normalized_text, mut memories)| {
                (memories.len() > 1).then(|| {
                    memories.sort_by(|left, right| right.created_at_ms.cmp(&left.created_at_ms));
                    DuplicateMemoryGroup {
                        normalized_text,
                        memories,
                    }
                })
            })
            .collect::<Vec<_>>();
        duplicate_groups.sort_by(|left, right| {
            right
                .memories
                .len()
                .cmp(&left.memories.len())
                .then_with(|| left.normalized_text.cmp(&right.normalized_text))
        });

        Ok(MemoryMaintenanceReport {
            active_count: active.len(),
            retired_count,
            duplicate_groups,
            stale_candidates: stale_memory_candidates(&active),
        })
    }

    pub async fn consolidate_duplicate_memories(
        &self,
    ) -> Result<MemoryConsolidationResult, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return consolidate_duplicate_memories_via_hugr_api(&storage_config);
        }

        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let report = self.memory_maintenance_report().await?;
        let executed_at = now_ms()?.to_string();
        let mut kept_memories = Vec::new();
        let mut retired_memories = Vec::new();

        for group in &report.duplicate_groups {
            let Some((kept, retired)) = group.memories.split_first() else {
                continue;
            };
            kept_memories.push(kept.clone());
            for memory in retired {
                conn.execute(
                    "
                    UPDATE memories
                    SET valid_to = ?1,
                        superseded_by = ?2
                    WHERE id = ?3 AND valid_to IS NULL
                    ",
                    params![executed_at.clone(), kept.id.clone(), memory.id.clone()],
                )
                .await
                .map_err(|error| error.to_string())?;
                retired_memories.push(memory.clone());
            }
        }

        Ok(MemoryConsolidationResult {
            executed_at,
            duplicate_groups: report.duplicate_groups,
            kept_memories,
            retired_memories,
        })
    }

    pub async fn retire_stale_memories(&self) -> Result<StaleRetirementResult, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return retire_stale_memories_via_hugr_api(&storage_config);
        }

        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let report = self.memory_maintenance_report().await?;
        let executed_at = now_ms()?.to_string();
        let mut kept_memories = Vec::new();
        let mut retired_memories = Vec::new();
        let mut seen_kept = HashSet::new();
        let mut seen_retired = HashSet::new();

        for candidate in &report.stale_candidates {
            if seen_kept.insert(candidate.newer_memory.id.clone()) {
                kept_memories.push(candidate.newer_memory.clone());
            }
            if !seen_retired.insert(candidate.older_memory.id.clone()) {
                continue;
            }

            conn.execute(
                "
                UPDATE memories
                SET valid_to = ?1,
                    superseded_by = ?2
                WHERE id = ?3 AND valid_to IS NULL
                ",
                params![
                    executed_at.clone(),
                    candidate.newer_memory.id.clone(),
                    candidate.older_memory.id.clone()
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
            retired_memories.push(candidate.older_memory.clone());
        }

        Ok(StaleRetirementResult {
            executed_at,
            stale_candidates: report.stale_candidates,
            kept_memories,
            retired_memories,
        })
    }

    pub async fn sync_current_project(&self) -> Result<Project, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return sync_current_project_via_hugr_api(&storage_config);
        }

        self.init().await?;
        self.project()
            .await?
            .ok_or_else(|| "project registry is empty after initialization".to_string())
    }

    pub async fn project(&self) -> Result<Option<Project>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return project_via_hugr_api(&storage_config);
        }

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

        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return record_discovered_files_via_hugr_api(&storage_config, files);
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

        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return record_code_index_via_hugr_api(
                &storage_config,
                self.embedding_provider()?,
                files,
                symbols,
                references,
            );
        }

        self.init().await?;
        let conn = self.connect().await?;
        let now = now_ms()?;
        let paths = files
            .iter()
            .map(|file| file.path.clone())
            .collect::<HashSet<_>>();

        for path in &paths {
            conn.execute(
                "
                DELETE FROM code_symbols
                WHERE project_id = ?1 AND path = ?2
                ",
                params![LOCAL_PROJECT_ID, path.clone()],
            )
            .await
            .map_err(|error| error.to_string())?;
            conn.execute(
                "
                DELETE FROM code_references
                WHERE project_id = ?1 AND path = ?2
                ",
                params![LOCAL_PROJECT_ID, path.clone()],
            )
            .await
            .map_err(|error| error.to_string())?;
        }

        for symbol in symbols {
            insert_code_symbol(&conn, symbol, now).await?;
        }

        for reference in references {
            insert_code_reference(&conn, reference, now).await?;
        }
        refresh_test_mappings_for_paths(&conn, &paths, now).await?;
        refresh_source_embeddings_for_files(&conn, files, symbols, now, self.embedding_provider()?)
            .await?;

        Ok(())
    }

    /// Removes indexed rows for files that no longer exist on disk so recall,
    /// context, and impact never surface stale symbols or dangling references
    /// after a file is deleted or renamed. Paths are resolved against `root`;
    /// a row is pruned only when its file is genuinely missing, so files merely
    /// truncated out of a ranked discovery pass are left untouched.
    pub async fn prune_missing_index_rows(&self, root: &Path) -> Result<PruneSummary, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) || !self.exists() {
            return Ok(PruneSummary::default());
        }

        let conn = self.connect().await?;
        let mut stored_paths = HashSet::new();
        for table in [
            "discovered_files",
            "code_symbols",
            "code_references",
            "source_embeddings",
        ] {
            collect_stored_paths(&conn, table, "path", &mut stored_paths).await?;
        }
        collect_stored_paths(&conn, "code_references", "target_path", &mut stored_paths).await?;
        collect_stored_paths(&conn, "test_mappings", "source_path", &mut stored_paths).await?;
        collect_stored_paths(&conn, "test_mappings", "test_path", &mut stored_paths).await?;

        let missing_paths = stored_paths
            .into_iter()
            .filter(|path| !root.join(path).exists())
            .collect::<Vec<_>>();
        if missing_paths.is_empty() {
            return Ok(PruneSummary::default());
        }

        let mut summary = PruneSummary::default();
        for path in &missing_paths {
            summary.discovered_files += conn
                .execute(
                    "DELETE FROM discovered_files WHERE project_id = ?1 AND path = ?2",
                    params![LOCAL_PROJECT_ID, path.clone()],
                )
                .await
                .map_err(|error| error.to_string())?;
            summary.symbols += conn
                .execute(
                    "DELETE FROM code_symbols WHERE project_id = ?1 AND path = ?2",
                    params![LOCAL_PROJECT_ID, path.clone()],
                )
                .await
                .map_err(|error| error.to_string())?;
            summary.references += conn
                .execute(
                    "DELETE FROM code_references WHERE project_id = ?1 AND (path = ?2 OR target_path = ?2)",
                    params![LOCAL_PROJECT_ID, path.clone()],
                )
                .await
                .map_err(|error| error.to_string())?;
            summary.test_mappings += conn
                .execute(
                    "DELETE FROM test_mappings WHERE project_id = ?1 AND (source_path = ?2 OR test_path = ?2)",
                    params![LOCAL_PROJECT_ID, path.clone()],
                )
                .await
                .map_err(|error| error.to_string())?;
            summary.source_embeddings += conn
                .execute(
                    "DELETE FROM source_embeddings WHERE project_id = ?1 AND path = ?2",
                    params![LOCAL_PROJECT_ID, path.clone()],
                )
                .await
                .map_err(|error| error.to_string())?;
        }

        summary.missing_paths = missing_paths.len();
        Ok(summary)
    }

    /// Reads every indexed symbol back from local storage. Partial re-index uses
    /// this to keep the full target set available when only a few files are
    /// re-parsed, so cross-file references stay resolvable.
    pub(crate) async fn stored_code_symbols(&self) -> Result<Vec<CodeSymbol>, String> {
        if !self.exists() {
            return Ok(Vec::new());
        }
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "
                SELECT path, language, name, kind, line_start, line_end, signature
                FROM code_symbols
                WHERE project_id = ?1
                ",
                params![LOCAL_PROJECT_ID],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut symbols = Vec::new();
        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            symbols.push(code_symbol_from_row(&row)?);
        }
        Ok(symbols)
    }

    /// Returns the distinct files that currently hold a stored reference whose
    /// target lives in one of `target_paths`. Partial re-index re-scans these
    /// inbound-reference sources so edits to a changed file's symbols correctly
    /// update references pointing at it from unchanged files.
    pub(crate) async fn reference_sources_targeting(
        &self,
        target_paths: &[String],
    ) -> Result<Vec<String>, String> {
        if target_paths.is_empty() || !self.exists() {
            return Ok(Vec::new());
        }
        let conn = self.connect().await?;
        let target_set = target_paths.iter().cloned().collect::<HashSet<_>>();
        let mut rows = conn
            .query(
                "
                SELECT DISTINCT path, target_path
                FROM code_references
                WHERE project_id = ?1
                ",
                params![LOCAL_PROJECT_ID],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut sources = HashSet::new();
        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            let path = row.get::<String>(0).map_err(|error| error.to_string())?;
            let target_path = row.get::<String>(1).map_err(|error| error.to_string())?;
            if target_set.contains(&target_path) {
                sources.insert(path);
            }
        }
        Ok(sources.into_iter().collect())
    }

    /// Applies a scoped index update: replaces `code_symbols` for exactly
    /// `symbol_paths` and `code_references` for exactly `reference_paths`, then
    /// inserts the provided rows. Unlike `record_code_index`, symbol and
    /// reference delete scopes are independent so a partial refresh can rewrite
    /// inbound references without disturbing an untouched file's symbols.
    pub(crate) async fn record_code_index_scoped(
        &self,
        symbol_paths: &HashSet<String>,
        reference_paths: &HashSet<String>,
        symbols: &[CodeSymbol],
        references: &[CodeReference],
    ) -> Result<(), String> {
        self.init().await?;
        let conn = self.connect().await?;
        let now = now_ms()?;

        for path in symbol_paths {
            conn.execute(
                "DELETE FROM code_symbols WHERE project_id = ?1 AND path = ?2",
                params![LOCAL_PROJECT_ID, path.clone()],
            )
            .await
            .map_err(|error| error.to_string())?;
        }
        for path in reference_paths {
            conn.execute(
                "DELETE FROM code_references WHERE project_id = ?1 AND path = ?2",
                params![LOCAL_PROJECT_ID, path.clone()],
            )
            .await
            .map_err(|error| error.to_string())?;
        }

        for symbol in symbols {
            insert_code_symbol(&conn, symbol, now).await?;
        }
        for reference in references {
            insert_code_reference(&conn, reference, now).await?;
        }
        refresh_test_mappings_for_paths(&conn, symbol_paths, now).await?;
        refresh_source_embeddings_for_paths(
            &conn,
            symbol_paths,
            symbols,
            now,
            self.embedding_provider()?,
        )
        .await?;

        Ok(())
    }

    pub async fn symbols_for_target(
        &self,
        target: &str,
        limit: usize,
    ) -> Result<Vec<CodeSymbol>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return symbols_for_target_via_hugr_api(&storage_config, target, limit);
        }

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
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return references_to_symbols_via_hugr_api(&storage_config, symbols, limit);
        }

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
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return references_from_symbols_via_hugr_api(&storage_config, symbols, limit);
        }

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
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return references_from_path_via_hugr_api(&storage_config, path, limit);
        }

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

    pub async fn context_graph_neighbors(
        &self,
        query: &str,
        files: &[String],
        symbols: &[CodeSymbol],
        limit: usize,
    ) -> Result<Vec<GraphNeighbor>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return context_graph_neighbors_via_hugr_api(
                &storage_config,
                query,
                files,
                symbols,
                limit,
            );
        }

        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut neighbors = Vec::new();
        let code_reference_limit = limit.saturating_mul(3).max(12);

        for reference in self
            .references_to_symbols(symbols, code_reference_limit)
            .await?
        {
            neighbors.push(graph_neighbor_from_code_reference(
                "incoming_reference",
                reference,
            ));
        }

        for reference in self
            .references_from_symbols(symbols, code_reference_limit)
            .await?
        {
            neighbors.push(graph_neighbor_from_code_reference(
                "outgoing_reference",
                reference,
            ));
        }

        for path in files.iter().take(8) {
            for reference in self.references_from_path(path, limit.max(4)).await? {
                neighbors.push(graph_neighbor_from_code_reference(
                    "path_reference",
                    reference,
                ));
            }
        }

        if self.exists() {
            let conn = self.connect().await?;
            migrations::migrate(&conn).await?;
            let sources = local_source_sync_records(&conn).await?;
            let entities = local_entity_sync_records(&conn).await?;
            let edges = local_edge_sync_records(&conn).await?;
            append_record_graph_neighbors(
                query,
                files,
                symbols,
                &sources,
                &entities,
                &edges,
                &mut neighbors,
                limit.saturating_mul(3).max(12),
            );
        }

        Ok(finalize_graph_neighbors(query, neighbors, limit))
    }

    pub async fn context_freshness_signals(
        &self,
        files: &[String],
        symbols: &[CodeSymbol],
        limit: usize,
    ) -> Result<Vec<FreshnessSignal>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return context_freshness_signals_via_hugr_api(&storage_config, files, symbols, limit);
        }

        if limit == 0 {
            return Ok(Vec::new());
        }

        let paths = freshness_paths(files, symbols);
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        if !self.exists() {
            return Ok(freshness_signals_from_index_timestamps(
                paths,
                HashMap::new(),
                limit,
            ));
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let timestamps = local_index_timestamps(&conn, &paths).await?;
        let latest_context_pack_updated_at_ms =
            local_latest_context_pack_updated_at_ms(&conn).await?;
        let edit_events =
            local_edit_freshness_events_after(&conn, latest_context_pack_updated_at_ms).await?;
        let mut signals = freshness_signals_from_index_timestamps(
            paths.clone(),
            timestamps,
            freshness_scan_limit(limit),
        );
        signals.extend(freshness_signals_from_edit_events(
            &paths,
            latest_context_pack_updated_at_ms,
            &edit_events,
            freshness_scan_limit(limit),
        ));
        signals.extend(freshness_signals_from_context_pack_age(
            &paths,
            latest_context_pack_updated_at_ms,
            freshness_scan_limit(limit),
        ));

        Ok(finalize_freshness_signals(signals, limit))
    }

    pub async fn record_diagnostics(
        &self,
        diagnostics: &[DiagnosticInput],
    ) -> Result<Vec<Diagnostic>, String> {
        if diagnostics.is_empty() {
            return Ok(Vec::new());
        }

        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return record_diagnostics_via_hugr_api(&storage_config, diagnostics);
        }

        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let records = diagnostic_records_from_inputs(diagnostics)?;
        insert_diagnostic_records(&conn, &records).await?;
        Ok(records
            .into_iter()
            .map(diagnostic_from_sync_record)
            .collect())
    }

    pub async fn recent_diagnostics(
        &self,
        query: &str,
        files: &[String],
        symbols: &[CodeSymbol],
        limit: usize,
    ) -> Result<Vec<Diagnostic>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return recent_diagnostics_via_hugr_api(&storage_config, query, files, symbols, limit);
        }

        if limit == 0 || !self.exists() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let records = local_diagnostic_sync_records(&conn).await?;
        Ok(rank_diagnostics(records, query, files, symbols, limit))
    }

    pub async fn likely_tests_for_files(
        &self,
        files: &[String],
        limit: usize,
    ) -> Result<Vec<TestCandidate>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return likely_tests_for_files_via_hugr_api(&storage_config, files, limit);
        }

        if limit == 0 || files.is_empty() || !self.exists() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut candidates = stored_test_candidates_for_files(&conn, files, limit).await?;
        let known_files = load_known_file_paths(&conn).await?;
        let inline_test_paths = load_inline_test_paths(&conn).await?;
        merge_test_candidates(
            &mut candidates,
            testmap::likely_tests_for_files(files, &known_files, &inline_test_paths, limit),
        );
        Ok(finalize_test_candidates(candidates, limit))
    }

    pub async fn source_embedding_file_candidates(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FileCandidate>, String> {
        if limit == 0 || query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return source_embedding_file_candidates_via_hugr_api(
                &storage_config,
                self.embedding_provider()?,
                query,
                limit,
            );
        }

        if !self.exists() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        local_source_embedding_file_candidates(&conn, self.embedding_provider()?, query, limit)
            .await
    }

    pub async fn recall_symbols(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<CodeSymbol>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return recall_symbols_via_hugr_api(&storage_config, query, limit);
        }

        if limit == 0 || !self.exists() {
            return Ok(Vec::new());
        }

        let terms = query_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let (sql, args) = symbol_recall_query(&terms);
        let mut rows = conn
            .query(&sql, args)
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
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return start_session_via_hugr_api(&storage_config, task);
        }

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
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return record_session_event_via_hugr_api(&storage_config, kind, detail);
        }

        self.init().await?;
        let conn = self.connect().await?;
        let session_id = active_session_id(&conn).await?;
        insert_session_event(&conn, session_id, kind, detail).await
    }

    pub(crate) async fn record_session_event_if_active(
        &self,
        kind: &str,
        detail: &str,
    ) -> Result<Option<SessionEvent>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return record_session_event_if_active_via_hugr_api(&storage_config, kind, detail);
        }

        if !self.exists() {
            return Ok(None);
        }
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let Some(session_id) = active_session_id_optional(&conn).await? else {
            return Ok(None);
        };
        insert_session_event(&conn, session_id, kind, detail)
            .await
            .map(Some)
    }

    pub async fn end_session(&self, summary: Option<&str>) -> Result<Session, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return end_session_via_hugr_api(&storage_config, summary);
        }

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

    pub async fn promote_latest_session(&self) -> Result<SessionPromotionResult, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return promote_latest_session_via_hugr_api(
                &storage_config,
                self.embedding_provider()?,
            );
        }

        if !self.exists() {
            return Err("no session available to promote".to_string());
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let session = latest_session(&conn)
            .await?
            .ok_or_else(|| "no session available to promote".to_string())?;
        self.promote_session(&conn, session).await
    }

    pub(crate) async fn promote_next_unpromoted_session(
        &self,
    ) -> Result<Option<SessionPromotionResult>, String> {
        if !self.exists() {
            return Ok(None);
        }

        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let Some(session) = next_unpromoted_ended_session(&conn).await? else {
            return Ok(None);
        };

        self.promote_session(&conn, session).await.map(Some)
    }

    async fn promote_session(
        &self,
        conn: &Connection,
        session: Session,
    ) -> Result<SessionPromotionResult, String> {
        let facts = session_promotion_facts(conn, &session).await?;
        if facts.is_empty() {
            return Err("latest session has no events or summary to promote".to_string());
        }

        if let Some(memory) = promoted_memory_for_session(conn, &session.id).await? {
            return Ok(SessionPromotionResult {
                session_id: session.id,
                task: session.task,
                fact_count: facts.len(),
                memory,
            });
        }

        let memory_text = session_promotion_text(&session, &facts);
        let project = project_from_conn(conn).await?;
        let structured_payload = session_promotion_payload(&session, &facts, project.as_ref());
        let memory = insert_memory(
            conn,
            self.embedding_provider()?,
            &memory_text,
            1.0,
            "normal".to_string(),
            Some(structured_payload),
        )
        .await?;
        insert_session_promotion(conn, &session.id, &memory.id).await?;
        Ok(SessionPromotionResult {
            session_id: session.id,
            task: session.task,
            fact_count: facts.len(),
            memory,
        })
    }

    pub async fn recent_session_facts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SessionFact>, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return recent_session_facts_via_hugr_api(&storage_config, query, limit);
        }

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

    pub(crate) async fn record_context_pack(
        &self,
        task: &str,
        payload_json: &str,
    ) -> Result<String, String> {
        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return record_context_pack_via_hugr_api(&storage_config, task, payload_json);
        }

        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        insert_context_pack(&conn, task, payload_json).await
    }

    pub async fn recall(&self, query: &str, limit: usize) -> Result<Vec<Memory>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let terms = query_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let storage_config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&storage_config) {
            return recall_via_hugr_api(&storage_config, query, &terms, limit);
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

    fn remember_via_hugr_api(
        &self,
        config: &StorageConfig,
        text: &str,
        options: MemoryWriteOptions,
    ) -> Result<Memory, String> {
        let (memory, payloads) =
            hugr_api_remember_payloads(self.embedding_provider()?, text, options)?;
        post_hugr_api_memory_payloads(config, "remember", &payloads)?;
        Ok(memory)
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
                SELECT
                    m.id,
                    m.created_at_ms,
                    m.kind,
                    m.text,
                    m.structured_payload,
                    bm25(memories_fts) AS fts_rank
                FROM memories_fts
                JOIN memories AS m ON m.rowid = memories_fts.rowid
                WHERE memories_fts MATCH ?1
                  AND m.valid_to IS NULL
                ORDER BY fts_rank, m.created_at_ms DESC
                LIMIT ?2
                ",
                params![search_query, candidate_limit],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut matches = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            let memory = memory_from_row(&row)?;
            let term_score = recall_score(&memory, terms, query);
            if term_score > 0 {
                matches.push(RankedMemory {
                    memory,
                    term_score,
                    fts_rank: Some(row.get::<f64>(5).map_err(|error| error.to_string())?),
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
                SELECT
                    m.id,
                    m.created_at_ms,
                    m.kind,
                    m.text,
                    m.structured_payload,
                    vector_matches.vector_rank
                FROM vector_matches
                JOIN memory_embeddings AS e ON e.rowid = vector_matches.id
                JOIN memories AS m ON m.id = e.memory_id
                WHERE m.valid_to IS NULL
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
                memory: memory_from_row(&row)?,
                term_score: 0,
                fts_rank: None,
                vector_rank: Some(row.get::<i64>(5).map_err(|error| error.to_string())?),
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

    /// Whether structural edits against the local working tree are available. Remote-only
    /// Hugr API mode has no working tree, so source-editing commands must refuse instead
    /// of silently doing nothing.
    pub fn supports_local_source_edits(&self) -> Result<bool, String> {
        Ok(!uses_remote_only_hugr_api_transport(self.storage_config()?))
    }

    pub fn sync_execution_plan(&self) -> Result<SyncExecutionPlan, String> {
        self.storage_config()
            .map(StorageConfig::sync_execution_plan)
    }

    pub async fn sync_history(&self, limit: usize) -> Result<Vec<SyncRunHistory>, String> {
        let config = self.storage_config()?.clone();
        if uses_hugr_api_transport(&config) {
            return fetch_hugr_api_history(&config, limit.max(1));
        }

        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let limit = i64::try_from(limit.max(1)).map_err(|error| error.to_string())?;
        let mut rows = conn
            .query(
                "
                SELECT id, operation, backend, status, started_at_ms, ended_at_ms
                FROM sync_runs
                ORDER BY started_at_ms DESC, id DESC
                LIMIT ?1
                ",
                params![limit],
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut history = Vec::new();

        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            let id = row.get::<String>(0).map_err(|error| error.to_string())?;
            let tables = sync_run_tables(&conn, &id).await?;
            history.push(SyncRunHistory {
                id,
                operation: row.get::<String>(1).map_err(|error| error.to_string())?,
                backend: row.get::<String>(2).map_err(|error| error.to_string())?,
                status: row.get::<String>(3).map_err(|error| error.to_string())?,
                started_at_ms: row.get::<i64>(4).map_err(|error| error.to_string())?,
                ended_at_ms: row.get::<i64>(5).map_err(|error| error.to_string())?,
                tables,
            });
        }

        Ok(history)
    }

    #[cfg(test)]
    pub(crate) async fn record_api_sync_run(
        &self,
        operation: &str,
        status: &str,
        tables: &[SyncTableResult],
    ) -> Result<String, String> {
        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let now = now_ms()?;
        self.record_sync_run(&conn, operation, "hugr_api", status, now, now, tables)
            .await
    }

    pub(crate) async fn apply_api_sync_push_payloads(
        &self,
        payloads: &[SyncApiTablePayload],
        dry_run: bool,
    ) -> Result<(Option<String>, String, Vec<SyncApiTablePayload>), String> {
        if dry_run {
            return Ok((None, "dry_run".to_string(), payloads.to_vec()));
        }

        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut applied_payloads = Vec::new();

        for payload in payloads {
            let result = match SyncTableKind::from_table_name(&payload.result.table) {
                Some(table)
                    if api_table_supports_records(table)
                        && payload.records.is_empty()
                        && payload.result.row_count > 0 =>
                {
                    missing_api_row_payload_result(&payload.result)
                }
                Some(SyncTableKind::Memories) => {
                    apply_api_push_memory_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::MemoryEmbeddings) => {
                    apply_api_push_memory_embedding_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Projects) => {
                    apply_api_push_project_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Sources) => {
                    apply_api_push_source_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::DiscoveredFiles) => {
                    apply_api_push_discovered_file_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Entities) => {
                    apply_api_push_entity_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::CodeSymbols) => {
                    apply_api_push_code_symbol_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Edges) => {
                    apply_api_push_edge_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::CodeReferences) => {
                    apply_api_push_code_reference_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::TestMappings) => {
                    apply_api_push_test_mapping_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::SourceEmbeddings) => {
                    apply_api_push_source_embedding_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Sessions) => {
                    apply_api_push_session_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::ContextPacks) => {
                    apply_api_push_context_pack_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Diagnostics) => {
                    apply_api_push_diagnostic_records(&conn, &payload.records).await?
                }
                None => unsupported_api_table_result(&payload.result),
            };
            applied_payloads.push(SyncApiTablePayload {
                result,
                records: Vec::new(),
            });
        }

        let status = api_sync_status_for_payloads(&applied_payloads);
        let now = now_ms()?;
        let tables = applied_payloads
            .iter()
            .map(|payload| payload.result.clone())
            .collect::<Vec<_>>();
        let run_id = self
            .record_sync_run(&conn, "push", "hugr_api", &status, now, now, &tables)
            .await?;

        Ok((Some(run_id), status, applied_payloads))
    }

    pub(crate) async fn api_sync_pull_payloads(
        &self,
        requested_payloads: &[SyncApiTablePayload],
        dry_run: bool,
    ) -> Result<(Option<String>, String, Vec<SyncApiTablePayload>), String> {
        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut response_payloads = Vec::new();

        for payload in requested_payloads {
            let response_payload = match SyncTableKind::from_table_name(&payload.result.table) {
                Some(table) if api_table_supports_records(table) => {
                    let result = planned_sync_table_result(
                        table,
                        table_row_count(&conn, table.table_name()).await?,
                    );
                    let records = if dry_run {
                        Vec::new()
                    } else {
                        api_sync_records_for_table(&conn, table).await?
                    };
                    SyncApiTablePayload { result, records }
                }
                Some(_) | None => SyncApiTablePayload {
                    result: unsupported_api_table_result(&payload.result),
                    records: Vec::new(),
                },
            };
            response_payloads.push(response_payload);
        }

        let status = if dry_run {
            "dry_run".to_string()
        } else {
            api_sync_status_for_payloads(&response_payloads)
        };
        let run_id = if dry_run {
            None
        } else {
            let now = now_ms()?;
            let tables = response_payloads
                .iter()
                .map(|payload| payload.result.clone())
                .collect::<Vec<_>>();
            Some(
                self.record_sync_run(&conn, "pull", "hugr_api", &status, now, now, &tables)
                    .await?,
            )
        };

        Ok((run_id, status, response_payloads))
    }

    pub(crate) async fn api_memory_records(&self) -> Result<Vec<serde_json::Value>, String> {
        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        memory_sync_records(&conn).await
    }

    pub(crate) async fn apply_api_memory_storage_payloads(
        &self,
        payloads: &[SyncApiTablePayload],
    ) -> Result<(String, Vec<SyncApiTablePayload>), String> {
        self.init().await?;
        let conn = self.connect().await?;
        migrations::migrate(&conn).await?;
        let mut applied_payloads = Vec::new();

        for payload in payloads {
            let result = match SyncTableKind::from_table_name(&payload.result.table) {
                Some(table)
                    if matches!(
                        table,
                        SyncTableKind::Projects
                            | SyncTableKind::Memories
                            | SyncTableKind::MemoryEmbeddings
                    ) && payload.records.is_empty()
                        && payload.result.row_count > 0 =>
                {
                    missing_api_row_payload_result(&payload.result)
                }
                Some(SyncTableKind::Projects) => {
                    apply_api_push_project_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Memories) => {
                    apply_api_push_memory_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::MemoryEmbeddings) => {
                    apply_api_push_memory_embedding_records(&conn, &payload.records).await?
                }
                Some(_) | None => unsupported_api_table_result(&payload.result),
            };
            applied_payloads.push(SyncApiTablePayload {
                result,
                records: Vec::new(),
            });
        }

        let status = api_sync_status_for_payloads(&applied_payloads);
        Ok((status, applied_payloads))
    }

    pub(crate) async fn api_storage_records(
        &self,
    ) -> Result<
        (
            Vec<SyncApiTablePayload>,
            Vec<serde_json::Value>,
            Vec<serde_json::Value>,
        ),
        String,
    > {
        let conn = self.connect_migrated_without_project_upsert().await?;
        let table_kinds = [
            SyncTableKind::Projects,
            SyncTableKind::Sources,
            SyncTableKind::DiscoveredFiles,
            SyncTableKind::Entities,
            SyncTableKind::CodeSymbols,
            SyncTableKind::Edges,
            SyncTableKind::CodeReferences,
            SyncTableKind::TestMappings,
            SyncTableKind::SourceEmbeddings,
            SyncTableKind::Sessions,
            SyncTableKind::ContextPacks,
            SyncTableKind::Diagnostics,
        ];
        let mut payloads = Vec::new();
        for table in table_kinds {
            let records = if table == SyncTableKind::Sessions {
                session_storage_records(&conn).await?
            } else {
                api_sync_records_for_table(&conn, table).await?
            };
            payloads.push(SyncApiTablePayload {
                result: planned_sync_table_result(
                    table,
                    table_row_count(&conn, table.table_name()).await?,
                ),
                records,
            });
        }
        Ok((
            payloads,
            session_event_sync_records(&conn).await?,
            session_promotion_sync_records(&conn).await?,
        ))
    }

    pub(crate) async fn apply_api_storage_payloads(
        &self,
        payloads: &[SyncApiTablePayload],
        session_events: &[serde_json::Value],
        session_promotions: &[serde_json::Value],
        replace_code_index_paths: &[String],
    ) -> Result<
        (
            String,
            Vec<SyncApiTablePayload>,
            SyncTableResult,
            SyncTableResult,
        ),
        String,
    > {
        let conn = self.connect_migrated_without_project_upsert().await?;
        if !replace_code_index_paths.is_empty() {
            delete_code_index_paths(&conn, replace_code_index_paths).await?;
        }

        let mut applied_payloads = Vec::new();
        for payload in payloads {
            let result = match SyncTableKind::from_table_name(&payload.result.table) {
                Some(table)
                    if api_storage_table_supports_records(table)
                        && payload.records.is_empty()
                        && payload.result.row_count > 0 =>
                {
                    missing_api_row_payload_result(&payload.result)
                }
                Some(SyncTableKind::Projects) => {
                    apply_api_push_project_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Sources) => {
                    apply_api_push_source_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::DiscoveredFiles) => {
                    apply_api_push_discovered_file_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Entities) => {
                    apply_api_push_entity_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::CodeSymbols) => {
                    apply_api_push_code_symbol_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Edges) => {
                    apply_api_push_edge_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::CodeReferences) => {
                    apply_api_push_code_reference_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::TestMappings) => {
                    apply_api_push_test_mapping_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::SourceEmbeddings) => {
                    apply_api_push_source_embedding_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Sessions) => {
                    apply_api_push_session_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::ContextPacks) => {
                    apply_api_push_context_pack_records(&conn, &payload.records).await?
                }
                Some(SyncTableKind::Diagnostics) => {
                    apply_api_push_diagnostic_records(&conn, &payload.records).await?
                }
                Some(_) | None => unsupported_api_table_result(&payload.result),
            };
            applied_payloads.push(SyncApiTablePayload {
                result,
                records: Vec::new(),
            });
        }
        let session_events_result =
            apply_api_storage_session_event_records(&conn, session_events).await?;
        let session_promotions_result =
            apply_api_storage_session_promotion_records(&conn, session_promotions).await?;
        let mut status_payloads = applied_payloads.clone();
        status_payloads.push(SyncApiTablePayload {
            result: session_events_result.clone(),
            records: Vec::new(),
        });
        status_payloads.push(SyncApiTablePayload {
            result: session_promotions_result.clone(),
            records: Vec::new(),
        });
        let status = api_sync_status_for_payloads(&status_payloads);
        Ok((
            status,
            applied_payloads,
            session_events_result,
            session_promotions_result,
        ))
    }

    pub async fn sync_push(&self, dry_run: bool) -> Result<SyncPushResult, String> {
        let config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&config) {
            return execute_hugr_api_push(&config, dry_run, &[]);
        }

        self.init().await?;
        let local_conn = self.connect().await?;
        migrations::migrate(&local_conn).await?;
        let mut tables = self.sync_table_results(&local_conn, &config, false).await?;
        let mut run_id = None;

        if !dry_run {
            if matches!(config.backend, SyncBackend::HugrApi) {
                let payloads = self
                    .sync_api_table_payloads(&local_conn, &tables, true)
                    .await?;
                return execute_hugr_api_push(&config, false, &payloads);
            } else {
                self.ensure_sync_push_execution_allowed(&config)?;
                let started_at_ms = now_ms()?;
                let remote_url = config
                    .remote_url
                    .as_ref()
                    .ok_or_else(|| "remote database URL is not configured".to_string())?;
                let remote_auth_token = config
                    .remote_auth_token
                    .as_ref()
                    .ok_or_else(|| "remote auth token is not configured".to_string())?;
                let remote_db = Builder::new_remote(remote_url.clone(), remote_auth_token.clone())
                    .build()
                    .await
                    .map_err(|error| error.to_string())?;
                let remote_conn = remote_db.connect().map_err(|error| error.to_string())?;
                migrations::migrate(&remote_conn).await?;
                tables = self
                    .copy_sync_tables(&local_conn, &remote_conn, &config)
                    .await?;
                let ended_at_ms = now_ms()?;
                run_id = Some(
                    self.record_sync_run(
                        &local_conn,
                        "push",
                        config.backend.as_str(),
                        "executed",
                        started_at_ms,
                        ended_at_ms,
                        &tables,
                    )
                    .await?,
                );
            }
        }

        Ok(SyncPushResult {
            run_id,
            dry_run,
            backend: config.backend.as_str().to_string(),
            status: if dry_run {
                "dry_run".to_string()
            } else {
                "executed".to_string()
            },
            tables,
        })
    }

    pub async fn sync_pull(&self, dry_run: bool) -> Result<SyncPullResult, String> {
        let config = self.storage_config()?.clone();
        if uses_remote_only_hugr_api_transport(&config) {
            return execute_hugr_api_pull(&config, dry_run, &[], None).await;
        }

        self.init().await?;
        let local_conn = self.connect().await?;
        migrations::migrate(&local_conn).await?;
        let mut tables = self.sync_table_results(&local_conn, &config, false).await?;
        let mut run_id = None;

        if !dry_run {
            if matches!(config.backend, SyncBackend::HugrApi) {
                let payloads = self
                    .sync_api_table_payloads(&local_conn, &tables, false)
                    .await?;
                return execute_hugr_api_pull(&config, false, &payloads, Some(&local_conn)).await;
            } else {
                self.ensure_sync_execute_allowed(&config, "pull")?;
                let started_at_ms = now_ms()?;
                let remote_url = config
                    .remote_url
                    .as_ref()
                    .ok_or_else(|| "remote database URL is not configured".to_string())?;
                let remote_auth_token = config
                    .remote_auth_token
                    .as_ref()
                    .ok_or_else(|| "remote auth token is not configured".to_string())?;
                let remote_db = Builder::new_remote(remote_url.clone(), remote_auth_token.clone())
                    .build()
                    .await
                    .map_err(|error| error.to_string())?;
                let remote_conn = remote_db.connect().map_err(|error| error.to_string())?;
                migrations::migrate(&remote_conn).await?;
                tables = self
                    .copy_pull_tables(&remote_conn, &local_conn, &config)
                    .await?;
                let ended_at_ms = now_ms()?;
                run_id = Some(
                    self.record_sync_run(
                        &local_conn,
                        "pull",
                        config.backend.as_str(),
                        "executed",
                        started_at_ms,
                        ended_at_ms,
                        &tables,
                    )
                    .await?,
                );
            }
        }

        Ok(SyncPullResult {
            run_id,
            dry_run,
            backend: config.backend.as_str().to_string(),
            status: if dry_run {
                "dry_run".to_string()
            } else {
                "executed".to_string()
            },
            tables,
        })
    }

    async fn connect(&self) -> Result<Connection, String> {
        let storage_config = self.storage_config()?;
        if matches!(storage_config.mode, StorageMode::Remote) {
            if matches!(storage_config.backend, SyncBackend::HugrApi) {
                return Err(
                    "HUGR_STORAGE_MODE=remote with HUGR_SYNC_BACKEND=hugr_api requires hosted Hugr API storage operations for local database commands; use `hugr sync status --json` to inspect the sync transport"
                        .to_string(),
                );
            }
            let remote_url = storage_config
                .remote_url
                .as_ref()
                .ok_or_else(|| "remote database URL is not configured".to_string())?;
            let remote_auth_token = storage_config
                .remote_auth_token
                .as_ref()
                .ok_or_else(|| "remote auth token is not configured".to_string())?;
            let db = Builder::new_remote(remote_url.clone(), remote_auth_token.clone())
                .build()
                .await
                .map_err(|error| error.to_string())?;
            let conn = db.connect().map_err(|error| error.to_string())?;
            conn.execute_batch("PRAGMA foreign_keys = ON;")
                .await
                .map_err(|error| error.to_string())?;
            return Ok(conn);
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

    /// True when this store routes all operations through a hosted Hugr API
    /// endpoint instead of a local database.
    pub fn is_remote_only(&self) -> Result<bool, String> {
        Ok(uses_remote_only_hugr_api_transport(self.storage_config()?))
    }

    fn ensure_sync_push_execution_allowed(&self, config: &StorageConfig) -> Result<(), String> {
        self.ensure_sync_execute_allowed(config, "push")
    }

    fn ensure_sync_execute_allowed(
        &self,
        config: &StorageConfig,
        operation: &str,
    ) -> Result<(), String> {
        if !matches!(config.mode, StorageMode::Hybrid) {
            return Err(format!(
                "hugr sync {operation} --execute requires HUGR_STORAGE_MODE=hybrid"
            ));
        }
        if !matches!(config.backend, SyncBackend::DirectLibsql) {
            return Err(format!(
                "hugr sync {operation} --execute only supports direct_libsql backend"
            ));
        }
        if config.remote_url.is_none() {
            return Err(format!(
                "hugr sync {operation} --execute requires HUGR_REMOTE_DATABASE_URL"
            ));
        }
        if config.remote_auth_token.is_none() {
            return Err(format!(
                "hugr sync {operation} --execute requires HUGR_REMOTE_AUTH_TOKEN"
            ));
        }
        Ok(())
    }

    async fn sync_table_results(
        &self,
        conn: &Connection,
        config: &StorageConfig,
        executed: bool,
    ) -> Result<Vec<SyncTableResult>, String> {
        let mut results = Vec::new();
        for table in sync_tables_for_config(config) {
            results.push(SyncTableResult {
                executed,
                ..planned_sync_table_result(table, table_row_count(conn, table.table_name()).await?)
            });
        }
        Ok(results)
    }

    async fn sync_api_table_payloads(
        &self,
        conn: &Connection,
        tables: &[SyncTableResult],
        include_records: bool,
    ) -> Result<Vec<SyncApiTablePayload>, String> {
        let mut payloads = Vec::new();

        for table in tables {
            let records = if include_records {
                match SyncTableKind::from_table_name(&table.table) {
                    Some(table) if api_table_supports_records(table) => {
                        api_sync_records_for_table(conn, table).await?
                    }
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            payloads.push(SyncApiTablePayload {
                result: table.clone(),
                records,
            });
        }

        Ok(payloads)
    }

    async fn copy_sync_tables(
        &self,
        local_conn: &Connection,
        remote_conn: &Connection,
        config: &StorageConfig,
    ) -> Result<Vec<SyncTableResult>, String> {
        let mut results = Vec::new();
        for table in sync_tables_for_config(config) {
            results.push(copy_sync_table(local_conn, remote_conn, table).await?);
        }
        Ok(results)
    }

    async fn copy_pull_tables(
        &self,
        remote_conn: &Connection,
        local_conn: &Connection,
        config: &StorageConfig,
    ) -> Result<Vec<SyncTableResult>, String> {
        let mut results = Vec::new();
        for table in sync_tables_for_config(config) {
            results.push(copy_pull_table(remote_conn, local_conn, table).await?);
        }
        Ok(results)
    }

    async fn record_sync_run(
        &self,
        conn: &Connection,
        operation: &str,
        backend: &str,
        status: &str,
        started_at_ms: i64,
        ended_at_ms: i64,
        tables: &[SyncTableResult],
    ) -> Result<String, String> {
        let id = format!("sync_{operation}_{ended_at_ms}");
        conn.execute(
            "
            INSERT INTO sync_runs (id, operation, backend, status, started_at_ms, ended_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                id.clone(),
                operation.to_string(),
                backend.to_string(),
                status.to_string(),
                started_at_ms,
                ended_at_ms
            ],
        )
        .await
        .map_err(|error| error.to_string())?;

        for table in tables {
            conn.execute(
                "
                INSERT INTO sync_table_runs (
                    sync_run_id, class, table_name, row_count, inserted_count,
                    updated_count, skipped_count, conflict_count, executed
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ",
                params![
                    id.clone(),
                    table.class.clone(),
                    table.table.clone(),
                    i64::try_from(table.row_count).map_err(|error| error.to_string())?,
                    i64::try_from(table.inserted_count).map_err(|error| error.to_string())?,
                    i64::try_from(table.updated_count).map_err(|error| error.to_string())?,
                    i64::try_from(table.skipped_count).map_err(|error| error.to_string())?,
                    i64::try_from(table.conflict_count).map_err(|error| error.to_string())?,
                    if table.executed { 1_i64 } else { 0_i64 }
                ],
            )
            .await
            .map_err(|error| error.to_string())?;

            for conflict in &table.conflicts {
                conn.execute(
                    "
                    INSERT INTO sync_table_conflicts (sync_run_id, table_name, reason, count)
                    VALUES (?1, ?2, ?3, ?4)
                    ",
                    params![
                        id.clone(),
                        table.table.clone(),
                        conflict.reason.clone(),
                        i64::try_from(conflict.count).map_err(|error| error.to_string())?
                    ],
                )
                .await
                .map_err(|error| error.to_string())?;
            }
        }

        Ok(id)
    }
}

impl SyncTableKind {
    fn from_table_name(table_name: &str) -> Option<Self> {
        match table_name {
            "projects" => Some(Self::Projects),
            "memories" => Some(Self::Memories),
            "memory_embeddings" => Some(Self::MemoryEmbeddings),
            "sources" => Some(Self::Sources),
            "discovered_files" => Some(Self::DiscoveredFiles),
            "entities" => Some(Self::Entities),
            "code_symbols" => Some(Self::CodeSymbols),
            "edges" => Some(Self::Edges),
            "code_references" => Some(Self::CodeReferences),
            "test_mappings" => Some(Self::TestMappings),
            "source_embeddings" => Some(Self::SourceEmbeddings),
            "sessions" => Some(Self::Sessions),
            "context_packs" => Some(Self::ContextPacks),
            "diagnostics" => Some(Self::Diagnostics),
            _ => None,
        }
    }

    fn table_name(self) -> &'static str {
        match self {
            Self::Projects => "projects",
            Self::Memories => "memories",
            Self::MemoryEmbeddings => "memory_embeddings",
            Self::Sources => "sources",
            Self::DiscoveredFiles => "discovered_files",
            Self::Entities => "entities",
            Self::CodeSymbols => "code_symbols",
            Self::Edges => "edges",
            Self::CodeReferences => "code_references",
            Self::TestMappings => "test_mappings",
            Self::SourceEmbeddings => "source_embeddings",
            Self::Sessions => "sessions",
            Self::ContextPacks => "context_packs",
            Self::Diagnostics => "diagnostics",
        }
    }

    fn sync_class(self) -> &'static str {
        match self {
            Self::Projects => "project_metadata",
            Self::Memories => "memories",
            Self::MemoryEmbeddings => "embeddings",
            Self::Sources | Self::DiscoveredFiles | Self::TestMappings => "sources",
            Self::SourceEmbeddings => "embeddings",
            Self::Entities | Self::CodeSymbols => "entities",
            Self::Edges | Self::CodeReferences => "edges",
            Self::Sessions => "session_summaries",
            Self::ContextPacks => "context_packs",
            Self::Diagnostics => "diagnostics",
        }
    }
}

fn sync_tables_for_config(config: &StorageConfig) -> Vec<SyncTableKind> {
    let mut tables = Vec::new();
    let mut seen = HashSet::new();

    push_sync_table(&mut tables, &mut seen, SyncTableKind::Projects);
    for class in &config.sync_classes {
        match class {
            SyncClass::Memories => push_sync_table(&mut tables, &mut seen, SyncTableKind::Memories),
            SyncClass::Sources => {
                push_sync_table(&mut tables, &mut seen, SyncTableKind::Sources);
                push_sync_table(&mut tables, &mut seen, SyncTableKind::DiscoveredFiles);
                push_sync_table(&mut tables, &mut seen, SyncTableKind::TestMappings);
            }
            SyncClass::Entities => {
                push_sync_table(&mut tables, &mut seen, SyncTableKind::Entities);
                push_sync_table(&mut tables, &mut seen, SyncTableKind::CodeSymbols);
            }
            SyncClass::Edges => {
                push_sync_table(&mut tables, &mut seen, SyncTableKind::Edges);
                push_sync_table(&mut tables, &mut seen, SyncTableKind::CodeReferences);
            }
            SyncClass::Embeddings => {
                push_sync_table(&mut tables, &mut seen, SyncTableKind::MemoryEmbeddings);
                push_sync_table(&mut tables, &mut seen, SyncTableKind::SourceEmbeddings);
            }
            SyncClass::SessionSummaries => {
                push_sync_table(&mut tables, &mut seen, SyncTableKind::Sessions)
            }
            SyncClass::ContextPacks => {
                push_sync_table(&mut tables, &mut seen, SyncTableKind::ContextPacks)
            }
            SyncClass::Diagnostics => {
                push_sync_table(&mut tables, &mut seen, SyncTableKind::Diagnostics)
            }
            SyncClass::FullSource
            | SyncClass::RawCommandOutput
            | SyncClass::Secrets
            | SyncClass::PrivateNotes => {}
        }
    }

    tables
}

fn push_sync_table(
    tables: &mut Vec<SyncTableKind>,
    seen: &mut HashSet<SyncTableKind>,
    table: SyncTableKind,
) {
    if seen.insert(table) {
        tables.push(table);
    }
}

fn uses_hugr_api_transport(config: &StorageConfig) -> bool {
    matches!(config.backend, SyncBackend::HugrApi)
        && matches!(config.mode, StorageMode::Hybrid | StorageMode::Remote)
}

fn uses_remote_only_hugr_api_transport(config: &StorageConfig) -> bool {
    matches!(config.backend, SyncBackend::HugrApi) && matches!(config.mode, StorageMode::Remote)
}

fn execute_hugr_api_push(
    config: &StorageConfig,
    dry_run: bool,
    payloads: &[SyncApiTablePayload],
) -> Result<SyncPushResult, String> {
    let response = post_hugr_api_sync(config, "push", dry_run, payloads)?;
    let parsed = parse_hugr_api_sync_response(&response)?;
    Ok(SyncPushResult {
        run_id: parsed.run_id,
        dry_run,
        backend: config.backend.as_str().to_string(),
        status: parsed.status,
        tables: parsed.tables,
    })
}

async fn execute_hugr_api_pull(
    config: &StorageConfig,
    dry_run: bool,
    payloads: &[SyncApiTablePayload],
    local_conn: Option<&Connection>,
) -> Result<SyncPullResult, String> {
    let response = post_hugr_api_sync(config, "pull", dry_run, payloads)?;
    let parsed = parse_hugr_api_sync_response(&response)?;
    let tables = if !dry_run {
        if let Some(local_conn) = local_conn {
            apply_api_pull_payloads(local_conn, &parsed.payloads).await?
        } else {
            parsed.tables
        }
    } else {
        parsed.tables
    };

    Ok(SyncPullResult {
        run_id: parsed.run_id,
        dry_run,
        backend: config.backend.as_str().to_string(),
        status: parsed.status,
        tables,
    })
}

fn hugr_api_remember_payloads(
    embedding_provider: &SelectedEmbeddingProvider,
    text: &str,
    options: MemoryWriteOptions,
) -> Result<(Memory, Vec<SyncApiTablePayload>), String> {
    let options = normalize_memory_write_options(options)?;
    let now = now_ms()?;
    let project = project_from_input(current_project_input()?, now);
    let structured_payload = memory_write_payload(&options, Some(&project));
    let memory = Memory {
        id: format!("mem_{now}"),
        created_at_ms: now,
        kind: "fact".to_string(),
        text: text.trim().to_string(),
        structured_payload: structured_payload.clone(),
    };
    let memory_record = MemorySyncRecord {
        id: memory.id.clone(),
        created_at_ms: memory.created_at_ms,
        kind: memory.kind.clone(),
        text: memory.text.clone(),
        confidence: options.confidence.unwrap_or(1.0),
        valid_from: None,
        valid_to: None,
        superseded_by: None,
        sensitivity: options
            .sensitivity
            .clone()
            .unwrap_or_else(|| "normal".to_string()),
        structured_payload,
    };
    let embedding = embedding_provider.embed(&memory.text)?;
    let embedding_record = MemoryEmbeddingSyncRecord {
        memory_id: memory.id.clone(),
        model: embedding.model.clone(),
        dimensions: embedding_dimensions_i64(&embedding)?,
        embedding: embedding.to_f32_blob(),
    };
    let payloads = vec![
        api_payload_for_records(
            SyncTableKind::Projects,
            vec![project_sync_record_value(&project)],
        ),
        api_payload_for_records(
            SyncTableKind::Memories,
            vec![memory_sync_record_value(&memory_record)],
        ),
        api_payload_for_records(
            SyncTableKind::MemoryEmbeddings,
            vec![memory_embedding_sync_record_value(&embedding_record)],
        ),
    ];

    Ok((memory, payloads))
}

fn hugr_api_session_promotion_payloads(
    embedding_provider: &SelectedEmbeddingProvider,
    session: &Session,
    facts: &[SessionFact],
    project: Option<&Project>,
) -> Result<(Memory, Vec<SyncApiTablePayload>, SessionPromotionSyncRecord), String> {
    let now = now_ms()?;
    let structured_payload = Some(session_promotion_payload(session, facts, project));
    let memory = Memory {
        id: format!("mem_{now}"),
        created_at_ms: now,
        kind: "fact".to_string(),
        text: session_promotion_text(session, facts),
        structured_payload: structured_payload.clone(),
    };
    let memory_record = MemorySyncRecord {
        id: memory.id.clone(),
        created_at_ms: memory.created_at_ms,
        kind: memory.kind.clone(),
        text: memory.text.clone(),
        confidence: 1.0,
        valid_from: None,
        valid_to: None,
        superseded_by: None,
        sensitivity: "normal".to_string(),
        structured_payload,
    };
    let embedding = embedding_provider.embed(&memory.text)?;
    let embedding_record = MemoryEmbeddingSyncRecord {
        memory_id: memory.id.clone(),
        model: embedding.model.clone(),
        dimensions: embedding_dimensions_i64(&embedding)?,
        embedding: embedding.to_f32_blob(),
    };
    let mut payloads = Vec::new();
    if let Some(project) = project {
        payloads.push(api_payload_for_records(
            SyncTableKind::Projects,
            vec![project_sync_record_value(project)],
        ));
    }
    payloads.push(api_payload_for_records(
        SyncTableKind::Memories,
        vec![memory_sync_record_value(&memory_record)],
    ));
    payloads.push(api_payload_for_records(
        SyncTableKind::MemoryEmbeddings,
        vec![memory_embedding_sync_record_value(&embedding_record)],
    ));

    let promotion_record = SessionPromotionSyncRecord {
        session_id: session.id.clone(),
        memory_id: memory.id.clone(),
        promoted_at_ms: now,
    };

    Ok((memory, payloads, promotion_record))
}

fn project_from_input(input: ProjectInput, now: i64) -> Project {
    Project {
        id: input.id,
        name: input.name,
        root_path: input.root_path,
        git_remote: input.git_remote,
        default_branch: input.default_branch,
        created_at_ms: now,
        updated_at_ms: now,
    }
}

fn embedding_dimensions_i64(embedding: &Embedding) -> Result<i64, String> {
    i64::try_from(embedding.dimensions()).map_err(|error| error.to_string())
}

fn api_payload_for_records(
    table: SyncTableKind,
    records: Vec<serde_json::Value>,
) -> SyncApiTablePayload {
    SyncApiTablePayload {
        result: planned_sync_table_result(table, records.len()),
        records,
    }
}

fn project_sync_record_value(project: &Project) -> serde_json::Value {
    json!({
        "id": &project.id,
        "name": &project.name,
        "root_path": &project.root_path,
        "git_remote": &project.git_remote,
        "default_branch": &project.default_branch,
        "created_at_ms": project.created_at_ms,
        "updated_at_ms": project.updated_at_ms
    })
}

fn memory_sync_record_value(record: &MemorySyncRecord) -> serde_json::Value {
    json!({
        "id": &record.id,
        "created_at_ms": record.created_at_ms,
        "kind": &record.kind,
        "text": &record.text,
        "confidence": record.confidence,
        "valid_from": &record.valid_from,
        "valid_to": &record.valid_to,
        "superseded_by": &record.superseded_by,
        "sensitivity": &record.sensitivity,
        "structured_payload": &record.structured_payload
    })
}

fn memory_embedding_sync_record_value(record: &MemoryEmbeddingSyncRecord) -> serde_json::Value {
    json!({
        "memory_id": &record.memory_id,
        "model": &record.model,
        "dimensions": record.dimensions,
        "embedding": &record.embedding
    })
}

fn discovered_file_sync_record_value(record: &DiscoveredFileSyncRecord) -> serde_json::Value {
    json!({
        "project_id": &record.project_id,
        "path": &record.path,
        "language": &record.language,
        "size_bytes": &record.size_bytes,
        "discovered_at_ms": record.discovered_at_ms,
        "updated_at_ms": record.updated_at_ms
    })
}

fn code_symbol_sync_record_value(record: &CodeSymbolSyncRecord) -> serde_json::Value {
    json!({
        "project_id": &record.project_id,
        "path": &record.path,
        "name": &record.name,
        "kind": &record.kind,
        "language": &record.language,
        "line_start": record.line_start,
        "line_end": &record.line_end,
        "signature": &record.signature,
        "indexed_at_ms": record.indexed_at_ms
    })
}

fn code_reference_sync_record_value(record: &CodeReferenceSyncRecord) -> serde_json::Value {
    json!({
        "project_id": &record.project_id,
        "path": &record.path,
        "target_path": &record.target_path,
        "target_name": &record.target_name,
        "target_kind": &record.target_kind,
        "kind": &record.kind,
        "language": &record.language,
        "line_start": record.line_start,
        "excerpt": &record.excerpt,
        "indexed_at_ms": record.indexed_at_ms
    })
}

fn test_mapping_sync_record_value(record: &TestMappingSyncRecord) -> serde_json::Value {
    json!({
        "project_id": &record.project_id,
        "source_path": &record.source_path,
        "test_path": &record.test_path,
        "reason": &record.reason,
        "score": record.score,
        "updated_at_ms": record.updated_at_ms
    })
}

fn source_embedding_sync_record_value(record: &SourceEmbeddingSyncRecord) -> serde_json::Value {
    json!({
        "project_id": &record.project_id,
        "path": &record.path,
        "model": &record.model,
        "dimensions": record.dimensions,
        "embedding": &record.embedding,
        "content_key": &record.content_key,
        "updated_at_ms": record.updated_at_ms
    })
}

fn session_sync_record_value(record: &SessionSyncRecord) -> serde_json::Value {
    json!({
        "id": &record.id,
        "project_id": &record.project_id,
        "task": &record.task,
        "branch": &record.branch,
        "started_at_ms": record.started_at_ms,
        "ended_at_ms": &record.ended_at_ms,
        "final_summary": &record.final_summary
    })
}

fn context_pack_sync_record_value(record: &ContextPackSyncRecord) -> serde_json::Value {
    json!({
        "id": &record.id,
        "project_id": &record.project_id,
        "task": &record.task,
        "payload_json": &record.payload_json,
        "created_at_ms": record.created_at_ms,
        "updated_at_ms": record.updated_at_ms
    })
}

fn diagnostic_sync_record_value(record: &DiagnosticSyncRecord) -> serde_json::Value {
    json!({
        "id": &record.id,
        "project_id": &record.project_id,
        "source": &record.source,
        "path": &record.path,
        "line_start": &record.line_start,
        "line_end": &record.line_end,
        "severity": &record.severity,
        "code": &record.code,
        "message": &record.message,
        "command": &record.command,
        "created_at_ms": record.created_at_ms
    })
}

fn session_event_sync_record_value(record: &SessionEventSyncRecord) -> serde_json::Value {
    json!({
        "id": &record.id,
        "session_id": &record.session_id,
        "kind": &record.kind,
        "detail": &record.detail,
        "created_at_ms": record.created_at_ms
    })
}

fn session_promotion_sync_record_value(record: &SessionPromotionSyncRecord) -> serde_json::Value {
    json!({
        "session_id": &record.session_id,
        "memory_id": &record.memory_id,
        "promoted_at_ms": record.promoted_at_ms
    })
}

fn sync_current_project_via_hugr_api(config: &StorageConfig) -> Result<Project, String> {
    let project = project_from_input(current_project_input()?, now_ms()?);
    post_hugr_api_storage_payloads(
        config,
        "project status",
        &[api_payload_for_records(
            SyncTableKind::Projects,
            vec![project_sync_record_value(&project)],
        )],
        &[],
        &[],
        &[],
    )?;
    Ok(project)
}

fn project_via_hugr_api(config: &StorageConfig) -> Result<Option<Project>, String> {
    let snapshot = fetch_hugr_api_storage_snapshot(config)?;
    Ok(storage_project_records(&snapshot)?
        .into_iter()
        .find(|project| project.id == LOCAL_PROJECT_ID))
}

fn record_context_pack_via_hugr_api(
    config: &StorageConfig,
    task: &str,
    payload_json: &str,
) -> Result<String, String> {
    let now = now_ms()?;
    let id = format!("ctx_{now}");
    let project = project_from_input(current_project_input()?, now);
    let record = ContextPackSyncRecord {
        id: id.clone(),
        project_id: LOCAL_PROJECT_ID.to_string(),
        task: task.trim().to_string(),
        payload_json: payload_json.to_string(),
        created_at_ms: now,
        updated_at_ms: now,
    };
    let payloads = vec![
        api_payload_for_records(
            SyncTableKind::Projects,
            vec![project_sync_record_value(&project)],
        ),
        api_payload_for_records(
            SyncTableKind::ContextPacks,
            vec![context_pack_sync_record_value(&record)],
        ),
    ];
    post_hugr_api_storage_payloads(config, "record context pack", &payloads, &[], &[], &[])?;
    Ok(id)
}

fn record_discovered_files_via_hugr_api(
    config: &StorageConfig,
    files: &[FileCandidate],
) -> Result<(), String> {
    let now = now_ms()?;
    let project = project_from_input(current_project_input()?, now);
    let records = files
        .iter()
        .map(|file| {
            let size_bytes = file
                .size_bytes
                .map(i64::try_from)
                .transpose()
                .map_err(|error| error.to_string())?;
            Ok(discovered_file_sync_record_value(
                &DiscoveredFileSyncRecord {
                    project_id: LOCAL_PROJECT_ID.to_string(),
                    path: file.path.clone(),
                    language: file.language.clone(),
                    size_bytes,
                    discovered_at_ms: now,
                    updated_at_ms: now,
                },
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let payloads = vec![
        api_payload_for_records(
            SyncTableKind::Projects,
            vec![project_sync_record_value(&project)],
        ),
        api_payload_for_records(SyncTableKind::DiscoveredFiles, records),
    ];
    post_hugr_api_storage_payloads(config, "record discovered files", &payloads, &[], &[], &[])?;
    Ok(())
}

fn record_code_index_via_hugr_api(
    config: &StorageConfig,
    embedding_provider: &SelectedEmbeddingProvider,
    files: &[FileCandidate],
    symbols: &[CodeSymbol],
    references: &[CodeReference],
) -> Result<(), String> {
    let now = now_ms()?;
    let symbol_records = symbols
        .iter()
        .map(|symbol| {
            code_symbol_sync_record_value(&CodeSymbolSyncRecord {
                project_id: LOCAL_PROJECT_ID.to_string(),
                path: symbol.path.clone(),
                name: symbol.name.clone(),
                kind: symbol.kind.clone(),
                language: symbol.language.clone(),
                line_start: symbol.line_start,
                line_end: symbol.line_end,
                signature: symbol.signature.clone(),
                indexed_at_ms: now,
            })
        })
        .collect::<Vec<_>>();
    let reference_records = references
        .iter()
        .map(|reference| {
            code_reference_sync_record_value(&CodeReferenceSyncRecord {
                project_id: LOCAL_PROJECT_ID.to_string(),
                path: reference.path.clone(),
                target_path: reference.target_path.clone(),
                target_name: reference.target_name.clone(),
                target_kind: reference.target_kind.clone(),
                kind: reference.kind.clone(),
                language: reference.language.clone(),
                line_start: reference.line_start,
                excerpt: reference.excerpt.clone(),
                indexed_at_ms: now,
            })
        })
        .collect::<Vec<_>>();
    let indexed_paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let inline_test_paths = testmap::inline_test_paths_from_symbols(symbols);
    let test_mapping_records =
        build_test_mapping_sync_records(&indexed_paths, &indexed_paths, &inline_test_paths, now)?;
    let source_embedding_records =
        build_source_embedding_sync_records(files, symbols, now, embedding_provider)?;
    let replace_paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let payloads = vec![
        api_payload_for_records(SyncTableKind::CodeSymbols, symbol_records),
        api_payload_for_records(SyncTableKind::CodeReferences, reference_records),
        api_payload_for_records(SyncTableKind::TestMappings, test_mapping_records),
        api_payload_for_records(SyncTableKind::SourceEmbeddings, source_embedding_records),
    ];
    post_hugr_api_storage_payloads(
        config,
        "record code index",
        &payloads,
        &[],
        &[],
        &replace_paths,
    )?;
    Ok(())
}

fn build_test_mapping_sync_records(
    source_paths: &[String],
    known_files: &[String],
    inline_test_paths: &HashSet<String>,
    now: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let mut records = Vec::new();
    let mut unique_sources = source_paths.to_vec();
    unique_sources.sort();
    unique_sources.dedup();

    for source_path in unique_sources {
        for candidate in testmap::likely_tests_for_files(
            std::slice::from_ref(&source_path),
            known_files,
            inline_test_paths,
            32,
        ) {
            records.push(test_mapping_sync_record_value(&TestMappingSyncRecord {
                project_id: LOCAL_PROJECT_ID.to_string(),
                source_path: source_path.clone(),
                test_path: candidate.path,
                reason: candidate.reason,
                score: i64::try_from(candidate.score).map_err(|error| error.to_string())?,
                updated_at_ms: now,
            }));
        }
    }

    Ok(records)
}

fn build_source_embedding_sync_records(
    files: &[FileCandidate],
    symbols: &[CodeSymbol],
    now: i64,
    embedding_provider: &SelectedEmbeddingProvider,
) -> Result<Vec<serde_json::Value>, String> {
    let mut records = Vec::new();
    for file in files {
        let size_bytes = file
            .size_bytes
            .map(i64::try_from)
            .transpose()
            .map_err(|error| error.to_string())?;
        let input = SourceEmbeddingInput {
            path: file.path.clone(),
            language: file.language.clone(),
            size_bytes,
        };
        let summary = source_embedding_summary(&input, symbols);
        let embedding = embedding_provider.embed(&summary)?;
        let dimensions =
            i64::try_from(embedding.dimensions()).map_err(|error| error.to_string())?;
        let embedding_blob = embedding.to_f32_blob();
        records.push(source_embedding_sync_record_value(
            &SourceEmbeddingSyncRecord {
                project_id: LOCAL_PROJECT_ID.to_string(),
                path: file.path.clone(),
                model: embedding.model,
                dimensions,
                embedding: embedding_blob,
                content_key: source_embedding_content_key(&summary),
                updated_at_ms: now,
            },
        ));
    }
    Ok(records)
}

fn symbols_for_target_via_hugr_api(
    config: &StorageConfig,
    target: &str,
    limit: usize,
) -> Result<Vec<CodeSymbol>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let target = normalize_target(target);
    if target.is_empty() {
        return Ok(Vec::new());
    }
    let mut symbols = storage_code_symbols(&fetch_hugr_api_storage_snapshot(config)?)?
        .into_iter()
        .filter(|symbol| symbol.path == target || symbol.name == target)
        .collect::<Vec<_>>();
    symbols.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line_start.cmp(&right.line_start))
    });
    if symbols.is_empty() {
        return recall_symbols_via_hugr_api(config, &target, limit);
    }
    symbols.truncate(limit);
    Ok(symbols)
}

fn references_to_symbols_via_hugr_api(
    config: &StorageConfig,
    symbols: &[CodeSymbol],
    limit: usize,
) -> Result<Vec<CodeReference>, String> {
    if limit == 0 || symbols.is_empty() {
        return Ok(Vec::new());
    }
    let targets = symbols
        .iter()
        .map(|symbol| (symbol.path.as_str(), symbol.name.as_str()))
        .collect::<HashSet<_>>();
    let mut references = storage_code_references(&fetch_hugr_api_storage_snapshot(config)?)?
        .into_iter()
        .filter(|reference| {
            targets.contains(&(
                reference.target_path.as_str(),
                reference.target_name.as_str(),
            ))
        })
        .collect::<Vec<_>>();
    references.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line_start.cmp(&right.line_start))
    });
    references.truncate(limit);
    Ok(references)
}

fn references_from_symbols_via_hugr_api(
    config: &StorageConfig,
    symbols: &[CodeSymbol],
    limit: usize,
) -> Result<Vec<CodeReference>, String> {
    if limit == 0 || symbols.is_empty() {
        return Ok(Vec::new());
    }
    let mut references = storage_code_references(&fetch_hugr_api_storage_snapshot(config)?)?
        .into_iter()
        .filter(|reference| {
            symbols
                .iter()
                .any(|symbol| reference_is_in_symbol(reference, symbol))
        })
        .collect::<Vec<_>>();
    references.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line_start.cmp(&right.line_start))
    });
    references.truncate(limit);
    Ok(references)
}

fn references_from_path_via_hugr_api(
    config: &StorageConfig,
    path: &str,
    limit: usize,
) -> Result<Vec<CodeReference>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let path = normalize_target(path);
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let mut references = storage_code_references(&fetch_hugr_api_storage_snapshot(config)?)?
        .into_iter()
        .filter(|reference| reference.path == path)
        .collect::<Vec<_>>();
    references.sort_by(|left, right| left.line_start.cmp(&right.line_start));
    references.truncate(limit);
    Ok(references)
}

fn context_graph_neighbors_via_hugr_api(
    config: &StorageConfig,
    query: &str,
    files: &[String],
    symbols: &[CodeSymbol],
    limit: usize,
) -> Result<Vec<GraphNeighbor>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let snapshot = fetch_hugr_api_storage_snapshot(config)?;
    let mut neighbors = Vec::new();
    let references = storage_code_references(&snapshot)?;
    let targets = symbols
        .iter()
        .map(|symbol| (symbol.path.as_str(), symbol.name.as_str()))
        .collect::<HashSet<_>>();
    let paths = files
        .iter()
        .map(|path| normalize_target(path))
        .filter(|path| !path.is_empty())
        .collect::<HashSet<_>>();

    for reference in &references {
        if targets.contains(&(
            reference.target_path.as_str(),
            reference.target_name.as_str(),
        )) {
            neighbors.push(graph_neighbor_from_code_reference(
                "incoming_reference",
                reference.clone(),
            ));
        }

        if symbols
            .iter()
            .any(|symbol| reference_is_in_symbol(reference, symbol))
        {
            neighbors.push(graph_neighbor_from_code_reference(
                "outgoing_reference",
                reference.clone(),
            ));
        }

        if paths.contains(reference.path.as_str()) {
            neighbors.push(graph_neighbor_from_code_reference(
                "path_reference",
                reference.clone(),
            ));
        }
    }

    append_record_graph_neighbors(
        query,
        files,
        symbols,
        &storage_source_records(&snapshot)?,
        &storage_entity_records(&snapshot)?,
        &storage_edge_records(&snapshot)?,
        &mut neighbors,
        limit.saturating_mul(3).max(12),
    );

    Ok(finalize_graph_neighbors(query, neighbors, limit))
}

async fn local_source_sync_records(conn: &Connection) -> Result<Vec<SourceSyncRecord>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, kind, locator, created_at_ms
            FROM sources
            ORDER BY created_at_ms DESC, id ASC
            LIMIT 1000
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(SourceSyncRecord {
            id: row.get::<String>(0).map_err(|error| error.to_string())?,
            kind: row.get::<String>(1).map_err(|error| error.to_string())?,
            locator: row.get::<String>(2).map_err(|error| error.to_string())?,
            created_at_ms: row.get::<i64>(3).map_err(|error| error.to_string())?,
        });
    }

    Ok(records)
}

async fn local_entity_sync_records(conn: &Connection) -> Result<Vec<EntitySyncRecord>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, kind, name, locator, created_at_ms
            FROM entities
            ORDER BY created_at_ms DESC, id ASC
            LIMIT 1000
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(EntitySyncRecord {
            id: row.get::<String>(0).map_err(|error| error.to_string())?,
            kind: row.get::<String>(1).map_err(|error| error.to_string())?,
            name: row.get::<String>(2).map_err(|error| error.to_string())?,
            locator: row
                .get::<Option<String>>(3)
                .map_err(|error| error.to_string())?,
            created_at_ms: row.get::<i64>(4).map_err(|error| error.to_string())?,
        });
    }

    Ok(records)
}

async fn local_edge_sync_records(conn: &Connection) -> Result<Vec<EdgeSyncRecord>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, from_id, to_id, kind, created_at_ms
            FROM edges
            ORDER BY created_at_ms DESC, id ASC
            LIMIT 1000
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(EdgeSyncRecord {
            id: row.get::<String>(0).map_err(|error| error.to_string())?,
            from_id: row.get::<String>(1).map_err(|error| error.to_string())?,
            to_id: row.get::<String>(2).map_err(|error| error.to_string())?,
            kind: row.get::<String>(3).map_err(|error| error.to_string())?,
            created_at_ms: row.get::<i64>(4).map_err(|error| error.to_string())?,
        });
    }

    Ok(records)
}

fn append_record_graph_neighbors(
    query: &str,
    files: &[String],
    symbols: &[CodeSymbol],
    sources: &[SourceSyncRecord],
    entities: &[EntitySyncRecord],
    edges: &[EdgeSyncRecord],
    neighbors: &mut Vec<GraphNeighbor>,
    limit: usize,
) {
    if limit == 0 {
        return;
    }

    let selector_terms = graph_selector_terms(query, files, symbols);
    if selector_terms.is_empty() {
        return;
    }

    let entity_labels = entities
        .iter()
        .map(|entity| {
            (
                entity.id.clone(),
                format!("{} {}", entity.kind, entity.name),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut matched_entity_ids = HashSet::new();
    let mut entity_matches = entities
        .iter()
        .filter_map(|entity| {
            let searchable = format!(
                "{} {} {} {}",
                entity.id,
                entity.kind,
                entity.name,
                entity.locator.as_deref().unwrap_or("")
            );
            let score = graph_text_match_score(&searchable, &selector_terms);
            (score > 0).then_some((score, entity))
        })
        .collect::<Vec<_>>();
    entity_matches.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.created_at_ms.cmp(&left.1.created_at_ms))
            .then_with(|| left.1.id.cmp(&right.1.id))
    });

    for (_, entity) in entity_matches.into_iter().take(limit) {
        matched_entity_ids.insert(entity.id.clone());
        neighbors.push(graph_neighbor_from_entity(entity));
    }

    let mut source_matches = sources
        .iter()
        .filter_map(|source| {
            let searchable = format!("{} {} {}", source.id, source.kind, source.locator);
            let score = graph_text_match_score(&searchable, &selector_terms);
            (score > 0).then_some((score, source))
        })
        .collect::<Vec<_>>();
    source_matches.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.created_at_ms.cmp(&left.1.created_at_ms))
            .then_with(|| left.1.id.cmp(&right.1.id))
    });

    for (_, source) in source_matches.into_iter().take(limit) {
        neighbors.push(graph_neighbor_from_source(source));
    }

    let mut edge_matches = edges
        .iter()
        .filter_map(|edge| {
            let adjacent = matched_entity_ids.contains(&edge.from_id)
                || matched_entity_ids.contains(&edge.to_id);
            let searchable = format!("{} {} {} {}", edge.id, edge.from_id, edge.kind, edge.to_id);
            let score =
                graph_text_match_score(&searchable, &selector_terms) + if adjacent { 1 } else { 0 };
            (score > 0).then_some((score, edge))
        })
        .collect::<Vec<_>>();
    edge_matches.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.created_at_ms.cmp(&left.1.created_at_ms))
            .then_with(|| left.1.id.cmp(&right.1.id))
    });

    for (_, edge) in edge_matches.into_iter().take(limit) {
        neighbors.push(graph_neighbor_from_edge(edge, &entity_labels));
    }
}

fn graph_neighbor_from_code_reference(kind: &str, reference: CodeReference) -> GraphNeighbor {
    let location = format!("{}:{}", reference.path, reference.line_start);
    // Name the defining file for cross-file targets so references to
    // same-named symbols in different files stay distinguishable.
    let target = if reference.target_path == reference.path {
        format!("{} {}", reference.target_kind, reference.target_name)
    } else {
        format!(
            "{} {} in {}",
            reference.target_kind, reference.target_name, reference.target_path
        )
    };
    let label = match kind {
        "incoming_reference" => format!("{location} references {target}"),
        "outgoing_reference" => format!("{location} reaches {target}"),
        _ => format!("{location} links to {target}"),
    };
    let detail = if reference.excerpt.trim().is_empty() {
        format!("{} reference to {target}", reference.kind)
    } else {
        format!(
            "{} reference to {target}: {}",
            reference.kind,
            reference.excerpt.trim()
        )
    };

    GraphNeighbor {
        kind: kind.to_string(),
        label,
        detail,
        path: Some(reference.path),
        target_path: Some(reference.target_path),
        target_name: Some(reference.target_name),
        line_start: Some(reference.line_start),
        site_count: 1,
    }
}

fn graph_neighbor_from_source(source: &SourceSyncRecord) -> GraphNeighbor {
    GraphNeighbor {
        kind: "source".to_string(),
        label: format!("{} source {}", source.kind, source.locator),
        detail: format!("source {} recorded at {}", source.id, source.created_at_ms),
        path: Some(source.locator.clone()),
        target_path: None,
        target_name: None,
        line_start: None,
        site_count: 1,
    }
}

fn graph_neighbor_from_entity(entity: &EntitySyncRecord) -> GraphNeighbor {
    GraphNeighbor {
        kind: "entity".to_string(),
        label: format!("{} {}", entity.kind, entity.name),
        detail: match &entity.locator {
            Some(locator) => format!("entity {} at {}", entity.id, locator),
            None => format!("entity {}", entity.id),
        },
        path: entity.locator.clone(),
        target_path: entity.locator.clone(),
        target_name: Some(entity.name.clone()),
        line_start: None,
        site_count: 1,
    }
}

fn graph_neighbor_from_edge(
    edge: &EdgeSyncRecord,
    entity_labels: &HashMap<String, String>,
) -> GraphNeighbor {
    let from_label = entity_labels
        .get(&edge.from_id)
        .cloned()
        .unwrap_or_else(|| edge.from_id.clone());
    let to_label = entity_labels
        .get(&edge.to_id)
        .cloned()
        .unwrap_or_else(|| edge.to_id.clone());

    GraphNeighbor {
        kind: "edge".to_string(),
        label: format!("{from_label} -{}-> {to_label}", edge.kind),
        detail: format!("edge {} recorded at {}", edge.id, edge.created_at_ms),
        path: None,
        target_path: None,
        target_name: Some(to_label),
        line_start: None,
        site_count: 1,
    }
}

fn graph_selector_terms(query: &str, files: &[String], symbols: &[CodeSymbol]) -> Vec<String> {
    let mut terms = query_terms(query);
    for file in files.iter().take(12) {
        let lower = file.to_lowercase();
        if lower.len() > 2 {
            terms.push(lower);
        }
        terms.extend(query_terms(file));
    }
    for symbol in symbols.iter().take(16) {
        let lower_name = symbol.name.to_lowercase();
        if lower_name.len() > 2 {
            terms.push(lower_name);
        }
        terms.extend(query_terms(&symbol.name));
        let lower_path = symbol.path.to_lowercase();
        if lower_path.len() > 2 {
            terms.push(lower_path);
        }
        terms.extend(query_terms(&symbol.path));
    }

    terms.sort();
    terms.dedup();
    terms
}

fn graph_text_match_score(value: &str, terms: &[String]) -> usize {
    if terms.is_empty() {
        return 0;
    }

    let lower = value.to_lowercase();
    terms
        .iter()
        .filter(|term| lower.contains(term.as_str()))
        .count()
}

fn finalize_graph_neighbors(
    query: &str,
    neighbors: Vec<GraphNeighbor>,
    limit: usize,
) -> Vec<GraphNeighbor> {
    let terms = query_terms(query);
    let mut seen = HashSet::new();
    let deduped = neighbors
        .into_iter()
        .filter(|neighbor| seen.insert(graph_neighbor_key(neighbor)))
        .collect::<Vec<_>>();
    let mut collapsed = collapse_reference_neighbor_sites(deduped);

    collapsed.sort_by(|left, right| {
        let left_score = graph_neighbor_sort_score(left, &terms);
        let right_score = graph_neighbor_sort_score(right, &terms);
        right_score
            .cmp(&left_score)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.label.cmp(&right.label))
    });
    collapsed.truncate(limit);
    collapsed
}

/// Folds repeated reference rows for one (kind, path, target) relationship
/// into a single neighbor carrying a site count, so a type referenced from
/// many lines of one file costs one context entry instead of many. The
/// earliest line is kept as the representative site.
fn collapse_reference_neighbor_sites(neighbors: Vec<GraphNeighbor>) -> Vec<GraphNeighbor> {
    let mut collapsed: Vec<GraphNeighbor> = Vec::new();
    let mut index_by_key = HashMap::<(String, String, String, String), usize>::new();

    for neighbor in neighbors {
        let collapsible = matches!(
            neighbor.kind.as_str(),
            "incoming_reference" | "outgoing_reference" | "path_reference"
        );
        if !collapsible {
            collapsed.push(neighbor);
            continue;
        }

        let key = (
            neighbor.kind.clone(),
            neighbor.path.clone().unwrap_or_default(),
            neighbor.target_path.clone().unwrap_or_default(),
            neighbor.target_name.clone().unwrap_or_default(),
        );
        match index_by_key.get(&key) {
            Some(&existing_index) => {
                let existing = &mut collapsed[existing_index];
                let site_count = existing.site_count + neighbor.site_count;
                let existing_line = existing.line_start.unwrap_or(i64::MAX);
                let incoming_line = neighbor.line_start.unwrap_or(i64::MAX);
                if incoming_line < existing_line {
                    *existing = neighbor;
                }
                existing.site_count = site_count;
            }
            None => {
                index_by_key.insert(key, collapsed.len());
                collapsed.push(neighbor);
            }
        }
    }

    collapsed
}

fn graph_neighbor_sort_score(neighbor: &GraphNeighbor, terms: &[String]) -> usize {
    let base = match neighbor.kind.as_str() {
        "incoming_reference" => 700,
        "outgoing_reference" => 680,
        "path_reference" => 640,
        "entity" => 560,
        "edge" => 540,
        "source" => 520,
        _ => 500,
    };
    let searchable = format!(
        "{} {} {} {} {}",
        neighbor.kind,
        neighbor.label,
        neighbor.detail,
        neighbor.path.as_deref().unwrap_or(""),
        neighbor.target_name.as_deref().unwrap_or("")
    );
    // Cross-file relationships tell an agent something it cannot see inside
    // the file it is about to open, so same-file references rank below them.
    let same_file_penalty = match (&neighbor.path, &neighbor.target_path) {
        (Some(path), Some(target_path)) if path == target_path => 120,
        _ => 0,
    };
    let site_bonus = neighbor.site_count.min(6) * 10;
    (base + graph_text_match_score(&searchable, terms) * 40 + site_bonus)
        .saturating_sub(same_file_penalty)
}

fn graph_neighbor_key(neighbor: &GraphNeighbor) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        neighbor.path.as_deref().unwrap_or(""),
        neighbor.target_path.as_deref().unwrap_or(""),
        neighbor.target_name.as_deref().unwrap_or(""),
        neighbor
            .line_start
            .map(|line| line.to_string())
            .unwrap_or_default(),
        neighbor.label
    )
}

fn context_freshness_signals_via_hugr_api(
    config: &StorageConfig,
    files: &[String],
    symbols: &[CodeSymbol],
    limit: usize,
) -> Result<Vec<FreshnessSignal>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let paths = freshness_paths(files, symbols);
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let snapshot = fetch_hugr_api_storage_snapshot(config)?;
    let timestamps = storage_index_timestamps(&snapshot, &paths)?;
    let latest_context_pack_updated_at_ms = storage_latest_context_pack_updated_at_ms(&snapshot)?;
    let edit_events =
        storage_edit_freshness_events_after(&snapshot, latest_context_pack_updated_at_ms);
    let mut signals = freshness_signals_from_index_timestamps(
        paths.clone(),
        timestamps,
        freshness_scan_limit(limit),
    );
    signals.extend(freshness_signals_from_edit_events(
        &paths,
        latest_context_pack_updated_at_ms,
        &edit_events,
        freshness_scan_limit(limit),
    ));
    signals.extend(freshness_signals_from_context_pack_age(
        &paths,
        latest_context_pack_updated_at_ms,
        freshness_scan_limit(limit),
    ));

    Ok(finalize_freshness_signals(signals, limit))
}

async fn local_index_timestamps(
    conn: &Connection,
    paths: &[String],
) -> Result<HashMap<String, i64>, String> {
    let path_set = paths.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut timestamps = HashMap::<String, i64>::new();

    let mut rows = conn
        .query(
            "
            SELECT path, updated_at_ms
            FROM discovered_files
            WHERE project_id = ?1
            ",
            params![LOCAL_PROJECT_ID],
        )
        .await
        .map_err(|error| error.to_string())?;
    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let path = row.get::<String>(0).map_err(|error| error.to_string())?;
        if path_set.contains(path.as_str()) {
            record_latest_timestamp(
                &mut timestamps,
                path,
                row.get::<i64>(1).map_err(|error| error.to_string())?,
            );
        }
    }

    let mut rows = conn
        .query(
            "
            SELECT path, indexed_at_ms
            FROM code_symbols
            WHERE project_id = ?1
            ",
            params![LOCAL_PROJECT_ID],
        )
        .await
        .map_err(|error| error.to_string())?;
    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let path = row.get::<String>(0).map_err(|error| error.to_string())?;
        if path_set.contains(path.as_str()) {
            record_latest_timestamp(
                &mut timestamps,
                path,
                row.get::<i64>(1).map_err(|error| error.to_string())?,
            );
        }
    }

    let mut rows = conn
        .query(
            "
            SELECT path, indexed_at_ms
            FROM code_references
            WHERE project_id = ?1
            ",
            params![LOCAL_PROJECT_ID],
        )
        .await
        .map_err(|error| error.to_string())?;
    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let path = row.get::<String>(0).map_err(|error| error.to_string())?;
        if path_set.contains(path.as_str()) {
            record_latest_timestamp(
                &mut timestamps,
                path,
                row.get::<i64>(1).map_err(|error| error.to_string())?,
            );
        }
    }

    Ok(timestamps)
}

async fn local_latest_context_pack_updated_at_ms(conn: &Connection) -> Result<Option<i64>, String> {
    let mut rows = conn
        .query(
            "
            SELECT updated_at_ms
            FROM context_packs
            WHERE project_id = ?1
            ORDER BY updated_at_ms DESC
            LIMIT 1
            ",
            params![LOCAL_PROJECT_ID],
        )
        .await
        .map_err(|error| error.to_string())?;

    rows.next()
        .await
        .map_err(|error| error.to_string())?
        .map(|row| row.get::<i64>(0).map_err(|error| error.to_string()))
        .transpose()
}

async fn local_edit_freshness_events_after(
    conn: &Connection,
    latest_context_pack_updated_at_ms: Option<i64>,
) -> Result<Vec<EditFreshnessEvent>, String> {
    let Some(latest_context_pack_updated_at_ms) = latest_context_pack_updated_at_ms else {
        return Ok(Vec::new());
    };

    let mut rows = conn
        .query(
            "
            SELECT e.detail, e.created_at_ms
            FROM session_events AS e
            JOIN sessions AS s ON s.id = e.session_id
            WHERE s.project_id = ?1
              AND e.kind = 'edit'
              AND e.created_at_ms > ?2
            ORDER BY e.created_at_ms DESC
            LIMIT 200
            ",
            params![LOCAL_PROJECT_ID, latest_context_pack_updated_at_ms],
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut events = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        events.push(EditFreshnessEvent {
            detail: row.get::<String>(0).map_err(|error| error.to_string())?,
            created_at_ms: row.get::<i64>(1).map_err(|error| error.to_string())?,
        });
    }

    Ok(events)
}

fn storage_index_timestamps(
    snapshot: &HugrApiStorageSnapshot,
    paths: &[String],
) -> Result<HashMap<String, i64>, String> {
    let path_set = paths.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut timestamps = HashMap::<String, i64>::new();

    for file in storage_discovered_files(snapshot)? {
        if path_set.contains(file.path.as_str()) {
            record_latest_timestamp(&mut timestamps, file.path, file.updated_at_ms);
        }
    }

    for value in storage_payload_records(snapshot, SyncTableKind::CodeSymbols) {
        let record = code_symbol_sync_record_from_value(&value)?;
        if path_set.contains(record.path.as_str()) {
            record_latest_timestamp(&mut timestamps, record.path, record.indexed_at_ms);
        }
    }

    for value in storage_payload_records(snapshot, SyncTableKind::CodeReferences) {
        let record = code_reference_sync_record_from_value(&value)?;
        if path_set.contains(record.path.as_str()) {
            record_latest_timestamp(&mut timestamps, record.path, record.indexed_at_ms);
        }
    }

    Ok(timestamps)
}

fn storage_latest_context_pack_updated_at_ms(
    snapshot: &HugrApiStorageSnapshot,
) -> Result<Option<i64>, String> {
    storage_payload_records(snapshot, SyncTableKind::ContextPacks)
        .into_iter()
        .map(|value| context_pack_sync_record_from_value(&value).map(|record| record.updated_at_ms))
        .try_fold(None, |latest, updated_at_ms| {
            updated_at_ms.map(|updated_at_ms| {
                Some(
                    latest
                        .map(|latest: i64| latest.max(updated_at_ms))
                        .unwrap_or(updated_at_ms),
                )
            })
        })
}

fn storage_edit_freshness_events_after(
    snapshot: &HugrApiStorageSnapshot,
    latest_context_pack_updated_at_ms: Option<i64>,
) -> Vec<EditFreshnessEvent> {
    let Some(latest_context_pack_updated_at_ms) = latest_context_pack_updated_at_ms else {
        return Vec::new();
    };

    let mut events = snapshot
        .session_events
        .iter()
        .filter(|event| event.kind == "edit")
        .filter(|event| event.created_at_ms > latest_context_pack_updated_at_ms)
        .map(|event| EditFreshnessEvent {
            detail: event.detail.clone(),
            created_at_ms: event.created_at_ms,
        })
        .collect::<Vec<_>>();
    events.sort_by(|left, right| right.created_at_ms.cmp(&left.created_at_ms));
    events.truncate(200);
    events
}

fn freshness_paths(files: &[String], symbols: &[CodeSymbol]) -> Vec<String> {
    let mut paths = files
        .iter()
        .chain(symbols.iter().map(|symbol| &symbol.path))
        .map(|path| normalize_target(path))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn freshness_scan_limit(limit: usize) -> usize {
    limit.saturating_mul(3).max(limit)
}

fn freshness_signals_from_index_timestamps(
    paths: Vec<String>,
    timestamps: HashMap<String, i64>,
    limit: usize,
) -> Vec<FreshnessSignal> {
    let mut signals = paths
        .into_iter()
        .filter_map(|path| {
            let indexed_at_ms = timestamps.get(&path).copied();
            let modified_at_ms = file_modified_at_ms(&path);
            match (indexed_at_ms, modified_at_ms) {
                (Some(indexed), Some(modified)) if modified > indexed + 1_000 => {
                    Some(FreshnessSignal {
                        detail: format!(
                            "{path} changed after its latest Hugr index timestamp ({modified} > {indexed})"
                        ),
                        path,
                        kind: "stale_index".to_string(),
                        indexed_at_ms: Some(indexed),
                        modified_at_ms: Some(modified),
                    })
                }
                (None, _) => Some(FreshnessSignal {
                    detail: format!("{path} has no matching Hugr index timestamp"),
                    path,
                    kind: "missing_index".to_string(),
                    indexed_at_ms: None,
                    modified_at_ms,
                }),
                _ => None,
            }
        })
        .collect::<Vec<_>>();

    signals.sort_by(|left, right| {
        freshness_kind_rank(&right.kind)
            .cmp(&freshness_kind_rank(&left.kind))
            .then_with(|| left.path.cmp(&right.path))
    });
    signals.truncate(limit);
    signals
}

/// Flags cited files whose on-disk modification time is newer than the latest
/// persisted context pack. Unlike `edit_after_context` (which only sees edits
/// recorded as Hugr session events) and `stale_index` (which compares to the
/// index timestamp), this catches files edited directly in an editor after a
/// pack was compiled, so agents are warned the persisted pack is out of date.
fn freshness_signals_from_context_pack_age(
    paths: &[String],
    latest_context_pack_updated_at_ms: Option<i64>,
    limit: usize,
) -> Vec<FreshnessSignal> {
    let Some(latest_context_pack_updated_at_ms) = latest_context_pack_updated_at_ms else {
        return Vec::new();
    };

    let mut signals = Vec::new();
    for path in paths {
        if signals.len() >= limit {
            break;
        }
        let Some(modified) = file_modified_at_ms(path) else {
            continue;
        };
        if modified > latest_context_pack_updated_at_ms + 1_000 {
            signals.push(FreshnessSignal {
                detail: format!(
                    "{path} changed on disk after the latest persisted context pack ({modified} > {latest_context_pack_updated_at_ms})"
                ),
                path: path.clone(),
                kind: "stale_context".to_string(),
                indexed_at_ms: Some(latest_context_pack_updated_at_ms),
                modified_at_ms: Some(modified),
            });
        }
    }
    signals
}

fn freshness_signals_from_edit_events(
    paths: &[String],
    latest_context_pack_updated_at_ms: Option<i64>,
    edit_events: &[EditFreshnessEvent],
    limit: usize,
) -> Vec<FreshnessSignal> {
    let Some(latest_context_pack_updated_at_ms) = latest_context_pack_updated_at_ms else {
        return Vec::new();
    };

    let mut seen_paths = HashSet::new();
    let mut signals = Vec::new();
    for event in edit_events {
        if event.created_at_ms <= latest_context_pack_updated_at_ms {
            continue;
        }

        for path in paths {
            if signals.len() >= limit {
                return signals;
            }
            if !seen_paths.contains(path) && edit_event_mentions_path(&event.detail, path) {
                seen_paths.insert(path.clone());
                signals.push(FreshnessSignal {
                    detail: format!(
                        "{path} has a Hugr edit event after the latest persisted context pack ({} > {}): {}",
                        event.created_at_ms,
                        latest_context_pack_updated_at_ms,
                        event.detail
                    ),
                    path: path.clone(),
                    kind: "edit_after_context".to_string(),
                    indexed_at_ms: Some(latest_context_pack_updated_at_ms),
                    modified_at_ms: Some(event.created_at_ms),
                });
            }
        }
    }

    signals
}

fn edit_event_mentions_path(detail: &str, path: &str) -> bool {
    detail.contains(path)
}

fn finalize_freshness_signals(
    mut signals: Vec<FreshnessSignal>,
    limit: usize,
) -> Vec<FreshnessSignal> {
    signals.sort_by(|left, right| {
        freshness_kind_rank(&right.kind)
            .cmp(&freshness_kind_rank(&left.kind))
            .then_with(|| right.modified_at_ms.cmp(&left.modified_at_ms))
            .then_with(|| left.path.cmp(&right.path))
    });
    signals.truncate(limit);
    signals
}

fn freshness_kind_rank(kind: &str) -> usize {
    match kind {
        "edit_after_context" => 4,
        "stale_context" => 3,
        "stale_index" => 2,
        "missing_index" => 1,
        _ => 0,
    }
}

fn record_latest_timestamp(timestamps: &mut HashMap<String, i64>, path: String, timestamp: i64) {
    timestamps
        .entry(path)
        .and_modify(|existing| *existing = (*existing).max(timestamp))
        .or_insert(timestamp);
}

fn file_modified_at_ms(path: &str) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let millis = modified.duration_since(UNIX_EPOCH).ok()?.as_millis();
    i64::try_from(millis).ok()
}

fn record_diagnostics_via_hugr_api(
    config: &StorageConfig,
    diagnostics: &[DiagnosticInput],
) -> Result<Vec<Diagnostic>, String> {
    let records = diagnostic_records_from_inputs(diagnostics)?;
    let now = now_ms()?;
    let project = project_from_input(current_project_input()?, now);
    let payloads = vec![
        api_payload_for_records(
            SyncTableKind::Projects,
            vec![project_sync_record_value(&project)],
        ),
        api_payload_for_records(
            SyncTableKind::Diagnostics,
            records.iter().map(diagnostic_sync_record_value).collect(),
        ),
    ];
    post_hugr_api_storage_payloads(config, "record diagnostics", &payloads, &[], &[], &[])?;
    Ok(records
        .into_iter()
        .map(diagnostic_from_sync_record)
        .collect())
}

fn recent_diagnostics_via_hugr_api(
    config: &StorageConfig,
    query: &str,
    files: &[String],
    symbols: &[CodeSymbol],
    limit: usize,
) -> Result<Vec<Diagnostic>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let records = storage_diagnostic_records(&fetch_hugr_api_storage_snapshot(config)?)?;
    Ok(rank_diagnostics(records, query, files, symbols, limit))
}

fn diagnostic_records_from_inputs(
    diagnostics: &[DiagnosticInput],
) -> Result<Vec<DiagnosticSyncRecord>, String> {
    let now = now_ms()?;
    diagnostics
        .iter()
        .filter_map(|diagnostic| normalize_diagnostic_input(diagnostic))
        .map(|diagnostic| {
            Ok(DiagnosticSyncRecord {
                id: diagnostic_id(now),
                project_id: LOCAL_PROJECT_ID.to_string(),
                source: diagnostic.source,
                path: diagnostic.path,
                line_start: diagnostic.line_start,
                line_end: diagnostic.line_end,
                severity: diagnostic.severity,
                code: diagnostic.code,
                message: diagnostic.message,
                command: diagnostic.command,
                created_at_ms: now,
            })
        })
        .collect()
}

fn normalize_diagnostic_input(input: &DiagnosticInput) -> Option<DiagnosticInput> {
    let message = input.message.trim();
    if message.is_empty() {
        return None;
    }

    Some(DiagnosticInput {
        source: input
            .source
            .trim()
            .to_lowercase()
            .replace(char::is_whitespace, "_"),
        path: input
            .path
            .as_deref()
            .map(normalize_target)
            .filter(|path| !path.is_empty()),
        line_start: input.line_start.filter(|line| *line > 0),
        line_end: input.line_end.filter(|line| *line > 0),
        severity: normalize_diagnostic_severity(&input.severity),
        code: input
            .code
            .as_deref()
            .map(str::trim)
            .filter(|code| !code.is_empty())
            .map(str::to_string),
        message: message.to_string(),
        command: input
            .command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .map(str::to_string),
    })
}

fn normalize_diagnostic_severity(severity: &str) -> String {
    match severity.trim().to_lowercase().as_str() {
        "error" | "warning" | "info" | "hint" => severity.trim().to_lowercase(),
        _ => "error".to_string(),
    }
}

async fn insert_diagnostic_records(
    conn: &Connection,
    records: &[DiagnosticSyncRecord],
) -> Result<(), String> {
    for record in records {
        conn.execute(
            "
            INSERT INTO diagnostics (
                id, project_id, source, path, line_start, line_end, severity, code,
                message, command, created_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(id) DO UPDATE SET
                project_id = excluded.project_id,
                source = excluded.source,
                path = excluded.path,
                line_start = excluded.line_start,
                line_end = excluded.line_end,
                severity = excluded.severity,
                code = excluded.code,
                message = excluded.message,
                command = excluded.command,
                created_at_ms = excluded.created_at_ms
            ",
            params![
                record.id.clone(),
                record.project_id.clone(),
                record.source.clone(),
                record.path.clone(),
                record.line_start,
                record.line_end,
                record.severity.clone(),
                record.code.clone(),
                record.message.clone(),
                record.command.clone(),
                record.created_at_ms
            ],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    Ok(())
}

async fn local_diagnostic_sync_records(
    conn: &Connection,
) -> Result<Vec<DiagnosticSyncRecord>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, project_id, source, path, line_start, line_end, severity,
                   code, message, command, created_at_ms
            FROM diagnostics
            WHERE project_id = ?1
            ORDER BY created_at_ms DESC, id ASC
            LIMIT 1000
            ",
            params![LOCAL_PROJECT_ID],
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(DiagnosticSyncRecord {
            id: row.get::<String>(0).map_err(|error| error.to_string())?,
            project_id: row.get::<String>(1).map_err(|error| error.to_string())?,
            source: row.get::<String>(2).map_err(|error| error.to_string())?,
            path: row
                .get::<Option<String>>(3)
                .map_err(|error| error.to_string())?,
            line_start: row
                .get::<Option<i64>>(4)
                .map_err(|error| error.to_string())?,
            line_end: row
                .get::<Option<i64>>(5)
                .map_err(|error| error.to_string())?,
            severity: row.get::<String>(6).map_err(|error| error.to_string())?,
            code: row
                .get::<Option<String>>(7)
                .map_err(|error| error.to_string())?,
            message: row.get::<String>(8).map_err(|error| error.to_string())?,
            command: row
                .get::<Option<String>>(9)
                .map_err(|error| error.to_string())?,
            created_at_ms: row.get::<i64>(10).map_err(|error| error.to_string())?,
        });
    }

    Ok(records)
}

fn storage_diagnostic_records(
    snapshot: &HugrApiStorageSnapshot,
) -> Result<Vec<DiagnosticSyncRecord>, String> {
    storage_payload_records(snapshot, SyncTableKind::Diagnostics)
        .iter()
        .map(diagnostic_sync_record_from_value)
        .collect()
}

fn rank_diagnostics(
    records: Vec<DiagnosticSyncRecord>,
    query: &str,
    files: &[String],
    symbols: &[CodeSymbol],
    limit: usize,
) -> Vec<Diagnostic> {
    if limit == 0 {
        return Vec::new();
    }

    let terms = query_terms(query);
    let paths = files
        .iter()
        .map(|path| normalize_target(path))
        .chain(symbols.iter().map(|symbol| normalize_target(&symbol.path)))
        .filter(|path| !path.is_empty())
        .collect::<HashSet<_>>();
    let mut matches = records
        .into_iter()
        .filter_map(|record| {
            let score = diagnostic_score(&record, &terms, &paths);
            (score > 0).then_some((score, record))
        })
        .collect::<Vec<_>>();

    matches.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.created_at_ms.cmp(&left.1.created_at_ms))
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    matches.truncate(limit);
    matches
        .into_iter()
        .map(|(_, record)| diagnostic_from_sync_record(record))
        .collect()
}

fn diagnostic_score(
    record: &DiagnosticSyncRecord,
    terms: &[String],
    paths: &HashSet<String>,
) -> usize {
    let mut relevance = 0;
    if let Some(path) = &record.path {
        if paths.contains(path) {
            relevance += 120;
        }
    }

    let searchable = format!(
        "{} {} {} {} {}",
        record.source,
        record.severity,
        record.code.as_deref().unwrap_or(""),
        record.message,
        record.command.as_deref().unwrap_or("")
    )
    .to_lowercase();
    relevance += terms
        .iter()
        .filter(|term| searchable.contains(term.as_str()))
        .count()
        * 20;
    if relevance == 0 {
        return 0;
    }

    let mut score = relevance;
    if matches!(record.severity.as_str(), "error") {
        score += 10;
    }
    score
}

fn diagnostic_from_sync_record(record: DiagnosticSyncRecord) -> Diagnostic {
    Diagnostic {
        id: record.id,
        source: record.source,
        path: record.path,
        line_start: record.line_start,
        line_end: record.line_end,
        severity: record.severity,
        code: record.code,
        message: record.message,
        command: record.command,
        created_at_ms: record.created_at_ms,
    }
}

fn diagnostic_id(created_at_ms: i64) -> String {
    let sequence = DIAGNOSTIC_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("diag_{created_at_ms}_{sequence}")
}

fn likely_tests_for_files_via_hugr_api(
    config: &StorageConfig,
    files: &[String],
    limit: usize,
) -> Result<Vec<TestCandidate>, String> {
    if limit == 0 || files.is_empty() {
        return Ok(Vec::new());
    }
    let snapshot = fetch_hugr_api_storage_snapshot(config)?;
    let mut candidates =
        test_candidates_from_mappings(&storage_test_mappings(&snapshot)?, files, limit);
    let known_files = storage_discovered_files(&snapshot)?
        .into_iter()
        .map(|record| record.path)
        .collect::<Vec<_>>();
    let inline_test_paths =
        testmap::inline_test_paths_from_symbols(&storage_code_symbols(&snapshot)?);
    merge_test_candidates(
        &mut candidates,
        testmap::likely_tests_for_files(files, &known_files, &inline_test_paths, limit),
    );
    Ok(finalize_test_candidates(candidates, limit))
}

fn source_embedding_file_candidates_via_hugr_api(
    config: &StorageConfig,
    embedding_provider: &SelectedEmbeddingProvider,
    query: &str,
    limit: usize,
) -> Result<Vec<FileCandidate>, String> {
    if limit == 0 || query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let snapshot = fetch_hugr_api_storage_snapshot(config)?;
    let embedding = embedding_provider.embed(query)?;
    let dimensions = i64::try_from(embedding.dimensions()).map_err(|error| error.to_string())?;
    let discovered_files = storage_discovered_files(&snapshot)?
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<HashMap<_, _>>();
    let mut ranked = Vec::<(f32, String, Option<String>, Option<u64>)>::new();

    for value in storage_payload_records(&snapshot, SyncTableKind::SourceEmbeddings) {
        let record = source_embedding_sync_record_from_value(&value)?;
        if record.model != embedding.model || record.dimensions != dimensions {
            continue;
        }
        let vector = f32_blob_to_vector(&record.embedding)?;
        if vector.len() != embedding.vector.len() {
            continue;
        }
        let score = cosine_like_score(&embedding.vector, &vector);
        if !score.is_finite() {
            continue;
        }
        let discovered = discovered_files.get(&record.path);
        let size_bytes = discovered
            .and_then(|file| file.size_bytes)
            .and_then(|bytes| u64::try_from(bytes).ok());
        ranked.push((
            score,
            record.path,
            discovered.and_then(|file| file.language.clone()),
            size_bytes,
        ));
    }

    ranked.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
    });
    ranked.truncate(limit);
    Ok(ranked
        .into_iter()
        .enumerate()
        .map(
            |(index, (_score, path, language, size_bytes))| FileCandidate {
                path,
                score: crate::discovery::source_embedding_score(index + 1),
                language,
                size_bytes,
            },
        )
        .collect())
}

fn recall_symbols_via_hugr_api(
    config: &StorageConfig,
    query: &str,
    limit: usize,
) -> Result<Vec<CodeSymbol>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let terms = query_terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut matches = storage_code_symbols(&fetch_hugr_api_storage_snapshot(config)?)?
        .into_iter()
        .filter_map(|symbol| {
            let score = code_symbol_score(&symbol, &terms, query);
            (score > 0).then_some((score, symbol))
        })
        .collect::<Vec<_>>();
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

fn start_session_via_hugr_api(config: &StorageConfig, task: &str) -> Result<Session, String> {
    let started_at_ms = now_ms()?;
    let project = project_from_input(current_project_input()?, started_at_ms);
    let session = Session {
        id: format!("ses_{started_at_ms}"),
        task: task.trim().to_string(),
        branch: current_branch().unwrap_or(None),
        started_at_ms,
        ended_at_ms: None,
        final_summary: None,
    };
    let record = session_record_from_session(&session);
    let payloads = vec![
        api_payload_for_records(
            SyncTableKind::Projects,
            vec![project_sync_record_value(&project)],
        ),
        api_payload_for_records(
            SyncTableKind::Sessions,
            vec![session_sync_record_value(&record)],
        ),
    ];
    post_hugr_api_storage_payloads(config, "start session", &payloads, &[], &[], &[])?;
    Ok(session)
}

fn record_session_event_via_hugr_api(
    config: &StorageConfig,
    kind: &str,
    detail: &str,
) -> Result<SessionEvent, String> {
    record_session_event_if_active_via_hugr_api(config, kind, detail)?
        .ok_or_else(|| "no active session; run `hugr session start <task>` first".to_string())
}

fn record_session_event_if_active_via_hugr_api(
    config: &StorageConfig,
    kind: &str,
    detail: &str,
) -> Result<Option<SessionEvent>, String> {
    let snapshot = fetch_hugr_api_storage_snapshot(config)?;
    let Some(session_id) = active_session_id_from_sessions(&storage_sessions(&snapshot)?) else {
        return Ok(None);
    };
    let created_at_ms = now_ms()?;
    let event = SessionEvent {
        id: session_event_id(created_at_ms),
        session_id,
        kind: kind.trim().to_string(),
        detail: detail.trim().to_string(),
        created_at_ms,
    };
    let record = SessionEventSyncRecord {
        id: event.id.clone(),
        session_id: event.session_id.clone(),
        kind: event.kind.clone(),
        detail: event.detail.clone(),
        created_at_ms: event.created_at_ms,
    };
    post_hugr_api_storage_payloads(
        config,
        "record session event",
        &[],
        &[session_event_sync_record_value(&record)],
        &[],
        &[],
    )?;
    Ok(Some(event))
}

fn end_session_via_hugr_api(
    config: &StorageConfig,
    summary: Option<&str>,
) -> Result<Session, String> {
    let snapshot = fetch_hugr_api_storage_snapshot(config)?;
    let mut session = storage_sessions(&snapshot)?
        .into_iter()
        .filter(|session| session.ended_at_ms.is_none())
        .max_by(|left, right| left.started_at_ms.cmp(&right.started_at_ms))
        .ok_or_else(|| "no active session; run `hugr session start <task>` first".to_string())?;
    session.ended_at_ms = Some(now_ms()?);
    session.final_summary = summary
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(str::to_string);
    let record = session_record_from_session(&session);
    post_hugr_api_storage_payloads(
        config,
        "end session",
        &[api_payload_for_records(
            SyncTableKind::Sessions,
            vec![session_sync_record_value(&record)],
        )],
        &[],
        &[],
        &[],
    )?;
    Ok(session)
}

fn promote_latest_session_via_hugr_api(
    config: &StorageConfig,
    embedding_provider: &SelectedEmbeddingProvider,
) -> Result<SessionPromotionResult, String> {
    let snapshot = fetch_hugr_api_storage_snapshot(config)?;
    let sessions = storage_sessions(&snapshot)?;
    let session = latest_session_from_sessions(&sessions)
        .ok_or_else(|| "no session available to promote".to_string())?;
    let facts = session_promotion_facts_from_records(&session, &snapshot.session_events);
    if facts.is_empty() {
        return Err("latest session has no events or summary to promote".to_string());
    }

    let memory_records = fetch_hugr_api_memory_records(config)?;
    if let Some(memory) = promoted_memory_for_session_records(
        &memory_records,
        &snapshot.session_promotions,
        &session.id,
    ) {
        return Ok(SessionPromotionResult {
            session_id: session.id,
            task: session.task,
            fact_count: facts.len(),
            memory,
        });
    }

    let project = project_for_storage_snapshot(&snapshot)?;
    let (memory, payloads, promotion_record) = hugr_api_session_promotion_payloads(
        embedding_provider,
        &session,
        &facts,
        project.as_ref(),
    )?;
    post_hugr_api_memory_payloads(config, "promote session", &payloads)?;
    post_hugr_api_storage_payloads(
        config,
        "record session promotion",
        &[],
        &[],
        &[session_promotion_sync_record_value(&promotion_record)],
        &[],
    )?;

    Ok(SessionPromotionResult {
        session_id: session.id,
        task: session.task,
        fact_count: facts.len(),
        memory,
    })
}

fn recent_session_facts_via_hugr_api(
    config: &StorageConfig,
    query: &str,
    limit: usize,
) -> Result<Vec<SessionFact>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let terms = query_terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let snapshot = fetch_hugr_api_storage_snapshot(config)?;
    let mut facts = Vec::new();
    for session in storage_sessions(&snapshot)? {
        facts.push(SessionFact {
            session_id: session.id.clone(),
            kind: "task".to_string(),
            detail: session.task.clone(),
            created_at_ms: session.started_at_ms,
        });
        if let Some(summary) = session.final_summary {
            facts.push(SessionFact {
                session_id: session.id.clone(),
                kind: "summary".to_string(),
                detail: summary,
                created_at_ms: session.ended_at_ms.unwrap_or(session.started_at_ms),
            });
        }
    }
    for event in &snapshot.session_events {
        facts.push(SessionFact {
            session_id: event.session_id.clone(),
            kind: event.kind.clone(),
            detail: event.detail.clone(),
            created_at_ms: event.created_at_ms,
        });
    }
    facts.retain(|fact| session_fact_score(fact, &terms) > 0);
    facts.sort_by(|left, right| {
        session_fact_score(right, &terms)
            .cmp(&session_fact_score(left, &terms))
            .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
    });
    facts.truncate(limit);
    Ok(facts)
}

fn session_record_from_session(session: &Session) -> SessionSyncRecord {
    SessionSyncRecord {
        id: session.id.clone(),
        project_id: LOCAL_PROJECT_ID.to_string(),
        task: session.task.clone(),
        branch: session.branch.clone(),
        started_at_ms: session.started_at_ms,
        ended_at_ms: session.ended_at_ms,
        final_summary: session.final_summary.clone(),
    }
}

fn session_from_sync_record(record: &SessionSyncRecord) -> Session {
    Session {
        id: record.id.clone(),
        task: record.task.clone(),
        branch: record.branch.clone(),
        started_at_ms: record.started_at_ms,
        ended_at_ms: record.ended_at_ms,
        final_summary: record.final_summary.clone(),
    }
}

fn active_session_id_from_sessions(sessions: &[Session]) -> Option<String> {
    sessions
        .iter()
        .filter(|session| session.ended_at_ms.is_none())
        .max_by(|left, right| left.started_at_ms.cmp(&right.started_at_ms))
        .map(|session| session.id.clone())
}

#[derive(Debug, Clone, PartialEq)]
struct HugrApiStorageSnapshot {
    payloads: Vec<SyncApiTablePayload>,
    session_events: Vec<SessionEventSyncRecord>,
    session_promotions: Vec<SessionPromotionSyncRecord>,
}

#[derive(Debug, Clone, PartialEq)]
struct HugrApiStorageApplyResponse {
    status: String,
    tables: Vec<SyncTableResult>,
    session_events_table: SyncTableResult,
    session_promotions_table: SyncTableResult,
}

fn fetch_hugr_api_storage_snapshot(
    config: &StorageConfig,
) -> Result<HugrApiStorageSnapshot, String> {
    let response = get_hugr_api_json(config, "/v1/storage")?;
    parse_hugr_api_storage_snapshot_response(&response)
}

fn post_hugr_api_storage_payloads(
    config: &StorageConfig,
    operation: &str,
    payloads: &[SyncApiTablePayload],
    session_events: &[serde_json::Value],
    session_promotions: &[serde_json::Value],
    replace_code_index_paths: &[String],
) -> Result<HugrApiStorageApplyResponse, String> {
    if payloads.iter().all(|payload| payload.records.is_empty())
        && session_events.is_empty()
        && session_promotions.is_empty()
        && replace_code_index_paths.is_empty()
    {
        return Ok(HugrApiStorageApplyResponse {
            status: "accepted".to_string(),
            tables: Vec::new(),
            session_events_table: planned_storage_table_result(
                "session_summaries",
                "session_events",
                0,
                false,
            ),
            session_promotions_table: planned_storage_table_result(
                "session_summaries",
                "session_promotions",
                0,
                false,
            ),
        });
    }

    let body = hugr_api_storage_apply_request(
        payloads,
        session_events,
        session_promotions,
        replace_code_index_paths,
    );
    let response = post_hugr_api_json(config, "/v1/storage", &body)?;
    let parsed = parse_hugr_api_storage_apply_response(&response)?;
    let mut tables = parsed.tables.clone();
    tables.push(parsed.session_events_table.clone());
    tables.push(parsed.session_promotions_table.clone());
    ensure_hugr_api_memory_operation_accepted(operation, &parsed.status, &tables)?;
    Ok(parsed)
}

fn hugr_api_storage_apply_request(
    payloads: &[SyncApiTablePayload],
    session_events: &[serde_json::Value],
    session_promotions: &[serde_json::Value],
    replace_code_index_paths: &[String],
) -> serde_json::Value {
    json!({
        "contract_version": HUGR_API_CONTRACT_VERSION,
        "tables": payloads.iter().map(sync_api_table_payload_value).collect::<Vec<_>>(),
        "session_events": session_events,
        "session_promotions": session_promotions,
        "replace_code_index_paths": replace_code_index_paths
    })
}

fn parse_hugr_api_storage_snapshot_response(
    response: &str,
) -> Result<HugrApiStorageSnapshot, String> {
    let value = serde_json::from_str::<serde_json::Value>(response)
        .map_err(|error| format!("invalid Hugr API storage response: {error}"))?;
    reject_hugr_api_error(&value)?;
    let contract_version = json_string_field(&value, "contract_version")?;
    if contract_version != HUGR_API_CONTRACT_VERSION {
        return Err(format!(
            "unsupported Hugr API contract version '{contract_version}'"
        ));
    }
    Ok(HugrApiStorageSnapshot {
        payloads: json_array_field(&value, "tables")?
            .iter()
            .map(parse_sync_api_table_payload)
            .collect::<Result<Vec<_>, _>>()?,
        session_events: json_array_field(&value, "session_events")?
            .iter()
            .map(session_event_sync_record_from_value)
            .collect::<Result<Vec<_>, _>>()?,
        session_promotions: optional_json_array_field(&value, "session_promotions")
            .iter()
            .map(session_promotion_sync_record_from_value)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn parse_hugr_api_storage_apply_response(
    response: &str,
) -> Result<HugrApiStorageApplyResponse, String> {
    let value = serde_json::from_str::<serde_json::Value>(response)
        .map_err(|error| format!("invalid Hugr API storage response: {error}"))?;
    reject_hugr_api_error(&value)?;
    let contract_version = json_string_field(&value, "contract_version")?;
    if contract_version != HUGR_API_CONTRACT_VERSION {
        return Err(format!(
            "unsupported Hugr API contract version '{contract_version}'"
        ));
    }
    Ok(HugrApiStorageApplyResponse {
        status: json_string_field(&value, "status")?,
        tables: json_array_field(&value, "tables")?
            .iter()
            .map(parse_sync_table_result)
            .collect::<Result<Vec<_>, _>>()?,
        session_events_table: parse_sync_table_result(
            value.get("session_events_table").ok_or_else(|| {
                "Hugr API response missing field 'session_events_table'".to_string()
            })?,
        )?,
        session_promotions_table: value
            .get("session_promotions_table")
            .map(parse_sync_table_result)
            .transpose()?
            .unwrap_or_else(|| {
                planned_storage_table_result("session_summaries", "session_promotions", 0, false)
            }),
    })
}

fn storage_payload_records(
    snapshot: &HugrApiStorageSnapshot,
    table: SyncTableKind,
) -> Vec<serde_json::Value> {
    snapshot
        .payloads
        .iter()
        .find(|payload| payload.result.table == table.table_name())
        .map(|payload| payload.records.clone())
        .unwrap_or_default()
}

fn storage_project_records(snapshot: &HugrApiStorageSnapshot) -> Result<Vec<Project>, String> {
    storage_payload_records(snapshot, SyncTableKind::Projects)
        .iter()
        .map(|value| {
            let record = project_sync_record_from_value(value)?;
            Ok(Project {
                id: record.id,
                name: record.name,
                root_path: record.root_path,
                git_remote: record.git_remote,
                default_branch: record.default_branch,
                created_at_ms: record.created_at_ms,
                updated_at_ms: record.updated_at_ms,
            })
        })
        .collect()
}

fn storage_discovered_files(
    snapshot: &HugrApiStorageSnapshot,
) -> Result<Vec<DiscoveredFileSyncRecord>, String> {
    storage_payload_records(snapshot, SyncTableKind::DiscoveredFiles)
        .iter()
        .map(discovered_file_sync_record_from_value)
        .collect()
}

fn storage_test_mappings(
    snapshot: &HugrApiStorageSnapshot,
) -> Result<Vec<TestMappingSyncRecord>, String> {
    storage_payload_records(snapshot, SyncTableKind::TestMappings)
        .iter()
        .map(test_mapping_sync_record_from_value)
        .collect()
}

fn storage_code_symbols(snapshot: &HugrApiStorageSnapshot) -> Result<Vec<CodeSymbol>, String> {
    storage_payload_records(snapshot, SyncTableKind::CodeSymbols)
        .iter()
        .map(|value| {
            let record = code_symbol_sync_record_from_value(value)?;
            Ok(CodeSymbol {
                path: record.path,
                language: record.language,
                name: record.name,
                kind: record.kind,
                line_start: record.line_start,
                line_end: record.line_end,
                signature: record.signature,
            })
        })
        .collect()
}

fn storage_code_references(
    snapshot: &HugrApiStorageSnapshot,
) -> Result<Vec<CodeReference>, String> {
    storage_payload_records(snapshot, SyncTableKind::CodeReferences)
        .iter()
        .map(|value| {
            let record = code_reference_sync_record_from_value(value)?;
            Ok(CodeReference {
                path: record.path,
                language: record.language,
                target_path: record.target_path,
                target_name: record.target_name,
                target_kind: record.target_kind,
                kind: record.kind,
                line_start: record.line_start,
                excerpt: record.excerpt,
            })
        })
        .collect()
}

fn storage_source_records(
    snapshot: &HugrApiStorageSnapshot,
) -> Result<Vec<SourceSyncRecord>, String> {
    storage_payload_records(snapshot, SyncTableKind::Sources)
        .iter()
        .map(source_sync_record_from_value)
        .collect()
}

fn storage_entity_records(
    snapshot: &HugrApiStorageSnapshot,
) -> Result<Vec<EntitySyncRecord>, String> {
    storage_payload_records(snapshot, SyncTableKind::Entities)
        .iter()
        .map(entity_sync_record_from_value)
        .collect()
}

fn storage_edge_records(snapshot: &HugrApiStorageSnapshot) -> Result<Vec<EdgeSyncRecord>, String> {
    storage_payload_records(snapshot, SyncTableKind::Edges)
        .iter()
        .map(edge_sync_record_from_value)
        .collect()
}

fn storage_sessions(snapshot: &HugrApiStorageSnapshot) -> Result<Vec<Session>, String> {
    storage_payload_records(snapshot, SyncTableKind::Sessions)
        .iter()
        .map(|value| {
            session_sync_record_from_value(value).map(|record| session_from_sync_record(&record))
        })
        .collect()
}

fn latest_session_from_sessions(sessions: &[Session]) -> Option<Session> {
    sessions
        .iter()
        .max_by(|left, right| {
            left.ended_at_ms
                .unwrap_or(left.started_at_ms)
                .cmp(&right.ended_at_ms.unwrap_or(right.started_at_ms))
                .then_with(|| left.started_at_ms.cmp(&right.started_at_ms))
        })
        .cloned()
}

fn session_promotion_facts_from_records(
    session: &Session,
    events: &[SessionEventSyncRecord],
) -> Vec<SessionFact> {
    let mut facts = events
        .iter()
        .filter(|event| event.session_id == session.id)
        .map(|event| SessionFact {
            session_id: event.session_id.clone(),
            kind: event.kind.clone(),
            detail: event.detail.clone(),
            created_at_ms: event.created_at_ms,
        })
        .collect::<Vec<_>>();
    facts.sort_by(|left, right| left.created_at_ms.cmp(&right.created_at_ms));
    facts.truncate(12);

    if let Some(summary) = session
        .final_summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        facts.push(SessionFact {
            session_id: session.id.clone(),
            kind: "summary".to_string(),
            detail: summary.to_string(),
            created_at_ms: session.ended_at_ms.unwrap_or(session.started_at_ms),
        });
    }

    facts
}

fn project_for_storage_snapshot(
    snapshot: &HugrApiStorageSnapshot,
) -> Result<Option<Project>, String> {
    let projects = storage_project_records(snapshot)?;
    if let Some(project) = projects
        .iter()
        .find(|project| project.id == LOCAL_PROJECT_ID)
        .cloned()
    {
        return Ok(Some(project));
    }
    if let Some(project) = projects
        .iter()
        .max_by(|left, right| left.updated_at_ms.cmp(&right.updated_at_ms))
        .cloned()
    {
        return Ok(Some(project));
    }

    Ok(Some(project_from_input(
        current_project_input()?,
        now_ms()?,
    )))
}

fn planned_storage_table_result(
    class: &str,
    table: &str,
    row_count: usize,
    executed: bool,
) -> SyncTableResult {
    SyncTableResult {
        class: class.to_string(),
        table: table.to_string(),
        row_count,
        inserted_count: 0,
        updated_count: 0,
        skipped_count: 0,
        conflict_count: 0,
        executed,
        conflicts: Vec::new(),
    }
}

fn hugr_api_active_memories(config: &StorageConfig) -> Result<Vec<Memory>, String> {
    Ok(fetch_hugr_api_memory_records(config)?
        .iter()
        .filter(|record| record.valid_to.is_none())
        .map(memory_from_sync_record)
        .collect())
}

fn recall_via_hugr_api(
    config: &StorageConfig,
    query: &str,
    terms: &[String],
    limit: usize,
) -> Result<Vec<Memory>, String> {
    let memories = hugr_api_active_memories(config)?;
    Ok(rank_memories_by_query(memories, terms, query, limit))
}

fn forget_via_hugr_api(
    config: &StorageConfig,
    query: &str,
    terms: &[String],
    limit: usize,
) -> Result<ForgetResult, String> {
    let records = fetch_hugr_api_memory_records(config)?;
    let mut matches = records
        .into_iter()
        .filter(|record| record.valid_to.is_none())
        .filter_map(|record| {
            let memory = memory_from_sync_record(&record);
            let score = recall_score(&memory, terms, query);
            (score > 0).then_some((score, record, memory))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.2.created_at_ms.cmp(&left.2.created_at_ms))
    });
    matches.truncate(limit.max(1));

    let forgotten_at = now_ms()?.to_string();
    let memories = matches
        .iter()
        .map(|(_, _, memory)| memory.clone())
        .collect::<Vec<_>>();
    let updates = matches
        .into_iter()
        .map(|(_, mut record, _)| {
            record.valid_to = Some(forgotten_at.clone());
            memory_sync_record_value(&record)
        })
        .collect::<Vec<_>>();
    post_hugr_api_memory_payloads(
        config,
        "forget",
        &[api_payload_for_records(SyncTableKind::Memories, updates)],
    )?;

    Ok(ForgetResult {
        query: query.to_string(),
        forgotten_count: memories.len(),
        forgotten_at,
        memories,
    })
}

fn memory_maintenance_report_via_hugr_api(
    config: &StorageConfig,
) -> Result<MemoryMaintenanceReport, String> {
    let records = fetch_hugr_api_memory_records(config)?;
    Ok(memory_maintenance_report_from_records(&records))
}

fn consolidate_duplicate_memories_via_hugr_api(
    config: &StorageConfig,
) -> Result<MemoryConsolidationResult, String> {
    let records = fetch_hugr_api_memory_records(config)?;
    let report = memory_maintenance_report_from_records(&records);
    let executed_at = now_ms()?.to_string();
    let mut records_by_id = records
        .into_iter()
        .map(|record| (record.id.clone(), record))
        .collect::<HashMap<_, _>>();
    let mut kept_memories = Vec::new();
    let mut retired_memories = Vec::new();
    let mut updates = Vec::new();

    for group in &report.duplicate_groups {
        let Some((kept, retired)) = group.memories.split_first() else {
            continue;
        };
        kept_memories.push(kept.clone());
        for memory in retired {
            let Some(record) = records_by_id.get_mut(&memory.id) else {
                continue;
            };
            record.valid_to = Some(executed_at.clone());
            record.superseded_by = Some(kept.id.clone());
            updates.push(memory_sync_record_value(record));
            retired_memories.push(memory.clone());
        }
    }
    post_hugr_api_memory_payloads(
        config,
        "consolidate duplicates",
        &[api_payload_for_records(SyncTableKind::Memories, updates)],
    )?;

    Ok(MemoryConsolidationResult {
        executed_at,
        duplicate_groups: report.duplicate_groups,
        kept_memories,
        retired_memories,
    })
}

fn retire_stale_memories_via_hugr_api(
    config: &StorageConfig,
) -> Result<StaleRetirementResult, String> {
    let records = fetch_hugr_api_memory_records(config)?;
    let report = memory_maintenance_report_from_records(&records);
    let executed_at = now_ms()?.to_string();
    let mut records_by_id = records
        .into_iter()
        .map(|record| (record.id.clone(), record))
        .collect::<HashMap<_, _>>();
    let mut kept_memories = Vec::new();
    let mut retired_memories = Vec::new();
    let mut seen_kept = HashSet::new();
    let mut seen_retired = HashSet::new();
    let mut updates = Vec::new();

    for candidate in &report.stale_candidates {
        if seen_kept.insert(candidate.newer_memory.id.clone()) {
            kept_memories.push(candidate.newer_memory.clone());
        }
        if !seen_retired.insert(candidate.older_memory.id.clone()) {
            continue;
        }
        let Some(record) = records_by_id.get_mut(&candidate.older_memory.id) else {
            continue;
        };
        record.valid_to = Some(executed_at.clone());
        record.superseded_by = Some(candidate.newer_memory.id.clone());
        updates.push(memory_sync_record_value(record));
        retired_memories.push(candidate.older_memory.clone());
    }
    post_hugr_api_memory_payloads(
        config,
        "retire stale memories",
        &[api_payload_for_records(SyncTableKind::Memories, updates)],
    )?;

    Ok(StaleRetirementResult {
        executed_at,
        stale_candidates: report.stale_candidates,
        kept_memories,
        retired_memories,
    })
}

fn memory_maintenance_report_from_records(records: &[MemorySyncRecord]) -> MemoryMaintenanceReport {
    let active = records
        .iter()
        .filter(|record| record.valid_to.is_none())
        .map(memory_from_sync_record)
        .collect::<Vec<_>>();
    let retired_count = records
        .iter()
        .filter(|record| record.valid_to.is_some())
        .count();
    let mut grouped = HashMap::<String, Vec<Memory>>::new();

    for memory in &active {
        grouped
            .entry(normalized_memory_text(&memory.text))
            .or_default()
            .push(memory.clone());
    }

    let mut duplicate_groups = grouped
        .into_iter()
        .filter_map(|(normalized_text, mut memories)| {
            (memories.len() > 1).then(|| {
                memories.sort_by(|left, right| right.created_at_ms.cmp(&left.created_at_ms));
                DuplicateMemoryGroup {
                    normalized_text,
                    memories,
                }
            })
        })
        .collect::<Vec<_>>();
    duplicate_groups.sort_by(|left, right| {
        right
            .memories
            .len()
            .cmp(&left.memories.len())
            .then_with(|| left.normalized_text.cmp(&right.normalized_text))
    });

    MemoryMaintenanceReport {
        active_count: active.len(),
        retired_count,
        duplicate_groups,
        stale_candidates: stale_memory_candidates(&active),
    }
}

fn rank_memories_by_query(
    memories: Vec<Memory>,
    terms: &[String],
    query: &str,
    limit: usize,
) -> Vec<Memory> {
    let mut matches = memories
        .into_iter()
        .filter_map(|memory| {
            let score = recall_score(&memory, terms, query);
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
    matches.into_iter().map(|(_, memory)| memory).collect()
}

fn memory_from_sync_record(record: &MemorySyncRecord) -> Memory {
    Memory {
        id: record.id.clone(),
        created_at_ms: record.created_at_ms,
        kind: record.kind.clone(),
        text: record.text.clone(),
        structured_payload: record.structured_payload.clone(),
    }
}

fn promoted_memory_for_session_records(
    records: &[MemorySyncRecord],
    promotions: &[SessionPromotionSyncRecord],
    session_id: &str,
) -> Option<Memory> {
    if let Some(memory_id) = promotions
        .iter()
        .filter(|promotion| promotion.session_id == session_id)
        .max_by(|left, right| left.promoted_at_ms.cmp(&right.promoted_at_ms))
        .map(|promotion| promotion.memory_id.as_str())
    {
        if let Some(record) = records.iter().find(|record| record.id == memory_id) {
            return Some(memory_from_sync_record(record));
        }
    }

    records
        .iter()
        .find(|record| memory_record_promotes_session(record, session_id))
        .map(memory_from_sync_record)
}

fn memory_record_promotes_session(record: &MemorySyncRecord, session_id: &str) -> bool {
    let Some(payload) = record.structured_payload.as_deref() else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    value
        .get("source")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|source| {
            source.get("type").and_then(serde_json::Value::as_str) == Some("session_promotion")
                && source.get("session_id").and_then(serde_json::Value::as_str) == Some(session_id)
        })
}

fn fetch_hugr_api_memory_records(config: &StorageConfig) -> Result<Vec<MemorySyncRecord>, String> {
    let response = get_hugr_api_json(config, "/v1/memories")?;
    parse_hugr_api_memory_records_response(&response)
}

fn post_hugr_api_memory_payloads(
    config: &StorageConfig,
    operation: &str,
    payloads: &[SyncApiTablePayload],
) -> Result<Vec<SyncTableResult>, String> {
    if payloads.iter().all(|payload| payload.records.is_empty()) {
        return Ok(Vec::new());
    }

    let body = hugr_api_memory_apply_request(payloads);
    let response = post_hugr_api_json(config, "/v1/memories", &body)?;
    let parsed = parse_hugr_api_memory_apply_response(&response)?;
    ensure_hugr_api_memory_operation_accepted(operation, &parsed.status, &parsed.tables)?;
    Ok(parsed.tables)
}

fn hugr_api_memory_apply_request(payloads: &[SyncApiTablePayload]) -> serde_json::Value {
    json!({
        "contract_version": HUGR_API_CONTRACT_VERSION,
        "tables": payloads.iter().map(sync_api_table_payload_value).collect::<Vec<_>>()
    })
}

fn ensure_hugr_api_memory_operation_accepted(
    operation: &str,
    status: &str,
    tables: &[SyncTableResult],
) -> Result<(), String> {
    if status == "accepted" && tables.iter().all(|table| table.conflict_count == 0) {
        return Ok(());
    }

    let conflicts = tables
        .iter()
        .filter(|table| table.conflict_count > 0)
        .map(|table| format!("{}:{}", table.table, table.conflict_count))
        .collect::<Vec<_>>();
    let details = if conflicts.is_empty() {
        String::new()
    } else {
        format!(" ({})", conflicts.join(", "))
    };
    Err(format!(
        "remote Hugr API {operation} returned status '{status}'{details}"
    ))
}

#[derive(Debug, Clone, PartialEq)]
struct HugrApiMemoryApplyResponse {
    status: String,
    tables: Vec<SyncTableResult>,
}

fn parse_hugr_api_memory_records_response(response: &str) -> Result<Vec<MemorySyncRecord>, String> {
    let value = serde_json::from_str::<serde_json::Value>(response)
        .map_err(|error| format!("invalid Hugr API memory response: {error}"))?;
    reject_hugr_api_error(&value)?;
    let contract_version = json_string_field(&value, "contract_version")?;
    if contract_version != HUGR_API_CONTRACT_VERSION {
        return Err(format!(
            "unsupported Hugr API contract version '{contract_version}'"
        ));
    }
    json_array_field(&value, "records")?
        .iter()
        .map(memory_sync_record_from_value)
        .collect()
}

fn parse_hugr_api_memory_apply_response(
    response: &str,
) -> Result<HugrApiMemoryApplyResponse, String> {
    let value = serde_json::from_str::<serde_json::Value>(response)
        .map_err(|error| format!("invalid Hugr API memory response: {error}"))?;
    reject_hugr_api_error(&value)?;
    let contract_version = json_string_field(&value, "contract_version")?;
    if contract_version != HUGR_API_CONTRACT_VERSION {
        return Err(format!(
            "unsupported Hugr API contract version '{contract_version}'"
        ));
    }
    Ok(HugrApiMemoryApplyResponse {
        status: json_string_field(&value, "status")?,
        tables: json_array_field(&value, "tables")?
            .iter()
            .map(parse_sync_table_result)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn post_hugr_api_sync(
    config: &StorageConfig,
    operation: &str,
    dry_run: bool,
    payloads: &[SyncApiTablePayload],
) -> Result<String, String> {
    let body = hugr_api_sync_request(config, operation, dry_run, payloads);
    post_hugr_api_json(config, &format!("/v1/sync/{operation}"), &body)
}

fn fetch_hugr_api_history(
    config: &StorageConfig,
    limit: usize,
) -> Result<Vec<SyncRunHistory>, String> {
    let response = get_hugr_api_json(config, &format!("/v1/sync/history?limit={limit}"))?;
    parse_hugr_api_history_response(&response)
}

fn hugr_api_sync_request(
    config: &StorageConfig,
    operation: &str,
    dry_run: bool,
    payloads: &[SyncApiTablePayload],
) -> serde_json::Value {
    json!({
        "contract_version": HUGR_API_CONTRACT_VERSION,
        "operation": operation,
        "dry_run": dry_run,
        "storage_mode": config.mode.as_str(),
        "sync_classes": config.sync_classes.iter().map(|class| class.as_str()).collect::<Vec<_>>(),
        "explicit_opt_in_classes": config.sync_classes.iter().filter(|class| class.requires_explicit_opt_in()).map(|class| class.as_str()).collect::<Vec<_>>(),
        "tables": payloads.iter().map(sync_api_table_payload_value).collect::<Vec<_>>()
    })
}

fn sync_api_table_payload_value(payload: &SyncApiTablePayload) -> serde_json::Value {
    let mut value = sync_table_result_value(&payload.result);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "records".to_string(),
            serde_json::Value::Array(payload.records.clone()),
        );
    }
    value
}

fn sync_table_result_value(table: &SyncTableResult) -> serde_json::Value {
    json!({
        "class": table.class,
        "table": table.table,
        "row_count": table.row_count,
        "inserted_count": table.inserted_count,
        "updated_count": table.updated_count,
        "skipped_count": table.skipped_count,
        "conflict_count": table.conflict_count,
        "executed": table.executed,
        "conflicts": table.conflicts.iter().map(|conflict| {
            json!({
                "reason": conflict.reason,
                "count": conflict.count
            })
        }).collect::<Vec<_>>()
    })
}

fn post_hugr_api_json(
    config: &StorageConfig,
    path: &str,
    body: &serde_json::Value,
) -> Result<String, String> {
    request_hugr_api_json(config, "POST", path, Some(body))
}

fn get_hugr_api_json(config: &StorageConfig, path: &str) -> Result<String, String> {
    request_hugr_api_json(config, "GET", path, None)
}

fn request_hugr_api_json(
    config: &StorageConfig,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<String, String> {
    let url = hugr_api_route_url(
        config
            .remote_url
            .as_ref()
            .ok_or_else(|| "HUGR_SYNC_BACKEND=hugr_api requires HUGR_API_URL".to_string())?,
        path,
    );
    let token = config
        .remote_auth_token
        .as_ref()
        .ok_or_else(|| "HUGR_SYNC_BACKEND=hugr_api requires HUGR_API_TOKEN".to_string())?;
    let mut args = vec![
        "-fsS".to_string(),
        "-X".to_string(),
        method.to_string(),
        url,
        "-H".to_string(),
        "Accept: application/json".to_string(),
        "-H".to_string(),
        format!("Authorization: Bearer {token}"),
    ];
    if body.is_some() {
        args.extend([
            "-H".to_string(),
            "Content-Type: application/json".to_string(),
            "--data-binary".to_string(),
            "@-".to_string(),
        ]);
    }

    let mut command = ProcessCommand::new("curl");
    command
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if body.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to execute curl for Hugr API: {error}"))?;

    if let Some(body) = body {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "failed to open Hugr API request stdin".to_string())?;
        stdin
            .write_all(body.to_string().as_bytes())
            .map_err(|error| format!("failed to write Hugr API request: {error}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to read Hugr API response: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(format!(
                "Hugr API request failed with status {}",
                output.status
            ));
        }
        return Err(format!("Hugr API request failed: {stderr}"));
    }

    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn hugr_api_route_url(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[derive(Debug, Clone, PartialEq)]
struct HugrApiSyncResponse {
    run_id: Option<String>,
    status: String,
    tables: Vec<SyncTableResult>,
    payloads: Vec<SyncApiTablePayload>,
}

fn parse_hugr_api_sync_response(response: &str) -> Result<HugrApiSyncResponse, String> {
    let value = serde_json::from_str::<serde_json::Value>(response)
        .map_err(|error| format!("invalid Hugr API sync response: {error}"))?;
    reject_hugr_api_error(&value)?;
    let payloads = json_array_field(&value, "tables")?
        .iter()
        .map(parse_sync_api_table_payload)
        .collect::<Result<Vec<_>, _>>()?;
    let tables = payloads
        .iter()
        .map(|payload| payload.result.clone())
        .collect();
    Ok(HugrApiSyncResponse {
        run_id: json_optional_string_field(&value, "run_id")?,
        status: json_string_field(&value, "status")?,
        tables,
        payloads,
    })
}

fn parse_sync_api_table_payload(value: &serde_json::Value) -> Result<SyncApiTablePayload, String> {
    let records = value
        .get("records")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(SyncApiTablePayload {
        result: parse_sync_table_result(value)?,
        records,
    })
}

fn parse_hugr_api_history_response(response: &str) -> Result<Vec<SyncRunHistory>, String> {
    let value = serde_json::from_str::<serde_json::Value>(response)
        .map_err(|error| format!("invalid Hugr API history response: {error}"))?;
    reject_hugr_api_error(&value)?;
    json_array_field(&value, "runs")?
        .iter()
        .map(parse_sync_run_history)
        .collect()
}

fn parse_sync_run_history(value: &serde_json::Value) -> Result<SyncRunHistory, String> {
    Ok(SyncRunHistory {
        id: json_string_field(value, "id")?,
        operation: json_string_field(value, "operation")?,
        backend: json_string_field(value, "backend")?,
        status: json_string_field(value, "status")?,
        started_at_ms: json_i64_field(value, "started_at_ms")?,
        ended_at_ms: json_i64_field(value, "ended_at_ms")?,
        tables: json_array_field(value, "tables")?
            .iter()
            .map(parse_sync_table_result)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn parse_sync_table_result(value: &serde_json::Value) -> Result<SyncTableResult, String> {
    Ok(SyncTableResult {
        class: json_string_field(value, "class")?,
        table: json_string_field(value, "table")?,
        row_count: json_usize_field(value, "row_count")?,
        inserted_count: json_usize_field(value, "inserted_count")?,
        updated_count: json_usize_field(value, "updated_count")?,
        skipped_count: json_usize_field(value, "skipped_count")?,
        conflict_count: json_usize_field(value, "conflict_count")?,
        executed: json_bool_field(value, "executed")?,
        conflicts: json_array_field(value, "conflicts")?
            .iter()
            .map(parse_sync_conflict_summary)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn parse_sync_conflict_summary(value: &serde_json::Value) -> Result<SyncConflictSummary, String> {
    Ok(SyncConflictSummary {
        reason: json_string_field(value, "reason")?,
        count: json_usize_field(value, "count")?,
    })
}

fn reject_hugr_api_error(value: &serde_json::Value) -> Result<(), String> {
    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
    {
        return Err(format!("Hugr API request failed: {message}"));
    }
    if let Some(message) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(format!("Hugr API request failed: {message}"));
    }
    Ok(())
}

fn json_array_field<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("Hugr API response missing array field '{field}'"))
}

fn optional_json_array_field(value: &serde_json::Value, field: &str) -> Vec<serde_json::Value> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn json_string_field(value: &serde_json::Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("Hugr API response missing string field '{field}'"))
}

fn json_optional_string_field(
    value: &serde_json::Value,
    field: &str,
) -> Result<Option<String>, String> {
    match value.get(field) {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| format!("Hugr API response field '{field}' must be a string")),
    }
}

fn json_i64_field(value: &serde_json::Value, field: &str) -> Result<i64, String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| format!("Hugr API response missing integer field '{field}'"))
}

fn json_usize_field(value: &serde_json::Value, field: &str) -> Result<usize, String> {
    let raw = value
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("Hugr API response missing unsigned integer field '{field}'"))?;
    usize::try_from(raw).map_err(|error| error.to_string())
}

fn json_bool_field(value: &serde_json::Value, field: &str) -> Result<bool, String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| format!("Hugr API response missing boolean field '{field}'"))
}

fn json_f64_field(value: &serde_json::Value, field: &str) -> Result<f64, String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| format!("Hugr API response missing number field '{field}'"))
}

fn json_optional_i64_field(value: &serde_json::Value, field: &str) -> Result<Option<i64>, String> {
    match value.get(field) {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("Hugr API response field '{field}' must be an integer")),
    }
}

fn json_bytes_field(value: &serde_json::Value, field: &str) -> Result<Vec<u8>, String> {
    let values = json_array_field(value, field)?;
    values
        .iter()
        .map(|value| {
            let byte = value
                .as_u64()
                .ok_or_else(|| format!("Hugr API response field '{field}' must contain bytes"))?;
            u8::try_from(byte).map_err(|error| error.to_string())
        })
        .collect()
}

fn api_sync_status_for_payloads(payloads: &[SyncApiTablePayload]) -> String {
    if payloads
        .iter()
        .any(|payload| payload.result.conflict_count > 0)
    {
        "partial".to_string()
    } else {
        "accepted".to_string()
    }
}

fn unsupported_api_table_result(result: &SyncTableResult) -> SyncTableResult {
    let skipped_count = result.row_count;
    let conflicts = if skipped_count == 0 {
        Vec::new()
    } else {
        vec![SyncConflictSummary {
            reason: "api_row_payload_not_supported".to_string(),
            count: skipped_count,
        }]
    };

    SyncTableResult {
        class: result.class.clone(),
        table: result.table.clone(),
        row_count: result.row_count,
        inserted_count: 0,
        updated_count: 0,
        skipped_count,
        conflict_count: skipped_count,
        executed: true,
        conflicts,
    }
}

fn missing_api_row_payload_result(result: &SyncTableResult) -> SyncTableResult {
    let skipped_count = result.row_count;
    SyncTableResult {
        class: result.class.clone(),
        table: result.table.clone(),
        row_count: result.row_count,
        inserted_count: 0,
        updated_count: 0,
        skipped_count,
        conflict_count: skipped_count,
        executed: true,
        conflicts: vec![SyncConflictSummary {
            reason: "api_row_payload_missing".to_string(),
            count: skipped_count,
        }],
    }
}

fn api_table_supports_records(table: SyncTableKind) -> bool {
    matches!(
        table,
        SyncTableKind::Projects
            | SyncTableKind::Memories
            | SyncTableKind::MemoryEmbeddings
            | SyncTableKind::Sources
            | SyncTableKind::DiscoveredFiles
            | SyncTableKind::Entities
            | SyncTableKind::CodeSymbols
            | SyncTableKind::Edges
            | SyncTableKind::CodeReferences
            | SyncTableKind::TestMappings
            | SyncTableKind::SourceEmbeddings
            | SyncTableKind::Sessions
            | SyncTableKind::ContextPacks
            | SyncTableKind::Diagnostics
    )
}

fn api_storage_table_supports_records(table: SyncTableKind) -> bool {
    matches!(
        table,
        SyncTableKind::Projects
            | SyncTableKind::Sources
            | SyncTableKind::DiscoveredFiles
            | SyncTableKind::Entities
            | SyncTableKind::CodeSymbols
            | SyncTableKind::Edges
            | SyncTableKind::CodeReferences
            | SyncTableKind::TestMappings
            | SyncTableKind::SourceEmbeddings
            | SyncTableKind::Sessions
            | SyncTableKind::ContextPacks
            | SyncTableKind::Diagnostics
    )
}

async fn api_sync_records_for_table(
    conn: &Connection,
    table: SyncTableKind,
) -> Result<Vec<serde_json::Value>, String> {
    match table {
        SyncTableKind::Projects => project_sync_records(conn).await,
        SyncTableKind::Memories => memory_sync_records(conn).await,
        SyncTableKind::MemoryEmbeddings => memory_embedding_sync_records(conn).await,
        SyncTableKind::Sources => source_sync_records(conn).await,
        SyncTableKind::DiscoveredFiles => discovered_file_sync_records(conn).await,
        SyncTableKind::Entities => entity_sync_records(conn).await,
        SyncTableKind::CodeSymbols => code_symbol_sync_records(conn).await,
        SyncTableKind::Edges => edge_sync_records(conn).await,
        SyncTableKind::CodeReferences => code_reference_sync_records(conn).await,
        SyncTableKind::TestMappings => test_mapping_sync_records(conn).await,
        SyncTableKind::SourceEmbeddings => source_embedding_sync_records(conn).await,
        SyncTableKind::Sessions => session_sync_records(conn).await,
        SyncTableKind::ContextPacks => context_pack_sync_records(conn).await,
        SyncTableKind::Diagnostics => diagnostic_sync_records(conn).await,
    }
}

async fn project_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, name, root_path, git_remote, default_branch, created_at_ms, updated_at_ms
            FROM projects
            ORDER BY id
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "name": row.get::<String>(1).map_err(|error| error.to_string())?,
            "root_path": row.get::<String>(2).map_err(|error| error.to_string())?,
            "git_remote": row.get::<Option<String>>(3).map_err(|error| error.to_string())?,
            "default_branch": row.get::<Option<String>>(4).map_err(|error| error.to_string())?,
            "created_at_ms": row.get::<i64>(5).map_err(|error| error.to_string())?,
            "updated_at_ms": row.get::<i64>(6).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn memory_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT
                id, created_at_ms, kind, text, confidence, valid_from, valid_to,
                superseded_by, sensitivity, structured_payload
            FROM memories
            ORDER BY created_at_ms, id
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "created_at_ms": row.get::<i64>(1).map_err(|error| error.to_string())?,
            "kind": row.get::<String>(2).map_err(|error| error.to_string())?,
            "text": row.get::<String>(3).map_err(|error| error.to_string())?,
            "confidence": row.get::<f64>(4).map_err(|error| error.to_string())?,
            "valid_from": row.get::<Option<String>>(5).map_err(|error| error.to_string())?,
            "valid_to": row.get::<Option<String>>(6).map_err(|error| error.to_string())?,
            "superseded_by": row.get::<Option<String>>(7).map_err(|error| error.to_string())?,
            "sensitivity": row.get::<String>(8).map_err(|error| error.to_string())?,
            "structured_payload": row.get::<Option<String>>(9).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn memory_embedding_sync_records(
    conn: &Connection,
) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "SELECT memory_id, model, dimensions, embedding FROM memory_embeddings ORDER BY memory_id",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "memory_id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "model": row.get::<String>(1).map_err(|error| error.to_string())?,
            "dimensions": row.get::<i64>(2).map_err(|error| error.to_string())?,
            "embedding": row.get::<Vec<u8>>(3).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn source_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "SELECT id, kind, locator, created_at_ms FROM sources ORDER BY id",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "kind": row.get::<String>(1).map_err(|error| error.to_string())?,
            "locator": row.get::<String>(2).map_err(|error| error.to_string())?,
            "created_at_ms": row.get::<i64>(3).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn discovered_file_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT project_id, path, language, size_bytes, discovered_at_ms, updated_at_ms
            FROM discovered_files
            ORDER BY project_id, path
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "project_id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "path": row.get::<String>(1).map_err(|error| error.to_string())?,
            "language": row.get::<Option<String>>(2).map_err(|error| error.to_string())?,
            "size_bytes": row.get::<Option<i64>>(3).map_err(|error| error.to_string())?,
            "discovered_at_ms": row.get::<i64>(4).map_err(|error| error.to_string())?,
            "updated_at_ms": row.get::<i64>(5).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn entity_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "SELECT id, kind, name, locator, created_at_ms FROM entities ORDER BY id",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "kind": row.get::<String>(1).map_err(|error| error.to_string())?,
            "name": row.get::<String>(2).map_err(|error| error.to_string())?,
            "locator": row.get::<Option<String>>(3).map_err(|error| error.to_string())?,
            "created_at_ms": row.get::<i64>(4).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn code_symbol_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT
                project_id, path, name, kind, language, line_start, line_end,
                signature, indexed_at_ms
            FROM code_symbols
            ORDER BY project_id, path, kind, name, line_start
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "project_id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "path": row.get::<String>(1).map_err(|error| error.to_string())?,
            "name": row.get::<String>(2).map_err(|error| error.to_string())?,
            "kind": row.get::<String>(3).map_err(|error| error.to_string())?,
            "language": row.get::<Option<String>>(4).map_err(|error| error.to_string())?,
            "line_start": row.get::<i64>(5).map_err(|error| error.to_string())?,
            "line_end": row.get::<Option<i64>>(6).map_err(|error| error.to_string())?,
            "signature": row.get::<String>(7).map_err(|error| error.to_string())?,
            "indexed_at_ms": row.get::<i64>(8).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn edge_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "SELECT id, from_id, to_id, kind, created_at_ms FROM edges ORDER BY id",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "from_id": row.get::<String>(1).map_err(|error| error.to_string())?,
            "to_id": row.get::<String>(2).map_err(|error| error.to_string())?,
            "kind": row.get::<String>(3).map_err(|error| error.to_string())?,
            "created_at_ms": row.get::<i64>(4).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn code_reference_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT
                project_id, path, target_path, target_name, target_kind, kind,
                language, line_start, excerpt, indexed_at_ms
            FROM code_references
            ORDER BY project_id, path, target_path, target_name, line_start, kind
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "project_id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "path": row.get::<String>(1).map_err(|error| error.to_string())?,
            "target_path": row.get::<String>(2).map_err(|error| error.to_string())?,
            "target_name": row.get::<String>(3).map_err(|error| error.to_string())?,
            "target_kind": row.get::<String>(4).map_err(|error| error.to_string())?,
            "kind": row.get::<String>(5).map_err(|error| error.to_string())?,
            "language": row.get::<Option<String>>(6).map_err(|error| error.to_string())?,
            "line_start": row.get::<i64>(7).map_err(|error| error.to_string())?,
            "excerpt": row.get::<String>(8).map_err(|error| error.to_string())?,
            "indexed_at_ms": row.get::<i64>(9).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn test_mapping_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT project_id, source_path, test_path, reason, score, updated_at_ms
            FROM test_mappings
            ORDER BY project_id, source_path, score DESC, test_path
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "project_id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "source_path": row.get::<String>(1).map_err(|error| error.to_string())?,
            "test_path": row.get::<String>(2).map_err(|error| error.to_string())?,
            "reason": row.get::<String>(3).map_err(|error| error.to_string())?,
            "score": row.get::<i64>(4).map_err(|error| error.to_string())?,
            "updated_at_ms": row.get::<i64>(5).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn source_embedding_sync_records(
    conn: &Connection,
) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT project_id, path, model, dimensions, embedding, content_key, updated_at_ms
            FROM source_embeddings
            ORDER BY project_id, path
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "project_id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "path": row.get::<String>(1).map_err(|error| error.to_string())?,
            "model": row.get::<String>(2).map_err(|error| error.to_string())?,
            "dimensions": row.get::<i64>(3).map_err(|error| error.to_string())?,
            "embedding": row.get::<Vec<u8>>(4).map_err(|error| error.to_string())?,
            "content_key": row.get::<String>(5).map_err(|error| error.to_string())?,
            "updated_at_ms": row.get::<i64>(6).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn session_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, project_id, task, branch, started_at_ms, ended_at_ms, final_summary
            FROM sessions
            WHERE final_summary IS NOT NULL
            ORDER BY started_at_ms, id
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "project_id": row.get::<String>(1).map_err(|error| error.to_string())?,
            "task": row.get::<String>(2).map_err(|error| error.to_string())?,
            "branch": row.get::<Option<String>>(3).map_err(|error| error.to_string())?,
            "started_at_ms": row.get::<i64>(4).map_err(|error| error.to_string())?,
            "ended_at_ms": row.get::<Option<i64>>(5).map_err(|error| error.to_string())?,
            "final_summary": row.get::<Option<String>>(6).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn session_storage_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, project_id, task, branch, started_at_ms, ended_at_ms, final_summary
            FROM sessions
            ORDER BY started_at_ms, id
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "project_id": row.get::<String>(1).map_err(|error| error.to_string())?,
            "task": row.get::<String>(2).map_err(|error| error.to_string())?,
            "branch": row.get::<Option<String>>(3).map_err(|error| error.to_string())?,
            "started_at_ms": row.get::<i64>(4).map_err(|error| error.to_string())?,
            "ended_at_ms": row.get::<Option<i64>>(5).map_err(|error| error.to_string())?,
            "final_summary": row.get::<Option<String>>(6).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn context_pack_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, project_id, task, payload_json, created_at_ms, updated_at_ms
            FROM context_packs
            ORDER BY project_id, updated_at_ms, id
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "project_id": row.get::<String>(1).map_err(|error| error.to_string())?,
            "task": row.get::<String>(2).map_err(|error| error.to_string())?,
            "payload_json": row.get::<String>(3).map_err(|error| error.to_string())?,
            "created_at_ms": row.get::<i64>(4).map_err(|error| error.to_string())?,
            "updated_at_ms": row.get::<i64>(5).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn diagnostic_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    Ok(local_diagnostic_sync_records(conn)
        .await?
        .iter()
        .map(diagnostic_sync_record_value)
        .collect())
}

async fn session_event_sync_records(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, session_id, kind, detail, created_at_ms
            FROM session_events
            ORDER BY created_at_ms, id
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "session_id": row.get::<String>(1).map_err(|error| error.to_string())?,
            "kind": row.get::<String>(2).map_err(|error| error.to_string())?,
            "detail": row.get::<String>(3).map_err(|error| error.to_string())?,
            "created_at_ms": row.get::<i64>(4).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

async fn session_promotion_sync_records(
    conn: &Connection,
) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = conn
        .query(
            "
            SELECT session_id, memory_id, promoted_at_ms
            FROM session_promotions
            ORDER BY promoted_at_ms, session_id
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut records = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        records.push(json!({
            "session_id": row.get::<String>(0).map_err(|error| error.to_string())?,
            "memory_id": row.get::<String>(1).map_err(|error| error.to_string())?,
            "promoted_at_ms": row.get::<i64>(2).map_err(|error| error.to_string())?
        }));
    }

    Ok(records)
}

#[derive(Debug, Clone)]
struct ProjectSyncRecord {
    id: String,
    name: String,
    root_path: String,
    git_remote: Option<String>,
    default_branch: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

fn project_sync_record_from_value(value: &serde_json::Value) -> Result<ProjectSyncRecord, String> {
    Ok(ProjectSyncRecord {
        id: json_string_field(value, "id")?,
        name: json_string_field(value, "name")?,
        root_path: json_string_field(value, "root_path")?,
        git_remote: json_optional_string_field(value, "git_remote")?,
        default_branch: json_optional_string_field(value, "default_branch")?,
        created_at_ms: json_i64_field(value, "created_at_ms")?,
        updated_at_ms: json_i64_field(value, "updated_at_ms")?,
    })
}

#[derive(Debug, Clone)]
struct MemorySyncRecord {
    id: String,
    created_at_ms: i64,
    kind: String,
    text: String,
    confidence: f64,
    valid_from: Option<String>,
    valid_to: Option<String>,
    superseded_by: Option<String>,
    sensitivity: String,
    structured_payload: Option<String>,
}

#[derive(Debug, Clone)]
struct MemoryEmbeddingSyncRecord {
    memory_id: String,
    model: String,
    dimensions: i64,
    embedding: Vec<u8>,
}

fn memory_embedding_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<MemoryEmbeddingSyncRecord, String> {
    Ok(MemoryEmbeddingSyncRecord {
        memory_id: json_string_field(value, "memory_id")?,
        model: json_string_field(value, "model")?,
        dimensions: json_i64_field(value, "dimensions")?,
        embedding: json_bytes_field(value, "embedding")?,
    })
}

fn memory_sync_record_from_value(value: &serde_json::Value) -> Result<MemorySyncRecord, String> {
    Ok(MemorySyncRecord {
        id: json_string_field(value, "id")?,
        created_at_ms: json_i64_field(value, "created_at_ms")?,
        kind: json_string_field(value, "kind")?,
        text: json_string_field(value, "text")?,
        confidence: json_f64_field(value, "confidence")?,
        valid_from: json_optional_string_field(value, "valid_from")?,
        valid_to: json_optional_string_field(value, "valid_to")?,
        superseded_by: json_optional_string_field(value, "superseded_by")?,
        sensitivity: json_string_field(value, "sensitivity")?,
        structured_payload: json_optional_string_field(value, "structured_payload")?,
    })
}

#[derive(Debug, Clone)]
struct SourceSyncRecord {
    id: String,
    kind: String,
    locator: String,
    created_at_ms: i64,
}

fn source_sync_record_from_value(value: &serde_json::Value) -> Result<SourceSyncRecord, String> {
    Ok(SourceSyncRecord {
        id: json_string_field(value, "id")?,
        kind: json_string_field(value, "kind")?,
        locator: json_string_field(value, "locator")?,
        created_at_ms: json_i64_field(value, "created_at_ms")?,
    })
}

#[derive(Debug, Clone)]
struct DiscoveredFileSyncRecord {
    project_id: String,
    path: String,
    language: Option<String>,
    size_bytes: Option<i64>,
    discovered_at_ms: i64,
    updated_at_ms: i64,
}

fn discovered_file_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<DiscoveredFileSyncRecord, String> {
    Ok(DiscoveredFileSyncRecord {
        project_id: json_string_field(value, "project_id")?,
        path: json_string_field(value, "path")?,
        language: json_optional_string_field(value, "language")?,
        size_bytes: json_optional_i64_field(value, "size_bytes")?,
        discovered_at_ms: json_i64_field(value, "discovered_at_ms")?,
        updated_at_ms: json_i64_field(value, "updated_at_ms")?,
    })
}

#[derive(Debug, Clone)]
struct EntitySyncRecord {
    id: String,
    kind: String,
    name: String,
    locator: Option<String>,
    created_at_ms: i64,
}

fn entity_sync_record_from_value(value: &serde_json::Value) -> Result<EntitySyncRecord, String> {
    Ok(EntitySyncRecord {
        id: json_string_field(value, "id")?,
        kind: json_string_field(value, "kind")?,
        name: json_string_field(value, "name")?,
        locator: json_optional_string_field(value, "locator")?,
        created_at_ms: json_i64_field(value, "created_at_ms")?,
    })
}

#[derive(Debug, Clone)]
struct CodeSymbolSyncRecord {
    project_id: String,
    path: String,
    name: String,
    kind: String,
    language: Option<String>,
    line_start: i64,
    line_end: Option<i64>,
    signature: String,
    indexed_at_ms: i64,
}

fn code_symbol_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<CodeSymbolSyncRecord, String> {
    Ok(CodeSymbolSyncRecord {
        project_id: json_string_field(value, "project_id")?,
        path: json_string_field(value, "path")?,
        name: json_string_field(value, "name")?,
        kind: json_string_field(value, "kind")?,
        language: json_optional_string_field(value, "language")?,
        line_start: json_i64_field(value, "line_start")?,
        line_end: json_optional_i64_field(value, "line_end")?,
        signature: json_string_field(value, "signature")?,
        indexed_at_ms: json_i64_field(value, "indexed_at_ms")?,
    })
}

#[derive(Debug, Clone)]
struct EdgeSyncRecord {
    id: String,
    from_id: String,
    to_id: String,
    kind: String,
    created_at_ms: i64,
}

fn edge_sync_record_from_value(value: &serde_json::Value) -> Result<EdgeSyncRecord, String> {
    Ok(EdgeSyncRecord {
        id: json_string_field(value, "id")?,
        from_id: json_string_field(value, "from_id")?,
        to_id: json_string_field(value, "to_id")?,
        kind: json_string_field(value, "kind")?,
        created_at_ms: json_i64_field(value, "created_at_ms")?,
    })
}

#[derive(Debug, Clone)]
struct DiagnosticSyncRecord {
    id: String,
    project_id: String,
    source: String,
    path: Option<String>,
    line_start: Option<i64>,
    line_end: Option<i64>,
    severity: String,
    code: Option<String>,
    message: String,
    command: Option<String>,
    created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditFreshnessEvent {
    detail: String,
    created_at_ms: i64,
}

fn diagnostic_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<DiagnosticSyncRecord, String> {
    Ok(DiagnosticSyncRecord {
        id: json_string_field(value, "id")?,
        project_id: json_string_field(value, "project_id")?,
        source: json_string_field(value, "source")?,
        path: json_optional_string_field(value, "path")?,
        line_start: json_optional_i64_field(value, "line_start")?,
        line_end: json_optional_i64_field(value, "line_end")?,
        severity: json_string_field(value, "severity")?,
        code: json_optional_string_field(value, "code")?,
        message: json_string_field(value, "message")?,
        command: json_optional_string_field(value, "command")?,
        created_at_ms: json_i64_field(value, "created_at_ms")?,
    })
}

#[derive(Debug, Clone)]
struct CodeReferenceSyncRecord {
    project_id: String,
    path: String,
    target_path: String,
    target_name: String,
    target_kind: String,
    kind: String,
    language: Option<String>,
    line_start: i64,
    excerpt: String,
    indexed_at_ms: i64,
}

#[derive(Debug, Clone)]
struct TestMappingSyncRecord {
    project_id: String,
    source_path: String,
    test_path: String,
    reason: String,
    score: i64,
    updated_at_ms: i64,
}

#[derive(Debug, Clone)]
struct SourceEmbeddingSyncRecord {
    project_id: String,
    path: String,
    model: String,
    dimensions: i64,
    embedding: Vec<u8>,
    content_key: String,
    updated_at_ms: i64,
}

fn code_reference_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<CodeReferenceSyncRecord, String> {
    Ok(CodeReferenceSyncRecord {
        project_id: json_string_field(value, "project_id")?,
        path: json_string_field(value, "path")?,
        target_path: json_string_field(value, "target_path")?,
        target_name: json_string_field(value, "target_name")?,
        target_kind: json_string_field(value, "target_kind")?,
        kind: json_string_field(value, "kind")?,
        language: json_optional_string_field(value, "language")?,
        line_start: json_i64_field(value, "line_start")?,
        excerpt: json_string_field(value, "excerpt")?,
        indexed_at_ms: json_i64_field(value, "indexed_at_ms")?,
    })
}

fn test_mapping_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<TestMappingSyncRecord, String> {
    Ok(TestMappingSyncRecord {
        project_id: json_string_field(value, "project_id")?,
        source_path: json_string_field(value, "source_path")?,
        test_path: json_string_field(value, "test_path")?,
        reason: json_string_field(value, "reason")?,
        score: json_i64_field(value, "score")?,
        updated_at_ms: json_i64_field(value, "updated_at_ms")?,
    })
}

fn source_embedding_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<SourceEmbeddingSyncRecord, String> {
    Ok(SourceEmbeddingSyncRecord {
        project_id: json_string_field(value, "project_id")?,
        path: json_string_field(value, "path")?,
        model: json_string_field(value, "model")?,
        dimensions: json_i64_field(value, "dimensions")?,
        embedding: json_bytes_field(value, "embedding")?,
        content_key: json_string_field(value, "content_key")?,
        updated_at_ms: json_i64_field(value, "updated_at_ms")?,
    })
}

#[derive(Debug, Clone)]
struct SessionSyncRecord {
    id: String,
    project_id: String,
    task: String,
    branch: Option<String>,
    started_at_ms: i64,
    ended_at_ms: Option<i64>,
    final_summary: Option<String>,
}

fn session_sync_record_from_value(value: &serde_json::Value) -> Result<SessionSyncRecord, String> {
    Ok(SessionSyncRecord {
        id: json_string_field(value, "id")?,
        project_id: json_string_field(value, "project_id")?,
        task: json_string_field(value, "task")?,
        branch: json_optional_string_field(value, "branch")?,
        started_at_ms: json_i64_field(value, "started_at_ms")?,
        ended_at_ms: json_optional_i64_field(value, "ended_at_ms")?,
        final_summary: json_optional_string_field(value, "final_summary")?,
    })
}

#[derive(Debug, Clone)]
struct ContextPackSyncRecord {
    id: String,
    project_id: String,
    task: String,
    payload_json: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

fn context_pack_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<ContextPackSyncRecord, String> {
    Ok(ContextPackSyncRecord {
        id: json_string_field(value, "id")?,
        project_id: json_string_field(value, "project_id")?,
        task: json_string_field(value, "task")?,
        payload_json: json_string_field(value, "payload_json")?,
        created_at_ms: json_i64_field(value, "created_at_ms")?,
        updated_at_ms: json_i64_field(value, "updated_at_ms")?,
    })
}

#[derive(Debug, Clone, PartialEq)]
struct SessionEventSyncRecord {
    id: String,
    session_id: String,
    kind: String,
    detail: String,
    created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct SessionPromotionSyncRecord {
    session_id: String,
    memory_id: String,
    promoted_at_ms: i64,
}

fn session_event_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<SessionEventSyncRecord, String> {
    Ok(SessionEventSyncRecord {
        id: json_string_field(value, "id")?,
        session_id: json_string_field(value, "session_id")?,
        kind: json_string_field(value, "kind")?,
        detail: json_string_field(value, "detail")?,
        created_at_ms: json_i64_field(value, "created_at_ms")?,
    })
}

fn session_promotion_sync_record_from_value(
    value: &serde_json::Value,
) -> Result<SessionPromotionSyncRecord, String> {
    Ok(SessionPromotionSyncRecord {
        session_id: json_string_field(value, "session_id")?,
        memory_id: json_string_field(value, "memory_id")?,
        promoted_at_ms: json_i64_field(value, "promoted_at_ms")?,
    })
}

async fn apply_api_push_project_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Projects;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = project_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO projects (
                    id, name, root_path, git_remote, default_branch, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    root_path = excluded.root_path,
                    git_remote = excluded.git_remote,
                    default_branch = excluded.default_branch,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![
                    record.id,
                    record.name,
                    record.root_path,
                    record.git_remote,
                    record.default_branch,
                    record.created_at_ms,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_project_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Projects;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = project_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO projects (
                    id, name, root_path, git_remote, default_branch, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    root_path = excluded.root_path,
                    git_remote = excluded.git_remote,
                    default_branch = excluded.default_branch,
                    updated_at_ms = excluded.updated_at_ms
                WHERE excluded.updated_at_ms > projects.updated_at_ms
                ",
                params![
                    record.id,
                    record.name,
                    record.root_path,
                    record.git_remote,
                    record.default_branch,
                    record.created_at_ms,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_memory_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Memories;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = memory_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO memories (
                    id, created_at_ms, kind, text, confidence, valid_from, valid_to,
                    superseded_by, sensitivity, structured_payload
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(id) DO UPDATE SET
                    created_at_ms = excluded.created_at_ms,
                    kind = excluded.kind,
                    text = excluded.text,
                    confidence = excluded.confidence,
                    valid_from = excluded.valid_from,
                    valid_to = excluded.valid_to,
                    superseded_by = excluded.superseded_by,
                    sensitivity = excluded.sensitivity,
                    structured_payload = excluded.structured_payload
                ",
                params![
                    record.id,
                    record.created_at_ms,
                    record.kind,
                    record.text,
                    record.confidence,
                    record.valid_from,
                    record.valid_to,
                    record.superseded_by,
                    record.sensitivity,
                    record.structured_payload
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_memory_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Memories;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = memory_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO memories (
                    id, created_at_ms, kind, text, confidence, valid_from, valid_to,
                    superseded_by, sensitivity, structured_payload
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(id) DO UPDATE SET
                    valid_to = COALESCE(memories.valid_to, excluded.valid_to),
                    superseded_by = COALESCE(memories.superseded_by, excluded.superseded_by)
                WHERE (memories.valid_to IS NULL AND excluded.valid_to IS NOT NULL)
                   OR (memories.superseded_by IS NULL AND excluded.superseded_by IS NOT NULL)
                ",
                params![
                    record.id,
                    record.created_at_ms,
                    record.kind,
                    record.text,
                    record.confidence,
                    record.valid_from,
                    record.valid_to,
                    record.superseded_by,
                    record.sensitivity,
                    record.structured_payload
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_memory_embedding_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::MemoryEmbeddings;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = memory_embedding_sync_record_from_value(value)?;
        if !memory_exists(conn, &record.memory_id).await? {
            stats.record_skip("missing_memory");
            continue;
        }
        let affected = conn
            .execute(
                "
                INSERT INTO memory_embeddings (memory_id, model, dimensions, embedding)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(memory_id) DO UPDATE SET
                    model = excluded.model,
                    dimensions = excluded.dimensions,
                    embedding = excluded.embedding
                ",
                params![
                    record.memory_id,
                    record.model,
                    record.dimensions,
                    record.embedding
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_memory_embedding_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::MemoryEmbeddings;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = memory_embedding_sync_record_from_value(value)?;
        if !memory_exists(conn, &record.memory_id).await? {
            stats.record_skip("missing_memory");
            continue;
        }
        let affected = conn
            .execute(
                "
                INSERT OR IGNORE INTO memory_embeddings (memory_id, model, dimensions, embedding)
                VALUES (?1, ?2, ?3, ?4)
                ",
                params![
                    record.memory_id,
                    record.model,
                    record.dimensions,
                    record.embedding
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_source_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Sources;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = source_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO sources (id, kind, locator, created_at_ms)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    locator = excluded.locator,
                    created_at_ms = excluded.created_at_ms
                ",
                params![record.id, record.kind, record.locator, record.created_at_ms],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_source_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Sources;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = source_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT OR IGNORE INTO sources (id, kind, locator, created_at_ms)
                VALUES (?1, ?2, ?3, ?4)
                ",
                params![record.id, record.kind, record.locator, record.created_at_ms],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_discovered_file_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::DiscoveredFiles;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = discovered_file_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO discovered_files (
                    project_id, path, language, size_bytes, discovered_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(project_id, path) DO UPDATE SET
                    language = excluded.language,
                    size_bytes = excluded.size_bytes,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![
                    record.project_id,
                    record.path,
                    record.language,
                    record.size_bytes,
                    record.discovered_at_ms,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_discovered_file_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::DiscoveredFiles;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = discovered_file_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO discovered_files (
                    project_id, path, language, size_bytes, discovered_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(project_id, path) DO UPDATE SET
                    language = excluded.language,
                    size_bytes = excluded.size_bytes,
                    updated_at_ms = excluded.updated_at_ms
                WHERE excluded.updated_at_ms > discovered_files.updated_at_ms
                ",
                params![
                    record.project_id,
                    record.path,
                    record.language,
                    record.size_bytes,
                    record.discovered_at_ms,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_entity_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Entities;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = entity_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO entities (id, kind, name, locator, created_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    name = excluded.name,
                    locator = excluded.locator,
                    created_at_ms = excluded.created_at_ms
                ",
                params![
                    record.id,
                    record.kind,
                    record.name,
                    record.locator,
                    record.created_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_entity_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Entities;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = entity_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT OR IGNORE INTO entities (id, kind, name, locator, created_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    record.id,
                    record.kind,
                    record.name,
                    record.locator,
                    record.created_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_code_symbol_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::CodeSymbols;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = code_symbol_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO code_symbols (
                    project_id, path, name, kind, language, line_start, line_end,
                    signature, indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(project_id, path, kind, name, line_start) DO UPDATE SET
                    language = excluded.language,
                    line_end = excluded.line_end,
                    signature = excluded.signature,
                    indexed_at_ms = excluded.indexed_at_ms
                ",
                params![
                    record.project_id,
                    record.path,
                    record.name,
                    record.kind,
                    record.language,
                    record.line_start,
                    record.line_end,
                    record.signature,
                    record.indexed_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_code_symbol_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::CodeSymbols;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = code_symbol_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO code_symbols (
                    project_id, path, name, kind, language, line_start, line_end,
                    signature, indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(project_id, path, kind, name, line_start) DO UPDATE SET
                    language = excluded.language,
                    line_end = excluded.line_end,
                    signature = excluded.signature,
                    indexed_at_ms = excluded.indexed_at_ms
                WHERE excluded.indexed_at_ms > code_symbols.indexed_at_ms
                ",
                params![
                    record.project_id,
                    record.path,
                    record.name,
                    record.kind,
                    record.language,
                    record.line_start,
                    record.line_end,
                    record.signature,
                    record.indexed_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_edge_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Edges;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = edge_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO edges (id, from_id, to_id, kind, created_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(id) DO UPDATE SET
                    from_id = excluded.from_id,
                    to_id = excluded.to_id,
                    kind = excluded.kind,
                    created_at_ms = excluded.created_at_ms
                ",
                params![
                    record.id,
                    record.from_id,
                    record.to_id,
                    record.kind,
                    record.created_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_edge_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Edges;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = edge_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT OR IGNORE INTO edges (id, from_id, to_id, kind, created_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    record.id,
                    record.from_id,
                    record.to_id,
                    record.kind,
                    record.created_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_code_reference_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::CodeReferences;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = code_reference_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO code_references (
                    project_id, path, target_path, target_name, target_kind, kind,
                    language, line_start, excerpt, indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(project_id, path, target_path, target_name, line_start, kind)
                DO UPDATE SET
                    target_kind = excluded.target_kind,
                    language = excluded.language,
                    excerpt = excluded.excerpt,
                    indexed_at_ms = excluded.indexed_at_ms
                ",
                params![
                    record.project_id,
                    record.path,
                    record.target_path,
                    record.target_name,
                    record.target_kind,
                    record.kind,
                    record.language,
                    record.line_start,
                    record.excerpt,
                    record.indexed_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_code_reference_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::CodeReferences;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = code_reference_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO code_references (
                    project_id, path, target_path, target_name, target_kind, kind,
                    language, line_start, excerpt, indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(project_id, path, target_path, target_name, line_start, kind)
                DO UPDATE SET
                    target_kind = excluded.target_kind,
                    language = excluded.language,
                    excerpt = excluded.excerpt,
                    indexed_at_ms = excluded.indexed_at_ms
                WHERE excluded.indexed_at_ms > code_references.indexed_at_ms
                ",
                params![
                    record.project_id,
                    record.path,
                    record.target_path,
                    record.target_name,
                    record.target_kind,
                    record.kind,
                    record.language,
                    record.line_start,
                    record.excerpt,
                    record.indexed_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_test_mapping_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::TestMappings;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = test_mapping_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO test_mappings (
                    project_id, source_path, test_path, reason, score, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(project_id, source_path, test_path) DO UPDATE SET
                    reason = excluded.reason,
                    score = excluded.score,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![
                    record.project_id,
                    record.source_path,
                    record.test_path,
                    record.reason,
                    record.score,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_test_mapping_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::TestMappings;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = test_mapping_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO test_mappings (
                    project_id, source_path, test_path, reason, score, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(project_id, source_path, test_path) DO UPDATE SET
                    reason = excluded.reason,
                    score = excluded.score,
                    updated_at_ms = excluded.updated_at_ms
                WHERE excluded.updated_at_ms > test_mappings.updated_at_ms
                ",
                params![
                    record.project_id,
                    record.source_path,
                    record.test_path,
                    record.reason,
                    record.score,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_source_embedding_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::SourceEmbeddings;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = source_embedding_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO source_embeddings (
                    project_id, path, model, dimensions, embedding, content_key, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(project_id, path) DO UPDATE SET
                    model = excluded.model,
                    dimensions = excluded.dimensions,
                    embedding = excluded.embedding,
                    content_key = excluded.content_key,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![
                    record.project_id,
                    record.path,
                    record.model,
                    record.dimensions,
                    record.embedding,
                    record.content_key,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_source_embedding_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::SourceEmbeddings;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = source_embedding_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO source_embeddings (
                    project_id, path, model, dimensions, embedding, content_key, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(project_id, path) DO UPDATE SET
                    model = excluded.model,
                    dimensions = excluded.dimensions,
                    embedding = excluded.embedding,
                    content_key = excluded.content_key,
                    updated_at_ms = excluded.updated_at_ms
                WHERE excluded.updated_at_ms > source_embeddings.updated_at_ms
                ",
                params![
                    record.project_id,
                    record.path,
                    record.model,
                    record.dimensions,
                    record.embedding,
                    record.content_key,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_session_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Sessions;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = session_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO sessions (
                    id, project_id, task, branch, started_at_ms, ended_at_ms, final_summary
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    project_id = excluded.project_id,
                    task = excluded.task,
                    branch = excluded.branch,
                    started_at_ms = excluded.started_at_ms,
                    ended_at_ms = excluded.ended_at_ms,
                    final_summary = excluded.final_summary
                ",
                params![
                    record.id,
                    record.project_id,
                    record.task,
                    record.branch,
                    record.started_at_ms,
                    record.ended_at_ms,
                    record.final_summary
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_session_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Sessions;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = session_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO sessions (
                    id, project_id, task, branch, started_at_ms, ended_at_ms, final_summary
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    project_id = excluded.project_id,
                    task = excluded.task,
                    branch = excluded.branch,
                    started_at_ms = excluded.started_at_ms,
                    ended_at_ms = excluded.ended_at_ms,
                    final_summary = excluded.final_summary
                WHERE sessions.ended_at_ms IS NULL
                   OR excluded.ended_at_ms > sessions.ended_at_ms
                ",
                params![
                    record.id,
                    record.project_id,
                    record.task,
                    record.branch,
                    record.started_at_ms,
                    record.ended_at_ms,
                    record.final_summary
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_context_pack_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::ContextPacks;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = context_pack_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO context_packs (
                    id, project_id, task, payload_json, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(id) DO UPDATE SET
                    project_id = excluded.project_id,
                    task = excluded.task,
                    payload_json = excluded.payload_json,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![
                    record.id,
                    record.project_id,
                    record.task,
                    record.payload_json,
                    record.created_at_ms,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_context_pack_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::ContextPacks;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = context_pack_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT INTO context_packs (
                    id, project_id, task, payload_json, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(id) DO UPDATE SET
                    project_id = excluded.project_id,
                    task = excluded.task,
                    payload_json = excluded.payload_json,
                    updated_at_ms = excluded.updated_at_ms
                WHERE excluded.updated_at_ms > context_packs.updated_at_ms
                ",
                params![
                    record.id,
                    record.project_id,
                    record.task,
                    record.payload_json,
                    record.created_at_ms,
                    record.updated_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_push_diagnostic_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Diagnostics;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = diagnostic_sync_record_from_value(value)?;
        insert_diagnostic_records(conn, std::slice::from_ref(&record)).await?;
        stats.record_affected(1)?;
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_diagnostic_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Diagnostics;
    let before_count = table_row_count(conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = diagnostic_sync_record_from_value(value)?;
        let affected = conn
            .execute(
                "
                INSERT OR IGNORE INTO diagnostics (
                    id, project_id, source, path, line_start, line_end, severity, code,
                    message, command, created_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ",
                params![
                    record.id,
                    record.project_id,
                    record.source,
                    record.path,
                    record.line_start,
                    record.line_end,
                    record.severity,
                    record.code,
                    record.message,
                    record.command,
                    record.created_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(conn, table, before_count, stats).await
}

async fn apply_api_pull_payloads(
    conn: &Connection,
    payloads: &[SyncApiTablePayload],
) -> Result<Vec<SyncTableResult>, String> {
    let mut results = Vec::new();
    for payload in payloads {
        match SyncTableKind::from_table_name(&payload.result.table) {
            Some(SyncTableKind::Projects) if !payload.records.is_empty() => {
                results.push(apply_api_pull_project_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::Memories) if !payload.records.is_empty() => {
                results.push(apply_api_pull_memory_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::MemoryEmbeddings) if !payload.records.is_empty() => {
                results
                    .push(apply_api_pull_memory_embedding_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::Sources) if !payload.records.is_empty() => {
                results.push(apply_api_pull_source_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::DiscoveredFiles) if !payload.records.is_empty() => {
                results.push(apply_api_pull_discovered_file_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::Entities) if !payload.records.is_empty() => {
                results.push(apply_api_pull_entity_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::CodeSymbols) if !payload.records.is_empty() => {
                results.push(apply_api_pull_code_symbol_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::Edges) if !payload.records.is_empty() => {
                results.push(apply_api_pull_edge_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::CodeReferences) if !payload.records.is_empty() => {
                results.push(apply_api_pull_code_reference_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::TestMappings) if !payload.records.is_empty() => {
                results.push(apply_api_pull_test_mapping_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::SourceEmbeddings) if !payload.records.is_empty() => {
                results
                    .push(apply_api_pull_source_embedding_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::Sessions) if !payload.records.is_empty() => {
                results.push(apply_api_pull_session_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::ContextPacks) if !payload.records.is_empty() => {
                results.push(apply_api_pull_context_pack_records(conn, &payload.records).await?);
            }
            Some(SyncTableKind::Diagnostics) if !payload.records.is_empty() => {
                results.push(apply_api_pull_diagnostic_records(conn, &payload.records).await?);
            }
            _ => results.push(payload.result.clone()),
        }
    }
    Ok(results)
}

async fn apply_api_storage_session_event_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let before_count = table_row_count(conn, "session_events").await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = session_event_sync_record_from_value(value)?;
        if !session_exists(conn, &record.session_id).await? {
            stats.record_skip("missing_session");
            continue;
        }
        let affected = conn
            .execute(
                "
                INSERT OR IGNORE INTO session_events (
                    id, session_id, kind, detail, created_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    record.id,
                    record.session_id,
                    record.kind,
                    record.detail,
                    record.created_at_ms
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_storage_table_result(
        conn,
        "session_summaries",
        "session_events",
        before_count,
        stats,
    )
    .await
}

async fn apply_api_storage_session_promotion_records(
    conn: &Connection,
    records: &[serde_json::Value],
) -> Result<SyncTableResult, String> {
    let before_count = table_row_count(conn, "session_promotions").await?;
    let mut stats = SyncApplyStats::default();

    for value in records {
        let record = session_promotion_sync_record_from_value(value)?;
        if !session_exists(conn, &record.session_id).await? {
            stats.record_skip("missing_session");
            continue;
        }
        if !memory_exists(conn, &record.memory_id).await? {
            stats.record_skip("missing_memory");
            continue;
        }
        let affected = conn
            .execute(
                "
                INSERT OR IGNORE INTO session_promotions (
                    session_id, memory_id, promoted_at_ms
                )
                VALUES (?1, ?2, ?3)
                ",
                params![record.session_id, record.memory_id, record.promoted_at_ms],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_storage_table_result(
        conn,
        "session_summaries",
        "session_promotions",
        before_count,
        stats,
    )
    .await
}

async fn delete_code_index_paths(conn: &Connection, paths: &[String]) -> Result<(), String> {
    let mut normalized_paths = paths
        .iter()
        .map(|path| normalize_target(path))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    normalized_paths.sort();
    normalized_paths.dedup();

    for path in normalized_paths {
        conn.execute(
            "
            DELETE FROM code_symbols
            WHERE project_id = ?1 AND path = ?2
            ",
            params![LOCAL_PROJECT_ID, path.clone()],
        )
        .await
        .map_err(|error| error.to_string())?;
        conn.execute(
            "
            DELETE FROM code_references
            WHERE project_id = ?1 AND path = ?2
            ",
            params![LOCAL_PROJECT_ID, path.clone()],
        )
        .await
        .map_err(|error| error.to_string())?;
        conn.execute(
            "
            DELETE FROM test_mappings
            WHERE project_id = ?1 AND (source_path = ?2 OR test_path = ?2)
            ",
            params![LOCAL_PROJECT_ID, path.clone()],
        )
        .await
        .map_err(|error| error.to_string())?;
        conn.execute(
            "
            DELETE FROM source_embeddings
            WHERE project_id = ?1 AND path = ?2
            ",
            params![LOCAL_PROJECT_ID, path],
        )
        .await
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[derive(Debug, Default)]
struct SyncApplyStats {
    affected_count: usize,
    skipped_count: usize,
    conflicts: Vec<SyncConflictSummary>,
}

impl SyncApplyStats {
    fn record_affected(&mut self, affected: u64) -> Result<(), String> {
        self.affected_count += usize::try_from(affected).map_err(|error| error.to_string())?;
        Ok(())
    }

    fn record_skip(&mut self, reason: &str) {
        self.skipped_count += 1;
        if let Some(existing) = self
            .conflicts
            .iter_mut()
            .find(|conflict| conflict.reason == reason)
        {
            existing.count += 1;
        } else {
            self.conflicts.push(SyncConflictSummary {
                reason: reason.to_string(),
                count: 1,
            });
        }
    }

    fn into_result(
        self,
        table: SyncTableKind,
        row_count: usize,
        before_count: usize,
        after_count: usize,
        executed: bool,
    ) -> SyncTableResult {
        let inserted_count = after_count.saturating_sub(before_count);
        let updated_count = self.affected_count.saturating_sub(inserted_count);
        let conflict_count = self.conflicts.iter().map(|conflict| conflict.count).sum();

        SyncTableResult {
            class: table.sync_class().to_string(),
            table: table.table_name().to_string(),
            row_count,
            inserted_count,
            updated_count,
            skipped_count: self.skipped_count,
            conflict_count,
            executed,
            conflicts: self.conflicts,
        }
    }
}

fn planned_sync_table_result(table: SyncTableKind, row_count: usize) -> SyncTableResult {
    SyncTableResult {
        class: table.sync_class().to_string(),
        table: table.table_name().to_string(),
        row_count,
        inserted_count: 0,
        updated_count: 0,
        skipped_count: 0,
        conflict_count: 0,
        executed: false,
        conflicts: Vec::new(),
    }
}

async fn table_row_count(conn: &Connection, table_name: &str) -> Result<usize, String> {
    let sql = format!("SELECT COUNT(*) FROM {table_name}");
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| error.to_string())?;
    let Some(row) = rows.next().await.map_err(|error| error.to_string())? else {
        return Ok(0);
    };
    let count = row.get::<i64>(0).map_err(|error| error.to_string())?;
    usize::try_from(count).map_err(|error| error.to_string())
}

async fn table_count_where(
    conn: &Connection,
    table_name: &str,
    predicate: &str,
) -> Result<usize, String> {
    let sql = format!("SELECT COUNT(*) FROM {table_name} WHERE {predicate}");
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| error.to_string())?;
    let Some(row) = rows.next().await.map_err(|error| error.to_string())? else {
        return Ok(0);
    };
    usize_from_i64(row.get::<i64>(0).map_err(|error| error.to_string())?)
}

async fn insert_memory(
    conn: &Connection,
    embedding_provider: &SelectedEmbeddingProvider,
    text: &str,
    confidence: f64,
    sensitivity: String,
    structured_payload: Option<String>,
) -> Result<Memory, String> {
    let created_at_ms = now_ms()?;
    let memory = Memory {
        id: format!("mem_{created_at_ms}"),
        created_at_ms,
        kind: "fact".to_string(),
        text: text.trim().to_string(),
        structured_payload,
    };

    conn.execute(
        "
        INSERT INTO memories (
            id,
            created_at_ms,
            kind,
            text,
            confidence,
            sensitivity,
            structured_payload
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ",
        params![
            memory.id.clone(),
            memory.created_at_ms,
            memory.kind.clone(),
            memory.text.clone(),
            confidence,
            sensitivity,
            memory.structured_payload.clone()
        ],
    )
    .await
    .map_err(|error| error.to_string())?;

    let embedding = embedding_provider.embed(&memory.text)?;
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

async fn active_memories(conn: &Connection) -> Result<Vec<Memory>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, created_at_ms, kind, text, structured_payload
            FROM memories
            WHERE valid_to IS NULL
            ORDER BY created_at_ms DESC
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut memories = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        memories.push(memory_from_row(&row)?);
    }

    Ok(memories)
}

fn memory_from_row(row: &Row) -> Result<Memory, String> {
    Ok(Memory {
        id: row.get::<String>(0).map_err(|error| error.to_string())?,
        created_at_ms: row.get::<i64>(1).map_err(|error| error.to_string())?,
        kind: row.get::<String>(2).map_err(|error| error.to_string())?,
        text: row.get::<String>(3).map_err(|error| error.to_string())?,
        structured_payload: row
            .get::<Option<String>>(4)
            .map_err(|error| error.to_string())?,
    })
}

fn normalized_memory_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn stale_memory_candidates(memories: &[Memory]) -> Vec<StaleMemoryCandidate> {
    let analyzed = memories
        .iter()
        .map(|memory| {
            (
                memory,
                meaningful_memory_terms(&memory.text),
                query_terms(&memory.text),
            )
        })
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();

    for left_index in 0..analyzed.len() {
        for right_index in (left_index + 1)..analyzed.len() {
            let (left, left_meaningful, left_terms) = &analyzed[left_index];
            let (right, right_meaningful, right_terms) = &analyzed[right_index];
            let Some(signal) = opposing_signal(left_terms, right_terms) else {
                continue;
            };
            let mut shared_terms = left_meaningful
                .iter()
                .filter(|term| right_meaningful.contains(*term))
                .cloned()
                .collect::<Vec<_>>();
            shared_terms.sort();
            shared_terms.dedup();
            if shared_terms.len() < 2 {
                continue;
            }

            let (newer_memory, older_memory) = if left.created_at_ms >= right.created_at_ms {
                ((**left).clone(), (**right).clone())
            } else {
                ((**right).clone(), (**left).clone())
            };

            candidates.push(StaleMemoryCandidate {
                reason: "opposing_terms".to_string(),
                signal,
                shared_terms,
                newer_memory,
                older_memory,
            });
        }
    }

    candidates.sort_by(|left, right| {
        right
            .newer_memory
            .created_at_ms
            .cmp(&left.newer_memory.created_at_ms)
            .then_with(|| {
                right
                    .older_memory
                    .created_at_ms
                    .cmp(&left.older_memory.created_at_ms)
            })
            .then_with(|| left.signal.cmp(&right.signal))
    });
    candidates
}

fn meaningful_memory_terms(text: &str) -> HashSet<String> {
    query_terms(text)
        .into_iter()
        .filter(|term| !is_memory_stop_term(term) && !is_opposing_signal_term(term))
        .collect()
}

fn is_memory_stop_term(term: &str) -> bool {
    matches!(
        term,
        "are"
            | "but"
            | "can"
            | "for"
            | "had"
            | "has"
            | "have"
            | "now"
            | "not"
            | "should"
            | "that"
            | "the"
            | "then"
            | "this"
            | "was"
            | "were"
            | "when"
            | "will"
            | "with"
    )
}

fn is_opposing_signal_term(term: &str) -> bool {
    OPPOSING_MEMORY_SIGNALS
        .iter()
        .any(|(left, right, _)| term == *left || term == *right)
}

fn opposing_signal(left_terms: &[String], right_terms: &[String]) -> Option<String> {
    for (left, right, signal) in OPPOSING_MEMORY_SIGNALS {
        let left_has_left = left_terms.iter().any(|term| term == left);
        let left_has_right = left_terms.iter().any(|term| term == right);
        let right_has_left = right_terms.iter().any(|term| term == left);
        let right_has_right = right_terms.iter().any(|term| term == right);
        if (left_has_left && right_has_right) || (left_has_right && right_has_left) {
            return Some((*signal).to_string());
        }
    }
    None
}

const OPPOSING_MEMORY_SIGNALS: &[(&str, &str, &str)] = &[
    ("after", "before", "after_vs_before"),
    ("allow", "deny", "allow_vs_deny"),
    ("allowed", "denied", "allowed_vs_denied"),
    ("enabled", "disabled", "enabled_vs_disabled"),
    ("enable", "disable", "enable_vs_disable"),
    ("present", "absent", "present_vs_absent"),
    ("true", "false", "true_vs_false"),
];

async fn finish_sync_table_result(
    conn: &Connection,
    table: SyncTableKind,
    before_count: usize,
    stats: SyncApplyStats,
) -> Result<SyncTableResult, String> {
    let after_count = table_row_count(conn, table.table_name()).await?;
    Ok(stats.into_result(table, after_count, before_count, after_count, true))
}

async fn finish_storage_table_result(
    conn: &Connection,
    class: &str,
    table: &str,
    before_count: usize,
    stats: SyncApplyStats,
) -> Result<SyncTableResult, String> {
    let after_count = table_row_count(conn, table).await?;
    let inserted_count = after_count.saturating_sub(before_count);
    let updated_count = stats.affected_count.saturating_sub(inserted_count);
    let conflict_count = stats.conflicts.iter().map(|conflict| conflict.count).sum();
    Ok(SyncTableResult {
        class: class.to_string(),
        table: table.to_string(),
        row_count: after_count,
        inserted_count,
        updated_count,
        skipped_count: stats.skipped_count,
        conflict_count,
        executed: true,
        conflicts: stats.conflicts,
    })
}

async fn sync_run_tables(conn: &Connection, run_id: &str) -> Result<Vec<SyncTableResult>, String> {
    let mut rows = conn
        .query(
            "
            SELECT
                class, table_name, row_count, inserted_count, updated_count,
                skipped_count, conflict_count, executed
            FROM sync_table_runs
            WHERE sync_run_id = ?1
            ORDER BY rowid
            ",
            params![run_id.to_string()],
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut tables = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let table = row.get::<String>(1).map_err(|error| error.to_string())?;
        tables.push(SyncTableResult {
            class: row.get::<String>(0).map_err(|error| error.to_string())?,
            conflicts: sync_table_conflicts(conn, run_id, &table).await?,
            table,
            row_count: usize_from_i64(row.get::<i64>(2).map_err(|error| error.to_string())?)?,
            inserted_count: usize_from_i64(row.get::<i64>(3).map_err(|error| error.to_string())?)?,
            updated_count: usize_from_i64(row.get::<i64>(4).map_err(|error| error.to_string())?)?,
            skipped_count: usize_from_i64(row.get::<i64>(5).map_err(|error| error.to_string())?)?,
            conflict_count: usize_from_i64(row.get::<i64>(6).map_err(|error| error.to_string())?)?,
            executed: row.get::<i64>(7).map_err(|error| error.to_string())? != 0,
        });
    }

    Ok(tables)
}

async fn sync_table_conflicts(
    conn: &Connection,
    run_id: &str,
    table: &str,
) -> Result<Vec<SyncConflictSummary>, String> {
    let mut rows = conn
        .query(
            "
            SELECT reason, count
            FROM sync_table_conflicts
            WHERE sync_run_id = ?1 AND table_name = ?2
            ORDER BY reason
            ",
            params![run_id.to_string(), table.to_string()],
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut conflicts = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        conflicts.push(SyncConflictSummary {
            reason: row.get::<String>(0).map_err(|error| error.to_string())?,
            count: usize_from_i64(row.get::<i64>(1).map_err(|error| error.to_string())?)?,
        });
    }

    Ok(conflicts)
}

fn usize_from_i64(value: i64) -> Result<usize, String> {
    usize::try_from(value).map_err(|error| error.to_string())
}

async fn memory_exists(conn: &Connection, memory_id: &str) -> Result<bool, String> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM memories WHERE id = ?1 LIMIT 1",
            params![memory_id.to_string()],
        )
        .await
        .map_err(|error| error.to_string())?;

    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| error.to_string())
}

async fn session_exists(conn: &Connection, session_id: &str) -> Result<bool, String> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sessions WHERE id = ?1 LIMIT 1",
            params![session_id.to_string()],
        )
        .await
        .map_err(|error| error.to_string())?;

    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| error.to_string())
}

async fn copy_sync_table(
    local_conn: &Connection,
    remote_conn: &Connection,
    table: SyncTableKind,
) -> Result<SyncTableResult, String> {
    match table {
        SyncTableKind::Projects => copy_projects(local_conn, remote_conn).await,
        SyncTableKind::Memories => copy_memories(local_conn, remote_conn).await,
        SyncTableKind::MemoryEmbeddings => copy_memory_embeddings(local_conn, remote_conn).await,
        SyncTableKind::Sources => copy_sources(local_conn, remote_conn).await,
        SyncTableKind::DiscoveredFiles => copy_discovered_files(local_conn, remote_conn).await,
        SyncTableKind::Entities => copy_entities(local_conn, remote_conn).await,
        SyncTableKind::CodeSymbols => copy_code_symbols(local_conn, remote_conn).await,
        SyncTableKind::Edges => copy_edges(local_conn, remote_conn).await,
        SyncTableKind::CodeReferences => copy_code_references(local_conn, remote_conn).await,
        SyncTableKind::TestMappings => copy_test_mappings(local_conn, remote_conn).await,
        SyncTableKind::SourceEmbeddings => copy_source_embeddings(local_conn, remote_conn).await,
        SyncTableKind::Sessions => copy_sessions(local_conn, remote_conn).await,
        SyncTableKind::ContextPacks => copy_context_packs(local_conn, remote_conn).await,
        SyncTableKind::Diagnostics => copy_diagnostics(local_conn, remote_conn).await,
    }
}

async fn copy_pull_table(
    remote_conn: &Connection,
    local_conn: &Connection,
    table: SyncTableKind,
) -> Result<SyncTableResult, String> {
    match table {
        SyncTableKind::Projects => pull_projects(remote_conn, local_conn).await,
        SyncTableKind::Memories => pull_memories(remote_conn, local_conn).await,
        SyncTableKind::MemoryEmbeddings => pull_memory_embeddings(remote_conn, local_conn).await,
        SyncTableKind::Sources => pull_sources(remote_conn, local_conn).await,
        SyncTableKind::DiscoveredFiles => pull_discovered_files(remote_conn, local_conn).await,
        SyncTableKind::Entities => pull_entities(remote_conn, local_conn).await,
        SyncTableKind::CodeSymbols => pull_code_symbols(remote_conn, local_conn).await,
        SyncTableKind::Edges => pull_edges(remote_conn, local_conn).await,
        SyncTableKind::CodeReferences => pull_code_references(remote_conn, local_conn).await,
        SyncTableKind::TestMappings => pull_test_mappings(remote_conn, local_conn).await,
        SyncTableKind::SourceEmbeddings => pull_source_embeddings(remote_conn, local_conn).await,
        SyncTableKind::Sessions => pull_sessions(remote_conn, local_conn).await,
        SyncTableKind::ContextPacks => pull_context_packs(remote_conn, local_conn).await,
        SyncTableKind::Diagnostics => pull_diagnostics(remote_conn, local_conn).await,
    }
}

async fn copy_projects(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Projects;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "
            SELECT id, name, root_path, git_remote, default_branch, created_at_ms, updated_at_ms
            FROM projects
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO projects (
                    id, name, root_path, git_remote, default_branch, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    root_path = excluded.root_path,
                    git_remote = excluded.git_remote,
                    default_branch = excluded.default_branch,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(3)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(4)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(5).map_err(|error| error.to_string())?,
                    row.get::<i64>(6).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_memories(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Memories;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "
            SELECT
                id, created_at_ms, kind, text, confidence, valid_from, valid_to,
                superseded_by, sensitivity, structured_payload
            FROM memories
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO memories (
                    id, created_at_ms, kind, text, confidence, valid_from, valid_to,
                    superseded_by, sensitivity, structured_payload
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(id) DO UPDATE SET
                    created_at_ms = excluded.created_at_ms,
                    kind = excluded.kind,
                    text = excluded.text,
                    confidence = excluded.confidence,
                    valid_from = excluded.valid_from,
                    valid_to = excluded.valid_to,
                    superseded_by = excluded.superseded_by,
                    sensitivity = excluded.sensitivity,
                    structured_payload = excluded.structured_payload
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<i64>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<f64>(4).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(5)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(6)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(7)
                        .map_err(|error| error.to_string())?,
                    row.get::<String>(8).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(9)
                        .map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_memory_embeddings(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::MemoryEmbeddings;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "SELECT memory_id, model, dimensions, embedding FROM memory_embeddings",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let memory_id = row.get::<String>(0).map_err(|error| error.to_string())?;
        if !memory_exists(remote_conn, &memory_id).await? {
            stats.record_skip("missing_memory");
            continue;
        }
        let affected = remote_conn
            .execute(
                "
                INSERT INTO memory_embeddings (memory_id, model, dimensions, embedding)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(memory_id) DO UPDATE SET
                    model = excluded.model,
                    dimensions = excluded.dimensions,
                    embedding = excluded.embedding
                ",
                params![
                    memory_id,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<i64>(2).map_err(|error| error.to_string())?,
                    row.get::<Vec<u8>>(3).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_sources(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Sources;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query("SELECT id, kind, locator, created_at_ms FROM sources", ())
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO sources (id, kind, locator, created_at_ms)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    locator = excluded.locator,
                    created_at_ms = excluded.created_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<i64>(3).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_discovered_files(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::DiscoveredFiles;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "
            SELECT project_id, path, language, size_bytes, discovered_at_ms, updated_at_ms
            FROM discovered_files
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO discovered_files (
                    project_id, path, language, size_bytes, discovered_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(project_id, path) DO UPDATE SET
                    language = excluded.language,
                    size_bytes = excluded.size_bytes,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(2)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<i64>>(3)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                    row.get::<i64>(5).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_entities(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Entities;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "SELECT id, kind, name, locator, created_at_ms FROM entities",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO entities (id, kind, name, locator, created_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    name = excluded.name,
                    locator = excluded.locator,
                    created_at_ms = excluded.created_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(3)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_code_symbols(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::CodeSymbols;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "
            SELECT
                project_id, path, name, kind, language, line_start, line_end,
                signature, indexed_at_ms
            FROM code_symbols
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO code_symbols (
                    project_id, path, name, kind, language, line_start, line_end,
                    signature, indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(project_id, path, kind, name, line_start) DO UPDATE SET
                    language = excluded.language,
                    line_end = excluded.line_end,
                    signature = excluded.signature,
                    indexed_at_ms = excluded.indexed_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(4)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(5).map_err(|error| error.to_string())?,
                    row.get::<Option<i64>>(6)
                        .map_err(|error| error.to_string())?,
                    row.get::<String>(7).map_err(|error| error.to_string())?,
                    row.get::<i64>(8).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_edges(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Edges;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "SELECT id, from_id, to_id, kind, created_at_ms FROM edges",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO edges (id, from_id, to_id, kind, created_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(id) DO UPDATE SET
                    from_id = excluded.from_id,
                    to_id = excluded.to_id,
                    kind = excluded.kind,
                    created_at_ms = excluded.created_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_code_references(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::CodeReferences;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "
            SELECT
                project_id, path, target_path, target_name, target_kind, kind,
                language, line_start, excerpt, indexed_at_ms
            FROM code_references
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO code_references (
                    project_id, path, target_path, target_name, target_kind, kind,
                    language, line_start, excerpt, indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(project_id, path, target_path, target_name, line_start, kind)
                DO UPDATE SET
                    target_kind = excluded.target_kind,
                    language = excluded.language,
                    excerpt = excluded.excerpt,
                    indexed_at_ms = excluded.indexed_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<String>(4).map_err(|error| error.to_string())?,
                    row.get::<String>(5).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(6)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(7).map_err(|error| error.to_string())?,
                    row.get::<String>(8).map_err(|error| error.to_string())?,
                    row.get::<i64>(9).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_test_mappings(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let records = test_mapping_sync_records(local_conn).await?;
    apply_api_push_test_mapping_records(remote_conn, &records).await
}

async fn copy_source_embeddings(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let records = source_embedding_sync_records(local_conn).await?;
    apply_api_push_source_embedding_records(remote_conn, &records).await
}

async fn copy_sessions(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Sessions;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "
            SELECT id, project_id, task, branch, started_at_ms, ended_at_ms, final_summary
            FROM sessions
            WHERE final_summary IS NOT NULL
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO sessions (
                    id, project_id, task, branch, started_at_ms, ended_at_ms, final_summary
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    project_id = excluded.project_id,
                    task = excluded.task,
                    branch = excluded.branch,
                    started_at_ms = excluded.started_at_ms,
                    ended_at_ms = excluded.ended_at_ms,
                    final_summary = excluded.final_summary
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(3)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                    row.get::<Option<i64>>(5)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(6)
                        .map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_context_packs(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::ContextPacks;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = local_conn
        .query(
            "
            SELECT id, project_id, task, payload_json, created_at_ms, updated_at_ms
            FROM context_packs
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = remote_conn
            .execute(
                "
                INSERT INTO context_packs (
                    id, project_id, task, payload_json, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(id) DO UPDATE SET
                    project_id = excluded.project_id,
                    task = excluded.task,
                    payload_json = excluded.payload_json,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                    row.get::<i64>(5).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        stats.record_affected(affected)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn copy_diagnostics(
    local_conn: &Connection,
    remote_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Diagnostics;
    let before_count = table_row_count(remote_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let records = local_diagnostic_sync_records(local_conn).await?;

    for record in records {
        insert_diagnostic_records(remote_conn, std::slice::from_ref(&record)).await?;
        stats.record_affected(1)?;
    }

    finish_sync_table_result(remote_conn, table, before_count, stats).await
}

async fn pull_projects(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Projects;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "
            SELECT id, name, root_path, git_remote, default_branch, created_at_ms, updated_at_ms
            FROM projects
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT INTO projects (
                    id, name, root_path, git_remote, default_branch, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    root_path = excluded.root_path,
                    git_remote = excluded.git_remote,
                    default_branch = excluded.default_branch,
                    updated_at_ms = excluded.updated_at_ms
                WHERE excluded.updated_at_ms > projects.updated_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(3)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(4)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(5).map_err(|error| error.to_string())?,
                    row.get::<i64>(6).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_memories(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Memories;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "
            SELECT
                id, created_at_ms, kind, text, confidence, valid_from, valid_to,
                superseded_by, sensitivity, structured_payload
            FROM memories
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT INTO memories (
                    id, created_at_ms, kind, text, confidence, valid_from, valid_to,
                    superseded_by, sensitivity, structured_payload
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(id) DO UPDATE SET
                    valid_to = COALESCE(memories.valid_to, excluded.valid_to),
                    superseded_by = COALESCE(memories.superseded_by, excluded.superseded_by)
                WHERE (memories.valid_to IS NULL AND excluded.valid_to IS NOT NULL)
                   OR (memories.superseded_by IS NULL AND excluded.superseded_by IS NOT NULL)
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<i64>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<f64>(4).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(5)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(6)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(7)
                        .map_err(|error| error.to_string())?,
                    row.get::<String>(8).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(9)
                        .map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_memory_embeddings(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::MemoryEmbeddings;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "SELECT memory_id, model, dimensions, embedding FROM memory_embeddings",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let memory_id = row.get::<String>(0).map_err(|error| error.to_string())?;
        if !memory_exists(local_conn, &memory_id).await? {
            stats.record_skip("missing_memory");
            continue;
        }
        let affected = local_conn
            .execute(
                "
                INSERT OR IGNORE INTO memory_embeddings (memory_id, model, dimensions, embedding)
                VALUES (?1, ?2, ?3, ?4)
                ",
                params![
                    memory_id,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<i64>(2).map_err(|error| error.to_string())?,
                    row.get::<Vec<u8>>(3).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_sources(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Sources;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query("SELECT id, kind, locator, created_at_ms FROM sources", ())
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT OR IGNORE INTO sources (id, kind, locator, created_at_ms)
                VALUES (?1, ?2, ?3, ?4)
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<i64>(3).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_discovered_files(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::DiscoveredFiles;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "
            SELECT project_id, path, language, size_bytes, discovered_at_ms, updated_at_ms
            FROM discovered_files
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT INTO discovered_files (
                    project_id, path, language, size_bytes, discovered_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(project_id, path) DO UPDATE SET
                    language = excluded.language,
                    size_bytes = excluded.size_bytes,
                    updated_at_ms = excluded.updated_at_ms
                WHERE excluded.updated_at_ms > discovered_files.updated_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(2)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<i64>>(3)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                    row.get::<i64>(5).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_entities(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Entities;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "SELECT id, kind, name, locator, created_at_ms FROM entities",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT OR IGNORE INTO entities (id, kind, name, locator, created_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(3)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_code_symbols(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::CodeSymbols;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "
            SELECT
                project_id, path, name, kind, language, line_start, line_end,
                signature, indexed_at_ms
            FROM code_symbols
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT INTO code_symbols (
                    project_id, path, name, kind, language, line_start, line_end,
                    signature, indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(project_id, path, kind, name, line_start) DO UPDATE SET
                    language = excluded.language,
                    line_end = excluded.line_end,
                    signature = excluded.signature,
                    indexed_at_ms = excluded.indexed_at_ms
                WHERE excluded.indexed_at_ms > code_symbols.indexed_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(4)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(5).map_err(|error| error.to_string())?,
                    row.get::<Option<i64>>(6)
                        .map_err(|error| error.to_string())?,
                    row.get::<String>(7).map_err(|error| error.to_string())?,
                    row.get::<i64>(8).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_edges(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Edges;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "SELECT id, from_id, to_id, kind, created_at_ms FROM edges",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT OR IGNORE INTO edges (id, from_id, to_id, kind, created_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_preserved");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_code_references(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::CodeReferences;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "
            SELECT
                project_id, path, target_path, target_name, target_kind, kind,
                language, line_start, excerpt, indexed_at_ms
            FROM code_references
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT INTO code_references (
                    project_id, path, target_path, target_name, target_kind, kind,
                    language, line_start, excerpt, indexed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(project_id, path, target_path, target_name, line_start, kind)
                DO UPDATE SET
                    target_kind = excluded.target_kind,
                    language = excluded.language,
                    excerpt = excluded.excerpt,
                    indexed_at_ms = excluded.indexed_at_ms
                WHERE excluded.indexed_at_ms > code_references.indexed_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<String>(4).map_err(|error| error.to_string())?,
                    row.get::<String>(5).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(6)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(7).map_err(|error| error.to_string())?,
                    row.get::<String>(8).map_err(|error| error.to_string())?,
                    row.get::<i64>(9).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_test_mappings(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let records = test_mapping_sync_records(remote_conn).await?;
    apply_api_pull_test_mapping_records(local_conn, &records).await
}

async fn pull_source_embeddings(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let records = source_embedding_sync_records(remote_conn).await?;
    apply_api_pull_source_embedding_records(local_conn, &records).await
}

async fn pull_sessions(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::Sessions;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "
            SELECT id, project_id, task, branch, started_at_ms, ended_at_ms, final_summary
            FROM sessions
            WHERE final_summary IS NOT NULL
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT INTO sessions (
                    id, project_id, task, branch, started_at_ms, ended_at_ms, final_summary
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    project_id = excluded.project_id,
                    task = excluded.task,
                    branch = excluded.branch,
                    started_at_ms = excluded.started_at_ms,
                    ended_at_ms = excluded.ended_at_ms,
                    final_summary = excluded.final_summary
                WHERE sessions.ended_at_ms IS NULL
                   OR excluded.ended_at_ms > sessions.ended_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(3)
                        .map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                    row.get::<Option<i64>>(5)
                        .map_err(|error| error.to_string())?,
                    row.get::<Option<String>>(6)
                        .map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_context_packs(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let table = SyncTableKind::ContextPacks;
    let before_count = table_row_count(local_conn, table.table_name()).await?;
    let mut stats = SyncApplyStats::default();
    let mut rows = remote_conn
        .query(
            "
            SELECT id, project_id, task, payload_json, created_at_ms, updated_at_ms
            FROM context_packs
            ",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let affected = local_conn
            .execute(
                "
                INSERT INTO context_packs (
                    id, project_id, task, payload_json, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(id) DO UPDATE SET
                    project_id = excluded.project_id,
                    task = excluded.task,
                    payload_json = excluded.payload_json,
                    updated_at_ms = excluded.updated_at_ms
                WHERE excluded.updated_at_ms > context_packs.updated_at_ms
                ",
                params![
                    row.get::<String>(0).map_err(|error| error.to_string())?,
                    row.get::<String>(1).map_err(|error| error.to_string())?,
                    row.get::<String>(2).map_err(|error| error.to_string())?,
                    row.get::<String>(3).map_err(|error| error.to_string())?,
                    row.get::<i64>(4).map_err(|error| error.to_string())?,
                    row.get::<i64>(5).map_err(|error| error.to_string())?,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if affected == 0 {
            stats.record_skip("local_row_newer_or_equal");
        } else {
            stats.record_affected(affected)?;
        }
    }

    finish_sync_table_result(local_conn, table, before_count, stats).await
}

async fn pull_diagnostics(
    remote_conn: &Connection,
    local_conn: &Connection,
) -> Result<SyncTableResult, String> {
    let records = diagnostic_sync_records(remote_conn).await?;
    let values = records;
    apply_api_pull_diagnostic_records(local_conn, &values).await
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

impl SyncBackend {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "direct" | "direct_libsql" | "libsql" | "turso" => Ok(Self::DirectLibsql),
            "api" | "hugr_api" => Ok(Self::HugrApi),
            unknown => Err(format!(
                "invalid HUGR_SYNC_BACKEND '{unknown}', expected direct_libsql or hugr_api"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::DirectLibsql => "direct_libsql",
            Self::HugrApi => "hugr_api",
        }
    }
}

impl StorageConfig {
    #[cfg(test)]
    fn local() -> Self {
        Self {
            mode: StorageMode::Local,
            backend: SyncBackend::DirectLibsql,
            remote_url: None,
            remote_auth_token: None,
            auth_token_configured: false,
            sync_classes: default_sync_classes(),
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
        let backend = lookup_first(&lookup, &["HUGR_SYNC_BACKEND"])
            .map(|value| SyncBackend::parse(&value))
            .transpose()?
            .unwrap_or(SyncBackend::DirectLibsql);
        let remote_url = lookup_first(
            &lookup,
            &[
                "HUGR_REMOTE_DATABASE_URL",
                "HUGR_API_URL",
                "HUGR_LIBSQL_URL",
                "TURSO_DATABASE_URL",
                "LIBSQL_URL",
            ],
        );
        let remote_auth_token = lookup_first(
            &lookup,
            &[
                "HUGR_REMOTE_AUTH_TOKEN",
                "HUGR_API_TOKEN",
                "HUGR_LIBSQL_AUTH_TOKEN",
                "TURSO_AUTH_TOKEN",
                "LIBSQL_AUTH_TOKEN",
            ],
        );
        let auth_token_configured = remote_auth_token.is_some();
        let sync_classes = lookup_first(&lookup, &["HUGR_SYNC_CLASSES"])
            .map(|value| parse_sync_classes(&value))
            .transpose()?
            .unwrap_or_else(default_sync_classes);

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
            backend,
            remote_url,
            remote_auth_token,
            auth_token_configured,
            sync_classes,
        })
    }

    fn summary(&self) -> String {
        let auth_status = if self.auth_token_configured {
            "auth configured"
        } else {
            "auth missing"
        };
        let sync_classes = format_sync_classes(&self.sync_classes);
        match self.mode {
            StorageMode::Local if self.remote_url.is_some() => {
                format!(
                    "local (remote configured, inactive, backend: {}, {auth_status}, sync classes: {sync_classes})",
                    self.backend.as_str()
                )
            }
            StorageMode::Local => "local".to_string(),
            StorageMode::Hybrid if self.backend == SyncBackend::DirectLibsql => format!(
                "hybrid (local active, guarded remote sync enabled, backend: {}, {auth_status}, sync classes: {sync_classes})",
                self.backend.as_str()
            ),
            StorageMode::Hybrid => format!(
                "hybrid (local active, Hugr API sync transport configured, backend: {}, {auth_status}, sync classes: {sync_classes})",
                self.backend.as_str()
            ),
            StorageMode::Remote => format!(
                "remote (backend: {}, {auth_status}, sync classes: {sync_classes})",
                self.backend.as_str()
            ),
        }
    }

    fn sync_execution_plan(&self) -> SyncExecutionPlan {
        let remote_configured = self.remote_url.is_some();
        let direct_hybrid_sync_ready = matches!(self.mode, StorageMode::Hybrid)
            && self.backend == SyncBackend::DirectLibsql
            && remote_configured
            && self.auth_token_configured;
        let direct_remote_ready = matches!(self.mode, StorageMode::Remote)
            && self.backend == SyncBackend::DirectLibsql
            && remote_configured
            && self.auth_token_configured;
        let hugr_api_transport_ready =
            matches!(self.mode, StorageMode::Hybrid | StorageMode::Remote)
                && self.backend == SyncBackend::HugrApi
                && remote_configured
                && self.auth_token_configured;
        let status = match self.mode {
            StorageMode::Local => "local_only",
            StorageMode::Hybrid if direct_hybrid_sync_ready => "remote_sync_ready",
            StorageMode::Hybrid if hugr_api_transport_ready => "hugr_api_transport_ready",
            StorageMode::Hybrid => "remote_sync_backend_pending",
            StorageMode::Remote if direct_remote_ready => "remote_storage_ready",
            StorageMode::Remote if hugr_api_transport_ready => "hugr_api_transport_ready",
            StorageMode::Remote => "remote_storage_pending",
        };

        SyncExecutionPlan {
            storage_mode: self.mode.as_str().to_string(),
            backend: self.backend.as_str().to_string(),
            local_writes_enabled: !matches!(self.mode, StorageMode::Remote),
            remote_configured,
            remote_auth_configured: self.auth_token_configured,
            remote_reads_enabled: direct_hybrid_sync_ready
                || direct_remote_ready
                || hugr_api_transport_ready,
            remote_writes_enabled: direct_hybrid_sync_ready
                || direct_remote_ready
                || hugr_api_transport_ready,
            remote_endpoint: self.remote_url.clone(),
            api_contract_version: (self.backend == SyncBackend::HugrApi)
                .then(|| HUGR_API_CONTRACT_VERSION.to_string()),
            api_routes: if self.backend == SyncBackend::HugrApi {
                HUGR_API_ROUTES
                    .iter()
                    .map(|route| route.to_string())
                    .collect()
            } else {
                Vec::new()
            },
            sync_classes: self
                .sync_classes
                .iter()
                .map(|class| class.as_str().to_string())
                .collect(),
            explicit_opt_in_classes: self
                .sync_classes
                .iter()
                .copied()
                .filter(|class| class.requires_explicit_opt_in())
                .map(|class| class.as_str().to_string())
                .collect(),
            status: status.to_string(),
        }
    }
}

impl SyncClass {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "memories" => Ok(Self::Memories),
            "sources" => Ok(Self::Sources),
            "entities" => Ok(Self::Entities),
            "edges" => Ok(Self::Edges),
            "embeddings" => Ok(Self::Embeddings),
            "context_packs" => Ok(Self::ContextPacks),
            "diagnostics" => Ok(Self::Diagnostics),
            "session_summaries" => Ok(Self::SessionSummaries),
            "full_source" => Ok(Self::FullSource),
            "raw_command_output" => Ok(Self::RawCommandOutput),
            "secrets" => Ok(Self::Secrets),
            "private_notes" => Ok(Self::PrivateNotes),
            unknown => Err(format!("invalid HUGR_SYNC_CLASSES value '{unknown}'")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Memories => "memories",
            Self::Sources => "sources",
            Self::Entities => "entities",
            Self::Edges => "edges",
            Self::Embeddings => "embeddings",
            Self::ContextPacks => "context_packs",
            Self::Diagnostics => "diagnostics",
            Self::SessionSummaries => "session_summaries",
            Self::FullSource => "full_source",
            Self::RawCommandOutput => "raw_command_output",
            Self::Secrets => "secrets",
            Self::PrivateNotes => "private_notes",
        }
    }

    fn requires_explicit_opt_in(self) -> bool {
        matches!(
            self,
            Self::FullSource | Self::RawCommandOutput | Self::Secrets | Self::PrivateNotes
        )
    }
}

fn default_sync_classes() -> Vec<SyncClass> {
    vec![
        SyncClass::Memories,
        SyncClass::Sources,
        SyncClass::Entities,
        SyncClass::Edges,
        SyncClass::Embeddings,
        SyncClass::ContextPacks,
        SyncClass::Diagnostics,
        SyncClass::SessionSummaries,
    ]
}

fn parse_sync_classes(value: &str) -> Result<Vec<SyncClass>, String> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized == "default" {
        return Ok(default_sync_classes());
    }
    if normalized == "none" {
        return Ok(Vec::new());
    }

    let mut classes = Vec::new();
    for raw in value.split(',') {
        let class = SyncClass::parse(raw)?;
        if !classes.contains(&class) {
            classes.push(class);
        }
    }
    Ok(classes)
}

fn format_sync_classes(classes: &[SyncClass]) -> String {
    if classes.is_empty() {
        return "none".to_string();
    }

    classes
        .iter()
        .map(|class| class.as_str())
        .collect::<Vec<_>>()
        .join(",")
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

async fn insert_context_pack(
    conn: &Connection,
    task: &str,
    payload_json: &str,
) -> Result<String, String> {
    let now = now_ms()?;
    let id = format!("ctx_{now}");
    conn.execute(
        "
        INSERT INTO context_packs (
            id, project_id, task, payload_json, created_at_ms, updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?5)
        ON CONFLICT(id) DO UPDATE SET
            task = excluded.task,
            payload_json = excluded.payload_json,
            updated_at_ms = excluded.updated_at_ms
        ",
        params![
            id.clone(),
            LOCAL_PROJECT_ID,
            task.trim().to_string(),
            payload_json.to_string(),
            now
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(id)
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

async fn insert_code_symbol(
    conn: &Connection,
    symbol: &CodeSymbol,
    now: i64,
) -> Result<(), String> {
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
    Ok(())
}

async fn insert_code_reference(
    conn: &Connection,
    reference: &CodeReference,
    now: i64,
) -> Result<(), String> {
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
    Ok(())
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
    let Some(session_id) = active_session_id_optional(conn).await? else {
        return Err("no active session; run `hugr session start <task>` first".to_string());
    };

    Ok(session_id)
}

async fn active_session_id_optional(conn: &Connection) -> Result<Option<String>, String> {
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
        return Ok(None);
    };

    row.get::<String>(0)
        .map(Some)
        .map_err(|error| error.to_string())
}

async fn insert_session_event(
    conn: &Connection,
    session_id: String,
    kind: &str,
    detail: &str,
) -> Result<SessionEvent, String> {
    let created_at_ms = now_ms()?;
    let event = SessionEvent {
        id: session_event_id(created_at_ms),
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

fn session_event_id(created_at_ms: i64) -> String {
    let sequence = SESSION_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("evt_{created_at_ms}_{sequence}")
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

async fn latest_session(conn: &Connection) -> Result<Option<Session>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, task, branch, started_at_ms, ended_at_ms, final_summary
            FROM sessions
            WHERE project_id = ?1
            ORDER BY COALESCE(ended_at_ms, started_at_ms) DESC
            LIMIT 1
            ",
            params![LOCAL_PROJECT_ID],
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

async fn next_unpromoted_ended_session(conn: &Connection) -> Result<Option<Session>, String> {
    let mut rows = conn
        .query(
            "
            SELECT id, task, branch, started_at_ms, ended_at_ms, final_summary
            FROM sessions AS s
            WHERE s.project_id = ?1
              AND s.ended_at_ms IS NOT NULL
              AND NOT EXISTS (
                  SELECT 1
                  FROM session_promotions AS p
                  WHERE p.session_id = s.id
              )
              AND (
                  TRIM(COALESCE(s.final_summary, '')) <> ''
                  OR EXISTS (
                      SELECT 1
                      FROM session_events AS e
                      WHERE e.session_id = s.id
                  )
              )
            ORDER BY s.ended_at_ms ASC, s.started_at_ms ASC
            LIMIT 1
            ",
            params![LOCAL_PROJECT_ID],
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

async fn session_events(
    conn: &Connection,
    session_id: &str,
    limit: usize,
) -> Result<Vec<SessionFact>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let limit = i64::try_from(limit).map_err(|error| error.to_string())?;
    let mut rows = conn
        .query(
            "
            SELECT session_id, kind, detail, created_at_ms
            FROM session_events
            WHERE session_id = ?1
            ORDER BY created_at_ms ASC
            LIMIT ?2
            ",
            params![session_id.to_string(), limit],
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut facts = Vec::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        facts.push(SessionFact {
            session_id: row.get::<String>(0).map_err(|error| error.to_string())?,
            kind: row.get::<String>(1).map_err(|error| error.to_string())?,
            detail: row.get::<String>(2).map_err(|error| error.to_string())?,
            created_at_ms: row.get::<i64>(3).map_err(|error| error.to_string())?,
        });
    }

    Ok(facts)
}

async fn session_promotion_facts(
    conn: &Connection,
    session: &Session,
) -> Result<Vec<SessionFact>, String> {
    let mut facts = session_events(conn, &session.id, 12).await?;
    if let Some(summary) = session
        .final_summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        facts.push(SessionFact {
            session_id: session.id.clone(),
            kind: "summary".to_string(),
            detail: summary.to_string(),
            created_at_ms: session.ended_at_ms.unwrap_or(session.started_at_ms),
        });
    }
    Ok(facts)
}

async fn promoted_memory_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<Memory>, String> {
    let mut rows = conn
        .query(
            "
            SELECT m.id, m.created_at_ms, m.kind, m.text, m.structured_payload
            FROM session_promotions AS p
            JOIN memories AS m ON m.id = p.memory_id
            WHERE p.session_id = ?1
            LIMIT 1
            ",
            params![session_id.to_string()],
        )
        .await
        .map_err(|error| error.to_string())?;

    let Some(row) = rows.next().await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };

    memory_from_row(&row).map(Some)
}

async fn insert_session_promotion(
    conn: &Connection,
    session_id: &str,
    memory_id: &str,
) -> Result<(), String> {
    conn.execute(
        "
        INSERT INTO session_promotions (session_id, memory_id, promoted_at_ms)
        VALUES (?1, ?2, ?3)
        ",
        params![session_id.to_string(), memory_id.to_string(), now_ms()?],
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn session_promotion_text(session: &Session, facts: &[SessionFact]) -> String {
    let facts = facts
        .iter()
        .map(session_promotion_fact_text)
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Session '{}' produced durable findings: {}",
        session.task, facts
    )
}

fn session_promotion_payload(
    session: &Session,
    facts: &[SessionFact],
    project: Option<&Project>,
) -> String {
    let facts = facts
        .iter()
        .map(|fact| {
            json!({
                "kind": &fact.kind,
                "detail": &fact.detail,
                "created_at_ms": fact.created_at_ms
            })
        })
        .collect::<Vec<_>>();

    let mut payload = serde_json::Map::new();
    payload.insert(
        "source".to_string(),
        json!({
            "type": "session_promotion",
            "session_id": &session.id,
            "task": &session.task,
            "branch": &session.branch,
            "started_at_ms": session.started_at_ms,
            "ended_at_ms": session.ended_at_ms
        }),
    );
    payload.insert("facts".to_string(), json!(facts));
    if let Some(project) = project {
        payload.insert("project".to_string(), project_scope_payload(project));
    }

    json!(payload).to_string()
}

fn project_scope_payload(project: &Project) -> serde_json::Value {
    json!({
        "id": &project.id,
        "name": &project.name,
        "root_path": &project.root_path,
        "git_remote": &project.git_remote,
        "default_branch": &project.default_branch
    })
}

fn memory_write_payload(options: &MemoryWriteOptions, project: Option<&Project>) -> Option<String> {
    let mut payload = serde_json::Map::new();

    if let Some(project) = project {
        payload.insert("project".to_string(), project_scope_payload(project));
    }

    if let Some(source) = &options.source {
        payload.insert(
            "source".to_string(),
            json!({
                "type": "manual",
                "kind": &source.kind,
                "locator": &source.locator
            }),
        );
    }

    let mut metadata = serde_json::Map::new();
    if let Some(confidence) = options.confidence {
        metadata.insert("confidence".to_string(), json!(confidence));
    }
    if let Some(sensitivity) = &options.sensitivity {
        metadata.insert("sensitivity".to_string(), json!(sensitivity));
    }
    if options.valid_from.is_some() || options.valid_to.is_some() {
        metadata.insert(
            "validity".to_string(),
            json!({
                "valid_from": &options.valid_from,
                "valid_to": &options.valid_to
            }),
        );
    }

    if !metadata.is_empty() {
        payload.insert("metadata".to_string(), json!(metadata));
    }

    (!payload.is_empty()).then(|| json!(payload).to_string())
}

fn normalize_memory_write_options(
    options: MemoryWriteOptions,
) -> Result<MemoryWriteOptions, String> {
    if let Some(confidence) = options.confidence {
        if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
            return Err("memory confidence must be between 0.0 and 1.0".to_string());
        }
    }

    let source = options.source.map(normalize_memory_source).transpose()?;
    let sensitivity = options
        .sensitivity
        .map(normalize_memory_label)
        .transpose()?;
    let valid_from = options.valid_from.map(normalize_memory_value).transpose()?;
    let valid_to = options.valid_to.map(normalize_memory_value).transpose()?;

    Ok(MemoryWriteOptions {
        source,
        confidence: options.confidence,
        sensitivity,
        valid_from,
        valid_to,
    })
}

fn normalize_memory_source(source: MemorySource) -> Result<MemorySource, String> {
    let kind = source.kind.trim();
    let locator = source.locator.trim();
    if kind.is_empty() {
        return Err("memory source kind is required".to_string());
    }
    if locator.is_empty() {
        return Err("memory source locator is required".to_string());
    }

    Ok(MemorySource {
        kind: kind.to_string(),
        locator: locator.to_string(),
    })
}

fn normalize_memory_label(value: String) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("memory sensitivity is required".to_string());
    }
    if !value
        .chars()
        .all(|char| char.is_ascii_alphanumeric() || char == '_' || char == '-')
    {
        return Err(
            "memory sensitivity may only contain letters, numbers, hyphens, or underscores"
                .to_string(),
        );
    }
    Ok(value.to_string())
}

fn normalize_memory_value(value: String) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err("memory validity value is required".to_string())
    } else {
        Ok(value.to_string())
    }
}

fn session_promotion_fact_text(fact: &SessionFact) -> String {
    let prefix = format!("{}:", fact.kind);
    if fact.detail.starts_with(&prefix) {
        fact.detail.clone()
    } else {
        format!("{}: {}", fact.kind, fact.detail)
    }
}

fn session_fact_score(fact: &SessionFact, terms: &[String]) -> usize {
    let text = format!("{} {}", fact.kind, fact.detail).to_lowercase();
    terms
        .iter()
        .filter(|term| text.contains(term.as_str()))
        .count()
}

/// Builds the SQL prefilter for symbol recall. A blind recency-ordered LIMIT
/// made symbols beyond the first 2000 rows invisible on large repositories,
/// so the WHERE clause narrows candidates per term instead. The name check
/// uses the term's four-character stem so bounded prefix stemming
/// (rank/ranking) still reaches the scorer, which remains the arbiter; LIKE
/// underscore wildcards only widen the candidate set, never narrow it.
fn symbol_recall_query(terms: &[String]) -> (String, Vec<libsql::Value>) {
    let mut sql = String::from(
        "SELECT path, language, name, kind, line_start, line_end, signature \
         FROM code_symbols WHERE project_id = ? AND (",
    );
    let mut args: Vec<libsql::Value> = vec![LOCAL_PROJECT_ID.into()];

    for (index, term) in terms.iter().enumerate() {
        if index > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str(
            "name LIKE '%' || ? || '%' OR path LIKE '%' || ? || '%' \
             OR signature LIKE '%' || ? || '%' OR kind = ?",
        );
        let name_stem = term.chars().take(4).collect::<String>();
        args.push(name_stem.into());
        args.push(term.clone().into());
        args.push(term.clone().into());
        args.push(term.clone().into());
    }

    sql.push_str(") ORDER BY indexed_at_ms DESC, path ASC, line_start ASC LIMIT 5000");
    (sql, args)
}

fn code_symbol_score(symbol: &CodeSymbol, terms: &[String], query: &str) -> usize {
    let name = symbol.name.to_lowercase();
    let path = symbol.path.to_lowercase();
    let kind = symbol.kind.to_lowercase();
    let signature = symbol.signature.to_lowercase();
    let query = query.to_lowercase();
    let name_words = code::identifier_word_tokens(&symbol.name);
    let exact_bonus = if name == query || signature.contains(&query) {
        12
    } else {
        0
    };

    let term_score = terms
        .iter()
        .map(|term| {
            let mut score = 0;
            if name == *term {
                score += 10;
            } else if name_words.iter().any(|word| word == term) {
                score += 7;
            } else if name_words
                .iter()
                .any(|word| code::term_matches_identifier_word(word, term))
            {
                score += 6;
            } else if name.contains(term.as_str()) {
                score += 5;
            }
            if kind == *term {
                score += 3;
            }
            if path.contains(term.as_str()) {
                score += 2;
            }
            if signature.contains(term.as_str()) {
                score += 1;
            }
            score
        })
        .sum::<usize>();

    // Prefer actionable definitions over same-file declarations only when the
    // symbol already matched the task, so unrelated symbols stay unmatched.
    let kind_bonus = if term_score > 0 {
        match kind.as_str() {
            "function" | "method" => 2,
            "struct" | "class" | "trait" | "interface" | "enum" | "impl" | "protocol"
            | "object" | "record" | "actor" | "type" => 1,
            _ => 0,
        }
    } else {
        0
    };

    exact_bonus + term_score + kind_bonus
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

async fn collect_stored_paths(
    conn: &Connection,
    table: &str,
    column: &str,
    paths: &mut HashSet<String>,
) -> Result<(), String> {
    // `table` and `column` are fixed internal literals, never user input.
    let sql = format!("SELECT DISTINCT {column} FROM {table} WHERE project_id = ?1");
    let mut rows = conn
        .query(&sql, params![LOCAL_PROJECT_ID])
        .await
        .map_err(|error| error.to_string())?;
    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let path = row.get::<String>(0).map_err(|error| error.to_string())?;
        paths.insert(path);
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct SourceEmbeddingInput {
    path: String,
    language: Option<String>,
    size_bytes: Option<i64>,
}

async fn refresh_test_mappings_for_paths(
    conn: &Connection,
    paths: &HashSet<String>,
    now: i64,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }

    let known_files = load_known_file_paths(conn).await?;
    if known_files.is_empty() {
        return Ok(());
    }

    let inline_test_paths = load_inline_test_paths(conn).await?;
    let mut refresh_paths = paths.clone();
    if paths.iter().any(|path| testmap::is_test_path(path)) {
        refresh_paths.extend(
            known_files
                .iter()
                .filter(|path| !testmap::is_test_path(path))
                .cloned(),
        );
    }

    let mut refresh_paths = refresh_paths.into_iter().collect::<Vec<_>>();
    refresh_paths.sort();
    for source_path in refresh_paths {
        conn.execute(
            "DELETE FROM test_mappings WHERE project_id = ?1 AND source_path = ?2",
            params![LOCAL_PROJECT_ID, source_path.clone()],
        )
        .await
        .map_err(|error| error.to_string())?;

        let candidates = testmap::likely_tests_for_files(
            std::slice::from_ref(&source_path),
            &known_files,
            &inline_test_paths,
            32,
        );
        for candidate in candidates {
            let score = i64::try_from(candidate.score).map_err(|error| error.to_string())?;
            conn.execute(
                "
                INSERT INTO test_mappings (
                    project_id, source_path, test_path, reason, score, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ",
                params![
                    LOCAL_PROJECT_ID,
                    source_path.clone(),
                    candidate.path,
                    candidate.reason,
                    score,
                    now
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

async fn load_inline_test_paths(conn: &Connection) -> Result<HashSet<String>, String> {
    let mut rows = conn
        .query(
            "
            SELECT DISTINCT path
            FROM code_symbols
            WHERE project_id = ?1
              AND kind = 'module'
              AND name IN ('tests', 'test')
              AND language = 'rust'
            ",
            params![LOCAL_PROJECT_ID],
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut paths = HashSet::new();
    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        paths.insert(row.get::<String>(0).map_err(|error| error.to_string())?);
    }
    Ok(paths)
}

async fn load_known_file_paths(conn: &Connection) -> Result<Vec<String>, String> {
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
    Ok(known_files)
}

async fn stored_test_candidates_for_files(
    conn: &Connection,
    files: &[String],
    limit: usize,
) -> Result<Vec<TestCandidate>, String> {
    if files.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut candidates = Vec::new();
    let row_limit = i64::try_from(limit.max(32)).map_err(|error| error.to_string())?;
    for file in files {
        let mut rows = conn
            .query(
                "
                SELECT test_path, reason, score
                FROM test_mappings
                WHERE project_id = ?1 AND source_path = ?2
                ORDER BY score DESC, test_path ASC
                LIMIT ?3
                ",
                params![LOCAL_PROJECT_ID, file.clone(), row_limit],
            )
            .await
            .map_err(|error| error.to_string())?;
        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            candidates.push(TestCandidate {
                path: row.get::<String>(0).map_err(|error| error.to_string())?,
                reason: row.get::<String>(1).map_err(|error| error.to_string())?,
                score: usize::try_from(row.get::<i64>(2).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?,
            });
        }
    }

    Ok(finalize_test_candidates(candidates, limit))
}

fn test_candidates_from_mappings(
    mappings: &[TestMappingSyncRecord],
    files: &[String],
    limit: usize,
) -> Vec<TestCandidate> {
    if files.is_empty() || limit == 0 {
        return Vec::new();
    }
    let file_set = files.iter().map(String::as_str).collect::<HashSet<_>>();
    let candidates = mappings
        .iter()
        .filter(|mapping| file_set.contains(mapping.source_path.as_str()))
        .filter_map(|mapping| {
            Some(TestCandidate {
                path: mapping.test_path.clone(),
                reason: mapping.reason.clone(),
                score: usize::try_from(mapping.score).ok()?,
            })
        })
        .collect::<Vec<_>>();
    finalize_test_candidates(candidates, limit)
}

fn merge_test_candidates(candidates: &mut Vec<TestCandidate>, additional: Vec<TestCandidate>) {
    candidates.extend(additional);
}

fn finalize_test_candidates(candidates: Vec<TestCandidate>, limit: usize) -> Vec<TestCandidate> {
    if limit == 0 {
        return Vec::new();
    }

    let mut merged = HashMap::<String, TestCandidate>::new();
    for candidate in candidates {
        match merged.get_mut(&candidate.path) {
            Some(existing)
                if candidate.score > existing.score
                    || (candidate.score == existing.score
                        && candidate.reason < existing.reason) =>
            {
                *existing = candidate;
            }
            Some(_) => {}
            None => {
                merged.insert(candidate.path.clone(), candidate);
            }
        }
    }
    let mut ranked = merged.into_values().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
    });
    ranked.truncate(limit);
    ranked
}

async fn local_source_embedding_file_candidates(
    conn: &Connection,
    embedding_provider: &SelectedEmbeddingProvider,
    query: &str,
    limit: usize,
) -> Result<Vec<FileCandidate>, String> {
    if limit == 0 || query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let embedding = embedding_provider.embed(query)?;
    let query_vector = embedding.to_vector_literal();
    let dimensions = i64::try_from(embedding.dimensions()).map_err(|error| error.to_string())?;
    let candidate_limit = i64::try_from(limit.max(50)).map_err(|error| error.to_string())?;
    let mut rows = conn
        .query(
            "
            WITH vector_matches AS (
                SELECT id, row_number() OVER () AS vector_rank
                FROM vector_top_k('source_embeddings_vector_idx', ?1, ?2)
            )
            SELECT
                e.path,
                d.language,
                d.size_bytes,
                vector_matches.vector_rank
            FROM vector_matches
            JOIN source_embeddings AS e ON e.rowid = vector_matches.id
            LEFT JOIN discovered_files AS d
              ON d.project_id = e.project_id AND d.path = e.path
            WHERE e.project_id = ?3
              AND e.model = ?4
              AND e.dimensions = ?5
            ORDER BY vector_matches.vector_rank
            LIMIT ?6
            ",
            params![
                query_vector,
                candidate_limit,
                LOCAL_PROJECT_ID,
                embedding.model,
                dimensions,
                i64::try_from(limit).map_err(|error| error.to_string())?
            ],
        )
        .await
        .map_err(|error| error.to_string())?;

    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let rank = usize::try_from(row.get::<i64>(3).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let size_bytes = row
            .get::<Option<i64>>(2)
            .map_err(|error| error.to_string())?
            .and_then(|bytes| u64::try_from(bytes).ok());
        candidates.push(FileCandidate {
            path: row.get::<String>(0).map_err(|error| error.to_string())?,
            language: row
                .get::<Option<String>>(1)
                .map_err(|error| error.to_string())?,
            size_bytes,
            score: crate::discovery::source_embedding_score(rank),
        });
    }

    Ok(candidates)
}

fn f32_blob_to_vector(blob: &[u8]) -> Result<Vec<f32>, String> {
    if !blob.len().is_multiple_of(4) {
        return Err("source embedding blob length is not divisible by 4".to_string());
    }

    Ok(blob
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn cosine_like_score(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

async fn refresh_source_embeddings_for_files(
    conn: &Connection,
    files: &[FileCandidate],
    symbols: &[CodeSymbol],
    now: i64,
    embedding_provider: &SelectedEmbeddingProvider,
) -> Result<(), String> {
    let inputs = files
        .iter()
        .map(|file| {
            let size_bytes = file
                .size_bytes
                .map(i64::try_from)
                .transpose()
                .map_err(|error| error.to_string())?;
            Ok(SourceEmbeddingInput {
                path: file.path.clone(),
                language: file.language.clone(),
                size_bytes,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    refresh_source_embeddings_for_inputs(conn, &inputs, symbols, now, embedding_provider).await
}

async fn refresh_source_embeddings_for_paths(
    conn: &Connection,
    paths: &HashSet<String>,
    symbols: &[CodeSymbol],
    now: i64,
    embedding_provider: &SelectedEmbeddingProvider,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let symbol_languages = symbols
        .iter()
        .filter_map(|symbol| {
            symbol
                .language
                .as_ref()
                .map(|language| (symbol.path.clone(), language.clone()))
        })
        .collect::<HashMap<_, _>>();
    let mut rows = conn
        .query(
            "
            SELECT path, language, size_bytes
            FROM discovered_files
            WHERE project_id = ?1
            ORDER BY path ASC
            ",
            params![LOCAL_PROJECT_ID],
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut inputs = Vec::new();
    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        let path = row.get::<String>(0).map_err(|error| error.to_string())?;
        if !paths.contains(&path) {
            continue;
        }
        let language = row
            .get::<Option<String>>(1)
            .map_err(|error| error.to_string())?
            .or_else(|| symbol_languages.get(&path).cloned());
        inputs.push(SourceEmbeddingInput {
            path,
            language,
            size_bytes: row
                .get::<Option<i64>>(2)
                .map_err(|error| error.to_string())?,
        });
    }
    for path in paths {
        if !inputs.iter().any(|input| &input.path == path) {
            inputs.push(SourceEmbeddingInput {
                path: path.clone(),
                language: symbol_languages.get(path).cloned(),
                size_bytes: None,
            });
        }
    }
    refresh_source_embeddings_for_inputs(conn, &inputs, symbols, now, embedding_provider).await
}

async fn refresh_source_embeddings_for_inputs(
    conn: &Connection,
    inputs: &[SourceEmbeddingInput],
    symbols: &[CodeSymbol],
    now: i64,
    embedding_provider: &SelectedEmbeddingProvider,
) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }

    for input in inputs {
        let summary = source_embedding_summary(input, symbols);
        let content_key = source_embedding_content_key(&summary);
        let dimensions =
            i64::try_from(embedding_provider.dimensions()).map_err(|error| error.to_string())?;
        if source_embedding_is_fresh(
            conn,
            &input.path,
            embedding_provider.model(),
            dimensions,
            &content_key,
        )
        .await?
        {
            continue;
        }
        let embedding = embedding_provider.embed(&summary)?;
        let embedding_dimensions =
            i64::try_from(embedding.dimensions()).map_err(|error| error.to_string())?;
        let embedding_vector = embedding.to_vector_literal();
        conn.execute(
            "
            INSERT INTO source_embeddings (
                project_id, path, model, dimensions, embedding, content_key, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, vector(?5), ?6, ?7)
            ON CONFLICT(project_id, path) DO UPDATE SET
                model = excluded.model,
                dimensions = excluded.dimensions,
                embedding = excluded.embedding,
                content_key = excluded.content_key,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                LOCAL_PROJECT_ID,
                input.path.clone(),
                embedding.model,
                embedding_dimensions,
                embedding_vector,
                content_key,
                now
            ],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    Ok(())
}

async fn source_embedding_is_fresh(
    conn: &Connection,
    path: &str,
    model: &str,
    dimensions: i64,
    content_key: &str,
) -> Result<bool, String> {
    let mut rows = conn
        .query(
            "
            SELECT 1
            FROM source_embeddings
            WHERE project_id = ?1
              AND path = ?2
              AND model = ?3
              AND dimensions = ?4
              AND content_key = ?5
            LIMIT 1
            ",
            params![
                LOCAL_PROJECT_ID,
                path.to_string(),
                model.to_string(),
                dimensions,
                content_key.to_string()
            ],
        )
        .await
        .map_err(|error| error.to_string())?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| error.to_string())
}

fn source_embedding_summary(input: &SourceEmbeddingInput, symbols: &[CodeSymbol]) -> String {
    let mut summary = String::new();
    summary.push_str("source ");
    summary.push_str(&input.path);
    summary.push('\n');
    if let Some(language) = &input.language {
        summary.push_str("language ");
        summary.push_str(language);
        summary.push('\n');
    }
    if let Some(size_bytes) = input.size_bytes {
        summary.push_str("size ");
        summary.push_str(&size_bytes.to_string());
        summary.push('\n');
    }

    let mut path_symbols = symbols
        .iter()
        .filter(|symbol| symbol.path == input.path)
        .collect::<Vec<_>>();
    path_symbols.sort_by(|left, right| {
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    for symbol in path_symbols {
        summary.push_str("symbol ");
        summary.push_str(&symbol.kind);
        summary.push(' ');
        summary.push_str(&symbol.name);
        summary.push_str(" line ");
        summary.push_str(&symbol.line_start.to_string());
        summary.push(' ');
        summary.push_str(&symbol.signature);
        summary.push('\n');
    }
    summary
}

fn source_embedding_content_key(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticInput, HUGR_API_CONTRACT_VERSION, HUGR_API_ROUTES, LOCAL_PROJECT_ID, Memory,
        MemorySource, MemorySyncRecord, MemoryWriteOptions, Project, Session,
        SessionEventSyncRecord, SessionPromotionSyncRecord, StorageConfig, StorageMode, Store,
        SyncApiTablePayload, SyncBackend, SyncClass, SyncConflictSummary, SyncTableKind,
        SyncTableResult, apply_api_pull_payloads, code_symbol_score, fts_query,
        hugr_api_memory_apply_request, hugr_api_remember_payloads, hugr_api_route_url,
        hugr_api_session_promotion_payloads, hugr_api_storage_apply_request, hugr_api_sync_request,
        memory_record_promotes_session, parse_hugr_api_history_response,
        parse_hugr_api_memory_apply_response, parse_hugr_api_memory_records_response,
        parse_hugr_api_storage_apply_response, parse_hugr_api_storage_snapshot_response,
        parse_hugr_api_sync_response, planned_storage_table_result, planned_sync_table_result,
        promoted_memory_for_session_records, query_terms, recall_score,
        session_promotion_facts_from_records, sync_api_table_payload_value,
        sync_table_result_value, table_row_count,
    };
    use crate::code::{CodeReference, CodeSymbol};
    use crate::discovery::{self, FileCandidate};
    use crate::embedding::{
        DEFAULT_EMBEDDING_DIMENSIONS, DETERMINISTIC_MODEL, SelectedEmbeddingProvider,
    };
    use libsql::{Connection, params};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

    fn scored_symbol(name: &str, kind: &str, signature: &str, line_start: i64) -> CodeSymbol {
        CodeSymbol {
            path: "src/context.rs".to_string(),
            language: Some("rust".to_string()),
            name: name.to_string(),
            kind: kind.to_string(),
            line_start,
            line_end: None,
            signature: signature.to_string(),
        }
    }

    #[test]
    fn code_symbol_score_prefers_name_word_matches_over_path_matches() {
        let query = "improve recall ranking quality in the context compiler";
        let terms = query_terms(query);
        let ranking_function = scored_symbol(
            "rank_context_sections",
            "function",
            "fn rank_context_sections(&mut self)",
            1029,
        );
        let top_of_file_struct =
            scored_symbol("ContextBudget", "struct", "struct ContextBudget", 43);
        let unrelated_constant = scored_symbol(
            "MAX_SIGNATURE_CHARS",
            "constant",
            "const MAX_SIGNATURE_CHARS: usize = 180",
            8,
        );

        let function_score = code_symbol_score(&ranking_function, &terms, query);
        let struct_score = code_symbol_score(&top_of_file_struct, &terms, query);
        let constant_score = code_symbol_score(&unrelated_constant, &terms, query);

        assert!(
            function_score > struct_score,
            "stem-matched function {function_score} should outrank path-matched struct {struct_score}"
        );
        assert!(struct_score > constant_score);
    }

    #[test]
    fn code_symbol_score_keeps_unmatched_symbols_at_zero() {
        let query = "unrelated telemetry pipeline";
        let terms = query_terms(query);
        let symbol = CodeSymbol {
            path: "src/worktree.rs".to_string(),
            language: Some("rust".to_string()),
            name: "inspect".to_string(),
            kind: "function".to_string(),
            line_start: 10,
            line_end: None,
            signature: "fn inspect(root: &Path) -> WorktreeState".to_string(),
        };

        assert_eq!(code_symbol_score(&symbol, &terms, query), 0);
    }

    #[test]
    fn recall_scores_query_terms() {
        let memory = Memory {
            id: "mem_1".into(),
            created_at_ms: 1,
            kind: "fact".into(),
            text: "plugin hooks run after configuration is loaded".into(),
            structured_payload: None,
        };
        let terms = query_terms("add plugin hooks");
        assert!(recall_score(&memory, &terms, "add plugin hooks") > 0);
    }

    fn reference_neighbor(path: &str, target_path: &str, line: i64) -> super::GraphNeighbor {
        super::graph_neighbor_from_code_reference(
            "incoming_reference",
            CodeReference {
                path: path.to_string(),
                language: Some("rust".to_string()),
                target_path: target_path.to_string(),
                target_name: "ContextSymbol".to_string(),
                target_kind: "struct".to_string(),
                kind: "type_reference".to_string(),
                line_start: line,
                excerpt: format!("symbols: &[ContextSymbol], // line {line}"),
            },
        )
    }

    #[test]
    fn finalize_graph_neighbors_collapses_repeated_reference_sites() {
        let neighbors = vec![
            reference_neighbor("src/context.rs", "src/context.rs", 1283),
            reference_neighbor("src/context.rs", "src/context.rs", 1609),
            reference_neighbor("src/context.rs", "src/context.rs", 1668),
            reference_neighbor("src/mcp.rs", "src/context.rs", 44),
        ];

        let finalized = super::finalize_graph_neighbors("inspect context symbols", neighbors, 12);

        assert_eq!(finalized.len(), 2, "same-file sites should collapse");
        let cross_file = &finalized[0];
        assert_eq!(cross_file.path.as_deref(), Some("src/mcp.rs"));
        assert_eq!(cross_file.site_count, 1);
        let same_file = &finalized[1];
        assert_eq!(same_file.path.as_deref(), Some("src/context.rs"));
        assert_eq!(same_file.site_count, 3);
        assert_eq!(
            same_file.line_start,
            Some(1283),
            "earliest site should represent the collapsed group"
        );
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
        assert_eq!(config.backend, SyncBackend::DirectLibsql);
        assert_eq!(config.remote_url, None);
        assert!(!config.auth_token_configured);
        assert_eq!(
            config.sync_classes,
            vec![
                SyncClass::Memories,
                SyncClass::Sources,
                SyncClass::Entities,
                SyncClass::Edges,
                SyncClass::Embeddings,
                SyncClass::ContextPacks,
                SyncClass::Diagnostics,
                SyncClass::SessionSummaries
            ]
        );
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
        assert_eq!(config.backend, SyncBackend::DirectLibsql);
        assert_eq!(
            config.remote_url.as_deref(),
            Some("libsql://example.turso.io")
        );
        assert!(config.auth_token_configured);
        assert_eq!(
            config.summary(),
            "hybrid (local active, guarded remote sync enabled, backend: direct_libsql, auth configured, sync classes: memories,sources,entities,edges,embeddings,context_packs,diagnostics,session_summaries)"
        );

        let plan = config.sync_execution_plan();
        assert_eq!(plan.storage_mode, "hybrid");
        assert_eq!(plan.backend, "direct_libsql");
        assert_eq!(plan.status, "remote_sync_ready");
        assert!(plan.local_writes_enabled);
        assert!(plan.remote_configured);
        assert!(plan.remote_auth_configured);
        assert!(plan.remote_reads_enabled);
        assert!(plan.remote_writes_enabled);
        assert_eq!(
            plan.remote_endpoint.as_deref(),
            Some("libsql://example.turso.io")
        );
        assert_eq!(plan.api_contract_version, None);
        assert!(plan.api_routes.is_empty());
    }

    #[test]
    fn storage_config_reads_remote_direct_libsql_execution() {
        let config = StorageConfig::from_lookup(env_lookup(&[
            ("HUGR_STORAGE_MODE", "remote"),
            ("HUGR_REMOTE_DATABASE_URL", "libsql://example.turso.io"),
            ("HUGR_REMOTE_AUTH_TOKEN", "secret-token"),
        ]))
        .unwrap();

        let plan = config.sync_execution_plan();
        assert_eq!(
            config.summary(),
            "remote (backend: direct_libsql, auth configured, sync classes: memories,sources,entities,edges,embeddings,context_packs,diagnostics,session_summaries)"
        );
        assert_eq!(plan.status, "remote_storage_ready");
        assert!(!plan.local_writes_enabled);
        assert!(plan.remote_reads_enabled);
        assert!(plan.remote_writes_enabled);
        assert_eq!(
            plan.remote_endpoint.as_deref(),
            Some("libsql://example.turso.io")
        );
    }

    #[test]
    fn storage_config_reads_sync_class_opt_ins() {
        let config = StorageConfig::from_lookup(env_lookup(&[
            ("HUGR_STORAGE_MODE", "hybrid"),
            ("HUGR_REMOTE_DATABASE_URL", "libsql://example.turso.io"),
            ("HUGR_REMOTE_AUTH_TOKEN", "secret-token"),
            ("HUGR_SYNC_CLASSES", "memories,full-source,secrets,memories"),
        ]))
        .unwrap();

        assert_eq!(
            config.sync_classes,
            vec![
                SyncClass::Memories,
                SyncClass::FullSource,
                SyncClass::Secrets
            ]
        );
        assert_eq!(
            config.sync_execution_plan().explicit_opt_in_classes,
            vec!["full_source".to_string(), "secrets".to_string()]
        );
        assert!(config.summary().contains("full_source,secrets"));
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

        let invalid_class =
            StorageConfig::from_lookup(env_lookup(&[("HUGR_SYNC_CLASSES", "memories,unknown")]))
                .unwrap_err();
        assert!(invalid_class.contains("invalid HUGR_SYNC_CLASSES"));

        let invalid_backend =
            StorageConfig::from_lookup(env_lookup(&[("HUGR_SYNC_BACKEND", "ftp")])).unwrap_err();
        assert!(invalid_backend.contains("invalid HUGR_SYNC_BACKEND"));
    }

    #[test]
    fn storage_config_reads_hugr_api_backend() {
        let config = StorageConfig::from_lookup(env_lookup(&[
            ("HUGR_STORAGE_MODE", "hybrid"),
            ("HUGR_SYNC_BACKEND", "hugr-api"),
            ("HUGR_API_URL", "https://hugr.example"),
            ("HUGR_API_TOKEN", "secret-token"),
        ]))
        .unwrap();

        assert_eq!(config.backend, SyncBackend::HugrApi);
        assert_eq!(config.sync_execution_plan().backend, "hugr_api");
        assert_eq!(
            config.summary(),
            "hybrid (local active, Hugr API sync transport configured, backend: hugr_api, auth configured, sync classes: memories,sources,entities,edges,embeddings,context_packs,diagnostics,session_summaries)"
        );
        assert_eq!(
            config.sync_execution_plan().status,
            "hugr_api_transport_ready"
        );
        assert!(config.sync_execution_plan().remote_reads_enabled);
        assert!(config.sync_execution_plan().remote_writes_enabled);
        assert_eq!(
            config.sync_execution_plan().remote_endpoint.as_deref(),
            Some("https://hugr.example")
        );
        assert_eq!(
            config.sync_execution_plan().api_contract_version.as_deref(),
            Some(HUGR_API_CONTRACT_VERSION)
        );
        assert_eq!(
            config.sync_execution_plan().api_routes,
            HUGR_API_ROUTES
                .iter()
                .map(|route| route.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn hugr_api_transport_builds_contract_request() {
        let config = StorageConfig {
            mode: StorageMode::Hybrid,
            backend: SyncBackend::HugrApi,
            remote_url: Some("https://hugr.example/".to_string()),
            remote_auth_token: Some("secret-token".to_string()),
            auth_token_configured: true,
            sync_classes: vec![SyncClass::Memories, SyncClass::Secrets],
        };
        let table = SyncTableResult {
            class: "memories".to_string(),
            table: "memories".to_string(),
            row_count: 2,
            inserted_count: 1,
            updated_count: 0,
            skipped_count: 1,
            conflict_count: 1,
            executed: true,
            conflicts: vec![SyncConflictSummary {
                reason: "local_row_newer_or_equal".to_string(),
                count: 1,
            }],
        };
        let payload = SyncApiTablePayload {
            result: table,
            records: vec![serde_json::json!({
                "id": "mem_1",
                "created_at_ms": 1,
                "kind": "fact",
                "text": "plugin hooks run after config",
                "confidence": 1.0,
                "valid_from": null,
                "valid_to": null,
                "superseded_by": null,
                "sensitivity": "normal",
                "structured_payload": null
            })],
        };

        let request = hugr_api_sync_request(&config, "push", false, &[payload]);

        assert_eq!(
            hugr_api_route_url("https://hugr.example/", "/v1/sync/push"),
            "https://hugr.example/v1/sync/push"
        );
        assert_eq!(
            request["contract_version"],
            serde_json::json!(HUGR_API_CONTRACT_VERSION)
        );
        assert_eq!(request["operation"], serde_json::json!("push"));
        assert_eq!(request["dry_run"], serde_json::json!(false));
        assert_eq!(
            request["sync_classes"],
            serde_json::json!(["memories", "secrets"])
        );
        assert_eq!(
            request["explicit_opt_in_classes"],
            serde_json::json!(["secrets"])
        );
        assert_eq!(request["tables"][0]["table"], serde_json::json!("memories"));
        assert_eq!(
            request["tables"][0]["conflicts"][0]["reason"],
            serde_json::json!("local_row_newer_or_equal")
        );
        assert_eq!(
            request["tables"][0]["records"][0]["id"],
            serde_json::json!("mem_1")
        );
    }

    #[test]
    fn parses_hugr_api_sync_response() {
        let response = r#"{
            "run_id": "api_sync_push_1",
            "status": "executed",
            "tables": [
                {
                    "class": "memories",
                    "table": "memories",
                    "row_count": 2,
                    "inserted_count": 1,
                    "updated_count": 0,
                    "skipped_count": 1,
                    "conflict_count": 1,
                    "executed": true,
                    "conflicts": [
                        {"reason": "local_row_newer_or_equal", "count": 1}
                    ]
                }
            ]
        }"#;

        let parsed = parse_hugr_api_sync_response(response).unwrap();

        assert_eq!(parsed.run_id.as_deref(), Some("api_sync_push_1"));
        assert_eq!(parsed.status, "executed");
        assert_eq!(parsed.tables.len(), 1);
        assert_eq!(parsed.tables[0].table, "memories");
        assert_eq!(parsed.tables[0].inserted_count, 1);
        assert_eq!(parsed.tables[0].conflicts[0].count, 1);
    }

    #[test]
    fn remote_remember_payloads_include_project_memory_and_embedding() {
        let options = MemoryWriteOptions {
            source: Some(MemorySource {
                kind: "url".to_string(),
                locator: "https://example.test/remote".to_string(),
            }),
            confidence: Some(0.8),
            sensitivity: Some("private".to_string()),
            valid_from: Some("now".to_string()),
            valid_to: None,
        };
        let (memory, payloads) = hugr_api_remember_payloads(
            &SelectedEmbeddingProvider::default(),
            "  remote remember payload  ",
            options,
        )
        .unwrap();

        assert_eq!(memory.text, "remote remember payload");
        assert_eq!(payloads.len(), 3);
        assert_eq!(payloads[0].result.table, "projects");
        assert_eq!(payloads[1].result.table, "memories");
        assert_eq!(payloads[2].result.table, "memory_embeddings");
        assert_eq!(payloads[1].records[0]["id"], serde_json::json!(memory.id));
        assert_eq!(payloads[1].records[0]["confidence"], serde_json::json!(0.8));
        assert_eq!(
            payloads[1].records[0]["sensitivity"],
            serde_json::json!("private")
        );
        assert!(payloads[1].records[0]["valid_from"].is_null());
        let structured_payload = serde_json::from_str::<serde_json::Value>(
            payloads[1].records[0]["structured_payload"]
                .as_str()
                .expect("structured payload should be a JSON string"),
        )
        .unwrap();
        assert_eq!(
            structured_payload["source"]["locator"],
            serde_json::json!("https://example.test/remote")
        );
        assert_eq!(
            structured_payload["metadata"]["validity"]["valid_from"],
            serde_json::json!("now")
        );
        assert_eq!(
            payloads[2].records[0]["embedding"]
                .as_array()
                .expect("embedding should be encoded as bytes")
                .len(),
            DEFAULT_EMBEDDING_DIMENSIONS * 4
        );
    }

    #[test]
    fn builds_and_parses_hugr_api_memory_route_payloads() {
        let payload = SyncApiTablePayload {
            result: planned_sync_table_result(SyncTableKind::Memories, 1),
            records: vec![serde_json::json!({
                "id": "mem_1",
                "created_at_ms": 1,
                "kind": "fact",
                "text": "remote memory",
                "confidence": 1.0,
                "valid_from": null,
                "valid_to": null,
                "superseded_by": null,
                "sensitivity": "normal",
                "structured_payload": null
            })],
        };
        let request = hugr_api_memory_apply_request(std::slice::from_ref(&payload));

        assert_eq!(
            request["contract_version"],
            serde_json::json!(HUGR_API_CONTRACT_VERSION)
        );
        assert!(request.get("operation").is_none());
        assert_eq!(request["tables"][0]["table"], serde_json::json!("memories"));

        let records_response = serde_json::json!({
            "status": "ok",
            "contract_version": HUGR_API_CONTRACT_VERSION,
            "records": payload.records
        })
        .to_string();
        let records = parse_hugr_api_memory_records_response(&records_response).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "mem_1");
        assert_eq!(records[0].text, "remote memory");

        let apply_response = serde_json::json!({
            "status": "accepted",
            "contract_version": HUGR_API_CONTRACT_VERSION,
            "tables": [sync_table_result_value(&payload.result)]
        })
        .to_string();
        let parsed = parse_hugr_api_memory_apply_response(&apply_response).unwrap();
        assert_eq!(parsed.status, "accepted");
        assert_eq!(parsed.tables[0].table, "memories");
    }

    #[test]
    fn builds_and_parses_hugr_api_storage_route_payloads() {
        let payload = SyncApiTablePayload {
            result: planned_sync_table_result(SyncTableKind::CodeSymbols, 1),
            records: vec![serde_json::json!({
                "project_id": LOCAL_PROJECT_ID,
                "path": "src/lib.rs",
                "name": "ApiThing",
                "kind": "struct",
                "language": "rust",
                "line_start": 1,
                "line_end": null,
                "signature": "pub struct ApiThing",
                "indexed_at_ms": 2
            })],
        };
        let session_event = serde_json::json!({
            "id": "evt_1_0",
            "session_id": "ses_1",
            "kind": "note",
            "detail": "indexed src/lib.rs",
            "created_at_ms": 3
        });
        let session_promotion = serde_json::json!({
            "session_id": "ses_1",
            "memory_id": "mem_1",
            "promoted_at_ms": 4
        });
        let request = hugr_api_storage_apply_request(
            std::slice::from_ref(&payload),
            std::slice::from_ref(&session_event),
            std::slice::from_ref(&session_promotion),
            &["src/lib.rs".to_string()],
        );

        assert_eq!(
            request["contract_version"],
            serde_json::json!(HUGR_API_CONTRACT_VERSION)
        );
        assert_eq!(
            request["tables"][0]["table"],
            serde_json::json!("code_symbols")
        );
        assert_eq!(
            request["replace_code_index_paths"],
            serde_json::json!(["src/lib.rs"])
        );
        assert_eq!(
            request["session_events"][0]["id"],
            serde_json::json!("evt_1_0")
        );
        assert_eq!(
            request["session_promotions"][0]["memory_id"],
            serde_json::json!("mem_1")
        );

        let snapshot_response = serde_json::json!({
            "status": "ok",
            "contract_version": HUGR_API_CONTRACT_VERSION,
            "tables": [sync_api_table_payload_value(&payload)],
            "session_events": [session_event],
            "session_promotions": [session_promotion]
        })
        .to_string();
        let snapshot = parse_hugr_api_storage_snapshot_response(&snapshot_response).unwrap();
        assert_eq!(snapshot.payloads.len(), 1);
        assert_eq!(snapshot.payloads[0].result.table, "code_symbols");
        assert_eq!(snapshot.session_events[0].detail, "indexed src/lib.rs");
        assert_eq!(snapshot.session_promotions[0].memory_id, "mem_1");

        let apply_response = serde_json::json!({
            "status": "accepted",
            "contract_version": HUGR_API_CONTRACT_VERSION,
            "tables": [sync_table_result_value(&payload.result)],
            "session_events_table": sync_table_result_value(&planned_storage_table_result(
                "session_summaries",
                "session_events",
                1,
                true
            )),
            "session_promotions_table": sync_table_result_value(&planned_storage_table_result(
                "session_summaries",
                "session_promotions",
                1,
                true
            ))
        })
        .to_string();
        let parsed = parse_hugr_api_storage_apply_response(&apply_response).unwrap();
        assert_eq!(parsed.status, "accepted");
        assert_eq!(parsed.tables[0].table, "code_symbols");
        assert_eq!(parsed.session_events_table.table, "session_events");
        assert_eq!(parsed.session_promotions_table.table, "session_promotions");
    }

    #[test]
    fn hosted_session_promotion_payloads_preserve_provenance() {
        let session = Session {
            id: "ses_1".to_string(),
            task: "live remote session".to_string(),
            branch: Some("main".to_string()),
            started_at_ms: 10,
            ended_at_ms: Some(30),
            final_summary: Some("LiveRemoteThing session complete".to_string()),
        };
        let events = vec![SessionEventSyncRecord {
            id: "evt_1_0".to_string(),
            session_id: "ses_1".to_string(),
            kind: "note".to_string(),
            detail: "LiveRemoteThing was indexed remotely".to_string(),
            created_at_ms: 20,
        }];
        let facts = session_promotion_facts_from_records(&session, &events);
        let project = Project {
            id: LOCAL_PROJECT_ID.to_string(),
            name: "hugr_api_live_client".to_string(),
            root_path: "/tmp/hugr_api_live_client".to_string(),
            git_remote: None,
            default_branch: Some("main".to_string()),
            created_at_ms: 1,
            updated_at_ms: 2,
        };
        let (memory, payloads, promotion) = hugr_api_session_promotion_payloads(
            &SelectedEmbeddingProvider::default(),
            &session,
            &facts,
            Some(&project),
        )
        .unwrap();

        assert_eq!(facts.len(), 2);
        assert_eq!(promotion.session_id, "ses_1");
        assert_eq!(promotion.memory_id, memory.id);
        assert!(memory.text.contains("live remote session"));
        assert!(memory.text.contains("LiveRemoteThing session complete"));
        assert!(
            payloads
                .iter()
                .any(|payload| payload.result.table == "projects")
        );
        assert!(
            payloads
                .iter()
                .any(|payload| payload.result.table == "memories")
        );
        assert!(
            payloads
                .iter()
                .any(|payload| payload.result.table == "memory_embeddings")
        );

        let payload = serde_json::from_str::<serde_json::Value>(
            memory.structured_payload.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(payload["source"]["type"], "session_promotion");
        assert_eq!(payload["source"]["session_id"], "ses_1");
        assert_eq!(payload["project"]["id"], LOCAL_PROJECT_ID);
        assert_eq!(payload["facts"][0]["kind"], "note");
        assert_eq!(payload["facts"][1]["kind"], "summary");

        let memory_record = MemorySyncRecord {
            id: memory.id.clone(),
            created_at_ms: memory.created_at_ms,
            kind: memory.kind.clone(),
            text: memory.text.clone(),
            confidence: 1.0,
            valid_from: None,
            valid_to: None,
            superseded_by: None,
            sensitivity: "normal".to_string(),
            structured_payload: memory.structured_payload.clone(),
        };
        let stale_promotion = SessionPromotionSyncRecord {
            session_id: "ses_1".to_string(),
            memory_id: "missing".to_string(),
            promoted_at_ms: 1,
        };

        assert!(memory_record_promotes_session(&memory_record, "ses_1"));
        assert_eq!(
            promoted_memory_for_session_records(
                std::slice::from_ref(&memory_record),
                std::slice::from_ref(&promotion),
                "ses_1",
            )
            .unwrap()
            .id,
            memory.id
        );
        assert_eq!(
            promoted_memory_for_session_records(
                std::slice::from_ref(&memory_record),
                std::slice::from_ref(&stale_promotion),
                "ses_1",
            )
            .unwrap()
            .id,
            memory.id
        );
    }

    #[test]
    fn parses_hugr_api_history_response() {
        let response = r#"{
            "runs": [
                {
                    "id": "api_sync_pull_1",
                    "operation": "pull",
                    "backend": "hugr_api",
                    "status": "executed",
                    "started_at_ms": 10,
                    "ended_at_ms": 20,
                    "tables": [
                        {
                            "class": "session_summaries",
                            "table": "sessions",
                            "row_count": 3,
                            "inserted_count": 1,
                            "updated_count": 1,
                            "skipped_count": 1,
                            "conflict_count": 1,
                            "executed": true,
                            "conflicts": [
                                {"reason": "remote_row_missing_dependency", "count": 1}
                            ]
                        }
                    ]
                }
            ]
        }"#;

        let history = parse_hugr_api_history_response(response).unwrap();

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, "api_sync_pull_1");
        assert_eq!(history[0].operation, "pull");
        assert_eq!(history[0].tables[0].table, "sessions");
        assert_eq!(history[0].tables[0].updated_count, 1);
    }

    #[tokio::test]
    async fn records_hugr_api_sync_runs() {
        let test = TestStore::new("hugr_api_sync_history");
        let table = SyncTableResult {
            class: "memories".to_string(),
            table: "memories".to_string(),
            row_count: 2,
            inserted_count: 0,
            updated_count: 0,
            skipped_count: 0,
            conflict_count: 0,
            executed: true,
            conflicts: Vec::new(),
        };

        let run_id = test
            .store
            .record_api_sync_run("push", "accepted", &[table])
            .await
            .unwrap();
        let history = test.store.sync_history(5).await.unwrap();

        assert_eq!(history[0].id, run_id);
        assert_eq!(history[0].operation, "push");
        assert_eq!(history[0].backend, "hugr_api");
        assert_eq!(history[0].status, "accepted");
        assert_eq!(history[0].tables[0].table, "memories");
        assert!(history[0].tables[0].executed);
    }

    #[tokio::test]
    async fn api_push_applies_memory_row_payloads() {
        let local = TestStore::new("hugr_api_push_local");
        let remote = TestStore::new("hugr_api_push_remote");
        let memory = local
            .store
            .remember("plugin hooks run after configuration")
            .await
            .unwrap();
        let local_conn = local.store.connect().await.unwrap();
        let table = SyncTableResult {
            class: "memories".to_string(),
            table: "memories".to_string(),
            row_count: 1,
            inserted_count: 0,
            updated_count: 0,
            skipped_count: 0,
            conflict_count: 0,
            executed: false,
            conflicts: Vec::new(),
        };
        let payloads = local
            .store
            .sync_api_table_payloads(&local_conn, &[table], true)
            .await
            .unwrap();

        let (run_id, status, response_payloads) = remote
            .store
            .apply_api_sync_push_payloads(&payloads, false)
            .await
            .unwrap();
        let remote_conn = remote.store.connect().await.unwrap();

        assert!(run_id.is_some());
        assert_eq!(status, "accepted");
        assert_eq!(response_payloads[0].result.table, "memories");
        assert_eq!(response_payloads[0].result.inserted_count, 1);
        assert!(response_payloads[0].records.is_empty());
        assert_eq!(
            memory_text(&remote_conn, &memory.id).await,
            "plugin hooks run after configuration"
        );
    }

    #[tokio::test]
    async fn api_pull_returns_and_applies_memory_row_payloads() {
        let remote = TestStore::new("hugr_api_pull_remote");
        let local = TestStore::new("hugr_api_pull_local");
        let memory = remote
            .store
            .remember("remote memory flows through API pull")
            .await
            .unwrap();
        local.store.init().await.unwrap();
        let local_conn = local.store.connect().await.unwrap();
        let request_payloads = vec![SyncApiTablePayload {
            result: SyncTableResult {
                class: "memories".to_string(),
                table: "memories".to_string(),
                row_count: 0,
                inserted_count: 0,
                updated_count: 0,
                skipped_count: 0,
                conflict_count: 0,
                executed: false,
                conflicts: Vec::new(),
            },
            records: Vec::new(),
        }];

        let (run_id, status, response_payloads) = remote
            .store
            .api_sync_pull_payloads(&request_payloads, false)
            .await
            .unwrap();
        let applied = apply_api_pull_payloads(&local_conn, &response_payloads)
            .await
            .unwrap();

        assert!(run_id.is_some());
        assert_eq!(status, "accepted");
        assert_eq!(response_payloads[0].records.len(), 1);
        assert_eq!(applied[0].inserted_count, 1);
        assert_eq!(
            memory_text(&local_conn, &memory.id).await,
            "remote memory flows through API pull"
        );
    }

    #[tokio::test]
    async fn api_push_applies_project_source_entity_and_edge_payloads() {
        let local = TestStore::new("hugr_api_push_structural_local");
        let remote = TestStore::new("hugr_api_push_structural_remote");
        local.store.init().await.unwrap();
        let local_conn = local.store.connect().await.unwrap();
        insert_source_for_sync(&local_conn, "src_1", "url", "https://example.test", 10).await;
        insert_entity_for_sync(&local_conn, "ent_1", "service", "PluginRegistry", None, 11).await;
        insert_edge_for_sync(&local_conn, "edge_1", "ent_1", "ent_2", "depends_on", 12).await;
        let tables = vec![
            planned_sync_table_result(SyncTableKind::Projects, 1),
            planned_sync_table_result(SyncTableKind::Sources, 1),
            planned_sync_table_result(SyncTableKind::Entities, 1),
            planned_sync_table_result(SyncTableKind::Edges, 1),
        ];

        let payloads = local
            .store
            .sync_api_table_payloads(&local_conn, &tables, true)
            .await
            .unwrap();
        let (_, status, response_payloads) = remote
            .store
            .apply_api_sync_push_payloads(&payloads, false)
            .await
            .unwrap();
        let remote_conn = remote.store.connect().await.unwrap();

        assert_eq!(status, "accepted");
        assert_eq!(response_payloads.len(), 4);
        assert_eq!(table_row_count(&remote_conn, "sources").await.unwrap(), 1);
        assert_eq!(table_row_count(&remote_conn, "entities").await.unwrap(), 1);
        assert_eq!(table_row_count(&remote_conn, "edges").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn api_pull_returns_and_applies_project_source_entity_and_edge_payloads() {
        let remote = TestStore::new("hugr_api_pull_structural_remote");
        let local = TestStore::new("hugr_api_pull_structural_local");
        remote.store.init().await.unwrap();
        local.store.init().await.unwrap();
        let remote_conn = remote.store.connect().await.unwrap();
        let local_conn = local.store.connect().await.unwrap();
        remote_conn
            .execute(
                "
                INSERT INTO projects (
                    id, name, root_path, git_remote, default_branch, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ",
                params![
                    "project_remote".to_string(),
                    "remote".to_string(),
                    "/remote".to_string(),
                    Option::<String>::None,
                    Option::<String>::None,
                    10_i64,
                    20_i64
                ],
            )
            .await
            .unwrap();
        insert_source_for_sync(&remote_conn, "src_1", "url", "https://example.test", 10).await;
        insert_entity_for_sync(
            &remote_conn,
            "ent_1",
            "service",
            "PluginRegistry",
            Some("src/registry.rs"),
            11,
        )
        .await;
        insert_edge_for_sync(&remote_conn, "edge_1", "ent_1", "ent_2", "depends_on", 12).await;
        let request_payloads = vec![
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::Projects, 0),
                records: Vec::new(),
            },
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::Sources, 0),
                records: Vec::new(),
            },
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::Entities, 0),
                records: Vec::new(),
            },
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::Edges, 0),
                records: Vec::new(),
            },
        ];

        let (_, status, response_payloads) = remote
            .store
            .api_sync_pull_payloads(&request_payloads, false)
            .await
            .unwrap();
        let applied = apply_api_pull_payloads(&local_conn, &response_payloads)
            .await
            .unwrap();

        assert_eq!(status, "accepted");
        assert_eq!(response_payloads.len(), 4);
        assert!(applied.iter().any(|table| table.table == "projects"));
        assert_eq!(table_row_count(&local_conn, "sources").await.unwrap(), 1);
        assert_eq!(table_row_count(&local_conn, "entities").await.unwrap(), 1);
        assert_eq!(table_row_count(&local_conn, "edges").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn api_push_applies_index_embedding_and_session_payloads() {
        let local = TestStore::new("hugr_api_push_index_local");
        let remote = TestStore::new("hugr_api_push_index_remote");
        local
            .store
            .remember("api index payload memory")
            .await
            .unwrap();
        local
            .store
            .record_discovered_files(&[FileCandidate {
                path: "src/plugin_hooks.rs".to_string(),
                score: 1,
                language: Some("rust".to_string()),
                size_bytes: Some(128),
            }])
            .await
            .unwrap();
        let symbol = CodeSymbol {
            path: "src/plugin_hooks.rs".to_string(),
            name: "PluginHooks".to_string(),
            kind: "struct".to_string(),
            language: Some("rust".to_string()),
            line_start: 1,
            line_end: Some(3),
            signature: "pub struct PluginHooks".to_string(),
        };
        let reference = CodeReference {
            path: "src/main.rs".to_string(),
            language: Some("rust".to_string()),
            target_path: "src/plugin_hooks.rs".to_string(),
            target_name: "PluginHooks".to_string(),
            target_kind: "struct".to_string(),
            kind: "type_reference".to_string(),
            line_start: 9,
            excerpt: "PluginHooks".to_string(),
        };
        local
            .store
            .record_code_index(
                &[FileCandidate {
                    path: "src/plugin_hooks.rs".to_string(),
                    score: 1,
                    language: Some("rust".to_string()),
                    size_bytes: Some(128),
                }],
                std::slice::from_ref(&symbol),
                &[reference],
            )
            .await
            .unwrap();
        let session = local.store.start_session("sync api indexes").await.unwrap();
        local
            .store
            .end_session(Some("api index sync complete"))
            .await
            .unwrap();
        local
            .store
            .record_context_pack(
                "sync api indexes",
                r#"{"task":"sync api indexes","relevant_files":[]}"#,
            )
            .await
            .unwrap();
        let local_conn = local.store.connect().await.unwrap();
        let tables = vec![
            planned_sync_table_result(SyncTableKind::Memories, 1),
            planned_sync_table_result(SyncTableKind::MemoryEmbeddings, 1),
            planned_sync_table_result(SyncTableKind::DiscoveredFiles, 1),
            planned_sync_table_result(SyncTableKind::CodeSymbols, 1),
            planned_sync_table_result(SyncTableKind::CodeReferences, 1),
            planned_sync_table_result(SyncTableKind::Sessions, 1),
            planned_sync_table_result(SyncTableKind::ContextPacks, 1),
        ];

        let payloads = local
            .store
            .sync_api_table_payloads(&local_conn, &tables, true)
            .await
            .unwrap();
        let (_, status, response_payloads) = remote
            .store
            .apply_api_sync_push_payloads(&payloads, false)
            .await
            .unwrap();
        let remote_conn = remote.store.connect().await.unwrap();

        assert_eq!(status, "accepted");
        assert!(
            response_payloads
                .iter()
                .all(|payload| payload.result.conflict_count == 0)
        );
        assert_eq!(
            table_row_count(&remote_conn, "memory_embeddings")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            table_row_count(&remote_conn, "discovered_files")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            table_row_count(&remote_conn, "code_symbols").await.unwrap(),
            1
        );
        assert_eq!(
            table_row_count(&remote_conn, "code_references")
                .await
                .unwrap(),
            1
        );
        assert_eq!(table_row_count(&remote_conn, "sessions").await.unwrap(), 1);
        assert_eq!(
            table_row_count(&remote_conn, "context_packs")
                .await
                .unwrap(),
            1
        );
        assert_eq!(session.task, "sync api indexes");
    }

    #[tokio::test]
    async fn api_pull_returns_and_applies_index_embedding_and_session_payloads() {
        let remote = TestStore::new("hugr_api_pull_index_remote");
        let local = TestStore::new("hugr_api_pull_index_local");
        remote
            .store
            .remember("api pull index memory")
            .await
            .unwrap();
        let symbol = CodeSymbol {
            path: "src/plugin_hooks.rs".to_string(),
            name: "PluginHooks".to_string(),
            kind: "struct".to_string(),
            language: Some("rust".to_string()),
            line_start: 1,
            line_end: Some(3),
            signature: "pub struct PluginHooks".to_string(),
        };
        let reference = CodeReference {
            path: "src/main.rs".to_string(),
            language: Some("rust".to_string()),
            target_path: "src/plugin_hooks.rs".to_string(),
            target_name: "PluginHooks".to_string(),
            target_kind: "struct".to_string(),
            kind: "type_reference".to_string(),
            line_start: 9,
            excerpt: "PluginHooks".to_string(),
        };
        let file = FileCandidate {
            path: "src/plugin_hooks.rs".to_string(),
            score: 1,
            language: Some("rust".to_string()),
            size_bytes: Some(128),
        };
        remote
            .store
            .record_discovered_files(std::slice::from_ref(&file))
            .await
            .unwrap();
        remote
            .store
            .record_code_index(std::slice::from_ref(&file), &[symbol], &[reference])
            .await
            .unwrap();
        remote
            .store
            .start_session("pull api indexes")
            .await
            .unwrap();
        remote
            .store
            .end_session(Some("pull api indexes complete"))
            .await
            .unwrap();
        remote
            .store
            .record_context_pack(
                "pull api indexes",
                r#"{"task":"pull api indexes","relevant_files":[]}"#,
            )
            .await
            .unwrap();
        local.store.init().await.unwrap();
        let local_conn = local.store.connect().await.unwrap();
        let request_payloads = vec![
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::Memories, 0),
                records: Vec::new(),
            },
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::MemoryEmbeddings, 0),
                records: Vec::new(),
            },
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::DiscoveredFiles, 0),
                records: Vec::new(),
            },
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::CodeSymbols, 0),
                records: Vec::new(),
            },
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::CodeReferences, 0),
                records: Vec::new(),
            },
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::Sessions, 0),
                records: Vec::new(),
            },
            SyncApiTablePayload {
                result: planned_sync_table_result(SyncTableKind::ContextPacks, 0),
                records: Vec::new(),
            },
        ];

        let (_, status, response_payloads) = remote
            .store
            .api_sync_pull_payloads(&request_payloads, false)
            .await
            .unwrap();
        let applied = apply_api_pull_payloads(&local_conn, &response_payloads)
            .await
            .unwrap();

        assert_eq!(status, "accepted");
        assert!(applied.iter().all(|table| table.conflict_count == 0));
        assert_eq!(
            table_row_count(&local_conn, "memory_embeddings")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            table_row_count(&local_conn, "discovered_files")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            table_row_count(&local_conn, "code_symbols").await.unwrap(),
            1
        );
        assert_eq!(
            table_row_count(&local_conn, "code_references")
                .await
                .unwrap(),
            1
        );
        assert_eq!(table_row_count(&local_conn, "sessions").await.unwrap(), 1);
        assert_eq!(
            table_row_count(&local_conn, "context_packs").await.unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn remote_storage_mode_does_not_open_local_database() {
        let mut test = TestStore::new("remote_mode");
        test.store.storage_config = Ok(StorageConfig {
            mode: StorageMode::Remote,
            backend: SyncBackend::HugrApi,
            remote_url: Some("https://hugr.example".to_string()),
            remote_auth_token: Some("secret-token".to_string()),
            auth_token_configured: true,
            sync_classes: vec![SyncClass::Memories],
        });

        let error = test.store.init().await.unwrap_err();

        assert!(error.contains("hosted Hugr API storage operations"));
        assert!(!test.store.db_path().exists());
    }

    #[tokio::test]
    async fn sync_push_dry_run_counts_selected_tables() {
        let test = TestStore::new("sync_push_dry_run");
        test.store
            .remember("plugin hooks are loaded")
            .await
            .unwrap();
        test.store
            .record_discovered_files(&[FileCandidate {
                path: "src/plugin_hooks.rs".to_string(),
                score: 1,
                language: Some("rust".to_string()),
                size_bytes: Some(128),
            }])
            .await
            .unwrap();

        let result = test.store.sync_push(true).await.unwrap();
        let memories = result
            .tables
            .iter()
            .find(|table| table.table == "memories")
            .unwrap();
        let discovered_files = result
            .tables
            .iter()
            .find(|table| table.table == "discovered_files")
            .unwrap();

        assert!(result.dry_run);
        assert_eq!(memories.row_count, 1);
        assert_eq!(discovered_files.row_count, 1);
        assert!(!memories.executed);
    }

    #[tokio::test]
    async fn sync_copy_pushes_safe_tables_to_connection() {
        let source = TestStore::new("sync_source");
        let target = TestStore::new("sync_target");
        source
            .store
            .remember("plugin hooks are loaded")
            .await
            .unwrap();
        source
            .store
            .record_discovered_files(&[FileCandidate {
                path: "src/plugin_hooks.rs".to_string(),
                score: 1,
                language: Some("rust".to_string()),
                size_bytes: Some(128),
            }])
            .await
            .unwrap();
        target.store.init().await.unwrap();

        let source_conn = source.store.connect().await.unwrap();
        let target_conn = target.store.connect().await.unwrap();
        let config = StorageConfig {
            mode: StorageMode::Hybrid,
            backend: SyncBackend::DirectLibsql,
            remote_url: Some("libsql://example.turso.io".to_string()),
            remote_auth_token: Some("secret-token".to_string()),
            auth_token_configured: true,
            sync_classes: vec![
                SyncClass::Memories,
                SyncClass::Embeddings,
                SyncClass::Sources,
            ],
        };

        source
            .store
            .copy_sync_tables(&source_conn, &target_conn, &config)
            .await
            .unwrap();

        assert_eq!(table_row_count(&target_conn, "memories").await.unwrap(), 1);
        assert_eq!(
            table_row_count(&target_conn, "memory_embeddings")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            table_row_count(&target_conn, "discovered_files")
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn sync_pull_inserts_remote_memories_without_clobbering_local() {
        let local = TestStore::new("sync_pull_local_memory");
        let remote = TestStore::new("sync_pull_remote_memory");
        local.store.init().await.unwrap();
        remote.store.init().await.unwrap();
        let local_conn = local.store.connect().await.unwrap();
        let remote_conn = remote.store.connect().await.unwrap();

        insert_memory_for_sync(&local_conn, "mem_shared", "local memory", 10)
            .await
            .unwrap();
        insert_memory_for_sync(&remote_conn, "mem_shared", "remote memory", 20)
            .await
            .unwrap();
        insert_memory_for_sync(&remote_conn, "mem_remote", "remote only", 30)
            .await
            .unwrap();

        let config = StorageConfig {
            mode: StorageMode::Hybrid,
            backend: SyncBackend::DirectLibsql,
            remote_url: Some("libsql://example.turso.io".to_string()),
            remote_auth_token: Some("secret-token".to_string()),
            auth_token_configured: true,
            sync_classes: vec![SyncClass::Memories],
        };

        local
            .store
            .copy_pull_tables(&remote_conn, &local_conn, &config)
            .await
            .unwrap();

        assert_eq!(memory_text(&local_conn, "mem_shared").await, "local memory");
        assert_eq!(memory_text(&local_conn, "mem_remote").await, "remote only");
    }

    #[tokio::test]
    async fn sync_pull_reports_and_records_conflicts() {
        let local = TestStore::new("sync_pull_history_local");
        let remote = TestStore::new("sync_pull_history_remote");
        local.store.init().await.unwrap();
        remote.store.init().await.unwrap();
        let local_conn = local.store.connect().await.unwrap();
        let remote_conn = remote.store.connect().await.unwrap();

        insert_memory_for_sync(&local_conn, "mem_shared", "local memory", 10)
            .await
            .unwrap();
        insert_memory_for_sync(&remote_conn, "mem_shared", "remote memory", 20)
            .await
            .unwrap();
        insert_memory_for_sync(&remote_conn, "mem_remote", "remote only", 30)
            .await
            .unwrap();

        let config = StorageConfig {
            mode: StorageMode::Hybrid,
            backend: SyncBackend::DirectLibsql,
            remote_url: Some("libsql://example.turso.io".to_string()),
            remote_auth_token: Some("secret-token".to_string()),
            auth_token_configured: true,
            sync_classes: vec![SyncClass::Memories],
        };

        let tables = local
            .store
            .copy_pull_tables(&remote_conn, &local_conn, &config)
            .await
            .unwrap();
        let memories = tables
            .iter()
            .find(|table| table.table == "memories")
            .unwrap();

        assert_eq!(memories.inserted_count, 1);
        assert_eq!(memories.updated_count, 0);
        assert_eq!(memories.skipped_count, 1);
        assert_eq!(memories.conflict_count, 1);
        assert_eq!(memories.conflicts[0].reason, "local_row_preserved");

        let run_id = local
            .store
            .record_sync_run(
                &local_conn,
                "pull",
                "direct_libsql",
                "executed",
                100,
                200,
                &tables,
            )
            .await
            .unwrap();
        let history = local.store.sync_history(5).await.unwrap();

        assert_eq!(history[0].id, run_id);
        assert_eq!(history[0].operation, "pull");
        let history_memories = history[0]
            .tables
            .iter()
            .find(|table| table.table == "memories")
            .unwrap();
        assert_eq!(history_memories.skipped_count, 1);
        assert_eq!(
            history_memories.conflicts,
            vec![SyncConflictSummary {
                reason: "local_row_preserved".to_string(),
                count: 1,
            }]
        );
    }

    #[tokio::test]
    async fn sync_pull_updates_code_indexes_only_when_remote_is_newer() {
        let local = TestStore::new("sync_pull_local_code");
        let remote = TestStore::new("sync_pull_remote_code");
        local.store.init().await.unwrap();
        remote.store.init().await.unwrap();
        let local_conn = local.store.connect().await.unwrap();
        let remote_conn = remote.store.connect().await.unwrap();

        insert_code_symbol_for_sync(&local_conn, "PluginHooks", "local signature", 10)
            .await
            .unwrap();
        insert_code_symbol_for_sync(&remote_conn, "PluginHooks", "remote signature", 20)
            .await
            .unwrap();
        insert_code_symbol_for_sync(&local_conn, "StableHooks", "local stable", 30)
            .await
            .unwrap();
        insert_code_symbol_for_sync(&remote_conn, "StableHooks", "remote stale", 5)
            .await
            .unwrap();

        let config = StorageConfig {
            mode: StorageMode::Hybrid,
            backend: SyncBackend::DirectLibsql,
            remote_url: Some("libsql://example.turso.io".to_string()),
            remote_auth_token: Some("secret-token".to_string()),
            auth_token_configured: true,
            sync_classes: vec![SyncClass::Entities],
        };

        local
            .store
            .copy_pull_tables(&remote_conn, &local_conn, &config)
            .await
            .unwrap();

        assert_eq!(
            code_symbol_signature(&local_conn, "PluginHooks").await,
            "remote signature"
        );
        assert_eq!(
            code_symbol_signature(&local_conn, "StableHooks").await,
            "local stable"
        );
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
        assert!(object_exists(&conn, "table", "session_promotions").await);
        assert!(object_exists(&conn, "table", "context_packs").await);
        assert!(object_exists(&conn, "table", "diagnostics").await);
        assert!(object_exists(&conn, "table", "code_symbols").await);
        assert!(object_exists(&conn, "table", "code_references").await);
        assert!(object_exists(&conn, "table", "test_mappings").await);
        assert!(object_exists(&conn, "table", "source_embeddings").await);
        assert!(object_exists(&conn, "table", "sync_runs").await);
        assert!(object_exists(&conn, "table", "sync_table_runs").await);
        assert!(object_exists(&conn, "table", "sync_table_conflicts").await);
        assert!(object_exists(&conn, "index", "code_symbols_project_name_idx").await);
        assert!(object_exists(&conn, "index", "code_references_target_name_idx").await);
        assert!(object_exists(&conn, "index", "test_mappings_source_idx").await);
        assert!(object_exists(&conn, "index", "source_embeddings_project_idx").await);
        assert!(object_exists(&conn, "index", "sync_runs_started_idx").await);
        assert!(object_exists(&conn, "index", "context_packs_project_updated_idx").await);
        assert!(object_exists(&conn, "index", "diagnostics_project_created_idx").await);
        assert!(object_exists(&conn, "index", "diagnostics_project_path_idx").await);
        assert!(object_exists(&conn, "index", "memory_embeddings_vector_idx").await);
        assert!(object_exists(&conn, "index", "source_embeddings_vector_idx").await);

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
                (6, "code_references".to_string()),
                (7, "sync_history".to_string()),
                (8, "session_promotions".to_string()),
                (9, "context_packs".to_string()),
                (10, "diagnostics".to_string()),
                (11, "test_mappings".to_string()),
                (12, "source_embeddings".to_string())
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
    async fn forget_retires_memories_without_deleting_rows() {
        let test = TestStore::new("forget");
        test.store.init().await.unwrap();
        let conn = test.store.connect().await.unwrap();
        insert_memory_for_sync(
            &conn,
            "mem_forget",
            "plugin hooks run after configuration is loaded",
            10,
        )
        .await
        .unwrap();
        insert_memory_for_sync(&conn, "mem_keep", "database migrations are recorded", 20)
            .await
            .unwrap();
        let forgotten = Memory {
            id: "mem_forget".to_string(),
            created_at_ms: 10,
            kind: "fact".to_string(),
            text: "plugin hooks run after configuration is loaded".to_string(),
            structured_payload: None,
        };
        let kept = Memory {
            id: "mem_keep".to_string(),
            created_at_ms: 20,
            kind: "fact".to_string(),
            text: "database migrations are recorded".to_string(),
            structured_payload: None,
        };

        let result = test.store.forget("plugin hooks", 25).await.unwrap();
        let active = test.store.memories().await.unwrap();
        let recalled = test.store.recall("plugin hooks", 5).await.unwrap();
        let report = test.store.memory_maintenance_report().await.unwrap();

        assert_eq!(result.forgotten_count, 1);
        assert_eq!(result.memories, vec![forgotten.clone()]);
        assert_eq!(active, vec![kept]);
        assert!(recalled.is_empty());
        assert_eq!(report.active_count, 1);
        assert_eq!(report.retired_count, 1);
        assert_eq!(table_row_count(&conn, "memories").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn memory_maintenance_report_groups_duplicate_active_memories() {
        let test = TestStore::new("improve");
        test.store.init().await.unwrap();
        let conn = test.store.connect().await.unwrap();
        insert_memory_for_sync(&conn, "mem_1", "Plugin hooks are loaded", 10)
            .await
            .unwrap();
        insert_memory_for_sync(&conn, "mem_2", "plugin hooks are loaded", 20)
            .await
            .unwrap();
        insert_memory_for_sync(&conn, "mem_3", "database migrations are recorded", 30)
            .await
            .unwrap();

        let report = test.store.memory_maintenance_report().await.unwrap();

        assert_eq!(report.active_count, 3);
        assert_eq!(report.retired_count, 0);
        assert_eq!(report.duplicate_groups.len(), 1);
        assert_eq!(
            report.duplicate_groups[0].normalized_text,
            "plugin hooks are loaded"
        );
        assert_eq!(report.duplicate_groups[0].memories.len(), 2);
    }

    #[tokio::test]
    async fn memory_maintenance_report_flags_opposing_memory_terms() {
        let test = TestStore::new("stale_candidates");
        test.store.init().await.unwrap();
        let conn = test.store.connect().await.unwrap();
        insert_memory_for_sync(
            &conn,
            "mem_old",
            "plugin hooks run after configuration is loaded",
            10,
        )
        .await
        .unwrap();
        insert_memory_for_sync(
            &conn,
            "mem_new",
            "plugin hooks now run before configuration is loaded",
            20,
        )
        .await
        .unwrap();
        insert_memory_for_sync(&conn, "mem_other", "database migrations are recorded", 30)
            .await
            .unwrap();

        let report = test.store.memory_maintenance_report().await.unwrap();

        assert_eq!(report.stale_candidates.len(), 1);
        let candidate = &report.stale_candidates[0];
        assert_eq!(candidate.reason, "opposing_terms");
        assert_eq!(candidate.signal, "after_vs_before");
        assert_eq!(candidate.newer_memory.id, "mem_new");
        assert_eq!(candidate.older_memory.id, "mem_old");
        assert!(candidate.shared_terms.contains(&"plugin".to_string()));
        assert!(candidate.shared_terms.contains(&"hooks".to_string()));
    }

    #[tokio::test]
    async fn retire_stale_memories_retires_older_candidates() {
        let test = TestStore::new("stale_execute");
        test.store.init().await.unwrap();
        let conn = test.store.connect().await.unwrap();
        insert_memory_for_sync(
            &conn,
            "mem_old",
            "plugin hooks run after configuration is loaded",
            10,
        )
        .await
        .unwrap();
        insert_memory_for_sync(
            &conn,
            "mem_new",
            "plugin hooks now run before configuration is loaded",
            20,
        )
        .await
        .unwrap();

        let result = test.store.retire_stale_memories().await.unwrap();
        let report = test.store.memory_maintenance_report().await.unwrap();
        let active_ids = test
            .store
            .memories()
            .await
            .unwrap()
            .into_iter()
            .map(|memory| memory.id)
            .collect::<Vec<_>>();
        let mut rows = conn
            .query(
                "SELECT valid_to, superseded_by FROM memories WHERE id = 'mem_old'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();

        assert_eq!(result.kept_memories[0].id, "mem_new");
        assert_eq!(result.retired_memories[0].id, "mem_old");
        assert_eq!(report.active_count, 1);
        assert_eq!(report.retired_count, 1);
        assert!(report.stale_candidates.is_empty());
        assert_eq!(active_ids, vec!["mem_new".to_string()]);
        assert!(row.get::<Option<String>>(0).unwrap().is_some());
        assert_eq!(
            row.get::<Option<String>>(1).unwrap(),
            Some("mem_new".to_string())
        );
    }

    #[tokio::test]
    async fn consolidate_duplicate_memories_retires_older_duplicates() {
        let test = TestStore::new("improve_execute");
        test.store.init().await.unwrap();
        let conn = test.store.connect().await.unwrap();
        insert_memory_for_sync(&conn, "mem_old", "Plugin hooks are loaded", 10)
            .await
            .unwrap();
        insert_memory_for_sync(&conn, "mem_new", "plugin hooks are loaded", 20)
            .await
            .unwrap();
        insert_memory_for_sync(&conn, "mem_keep", "database migrations are recorded", 30)
            .await
            .unwrap();

        let result = test.store.consolidate_duplicate_memories().await.unwrap();
        let report = test.store.memory_maintenance_report().await.unwrap();
        let active_ids = test
            .store
            .memories()
            .await
            .unwrap()
            .into_iter()
            .map(|memory| memory.id)
            .collect::<Vec<_>>();
        let mut rows = conn
            .query(
                "SELECT valid_to, superseded_by FROM memories WHERE id = 'mem_old'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();

        assert_eq!(result.kept_memories[0].id, "mem_new");
        assert_eq!(result.retired_memories[0].id, "mem_old");
        assert_eq!(report.active_count, 2);
        assert_eq!(report.retired_count, 1);
        assert!(report.duplicate_groups.is_empty());
        assert_eq!(
            active_ids,
            vec!["mem_keep".to_string(), "mem_new".to_string()]
        );
        assert!(row.get::<Option<String>>(0).unwrap().is_some());
        assert_eq!(
            row.get::<Option<String>>(1).unwrap(),
            Some("mem_new".to_string())
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
    async fn remember_with_options_preserves_structured_metadata() {
        let test = TestStore::new("memory_source");
        let memory = test
            .store
            .remember_with_options(
                "plugin hooks are documented",
                MemoryWriteOptions {
                    source: Some(MemorySource {
                        kind: "file".to_string(),
                        locator: "docs/plugins.md".to_string(),
                    }),
                    confidence: Some(0.75),
                    sensitivity: Some("private".to_string()),
                    valid_from: Some("2026-01-01".to_string()),
                    valid_to: Some("2026-12-31".to_string()),
                },
            )
            .await
            .unwrap();
        let payload = serde_json::from_str::<serde_json::Value>(
            memory.structured_payload.as_deref().unwrap(),
        )
        .unwrap();

        assert_eq!(payload["source"]["type"], "manual");
        assert_eq!(payload["source"]["kind"], "file");
        assert_eq!(payload["source"]["locator"], "docs/plugins.md");
        assert_eq!(payload["project"]["id"], LOCAL_PROJECT_ID);
        assert!(payload["project"]["root_path"].as_str().is_some());
        assert_eq!(payload["metadata"]["confidence"], 0.75);
        assert_eq!(payload["metadata"]["sensitivity"], "private");
        assert_eq!(payload["metadata"]["validity"]["valid_from"], "2026-01-01");
        assert_eq!(payload["metadata"]["validity"]["valid_to"], "2026-12-31");

        let conn = test.store.connect().await.unwrap();
        let mut rows = conn
            .query(
                "SELECT confidence, sensitivity FROM memories WHERE id = ?1",
                params![memory.id.clone()],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<f64>(0).unwrap(), 0.75);
        assert_eq!(row.get::<String>(1).unwrap(), "private");

        let memories = test
            .store
            .recall("plugin hooks documented", 5)
            .await
            .unwrap();
        let recalled = memories
            .iter()
            .find(|candidate| candidate.id == memory.id)
            .unwrap();
        let recalled_payload = serde_json::from_str::<serde_json::Value>(
            recalled.structured_payload.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(recalled_payload["source"]["locator"], "docs/plugins.md");
        assert_eq!(recalled_payload["project"]["id"], LOCAL_PROJECT_ID);
        assert_eq!(recalled_payload["metadata"]["sensitivity"], "private");
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
    async fn recall_symbols_finds_targets_beyond_recency_windows() {
        let test = TestStore::new("symbol_prefilter");
        let target_file = FileCandidate {
            path: "src/zz_special.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(120),
        };
        let target = CodeSymbol {
            path: target_file.path.clone(),
            language: target_file.language.clone(),
            name: "special_target_fn".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: None,
            signature: "pub fn special_target_fn()".to_string(),
        };
        test.store
            .record_code_index(
                std::slice::from_ref(&target_file),
                std::slice::from_ref(&target),
                &[],
            )
            .await
            .unwrap();

        // Newer filler rows in excess of the old blind LIMIT 2000 window, so a
        // recency-ordered scan without a prefilter would never see the target.
        let filler_file = FileCandidate {
            path: "src/aa_filler.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(120),
        };
        let fillers = (0..2100)
            .map(|index| CodeSymbol {
                path: filler_file.path.clone(),
                language: filler_file.language.clone(),
                name: format!("filler_helper_{index}"),
                kind: "function".to_string(),
                line_start: index + 1,
                line_end: None,
                signature: format!("fn filler_helper_{index}()"),
            })
            .collect::<Vec<_>>();
        test.store
            .record_code_index(std::slice::from_ref(&filler_file), &fillers, &[])
            .await
            .unwrap();

        let matches = test
            .store
            .recall_symbols("special target", 5)
            .await
            .unwrap();
        assert_eq!(
            matches.first().map(|symbol| symbol.name.as_str()),
            Some("special_target_fn"),
            "prefiltered recall must reach symbols older than the recency window"
        );

        let stemmed = test.store.recall_symbols("targeting", 5).await.unwrap();
        assert!(
            stemmed
                .iter()
                .any(|symbol| symbol.name == "special_target_fn"),
            "stem matches must survive the SQL prefilter"
        );
    }

    #[tokio::test]
    async fn record_code_index_persists_test_mappings_and_source_embeddings() {
        let test = TestStore::new("code_index_metadata");
        let source_file = FileCandidate {
            path: "src/plugin_hooks.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(120),
        };
        let test_file = FileCandidate {
            path: "tests/plugin_hooks.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(90),
        };
        let symbol = CodeSymbol {
            path: source_file.path.clone(),
            language: source_file.language.clone(),
            name: "PluginHooks".to_string(),
            kind: "struct".to_string(),
            line_start: 3,
            line_end: None,
            signature: "pub struct PluginHooks".to_string(),
        };

        test.store
            .record_discovered_files(&[source_file.clone(), test_file])
            .await
            .unwrap();
        test.store
            .record_code_index(
                std::slice::from_ref(&source_file),
                std::slice::from_ref(&symbol),
                &[],
            )
            .await
            .unwrap();

        let conn = test.store.connect().await.unwrap();
        let mut mappings = conn
            .query(
                "
                SELECT test_path, reason, score
                FROM test_mappings
                WHERE project_id = ?1 AND source_path = ?2
                ",
                params![LOCAL_PROJECT_ID, source_file.path.clone()],
            )
            .await
            .unwrap();
        let mapping = mappings.next().await.unwrap().unwrap();
        assert_eq!(mapping.get::<String>(0).unwrap(), "tests/plugin_hooks.rs");
        assert!(mapping.get::<String>(1).unwrap().contains("tests"));
        assert!(mapping.get::<i64>(2).unwrap() > 0);
        assert!(mappings.next().await.unwrap().is_none());

        let mut embeddings = conn
            .query(
                "
                SELECT model, dimensions, length(embedding), content_key
                FROM source_embeddings
                WHERE project_id = ?1 AND path = ?2
                ",
                params![LOCAL_PROJECT_ID, source_file.path.clone()],
            )
            .await
            .unwrap();
        let embedding = embeddings.next().await.unwrap().unwrap();
        assert_eq!(embedding.get::<String>(0).unwrap(), DETERMINISTIC_MODEL);
        assert_eq!(
            embedding.get::<i64>(1).unwrap(),
            DEFAULT_EMBEDDING_DIMENSIONS as i64
        );
        assert_eq!(
            embedding.get::<i64>(2).unwrap(),
            (DEFAULT_EMBEDDING_DIMENSIONS * 4) as i64
        );
        assert!(!embedding.get::<String>(3).unwrap().is_empty());
        assert!(embeddings.next().await.unwrap().is_none());

        let likely = test
            .store
            .likely_tests_for_files(&[source_file.path.clone()], 5)
            .await
            .unwrap();
        assert_eq!(likely[0].path, "tests/plugin_hooks.rs");
    }

    #[tokio::test]
    async fn source_embedding_file_candidates_match_symbol_summaries() {
        let test = TestStore::new("source_embedding_candidates");
        let file = FileCandidate {
            path: "src/payments.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(160),
        };
        let symbol = CodeSymbol {
            path: file.path.clone(),
            language: file.language.clone(),
            name: "InvoiceLedger".to_string(),
            kind: "struct".to_string(),
            line_start: 4,
            line_end: Some(6),
            signature: "pub struct InvoiceLedger".to_string(),
        };

        test.store
            .record_discovered_files(std::slice::from_ref(&file))
            .await
            .unwrap();
        test.store
            .record_code_index(
                std::slice::from_ref(&file),
                std::slice::from_ref(&symbol),
                &[],
            )
            .await
            .unwrap();

        let candidates = test
            .store
            .source_embedding_file_candidates("invoice ledger balancing", 5)
            .await
            .unwrap();

        assert_eq!(candidates[0].path, "src/payments.rs");
        assert_eq!(candidates[0].language.as_deref(), Some("rust"));
        assert!(discovery::source_embedding_rank(candidates[0].score).is_some());
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
    async fn prune_missing_index_rows_removes_deleted_files_and_dangling_references() {
        let test = TestStore::new("prune_missing");
        // The kept file exists on disk; the moved-away file does not.
        let kept_dir = test.workspace.join("src");
        fs::create_dir_all(&kept_dir).unwrap();
        fs::write(kept_dir.join("main.rs"), "fn main() {}\n").unwrap();

        let kept = FileCandidate {
            path: "src/main.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(80),
        };
        let deleted = FileCandidate {
            path: "src/deleted.rs".to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: Some(120),
        };
        let kept_symbol = CodeSymbol {
            path: kept.path.clone(),
            language: kept.language.clone(),
            name: "main".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: Some(1),
            signature: "fn main()".to_string(),
        };
        let deleted_symbol = CodeSymbol {
            path: deleted.path.clone(),
            language: deleted.language.clone(),
            name: "gone_helper".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: Some(3),
            signature: "fn gone_helper()".to_string(),
        };
        // A dangling edge: the kept file references a symbol in the deleted file.
        let dangling_reference = CodeReference {
            path: kept.path.clone(),
            language: kept.language.clone(),
            target_path: deleted.path.clone(),
            target_name: "gone_helper".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 1,
            excerpt: "gone_helper();".to_string(),
        };

        test.store
            .record_discovered_files(&[kept.clone(), deleted.clone()])
            .await
            .unwrap();
        test.store
            .record_code_index(
                &[kept, deleted],
                &[kept_symbol.clone(), deleted_symbol],
                &[dangling_reference],
            )
            .await
            .unwrap();

        let summary = test
            .store
            .prune_missing_index_rows(&test.workspace)
            .await
            .unwrap();

        assert_eq!(summary.missing_paths, 1);
        assert_eq!(summary.symbols, 1);
        assert_eq!(summary.discovered_files, 1);
        // Removes both the deleted file's own row (none here) and the dangling edge.
        assert_eq!(summary.references, 1);

        // The kept file's symbol survives; the deleted file's symbol is gone.
        let kept_matches = test.store.symbols_for_target("main", 5).await.unwrap();
        assert_eq!(kept_matches, vec![kept_symbol]);
        assert!(
            test.store
                .symbols_for_target("gone_helper", 5)
                .await
                .unwrap()
                .is_empty()
        );
        // The dangling reference from the kept file is gone.
        assert!(
            test.store
                .references_from_path("src/main.rs", 5)
                .await
                .unwrap()
                .is_empty()
        );

        // A second prune with nothing missing is a no-op.
        let second = test
            .store
            .prune_missing_index_rows(&test.workspace)
            .await
            .unwrap();
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn context_graph_neighbors_include_references_and_entities() {
        let test = TestStore::new("context_graph_neighbors");
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
        let conn = test.store.connect().await.unwrap();
        insert_source_for_sync(&conn, "src_1", "file", "src/plugin_hooks.rs", 10).await;
        insert_entity_for_sync(
            &conn,
            "ent_plugin",
            "function",
            "run_after_config",
            Some("src/plugin_hooks.rs"),
            11,
        )
        .await;
        insert_entity_for_sync(&conn, "ent_main", "file", "main", Some("src/main.rs"), 12).await;
        insert_edge_for_sync(&conn, "edge_1", "ent_main", "ent_plugin", "references", 13).await;

        let neighbors = test
            .store
            .context_graph_neighbors(
                "plugin hooks run after config",
                &[source.path],
                std::slice::from_ref(&symbol),
                10,
            )
            .await
            .unwrap();

        assert!(neighbors.iter().any(|neighbor| {
            neighbor.kind == "incoming_reference"
                && neighbor
                    .target_name
                    .as_deref()
                    .is_some_and(|name| name == "run_after_config")
        }));
        assert!(neighbors.iter().any(|neighbor| {
            neighbor.kind == "entity" && neighbor.label.contains("run_after_config")
        }));
        assert!(neighbors.iter().any(|neighbor| {
            neighbor.kind == "edge" && neighbor.label.contains("-references->")
        }));
        assert!(
            neighbors.iter().any(
                |neighbor| neighbor.kind == "source" && neighbor.label.contains("plugin_hooks")
            )
        );
    }

    #[tokio::test]
    async fn context_freshness_signals_report_missing_index() {
        let test = TestStore::new("context_freshness_missing");
        let source_dir = test.workspace.join("src");
        fs::create_dir_all(&source_dir).unwrap();
        let source_path = source_dir.join("plugin_hooks.rs");
        fs::write(&source_path, "pub fn run_after_config() {}\n").unwrap();
        let source_path = source_path.to_string_lossy().to_string();

        let signals = test
            .store
            .context_freshness_signals(std::slice::from_ref(&source_path), &[], 5)
            .await
            .unwrap();

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].path, source_path);
        assert_eq!(signals[0].kind, "missing_index");
        assert!(signals[0].modified_at_ms.is_some());
    }

    #[tokio::test]
    async fn context_freshness_signals_report_edits_after_context() {
        let test = TestStore::new("context_freshness_edit_after_context");
        let source_dir = test.workspace.join("src");
        fs::create_dir_all(&source_dir).unwrap();
        let source_path = source_dir.join("plugin_hooks.rs");
        fs::write(&source_path, "pub fn run_after_config() {}\n").unwrap();
        let source_path = source_path.to_string_lossy().to_string();

        test.store
            .record_context_pack("plugin hooks", "{}")
            .await
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        test.store.start_session("edit plugin hooks").await.unwrap();
        test.store
            .record_session_event(
                "edit",
                &format!(
                    "replace-symbol function run_after_config at {source_path}:1-1 -> {source_path}:1-1"
                ),
            )
            .await
            .unwrap();

        let signals = test
            .store
            .context_freshness_signals(std::slice::from_ref(&source_path), &[], 10)
            .await
            .unwrap();
        let edit_signal = signals
            .iter()
            .find(|signal| signal.kind == "edit_after_context")
            .expect("edit-after-context signal should be present");

        assert_eq!(edit_signal.path, source_path);
        assert!(edit_signal.detail.contains("latest persisted context pack"));
        assert!(edit_signal.indexed_at_ms < edit_signal.modified_at_ms);
    }

    #[tokio::test]
    async fn records_and_recalls_structured_diagnostics() {
        let test = TestStore::new("diagnostics");
        let written = test
            .store
            .record_diagnostics(&[DiagnosticInput {
                source: "command_stderr".to_string(),
                path: Some("src/plugin_hooks.rs".to_string()),
                line_start: Some(12),
                line_end: None,
                severity: "error".to_string(),
                code: Some("E0425".to_string()),
                message: "cannot find value hook in this scope".to_string(),
                command: Some("cargo test".to_string()),
            }])
            .await
            .unwrap();

        let recalled = test
            .store
            .recent_diagnostics(
                "fix plugin hooks",
                &["src/plugin_hooks.rs".to_string()],
                &[],
                5,
            )
            .await
            .unwrap();
        let unrelated = test
            .store
            .recent_diagnostics("database migrations", &[], &[], 5)
            .await
            .unwrap();

        assert_eq!(written.len(), 1);
        assert_eq!(written[0].severity, "error");
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].path.as_deref(), Some("src/plugin_hooks.rs"));
        assert_eq!(recalled[0].line_start, Some(12));
        assert_eq!(recalled[0].code.as_deref(), Some("E0425"));
        assert!(unrelated.is_empty());
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

    #[tokio::test]
    async fn records_session_event_only_when_session_is_active() {
        let test = TestStore::new("optional_session_event");

        let skipped = test
            .store
            .record_session_event_if_active("daemon_observation", "files changed: src/lib.rs")
            .await
            .unwrap();
        assert!(skipped.is_none());

        let session = test
            .store
            .start_session("observe daemon edits")
            .await
            .unwrap();
        let recorded = test
            .store
            .record_session_event_if_active("daemon_observation", "files changed: src/lib.rs")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(recorded.session_id, session.id);
        assert_eq!(recorded.kind, "daemon_observation");

        let facts = test
            .store
            .recent_session_facts("daemon edits", 5)
            .await
            .unwrap();
        assert!(facts.iter().any(|fact| {
            fact.kind == "daemon_observation" && fact.detail.contains("src/lib.rs")
        }));
    }

    #[tokio::test]
    async fn promotes_latest_session_facts_to_memory() {
        let test = TestStore::new("session_promotion");
        test.store
            .start_session("stabilize plugin registry")
            .await
            .unwrap();
        test.store
            .record_session_event("command", "command: cargo test; status: 0")
            .await
            .unwrap();

        let promoted = test.store.promote_latest_session().await.unwrap();

        assert_eq!(promoted.fact_count, 1);
        assert_eq!(promoted.memory.kind, "fact");
        assert!(promoted.memory.text.contains("stabilize plugin registry"));
        assert!(
            promoted
                .memory
                .text
                .contains("command: cargo test; status: 0")
        );
        assert!(!promoted.memory.text.contains("command: command:"));
        let payload = serde_json::from_str::<serde_json::Value>(
            promoted.memory.structured_payload.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(payload["source"]["type"], "session_promotion");
        assert_eq!(payload["project"]["id"], LOCAL_PROJECT_ID);
        assert_eq!(
            payload["source"]["session_id"],
            promoted.session_id.as_str()
        );
        assert_eq!(payload["source"]["task"], "stabilize plugin registry");
        assert_eq!(payload["facts"][0]["kind"], "command");
        assert_eq!(
            payload["facts"][0]["detail"],
            "command: cargo test; status: 0"
        );

        let memories = test
            .store
            .recall("plugin registry cargo test", 5)
            .await
            .unwrap();
        assert!(
            memories.iter().any(
                |memory| memory.id == promoted.memory.id && memory.structured_payload.is_some()
            )
        );
    }

    #[tokio::test]
    async fn promotes_latest_session_only_once() {
        let test = TestStore::new("session_promotion_idempotent");
        test.store
            .start_session("stabilize plugin registry")
            .await
            .unwrap();
        test.store
            .record_session_event("command", "command: cargo test; status: 0")
            .await
            .unwrap();

        let first = test.store.promote_latest_session().await.unwrap();
        let second = test.store.promote_latest_session().await.unwrap();
        let conn = test.store.connect().await.unwrap();

        assert_eq!(second.memory.id, first.memory.id);
        assert_eq!(table_row_count(&conn, "memories").await.unwrap(), 1);
        assert_eq!(
            table_row_count(&conn, "session_promotions").await.unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn promotes_next_unpromoted_ended_session() {
        let test = TestStore::new("session_auto_promotion");
        test.store
            .start_session("capture plugin registry discovery")
            .await
            .unwrap();
        test.store
            .record_session_event("command", "command: cargo test plugin_registry; status: 0")
            .await
            .unwrap();
        test.store
            .end_session(Some("plugin registry tests passed"))
            .await
            .unwrap();

        let promoted = test
            .store
            .promote_next_unpromoted_session()
            .await
            .unwrap()
            .unwrap();
        let skipped = test.store.promote_next_unpromoted_session().await.unwrap();

        assert!(skipped.is_none());
        assert_eq!(promoted.fact_count, 2);
        assert!(
            promoted
                .memory
                .text
                .contains("plugin registry tests passed")
        );
        assert!(
            promoted
                .memory
                .text
                .contains("command: cargo test plugin_registry; status: 0")
        );
    }

    async fn insert_memory_for_sync(
        conn: &Connection,
        id: &str,
        text: &str,
        created_at_ms: i64,
    ) -> Result<(), String> {
        conn.execute(
            "
            INSERT INTO memories (id, created_at_ms, kind, text)
            VALUES (?1, ?2, 'fact', ?3)
            ",
            params![id.to_string(), created_at_ms, text.to_string()],
        )
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    async fn insert_source_for_sync(
        conn: &Connection,
        id: &str,
        kind: &str,
        locator: &str,
        created_at_ms: i64,
    ) {
        conn.execute(
            "
            INSERT INTO sources (id, kind, locator, created_at_ms)
            VALUES (?1, ?2, ?3, ?4)
            ",
            params![
                id.to_string(),
                kind.to_string(),
                locator.to_string(),
                created_at_ms
            ],
        )
        .await
        .unwrap();
    }

    async fn insert_entity_for_sync(
        conn: &Connection,
        id: &str,
        kind: &str,
        name: &str,
        locator: Option<&str>,
        created_at_ms: i64,
    ) {
        conn.execute(
            "
            INSERT INTO entities (id, kind, name, locator, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                id.to_string(),
                kind.to_string(),
                name.to_string(),
                locator.map(|value| value.to_string()),
                created_at_ms
            ],
        )
        .await
        .unwrap();
    }

    async fn insert_edge_for_sync(
        conn: &Connection,
        id: &str,
        from_id: &str,
        to_id: &str,
        kind: &str,
        created_at_ms: i64,
    ) {
        conn.execute(
            "
            INSERT INTO edges (id, from_id, to_id, kind, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                id.to_string(),
                from_id.to_string(),
                to_id.to_string(),
                kind.to_string(),
                created_at_ms
            ],
        )
        .await
        .unwrap();
    }

    async fn memory_text(conn: &Connection, id: &str) -> String {
        let mut rows = conn
            .query(
                "SELECT text FROM memories WHERE id = ?1",
                params![id.to_string()],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get::<String>(0).unwrap()
    }

    async fn insert_code_symbol_for_sync(
        conn: &Connection,
        name: &str,
        signature: &str,
        indexed_at_ms: i64,
    ) -> Result<(), String> {
        conn.execute(
            "
            INSERT INTO code_symbols (
                project_id, path, name, kind, language, line_start, line_end,
                signature, indexed_at_ms
            )
            VALUES (?1, 'src/plugin_hooks.rs', ?2, 'struct', 'rust', 1, 1, ?3, ?4)
            ",
            params![
                LOCAL_PROJECT_ID,
                name.to_string(),
                signature.to_string(),
                indexed_at_ms
            ],
        )
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    async fn code_symbol_signature(conn: &Connection, name: &str) -> String {
        let mut rows = conn
            .query(
                "SELECT signature FROM code_symbols WHERE project_id = ?1 AND name = ?2",
                params![LOCAL_PROJECT_ID, name.to_string()],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get::<String>(0).unwrap()
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
