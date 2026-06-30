use libsql::{Connection, params};
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

const EMBEDDING_DIMENSIONS: i64 = 1536;
const INITIAL_SCHEMA_VERSION: i64 = 1;
const INITIAL_SCHEMA_NAME: &str = "initial_schema";
const PROJECT_REGISTRY_VERSION: i64 = 2;
const PROJECT_REGISTRY_NAME: &str = "project_registry";
const FILE_DISCOVERY_VERSION: i64 = 3;
const FILE_DISCOVERY_NAME: &str = "file_discovery";

pub async fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        );
        ",
    )
    .await
    .map_err(|error| error.to_string())?;

    let applied = applied_migrations(conn).await?;
    if !applied.contains(&INITIAL_SCHEMA_VERSION) {
        conn.execute_batch(&initial_schema_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![INITIAL_SCHEMA_VERSION, INITIAL_SCHEMA_NAME, now_ms()?],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    if !applied.contains(&PROJECT_REGISTRY_VERSION) {
        conn.execute_batch(project_registry_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![PROJECT_REGISTRY_VERSION, PROJECT_REGISTRY_NAME, now_ms()?],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    if !applied.contains(&FILE_DISCOVERY_VERSION) {
        conn.execute_batch(file_discovery_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![FILE_DISCOVERY_VERSION, FILE_DISCOVERY_NAME, now_ms()?],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    Ok(())
}

async fn applied_migrations(conn: &Connection) -> Result<HashSet<i64>, String> {
    let mut rows = conn
        .query("SELECT version FROM schema_migrations", ())
        .await
        .map_err(|error| error.to_string())?;
    let mut applied = HashSet::new();

    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        applied.insert(row.get::<i64>(0).map_err(|error| error.to_string())?);
    }

    Ok(applied)
}

fn initial_schema_sql() -> String {
    format!(
        "
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

        INSERT INTO memories_fts(memories_fts) VALUES ('rebuild');
        "
    )
}

fn project_registry_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS projects (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        root_path TEXT NOT NULL,
        git_remote TEXT,
        default_branch TEXT,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS projects_root_path_idx
    ON projects(root_path);
    "
}

fn file_discovery_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS discovered_files (
        project_id TEXT NOT NULL,
        path TEXT NOT NULL,
        language TEXT,
        size_bytes INTEGER,
        discovered_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (project_id, path)
    );

    CREATE INDEX IF NOT EXISTS discovered_files_project_idx
    ON discovered_files(project_id, updated_at_ms DESC);
    "
}

fn now_ms() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| error.to_string())?;
    i64::try_from(millis).map_err(|error| error.to_string())
}
