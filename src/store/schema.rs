//! Versioned SQLite query projections. JSON payloads remain untouched evidence;
//! migrations derive searchable columns without changing capture/archive formats.
use crate::model::Metric;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, ErrorCode, TransactionBehavior, params};
use serde::Deserialize;
use std::{thread, time::Duration};

mod progress;
use progress::MigrationProgress;

// The database evolves independently of model::SCHEMA_VERSION (portable captures).
const DATABASE_VERSION: u32 = 3;
const STARTUP_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// Initialize new stores or atomically upgrade existing projections under one writer lock.
pub(super) fn initialize(mut connection: Connection) -> Result<()> {
    // A one-time upgrade can hold SQLite's writer lock for minutes. This owned
    // connection is only used for initialization and closes below; regular store
    // connections retain their bounded busy timeout.
    connection.busy_handler(Some(wait_for_startup_lock))?;
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        version <= DATABASE_VERSION,
        "this database requires a newer Buildscope"
    );
    // Announce legacy upgrades before journal setup or the writer lock can block.
    // Fresh and current stores stay quiet, including commands emitting JSON.
    let legacy = version == 1 || (version == 0 && table_count(&connection)? > 0);
    let mut progress = legacy.then(MigrationProgress::new);
    configure_journal(&connection)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Another opener may have migrated while this connection waited for the lock.
    let version: u32 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        version <= DATABASE_VERSION,
        "this database requires a newer Buildscope"
    );
    match version {
        0 => {
            let tables = table_count(&transaction)?;
            if tables > 0 {
                validate_legacy(&transaction)?;
                let progress = progress.get_or_insert_with(MigrationProgress::new);
                migrate_query_fields(&transaction, progress)?;
            } else {
                create_report_tables(&transaction, "report_units", "profiles")?;
            }
        }
        1 => {
            let progress = progress.get_or_insert_with(MigrationProgress::new);
            migrate_query_fields(&transaction, progress)?;
        }
        _ => {}
    }
    // Also restore optional indexes removed from an otherwise current database.
    if let Some(progress) = &progress {
        progress.stage("Creating query indexes");
    }
    transaction.execute_batch(PHASE_SCHEMA)?;
    transaction.execute_batch(COMMON_SCHEMA)?;
    transaction.execute_batch(COMPILER_STACK_SCHEMA)?;
    transaction.pragma_update(None, "user_version", DATABASE_VERSION)?;
    if let Some(progress) = &progress {
        progress.stage("Committing database upgrade (disk writes may take a while)");
    }
    transaction.commit()?;
    if let Some(progress) = &progress {
        progress.stage("Flushing database journal (disk writes may take a while)");
    }
    connection.close().map_err(|(_, error)| error)?;
    if let Some(progress) = progress {
        progress.finish();
    }
    Ok(())
}

fn wait_for_startup_lock(_attempt: i32) -> bool {
    thread::sleep(STARTUP_RETRY_INTERVAL);
    true
}

fn configure_journal(connection: &Connection) -> Result<()> {
    // SQLite can return BUSY from a journal-mode change without invoking its busy
    // handler. Retry that case as well, while surfacing every other error.
    loop {
        match connection.execute_batch("PRAGMA journal_mode=WAL;") {
            Ok(()) => return Ok(()),
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {
                thread::sleep(STARTUP_RETRY_INTERVAL);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Derive the existing compiler-work target interpretation once at indexing time.
pub(super) fn target_for(output: &str) -> &str {
    output
        .split_once("CMakeFiles/")
        .and_then(|(_, suffix)| suffix.split_once(".dir/"))
        .map_or("(other)", |(target, _)| target)
}

/// Index native measurements; callers clear old phases with their profile first.
/// Ordinals retain duplicate names without merging distinct observations.
pub(super) fn index_phases(
    connection: &Connection,
    run: &str,
    unit: &str,
    phases: &[Metric],
) -> Result<()> {
    let mut insert = connection.prepare(
        "INSERT INTO profile_phases(run,unit,ordinal,name,value,measurement_unit,origin,coverage)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
    )?;
    for (ordinal, phase) in phases.iter().enumerate() {
        ensure!(
            phase.value.is_finite(),
            "non-finite phase value for {run}/{unit}"
        );
        insert.execute(params![
            run,
            unit,
            i64::try_from(ordinal)?,
            phase.name,
            phase.value,
            phase.unit,
            phase.origin,
            phase.coverage
        ])?;
    }
    Ok(())
}

fn create_report_tables(connection: &Connection, units: &str, profiles: &str) -> Result<()> {
    // Names are private migration constants, never user input. New tables permit
    // required metadata without inventing defaults for preexisting rows.
    connection.execute_batch(&format!(
        "CREATE TABLE {units}(
            run TEXT NOT NULL,unit TEXT NOT NULL,data TEXT NOT NULL,
            source_key TEXT NOT NULL,output_key TEXT NOT NULL,
            compiler INTEGER NOT NULL CHECK(typeof(compiler)='integer' AND compiler>=0),
            target TEXT NOT NULL,PRIMARY KEY(run,unit));
         CREATE TABLE {profiles}(
            run TEXT NOT NULL,unit TEXT NOT NULL,data TEXT NOT NULL,family TEXT NOT NULL,
            event_count INTEGER NOT NULL CHECK(typeof(event_count)='integer' AND event_count>=0),
            granularity_us INTEGER CHECK(granularity_us IS NULL OR
                (typeof(granularity_us)='integer' AND granularity_us>=0)),
            PRIMARY KEY(run,unit));"
    ))?;
    Ok(())
}

// Deserialize only the projection, skipping potentially large symbol arrays and
// unknown detail fields. The original JSON string is copied verbatim into v2.
#[derive(Deserialize)]
struct UnitFields {
    source_key: String,
    output_key: String,
    compiler: usize,
}

#[derive(Deserialize)]
struct ProfileFields {
    family: String,
    event_count: usize,
    granularity_us: Option<u64>,
    phases: Vec<Metric>,
}

#[derive(Deserialize)]
struct EventLocation {
    file: Option<String>,
    line: Option<u64>,
}

fn validate_legacy(connection: &Connection) -> Result<()> {
    // Version zero is accepted only for the complete known legacy layout, never
    // interpreted as permission to initialize an unrelated nonempty database.
    for (table, columns) in [
        (
            "runs",
            "id,started,status,kind,configuration,targets,local,manifest",
        ),
        ("launcher_receipts", "build,invocation,run"),
        ("events", "run,unit,idx,name,detail,start,duration,data"),
        ("headers", "run,path,key,trigger,contributor,rebuilt,work"),
        ("report_runs", "id,data"),
        ("profiles", "run,unit,data"),
        ("report_units", "run,unit,data"),
        ("report_headers", "run,path,data"),
        ("include_paths", "run,path,chain,unit"),
    ] {
        connection
            .prepare(&format!("SELECT {columns} FROM {table} LIMIT 0"))
            .context("unrecognized unversioned Buildscope database")?;
    }
    Ok(())
}

fn table_count(connection: &Connection) -> Result<i64> {
    Ok(connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?)
}

fn migrate_query_fields(connection: &Connection, progress: &MigrationProgress) -> Result<()> {
    progress.stage("Preparing typed metadata tables");
    create_report_tables(connection, "report_units_v2", "profiles_v2")?;
    connection.execute_batch(PHASE_SCHEMA)?;
    progress.stage("Copying compilation unit metadata");
    {
        let mut processed = 0;
        let mut select = connection.prepare("SELECT run,unit,data FROM report_units")?;
        let mut rows = select.query([])?;
        let mut insert = connection.prepare(
            "INSERT INTO report_units_v2(run,unit,data,source_key,output_key,compiler,target)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
        )?;
        while let Some(row) = rows.next()? {
            let (run, unit, data): (String, String, String) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let fields: UnitFields = serde_json::from_str(&data)
                .with_context(|| format!("invalid unit metadata for {run}/{unit}"))?;
            let compiler = i64::try_from(fields.compiler).with_context(|| {
                format!("compiler index exceeds SQLite integer range for {run}/{unit}")
            })?;
            insert.execute(params![
                run,
                unit,
                data,
                fields.source_key,
                fields.output_key,
                compiler,
                target_for(&fields.output_key)
            ])?;
            processed += 1;
            progress.rows(processed);
        }
    }
    progress.stage("Copying compiler profiles and phases");
    {
        let mut processed = 0;
        let mut select = connection.prepare("SELECT run,unit,data FROM profiles")?;
        let mut rows = select.query([])?;
        let mut insert = connection.prepare(
            "INSERT INTO profiles_v2(run,unit,data,family,event_count,granularity_us)
             VALUES(?1,?2,?3,?4,?5,?6)",
        )?;
        while let Some(row) = rows.next()? {
            let (run, unit, data): (String, String, String) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let fields: ProfileFields = serde_json::from_str(&data)
                .with_context(|| format!("invalid profile metadata for {run}/{unit}"))?;
            let event_count = i64::try_from(fields.event_count).with_context(|| {
                format!("event count exceeds SQLite integer range for {run}/{unit}")
            })?;
            let granularity = fields
                .granularity_us
                .map(i64::try_from)
                .transpose()
                .with_context(|| {
                    format!("granularity exceeds SQLite integer range for {run}/{unit}")
                })?;
            insert.execute(params![
                run,
                unit,
                data,
                fields.family,
                event_count,
                granularity
            ])?;
            index_phases(connection, &run, &unit, &fields.phases)?;
            processed += 1;
            progress.rows(processed);
        }
    }
    progress.stage("Preparing event columns and validating existing rows");
    connection.execute_batch(
        "ALTER TABLE events ADD COLUMN file TEXT;
         ALTER TABLE events ADD COLUMN line INTEGER CHECK(line IS NULL OR (typeof(line)='integer' AND line>=0));"
    )?;
    // Keyset batches release each read cursor before updating the same table,
    // keeping memory bounded and avoiding cursor invalidation during backfill.
    progress.stage("Backfilling event locations");
    let mut processed = 0;
    let mut cursor: Option<i64> = None;
    loop {
        let sql = if cursor.is_none() {
            "SELECT rowid,run,unit,idx,data FROM events WHERE rowid>=?1 ORDER BY rowid LIMIT 256"
        } else {
            "SELECT rowid,run,unit,idx,data FROM events WHERE rowid>?1 ORDER BY rowid LIMIT 256"
        };
        let mut select = connection.prepare(sql)?;
        let batch: Vec<(i64, String, String, i64, String)> = select
            .query_map([cursor.unwrap_or(i64::MIN)], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(select);
        if batch.is_empty() {
            break;
        }
        let mut update = connection.prepare("UPDATE events SET file=?1,line=?2 WHERE rowid=?3")?;
        for (rowid, run, unit, idx, data) in batch {
            let fields: EventLocation = serde_json::from_str(&data)
                .with_context(|| format!("invalid event metadata for {run}/{unit}/{idx}"))?;
            let line = fields
                .line
                .map(i64::try_from)
                .transpose()
                .with_context(|| {
                    format!("event line exceeds SQLite integer range for {run}/{unit}/{idx}")
                })?;
            update.execute(params![fields.file, line, rowid])?;
            cursor = Some(rowid);
            processed += 1;
        }
        progress.rows(processed);
    }
    progress.stage("Installing typed metadata tables");
    connection.execute_batch(
        "DROP TABLE report_units;
         ALTER TABLE report_units_v2 RENAME TO report_units;
         DROP TABLE profiles;
         ALTER TABLE profiles_v2 RENAME TO profiles;",
    )?;
    Ok(())
}

const PHASE_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS profile_phases(
        run TEXT NOT NULL,unit TEXT NOT NULL,
        ordinal INTEGER NOT NULL CHECK(typeof(ordinal)='integer' AND ordinal>=0),
        name TEXT NOT NULL,value REAL NOT NULL,measurement_unit TEXT NOT NULL,
        origin TEXT NOT NULL,coverage TEXT NOT NULL,PRIMARY KEY(run,unit,ordinal));";

const COMMON_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS runs(
        id TEXT PRIMARY KEY,started INTEGER NOT NULL,status TEXT NOT NULL,
        kind TEXT NOT NULL,configuration TEXT NOT NULL,targets TEXT NOT NULL,
        local INTEGER NOT NULL,manifest TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS launcher_receipts(build TEXT NOT NULL,invocation TEXT NOT NULL,run TEXT NOT NULL,
        PRIMARY KEY(build,invocation));
    CREATE INDEX IF NOT EXISTS run_history ON runs(configuration,kind,started);
    CREATE TABLE IF NOT EXISTS events(
        run TEXT NOT NULL,unit TEXT NOT NULL,idx INTEGER NOT NULL,
        name TEXT NOT NULL,detail TEXT NOT NULL,start REAL NOT NULL,
        duration REAL NOT NULL,data TEXT NOT NULL,file TEXT,
        line INTEGER CHECK(line IS NULL OR (typeof(line)='integer' AND line>=0)),
        PRIMARY KEY(run,unit,idx));
    CREATE INDEX IF NOT EXISTS event_query ON events(run,unit,duration DESC);
    CREATE INDEX IF NOT EXISTS compiler_work_query ON events(run,name,detail,unit)
        WHERE name IN ('InstantiateClass','InstantiateFunction','OptFunction','CodeGen Function','isIntegerConstantExpr')
            OR name GLOB 'Evaluate*';
    CREATE INDEX IF NOT EXISTS event_parent ON events(run,unit,json_extract(data,'$.parent'));
    CREATE TABLE IF NOT EXISTS headers(
        run TEXT NOT NULL,path TEXT NOT NULL,key TEXT NOT NULL,
        trigger INTEGER NOT NULL,contributor INTEGER NOT NULL,
        rebuilt INTEGER NOT NULL,work REAL NOT NULL,PRIMARY KEY(run,path));
    CREATE INDEX IF NOT EXISTS header_history ON headers(key,run);
    CREATE TABLE IF NOT EXISTS report_runs(id TEXT PRIMARY KEY,data TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS report_headers(run TEXT NOT NULL,path TEXT NOT NULL,data TEXT NOT NULL,
        PRIMARY KEY(run,path));
    CREATE TABLE IF NOT EXISTS include_paths(run TEXT NOT NULL,path TEXT NOT NULL,
        chain TEXT NOT NULL,unit TEXT NOT NULL,PRIMARY KEY(run,path,chain,unit));
    CREATE INDEX IF NOT EXISTS unit_target ON report_units(run,target,unit);";

// Compact terminal stack projection. A marker exists even for empty traces;
// absent markers on older captures are explicit and never trigger startup scans.
const COMPILER_STACK_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS compiler_stack_units(
 run TEXT NOT NULL, unit TEXT NOT NULL, PRIMARY KEY(run,unit));
CREATE TABLE IF NOT EXISTS compiler_stacks(
 run TEXT NOT NULL,unit TEXT NOT NULL,ordinal INTEGER NOT NULL,data TEXT NOT NULL,
 PRIMARY KEY(run,unit,ordinal));";
