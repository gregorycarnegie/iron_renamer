// Rule presets: the rule stack plus numbering/output settings, saved as a
// small escaped-TSV text file. The GUI and the CLI (--preset) read the same
// format, so a preset built visually can drive scripted runs.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct Preset {
    pub settings: HashMap<String, String>, // start, pad, mode, dest, collide, collide_pattern
    pub rules: Vec<(String, String, String, String)>, // kind, mods, a, b
}

/// Default folder for presets; dialogs open here so saved presets act as
/// the "recent presets" list.
pub fn dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("iron_renamer"))
        .or_else(|| std::env::var_os("HOME").map(|d| PathBuf::from(d).join(".iron_renamer")))
        .unwrap_or_else(|| PathBuf::from(".iron_renamer"))
        .join("presets")
}

/// Resolve a CLI preset argument: a real path wins, otherwise look in the
/// preset folder, with or without the .preset extension.
pub fn resolve(arg: &str) -> PathBuf {
    let direct = PathBuf::from(arg);
    if direct.is_file() {
        return direct;
    }
    let named = dir().join(arg);
    if named.is_file() {
        return named;
    }
    dir().join(format!("{arg}.preset"))
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "")
}

fn unesc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

pub fn save(path: &Path, p: &Preset) -> io::Result<()> {
    let mut out = String::from("#iron_renamer preset v1\n");
    let mut keys: Vec<&String> = p.settings.keys().collect();
    keys.sort();
    for k in keys {
        out.push_str(&format!("set\t{k}\t{}\n", esc(&p.settings[k])));
    }
    for (kind, mods, a, b) in &p.rules {
        out.push_str(&format!("rule\t{kind}\t{mods}\t{}\t{}\n", esc(a), esc(b)));
    }
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    fs::write(path, out)
}

pub fn load(path: &Path) -> Result<Preset, String> {
    let body =
        fs::read_to_string(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    let mut p = Preset { settings: HashMap::new(), rules: Vec::new() };
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        match parts.as_slice() {
            ["set", k, v] => {
                p.settings.insert((*k).to_string(), unesc(v));
            }
            ["rule", kind, mods, a, b] => {
                p.rules.push(((*kind).to_string(), (*mods).to_string(), unesc(a), unesc(b)));
            }
            _ => return Err(format!("bad preset line: {line}")),
        }
    }
    Ok(p)
}

/// Minimal quote-aware CSV line split (fields may be "quoted, with commas"
/// and doubled quotes).
pub fn csv_split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_q && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_q = !in_q,
            ',' if !in_q => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_roundtrip_with_awkward_text() {
        let d = std::env::temp_dir().join(format!("iron_preset_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let path = d.join("test.preset");
        let p = Preset {
            settings: [("start".to_string(), "5".to_string()), ("dest".to_string(), "a\\b".to_string())]
                .into(),
            rules: vec![
                ("replace".into(), "ci:first".into(), "tab\there".into(), "line\nbreak".into()),
                ("names".into(), String::new(), "one\ntwo\nthree".into(), String::new()),
            ],
        };
        save(&path, &p).unwrap();
        let q = load(&path).unwrap();
        assert_eq!(q.settings.get("start").map(String::as_str), Some("5"));
        assert_eq!(q.settings.get("dest").map(String::as_str), Some("a\\b"));
        assert_eq!(q.rules, p.rules);
    }

    #[test]
    fn csv_split_handles_quotes() {
        assert_eq!(csv_split("a,b,c"), vec!["a", "b", "c"]);
        assert_eq!(csv_split(r#""a,b",c"#), vec!["a,b", "c"]);
        assert_eq!(csv_split(r#""say ""hi""",x"#), vec![r#"say "hi""#, "x"]);
        assert_eq!(csv_split("one"), vec!["one"]);
        assert_eq!(csv_field("plain.txt"), "plain.txt");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("q\"t"), "\"q\"\"t\"");
        assert_eq!(json_str("a\"b\\c"), r#""a\"b\\c""#);
    }
}
