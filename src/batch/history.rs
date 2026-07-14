use super::{Mode, Op, execute};
use crate::tags;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

// ───────────────────────── history

fn history_path() -> PathBuf {
    crate::presets::data_dir().join("history.tsv")
}

/// Append an applied batch (in execution order) to the history file.
pub fn record(ops: &[Op]) -> io::Result<u64> {
    record_at(&history_path(), ops)
}

pub(crate) fn record_at(path: &Path, ops: &[Op]) -> io::Result<u64> {
    if ops.is_empty() {
        return Ok(0);
    }
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut body = String::new();
    for op in ops {
        body.push_str(&format!(
            "{id}\t{}\t{}\n",
            op.from.display(),
            op.to.display()
        ));
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(body.as_bytes())?;
    Ok(id)
}

fn read_history(path: &Path) -> Vec<(u64, Op)> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\t');
            let id = parts.next()?.parse().ok()?;
            let from = PathBuf::from(parts.next()?);
            let to = PathBuf::from(parts.next()?);
            Some((id, Op { from, to }))
        })
        .collect()
}

/// Past batches as (id, date, item count), newest first.
pub fn history() -> Vec<(u64, String, usize)> {
    history_at(&history_path())
}

pub(super) fn history_at(path: &Path) -> Vec<(u64, String, usize)> {
    let mut out: Vec<(u64, String, usize)> = Vec::new();
    for (id, _) in read_history(path) {
        match out.iter_mut().find(|(i, ..)| *i == id) {
            Some((.., n)) => *n += 1,
            None => out.push((id, date_str(id), 1)),
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.0));
    out
}

/// Revert one batch (latest if `id` is None) through the same safe executor,
/// so undoing swaps, chains, and moves works too. Reverted entries are
/// removed from history; entries that failed to revert are kept for retry.
/// Returns the reverted ops (new path -> restored original path).
pub fn undo(id: Option<u64>) -> Result<(Vec<Op>, Vec<String>), String> {
    undo_at(&history_path(), id)
}

pub(crate) fn undo_at(path: &Path, id: Option<u64>) -> Result<(Vec<Op>, Vec<String>), String> {
    let all = read_history(path);
    let id = id
        .or_else(|| all.iter().map(|(i, _)| *i).max())
        .ok_or("no batch history")?;
    let batch: Vec<Op> = all
        .iter()
        .filter(|(i, _)| *i == id)
        .map(|(_, o)| o.clone())
        .collect();
    if batch.is_empty() {
        return Err(format!("no batch with id {id} (see 'history')"));
    }

    let inverse: Vec<Op> = batch
        .iter()
        .rev()
        .map(|o| Op {
            from: o.to.clone(),
            to: o.from.clone(),
        })
        .collect();
    // Move handles everything undo needs: directory creation and volumes.
    let res = execute(inverse, Mode::Move);

    // A failed inverse op's `to` is the original `from` of the recorded op.
    let still_applied: Vec<&PathBuf> = res.failed.iter().map(|(op, _)| &op.to).collect();
    let keep: String = all
        .iter()
        .filter(|(i, o)| *i != id || still_applied.contains(&&o.from))
        .map(|(i, o)| format!("{i}\t{}\t{}\n", o.from.display(), o.to.display()))
        .collect();
    let write_res = if keep.is_empty() {
        fs::remove_file(path).or(Ok(()))
    } else {
        fs::write(path, keep)
    };
    if let Err(e) = write_res {
        return Err(format!(
            "reverted {} but could not update history: {e}",
            res.renamed.len()
        ));
    }

    let errors = res
        .failed
        .iter()
        .map(|(op, e)| format!("{} -> {}: {e}", op.from.display(), op.to.display()))
        .collect();
    Ok((res.renamed, errors))
}

pub(super) fn date_str(id_millis: u64) -> String {
    let (y, m, d, h, mi, _) = tags::civil_utc((id_millis / 1000) as i64);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02} UTC")
}
