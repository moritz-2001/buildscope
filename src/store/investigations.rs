//! Optional saved investigations. Report queries only read capture artifacts;
//! local Git collection is an explicit CLI operation outside measured builds.
use super::*;
use crate::{
    header_history::{self, HistorySnapshot},
    rebuild::RebuildReport,
};
use serde::de::DeserializeOwned;

const MAX_ANALYSIS_BYTES: u64 = 64 * 1024 * 1024;

/// Selection and bounded paging for committed-header hotspots.
#[derive(Default)]
pub struct HeaderHotspotQuery<'a> {
    /// Window ending at capture observation time; zero selects 90 days.
    pub days: u32,
    /// Exact report target; empty selects every target.
    pub target: &'a str,
    /// Case-insensitive substring of the portable header path.
    pub text: &'a str,
    /// Exact header path for saved commit details; empty returns a ranking.
    pub path: &'a str,
    /// Number of ranked headers to skip.
    pub offset: usize,
    /// Page size, bounded to 200; zero selects 100.
    pub limit: usize,
}

impl Store {
    /// Collect history only for a locally recorded run and retain it for offline reports.
    pub fn analyze_history(&self, run: &str, days: u32) -> Result<HistorySnapshot> {
        ensure!(
            (1..=3660).contains(&days),
            "history window must be between 1 and 3660 days"
        );
        let capture = self.load(run)?;
        ensure!(
            self.is_local(&capture.id)?,
            "history enrichment requires a locally recorded capture; imported reports use saved history"
        );
        let since = capture
            .git
            .observed_ms
            .unwrap_or(capture.started_ms)
            .saturating_sub(u64::from(days) * 86_400_000);
        let root = self.run_dir(&capture.id)?;
        if let Some(saved) = read_analysis::<HistorySnapshot>(&root, "header-history.json")? {
            ensure!(
                saved.version == 1,
                "unsupported saved header-history version"
            );
            if saved.complete
                && capture.git.commit.as_deref() == Some(saved.anchor.as_str())
                && saved.since_ms <= since
            {
                return Ok(saved);
            }
        }
        let snapshot = header_history::collect(&capture, days)?;
        ensure!(
            serde_json::to_vec(&snapshot)?.len() as u64 <= MAX_ANALYSIS_BYTES,
            "history analysis exceeds the 64 MiB artifact budget; choose a smaller window"
        );
        write_json(&root.join("header-history.json"), &snapshot)?;
        Ok(snapshot)
    }

    /// Read a bounded ranking or commit detail without invoking Git or reading source paths.
    pub fn header_hotspots(&self, run: &str, query: HeaderHotspotQuery<'_>) -> Result<Value> {
        let capture = self.load(run)?;
        let Some(history) =
            read_analysis::<HistorySnapshot>(&self.run_dir(&capture.id)?, "header-history.json")?
        else {
            return Ok(
                json!({"available":false,"rows":[],"total":0,"notes":["Committed header history was not collected for this capture."],
                "command":format!("buildscope analyze-history {} --days 180",capture.id)}),
            );
        };
        ensure!(
            history.version == 1,
            "unsupported saved header-history version"
        );
        ensure!(
            capture.git.commit.as_deref() == Some(history.anchor.as_str()),
            "header history does not match the capture's saved commit"
        );
        let days = if query.days == 0 { 90 } else { query.days };
        ensure!(days <= 3660, "history window exceeds 3660 days");
        let mut value = header_history::hotspots(
            &capture,
            &history,
            days,
            query.target,
            query.text,
            query.path,
        );
        let limit = if query.limit == 0 {
            100
        } else {
            query.limit.min(200)
        };
        let rows = value["rows"]
            .as_array_mut()
            .context("invalid header ranking")?;
        let total = rows.len();
        *rows = rows
            .iter()
            .skip(query.offset)
            .take(limit)
            .cloned()
            .collect();
        value["total"] = json!(total);
        value["offset"] = json!(query.offset);
        value["limit"] = json!(limit);
        value["command"] = json!(format!(
            "buildscope analyze-history {} --days {days}",
            capture.id
        ));
        Ok(value)
    }

    /// Load retained timestamp comparisons; absence in old captures is not a zero finding count.
    pub fn rebuild_analysis(&self, run: &str) -> Result<Value> {
        let capture = self.load(run)?;
        match read_analysis::<RebuildReport>(&self.run_dir(&capture.id)?, "rebuild-evidence.json")?
        {
            Some(mut report) => {
                ensure!(
                    report.version == 1,
                    "unsupported saved rebuild-evidence version"
                );
                report.before_inputs.clear();
                let mut value = serde_json::to_value(report)?;
                // The raw artifact retains the full recovery baseline; report clients need only changed comparisons.
                value.as_object_mut().unwrap().remove("before_inputs");
                // Nanosecond Unix times exceed JavaScript's exact integer range.
                if let Some(comparisons) = value["comparisons"].as_array_mut() {
                    for comparison in comparisons {
                        for boundary in ["before", "after"] {
                            if let Some(stamp) = comparison[boundary].as_object_mut()
                                && let Some(modified) = stamp.get_mut("modified_ns")
                            {
                                *modified = Value::String(modified.to_string());
                            }
                        }
                    }
                }
                value["available"] = json!(true);
                Ok(value)
            }
            None => Ok(
                json!({"available":false,"candidates":[],"notes":["Rebuild fingerprint evidence was not retained for this capture. Record an ordinary build with this version to collect it."]}),
            ),
        }
    }
}

fn read_analysis<T: DeserializeOwned>(root: &Path, name: &str) -> Result<Option<T>> {
    let path = root.join(name);
    if !path.try_exists()? {
        return Ok(None);
    }
    let path = artifact_path(root, name)?;
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= MAX_ANALYSIS_BYTES,
        "saved analysis exceeds the 64 MiB budget"
    );
    Ok(Some(serde_json::from_reader(BufReader::new(file))?))
}

pub(super) fn validate_optional_analyses(root: &Path) -> Result<()> {
    if let Some(history) = read_analysis::<HistorySnapshot>(root, "header-history.json")? {
        ensure!(
            history.version == 1,
            "unsupported saved header-history version"
        );
    }
    if let Some(report) = read_analysis::<RebuildReport>(root, "rebuild-evidence.json")? {
        ensure!(
            report.version == 1,
            "unsupported saved rebuild-evidence version"
        );
    }
    Ok(())
}
