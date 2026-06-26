use libsql::{Builder, Connection, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const HUGR_DIR: &str = ".hugr";
const HUGR_DB: &str = "hugr.db";
const EMBEDDING_DIMENSIONS: i64 = 1536;

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

impl Store {
    pub fn open_current() -> Self {
        Self {
            root: PathBuf::from(HUGR_DIR),
        }
    }

    pub async fn init(&self) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.root.join("sessions")).map_err(|error| error.to_string())?;
        let conn = self.connect().await?;
        migrate(&conn).await?;
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

    pub async fn recall(&self, query: &str, limit: usize) -> Result<Vec<Memory>, String> {
        let terms = query_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }

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
        db.connect().map_err(|error| error.to_string())
    }
}

async fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(&format!(
        "
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY,
            created_at_ms INTEGER NOT NULL,
            kind TEXT NOT NULL,
            text TEXT NOT NULL,
            confidence REAL NOT NULL DEFAULT 1.0,
            valid_from TEXT,
            valid_to TEXT,
            superseded_by TEXT,
            sensitivity TEXT NOT NULL DEFAULT 'normal',
            structured_payload TEXT
        );

        CREATE TABLE IF NOT EXISTS memory_embeddings (
            memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
            model TEXT NOT NULL,
            dimensions INTEGER NOT NULL DEFAULT {EMBEDDING_DIMENSIONS},
            embedding F32_BLOB({EMBEDDING_DIMENSIONS})
        );

        CREATE INDEX IF NOT EXISTS memory_embeddings_vector_idx
        ON memory_embeddings (libsql_vector_idx(embedding));

        CREATE TABLE IF NOT EXISTS sources (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            locator TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS entities (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            locator TEXT,
            created_at_ms INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS edges (
            id TEXT PRIMARY KEY,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            id UNINDEXED,
            kind,
            text,
            content='memories',
            content_rowid='rowid'
        );

        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, id, kind, text)
            VALUES (new.rowid, new.id, new.kind, new.text);
        END;

        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, id, kind, text)
            VALUES ('delete', old.rowid, old.id, old.kind, old.text);
        END;

        CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, id, kind, text)
            VALUES ('delete', old.rowid, old.id, old.kind, old.text);
            INSERT INTO memories_fts(rowid, id, kind, text)
            VALUES (new.rowid, new.id, new.kind, new.text);
        END;
        "
    ))
    .await
    .map_err(|error| error.to_string())?;

    Ok(())
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
    use super::{Memory, query_terms, recall_score};

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
}
