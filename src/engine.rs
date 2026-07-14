// Shared rename engine: rule model, application, parsing, and file discovery.

use regex::Regex;
use std::path::Path;

mod apply;
mod files;
mod parse;

pub(crate) use apply::change_case;
pub use apply::{apply_entry, split_ext};
#[cfg(test)]
use files::wild_match;
pub use files::{Masks, collect_dir, expand, name_of, natural_key};
pub use parse::{build_cond, build_rule};

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
    Title, // Each Word
    First, // sentence case
    Invert,
}

pub enum CaseScope {
    All,
    Pos { start: usize, len: usize }, // in chars
    Match(Regex),
}

pub enum InsertAt {
    Pos(usize), // in chars; clamped to the end
    FromEnd(usize),
    Before(Regex), // first match; no match = no change
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
    Replace {
        find: String,
        repl: String,
        ci: bool,
        occ: Occurrence,
    },
    Regex(Regex, String),
    Case {
        mode: CaseMode,
        scope: CaseScope,
    },
    Pattern(String),
    Insert {
        text: String,
        at: InsertAt,
    },
    Remove(RemoveWhat),
    Trim {
        chars: String,
        at: TrimAt,
        invert: bool,
    }, // empty chars = whitespace
    Renumber {
        nth: usize,
        mode: RenumMode,
        pad: usize,
    }, // pad 0 = keep width
    MoveText {
        pat: Regex,
        to: InsertAt,
    },
    Swap(String),           // swap around first separator: "a - b" -> "b - a"
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
    pub index: usize, // 0-based list position
    pub num: usize,   // counter (start + index)
    pub pad: usize,
    pub folder_num: usize, // 1-based position among list items in the same folder
    pub path: &'a Path,
    pub original: &'a str,
}

#[cfg(test)]
mod tests;
