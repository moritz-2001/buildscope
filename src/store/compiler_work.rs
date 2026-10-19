//! Global compiler-work queries over normalized scopes. `CompilerWorkQuery` keeps
//! filters identical between entity rankings, contributing units, and event pages.
use super::Store;
use anyhow::{Result, ensure};
use rusqlite::{functions::FunctionFlags, named_params};
use serde_json::{Value, json};

/// A page of compiler work within one capture. Totals are inclusive recorded
/// durations, never elapsed time. Compiler identities and activities stay separate.
pub struct CompilerWorkQuery<'a> {
    /// templates, functions, or evaluations; rejects other categories.
    pub category: &'a str,
    /// Literal substring of the exact spelling or source path; empty matches all.
    pub text: &'a str,
    /// Exact CMake target name; empty selects all targets.
    pub target: &'a str,
    /// Compiler index in the capture inventory; None keeps separate rows for all compilers.
    pub compiler: Option<usize>,
    /// Native activity name; empty includes all activities without merging them.
    pub activity: &'a str,
    /// Approximate template-family display grouping; exact spellings remain in drill-down.
    pub families: bool,
    /// Opaque returned row key; empty requests the global ranking.
    pub entity: &'a str,
    /// units, members, or events when entity is set; members retain exact source variants.
    pub breakdown: &'a str,
    /// Unit ID for an event breakdown; empty permits events from all contributing units.
    pub unit: &'a str,
    /// work, units, count, longest, or name; unknown values are rejected.
    pub order: &'a str,
    /// Whether the selected ordering is descending.
    pub descending: bool,
    /// Matching groups to skip; each breakdown pages independently.
    pub offset: usize,
    /// Requested page size, capped at 200.
    pub limit: usize,
}

impl Default for CompilerWorkQuery<'_> {
    fn default() -> Self {
        Self {
            category: "templates",
            text: "",
            target: "",
            compiler: None,
            activity: "",
            families: false,
            entity: "",
            breakdown: "units",
            unit: "",
            order: "work",
            descending: true,
            offset: 0,
            limit: 100,
        }
    }
}

impl Store {
    /// Aggregate named scopes across translation units, or page a selected entity's
    /// exact spellings, per-unit contributions, or original events. The same filters
    /// precede every aggregation, so target selection changes every displayed total.
    pub fn compiler_work(&self, run: &str, query: CompilerWorkQuery<'_>) -> Result<Value> {
        ensure!(
            ["templates", "functions", "evaluations"].contains(&query.category),
            "unknown compiler-work category"
        );
        ensure!(
            ["units", "members", "events"].contains(&query.breakdown),
            "unknown compiler-work breakdown"
        );
        let order = match query.order {
            "work" => "work_ms",
            "units" => "units",
            "count" => "count",
            "longest" => "max_ms",
            "name" => "name",
            _ => anyhow::bail!("unknown compiler-work order"),
        };
        let connection = self.connection()?;
        connection.create_scalar_function(
            "display_family",
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |context| Ok(display_family(&context.get::<String>(0)?)),
        )?;
        // Keep coverage, summary, total count, and the page on the same read snapshot while
        // a recorder may be publishing the latest invocation for another unit.
        let connection = connection.unchecked_transaction()?;
        ensure!(
            query
                .compiler
                .is_none_or(|value| value <= i64::MAX as usize),
            "invalid compiler index"
        );
        let compiler = query.compiler.map(|v| v as i64).unwrap_or(-1);
        let families = query.families && query.category == "templates";
        // This connection owns both temporary tables; closing it clears them after
        // success or failure. The keyed inventory lets event-first queries look up
        // metadata without scanning units or revisiting large profile JSON rows.
        // Keep target filtering here so coverage and GCC timers share its scope.
        let target_filter = if query.target.is_empty() {
            ":target=''"
        } else {
            "u.target=:target"
        };
        connection.execute_batch(
            "CREATE TEMP TABLE compiler_work_units(
                unit TEXT PRIMARY KEY,has_profile INTEGER NOT NULL,
                source_key TEXT NOT NULL,output_key TEXT NOT NULL,compiler INTEGER NOT NULL,
                family TEXT,event_count INTEGER,granularity_us INTEGER)",
        )?;
        connection.execute(
            &format!(
                "INSERT INTO compiler_work_units
                SELECT u.unit,p.unit IS NOT NULL,u.source_key,u.output_key,u.compiler,
                    p.family,p.event_count,p.granularity_us
                FROM report_units u LEFT JOIN profiles p ON p.run=u.run AND p.unit=u.unit
                WHERE u.run=:run AND {target_filter}"
            ),
            named_params! {":run":run,":target":query.target},
        )?;
        let coverage: Vec<Value> = connection
            .prepare(
                "SELECT compiler,count(*),sum(has_profile),
                    sum(CASE WHEN family='clang' AND event_count>0 THEN 1 ELSE 0 END),
                    group_concat(DISTINCT granularity_us)
                FROM compiler_work_units GROUP BY compiler ORDER BY compiler",
            )?
            .query_map([], |row| {
                Ok(
                    json!({"compiler":row.get::<_,i64>(0)?,"units":row.get::<_,i64>(1)?,
                    "profile_units":row.get::<_,i64>(2)?,"trace_units":row.get::<_,i64>(3)?,
                    "granularities_us":row.get::<_,Option<String>>(4)?}),
                )
            })?
            .collect::<rusqlite::Result<_>>()?;
        // Native GCC timers remain separate from named Clang work. Timer names and
        // nesting are retained; a total here is not a partition of compilation time.
        let timers: Vec<Value> = connection
            .prepare(
                "SELECT s.compiler,m.name,sum(m.value),count(DISTINCT s.unit)
                FROM compiler_work_units s JOIN profile_phases m ON m.run=:run AND m.unit=s.unit
                WHERE s.family='gcc' AND (:compiler<0 OR s.compiler=:compiler)
                    AND m.measurement_unit='ms'
                    AND ((:category='templates' AND lower(m.name) LIKE '%template%instantiat%')
                        OR (:category='functions' AND (lower(m.name) LIKE '%opt%'
                            OR lower(m.name) LIKE '%generat%'))
                        OR (:category='evaluations' AND lower(m.name) LIKE '%constant%expr%'))
                GROUP BY s.compiler,m.name ORDER BY 3 DESC,1,2",
            )?
            .query_map(
                named_params! {":run":run,":compiler":compiler,":category":query.category},
                |row| {
                    Ok(
                        json!({"compiler":row.get::<_,i64>(0)?,"name":row.get::<_,String>(1)?,
                    "work_ms":row.get::<_,f64>(2)?,"units":row.get::<_,i64>(3)?}),
                    )
                },
            )?
            .collect::<rusqlite::Result<_>>()?;
        // Empty target/compiler/unit selections should not scan the event index.
        // Profile presence, rather than event_count metadata, determines eligibility.
        let has_named_units: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM compiler_work_units
                WHERE family='clang' AND (:compiler<0 OR compiler=:compiler)
                    AND (:unit='' OR unit=:unit))",
            named_params! {":compiler":compiler,":unit":query.unit},
            |row| row.get(0),
        )?;
        if !has_named_units {
            return Ok(json!({"rows":[],"total":0,"summary":{"work_ms":0.0},
                "offset":query.offset.min(i64::MAX as usize),"limit":query.limit.clamp(1,200),
                "coverage":coverage,"timers":timers}));
        }
        // Literal activity predicates expose the partial index's leading run/name
        // keys to SQLite. CROSS JOIN keeps events first and the small keyed lookup
        // second, avoiding a scan of every activity in each selected unit.
        let category_filter = match query.category {
            "templates" => "e.name IN ('InstantiateClass','InstantiateFunction')",
            "functions" => "e.name IN ('OptFunction','CodeGen Function')",
            _ => "(e.name GLOB 'Evaluate*' OR e.name='isIntegerConstantExpr')",
        };
        // Decode only to add safe narrowing predicates. The canonical opaque-key
        // comparison below remains authoritative, including malformed keys and
        // exact file/line variants. Families cannot filter by exact spelling.
        let entity_key = serde_json::from_str::<Value>(query.entity).ok();
        let entity_activity = entity_key.as_ref().and_then(|key| key.get(1)?.as_str());
        let entity_spelling = if families {
            None
        } else {
            entity_key.as_ref().and_then(|key| key.get(2)?.as_str())
        };
        let entity_activity_filter = if entity_activity.is_some() {
            "e.name=:entity_activity"
        } else {
            ":entity_activity IS NULL"
        };
        let entity_spelling_filter = if entity_spelling.is_some() {
            "e.detail=:entity_spelling"
        } else {
            ":entity_spelling IS NULL"
        };
        // A restricted unit set must reject events using the index's unit column
        // before a spelling/path search fetches their payload. The later keyed join
        // alone lets SQLite hoist the event-only text predicate ahead of that lookup.
        let scoped_event_filter =
            if query.target.is_empty() && query.compiler.is_none() && query.unit.is_empty() {
                ""
            } else {
                "AND e.unit IN (SELECT unit FROM compiler_work_units
                WHERE family='clang' AND (:compiler<0 OR compiler=:compiler)
                    AND (:unit='' OR unit=:unit))"
            };
        let activity_filter = if query.activity.is_empty() {
            ":activity=''"
        } else {
            "e.name=:activity"
        };
        // Keep the complete partial-index predicate below: SQLite cannot infer
        // its applicability from a category's smaller set of activities alone.
        let cte = format!("WITH named AS (
            SELECT e.*,s.compiler,s.source_key,s.output_key,
                CASE WHEN :families THEN display_family(e.detail) ELSE e.detail END AS spelling
            FROM events e INDEXED BY compiler_work_query
                CROSS JOIN compiler_work_units s ON s.unit=e.unit
            WHERE e.run=:run AND {category_filter}
                {scoped_event_filter}
                AND (e.name IN ('InstantiateClass','InstantiateFunction','OptFunction','CodeGen Function','isIntegerConstantExpr') OR e.name GLOB 'Evaluate*')
                AND s.family='clang'
                AND {entity_activity_filter} AND {entity_spelling_filter}
                AND (:unit='' OR e.unit=:unit)
                AND (:compiler<0 OR s.compiler=:compiler) AND {activity_filter}
                AND (:text='' OR instr(lower(e.detail||char(10)||coalesce(e.file,'')),lower(:text))>0)
        ), keyed AS (
            SELECT *,json_array(compiler,name,spelling,CASE WHEN :families THEN NULL ELSE file END,
                CASE WHEN :families THEN NULL ELSE line END) AS entity_key FROM named
        ), selected AS (
            SELECT * FROM keyed WHERE (:entity='' OR entity_key=:entity)
        )");
        let parameters = named_params! {
            ":run":run, ":compiler":compiler,
            ":activity":query.activity, ":text":query.text, ":families":families,
            ":entity":query.entity, ":unit":query.unit,
            ":entity_activity":entity_activity, ":entity_spelling":entity_spelling,
        };
        let grouped = if query.entity.is_empty() {
            "SELECT entity_key AS key,compiler,name AS activity,spelling AS name,
                CASE WHEN :families THEN NULL ELSE file END AS file,
                CASE WHEN :families THEN NULL ELSE line END AS line,
                sum(duration)/1000.0 AS work_ms,count(*) AS count,max(duration)/1000.0 AS max_ms,
                count(DISTINCT unit) AS units,NULL AS unit_id,NULL AS source_key,NULL AS output_key,NULL AS event
                FROM selected GROUP BY entity_key"
        } else {
            match query.breakdown {
                "members" => "SELECT json_array(compiler,name,detail,file,line) AS key,compiler,name AS activity,detail AS name,
                    file,line,sum(duration)/1000.0 AS work_ms,count(*) AS count,max(duration)/1000.0 AS max_ms,
                    count(DISTINCT unit) AS units,NULL AS unit_id,NULL AS source_key,NULL AS output_key,NULL AS event
                    FROM selected GROUP BY compiler,name,detail,file,line",
                "events" => "SELECT entity_key AS key,compiler,name AS activity,detail AS name,file,line,
                    duration/1000.0 AS work_ms,1 AS count,duration/1000.0 AS max_ms,1 AS units,
                    unit AS unit_id,source_key,output_key,data AS event FROM selected",
                _ => "SELECT entity_key AS key,compiler,name AS activity,spelling AS name,NULL AS file,NULL AS line,
                    sum(duration)/1000.0 AS work_ms,count(*) AS count,max(duration)/1000.0 AS max_ms,
                    1 AS units,unit AS unit_id,source_key,output_key,NULL AS event
                    FROM selected GROUP BY unit",
            }
        };
        // Evaluate the expensive grouping once, then read totals and a bounded
        // sorted page from its connection-local result. Empty and out-of-range
        // pages still return totals for the entire filtered selection.
        connection.execute(
            &format!("CREATE TEMP TABLE compiler_work_groups AS {cte} {grouped}"),
            parameters,
        )?;
        let (total, work_ms): (i64, f64) = connection.query_row(
            "SELECT count(*),coalesce(sum(work_ms),0.0) FROM compiler_work_groups",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let limit = query.limit.clamp(1, 200);
        let offset = query.offset.min(i64::MAX as usize);
        let direction = if query.descending { "DESC" } else { "ASC" };
        // Only validated order tokens and bounded integers enter SQL. User text and
        // opaque entity keys are always bound parameters, including drill-downs.
        let sql = format!(
            "SELECT * FROM compiler_work_groups ORDER BY {order} {direction},key,unit_id,event LIMIT {limit} OFFSET {offset}"
        );
        let rows: Vec<Value> = connection.prepare(&sql)?.query_map([], |row| {
            let event: Option<String> = row.get(13)?;
            Ok(json!({"key":row.get::<_,String>(0)?,"compiler":row.get::<_,i64>(1)?,
                "activity":row.get::<_,String>(2)?,"name":row.get::<_,String>(3)?,
                "file":row.get::<_,Option<String>>(4)?,"line":row.get::<_,Option<i64>>(5)?,
                "work_ms":row.get::<_,f64>(6)?,"count":row.get::<_,i64>(7)?,"max_ms":row.get::<_,f64>(8)?,
                "units":row.get::<_,i64>(9)?,"unit_id":row.get::<_,Option<String>>(10)?,
                "source_key":row.get::<_,Option<String>>(11)?,"output_key":row.get::<_,Option<String>>(12)?,
                "event":event.and_then(|value|serde_json::from_str::<Value>(&value).ok())}))
        })?.collect::<rusqlite::Result<_>>()?;
        Ok(
            json!({"rows":rows,"total":total,"summary":{"work_ms":work_ms},"offset":offset,"limit":limit,"coverage":coverage,"timers":timers}),
        )
    }
}

/// Approximate browsing only: never used as semantic specialization identity.
/// Preserve operator tokens and quoted/parenthesized expressions instead of
/// treating every angle bracket as a template delimiter. Ambiguous or incomplete
/// spellings stay exact, so a failed display parse cannot discard an observation.
fn display_family(name: &str) -> String {
    let characters: Vec<char> = name.chars().collect();
    let mut result = String::new();
    let mut index = 0;
    while index < characters.len() {
        if let Some(end) = angle_operator_end(&characters, index) {
            result.extend(&characters[index..end]);
            index = end;
        } else if characters[index] == '<' {
            let Some(end) = template_arguments_end(&characters, index) else {
                return name.into();
            };
            result.push_str("<…>");
            index = end;
        } else {
            result.push(characters[index]);
            index += 1;
        }
    }
    result
}

fn template_arguments_end(characters: &[char], start: usize) -> Option<usize> {
    let mut depth = 1;
    let mut delimiters = Vec::new();
    let mut index = start + 1;
    while index < characters.len() {
        if let Some(end) = angle_operator_end(characters, index) {
            index = end;
            continue;
        }
        let character = characters[index];
        let digit_separator = character == '\''
            && index > 0
            && characters[index - 1].is_ascii_hexdigit()
            && characters
                .get(index + 1)
                .is_some_and(char::is_ascii_hexdigit);
        if matches!(character, '\'' | '"') && !digit_separator {
            index = quoted_literal_end(characters, index)?;
            continue;
        }
        match character {
            '(' | '[' | '{' => delimiters.push(character),
            ')' | ']' | '}' => {
                let opening = match character {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if delimiters.pop() != Some(opening) {
                    return None;
                }
            }
            '<' if delimiters.is_empty() => depth += 1,
            '>' if delimiters.is_empty() => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn angle_operator_end(characters: &[char], start: usize) -> Option<usize> {
    const OPERATOR: [char; 8] = ['o', 'p', 'e', 'r', 'a', 't', 'o', 'r'];
    if characters.get(start..start + OPERATOR.len()) != Some(OPERATOR.as_slice())
        || start > 0 && (characters[start - 1].is_alphanumeric() || characters[start - 1] == '_')
    {
        return None;
    }
    let mut symbol = start + OPERATOR.len();
    while characters
        .get(symbol)
        .is_some_and(|value| value.is_whitespace())
    {
        symbol += 1;
    }
    for operator in ["<=>", "<<=", ">>=", "<<", ">>", "<=", ">=", "<", ">"] {
        if operator
            .chars()
            .enumerate()
            .all(|(offset, value)| characters.get(symbol + offset) == Some(&value))
        {
            return Some(symbol + operator.len());
        }
    }
    None
}

fn quoted_literal_end(characters: &[char], start: usize) -> Option<usize> {
    let quote = characters[start];
    if quote == '"' && start > 0 && characters[start - 1] == 'R' {
        let opening = (start + 1..characters.len()).find(|&index| characters[index] == '(')?;
        let delimiter = &characters[start + 1..opening];
        if delimiter.len() > 16 {
            return None;
        }
        for index in opening + 1..characters.len() {
            let closing = index + 1 + delimiter.len();
            if characters[index] == ')'
                && characters.get(index + 1..closing) == Some(delimiter)
                && characters.get(closing) == Some(&'"')
            {
                return Some(closing + 1);
            }
        }
        return None;
    }
    let mut index = start + 1;
    while index < characters.len() {
        if characters[index] == quote {
            return Some(index + 1);
        }
        if characters[index] == '\\' {
            index += 1;
        }
        index += 1;
    }
    None
}
