use super::*;
use crate::tags;
use regex::Regex;

// ───────────────────────── application

pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, e),
        _ => (name, ""),
    }
}

pub(crate) fn join_ext(stem: &str, ext: &str) -> String {
    if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    }
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
        Rule::Replace {
            find,
            repl,
            ci,
            occ,
        } => replace_occ(s, find, &tags::expand(repl, full, ctx), *ci, occ),
        Rule::Regex(re, repl) => re
            .replace_all(s, tags::expand(repl, full, ctx).as_str())
            .into_owned(),
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
        Rule::ListNames(names) => names
            .get(ctx.index)
            .cloned()
            .unwrap_or_else(|| s.to_string()),
        Rule::Js(src) => eval_js(src, s, full, ctx),
    }
}

// ───────────────────────── sandboxed JS rules
//
// Boa exposes no filesystem or network by default, and we register nothing
// beyond plain values — that IS the sandbox, and the "explicit file-read
// permission" granted is none. One engine context is reused across a batch
// (reset by plan()), so script globals persist item to item: pre-batch
// state is just `if (typeof n == 'undefined') n = 0;`.
thread_local! {
    static JS: std::cell::RefCell<Option<boa_engine::Context>> =
        const { std::cell::RefCell::new(None) };
}

/// Drop the shared JS context so a new batch/preview starts stateless.
pub fn reset_js() {
    JS.with(|c| *c.borrow_mut() = None);
}

/// Run `src` with globals name/ext/stem/original/path/index/num set for this
/// item. The script's completion value becomes the new name; undefined/null
/// or any runtime error leaves the name unchanged.
pub(super) fn eval_js(src: &str, s: &str, full: &str, ctx: &Ctx) -> String {
    use boa_engine::{Context, JsString, JsValue, Source, property::Attribute};
    JS.with(|cell| {
        let mut slot = cell.borrow_mut();
        let jsctx = slot.get_or_insert_with(Context::default);
        let (stem, ext) = split_ext(full);
        let vars: &[(&str, JsValue)] = &[
            ("name", JsString::from(s).into()),
            ("stem", JsString::from(stem).into()),
            ("ext", JsString::from(ext).into()),
            ("original", JsString::from(ctx.original).into()),
            (
                "path",
                JsString::from(ctx.path.to_string_lossy().as_ref()).into(),
            ),
            ("index", JsValue::from(ctx.index as f64)),
            ("num", JsValue::from(ctx.num as f64)),
        ];
        for (k, v) in vars {
            let _ = jsctx.register_global_property(JsString::from(*k), v.clone(), Attribute::all());
        }
        match jsctx.eval(Source::from_bytes(src)) {
            Ok(v) if !v.is_null_or_undefined() => v
                .to_string(jsctx)
                .map(|js| js.to_std_string_escaped())
                .unwrap_or_else(|_| s.to_string()),
            _ => s.to_string(),
        }
    })
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
        s.match_indices(find)
            .map(|(i, _)| (i, i + find.len()))
            .collect()
    };
    let picked: Vec<(usize, usize)> = match occ {
        Occurrence::All => ranges,
        Occurrence::First => ranges.first().into_iter().copied().collect(),
        Occurrence::Last => ranges.last().into_iter().copied().collect(),
        Occurrence::Nth(n) => ranges
            .get(n.saturating_sub(1))
            .into_iter()
            .copied()
            .collect(),
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
            let (head, tail): (String, String) = (
                chars[..*start].iter().collect(),
                chars[end..].iter().collect(),
            );
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
        s.char_indices()
            .nth(nchars)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
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
        let in_set = if chars.is_empty() {
            c.is_whitespace()
        } else {
            chars.contains(c)
        };
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
