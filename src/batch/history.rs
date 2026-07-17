use super::{Mode, Op, execute};
use crate::tags;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};

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
        body.push_str(&history_line(id, op));
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(body.as_bytes())?;
    Ok(id)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    fn nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| Some(nibble(p[0])? << 4 | nibble(p[1])?))
        .collect()
}

// v2 stores native path bytes as ASCII hex, so tabs/newlines and non-UTF-8
// Unix names cannot corrupt the line-oriented history file.
#[cfg(unix)]
fn encode_path(path: &Path) -> String {
    hex(path.as_os_str().as_bytes())
}

#[cfg(windows)]
fn encode_path(path: &Path) -> String {
    let bytes: Vec<u8> = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    hex(&bytes)
}

#[cfg(unix)]
fn decode_path(s: &str) -> Option<PathBuf> {
    Some(OsString::from_vec(unhex(s)?).into())
}

#[cfg(windows)]
fn decode_path(s: &str) -> Option<PathBuf> {
    let bytes = unhex(s)?;
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let wide: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    Some(OsString::from_wide(&wide).into())
}

fn history_line(id: u64, op: &Op) -> String {
    format!(
        "v2\t{id}\t{}\t{}\n",
        encode_path(&op.from),
        encode_path(&op.to)
    )
}

fn parse_history_line(line: &str) -> Option<(u64, Op)> {
    if let Some(line) = line.strip_prefix("v2\t") {
        let mut parts = line.splitn(3, '\t');
        let id = parts.next()?.parse().ok()?;
        let from = decode_path(parts.next()?)?;
        let to = decode_path(parts.next()?)?;
        return Some((id, Op { from, to }));
    }

    // v1 compatibility: old histories used raw display paths in three fields.
    let mut parts = line.splitn(3, '\t');
    let id = parts.next()?.parse().ok()?;
    let from = PathBuf::from(parts.next()?);
    let to = PathBuf::from(parts.next()?);
    Some((id, Op { from, to }))
}

fn read_history(path: &Path) -> Vec<(u64, Op)> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(parse_history_line)
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
        .map(|(i, o)| history_line(*i, o))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_codec_roundtrips_delimiter_paths_and_reads_v1() {
        let op = Op {
            from: PathBuf::from("from\tname\n.txt"),
            to: PathBuf::from("to\nname\t.txt"),
        };
        assert_eq!(
            parse_history_line(history_line(42, &op).trim_end()),
            Some((42, op))
        );
        assert_eq!(
            parse_history_line("7\told name\tnew name"),
            Some((
                7,
                Op {
                    from: "old name".into(),
                    to: "new name".into(),
                }
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn history_codec_roundtrips_non_utf8_unix_paths() {
        let path = PathBuf::from(OsString::from_vec(vec![b'a', 0xff, b'\t', b'b']));
        assert_eq!(decode_path(&encode_path(&path)), Some(path));
    }
}
