//! SQLite indexes and portable, compressed capture archives. Raw evidence lives in
//! per-run directories; imported archives never grant access to local source files.
use crate::{model::*, util::*};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::{Component, Path, PathBuf},
};

mod compiler_work;
mod schema;
pub use compiler_work::CompilerWorkQuery;

/// A local capture repository. Connections are short-lived so reports can read while
/// another process finalizes a capture.
#[derive(Clone)]
pub struct Store {
    /// Absolute directory containing SQLite and immutable per-run artifacts.
    pub root: PathBuf,
}

/// A compressed event file prepared by a decoder, awaiting ordered publication.
/// Each invocation owns its temporary file, even when several invocations share a unit.
pub(crate) struct PreparedEvents {
    /// Capture whose SQLite event rows will be replaced when this artifact is consumed.
    run: String,
    /// Unit whose events are published; repeated invocations replace it in input order.
    unit: String,
    /// Unique unpublished file, removed on drop after errors or abandoned worker results.
    temporary: PathBuf,
    /// Stable per-unit archive path; only the serial writer replaces this file.
    destination: PathBuf,
}

impl Drop for PreparedEvents {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temporary);
    }
}

/// Search and pagination for one unit's compiler scopes.
pub struct EventQuery<'a> {
    /// Substring matched against native activity and detail text.
    pub text: &'a str,
    /// Exact activity name; empty means all activities.
    pub activity: &'a str,
    /// Number of matching rows to skip.
    pub offset: usize,
    /// Maximum result count, capped at 5000 by the repository.
    pub limit: usize,
    /// duration orders longest first; other values order chronologically.
    pub order: &'a str,
}

impl Default for EventQuery<'_> {
    fn default() -> Self {
        Self {
            text: "",
            activity: "",
            offset: 0,
            limit: 250,
            order: "duration",
        }
    }
}

impl Store {
    /// Open or initialize a repository, rejecting unknown database versions.
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        let store = Self {
            root: root.canonicalize()?,
        };
        fs::create_dir_all(store.root.join("runs"))?;
        schema::initialize(store.connection()?)?;
        Ok(store)
    }

    /// Resolve an exact ID, unique prefix, or latest run.
    pub fn resolve_id(&self, id: &str) -> Result<String> {
        let connection = self.connection()?;
        if id == "latest" {
            return connection
                .query_row(
                    "SELECT id FROM runs ORDER BY started DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .context("no captures in this store");
        }
        validate_id(id)?;
        let mut statement =
            connection.prepare("SELECT id FROM runs WHERE id LIKE ?1 ORDER BY started DESC")?;
        let ids: Vec<String> = statement
            .query_map([format!("{id}%")], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        if ids.iter().any(|value| value == id) {
            return Ok(id.into());
        }
        match ids.as_slice() {
            [only] => Ok(only.clone()),
            [] => bail!("capture {id} was not found"),
            _ => bail!("capture prefix {id} is ambiguous"),
        }
    }

    /// Resolve a safe run directory without requiring a finalized database entry.
    pub fn run_dir(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join("runs").join(id))
    }

    /// Persist a local run, including its full recovery manifest before execution begins.
    /// Running captures defer report indexing until finalization or an on-demand report read.
    pub fn save(&self, capture: &Capture) -> Result<()> {
        self.persist(capture, true, None)
    }

    /// Atomically finalize a launcher batch and acknowledge its invocation IDs. Spool
    /// cleanup follows this commit, so interrupted cleanup cannot duplicate a capture.
    pub fn save_launcher(&self, capture: &Capture, build: &str, ids: &[String]) -> Result<()> {
        self.persist(capture, true, Some((build, ids)))
    }

    /// Invocation IDs already committed for this build, used to resume spool cleanup.
    pub fn launcher_receipts(&self, build: &str) -> Result<BTreeSet<String>> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT invocation FROM launcher_receipts WHERE build=?1")?;
        Ok(statement
            .query_map([build], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// Load one versioned capture by exact ID, prefix, or latest.
    pub fn load(&self, id: &str) -> Result<Capture> {
        let id = self.resolve_id(id)?;
        let connection = self.connection()?;
        let manifest: String =
            connection.query_row("SELECT manifest FROM runs WHERE id=?1", [&id], |r| r.get(0))?;
        let capture: Capture = serde_json::from_str(&manifest)?;
        ensure!(
            capture.schema_version == SCHEMA_VERSION,
            "unsupported capture schema"
        );
        Ok(capture)
    }

    /// Load only report summaries, omitting dependency matrices and named compiler detail.
    pub fn overview(&self, id: &str) -> Result<Value> {
        let id = self.resolve_id(id)?;
        let mut connection = self.connection()?;
        let value = connection.query_row("SELECT data FROM report_runs WHERE id=?1", [&id], |r| {
            r.get::<_, String>(0)
        });
        let mut overview: Value = match value {
            Ok(value) => serde_json::from_str(&value)?,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Serialize lazy indexing with finalization and reload under the writer
                // lock so an old running manifest cannot overwrite a completed report.
                let transaction = connection
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let capture = self.load(&id)?;
                self.index_report(&transaction, &capture)?;
                let overview = report_overview(&capture)?;
                transaction.commit()?;
                overview
            }
            Err(error) => return Err(error.into()),
        };
        let configuration = overview["project"]["configuration_id"]
            .as_str()
            .context("missing report configuration")?;
        let targets = overview["targets"].to_string();
        let mut statement = connection.prepare(
            "SELECT h.key,r.kind,count(*),sum(h.trigger),sum(h.contributor)
            FROM headers h JOIN runs r ON r.id=h.run WHERE r.configuration=?1 AND r.targets=?2
            AND r.status!='running' GROUP BY h.key,r.kind",
        )?;
        let mut history = BTreeMap::<String, Value>::new();
        for row in statement.query_map(params![configuration, targets], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })? {
            let (key, kind, captures, triggers, contributors) = row?;
            history.entry(key).or_insert_with(|| json!({}))[kind] = json!({
                "captures":captures,"trigger_runs":triggers,"contributor_runs":contributors});
        }
        if let Some(headers) = overview["headers"].as_array_mut() {
            for header in headers {
                if let Some(value) = header["key"]
                    .as_str()
                    .and_then(|key| history.get(key))
                    .cloned()
                {
                    header["history"] = value;
                }
            }
        }
        Ok(overview)
    }

    /// Load one unit's command and compiler detail through an indexed lookup.
    pub fn unit_detail(&self, run: &str, unit: &str) -> Result<Value> {
        let connection = self.connection()?;
        let unit_data: String = connection.query_row(
            "SELECT data FROM report_units WHERE run=?1 AND unit=?2",
            params![run, unit],
            |r| r.get(0),
        )?;
        let profile = connection.query_row(
            "SELECT data FROM profiles WHERE run=?1 AND unit=?2",
            params![run, unit],
            |r| r.get::<_, String>(0),
        );
        let profile = match profile {
            Ok(value) => serde_json::from_str::<Value>(&value)?,
            Err(rusqlite::Error::QueryReturnedNoRows) => Value::Null,
            Err(error) => return Err(error.into()),
        };
        Ok(json!({"unit":serde_json::from_str::<Value>(&unit_data)?,"profile":profile}))
    }

    /// Load one header's distribution and evidence, separate from the inventory list.
    pub fn header_detail(&self, run: &str, path: &str) -> Result<Value> {
        let data: String = self.connection()?.query_row(
            "SELECT data FROM report_headers WHERE run=?1 AND path=?2",
            params![run, path],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }

    /// Inspect a recorded time-containment scope and its immediate recorded children.
    pub fn scope(&self, run: &str, unit: &str, index: usize) -> Result<Value> {
        let connection = self.connection()?;
        let root: String = connection.query_row(
            "SELECT data FROM events WHERE run=?1 AND unit=?2 AND idx=?3",
            params![run, unit, index as i64],
            |r| r.get(0),
        )?;
        let mut statement = connection.prepare(
            "SELECT data FROM events WHERE run=?1 AND unit=?2
            AND json_extract(data,'$.parent')=?3 ORDER BY duration DESC",
        )?;
        let children: Vec<Value> = statement
            .query_map(params![run, unit, index as i64], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_>>()?;
        Ok(json!({"root":serde_json::from_str::<Value>(&root)?,"children":children}))
    }

    /// Group observed include chains without loading every compiler profile in the browser.
    pub fn include_paths(&self, run: &str, path: &str) -> Result<Value> {
        let connection = self.connection()?;
        let total: i64 = connection.query_row(
            "SELECT count(DISTINCT chain) FROM include_paths WHERE run=?1 AND path=?2",
            params![run, path],
            |r| r.get(0),
        )?;
        let mut statement = connection.prepare(
            "SELECT chain,count(DISTINCT unit) FROM include_paths
            WHERE run=?1 AND path=?2 GROUP BY chain ORDER BY count(DISTINCT unit) DESC LIMIT 200",
        )?;
        let paths: Vec<Value> = statement
            .query_map(params![run, path], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .map(|row| {
                let (chain, count) = row?;
                Ok(json!({"chain":serde_json::from_str::<Value>(&chain)?,"units":count}))
            })
            .collect::<Result<_>>()?;
        Ok(json!({"paths":paths,"total":total}))
    }

    /// Find the prior run with the same configuration, mode, and requested targets.
    pub fn previous(
        &self,
        configuration: &str,
        kind: &str,
        targets: &[String],
    ) -> Result<Option<Capture>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT manifest FROM runs WHERE configuration=?1 AND kind=?2 AND targets=?3 AND status!='running' ORDER BY started DESC LIMIT 1")?;
        let mut rows = statement.query(params![
            configuration,
            kind,
            serde_json::to_string(targets)?
        ])?;
        rows.next()?
            .map(|row| -> Result<Capture> { Ok(serde_json::from_str(&row.get::<_, String>(0)?)?) })
            .transpose()
    }

    /// Small metadata rows for history and run selection.
    pub fn list(&self) -> Result<Vec<Value>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT COALESCE(o.data,r.manifest) FROM runs r LEFT JOIN report_runs o ON o.id=r.id ORDER BY r.started DESC")?;
        let mut result = Vec::new();
        for manifest in statement.query_map([], |row| row.get::<_, String>(0))? {
            let run: Capture = serde_json::from_str(&manifest?)?;
            result.push(json!({"id":run.id,"started_ms":run.started_ms,"status":run.status,"kind":run.kind,
                "label":run.label,"elapsed_ms":run.elapsed_ms,"actions":run.actions.len(),"units":run.project.units.len(),
                "headers":run.headers.len(),"configuration_id":run.project.configuration_id,
                "build_dir":run.project.build_dir,"summary":run.summary,"git":run.git,
                "project_id":run.project.project_id,"machine":run.machine,"targets":run.targets,
                "environment":run.environment,"cache_policy":run.cache_policy,"jobs":run.jobs,"workload":run.workload,
                "instrumentation":run.profiles.iter().map(|p| (p.granularity_us,p.verbose)).collect::<std::collections::BTreeSet<_>>()}));
        }
        Ok(result)
    }

    /// Successful captures with an exact experiment label, using lightweight report data.
    pub fn with_label(&self, label: &str) -> Result<Vec<Capture>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT COALESCE(o.data,r.manifest) FROM runs r
            LEFT JOIN report_runs o ON o.id=r.id WHERE json_extract(r.manifest,'$.label')=?1
            AND r.status='success' ORDER BY r.started",
        )?;
        statement
            .query_map([label], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    /// Index normalized events and keep a compressed copy for independent archives.
    pub fn save_events(&self, run: &str, unit: &str, events: &[TraceEvent]) -> Result<()> {
        self.save_prepared_events(self.prepare_events(run, unit, events)?, events)
    }

    /// Compress events independently of SQLite and other invocations of the same unit.
    /// Dropping an unpublished result removes its temporary artifact.
    pub(crate) fn prepare_events(
        &self,
        run: &str,
        unit: &str,
        events: &[TraceEvent],
    ) -> Result<PreparedEvents> {
        validate_id(unit)?;
        let directory = self.run_dir(run)?.join("events");
        fs::create_dir_all(&directory)?;
        let prepared = PreparedEvents {
            run: run.into(),
            unit: unit.into(),
            temporary: directory.join(format!(".{unit}.{}.tmp", unique_id())),
            destination: directory.join(format!("{unit}.json.zst")),
        };
        let mut encoder = zstd::stream::write::Encoder::new(File::create(&prepared.temporary)?, 3)?;
        serde_json::to_writer(&mut encoder, events)?;
        encoder.finish()?;
        Ok(prepared)
    }

    /// Publish a prepared event archive and replace its rows on the calling writer thread.
    /// Archive publication precedes the transaction, as in `save_events`; an interrupted
    /// index is rebuilt from retained invocation evidence during recovery.
    pub(crate) fn save_prepared_events(
        &self,
        prepared: PreparedEvents,
        events: &[TraceEvent],
    ) -> Result<()> {
        // Check the entire batch before publishing its JSON archive. Otherwise a
        // later invalid location could reject the index after replacing its backup.
        ensure!(
            events
                .iter()
                .all(|event| event.line.is_none_or(|line| line <= i64::MAX as u64)),
            "event line exceeds SQLite integer range"
        );
        fs::rename(&prepared.temporary, &prepared.destination)?;
        let run = &prepared.run;
        let unit = &prepared.unit;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM events WHERE run=?1 AND unit=?2",
            params![run, unit],
        )?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO events(run,unit,idx,name,detail,start,duration,data,file,line)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            )?;
            for event in events {
                insert.execute(params![
                    run,
                    unit,
                    event.id as i64,
                    event.name,
                    event.detail,
                    event.start_us,
                    event.duration_us,
                    serde_json::to_string(event)?,
                    event.file,
                    event
                        .line
                        .map(i64::try_from)
                        .transpose()
                        .context("event line exceeds SQLite integer range")?
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Page filtered events through the SQLite index instead of loading an entire trace.
    pub fn events(&self, run: &str, unit: &str, query: EventQuery<'_>) -> Result<Value> {
        let EventQuery {
            text: query,
            activity: kind,
            offset,
            limit,
            order: sort,
        } = query;
        let connection = self.connection()?;
        let pattern = format!("%{query}%");
        let filter =
            "run=?1 AND unit=?2 AND (detail LIKE ?3 OR name LIKE ?3) AND (?4='' OR name=?4)";
        let count: i64 = connection.query_row(
            &format!("SELECT count(*) FROM events WHERE {filter}"),
            params![run, unit, pattern, kind],
            |row| row.get(0),
        )?;
        let order = if sort == "duration" {
            "duration DESC,start"
        } else {
            "start,duration DESC"
        };
        let mut statement = connection.prepare(&format!(
            "SELECT data FROM events WHERE {filter} ORDER BY {order} LIMIT ?5 OFFSET ?6"
        ))?;
        let values: Vec<Value> = statement
            .query_map(
                params![
                    run,
                    unit,
                    pattern,
                    kind,
                    limit.min(5000) as i64,
                    offset as i64
                ],
                |row| row.get::<_, String>(0),
            )?
            .map(|s| Ok(serde_json::from_str(&s?)?))
            .collect::<Result<_>>()?;
        let mut names = connection
            .prepare("SELECT DISTINCT name FROM events WHERE run=?1 AND unit=?2 ORDER BY name")?;
        let names: Vec<String> = names
            .query_map(params![run, unit], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(json!({"total":count,"events":values,"names":names,"offset":offset}))
    }

    /// Historical supported triggers and changed contributors within a compatible configuration.
    pub fn header_history(&self, run: &Capture, key: &str) -> Result<Vec<Value>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT r.id,r.started,r.kind,h.trigger,h.contributor,h.rebuilt,h.work
             FROM headers h JOIN runs r ON r.id=h.run WHERE h.key=?1 AND r.configuration=?2
             AND r.status!='running' AND r.targets=?3 ORDER BY r.started DESC",
        )?;
        let result = statement.query_map(params![key, run.project.configuration_id,serde_json::to_string(&run.targets)?], |row| {
            Ok(json!({"id":row.get::<_,String>(0)?,"started_ms":row.get::<_,i64>(1)?,"kind":row.get::<_,String>(2)?,
                "explicit_trigger":row.get::<_,bool>(3)?,"changed_contributor":row.get::<_,bool>(4)?,
                "rebuilt_units":row.get::<_,i64>(5)?,"rebuilt_work_ms":row.get::<_,f64>(6)?}))
        })?.collect::<std::result::Result<_, _>>()?;
        Ok(result)
    }

    /// Export one self-contained archive. Source inclusion requires matching captured hashes.
    pub fn export(&self, id: &str, output: &Path, include_source: bool) -> Result<()> {
        let capture = self.load(id)?;
        let run_dir = self.run_dir(&capture.id)?;
        let output = resolve(std::env::current_dir()?, output);
        ensure!(
            !output.starts_with(&run_dir),
            "archive output must be outside the capture directory"
        );
        let temporary = output.with_extension(format!("{}.tmp", unique_id()));
        let encoder = zstd::stream::write::Encoder::new(File::create(&temporary)?, 3)?;
        let mut archive = tar::Builder::new(encoder);
        archive.append_dir_all("capture", &run_dir)?;
        if include_source {
            ensure!(
                self.is_local(&capture.id)?,
                "source export requires a locally recorded capture"
            );
            let mut mapping = BTreeMap::<String, String>::new();
            for (path, stamp) in &capture.inputs {
                if !Path::new(path).starts_with(&capture.project.source_dir) {
                    continue;
                }
                let Some(expected) = &stamp.digest else {
                    continue;
                };
                let bytes =
                    fs::read(path).with_context(|| format!("read captured source {path}"))?;
                ensure!(
                    digest(&bytes) == *expected,
                    "source changed since capture: {path}"
                );
                if !mapping.values().any(|value| value == expected) {
                    append_bytes(&mut archive, &format!("capture/sources/{expected}"), &bytes)?;
                }
                mapping.insert(path.clone(), expected.clone());
            }
            append_bytes(
                &mut archive,
                "capture/sources.json",
                &serde_json::to_vec(&mapping)?,
            )?;
        }
        archive.into_inner()?.finish()?;
        fs::rename(temporary, output)?;
        Ok(())
    }

    /// Import only regular files and directories under a checked archive prefix.
    pub fn import(&self, input: &Path) -> Result<String> {
        let staging = self.root.join(format!(".import-{}", unique_id()));
        fs::create_dir(&staging)?;
        let result = self.import_into(input, &staging);
        let _ = fs::remove_dir_all(&staging);
        result
    }

    /// True only for locally recorded captures; importing metadata cannot grant source access.
    pub fn is_local(&self, id: &str) -> Result<bool> {
        Ok(self
            .connection()?
            .query_row("SELECT local FROM runs WHERE id=?1", [id], |row| row.get(0))?)
    }

    /// Read a bundled source snapshot, otherwise a matching local source or explicit path mapping.
    pub fn source(
        &self,
        run: &Capture,
        path: &str,
        mappings: &[(String, String)],
    ) -> Result<Value> {
        let stamp = run
            .inputs
            .get(path)
            .context("file is not a captured input")?;
        let run_dir = self.run_dir(&run.id)?;
        let source_map: BTreeMap<String, String> = fs::read(run_dir.join("sources.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let bundled = source_map
            .get(path)
            .filter(|hash| hash.chars().all(|c| c.is_ascii_hexdigit()))
            .map(|hash| run_dir.join("sources").join(hash));
        let mut mapped = None;
        for (old, new) in mappings {
            if let Ok(relative) = Path::new(path).strip_prefix(old) {
                ensure!(
                    relative
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                    "source path escapes its mapped root"
                );
                let root = Path::new(new).canonicalize()?;
                let actual = resolve(&root, relative);
                ensure!(
                    actual.starts_with(&root),
                    "source symlink escapes its mapped root"
                );
                mapped = Some(actual);
                break;
            }
        }
        let actual = if let Some(path) = bundled {
            path
        } else if let Some(path) = mapped {
            path
        } else {
            ensure!(
                self.is_local(&run.id)?,
                "imported capture needs bundled sources or --path-map"
            );
            PathBuf::from(path)
        };
        ensure!(
            fs::metadata(&actual)?.len() <= 2 * 1024 * 1024,
            "source preview exceeds 2 MiB"
        );
        let bytes = fs::read(actual)?;
        let matches = stamp
            .digest
            .as_ref()
            .is_some_and(|value| *value == digest(&bytes));
        Ok(json!({"path":path,"matches_capture":matches,"text":String::from_utf8_lossy(&bytes)}))
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(self.root.join("index.sqlite3"))?;
        connection.busy_timeout(std::time::Duration::from_secs(10))?;
        Ok(connection)
    }

    fn persist(
        &self,
        capture: &Capture,
        local: bool,
        receipts: Option<(&str, &[String])>,
    ) -> Result<()> {
        ensure!(
            capture.schema_version == SCHEMA_VERSION,
            "unsupported capture schema"
        );
        let directory = self.run_dir(&capture.id)?;
        write_json(&directory.join("capture.json"), capture)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO runs VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET status=excluded.status,configuration=excluded.configuration,
             targets=excluded.targets,manifest=excluded.manifest",
            params![capture.id, capture.started_ms as i64, capture.status, capture.kind, capture.project.configuration_id,
                serde_json::to_string(&capture.targets)?, local, serde_json::to_string(capture)?])?;
        transaction.execute("DELETE FROM headers WHERE run=?1", [&capture.id])?;
        if capture.status == "running" {
            // A retry can replace a previously queried/recovered capture. Invalidate its
            // derived rows without rebuilding them; recovery reads the durable manifest.
            self.clear_report(&transaction, &capture.id)?;
        } else {
            for header in &capture.headers {
                transaction.execute(
                    "INSERT INTO headers VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        capture.id,
                        header.path,
                        header.key,
                        header.explicit_trigger,
                        header.changed_contributor,
                        header.rebuilt_units as i64,
                        header.rebuilt_work_ms
                    ],
                )?;
            }
            self.index_report(&transaction, capture)?;
        }
        if let Some((build, ids)) = receipts {
            for id in ids {
                transaction.execute(
                    "INSERT OR REPLACE INTO launcher_receipts VALUES(?1,?2,?3)",
                    params![build, id, capture.id],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn clear_report(&self, connection: &Connection, run: &str) -> Result<()> {
        connection.execute("DELETE FROM report_runs WHERE id=?1", [run])?;
        for table in [
            "profile_phases",
            "profiles",
            "report_units",
            "report_headers",
            "include_paths",
        ] {
            connection.execute(&format!("DELETE FROM {table} WHERE run=?1"), [run])?;
        }
        Ok(())
    }

    fn index_report(&self, connection: &Connection, capture: &Capture) -> Result<()> {
        self.clear_report(connection, &capture.id)?;
        connection.execute(
            "INSERT OR REPLACE INTO report_runs VALUES(?1,?2)",
            params![capture.id, report_overview(capture)?.to_string()],
        )?;
        for unit in &capture.project.units {
            let mut detail = serde_json::to_value(unit)?;
            detail["execution_command"] = capture
                .project
                .commands
                .get(&unit.output)
                .map(|command| json!(command))
                .unwrap_or(Value::Null);
            let responses = capture
                .project
                .commands
                .get(&unit.output)
                .and_then(|command| shlex::split(command))
                .and_then(|arguments| {
                    crate::project::expand_responses(
                        &arguments,
                        Path::new(&unit.directory),
                        &capture.project.response_files,
                    )
                    .ok()
                })
                .map(|(_, files)| files)
                .unwrap_or_default();
            detail["response_files"] = serde_json::to_value(responses)?;
            connection.execute(
                "INSERT INTO report_units(run,unit,data,source_key,output_key,compiler,target)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    capture.id,
                    unit.id,
                    detail.to_string(),
                    unit.source_key,
                    unit.output_key,
                    i64::try_from(unit.compiler)
                        .context("compiler index exceeds SQLite integer range")?,
                    schema::target_for(&unit.output_key)
                ],
            )?;
        }
        for profile in &capture.profiles {
            connection.execute(
                "INSERT INTO profiles(run,unit,data,family,event_count,granularity_us)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    capture.id,
                    profile.unit_id,
                    serde_json::to_string(profile)?,
                    profile.family,
                    i64::try_from(profile.event_count)
                        .context("event count exceeds SQLite integer range")?,
                    profile
                        .granularity_us
                        .map(i64::try_from)
                        .transpose()
                        .context("granularity exceeds SQLite integer range")?
                ],
            )?;
            schema::index_phases(connection, &capture.id, &profile.unit_id, &profile.phases)?;
            let mut chain = Vec::<String>::new();
            for edge in &profile.includes {
                if edge.depth == 0 || edge.depth > chain.len() + 1 {
                    continue;
                }
                chain.truncate(edge.depth - 1);
                chain.push(edge.child.clone());
                connection.execute(
                    "INSERT OR IGNORE INTO include_paths VALUES(?1,?2,?3,?4)",
                    params![
                        capture.id,
                        edge.child,
                        serde_json::to_string(&chain)?,
                        profile.unit_id
                    ],
                )?;
            }
        }
        for header in &capture.headers {
            connection.execute(
                "INSERT INTO report_headers VALUES(?1,?2,?3)",
                params![capture.id, header.path, serde_json::to_string(header)?],
            )?;
        }
        Ok(())
    }

    fn import_into(&self, input: &Path, staging: &Path) -> Result<String> {
        let decoder = zstd::stream::read::Decoder::new(File::open(input)?)?;
        let mut archive = tar::Archive::new(decoder);
        let mut total = 0u64;
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            ensure!(
                path.components().all(|p| matches!(p, Component::Normal(_))),
                "unsafe archive path"
            );
            let relative = path
                .strip_prefix("capture")
                .context("unexpected archive prefix")?;
            let kind = entry.header().entry_type();
            ensure!(
                kind.is_dir() || kind.is_file(),
                "archive links and special files are not supported"
            );
            total = total
                .checked_add(entry.size())
                .context("archive length overflow")?;
            ensure!(
                total <= 16 * 1024 * 1024 * 1024 && entry.size() <= 2 * 1024 * 1024 * 1024,
                "archive exceeds import limits"
            );
            let target = staging.join(relative);
            if kind.is_dir() {
                fs::create_dir_all(&target)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?;
            std::io::copy(&mut entry, &mut file)?;
        }
        let capture: Capture =
            serde_json::from_reader(BufReader::new(File::open(staging.join("capture.json"))?))?;
        ensure!(
            capture.schema_version == SCHEMA_VERSION,
            "unsupported archive version"
        );
        validate_id(&capture.id)?;
        // Validate streams before installing a finished imported capture.
        let staged_events = staging.join("events");
        if staged_events.exists() {
            for entry in fs::read_dir(staged_events)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(unit) = name.strip_suffix(".json.zst") else {
                    continue;
                };
                validate_id(unit)?;
                let decoder = zstd::stream::read::Decoder::new(File::open(entry.path())?)?;
                let events: Vec<TraceEvent> =
                    serde_json::from_reader(BufReader::new(decoder.take(1024 * 1024 * 1024)))?;
                for (index, event) in events.iter().enumerate() {
                    ensure!(
                        event.id == index && event.parent.is_none_or(|parent| parent < index),
                        "invalid normalized event index or parent"
                    );
                    ensure!(
                        event.start_us.is_finite()
                            && event.duration_us.is_finite()
                            && event.duration_us >= 0.0,
                        "invalid normalized event timing"
                    );
                }
            }
        }
        let destination = self.run_dir(&capture.id)?;
        if destination.exists() {
            let existing = self.load(&capture.id)?;
            ensure!(
                serde_json::to_value(&existing)? == serde_json::to_value(&capture)?,
                "capture ID collision with different data"
            );
            return Ok(capture.id);
        }
        fs::rename(staging, &destination)?;
        self.persist(&capture, false, None)?;
        let events_dir = destination.join("events");
        if events_dir.exists() {
            for entry in fs::read_dir(events_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(unit) = name.strip_suffix(".json.zst") else {
                    continue;
                };
                validate_id(unit)?;
                let decoder = zstd::stream::read::Decoder::new(File::open(entry.path())?)?;
                let events: Vec<TraceEvent> =
                    serde_json::from_reader(BufReader::new(decoder.take(1024 * 1024 * 1024)))?;
                self.save_events(&capture.id, unit, &events)?;
            }
        }
        Ok(capture.id)
    }
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "invalid capture/unit identifier"
    );
    Ok(())
}

fn report_overview(capture: &Capture) -> Result<Value> {
    let mut report = capture.clone();
    report.inputs.clear();
    report.project.dependencies.clear();
    report.project.graph.clear();
    report.project.cmake_cache.clear();
    report.project.commands.clear();
    report.project.response_files.clear();
    for unit in &mut report.project.units {
        unit.arguments.clear();
    }
    for profile in &mut report.profiles {
        profile.templates.clear();
        profile.functions.clear();
        profile.evaluations.clear();
        profile.header_costs.clear();
        profile.includes.clear();
    }
    for header in &mut report.headers {
        header.samples_ms.clear();
        header.evidence.clear();
    }
    Ok(serde_json::to_value(report)?)
}

fn append_bytes<W: Write>(archive: &mut tar::Builder<W>, name: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    archive.append_data(&mut header, name, bytes)?;
    Ok(())
}
