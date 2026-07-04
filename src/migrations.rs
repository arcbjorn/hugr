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
const SESSIONS_VERSION: i64 = 4;
const SESSIONS_NAME: &str = "sessions";
const CODE_SYMBOLS_VERSION: i64 = 5;
const CODE_SYMBOLS_NAME: &str = "code_symbols";
const CODE_REFERENCES_VERSION: i64 = 6;
const CODE_REFERENCES_NAME: &str = "code_references";
const SYNC_HISTORY_VERSION: i64 = 7;
const SYNC_HISTORY_NAME: &str = "sync_history";
const SESSION_PROMOTIONS_VERSION: i64 = 8;
const SESSION_PROMOTIONS_NAME: &str = "session_promotions";
const CONTEXT_PACKS_VERSION: i64 = 9;
const CONTEXT_PACKS_NAME: &str = "context_packs";
const DIAGNOSTICS_VERSION: i64 = 10;
const DIAGNOSTICS_NAME: &str = "diagnostics";

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

    if !applied.contains(&SESSIONS_VERSION) {
        conn.execute_batch(sessions_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![SESSIONS_VERSION, SESSIONS_NAME, now_ms()?],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    if !applied.contains(&CODE_SYMBOLS_VERSION) {
        conn.execute_batch(code_symbols_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![CODE_SYMBOLS_VERSION, CODE_SYMBOLS_NAME, now_ms()?],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    if !applied.contains(&CODE_REFERENCES_VERSION) {
        conn.execute_batch(code_references_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![CODE_REFERENCES_VERSION, CODE_REFERENCES_NAME, now_ms()?],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    if !applied.contains(&SYNC_HISTORY_VERSION) {
        conn.execute_batch(sync_history_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![SYNC_HISTORY_VERSION, SYNC_HISTORY_NAME, now_ms()?],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    if !applied.contains(&SESSION_PROMOTIONS_VERSION) {
        conn.execute_batch(session_promotions_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![
                SESSION_PROMOTIONS_VERSION,
                SESSION_PROMOTIONS_NAME,
                now_ms()?
            ],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    if !applied.contains(&CONTEXT_PACKS_VERSION) {
        conn.execute_batch(context_packs_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![CONTEXT_PACKS_VERSION, CONTEXT_PACKS_NAME, now_ms()?],
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    if !applied.contains(&DIAGNOSTICS_VERSION) {
        conn.execute_batch(diagnostics_sql())
            .await
            .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![DIAGNOSTICS_VERSION, DIAGNOSTICS_NAME, now_ms()?],
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

fn sessions_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        task TEXT NOT NULL,
        branch TEXT,
        started_at_ms INTEGER NOT NULL,
        ended_at_ms INTEGER,
        final_summary TEXT
    );

    CREATE INDEX IF NOT EXISTS sessions_active_idx
    ON sessions(project_id, ended_at_ms, started_at_ms DESC);

    CREATE TABLE IF NOT EXISTS session_events (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        detail TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS session_events_session_idx
    ON session_events(session_id, created_at_ms DESC);
    "
}

fn session_promotions_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS session_promotions (
        session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
        memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        promoted_at_ms INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS session_promotions_memory_idx
    ON session_promotions(memory_id);
    "
}

fn context_packs_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS context_packs (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        task TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS context_packs_project_updated_idx
    ON context_packs(project_id, updated_at_ms DESC);
    "
}

fn diagnostics_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS diagnostics (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        source TEXT NOT NULL,
        path TEXT,
        line_start INTEGER,
        line_end INTEGER,
        severity TEXT NOT NULL,
        code TEXT,
        message TEXT NOT NULL,
        command TEXT,
        created_at_ms INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS diagnostics_project_created_idx
    ON diagnostics(project_id, created_at_ms DESC);

    CREATE INDEX IF NOT EXISTS diagnostics_project_path_idx
    ON diagnostics(project_id, path, line_start);
    "
}

fn code_symbols_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS code_symbols (
        project_id TEXT NOT NULL,
        path TEXT NOT NULL,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        language TEXT,
        line_start INTEGER NOT NULL,
        line_end INTEGER,
        signature TEXT NOT NULL,
        indexed_at_ms INTEGER NOT NULL,
        PRIMARY KEY (project_id, path, kind, name, line_start)
    );

    CREATE INDEX IF NOT EXISTS code_symbols_project_name_idx
    ON code_symbols(project_id, name);

    CREATE INDEX IF NOT EXISTS code_symbols_project_path_idx
    ON code_symbols(project_id, path, line_start);
    "
}

fn code_references_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS code_references (
        project_id TEXT NOT NULL,
        path TEXT NOT NULL,
        target_path TEXT NOT NULL,
        target_name TEXT NOT NULL,
        target_kind TEXT NOT NULL,
        kind TEXT NOT NULL,
        language TEXT,
        line_start INTEGER NOT NULL,
        excerpt TEXT NOT NULL,
        indexed_at_ms INTEGER NOT NULL,
        PRIMARY KEY (
            project_id,
            path,
            target_path,
            target_name,
            line_start,
            kind
        )
    );

    CREATE INDEX IF NOT EXISTS code_references_target_name_idx
    ON code_references(project_id, target_name, target_path);

    CREATE INDEX IF NOT EXISTS code_references_target_path_idx
    ON code_references(project_id, target_path);

    CREATE INDEX IF NOT EXISTS code_references_path_idx
    ON code_references(project_id, path, line_start);
    "
}

fn sync_history_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS sync_runs (
        id TEXT PRIMARY KEY,
        operation TEXT NOT NULL,
        backend TEXT NOT NULL,
        status TEXT NOT NULL,
        started_at_ms INTEGER NOT NULL,
        ended_at_ms INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS sync_runs_started_idx
    ON sync_runs(started_at_ms DESC);

    CREATE TABLE IF NOT EXISTS sync_table_runs (
        sync_run_id TEXT NOT NULL REFERENCES sync_runs(id) ON DELETE CASCADE,
        class TEXT NOT NULL,
        table_name TEXT NOT NULL,
        row_count INTEGER NOT NULL,
        inserted_count INTEGER NOT NULL,
        updated_count INTEGER NOT NULL,
        skipped_count INTEGER NOT NULL,
        conflict_count INTEGER NOT NULL,
        executed INTEGER NOT NULL,
        PRIMARY KEY (sync_run_id, table_name)
    );

    CREATE TABLE IF NOT EXISTS sync_table_conflicts (
        sync_run_id TEXT NOT NULL REFERENCES sync_runs(id) ON DELETE CASCADE,
        table_name TEXT NOT NULL,
        reason TEXT NOT NULL,
        count INTEGER NOT NULL,
        PRIMARY KEY (sync_run_id, table_name, reason)
    );
    "
}

fn now_ms() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| error.to_string())?;
    i64::try_from(millis).map_err(|error| error.to_string())
}
