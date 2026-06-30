use crate::discovery::FileCandidate;
use crate::embedding::{DeterministicEmbeddingProvider, EmbeddingProvider};
use crate::migrations;
use libsql::{Builder, Connection, params};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};

const HUGR_DIR: &str = ".hugr";
const HUGR_DB: &str = "hugr.db";
const LOCAL_PROJECT_ID: &str = "project_local";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub id: String,
    pub created_at_ms: i64,
    pub kind: String,
    pub text: String,
}

pub struct Store {
    root: PathBuf,
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
        }
    }

    #[cfg(test)]
    fn open_at(root: PathBuf) -> Self {
        Self { root }
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

        let embedding = DeterministicEmbeddingProvider::default().embed(&memory.text)?;
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
        let embedding = DeterministicEmbeddingProvider::default().embed(query)?;
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

    async fn connect(&self) -> Result<Connection, String> {
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
    use super::{Memory, Store, fts_query, query_terms, recall_score};
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
                (3, "file_discovery".to_string())
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
