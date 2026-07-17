use crate::{engine::name_of, tags};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

// ───────────────────────── timestamps

pub enum TouchValue {
    Absolute(i64), // epoch secs, UTC
    Delta(i64),    // shift each selected field by this many seconds
    FromName,      // yyyy?MM?dd[?HH?mm[?ss]] extracted from the file name
    FromParent,    // ... extracted from the parent folder name
    FromExif,      // DateTimeOriginal / CreateDate from file metadata
}

pub struct TouchSpec {
    pub created: bool,
    pub modified: bool,
    pub accessed: bool,
    pub value: TouchValue,
}

/// Parse "WHICH=VALUE": WHICH is a comma list of created|modified|accessed
/// or all; VALUE is an absolute date ("2024-05-01 10:30"), a delta
/// ("+3d", "-2h"), or name | parent | exif.
pub fn parse_touch(s: &str) -> Result<TouchSpec, String> {
    let (which, value) = s.split_once('=').ok_or("timestamp spec is WHICH=VALUE")?;
    let mut spec = TouchSpec {
        created: false,
        modified: false,
        accessed: false,
        value: TouchValue::Delta(0),
    };
    // Creation time is only settable on Windows and macOS; "all" quietly
    // skips it elsewhere, an explicit "created" fails at parse time.
    const CREATED_OK: bool = cfg!(any(windows, target_os = "macos"));
    for w in which.split(',').map(str::trim) {
        match w {
            "created" if !CREATED_OK => {
                return Err("creation time is not settable on this OS".into());
            }
            "created" => spec.created = true,
            "modified" => spec.modified = true,
            "accessed" => spec.accessed = true,
            "all" => (spec.created, spec.modified, spec.accessed) = (CREATED_OK, true, true),
            other => {
                return Err(format!(
                    "unknown timestamp field '{other}' (created|modified|accessed|all)"
                ));
            }
        }
    }
    let value = value.trim();
    spec.value = match value {
        "name" => TouchValue::FromName,
        "parent" => TouchValue::FromParent,
        "exif" => TouchValue::FromExif,
        v if v.starts_with('+') || v.starts_with('-') => {
            TouchValue::Delta(tags::parse_offset(v).ok_or_else(|| format!("bad time delta '{v}'"))?)
        }
        v => TouchValue::Absolute(
            tags::extract_datetime(v).ok_or_else(|| format!("no date found in '{v}'"))?,
        ),
    };
    Ok(spec)
}

fn system_time(secs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + std::time::Duration::from_secs(secs as u64)
    } else {
        UNIX_EPOCH
    }
}

/// Set the selected timestamps on every path (files only). Returns the
/// number touched and per-file error messages.
pub fn apply_touch(paths: &[PathBuf], spec: &TouchSpec) -> (usize, Vec<String>) {
    let mut done = 0;
    let mut errors = Vec::new();
    for p in paths {
        match touch_one(p, spec) {
            Ok(true) => done += 1,
            Ok(false) => {} // no date derivable for this file — skip silently
            Err(e) => errors.push(format!("{}: {e}", p.display())),
        }
    }
    (done, errors)
}

fn touch_one(p: &Path, spec: &TouchSpec) -> Result<bool, String> {
    let secs_of = |t: SystemTime| {
        t.duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    };
    let fixed: Option<SystemTime> = match &spec.value {
        TouchValue::Absolute(s) => Some(system_time(*s)),
        TouchValue::Delta(_) => None, // per field, below
        TouchValue::FromName => tags::extract_datetime(&name_of(p)).map(system_time),
        TouchValue::FromParent => {
            let parent = std::path::absolute(p)
                .ok()
                .and_then(|q| {
                    q.parent()
                        .and_then(|d| d.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            tags::extract_datetime(&parent).map(system_time)
        }
        TouchValue::FromExif => {
            let v = crate::meta::get(p, "datetimeoriginal")
                .filter(|v| !v.is_empty())
                .or_else(|| crate::meta::get(p, "createdate"))
                .ok_or("file unreadable")?;
            tags::extract_datetime(&v).map(system_time)
        }
    };
    if fixed.is_none() && !matches!(spec.value, TouchValue::Delta(_)) {
        return Ok(false);
    }

    let md = fs::metadata(p).map_err(|e| e.to_string())?;
    let field = |cur: io::Result<SystemTime>| -> Result<SystemTime, String> {
        match (&spec.value, fixed) {
            (TouchValue::Delta(d), _) => {
                let base = cur.map_err(|e| e.to_string())?;
                Ok(system_time(secs_of(base) + d))
            }
            (_, Some(t)) => Ok(t),
            _ => unreachable!(),
        }
    };

    let mut times = fs::FileTimes::new();
    if spec.modified {
        times = times.set_modified(field(md.modified())?);
    }
    if spec.accessed {
        times = times.set_accessed(field(md.accessed())?);
    }
    #[cfg(any(windows, target_os = "macos"))]
    if spec.created {
        #[cfg(target_os = "macos")]
        use std::os::macos::fs::FileTimesExt;
        #[cfg(windows)]
        use std::os::windows::fs::FileTimesExt;
        times = times.set_created(field(md.created())?);
    }
    let f = fs::File::options()
        .write(true)
        .open(p)
        .map_err(|e| e.to_string())?;
    f.set_times(times).map_err(|e| e.to_string())?;
    Ok(true)
}
