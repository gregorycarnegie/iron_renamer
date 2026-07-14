// Shared rename engine: rules, natural sort, globbing.

use std::fs;
use std::path::{Path, PathBuf};

pub enum Rule {
    Replace(String, String),
    Regex(regex::Regex, String),
    Case(CaseMode),
    Pattern(String),
}

#[derive(Clone, Copy)]
pub enum CaseMode {
    Lower,
    Upper,
    Title,
}

pub fn apply_rule(rule: &Rule, name: &str, num: usize, pad: usize) -> String {
    match rule {
        Rule::Replace(old, new) => name.replace(old, new),
        Rule::Regex(re, repl) => re.replace_all(name, repl.as_str()).into_owned(),
        Rule::Case(mode) => change_case(name, *mode),
        Rule::Pattern(pat) => {
            let (stem, ext) = match name.rsplit_once('.') {
                Some((s, e)) if !s.is_empty() => (s, e),
                _ => (name, ""),
            };
            pat.replace("<name>", stem)
                .replace("<ext>", ext)
                .replace("<num>", &format!("{num:0pad$}"))
        }
    }
}

fn change_case(s: &str, mode: CaseMode) -> String {
    match mode {
        CaseMode::Lower => s.to_lowercase(),
        CaseMode::Upper => s.to_uppercase(),
        CaseMode::Title => {
            let mut out = String::with_capacity(s.len());
            let mut at_word_start = true;
            for c in s.chars() {
                if c.is_alphanumeric() {
                    if at_word_start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    at_word_start = false;
                } else {
                    out.push(c);
                    at_word_start = true;
                }
            }
            out
        }
    }
}

// Split into (text, number) chunks so "img9" < "img10".
pub fn natural_key(s: &str) -> Vec<(String, u64)> {
    let mut key = Vec::new();
    let mut text = String::new();
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if !digits.is_empty() {
                key.push((text.to_lowercase(), digits.parse().unwrap_or(u64::MAX)));
                text = String::new();
                digits.clear();
            }
            text.push(c);
        }
    }
    key.push((text.to_lowercase(), digits.parse().unwrap_or(0)));
    key
}

// PowerShell doesn't expand globs, so handle * and ? ourselves.
// `dirs` switches matching from files to folders.
pub fn expand(arg: &str, dirs: bool) -> Vec<PathBuf> {
    if !arg.contains('*') && !arg.contains('?') {
        return vec![PathBuf::from(arg)];
    }
    let p = PathBuf::from(arg);
    let pat = name_of(&p);
    let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let kind_ok = if dirs { e.path().is_dir() } else { e.path().is_file() };
            if kind_ok && wild_match(&pat.to_lowercase(), &n.to_lowercase()) {
                out.push(if dir.as_os_str() == "." { PathBuf::from(n) } else { dir.join(n) });
            }
        }
    }
    if out.is_empty() {
        eprintln!("warning: '{arg}' matched nothing");
    }
    out
}

pub fn wild_match(pat: &str, s: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    go(&p, &t)
}

pub fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_work() {
        let r = |rule: &Rule, name: &str| apply_rule(rule, name, 7, 3);
        assert_eq!(r(&Rule::Replace(" ".into(), "_".into()), "a b.txt"), "a_b.txt");
        assert_eq!(
            r(&Rule::Regex(regex::Regex::new(r"(\d+)").unwrap(), "n$1".into()), "img42.jpg"),
            "imgn42.jpg"
        );
        assert_eq!(r(&Rule::Case(CaseMode::Title), "my file.txt"), "My File.Txt");
        assert_eq!(r(&Rule::Pattern("x_<num>.<ext>".into()), "old.jpg"), "x_007.jpg");
        assert_eq!(r(&Rule::Pattern("<name>!".into()), "noext"), "noext!");
    }

    #[test]
    fn natural_sort_and_glob() {
        let mut v = vec!["img10.jpg", "img9.jpg", "img1.jpg"];
        v.sort_by(|a, b| natural_key(a).cmp(&natural_key(b)));
        assert_eq!(v, vec!["img1.jpg", "img9.jpg", "img10.jpg"]);
        assert!(wild_match("*.jpg", "photo.jpg"));
        assert!(wild_match("img?.png", "img1.png"));
        assert!(!wild_match("*.jpg", "photo.png"));
    }
}
