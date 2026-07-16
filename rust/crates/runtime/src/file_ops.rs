use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use glob::Pattern;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use walkdir::{DirEntry, WalkDir};

/// Bounds and default for `CLAW_READ_FILE_MAX_BYTES`, the largest file
/// `read_file` will load whole. Files above the limit must be read in
/// windows via `offset`/`limit` (streamed, never fully loaded) or rejected
/// with a clear error.
const READ_FILE_MAX_BYTES_DEFAULT: u64 = 2_000_000;
const READ_FILE_MAX_BYTES_MIN: u64 = 64 * 1024;
const READ_FILE_MAX_BYTES_MAX: u64 = 50 * 1024 * 1024;

/// Maximum file size that can be written (10 MB).
const MAX_WRITE_SIZE: usize = 10 * 1024 * 1024;

/// Parses a raw `CLAW_READ_FILE_MAX_BYTES` value into the whole-file read
/// limit in bytes, clamped to `65_536..=52_428_800` (64 KB..=50 MB).
/// Missing, empty, or unparseable values fall back to the default
/// (2 000 000 bytes).
#[must_use]
pub fn parse_read_file_max_bytes(raw: Option<&str>) -> u64 {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(READ_FILE_MAX_BYTES_DEFAULT, |value| {
            value.clamp(READ_FILE_MAX_BYTES_MIN, READ_FILE_MAX_BYTES_MAX)
        })
}

/// Thin env wrapper over [`parse_read_file_max_bytes`]: reads
/// `CLAW_READ_FILE_MAX_BYTES` from the process environment.
#[must_use]
pub fn read_file_max_bytes() -> u64 {
    parse_read_file_max_bytes(std::env::var("CLAW_READ_FILE_MAX_BYTES").ok().as_deref())
}

/// In-process registry of the on-disk mtime observed at each file's last
/// successful `read_file`/`write_file`/`edit_file`, used by `edit_file` to
/// warn when the file changed on disk (editor, formatter, another agent)
/// since this process last saw it.
static FILE_MTIME_REGISTRY: OnceLock<Mutex<HashMap<PathBuf, SystemTime>>> = OnceLock::new();

fn file_mtime_registry() -> &'static Mutex<HashMap<PathBuf, SystemTime>> {
    FILE_MTIME_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Records `path`'s current on-disk mtime in the registry (best effort:
/// filesystems without mtime support simply record nothing).
fn record_file_mtime(path: &Path) {
    if let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) {
        record_file_mtime_as(path, modified);
    }
}

fn record_file_mtime_as(path: &Path, mtime: SystemTime) {
    file_mtime_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf(), mtime);
}

fn recorded_file_mtime(path: &Path) -> Option<SystemTime> {
    file_mtime_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .copied()
}

/// Pure decision core of the external-modification guard: given the mtime
/// recorded at the last read/write and the current on-disk mtime, returns
/// the warning to attach to an edit result. `None` when the file was never
/// tracked (first touch in this process) or has not changed — a warning,
/// never an error, so legitimate flows keep working.
#[must_use]
pub fn external_modification_warning(
    recorded_mtime: Option<SystemTime>,
    current_mtime: Option<SystemTime>,
) -> Option<String> {
    let recorded = recorded_mtime?;
    let current = current_mtime?;
    (recorded != current).then(|| {
        String::from(
            "WARNING: the file changed on disk since it was last read in this session; \
             the edit was applied on top of the current on-disk content — re-read the \
             file to verify the result",
        )
    })
}

const GLOB_SEARCH_IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".build",
    "target",
    "dist",
    "coverage",
];

/// Check whether a file appears to contain binary content by examining
/// the first chunk for NUL bytes.
fn is_binary_file(path: &Path) -> io::Result<bool> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let bytes_read = file.read(&mut buffer)?;
    Ok(buffer[..bytes_read].contains(&0))
}

/// Validate that a resolved path stays within the given workspace root.
/// Returns the canonical path on success, or an error if the path escapes
/// the workspace boundary (e.g. via `../` traversal or symlink).
#[allow(dead_code)]
fn validate_workspace_boundary(resolved: &Path, workspace_root: &Path) -> io::Result<()> {
    if !resolved.starts_with(workspace_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "path {} escapes workspace boundary {}",
                resolved.display(),
                workspace_root.display()
            ),
        ));
    }
    Ok(())
}

/// Text payload returned by file-reading operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextFilePayload {
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "numLines")]
    pub num_lines: usize,
    #[serde(rename = "startLine")]
    pub start_line: usize,
    #[serde(rename = "totalLines")]
    pub total_lines: usize,
}

/// Output envelope for the `read_file` tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    pub file: TextFilePayload,
}

/// Structured patch hunk emitted by write and edit operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StructuredPatchHunk {
    #[serde(rename = "oldStart")]
    pub old_start: usize,
    #[serde(rename = "oldLines")]
    pub old_lines: usize,
    #[serde(rename = "newStart")]
    pub new_start: usize,
    #[serde(rename = "newLines")]
    pub new_lines: usize,
    pub lines: Vec<String>,
}

/// Output envelope for full-file write operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "originalFile")]
    pub original_file: Option<String>,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
}

/// Output envelope for targeted string-replacement edits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditFileOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "oldString")]
    pub old_string: String,
    #[serde(rename = "newString")]
    pub new_string: String,
    #[serde(rename = "originalFile")]
    pub original_file: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "userModified")]
    pub user_modified: bool,
    #[serde(rename = "replaceAll")]
    pub replace_all: bool,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
    /// Set when the file's on-disk mtime changed since this process last
    /// read or wrote it (see [`external_modification_warning`]). Advisory
    /// only — the edit is still applied.
    #[serde(
        rename = "externalModificationWarning",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub external_modification_warning: Option<String>,
}

/// Result of a glob-based filename search.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobSearchOutput {
    #[serde(rename = "durationMs")]
    pub duration_ms: u128,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub truncated: bool,
}

/// Parameters accepted by the grep-style search tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchInput {
    pub pattern: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    #[serde(rename = "output_mode")]
    pub output_mode: Option<String>,
    #[serde(rename = "-B")]
    pub before: Option<usize>,
    #[serde(rename = "-A")]
    pub after: Option<usize>,
    #[serde(rename = "-C")]
    pub context_short: Option<usize>,
    pub context: Option<usize>,
    #[serde(rename = "-n")]
    pub line_numbers: Option<bool>,
    #[serde(rename = "-i")]
    pub case_insensitive: Option<bool>,
    #[serde(rename = "type")]
    pub file_type: Option<String>,
    pub head_limit: Option<usize>,
    pub offset: Option<usize>,
    pub multiline: Option<bool>,
}

/// Result payload returned by the grep-style search tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchOutput {
    pub mode: Option<String>,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub content: Option<String>,
    #[serde(rename = "numLines")]
    pub num_lines: Option<usize>,
    #[serde(rename = "numMatches")]
    pub num_matches: Option<usize>,
    #[serde(rename = "appliedLimit")]
    pub applied_limit: Option<usize>,
    #[serde(rename = "appliedOffset")]
    pub applied_offset: Option<usize>,
}

/// Reads a text file and returns a line-windowed payload.
///
/// Files larger than `CLAW_READ_FILE_MAX_BYTES` (default 2 000 000 bytes,
/// clamped to 64 KB..=50 MB) are never loaded whole: with `offset`/`limit`
/// the requested window is streamed line by line instead, and without them
/// the call fails with the actual size and a suggestion to pass
/// `offset`/`limit`.
pub fn read_file(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> io::Result<ReadFileOutput> {
    read_file_with_max_bytes(path, offset, limit, read_file_max_bytes())
}

/// Limit-injected core of [`read_file`], so tests can exercise the
/// oversized-file paths without multi-megabyte fixtures or env mutation.
fn read_file_with_max_bytes(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    max_bytes: u64,
) -> io::Result<ReadFileOutput> {
    let absolute_path = normalize_path(path)?;

    // Detect binary files
    if is_binary_file(&absolute_path)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file appears to be binary",
        ));
    }

    // Check file size before loading it whole.
    let metadata = fs::metadata(&absolute_path)?;
    if metadata.len() > max_bytes {
        if offset.is_none() && limit.is_none() {
            return Err(read_file_too_large_error(metadata.len(), max_bytes));
        }
        // A window was requested: stream just that window instead of
        // loading the whole file into memory.
        let output = read_file_window_streaming(&absolute_path, offset.unwrap_or(0), limit)?;
        record_file_mtime(&absolute_path);
        return Ok(output);
    }

    let content = fs::read_to_string(&absolute_path)?;
    let lines: Vec<&str> = content.lines().collect();
    let start_index = offset.unwrap_or(0).min(lines.len());
    let end_index = limit.map_or(lines.len(), |limit| {
        start_index.saturating_add(limit).min(lines.len())
    });
    let selected = lines[start_index..end_index].join("\n");
    record_file_mtime(&absolute_path);

    Ok(ReadFileOutput {
        kind: String::from("text"),
        file: TextFilePayload {
            file_path: absolute_path.to_string_lossy().into_owned(),
            content: selected,
            num_lines: end_index.saturating_sub(start_index),
            start_line: start_index.saturating_add(1),
            total_lines: lines.len(),
        },
    })
}

/// Error returned when a file exceeds the whole-file read limit and no
/// window was requested.
fn read_file_too_large_error(actual_bytes: u64, max_bytes: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "file is too large to read whole ({actual_bytes} bytes, limit {max_bytes} bytes \
             from CLAW_READ_FILE_MAX_BYTES); pass offset/limit to read a window of lines \
             instead, or raise CLAW_READ_FILE_MAX_BYTES"
        ),
    )
}

/// Streams a line window out of a file that is too large to load whole.
/// Only the selected window is kept in memory; the rest of the file is
/// scanned to report `total_lines`.
fn read_file_window_streaming(
    absolute_path: &Path,
    start_index: usize,
    limit: Option<usize>,
) -> io::Result<ReadFileOutput> {
    use std::io::BufRead as _;

    let file = fs::File::open(absolute_path)?;
    let reader = io::BufReader::new(file);
    let mut selected: Vec<String> = Vec::new();
    let mut total_lines = 0_usize;
    for line in reader.lines() {
        // Invalid UTF-8 fails here, matching the read_to_string behaviour
        // of the whole-file path.
        let line = line?;
        if total_lines >= start_index && limit.is_none_or(|limit| selected.len() < limit) {
            selected.push(line);
        }
        total_lines += 1;
    }
    let start_index = start_index.min(total_lines);

    Ok(ReadFileOutput {
        kind: String::from("text"),
        file: TextFilePayload {
            file_path: absolute_path.to_string_lossy().into_owned(),
            num_lines: selected.len(),
            content: selected.join("\n"),
            start_line: start_index.saturating_add(1),
            total_lines,
        },
    })
}

/// Replaces a file's contents and returns patch metadata.
pub fn write_file(path: &str, content: &str) -> io::Result<WriteFileOutput> {
    if content.len() > MAX_WRITE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "content is too large ({} bytes, max {} bytes)",
                content.len(),
                MAX_WRITE_SIZE
            ),
        ));
    }

    let absolute_path = normalize_path_allow_missing(path)?;
    // Existence decides create-vs-update: an existing non-UTF-8 file makes
    // read_to_string fail, which must not be misreported as a "create" of an
    // empty file in the patch metadata.
    let existed_before = absolute_path.exists();
    let original_file = fs::read_to_string(&absolute_path).ok();
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&absolute_path, content)?;
    record_file_mtime(&absolute_path);

    Ok(WriteFileOutput {
        kind: if existed_before {
            String::from("update")
        } else {
            String::from("create")
        },
        file_path: absolute_path.to_string_lossy().into_owned(),
        content: content.to_owned(),
        structured_patch: make_patch(original_file.as_deref().unwrap_or(""), content),
        original_file,
        git_diff: None,
    })
}

/// Performs an in-file string replacement and returns patch metadata.
pub fn edit_file(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> io::Result<EditFileOutput> {
    let absolute_path = normalize_path(path)?;
    let original_file = fs::read_to_string(&absolute_path)?;
    // An empty old_string passes `contains("")` (always true) and would then
    // splice new_string between every character via `replace`, destroying
    // the file.
    if old_string.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "old_string must not be empty",
        ));
    }
    if old_string == new_string {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "old_string and new_string must differ",
        ));
    }
    let occurrences = original_file.matches(old_string).count();
    if occurrences == 0 {
        // Most misses are a stale read: the block moved or was reformatted.
        // Showing a mini-diff against the most similar region lets the caller
        // (usually an agent) re-anchor without a blind retry.
        let hint = closest_match_diff(&original_file, old_string)
            .map(|diff| {
                format!(
                    "; the closest region differs — re-read the file and retry \
                     with the current text:\n{diff}"
                )
            })
            .unwrap_or_default();
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("old_string not found in file{hint}"),
        ));
    }
    // Editing "the first match" of a non-unique string silently mutates a
    // location the caller may not have meant; require a unique anchor or an
    // explicit replace_all.
    if !replace_all && occurrences > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "old_string appears {occurrences} times in the file; include more \
                 surrounding context to make it unique, or pass replace_all: true"
            ),
        ));
    }

    // External-modification guard: compare the mtime recorded at the last
    // read/write in this process against the current on-disk mtime. A
    // mismatch means the file changed underneath us (editor, formatter,
    // another agent) — warn, but never fail, so legitimate flows keep
    // working.
    let current_mtime = fs::metadata(&absolute_path)
        .and_then(|metadata| metadata.modified())
        .ok();
    let external_warning =
        external_modification_warning(recorded_file_mtime(&absolute_path), current_mtime);

    let updated = if replace_all {
        original_file.replace(old_string, new_string)
    } else {
        original_file.replacen(old_string, new_string, 1)
    };
    fs::write(&absolute_path, &updated)?;
    record_file_mtime(&absolute_path);

    Ok(EditFileOutput {
        file_path: absolute_path.to_string_lossy().into_owned(),
        old_string: old_string.to_owned(),
        new_string: new_string.to_owned(),
        original_file: original_file.clone(),
        structured_patch: make_patch(&original_file, &updated),
        user_modified: external_warning.is_some(),
        replace_all,
        git_diff: None,
        external_modification_warning: external_warning,
    })
}

/// Expands a glob pattern and returns matching filenames.
pub fn glob_search(pattern: &str, path: Option<&str>) -> io::Result<GlobSearchOutput> {
    glob_search_impl(pattern, path, None)
}

fn glob_search_impl(
    pattern: &str,
    path: Option<&str>,
    workspace_root: Option<&Path>,
) -> io::Result<GlobSearchOutput> {
    let started = Instant::now();
    let base_dir = path
        .map(normalize_path)
        .transpose()?
        .unwrap_or(std::env::current_dir()?);
    let canonical_root = workspace_root.map(canonicalize_workspace_root);
    if let Some(root) = canonical_root.as_deref() {
        validate_workspace_boundary(&base_dir, root)?;
    }
    let search_pattern = if Path::new(pattern).is_absolute() {
        pattern.to_owned()
    } else {
        base_dir.join(pattern).to_string_lossy().into_owned()
    };

    // The `glob` crate does not support brace expansion ({a,b,c}).
    // Expand braces into multiple patterns so patterns like
    // `Assets/**/*.{cs,uxml,uss}` work correctly.
    let expanded = expand_braces(&search_pattern);

    let mut seen = HashSet::new();
    let mut matches = Vec::new();
    for pat in &expanded {
        let compiled = Pattern::new(pat)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
        let walk_root = derive_glob_walk_root(pat);
        if let Some(root) = canonical_root.as_deref() {
            let canonical_walk_root = walk_root
                .canonicalize()
                .unwrap_or_else(|_| walk_root.clone());
            validate_workspace_boundary(&canonical_walk_root, root)?;
        }
        let entries = WalkDir::new(&walk_root)
            .into_iter()
            .filter_entry(|entry| !should_skip_glob_dir(entry));
        for entry in entries.flatten() {
            let candidate = entry.path();
            if entry.file_type().is_file()
                && compiled.matches_path(candidate)
                && seen.insert(candidate.to_path_buf())
            {
                if let Some(root) = canonical_root.as_deref() {
                    let canonical_candidate = candidate.canonicalize()?;
                    validate_workspace_boundary(&canonical_candidate, root)?;
                }
                matches.push(candidate.to_path_buf());
            }
        }
    }

    matches.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(Reverse)
    });

    let truncated = matches.len() > 100;
    let filenames = matches
        .into_iter()
        .take(100)
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    Ok(GlobSearchOutput {
        duration_ms: started.elapsed().as_millis(),
        num_files: filenames.len(),
        filenames,
        truncated,
    })
}

/// Runs a regex search over workspace files with optional context lines.
pub fn grep_search(input: &GrepSearchInput) -> io::Result<GrepSearchOutput> {
    grep_search_impl(input, None)
}

fn grep_search_impl(
    input: &GrepSearchInput,
    workspace_root: Option<&Path>,
) -> io::Result<GrepSearchOutput> {
    let base_path = input
        .path
        .as_deref()
        .map(normalize_path)
        .transpose()?
        .unwrap_or(std::env::current_dir()?);
    let canonical_root = workspace_root.map(canonicalize_workspace_root);
    if let Some(root) = canonical_root.as_deref() {
        validate_workspace_boundary(&base_path, root)?;
    }

    let regex = RegexBuilder::new(&input.pattern)
        .case_insensitive(input.case_insensitive.unwrap_or(false))
        .dot_matches_new_line(input.multiline.unwrap_or(false))
        .build()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;

    let glob_filter = input
        .glob
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let file_type = input.file_type.as_deref();
    let output_mode = input
        .output_mode
        .clone()
        .unwrap_or_else(|| String::from("files_with_matches"));
    let context = input.context.or(input.context_short).unwrap_or(0);

    let mut filenames = Vec::new();
    let mut content_lines = Vec::new();
    let mut total_matches = 0usize;

    for file_path in collect_search_files(&base_path)? {
        if let Some(root) = canonical_root.as_deref() {
            let canonical_file = file_path.canonicalize()?;
            validate_workspace_boundary(&canonical_file, root)?;
        }
        if !matches_optional_filters(&file_path, glob_filter.as_ref(), file_type) {
            continue;
        }

        let Ok(file_contents) = fs::read_to_string(&file_path) else {
            continue;
        };

        if output_mode == "count" {
            let count = regex.find_iter(&file_contents).count();
            if count > 0 {
                filenames.push(file_path.to_string_lossy().into_owned());
                total_matches += count;
            }
            continue;
        }

        let lines: Vec<&str> = file_contents.lines().collect();
        let mut matched_lines = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if regex.is_match(line) {
                total_matches += 1;
                matched_lines.push(index);
            }
        }

        if matched_lines.is_empty() {
            continue;
        }

        filenames.push(file_path.to_string_lossy().into_owned());
        if output_mode == "content" {
            for index in matched_lines {
                let start = index.saturating_sub(input.before.unwrap_or(context));
                let end = (index + input.after.unwrap_or(context) + 1).min(lines.len());
                for (current, line) in lines.iter().enumerate().take(end).skip(start) {
                    let prefix = if input.line_numbers.unwrap_or(true) {
                        format!("{}:{}:", file_path.to_string_lossy(), current + 1)
                    } else {
                        format!("{}:", file_path.to_string_lossy())
                    };
                    content_lines.push(format!("{prefix}{line}"));
                }
            }
        }
    }

    let (filenames, applied_limit, applied_offset) =
        apply_limit(filenames, input.head_limit, input.offset);
    if output_mode == "content" {
        return Ok(build_grep_content_output(
            output_mode,
            filenames,
            content_lines,
            input.head_limit,
            input.offset,
        ));
    }

    Ok(GrepSearchOutput {
        mode: Some(output_mode.clone()),
        num_files: filenames.len(),
        filenames,
        content: None,
        num_lines: None,
        num_matches: (output_mode == "count").then_some(total_matches),
        applied_limit,
        applied_offset,
    })
}

fn build_grep_content_output(
    output_mode: String,
    filenames: Vec<String>,
    content_lines: Vec<String>,
    head_limit: Option<usize>,
    offset: Option<usize>,
) -> GrepSearchOutput {
    let (lines, limit, offset) = apply_limit(content_lines, head_limit, offset);
    GrepSearchOutput {
        mode: Some(output_mode),
        num_files: filenames.len(),
        filenames,
        num_lines: Some(lines.len()),
        content: Some(lines.join("\n")),
        num_matches: None,
        applied_limit: limit,
        applied_offset: offset,
    }
}

fn canonicalize_workspace_root(workspace_root: &Path) -> PathBuf {
    workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf())
}

fn should_skip_glob_dir(entry: &DirEntry) -> bool {
    entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| GLOB_SEARCH_IGNORED_DIRS.contains(&name))
}

fn derive_glob_walk_root(pattern: &str) -> PathBuf {
    let path = Path::new(pattern);
    let mut prefix = PathBuf::new();
    let mut saw_component = false;

    for component in path.components() {
        let text = component.as_os_str().to_string_lossy();
        if component_contains_glob(&text) {
            break;
        }
        prefix.push(component.as_os_str());
        saw_component = true;
    }

    if saw_component {
        prefix
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

fn component_contains_glob(component: &str) -> bool {
    component.contains('*') || component.contains('?') || component.contains('[')
}

fn collect_search_files(base_path: &Path) -> io::Result<Vec<PathBuf>> {
    if base_path.is_file() {
        return Ok(vec![base_path.to_path_buf()]);
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(base_path) {
        let entry = entry.map_err(|error| io::Error::other(error.to_string()))?;
        if entry.file_type().is_file() {
            files.push(entry.path().to_path_buf());
        }
    }
    Ok(files)
}

fn matches_optional_filters(
    path: &Path,
    glob_filter: Option<&Pattern>,
    file_type: Option<&str>,
) -> bool {
    if let Some(glob_filter) = glob_filter {
        let path_string = path.to_string_lossy();
        if !glob_filter.matches(&path_string) && !glob_filter.matches_path(path) {
            return false;
        }
    }

    if let Some(file_type) = file_type {
        let extension = path.extension().and_then(|extension| extension.to_str());
        if extension != Some(file_type) {
            return false;
        }
    }

    true
}

fn apply_limit<T>(
    items: Vec<T>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> (Vec<T>, Option<usize>, Option<usize>) {
    let offset_value = offset.unwrap_or(0);
    let mut items = items.into_iter().skip(offset_value).collect::<Vec<_>>();
    let explicit_limit = limit.unwrap_or(250);
    if explicit_limit == 0 {
        return (items, None, (offset_value > 0).then_some(offset_value));
    }

    let truncated = items.len() > explicit_limit;
    items.truncate(explicit_limit);
    (
        items,
        truncated.then_some(explicit_limit),
        (offset_value > 0).then_some(offset_value),
    )
}

/// Caps for the `edit_file` mismatch hint so a huge `old_string` or file
/// region can never balloon the error message.
const DIFF_HINT_MAX_LINES_PER_SIDE: usize = 6;
const DIFF_HINT_CONTEXT_LINES: usize = 2;
const DIFF_HINT_MAX_LINE_CHARS: usize = 120;

/// Locates the region of `file_contents` most similar to `old_string` and
/// renders a small diff-style hint: surrounding context lines are indented,
/// the expected (missing) lines are prefixed with `-`, and the lines actually
/// found in the file with `+`. Both sides are capped at
/// [`DIFF_HINT_MAX_LINES_PER_SIDE`] lines of [`DIFF_HINT_MAX_LINE_CHARS`]
/// characters. Returns `None` when no line of the file resembles the
/// expected block (no candidate region to point at).
#[must_use]
pub fn closest_match_diff(file_contents: &str, old_string: &str) -> Option<String> {
    let expected: Vec<&str> = old_string.lines().collect();
    let file_lines: Vec<&str> = file_contents.lines().collect();
    if expected.is_empty() || file_lines.is_empty() {
        return None;
    }

    // Slide a window of the expected block's height over the file (clamped
    // for files shorter than the block) and keep the best-scoring region.
    let window = expected.len().min(file_lines.len());
    let mut best_start = 0usize;
    let mut best_score = 0usize;
    for start in 0..=(file_lines.len() - window) {
        let score = expected
            .iter()
            .zip(&file_lines[start..start + window])
            .map(|(expected_line, found_line)| line_similarity(expected_line, found_line))
            .sum();
        if score > best_score {
            best_score = score;
            best_start = start;
        }
    }
    if best_score == 0 {
        return None;
    }

    let mut rendered = vec![format!("closest match at line {}:", best_start + 1)];
    let context_start = best_start.saturating_sub(DIFF_HINT_CONTEXT_LINES);
    for line in &file_lines[context_start..best_start] {
        rendered.push(format!("  {}", cap_line(line)));
    }
    push_capped_side(&mut rendered, '-', &expected);
    push_capped_side(
        &mut rendered,
        '+',
        &file_lines[best_start..best_start + window],
    );
    let context_end = (best_start + window + DIFF_HINT_CONTEXT_LINES).min(file_lines.len());
    for line in &file_lines[best_start + window..context_end] {
        rendered.push(format!("  {}", cap_line(line)));
    }
    Some(rendered.join("\n"))
}

/// Similarity of a single line pair for [`closest_match_diff`]: 2 for an
/// exact trimmed match, 1 when one trimmed line contains the other (and the
/// shorter side is long enough to be meaningful), 0 otherwise. Blank pairs
/// score 0 so the window never anchors on empty lines.
fn line_similarity(expected: &str, found: &str) -> usize {
    let expected = expected.trim();
    let found = found.trim();
    if expected.is_empty() || found.is_empty() {
        return 0;
    }
    if expected == found {
        return 2;
    }
    let shorter = expected.len().min(found.len());
    if shorter >= 3 && (expected.contains(found) || found.contains(expected)) {
        return 1;
    }
    0
}

fn cap_line(line: &str) -> String {
    if line.chars().count() <= DIFF_HINT_MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut capped: String = line.chars().take(DIFF_HINT_MAX_LINE_CHARS).collect();
    capped.push('…');
    capped
}

fn push_capped_side(rendered: &mut Vec<String>, prefix: char, lines: &[&str]) {
    for line in lines.iter().take(DIFF_HINT_MAX_LINES_PER_SIDE) {
        rendered.push(format!("{prefix} {}", cap_line(line)));
    }
    if lines.len() > DIFF_HINT_MAX_LINES_PER_SIDE {
        rendered.push(format!(
            "{prefix} … ({} more lines)",
            lines.len() - DIFF_HINT_MAX_LINES_PER_SIDE
        ));
    }
}

fn make_patch(original: &str, updated: &str) -> Vec<StructuredPatchHunk> {
    let mut lines = Vec::new();
    for line in original.lines() {
        lines.push(format!("-{line}"));
    }
    for line in updated.lines() {
        lines.push(format!("+{line}"));
    }

    vec![StructuredPatchHunk {
        old_start: 1,
        old_lines: original.lines().count(),
        new_start: 1,
        new_lines: updated.lines().count(),
        lines,
    }]
}

fn normalize_path(path: &str) -> io::Result<PathBuf> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };
    candidate.canonicalize()
}

fn normalize_path_allow_missing(path: &str) -> io::Result<PathBuf> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };

    if let Ok(canonical) = candidate.canonicalize() {
        return Ok(canonical);
    }

    if let Some(parent) = candidate.parent() {
        let canonical_parent = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        if let Some(name) = candidate.file_name() {
            return Ok(canonical_parent.join(name));
        }
    }

    Ok(candidate)
}

/// Read a file with workspace boundary enforcement.
#[allow(dead_code)]
pub fn read_file_in_workspace(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    workspace_root: &Path,
) -> io::Result<ReadFileOutput> {
    let absolute_path = normalize_path(path)?;
    let canonical_root = canonicalize_workspace_root(workspace_root);
    validate_workspace_boundary(&absolute_path, &canonical_root)?;
    read_file(path, offset, limit)
}

/// Write a file with workspace boundary enforcement.
#[allow(dead_code)]
pub fn write_file_in_workspace(
    path: &str,
    content: &str,
    workspace_root: &Path,
) -> io::Result<WriteFileOutput> {
    let absolute_path = normalize_path_allow_missing(path)?;
    let canonical_root = canonicalize_workspace_root(workspace_root);
    validate_workspace_boundary(&absolute_path, &canonical_root)?;
    write_file(path, content)
}

/// Edit a file with workspace boundary enforcement.
#[allow(dead_code)]
pub fn edit_file_in_workspace(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    workspace_root: &Path,
) -> io::Result<EditFileOutput> {
    let absolute_path = normalize_path(path)?;
    let canonical_root = canonicalize_workspace_root(workspace_root);
    validate_workspace_boundary(&absolute_path, &canonical_root)?;
    edit_file(path, old_string, new_string, replace_all)
}

/// Expand a glob pattern with workspace boundary enforcement.
#[allow(dead_code)]
pub fn glob_search_in_workspace(
    pattern: &str,
    path: Option<&str>,
    workspace_root: &Path,
) -> io::Result<GlobSearchOutput> {
    glob_search_impl(pattern, path, Some(workspace_root))
}

/// Search file contents with workspace boundary enforcement.
#[allow(dead_code)]
pub fn grep_search_in_workspace(
    input: &GrepSearchInput,
    workspace_root: &Path,
) -> io::Result<GrepSearchOutput> {
    grep_search_impl(input, Some(workspace_root))
}

/// Check whether a path is a symlink that resolves outside the workspace.
#[allow(dead_code)]
pub fn is_symlink_escape(path: &Path, workspace_root: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_symlink() {
        return Ok(false);
    }
    let resolved = path.canonicalize()?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    Ok(!resolved.starts_with(&canonical_root))
}

/// Expand shell-style brace groups in a glob pattern.
///
/// Handles one level of braces: `foo.{a,b,c}` → `["foo.a", "foo.b", "foo.c"]`.
/// Nested braces are not expanded (uncommon in practice).
/// Patterns without braces pass through unchanged.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(close) = pattern[open..].find('}').map(|i| open + i) else {
        // Unmatched brace — treat as literal.
        return vec![pattern.to_owned()];
    };
    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let alternatives = &pattern[open + 1..close];
    alternatives
        .split(',')
        .flat_map(|alt| expand_braces(&format!("{prefix}{alt}{suffix}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        closest_match_diff, component_contains_glob, derive_glob_walk_root, edit_file,
        expand_braces, external_modification_warning, glob_search, grep_search, is_symlink_escape,
        parse_read_file_max_bytes, read_file, read_file_in_workspace, read_file_with_max_bytes,
        record_file_mtime_as, write_file, write_file_in_workspace, GrepSearchInput, MAX_WRITE_SIZE,
        READ_FILE_MAX_BYTES_DEFAULT, READ_FILE_MAX_BYTES_MAX, READ_FILE_MAX_BYTES_MIN,
    };

    fn temp_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!("clawd-native-{name}-{unique}"))
    }

    #[test]
    fn parse_read_file_max_bytes_defaults_when_missing_or_invalid() {
        assert_eq!(parse_read_file_max_bytes(None), READ_FILE_MAX_BYTES_DEFAULT);
        assert_eq!(
            parse_read_file_max_bytes(Some("")),
            READ_FILE_MAX_BYTES_DEFAULT
        );
        assert_eq!(
            parse_read_file_max_bytes(Some("  ")),
            READ_FILE_MAX_BYTES_DEFAULT
        );
        assert_eq!(
            parse_read_file_max_bytes(Some("lots")),
            READ_FILE_MAX_BYTES_DEFAULT
        );
        assert_eq!(
            parse_read_file_max_bytes(Some("-1")),
            READ_FILE_MAX_BYTES_DEFAULT
        );
    }

    #[test]
    fn parse_read_file_max_bytes_accepts_and_clamps_values() {
        assert_eq!(parse_read_file_max_bytes(Some("1000000")), 1_000_000);
        assert_eq!(parse_read_file_max_bytes(Some(" 5000000 ")), 5_000_000);
        assert_eq!(
            parse_read_file_max_bytes(Some("1")),
            READ_FILE_MAX_BYTES_MIN
        );
        assert_eq!(
            parse_read_file_max_bytes(Some("999999999999")),
            READ_FILE_MAX_BYTES_MAX
        );
    }

    #[test]
    fn oversized_read_without_window_reports_size_and_suggests_offset_limit() {
        let path = temp_path("oversize-read.txt");
        std::fs::write(&path, "line one\nline two\nline three\n").expect("seed file");
        // Inject a tiny limit so the fixture stays small.
        let result = read_file_with_max_bytes(path.to_string_lossy().as_ref(), None, None, 10);
        let error = result.expect_err("oversized whole-file read must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let message = error.to_string();
        assert!(
            message.contains("29 bytes"),
            "actual size in error: {message}"
        );
        assert!(
            message.contains("limit 10 bytes"),
            "limit in error: {message}"
        );
        assert!(
            message.contains("offset/limit"),
            "suggestion in error: {message}"
        );
        assert!(
            message.contains("CLAW_READ_FILE_MAX_BYTES"),
            "env knob in error: {message}"
        );
    }

    #[test]
    fn oversized_read_with_window_streams_the_requested_lines() {
        let path = temp_path("oversize-window.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").expect("seed file");
        let output =
            read_file_with_max_bytes(path.to_string_lossy().as_ref(), Some(1), Some(2), 10)
                .expect("windowed read of oversized file must stream");
        assert_eq!(output.file.content, "beta\ngamma");
        assert_eq!(output.file.num_lines, 2);
        assert_eq!(output.file.start_line, 2);
        assert_eq!(output.file.total_lines, 4);
    }

    #[test]
    fn oversized_read_window_clamps_offset_past_end() {
        let path = temp_path("oversize-window-past-end.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("seed file");
        let output =
            read_file_with_max_bytes(path.to_string_lossy().as_ref(), Some(10), Some(2), 5)
                .expect("window past end yields empty content");
        assert_eq!(output.file.content, "");
        assert_eq!(output.file.num_lines, 0);
        assert_eq!(output.file.total_lines, 2);
    }

    #[test]
    fn external_modification_warning_is_none_without_history_or_change() {
        let now = SystemTime::now();
        assert_eq!(external_modification_warning(None, Some(now)), None);
        assert_eq!(external_modification_warning(Some(now), None), None);
        assert_eq!(external_modification_warning(Some(now), Some(now)), None);
    }

    #[test]
    fn external_modification_warning_fires_when_mtimes_differ() {
        let warning = external_modification_warning(Some(UNIX_EPOCH), Some(SystemTime::now()))
            .expect("differing mtimes must warn");
        assert!(warning.contains("changed on disk"));
    }

    #[test]
    fn edit_after_read_carries_no_external_modification_warning() {
        let path = temp_path("guard-clean-edit.txt");
        write_file(path.to_string_lossy().as_ref(), "alpha beta gamma").expect("seed");
        read_file(path.to_string_lossy().as_ref(), None, None).expect("read records mtime");
        let output = edit_file(path.to_string_lossy().as_ref(), "beta", "BETA", false)
            .expect("edit succeeds");
        assert_eq!(output.external_modification_warning, None);
        assert!(!output.user_modified);
    }

    #[test]
    fn edit_warns_when_file_changed_on_disk_since_last_read() {
        let path = temp_path("guard-external-change.txt");
        write_file(path.to_string_lossy().as_ref(), "alpha beta gamma").expect("seed");
        // Simulate "read long ago, file rewritten since": force the recorded
        // mtime to a value that cannot match the current on-disk mtime.
        let absolute = std::fs::canonicalize(&path).expect("canonicalize");
        record_file_mtime_as(&absolute, UNIX_EPOCH);
        let output = edit_file(path.to_string_lossy().as_ref(), "beta", "BETA", false)
            .expect("edit still succeeds despite the warning");
        let warning = output
            .external_modification_warning
            .expect("edit must warn about the external change");
        assert!(warning.contains("changed on disk"));
        assert!(output.user_modified);
        // The edit itself still landed.
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "alpha BETA gamma"
        );

        // The registry was refreshed by the edit: a follow-up edit is clean.
        let second = edit_file(path.to_string_lossy().as_ref(), "BETA", "beta2", false)
            .expect("second edit succeeds");
        assert_eq!(second.external_modification_warning, None);
    }

    #[test]
    fn reads_and_writes_files() {
        let path = temp_path("read-write.txt");
        let write_output = write_file(path.to_string_lossy().as_ref(), "one\ntwo\nthree")
            .expect("write should succeed");
        assert_eq!(write_output.kind, "create");

        let read_output = read_file(path.to_string_lossy().as_ref(), Some(1), Some(1))
            .expect("read should succeed");
        assert_eq!(read_output.file.content, "two");
    }

    #[test]
    fn edits_file_contents() {
        let path = temp_path("edit.txt");
        write_file(path.to_string_lossy().as_ref(), "alpha beta alpha")
            .expect("initial write should succeed");
        let output = edit_file(path.to_string_lossy().as_ref(), "alpha", "omega", true)
            .expect("edit should succeed");
        assert!(output.replace_all);
    }

    #[test]
    fn edit_rejects_empty_old_string() {
        let path = temp_path("edit-empty.txt");
        write_file(path.to_string_lossy().as_ref(), "abc").expect("write");
        // `"abc".contains("")` is true; without the guard, replace_all would
        // splice new_string between every character.
        let error = edit_file(path.to_string_lossy().as_ref(), "", "X", true)
            .expect_err("empty old_string must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "abc",
            "file must be untouched"
        );
    }

    #[test]
    fn edit_not_found_hints_at_moved_block() {
        let path = temp_path("edit-hint.txt");
        write_file(
            path.to_string_lossy().as_ref(),
            "fn main() {\n    println!(\"hola\");\n}\n",
        )
        .expect("write");
        // The block anchors but its body differs: the error should carry a
        // mini-diff of the closest region (expected `-`, found `+`).
        let error = edit_file(
            path.to_string_lossy().as_ref(),
            "fn main() {\n    println!(\"adios\");\n}",
            "X",
            false,
        )
        .expect_err("stale block must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        let message = error.to_string();
        assert!(message.contains("closest match at line 1"), "{message}");
        assert!(message.contains("-     println!(\"adios\");"), "{message}");
        assert!(message.contains("+     println!(\"hola\");"), "{message}");
        // No anchor at all: plain not-found, no hint.
        let error = edit_file(path.to_string_lossy().as_ref(), "no existe", "X", false)
            .expect_err("missing text must be rejected");
        assert_eq!(
            error.to_string(),
            "old_string not found in file",
            "no candidate region must yield the plain error"
        );
    }

    #[test]
    fn closest_match_diff_renders_partial_match_with_context() {
        let file = "use std::io;\n\nfn helper() {}\n\nfn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n";
        let old = "fn main() {\n    let x = 2;\n    println!(\"{x}\");\n}";
        let diff = closest_match_diff(file, old).expect("partial match should yield a hint");
        assert!(diff.contains("closest match at line 5"), "{diff}");
        // Expected lines carry `-`, found lines carry `+`.
        assert!(diff.contains("-     let x = 2;"), "{diff}");
        assert!(diff.contains("+     let x = 1;"), "{diff}");
        // Two context lines surround the region.
        assert!(diff.contains("  fn helper() {}"), "{diff}");
    }

    #[test]
    fn closest_match_diff_returns_none_without_candidate() {
        let file = "alpha\nbeta\ngamma\n";
        assert_eq!(closest_match_diff(file, "totally unrelated content"), None);
        assert_eq!(closest_match_diff("", "anything"), None);
        assert_eq!(closest_match_diff(file, ""), None);
    }

    #[test]
    fn closest_match_diff_handles_file_shorter_than_block() {
        let file = "only line\n";
        let old = "only line\nsecond line\nthird line";
        let diff = closest_match_diff(file, old).expect("clamped window should still match");
        assert!(diff.contains("closest match at line 1"), "{diff}");
        assert!(diff.contains("- only line"), "{diff}");
        assert!(diff.contains("- second line"), "{diff}");
        assert!(diff.contains("+ only line"), "{diff}");
    }

    #[test]
    fn closest_match_diff_caps_oversized_blocks_and_lines() {
        let long_line = "x".repeat(500);
        let file_block: String = (0..30).map(|i| format!("shared line {i}\n")).collect();
        let file = format!("{file_block}{long_line}\n");
        let expected: String = (0..30)
            .map(|i| format!("shared line {i} CHANGED\n"))
            .collect();
        let diff = closest_match_diff(&file, &expected).expect("large block should still hint");
        assert!(diff.contains("more lines)"), "{diff}");
        let diff_lines = diff.lines().count();
        assert!(
            diff_lines <= 20,
            "hint too large ({diff_lines} lines): {diff}"
        );
        for line in diff.lines() {
            assert!(line.chars().count() <= 130, "uncapped line: {line}");
        }
    }

    #[test]
    fn edit_requires_unique_match_without_replace_all() {
        let path = temp_path("edit-unique.txt");
        write_file(path.to_string_lossy().as_ref(), "alpha beta alpha").expect("write");
        let error = edit_file(path.to_string_lossy().as_ref(), "alpha", "omega", false)
            .expect_err("ambiguous old_string must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("2 times"));
        // A unique anchor still works without replace_all.
        let output = edit_file(path.to_string_lossy().as_ref(), "beta", "gamma", false)
            .expect("unique edit succeeds");
        assert!(!output.replace_all);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "alpha gamma alpha"
        );
    }

    #[test]
    fn write_over_non_utf8_file_reports_update() {
        let path = temp_path("overwrite-binary.txt");
        std::fs::write(&path, [0xFF, 0xFE, 0x00]).expect("seed binary");
        let output =
            write_file(path.to_string_lossy().as_ref(), "clean text").expect("overwrite succeeds");
        assert_eq!(output.kind, "update", "existing file is an update");
    }

    #[test]
    fn rejects_binary_files() {
        let path = temp_path("binary-test.bin");
        std::fs::write(&path, b"\x00\x01\x02\x03binary content").expect("write should succeed");
        let result = read_file(path.to_string_lossy().as_ref(), None, None);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("binary"));
    }

    #[test]
    fn rejects_oversized_writes() {
        let path = temp_path("oversize-write.txt");
        let huge = "x".repeat(MAX_WRITE_SIZE + 1);
        let result = write_file(path.to_string_lossy().as_ref(), &huge);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("too large"));
    }

    #[test]
    fn enforces_workspace_boundary() {
        let workspace = temp_path("workspace-boundary");
        std::fs::create_dir_all(&workspace).expect("workspace dir should be created");
        let inside = workspace.join("inside.txt");
        write_file(inside.to_string_lossy().as_ref(), "safe content")
            .expect("write inside workspace should succeed");

        // Reading inside workspace should succeed
        let result =
            read_file_in_workspace(inside.to_string_lossy().as_ref(), None, None, &workspace);
        assert!(result.is_ok());

        // Reading outside workspace should fail
        let outside = temp_path("outside-boundary.txt");
        write_file(outside.to_string_lossy().as_ref(), "unsafe content")
            .expect("write outside should succeed");
        let result =
            read_file_in_workspace(outside.to_string_lossy().as_ref(), None, None, &workspace);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("escapes workspace"));
    }

    #[test]
    fn detects_symlink_escape() {
        let workspace = temp_path("symlink-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir should be created");
        let outside = temp_path("symlink-target.txt");
        std::fs::write(&outside, "target content").expect("target should write");

        let link_path = workspace.join("escape-link.txt");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link_path).expect("symlink should create");
            assert!(is_symlink_escape(&link_path, &workspace).expect("check should succeed"));
        }

        // Non-symlink file should not be an escape
        let normal = workspace.join("normal.txt");
        std::fs::write(&normal, "normal content").expect("normal file should write");
        assert!(!is_symlink_escape(&normal, &workspace).expect("check should succeed"));
    }

    #[test]
    #[cfg(unix)]
    fn workspace_read_rejects_symlink_escape_regression_3007_class() {
        let workspace = temp_path("workspace-read-symlink-escape");
        let outside = temp_path("workspace-read-symlink-target");
        std::fs::create_dir_all(&workspace).expect("workspace dir should be created");
        std::fs::create_dir_all(&outside).expect("outside dir should be created");
        let outside_file = outside.join("secret.txt");
        std::fs::write(&outside_file, "outside secret").expect("outside file should write");

        let link_path = workspace.join("linked-secret.txt");
        std::os::unix::fs::symlink(&outside_file, &link_path).expect("symlink should create");

        let result =
            read_file_in_workspace(link_path.to_string_lossy().as_ref(), None, None, &workspace);

        assert!(result.is_err(), "symlink escape must be rejected");
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            error.to_string().contains("escapes workspace"),
            "error should explain workspace escape: {error}"
        );

        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    #[cfg(unix)]
    fn workspace_write_rejects_parent_symlink_escape_regression_3007_class() {
        let workspace = temp_path("workspace-write-symlink-escape");
        let outside = temp_path("workspace-write-symlink-target");
        std::fs::create_dir_all(&workspace).expect("workspace dir should be created");
        std::fs::create_dir_all(&outside).expect("outside dir should be created");

        let link_dir = workspace.join("linked-outside");
        std::os::unix::fs::symlink(&outside, &link_dir).expect("symlink dir should create");
        let escaped_child = link_dir.join("created.txt");

        let result = write_file_in_workspace(
            escaped_child.to_string_lossy().as_ref(),
            "must not escape",
            &workspace,
        );

        assert!(result.is_err(), "parent symlink escape must be rejected");
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            error.to_string().contains("escapes workspace"),
            "error should explain workspace escape: {error}"
        );
        assert!(
            !outside.join("created.txt").exists(),
            "write should not create through an escaping symlink"
        );

        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn globs_and_greps_directory() {
        let dir = temp_path("search-dir");
        std::fs::create_dir_all(&dir).expect("directory should be created");
        let file = dir.join("demo.rs");
        write_file(
            file.to_string_lossy().as_ref(),
            "fn main() {\n println!(\"hello\");\n}\n",
        )
        .expect("file write should succeed");

        let globbed = glob_search("**/*.rs", Some(dir.to_string_lossy().as_ref()))
            .expect("glob should succeed");
        assert_eq!(globbed.num_files, 1);

        let grep_output = grep_search(&GrepSearchInput {
            pattern: String::from("hello"),
            path: Some(dir.to_string_lossy().into_owned()),
            glob: Some(String::from("**/*.rs")),
            output_mode: Some(String::from("content")),
            before: None,
            after: None,
            context_short: None,
            context: None,
            line_numbers: Some(true),
            case_insensitive: Some(false),
            file_type: None,
            head_limit: Some(10),
            offset: Some(0),
            multiline: Some(false),
        })
        .expect("grep should succeed");
        assert!(grep_output.content.unwrap_or_default().contains("hello"));
    }

    #[test]
    fn expand_braces_no_braces() {
        assert_eq!(expand_braces("*.rs"), vec!["*.rs"]);
    }

    #[test]
    fn expand_braces_single_group() {
        let mut result = expand_braces("Assets/**/*.{cs,uxml,uss}");
        result.sort();
        assert_eq!(
            result,
            vec!["Assets/**/*.cs", "Assets/**/*.uss", "Assets/**/*.uxml",]
        );
    }

    #[test]
    fn expand_braces_nested() {
        let mut result = expand_braces("src/{a,b}.{rs,toml}");
        result.sort();
        assert_eq!(
            result,
            vec!["src/a.rs", "src/a.toml", "src/b.rs", "src/b.toml"]
        );
    }

    #[test]
    fn expand_braces_unmatched() {
        assert_eq!(expand_braces("foo.{bar"), vec!["foo.{bar"]);
    }

    #[test]
    fn glob_search_with_braces_finds_files() {
        let dir = temp_path("glob-braces");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("b.toml"), "[package]").unwrap();
        std::fs::write(dir.join("c.txt"), "hello").unwrap();

        let result =
            glob_search("*.{rs,toml}", Some(dir.to_str().unwrap())).expect("glob should succeed");
        assert_eq!(
            result.num_files, 2,
            "should match .rs and .toml but not .txt"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_search_skips_common_heavy_directories() {
        let dir = temp_path("glob-ignored-dirs");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(dir.join(".build/checkouts/pkg")).unwrap();
        std::fs::create_dir_all(dir.join("target/debug/deps")).unwrap();

        std::fs::write(dir.join("src/AGENTS.md"), "src").unwrap();
        std::fs::write(dir.join("docs/AGENTS.md"), "docs").unwrap();
        std::fs::write(dir.join("node_modules/pkg/AGENTS.md"), "node_modules").unwrap();
        std::fs::write(dir.join(".build/checkouts/pkg/AGENTS.md"), ".build").unwrap();
        std::fs::write(dir.join("target/debug/deps/AGENTS.md"), "target").unwrap();

        let result =
            glob_search("**/AGENTS.md", Some(dir.to_str().unwrap())).expect("glob should succeed");

        assert_eq!(result.num_files, 2, "ignored dirs should be pruned");
        assert!(result
            .filenames
            .iter()
            .any(|path| path.ends_with("src/AGENTS.md")));
        assert!(result
            .filenames
            .iter()
            .any(|path| path.ends_with("docs/AGENTS.md")));
        assert!(!result
            .filenames
            .iter()
            .any(|path| path.contains("node_modules")
                || path.contains(".build")
                || path.contains("/target/")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn derive_glob_walk_root_stops_at_first_glob_component() {
        let root = derive_glob_walk_root("/tmp/demo/**/AGENTS.md");
        assert_eq!(root, PathBuf::from("/tmp/demo"));
        assert!(component_contains_glob("**"));
        assert!(component_contains_glob("*.rs"));
        assert!(!component_contains_glob("src"));
    }
}

#[cfg(test)]
mod read_limit_property_tests {
    use super::parse_read_file_max_bytes;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// `CLAW_READ_FILE_MAX_BYTES` parsing never leaves its
        /// 64 KiB..=50 MiB clamp, whatever the raw value.
        #[test]
        fn parse_read_file_max_bytes_stays_within_clamp(
            raw in proptest::option::of("\\PC{0,24}"),
        ) {
            let value = parse_read_file_max_bytes(raw.as_deref());
            prop_assert!((65_536..=52_428_800).contains(&value));
        }

        /// Numeric strings — the parseable subset — are clamped, not passed
        /// through.
        #[test]
        fn parse_read_file_max_bytes_clamps_all_numeric_inputs(value in any::<u64>()) {
            let parsed = parse_read_file_max_bytes(Some(&value.to_string()));
            prop_assert!((65_536..=52_428_800).contains(&parsed));
        }
    }
}
