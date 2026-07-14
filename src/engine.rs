// Shared rename engine: rules, rule parsing, natural sort, globbing.
// Rules are built with `build_rule` (same syntax for CLI and GUI) and applied
// with `apply_entry`, which handles apply-to (name/ext/both) and conditions.

use crate::tags;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

// ───────────────────────── rule model

#[derive(Clone, Copy, PartialEq)]
pub enum Part {
    Both,
    Name,
    Ext,
}

pub enum Occurrence {
    All,
    First,
    Last,
    Nth(usize), // 1-based
}

#[derive(Clone, Copy)]
pub enum CaseMode {
    Lower,
    Upper,
    Title,  // Each Word
    First,  // sentence case
    Invert,
}

pub enum CaseScope {
    All,
    Pos { start: usize, len: usize }, // in chars
    Match(Regex),
}

pub enum InsertAt {
    Pos(usize),     // in chars; clamped to the end
    FromEnd(usize),
    Before(Regex),  // first match; no match = no change
    After(Regex),
}

pub enum RemoveWhat {
    Range { start: usize, len: usize }, // in chars
    Match(Regex),
    Chars(String),
    Digits,
    Upper,
    Lower,
    Diacritics, // é -> e, not removal
}

#[derive(Clone, Copy)]
pub enum TrimAt {
    Start,
    End,
    Both,
    All, // throughout
}

pub enum RenumMode {
    Delta(i64),
    Sequence { start: i64, step: i64 }, // start + step * list index
}

pub enum Rule {
    Replace { find: String, repl: String, ci: bool, occ: Occurrence },
    Regex(Regex, String),
    Case { mode: CaseMode, scope: CaseScope },
    Pattern(String),
    Insert { text: String, at: InsertAt },
    Remove(RemoveWhat),
    Trim { chars: String, at: TrimAt, invert: bool }, // empty chars = whitespace
    Renumber { nth: usize, mode: RenumMode, pad: usize }, // pad 0 = keep width
    MoveText { pat: Regex, to: InsertAt },
    Swap(String), // swap around first separator: "a - b" -> "b - a"
    ListNames(Vec<String>), // one explicit new name per list position
}

pub enum CondField {
    Original, // original file name
    Current,  // name after the rules so far
    Ext,
    Path,
}

pub enum CondOp {
    Has,
    Starts,
    Ends,
    Eq,
    Re(Regex),
}

pub struct Cond {
    pub field: CondField,
    pub op: CondOp,
    pub value: String,
    pub negate: bool,
}

pub struct RuleEntry {
    pub rule: Rule,
    pub part: Part,
    pub cond: Option<Cond>,
}

/// Per-file context a rule application runs in.
pub struct Ctx<'a> {
    pub index: usize,      // 0-based list position
    pub num: usize,        // counter (start + index)
    pub pad: usize,
    pub folder_num: usize, // 1-based position among list items in the same folder
    pub path: &'a Path,
    pub original: &'a str,
}

// ───────────────────────── application

pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, e),
        _ => (name, ""),
    }
}

fn join_ext(stem: &str, ext: &str) -> String {
    if ext.is_empty() { stem.to_string() } else { format!("{stem}.{ext}") }
}

pub fn apply_entry(e: &RuleEntry, name: &str, ctx: &Ctx) -> String {
    if let Some(c) = &e.cond
        && !cond_matches(c, name, ctx)
    {
        return name.to_string();
    }
    match e.part {
        Part::Both => apply_rule(&e.rule, name, name, ctx),
        Part::Name => {
            let (stem, ext) = split_ext(name);
            join_ext(&apply_rule(&e.rule, stem, name, ctx), ext)
        }
        Part::Ext => {
            let (stem, ext) = split_ext(name);
            join_ext(stem, &apply_rule(&e.rule, ext, name, ctx))
        }
    }
}

// `s` is the slice being edited (per apply-to); `full` is the whole current
// name, which tags resolve against.
fn apply_rule(rule: &Rule, s: &str, full: &str, ctx: &Ctx) -> String {
    match rule {
        Rule::Replace { find, repl, ci, occ } => {
            replace_occ(s, find, &tags::expand(repl, full, ctx), *ci, occ)
        }
        Rule::Regex(re, repl) => {
            re.replace_all(s, tags::expand(repl, full, ctx).as_str()).into_owned()
        }
        Rule::Case { mode, scope } => case_scoped(s, *mode, scope),
        Rule::Pattern(t) => tags::expand(t, full, ctx),
        Rule::Insert { text, at } => insert_at(s, &tags::expand(text, full, ctx), at),
        Rule::Remove(w) => remove(s, w),
        Rule::Trim { chars, at, invert } => trim(s, chars, *at, *invert),
        Rule::Renumber { nth, mode, pad } => renumber(s, *nth, mode, *pad, ctx),
        Rule::MoveText { pat, to } => match pat.find(s) {
            Some(m) => {
                let text = m.as_str().to_string();
                let rest = format!("{}{}", &s[..m.start()], &s[m.end()..]);
                insert_at(&rest, &text, to)
            }
            None => s.to_string(),
        },
        Rule::Swap(sep) => match s.split_once(sep.as_str()) {
            Some((a, b)) => format!("{b}{sep}{a}"),
            None => s.to_string(),
        },
        Rule::ListNames(names) => {
            names.get(ctx.index).cloned().unwrap_or_else(|| s.to_string())
        }
    }
}

fn cond_matches(c: &Cond, current: &str, ctx: &Ctx) -> bool {
    let field = match c.field {
        CondField::Original => ctx.original.to_string(),
        CondField::Current => current.to_string(),
        CondField::Ext => split_ext(current).1.to_string(),
        CondField::Path => ctx.path.to_string_lossy().into_owned(),
    };
    let hit = match &c.op {
        CondOp::Re(re) => re.is_match(&field),
        op => {
            // Literal ops are case-insensitive, like Windows file names.
            let (f, v) = (field.to_lowercase(), c.value.to_lowercase());
            match op {
                CondOp::Has => f.contains(&v),
                CondOp::Starts => f.starts_with(&v),
                CondOp::Ends => f.ends_with(&v),
                CondOp::Eq => f == v,
                CondOp::Re(_) => unreachable!(),
            }
        }
    };
    hit != c.negate
}

fn replace_occ(s: &str, find: &str, repl: &str, ci: bool, occ: &Occurrence) -> String {
    if find.is_empty() {
        return s.to_string();
    }
    let ranges: Vec<(usize, usize)> = if ci {
        // Regex handles Unicode case folding; lowercasing can shift byte offsets.
        let re = Regex::new(&format!("(?i){}", regex::escape(find))).unwrap();
        re.find_iter(s).map(|m| (m.start(), m.end())).collect()
    } else {
        s.match_indices(find).map(|(i, _)| (i, i + find.len())).collect()
    };
    let picked: Vec<(usize, usize)> = match occ {
        Occurrence::All => ranges,
        Occurrence::First => ranges.first().into_iter().copied().collect(),
        Occurrence::Last => ranges.last().into_iter().copied().collect(),
        Occurrence::Nth(n) => ranges.get(n.saturating_sub(1)).into_iter().copied().collect(),
    };
    let mut out = String::with_capacity(s.len());
    let mut prev = 0;
    for (a, b) in picked {
        out.push_str(&s[prev..a]);
        out.push_str(repl);
        prev = b;
    }
    out.push_str(&s[prev..]);
    out
}

pub(crate) fn change_case(s: &str, mode: CaseMode) -> String {
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
        CaseMode::First => {
            let mut out = String::with_capacity(s.len());
            let mut done = false;
            for c in s.chars() {
                if !done && c.is_alphanumeric() {
                    out.extend(c.to_uppercase());
                    done = true;
                } else {
                    out.extend(c.to_lowercase());
                }
            }
            out
        }
        CaseMode::Invert => {
            let mut out = String::with_capacity(s.len());
            for c in s.chars() {
                if c.is_uppercase() {
                    out.extend(c.to_lowercase());
                } else if c.is_lowercase() {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
            }
            out
        }
    }
}

fn case_scoped(s: &str, mode: CaseMode, scope: &CaseScope) -> String {
    match scope {
        CaseScope::All => change_case(s, mode),
        CaseScope::Pos { start, len } => {
            let chars: Vec<char> = s.chars().collect();
            if *start >= chars.len() {
                return s.to_string();
            }
            let end = (start + len).min(chars.len());
            let seg: String = chars[*start..end].iter().collect();
            let (head, tail): (String, String) =
                (chars[..*start].iter().collect(), chars[end..].iter().collect());
            format!("{head}{}{tail}", change_case(&seg, mode))
        }
        CaseScope::Match(re) => {
            let mut out = String::with_capacity(s.len());
            let mut prev = 0;
            for m in re.find_iter(s) {
                out.push_str(&s[prev..m.start()]);
                out.push_str(&change_case(m.as_str(), mode));
                prev = m.end();
            }
            out.push_str(&s[prev..]);
            out
        }
    }
}

fn insert_at(s: &str, text: &str, at: &InsertAt) -> String {
    let char_len = s.chars().count();
    let byte_of = |nchars: usize| {
        s.char_indices().nth(nchars).map(|(i, _)| i).unwrap_or(s.len())
    };
    let pos = match at {
        InsertAt::Pos(n) => byte_of((*n).min(char_len)),
        InsertAt::FromEnd(n) => byte_of(char_len.saturating_sub(*n)),
        InsertAt::Before(re) => match re.find(s) {
            Some(m) => m.start(),
            None => return s.to_string(),
        },
        InsertAt::After(re) => match re.find(s) {
            Some(m) => m.end(),
            None => return s.to_string(),
        },
    };
    format!("{}{text}{}", &s[..pos], &s[pos..])
}

fn remove(s: &str, w: &RemoveWhat) -> String {
    match w {
        RemoveWhat::Range { start, len } => {
            let chars: Vec<char> = s.chars().collect();
            if *start >= chars.len() {
                return s.to_string();
            }
            let end = (start + len).min(chars.len());
            chars[..*start].iter().chain(&chars[end..]).collect()
        }
        RemoveWhat::Match(re) => re.replace_all(s, "").into_owned(),
        RemoveWhat::Chars(list) => s.chars().filter(|c| !list.contains(*c)).collect(),
        RemoveWhat::Digits => s.chars().filter(|c| !c.is_ascii_digit()).collect(),
        RemoveWhat::Upper => s.chars().filter(|c| !c.is_uppercase()).collect(),
        RemoveWhat::Lower => s.chars().filter(|c| !c.is_lowercase()).collect(),
        RemoveWhat::Diacritics => s.chars().map(strip_diacritic).collect(),
    }
}

// ponytail: common Latin accents only; unicode-normalization crate if full
// coverage ever matters.
fn strip_diacritic(c: char) -> char {
    match c {
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'À'..='Å' | 'Ā' | 'Ă' | 'Ą' => 'A',
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'È'..='Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => 'E',
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'Ì'..='Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' => 'I',
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'Ò'..='Ö' | 'Ø' | 'Ō' | 'Ŏ' | 'Ő' => 'O',
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'Ù'..='Ü' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => 'U',
        'ý' | 'ÿ' => 'y',
        'Ý' | 'Ÿ' => 'Y',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' => 'C',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' => 'N',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'Ś' | 'Ŝ' | 'Ş' | 'Š' => 'S',
        'ź' | 'ż' | 'ž' => 'z',
        'Ź' | 'Ż' | 'Ž' => 'Z',
        'ď' | 'đ' => 'd',
        'Ď' | 'Đ' => 'D',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => 'G',
        'ĺ' | 'ļ' | 'ľ' | 'ł' => 'l',
        'Ĺ' | 'Ļ' | 'Ľ' | 'Ł' => 'L',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'Ŕ' | 'Ŗ' | 'Ř' => 'R',
        'ţ' | 'ť' => 't',
        'Ţ' | 'Ť' => 'T',
        other => other,
    }
}

fn trim(s: &str, chars: &str, at: TrimAt, invert: bool) -> String {
    let hit = |c: char| {
        let in_set = if chars.is_empty() { c.is_whitespace() } else { chars.contains(c) };
        in_set != invert
    };
    match at {
        TrimAt::Start => s.trim_start_matches(hit).to_string(),
        TrimAt::End => s.trim_end_matches(hit).to_string(),
        TrimAt::Both => s.trim_matches(hit).to_string(),
        TrimAt::All => s.chars().filter(|c| !hit(*c)).collect(),
    }
}

fn renumber(s: &str, nth: usize, mode: &RenumMode, pad: usize, ctx: &Ctx) -> String {
    let bytes = s.as_bytes();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            runs.push((start, i));
        } else {
            i += 1;
        }
    }
    let Some(&(a, b)) = runs.get(nth.saturating_sub(1)) else {
        return s.to_string();
    };
    let Ok(old) = s[a..b].parse::<i64>() else {
        return s.to_string();
    };
    let new = match mode {
        RenumMode::Delta(d) => old + d,
        RenumMode::Sequence { start, step } => start + step * ctx.index as i64,
    }
    .max(0);
    let width = if pad == 0 { b - a } else { pad };
    format!("{}{new:0width$}{}", &s[..a], &s[b..])
}

// ───────────────────────── rule parsing (shared CLI/GUI syntax)

fn re(pat: &str) -> Result<Regex, String> {
    Regex::new(pat).map_err(|e| format!("bad regex '{pat}': {e}"))
}

// "re:PAT" is a regex; anything else matches literally.
fn pat_or_literal(s: &str) -> Result<Regex, String> {
    match s.strip_prefix("re:") {
        Some(p) => re(p),
        None => re(&regex::escape(s)),
    }
}

/// Position grammar shared by insert and move:
/// `start` · `end` · `N` (chars from start) · `-N` (chars from end) ·
/// `before:TEXT` · `after:TEXT` · `rbefore:PAT` · `rafter:PAT`
pub fn parse_pos(s: &str) -> Result<InsertAt, String> {
    if let Some(t) = s.strip_prefix("before:") {
        return Ok(InsertAt::Before(re(&regex::escape(t))?));
    }
    if let Some(t) = s.strip_prefix("after:") {
        return Ok(InsertAt::After(re(&regex::escape(t))?));
    }
    if let Some(p) = s.strip_prefix("rbefore:") {
        return Ok(InsertAt::Before(re(p)?));
    }
    if let Some(p) = s.strip_prefix("rafter:") {
        return Ok(InsertAt::After(re(p)?));
    }
    match s {
        "start" => Ok(InsertAt::Pos(0)),
        "end" | "" => Ok(InsertAt::FromEnd(0)),
        _ => match s.strip_prefix('-') {
            Some(n) => n
                .parse()
                .map(InsertAt::FromEnd)
                .map_err(|_| format!("bad position '{s}'")),
            None => s.parse().map(InsertAt::Pos).map_err(|_| format!("bad position '{s}'")),
        },
    }
}

fn parse_remove(s: &str) -> Result<RemoveWhat, String> {
    if let Some(range) = s.strip_prefix("pos:") {
        let (start, len) = range
            .split_once(',')
            .ok_or_else(|| format!("bad range '{s}' (use pos:START,LEN)"))?;
        return Ok(RemoveWhat::Range {
            start: start.parse().map_err(|_| format!("bad range '{s}'"))?,
            len: len.parse().map_err(|_| format!("bad range '{s}'"))?,
        });
    }
    if let Some(list) = s.strip_prefix("chars:") {
        return Ok(RemoveWhat::Chars(list.to_string()));
    }
    if let Some(p) = s.strip_prefix("re:") {
        return Ok(RemoveWhat::Match(re(p)?));
    }
    match s {
        "digits" => Ok(RemoveWhat::Digits),
        "upper" => Ok(RemoveWhat::Upper),
        "lower" => Ok(RemoveWhat::Lower),
        "diacritics" => Ok(RemoveWhat::Diacritics),
        "" => Err("remove needs something to remove".into()),
        text => Ok(RemoveWhat::Match(re(&regex::escape(text))?)),
    }
}

fn parse_case_scope(s: &str) -> Result<CaseScope, String> {
    if s.is_empty() {
        return Ok(CaseScope::All);
    }
    if let Some(range) = s.strip_prefix("at:") {
        let (start, len) = range
            .split_once(',')
            .ok_or_else(|| format!("bad scope '{s}' (use at:START,LEN)"))?;
        return Ok(CaseScope::Pos {
            start: start.parse().map_err(|_| format!("bad scope '{s}'"))?,
            len: len.parse().map_err(|_| format!("bad scope '{s}'"))?,
        });
    }
    Ok(CaseScope::Match(pat_or_literal(s)?))
}

fn parse_renum(s: &str) -> Result<RenumMode, String> {
    if let Some(d) = s.strip_prefix('+') {
        return d.parse().map(RenumMode::Delta).map_err(|_| format!("bad delta '{s}'"));
    }
    if s.starts_with('-') {
        return s.parse().map(RenumMode::Delta).map_err(|_| format!("bad delta '{s}'"));
    }
    let (start, step) = match s.split_once('/') {
        Some((a, b)) => (a, b),
        None => (s, "1"),
    };
    Ok(RenumMode::Sequence {
        start: start.parse().map_err(|_| format!("bad start '{s}'"))?,
        step: step.parse().map_err(|_| format!("bad step '{s}'"))?,
    })
}

/// Build a rule from its kind, option mods, and one or two text arguments.
/// The same syntax drives CLI flag suffixes (`-r:ci:first`) and the GUI.
/// Mods common to all kinds: `name` / `ext` (apply-to; default both).
pub fn build_rule(kind: &str, mods: &[&str], a: &str, b: &str) -> Result<(Rule, Part), String> {
    let mut part = Part::Both;
    let mut rest: Vec<&str> = Vec::new();
    for m in mods {
        match *m {
            "name" | "stem" => part = Part::Name,
            "ext" => part = Part::Ext,
            "both" => part = Part::Both,
            other => rest.push(other),
        }
    }
    let unknown = |m: &str| format!("unknown {kind} option '{m}'");
    let rule = match kind {
        "replace" => {
            let (mut ci, mut occ) = (false, Occurrence::All);
            for m in &rest {
                match *m {
                    "ci" => ci = true,
                    "all" => occ = Occurrence::All,
                    "first" => occ = Occurrence::First,
                    "last" => occ = Occurrence::Last,
                    m => match m.strip_prefix('n').and_then(|n| n.parse().ok()) {
                        Some(n) => occ = Occurrence::Nth(n),
                        None => return Err(unknown(m)),
                    },
                }
            }
            if a.is_empty() {
                return Err("replace needs text to find".into());
            }
            Rule::Replace { find: a.into(), repl: b.into(), ci, occ }
        }
        "regex" => {
            if let Some(m) = rest.first() {
                return Err(unknown(m));
            }
            Rule::Regex(re(a)?, b.into())
        }
        "case" => {
            if let Some(m) = rest.first() {
                return Err(unknown(m));
            }
            let mode = match a {
                "lower" => CaseMode::Lower,
                "upper" => CaseMode::Upper,
                "title" => CaseMode::Title,
                "first" => CaseMode::First,
                "invert" => CaseMode::Invert,
                other => return Err(format!("unknown case '{other}' (lower|upper|title|first|invert)")),
            };
            Rule::Case { mode, scope: parse_case_scope(b)? }
        }
        "pattern" => {
            if let Some(m) = rest.first() {
                return Err(unknown(m));
            }
            if a.is_empty() {
                return Err("pattern needs a template".into());
            }
            Rule::Pattern(a.into())
        }
        "insert" => {
            if let Some(m) = rest.first() {
                return Err(unknown(m));
            }
            if a.is_empty() {
                return Err("insert needs text".into());
            }
            Rule::Insert { text: a.into(), at: parse_pos(b)? }
        }
        "remove" => {
            if let Some(m) = rest.first() {
                return Err(unknown(m));
            }
            Rule::Remove(parse_remove(a)?)
        }
        "trim" => {
            let (mut at, mut invert) = (TrimAt::Both, false);
            for m in &rest {
                match *m {
                    "start" => at = TrimAt::Start,
                    "end" => at = TrimAt::End,
                    "both" => at = TrimAt::Both,
                    "all" => at = TrimAt::All,
                    "inv" => invert = true,
                    m => return Err(unknown(m)),
                }
            }
            Rule::Trim { chars: a.into(), at, invert }
        }
        "renumber" => {
            let mut pad = 0;
            for m in &rest {
                match m.strip_prefix("pad").and_then(|p| p.parse().ok()) {
                    Some(p) => pad = p,
                    None => return Err(unknown(m)),
                }
            }
            let nth: usize = a.parse().map_err(|_| format!("bad number position '{a}'"))?;
            if nth == 0 {
                return Err("number position is 1-based".into());
            }
            Rule::Renumber { nth, mode: parse_renum(b)?, pad }
        }
        "move" => {
            if let Some(m) = rest.first() {
                return Err(unknown(m));
            }
            if a.is_empty() {
                return Err("move needs text to move".into());
            }
            Rule::MoveText { pat: pat_or_literal(a)?, to: parse_pos(b)? }
        }
        "swap" => {
            if let Some(m) = rest.first() {
                return Err(unknown(m));
            }
            if a.is_empty() {
                return Err("swap needs a separator".into());
            }
            Rule::Swap(a.into())
        }
        "names" => {
            if let Some(m) = rest.first() {
                return Err(unknown(m));
            }
            Rule::ListNames(a.lines().map(|l| l.trim_end().to_string()).collect())
        }
        other => return Err(format!("unknown rule kind '{other}'")),
    };
    Ok((rule, part))
}

/// Condition spec: `[not:]FIELD:OP` with fields `name|new|ext|path` and ops
/// `has|starts|ends|eq|re`. The value is the second argument.
pub fn build_cond(spec: &str, value: &str) -> Result<Cond, String> {
    let (negate, spec) = match spec.strip_prefix("not:") {
        Some(rest) => (true, rest),
        None => (false, spec),
    };
    let (field, op) = spec
        .split_once(':')
        .ok_or_else(|| format!("bad condition '{spec}' (use FIELD:OP, e.g. ext:eq)"))?;
    let field = match field {
        "name" => CondField::Original,
        "new" => CondField::Current,
        "ext" => CondField::Ext,
        "path" => CondField::Path,
        other => return Err(format!("unknown condition field '{other}' (name|new|ext|path)")),
    };
    let op = match op {
        "has" => CondOp::Has,
        "starts" => CondOp::Starts,
        "ends" => CondOp::Ends,
        "eq" => CondOp::Eq,
        "re" => CondOp::Re(re(value)?),
        other => return Err(format!("unknown condition op '{other}' (has|starts|ends|eq|re)")),
    };
    Ok(Cond { field, op, value: value.into(), negate })
}

// ───────────────────────── sorting and globbing

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

    fn entry(kind: &str, mods: &[&str], a: &str, b: &str) -> RuleEntry {
        let (rule, part) = build_rule(kind, mods, a, b).unwrap();
        RuleEntry { rule, part, cond: None }
    }

    fn run(e: &RuleEntry, name: &str) -> String {
        let path = Path::new("C:/photos/trip").join(name);
        let ctx = Ctx { index: 6, num: 7, pad: 3, folder_num: 1, path: &path, original: name };
        apply_entry(e, name, &ctx)
    }

    #[test]
    fn replace_options() {
        assert_eq!(run(&entry("replace", &[], " ", "_"), "a b c.txt"), "a_b_c.txt");
        assert_eq!(run(&entry("replace", &["first"], " ", "_"), "a b c.txt"), "a_b c.txt");
        assert_eq!(run(&entry("replace", &["last"], " ", "_"), "a b c.txt"), "a b_c.txt");
        assert_eq!(run(&entry("replace", &["n2"], "o", "0"), "foo woof.txt"), "fo0 woof.txt");
        assert_eq!(run(&entry("replace", &["ci"], "IMG", "pic"), "img_Img.jpg"), "pic_pic.jpg");
        assert_eq!(run(&entry("replace", &[], "É", "E"), "cafÉ.txt"), "cafE.txt");
    }

    #[test]
    fn apply_to_parts() {
        assert_eq!(run(&entry("case", &["ext"], "lower", ""), "Photo.JPG"), "Photo.jpg");
        assert_eq!(run(&entry("case", &["name"], "upper", ""), "photo.jpg"), "PHOTO.jpg");
        assert_eq!(run(&entry("replace", &["name"], "o", "0"), "photo.mov"), "ph0t0.mov");
        assert_eq!(run(&entry("case", &[], "lower", ""), "Photo.JPG"), "photo.jpg");
        // no extension: ext rules are a no-op, name rules hit the whole thing
        assert_eq!(run(&entry("case", &["ext"], "upper", ""), "readme"), "readme");
        assert_eq!(run(&entry("case", &["name"], "upper", ""), "readme"), "README");
    }

    #[test]
    fn case_modes_and_scopes() {
        assert_eq!(run(&entry("case", &[], "title", ""), "my file.txt"), "My File.Txt");
        assert_eq!(run(&entry("case", &["name"], "first", ""), "my file.txt"), "My file.txt");
        assert_eq!(run(&entry("case", &[], "invert", ""), "aBc.TXT"), "AbC.txt");
        assert_eq!(run(&entry("case", &["name"], "upper", "at:0,2"), "abcdef.txt"), "ABcdef.txt");
        assert_eq!(run(&entry("case", &[], "upper", "img"), "img_img.jpg"), "IMG_IMG.jpg");
    }

    #[test]
    fn insert_positions() {
        assert_eq!(run(&entry("insert", &["name"], "new_", "start"), "a.txt"), "new_a.txt");
        assert_eq!(run(&entry("insert", &["name"], "_old", "end"), "a.txt"), "a_old.txt");
        assert_eq!(run(&entry("insert", &["name"], "-", "2"), "abcd.txt"), "ab-cd.txt");
        assert_eq!(run(&entry("insert", &["name"], "-", "-1"), "abcd.txt"), "abc-d.txt");
        assert_eq!(run(&entry("insert", &[], "X", "before:cd"), "abcd.txt"), "abXcd.txt");
        assert_eq!(run(&entry("insert", &[], "X", "after:cd"), "abcd.txt"), "abcdX.txt");
        assert_eq!(run(&entry("insert", &[], "X", "rbefore:\\d+"), "ab12.txt"), "abX12.txt");
        assert_eq!(run(&entry("insert", &[], "X", "before:zzz"), "abcd.txt"), "abcd.txt");
        // tags in inserted text
        assert_eq!(run(&entry("insert", &["name"], "_<num>", "end"), "a.txt"), "a_007.txt");
    }

    #[test]
    fn remove_kinds() {
        assert_eq!(run(&entry("remove", &["name"], "pos:1,2", ""), "abcd.txt"), "ad.txt");
        assert_eq!(run(&entry("remove", &[], "chars:_-", ""), "a_b-c.txt"), "abc.txt");
        assert_eq!(run(&entry("remove", &["name"], "digits", ""), "a1b2.txt"), "ab.txt");
        assert_eq!(run(&entry("remove", &["name"], "upper", ""), "aXbY.txt"), "ab.txt");
        assert_eq!(run(&entry("remove", &["name"], "lower", ""), "aXbY.txt"), "XY.txt");
        assert_eq!(run(&entry("remove", &[], "diacritics", ""), "café_señor.txt"), "cafe_senor.txt");
        assert_eq!(run(&entry("remove", &["name"], "re:\\(\\d+\\)", ""), "a(1).txt"), "a.txt");
        assert_eq!(run(&entry("remove", &[], "copy", ""), "a copy.txt"), "a .txt");
    }

    #[test]
    fn trim_kinds() {
        assert_eq!(run(&entry("trim", &["name"], "", ""), " a b .txt"), "a b.txt");
        assert_eq!(run(&entry("trim", &["name", "start"], "_", ""), "__a__.txt"), "a__.txt");
        assert_eq!(run(&entry("trim", &["name", "all"], "_", ""), "_a_b_.txt"), "ab.txt");
        // inverse: keep only underscores and letters a/b at the edges trimmed away
        assert_eq!(run(&entry("trim", &["name", "inv"], "ab", ""), "xxabyy.txt"), "ab.txt");
    }

    #[test]
    fn renumber_modes() {
        // ctx.index is 6
        assert_eq!(run(&entry("renumber", &[], "1", "+10"), "img005.jpg"), "img015.jpg");
        assert_eq!(run(&entry("renumber", &[], "1", "-9"), "ep12.mkv"), "ep03.mkv");
        assert_eq!(run(&entry("renumber", &[], "2", "+1"), "s01e04.mkv"), "s01e05.mkv");
        assert_eq!(run(&entry("renumber", &["pad4"], "1", "100/10"), "img5.jpg"), "img0160.jpg");
        assert_eq!(run(&entry("renumber", &[], "3", "+1"), "a1b2.txt"), "a1b2.txt");
    }

    #[test]
    fn move_and_swap() {
        assert_eq!(run(&entry("move", &["name"], "re:\\d+", "start"), "abc123.txt"), "123abc.txt");
        assert_eq!(run(&entry("move", &["name"], "CD", "end"), "abCDef.txt"), "abefCD.txt");
        assert_eq!(run(&entry("swap", &["name"], " - ", ""), "Artist - Title.mp3"), "Title - Artist.mp3");
        assert_eq!(run(&entry("swap", &["name"], " - ", ""), "NoSep.mp3"), "NoSep.mp3");
    }

    #[test]
    fn list_names_by_index() {
        let names = "zero\none\ntwo\nthree\nfour\nfive\nsix\nseven";
        // ctx.index is 6 -> "six"; applied to the stem it keeps the extension
        assert_eq!(run(&entry("names", &[], names, ""), "old.txt"), "six");
        assert_eq!(run(&entry("names", &["name"], names, ""), "old.txt"), "six.txt");
    }

    #[test]
    fn conditions_gate_rules() {
        let mut e = entry("case", &[], "upper", "");
        e.cond = Some(build_cond("ext:eq", "jpg").unwrap());
        assert_eq!(run(&e, "photo.jpg"), "PHOTO.JPG");
        assert_eq!(run(&e, "photo.png"), "photo.png");

        e.cond = Some(build_cond("not:name:has", "draft").unwrap());
        assert_eq!(run(&e, "draft_1.jpg"), "draft_1.jpg");
        assert_eq!(run(&e, "final_1.jpg"), "FINAL_1.JPG");

        e.cond = Some(build_cond("name:re", r"^\d").unwrap());
        assert_eq!(run(&e, "1st.jpg"), "1ST.JPG");
        assert_eq!(run(&e, "first.jpg"), "first.jpg");

        e.cond = Some(build_cond("path:has", "trip").unwrap());
        assert_eq!(run(&e, "a.jpg"), "A.JPG");
    }

    #[test]
    fn pattern_uses_shared_tags() {
        assert_eq!(run(&entry("pattern", &[], "x_<num>.<ext>", ""), "old.jpg"), "x_007.jpg");
        assert_eq!(run(&entry("pattern", &[], "<parent>_<name>!", ""), "noext"), "trip_noext!");
        // pattern applied to the stem only keeps the real extension
        assert_eq!(run(&entry("pattern", &["name"], "<name>_<num>", ""), "a.jpg"), "a_007.jpg");
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
