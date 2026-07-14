use super::*;

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
            None => s
                .parse()
                .map(InsertAt::Pos)
                .map_err(|_| format!("bad position '{s}'")),
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
        return d
            .parse()
            .map(RenumMode::Delta)
            .map_err(|_| format!("bad delta '{s}'"));
    }
    if s.starts_with('-') {
        return s
            .parse()
            .map(RenumMode::Delta)
            .map_err(|_| format!("bad delta '{s}'"));
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
            Rule::Replace {
                find: a.into(),
                repl: b.into(),
                ci,
                occ,
            }
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
                other => {
                    return Err(format!(
                        "unknown case '{other}' (lower|upper|title|first|invert)"
                    ));
                }
            };
            Rule::Case {
                mode,
                scope: parse_case_scope(b)?,
            }
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
            Rule::Insert {
                text: a.into(),
                at: parse_pos(b)?,
            }
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
            Rule::Trim {
                chars: a.into(),
                at,
                invert,
            }
        }
        "renumber" => {
            let mut pad = 0;
            for m in &rest {
                match m.strip_prefix("pad").and_then(|p| p.parse().ok()) {
                    Some(p) => pad = p,
                    None => return Err(unknown(m)),
                }
            }
            let nth: usize = a
                .parse()
                .map_err(|_| format!("bad number position '{a}'"))?;
            if nth == 0 {
                return Err("number position is 1-based".into());
            }
            Rule::Renumber {
                nth,
                mode: parse_renum(b)?,
                pad,
            }
        }
        "move" => {
            if let Some(m) = rest.first() {
                return Err(unknown(m));
            }
            if a.is_empty() {
                return Err("move needs text to move".into());
            }
            Rule::MoveText {
                pat: pat_or_literal(a)?,
                to: parse_pos(b)?,
            }
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
        other => {
            return Err(format!(
                "unknown condition field '{other}' (name|new|ext|path)"
            ));
        }
    };
    let op = match op {
        "has" => CondOp::Has,
        "starts" => CondOp::Starts,
        "ends" => CondOp::Ends,
        "eq" => CondOp::Eq,
        "re" => CondOp::Re(re(value)?),
        other => {
            return Err(format!(
                "unknown condition op '{other}' (has|starts|ends|eq|re)"
            ));
        }
    };
    Ok(Cond {
        field,
        op,
        value: value.into(),
        negate,
    })
}
