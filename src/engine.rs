// Shared rename engine: rule model, application, parsing, and file discovery.

use regex::Regex;
use std::path::Path;

mod apply;
mod files;
mod parse;

pub use apply::{apply_entry, reset_js, split_ext};
pub(crate) use apply::{change_case, join_ext};
#[cfg(test)]
use files::mask_re;
pub use files::{FsKinds, Masks, collect_dir, expand, name_of, natural_key, sort_files};
pub use parse::{build_cond, build_rule};

// ───────────────────────── rule model

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Part {
    Both,
    Name,
    Ext,
}

#[derive(Debug)]
pub enum Occurrence {
    All,
    First,
    Last,
    Nth(usize), // 1-based
}

#[derive(Clone, Copy, Debug)]
pub enum CaseMode {
    Lower,
    Upper,
    Title, // Each Word
    First, // sentence case
    Invert,
}

#[derive(Debug)]
pub enum CaseScope {
    All,
    Pos { start: usize, len: usize }, // in chars
    Match(Regex),
}

#[derive(Debug)]
pub enum InsertAt {
    Pos(usize), // in chars; clamped to the end
    FromEnd(usize),
    Before(Regex), // first match; no match = no change
    After(Regex),
}

#[derive(Debug)]
pub enum RemoveWhat {
    Range { start: usize, len: usize }, // in chars
    Match(Regex),
    Chars(String),
    Digits,
    Upper,
    Lower,
    Diacritics, // é -> e, not removal
}

#[derive(Clone, Copy, Debug)]
pub enum TrimAt {
    Start,
    End,
    Both,
    All, // throughout
}

#[derive(Debug)]
pub enum RenumMode {
    Delta(i64),
    Sequence { start: i64, step: i64 }, // start + step * list index
}

#[derive(Debug)]
pub enum Rule {
    Replace {
        find: String,
        repl: String,
        ci: Option<Regex>,
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
        pad: u16,
    }, // pad 0 = keep width; u16 is the formatter's width limit
    MoveText {
        pat: Regex,
        to: InsertAt,
    },
    Swap(String),           // swap around first separator: "a - b" -> "b - a"
    ListNames(Vec<String>), // one explicit new name per list position
    // Many find→replace pairs applied in order (AR's "List Replace").
    Pairs {
        pairs: Vec<(String, String, Option<Regex>)>,
    },
    Js(String), // sandboxed script; completion value = new name
}

#[derive(Debug)]
pub enum CondField {
    Original, // original file name
    Current,  // name after the rules so far
    Ext,
    Path,
}

#[derive(Debug)]
pub enum CondOp {
    Has,
    Starts,
    Ends,
    Eq,
    Re(Regex),
}

#[derive(Debug)]
pub struct Cond {
    pub field: CondField,
    pub op: CondOp,
    pub value: String,
    pub negate: bool,
}

#[derive(Debug)]
pub struct RuleEntry {
    pub rule: Rule,
    pub part: Part,
    pub cond: Option<Cond>,
}

/// Per-file context a rule application runs in.
pub struct Ctx<'a> {
    pub index: usize,           // 0-based list position
    pub num: usize,             // counter (start + index)
    pub pad: u16,               // formatter widths panic past u16::MAX
    pub folder_num: usize,      // 1-based position among list items in the same folder
    pub total: usize,           // batch size for <total>; 0 = no batch context
    pub csv: &'a [Vec<String>], // rows for <csv:COL>; empty = no CSV loaded
    pub path: &'a Path,
    pub original: &'a str,
}

#[cfg(test)]
mod tests;
