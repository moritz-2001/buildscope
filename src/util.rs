//! Process, identity, path, and atomic-file operations shared by collectors.
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

/// Wall-clock milliseconds for history ordering, never for measuring durations.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Unique artifact name across processes and repeated calls.
pub fn unique_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{:x}-{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// SHA256 identity for configuration and content comparisons.
pub fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

/// Resolve existing paths canonically and missing paths lexically.
pub fn resolve(base: impl AsRef<Path>, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.as_ref().join(path)
    };
    if let Ok(value) = joined.canonicalize() {
        return value;
    }
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            _ => out.push(part),
        }
    }
    out
}

/// Convert a filesystem path into a manifest/display string.
pub fn path_string(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().into_owned()
}

/// Portable identity preserving the distinction between generated and external files.
pub fn path_key(path: &str, source: &str, build: &str) -> String {
    let path = Path::new(path);
    if let Ok(rel) = path.strip_prefix(build) {
        return format!("<build>/{}", rel.display());
    }
    if let Ok(rel) = path.strip_prefix(source) {
        return rel.to_string_lossy().into_owned();
    }
    format!("<external>{}", path.display())
}

/// Atomically replace JSON so interrupted writers never expose a partial manifest.
pub fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", unique_id()));
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

/// Run a metadata query and retain diagnostics on failure.
pub fn checked(command: &mut Command) -> Result<Output> {
    let description = format!("{command:?}");
    let result = command
        .output()
        .with_context(|| format!("execute {description}"))?;
    if !result.status.success() {
        bail!(
            "{description} failed:\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(result)
}

/// Read an optional artifact, returning empty text when it does not exist.
pub fn read_optional(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// Resolve a retained artifact without allowing a manifest to escape its run directory.
pub fn artifact_path(root: &Path, relative: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        !relative.is_empty()
            && Path::new(relative)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        "invalid retained artifact path"
    );
    let path = root.join(relative);
    if path.exists() {
        let path = path.canonicalize()?;
        anyhow::ensure!(
            path.starts_with(root.canonicalize()?),
            "artifact escapes the capture directory"
        );
        Ok(path)
    } else {
        Ok(path)
    }
}

/// Extract an output argument from an already-tokenized compiler command.
pub fn output_argument(arguments: &[String]) -> Option<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == "-o")
        .map(|pair| pair[1].clone())
        .or_else(|| {
            arguments.iter().find_map(|arg| {
                arg.strip_prefix("-o")
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
        })
}
