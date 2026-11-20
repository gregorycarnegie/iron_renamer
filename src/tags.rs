// Tag expansion shared by every text rule (Pattern, Insert, Replace/Regex
// replacements) and, later, destination paths. Unknown tags and bad
// arguments pass through unchanged so plain '<' and '>' in names stay safe.
//
// Syntax: <tag[:arg[:arg]][|modifier[:args]]...>
//
// Names       <name> <ext>        current stem / extension
//             <oname> <oext>      original stem / extension
// Counters    <num[:START[:STEP]]>    decimal (pad follows the batch pad)
//             <hex[:START[:STEP]]>    hexadecimal
//             <alpha[:START[:STEP]]>  a..z, aa.. (START may be letters)
//             <roman[:START[:STEP]]>  I, II, III...
//             <dirnum[:START[:STEP]]> resets per parent folder
//             <index>                 1-based list position
//             <total>                 number of items in the batch
// Location    <parent> <path>     folder name / full folder path
//             <subfolder[:N]>     Nth ancestor folder (1 = parent)
// File        <size[:kb|mb|gb|tb|h]> ('h' = human text, "1.4 GB")
//             <crc32> <md5> <sha1>
// Dates (UTC) <now|created|modified|accessed[:FMT[:OFFSET]]>
//             FMT tokens yyyy yy MM dd HH mm ss (default yyyy-MM-dd),
//             or the literal FMT `unix` for epoch seconds,
//             OFFSET like +3d -12h +30m
// Random      <rand[:MIN[:MAX]]> <rands[:LEN]>
// Data        <csv:COL> — column COL (1-based number, or a header name)
//             of the row matching the item's list position; rows are
//             loaded by the frontend (--csv / GUI import)
// Metadata    <exif:TAG> (built-in readers, no external tools) plus aliases
//             <width> <height> <lat> <lon> (signed decimal GPS)
//             <datetaken> <artist> <album> <track> <title> <duration>
//             <author> — values are sanitized for file names (':'
//             becomes '-'). A tag missing from the file gives ""
//             (so |fallback applies).
// Modifiers   |upper |lower |title |sub:START[,LEN] |pad:N |trim[:CHARS]
//             |replace:OLD[,NEW] |split:SEP,N (empty SEP = whitespace;
//             N 1-based, negative from the end) |fallback:TEXT
//             |+N |-N |*N |/N

use crate::arith::{self, seed_from, xorshift64};
use crate::engine::{CaseMode, Ctx, change_case, split_ext};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn expand(template: &str, full_name: &str, ctx: &Ctx) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            out.push('<');
            rest = after;
            continue;
        };
        match eval(&after[..close], full_name, ctx) {
            Some(v) => {
                out.push_str(&v);
                rest = &after[close + 1..];
            }
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn eval(body: &str, full_name: &str, ctx: &Ctx) -> Option<String> {
    let mut parts = body.split('|');
    let head = parts.next().unwrap();
    let mut segs = head.split(':');
    let tag = segs.next().unwrap().to_ascii_lowercase();
    let args: Vec<&str> = segs.collect();
    let (stem, ext) = split_ext(full_name);
    let (ostem, oext) = split_ext(ctx.original);

    let mut val = match tag.as_str() {
        "name" => stem.to_string(),
        "ext" => ext.to_string(),
        "oname" => ostem.to_string(),
        "oext" => oext.to_string(),
        "index" => (ctx.index + 1).to_string(),
        "num" => format!(
            "{:0w$}",
            counter(&args, ctx.index, ctx.num, false)?,
            w = usize::from(ctx.pad)
        ),
        "hex" => format!(
            "{:0w$x}",
            counter(&args, ctx.index, ctx.num, false)?,
            w = usize::from(ctx.pad)
        ),
        "roman" => roman(counter(&args, ctx.index, ctx.num, false)?),
        "alpha" => alpha(counter(&args, ctx.index, ctx.num, true)?),
        "dirnum" => {
            format!(
                "{:0w$}",
                counter(&args, ctx.folder_num - 1, ctx.folder_num, false)?,
                w = usize::from(ctx.pad)
            )
        }
        "total" => {
            if ctx.total == 0 {
                return None; // no batch context (e.g. bare expand)
            }
            ctx.total.to_string()
        }
        "parent" => abs_parent(ctx)?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "path" => abs_parent(ctx)?.display().to_string(),
        "subfolder" => {
            let n: usize = match args.first().copied().unwrap_or("") {
                "" => 1,
                s => s.parse().ok().filter(|&n| n >= 1)?,
            };
            let mut p = abs_parent(ctx)?;
            for _ in 1..n {
                p = p.parent()?.to_path_buf();
            }
            p.file_name()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_default()
        }
        "size" => {
            let bytes = fs::metadata(ctx.path).ok()?.len();
            match args.first().copied().unwrap_or("") {
                "" | "b" => bytes.to_string(),
                "kb" => (bytes >> 10).to_string(),
                "mb" => (bytes >> 20).to_string(),
                "gb" => (bytes >> 30).to_string(),
                "tb" => (bytes >> 40).to_string(),
                "h" => human_size(bytes),
                _ => return None,
            }
        }
        "crc32" | "md5" | "sha1" => file_hash(ctx, &tag)?,
        "csv" => {
            let col = args.first().copied().filter(|c| !c.is_empty())?;
            csv_cell(col, ctx)?
        }
        "now" | "created" | "modified" | "accessed" => {
            let t = match tag.as_str() {
                "now" => SystemTime::now(),
                "created" => fs::metadata(ctx.path).ok()?.created().ok()?,
                "accessed" => fs::metadata(ctx.path).ok()?.accessed().ok()?,
                _ => fs::metadata(ctx.path).ok()?.modified().ok()?,
            };
            let mut secs = i64::try_from(t.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
            if let Some(off) = args.get(1) {
                secs = secs.checked_add(parse_offset(off)?)?;
            }
            match args.first().filter(|s| !s.is_empty()).copied() {
                Some("unix") => secs.to_string(),
                fmt => fmt_dt(fmt.unwrap_or("yyyy-MM-dd"), secs),
            }
        }
        "rand" => {
            let min: i64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let max: i64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(9999);
            arith::rand_in(min, max, rng())?.to_string()
        }
        "rands" => {
            // u16 caps the length; anything longer is a bad argument.
            let len = match args.first().copied().unwrap_or("") {
                "" => 8,
                s => s.parse::<u16>().ok()?,
            };
            const CS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
            (0..len)
                .map(|_| CS[(rng() % CS.len() as u64) as usize] as char)
                .collect()
        }
        "exif" => {
            let tag = args.first().copied().filter(|t| !t.is_empty())?;
            sanitize(&crate::meta::get(ctx.path, tag)?)
        }
        "width" => meta_alias(ctx, &["imagewidth"])?,
        "height" => meta_alias(ctx, &["imageheight"])?,
        "datetaken" => meta_alias(ctx, &["datetimeoriginal", "createdate"])?,
        "artist" => meta_alias(ctx, &["artist", "albumartist"])?,
        "album" => meta_alias(ctx, &["album"])?,
        "track" => meta_alias(ctx, &["track", "tracknumber"])?,
        "title" => meta_alias(ctx, &["title"])?,
        "duration" => meta_alias(ctx, &["duration"])?,
        "author" => meta_alias(ctx, &["author", "creator"])?,
        // Signed decimal degrees (see meta.rs insert_gps).
        "lat" => meta_alias(ctx, &["gpslatitude"])?,
        "lon" => meta_alias(ctx, &["gpslongitude"])?,
        _ => return None,
    };
    for m in parts {
        val = modify(val, m)?;
    }
    Some(val)
}

// First non-empty of several metadata field names; "" when the file simply
// lacks them all (so |fallback applies), None when the file is unreadable.
fn meta_alias(ctx: &Ctx, names: &[&str]) -> Option<String> {
    for n in names {
        let v = crate::meta::get(ctx.path, n)?;
        if !v.is_empty() {
            return Some(sanitize(&v));
        }
    }
    Some(String::new())
}

// Make a metadata value safe for a file name: ':' -> '-' (keeps dates and
// durations readable), other invalid characters dropped.
fn sanitize(v: &str) -> String {
    let cleaned: String = v
        .chars()
        .filter_map(|c| match c {
            ':' => Some('-'),
            c if crate::batch::INVALID_CHARS.contains(&c) || (c as u32) < 0x20 => None,
            c => Some(c),
        })
        .collect();
    cleaned.trim().to_string()
}

// Counter value: no args -> the batch counter; with args -> START + STEP*i.
// For <alpha>, START may be letters ("aa" = 27). Values too large for i64
// count as bad arguments; the arithmetic is Verus-verified in tags/arith.rs.
fn counter(args: &[&str], i: usize, default: usize, letters: bool) -> Option<i64> {
    if args.is_empty() || args[0].is_empty() {
        return i64::try_from(default).ok();
    }
    let start: i64 = match args[0].parse() {
        Ok(n) => n,
        Err(_) if letters => arith::alpha_to_num(args[0].as_bytes())?,
        Err(_) => return None,
    };
    let step: i64 = match args.get(1) {
        Some(s) => s.parse().ok()?,
        None => 1,
    };
    arith::counter_value(start, step, i)
}

pub(crate) fn alpha(mut n: i64) -> String {
    if n < 1 {
        n = 1;
    }
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(b'a' + (n % 26) as u8);
        n /= 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

fn roman(mut n: i64) -> String {
    if !(1..=3999).contains(&n) {
        return n.to_string();
    }
    const STEPS: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (v, sym) in STEPS {
        while n >= v {
            out.push_str(sym);
            n -= v;
        }
    }
    out
}

// Absolutize so relative paths like "img.jpg" still have a parent.
fn abs_parent(ctx: &Ctx) -> Option<PathBuf> {
    std::path::absolute(ctx.path)
        .ok()?
        .parent()
        .map(PathBuf::from)
}

fn file_hash(ctx: &Ctx, kind: &str) -> Option<String> {
    thread_local! {
        static CACHE: RefCell<HashMap<(PathBuf, String), String>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        let key = (ctx.path.to_path_buf(), kind.to_string());
        if let Some(v) = c.borrow().get(&key) {
            return Some(v.clone());
        }
        let data = fs::read(ctx.path).ok()?;
        let v = match kind {
            "md5" => format!("{:x}", md5::compute(&data)),
            "sha1" => sha1_smol::Sha1::from(&data[..]).digest().to_string(),
            _ => format!("{:08x}", crc32fast::hash(&data)),
        };
        c.borrow_mut().insert(key, v.clone());
        Some(v)
    })
}

fn human_size(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

// Numeric COL: 1-based column of row `ctx.index`. Header COL: the named column
// (case-insensitive, row 0 is the header) of row `index + 1`. A missing
// row/cell gives "" so |fallback applies; no CSV loaded leaves the tag literal.
fn csv_cell(col: &str, ctx: &Ctx) -> Option<String> {
    let rows = ctx.csv;
    if rows.is_empty() {
        return None;
    }
    let (row, ncol) = match col.parse::<usize>() {
        Ok(n) if n >= 1 => (ctx.index, n - 1),
        _ => {
            let n = rows[0].iter().position(|h| h.eq_ignore_ascii_case(col))?;
            (ctx.index + 1, n)
        }
    };
    Some(sanitize(
        rows.get(row).and_then(|r| r.get(ncol)).map_or("", |s| s),
    ))
}

pub(crate) fn parse_offset(s: &str) -> Option<i64> {
    let (num, mult) = match s.chars().last()? {
        'd' => (&s[..s.len() - 1], 86400),
        'h' => (&s[..s.len() - 1], 3600),
        'm' => (&s[..s.len() - 1], 60),
        's' => (&s[..s.len() - 1], 1),
        _ => (s, 1),
    };
    num.parse::<i64>().ok()?.checked_mul(mult)
}

/// Civil date from epoch seconds (Howard Hinnant's algorithm), UTC.
pub fn civil_utc(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m as u32, d as u32, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Epoch seconds from a UTC civil date (inverse of `civil_utc`).
pub(crate) fn epoch_from_civil(y: i64, m: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s)
}

/// Pull the first yyyy?MM?dd[?HH?mm[?ss]] out of arbitrary text
/// ("IMG_20240501_1230.jpg", "2024-05-01", "trip 2024.05.01") as epoch secs.
pub(crate) fn extract_datetime(text: &str) -> Option<i64> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        // ASCII digits only: Unicode \d also matches e.g. Arabic-Indic digits,
        // which str::parse rejects.
        regex::Regex::new(
            r"([0-9]{4})[^0-9]?([0-9]{2})[^0-9]?([0-9]{2})(?:[^0-9]?([0-9]{2})[^0-9]?([0-9]{2})(?:[^0-9]?([0-9]{2}))?)?",
        )
        .unwrap()
    });
    for c in re.captures_iter(text) {
        let g = |i: usize| {
            c.get(i)
                .map(|m| m.as_str().parse::<u32>().unwrap())
                .unwrap_or(0)
        };
        let (y, m, d) = (c[1].parse::<i64>().unwrap(), g(2), g(3));
        let (h, mi, s) = (g(4), g(5), g(6));
        if (1900..=2999).contains(&y)
            && (1..=12).contains(&m)
            && (1..=31).contains(&d)
            && h < 24
            && mi < 60
            && s < 60
        {
            return Some(epoch_from_civil(y, m, d, h, mi, s));
        }
    }
    None
}

/// "yyyy-MM-dd HH:mm" for a filesystem time; used by the item-details panel.
pub fn dt_string(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    fmt_dt("yyyy-MM-dd HH:mm", secs)
}

fn fmt_dt(fmt: &str, secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_utc(secs);
    let cs: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        let run = cs[i..].iter().take_while(|&&x| x == cs[i]).count();
        match cs[i] {
            'y' if run >= 4 => out.push_str(&format!("{y:04}")),
            'y' => out.push_str(&format!("{:02}", y.rem_euclid(100))),
            'M' => out.push_str(&format!("{mo:02}")),
            'd' => out.push_str(&format!("{d:02}")),
            'H' => out.push_str(&format!("{h:02}")),
            'm' => out.push_str(&format!("{mi:02}")),
            's' => out.push_str(&format!("{s:02}")),
            c => {
                for _ in 0..run {
                    out.push(c);
                }
            }
        }
        i += run;
    }
    out
}

// Seeded once per thread; random tags regenerate on every preview.
fn rng() -> u64 {
    thread_local! { static STATE: Cell<u64> = const { Cell::new(0) }; }
    STATE.with(|st| {
        let mut x = st.get();
        if x == 0 {
            x = seed_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0x9E37_79B9),
            );
        }
        x = xorshift64(x);
        st.set(x);
        x
    })
}

fn modify(val: String, m: &str) -> Option<String> {
    let (op, arg) = m.split_once(':').unwrap_or((m, ""));
    Some(match op {
        "upper" => val.to_uppercase(),
        "lower" => val.to_lowercase(),
        "title" => change_case(&val, CaseMode::Title),
        "sub" => {
            let (start, len) = match arg.split_once(',') {
                Some((a, b)) => (a.parse::<i64>().ok()?, Some(b.parse::<usize>().ok()?)),
                None => (arg.parse::<i64>().ok()?, None),
            };
            let chars: Vec<char> = val.chars().collect();
            let from = if start < 0 {
                chars.len().saturating_sub(start.unsigned_abs() as usize)
            } else {
                (start as usize).min(chars.len())
            };
            // No LEN = to the end.
            let (from, to) = arith::char_window(from, len.unwrap_or(usize::MAX), chars.len());
            chars[from..to].iter().collect()
        }
        "pad" => {
            // The formatter panics past u16::MAX; wider is a bad argument.
            let w = usize::from(arg.parse::<u16>().ok()?);
            format!("{val:0>w$}")
        }
        "trim" => {
            if arg.is_empty() {
                val.trim().to_string()
            } else {
                val.trim_matches(|c| arg.contains(c)).to_string()
            }
        }
        "replace" => {
            let (old, new) = arg.split_once(',').unwrap_or((arg, ""));
            if old.is_empty() {
                return None;
            }
            val.replace(old, new)
        }
        "split" => {
            // SEP,N — Nth piece (1-based; negative counts from the end);
            // empty SEP splits on whitespace. Out of range gives "".
            let (sep, n) = arg.rsplit_once(',')?;
            let n: i64 = n.parse().ok()?;
            if n == 0 {
                return None;
            }
            let parts: Vec<&str> = if sep.is_empty() {
                val.split_whitespace().collect()
            } else {
                val.split(sep).collect()
            };
            let idx = if n < 0 { parts.len() as i64 + n } else { n - 1 };
            usize::try_from(idx)
                .ok()
                .and_then(|i| parts.get(i))
                .copied()
                .unwrap_or("")
                .to_string()
        }
        "fallback" => {
            if val.is_empty() {
                arg.to_string()
            } else {
                val
            }
        }
        _ if matches!(op.as_bytes().first(), Some(b'+' | b'-' | b'*' | b'/')) => {
            let n: i64 = op[1..].parse().ok()?;
            let v: i64 = val.trim().parse().ok()?;
            let r = match op.as_bytes()[0] {
                b'+' => v.checked_add(n)?,
                b'-' => v.checked_sub(n)?,
                b'*' => v.checked_mul(n)?,
                _ => v.checked_div(n)?,
            };
            // keep leading-zero width
            format!("{r:0w$}", w = val.len())
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use googletest::prelude::*;
    use proptest::prelude::*;
    use rstest::rstest;
    use std::path::Path;

    proptest! {
        #[test]
        fn alpha_counters_roundtrip(n in 1i64..=1_000_000) {
            prop_assert_eq!(arith::alpha_to_num(alpha(n).as_bytes()), Some(n));
        }

        #[test]
        fn alpha_counters_reject_nonletters(s in "[^A-Za-z]+") {
            prop_assert_eq!(arith::alpha_to_num(s.as_bytes()), None);
        }

        #[test]
        fn out_of_range_roman_counters_stay_decimal(
            n in prop_oneof![i64::MIN..=0, 4000i64..=i64::MAX],
        ) {
            prop_assert_eq!(roman(n), n.to_string());
        }

        #[test]
        fn civil_dates_roundtrip(secs in 0i64..4_102_444_800) {
            let (y, m, d, h, mi, s) = civil_utc(secs);
            prop_assert_eq!(epoch_from_civil(y, m, d, h, mi, s), secs);
        }

        #[test]
        fn plain_templates_pass_through(template in "[^<]*") {
            let path = Path::new("a.txt");
            prop_assert_eq!(expand(&template, "a.txt", &ctx(path, "a.txt")), template);
        }

        #[test]
        fn counter_tags_match_wide_arithmetic(start in edge_i64(), step in edge_i64()) {
            let template = format!("<num:{start}:{step}>");
            let v = start as i128 + step as i128 * 4; // ctx index 4
            let expected = if v > i64::MAX as i128 {
                template.clone()
            } else {
                format!("{:03}", v.max(0))
            };
            prop_assert_eq!(x(&template), expected);
        }

        #[test]
        fn rand_tags_stay_in_range(a in edge_i64(), b in edge_i64()) {
            let template = format!("<rand:{a}:{b}>");
            let out = x(&template);
            if b < a {
                prop_assert_eq!(out, template);
            } else {
                let v: i64 = out.parse().unwrap();
                prop_assert!(a <= v && v <= b, "{v} outside {a}..={b}");
            }
        }

        #[test]
        fn sub_takes_the_clamped_char_window(
            stem in "[a-zé ]{1,10}",
            start in edge_i64(),
            len in prop::option::of(edge_usize()),
        ) {
            let chars: Vec<char> = stem.chars().collect();
            let n = chars.len();
            let from = if start < 0 {
                n.saturating_sub(start.unsigned_abs() as usize)
            } else {
                (start as u64).min(n as u64) as usize
            };
            let to = len.map_or(n, |l| (from as u128 + l as u128).min(n as u128) as usize);
            let arg = match len {
                Some(l) => format!("{start},{l}"),
                None => start.to_string(),
            };
            let full = format!("{stem}.txt");
            let path = Path::new("/t/a.txt");
            let out = expand(&format!("<name|sub:{arg}>"), &full, &ctx(path, &full));
            prop_assert_eq!(out, chars[from..to].iter().collect::<String>());
        }

        #[test]
        fn pad_widens_to_the_width_or_passes_through(
            w in prop_oneof![0u64..12, 65_530u64..65_542, any::<u64>()],
        ) {
            let template = format!("<name|pad:{w}>");
            let out = x(&template);
            if w <= u16::MAX as u64 {
                prop_assert_eq!(out.len() as u64, w.max(1));
                prop_assert!(out.ends_with('a'));
            } else {
                prop_assert_eq!(out, template);
            }
        }

        #[test]
        fn offsets_match_wide_arithmetic(
            n in edge_i64(),
            (unit, mult) in prop::sample::select(vec![("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1), ("", 1)]),
        ) {
            let wide = n as i128 * mult as i128;
            let expected = i64::try_from(wide).ok();
            prop_assert_eq!(parse_offset(&format!("{n}{unit}")), expected);
        }

        #[test]
        fn datetime_extraction_never_panics(
            text in prop_oneof![
                "\\PC{0,30}",
                // ASCII mixed with Arabic-Indic and Devanagari digits
                "[0-9\u{0660}-\u{0669}\u{0966}-\u{096F}_ .-]{0,24}",
            ],
        ) {
            let _ = extract_datetime(&text);
        }

        // Catch-all over the tag grammar with boundary arguments.
        #[test]
        fn any_tag_never_panics(
            tag in prop::sample::select(vec![
                "num", "hex", "alpha", "roman", "dirnum", "index", "total", "name",
                "ext", "parent", "path", "subfolder", "size", "csv", "now", "rand",
                "rands", "md5", "exif", "width",
            ]),
            a in tag_arg(),
            b in tag_arg(),
            modifier in prop_oneof![
                Just(String::new()),
                (prop::sample::select(vec!["sub", "pad", "split", "replace", "trim", "fallback"]), tag_arg())
                    .prop_map(|(m, v)| format!("|{m}:{v}")),
                (prop::sample::select(vec!["+", "-", "*", "/"]), edge_i64())
                    .prop_map(|(op, n)| format!("|{op}{n}")),
            ],
        ) {
            let _ = x(&format!("<{tag}:{a}:{b}{modifier}>"));
        }
    }

    fn edge_i64() -> impl Strategy<Value = i64> {
        prop_oneof![
            -20i64..20,
            Just(i64::MAX),
            Just(i64::MIN),
            Just(i64::MAX / 4),
            any::<i64>(),
        ]
    }

    fn edge_usize() -> impl Strategy<Value = usize> {
        prop_oneof![0usize..12, Just(usize::MAX), any::<usize>()]
    }

    fn tag_arg() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-z0-9 ,.-]{0,6}",
            edge_i64().prop_map(|n| n.to_string()),
            edge_usize().prop_map(|n| n.to_string()),
            (edge_i64(), edge_usize()).prop_map(|(a, b)| format!("{a},{b}")),
        ]
    }

    /// Expand against the standard test ctx for "a.txt" (index 4, num 7, pad 3).
    fn x(template: &str) -> String {
        let path = Path::new("/t/a.txt");
        expand(template, "a.txt", &ctx(path, "a.txt"))
    }

    fn ctx<'a>(path: &'a Path, original: &'a str) -> Ctx<'a> {
        Ctx {
            index: 4,
            num: 7,
            pad: 3,
            folder_num: 2,
            total: 0,
            csv: &[],
            path,
            original,
        }
    }

    #[rstest]
    #[case::name_number_extension("<name>_<num>.<ext>", "img.jpg", "img_007.jpg")]
    #[case::original_name("<oname>", "img.jpg", "orig img")]
    #[case::parent_index("<parent>-<index>", "img.jpg", "trip-5")]
    #[case::case_insensitive("<NAME>.<Ext>", "img.jpg", "img.jpg")]
    #[case::unknown("a<unknown>b", "img.jpg", "a<unknown>b")]
    #[case::plain_angle_brackets("2 < 3 > 1", "img.jpg", "2 < 3 > 1")]
    #[case::unterminated("<name", "img.jpg", "<name")]
    fn expands_tags_and_leaves_unknown(
        #[case] template: &str,
        #[case] input: &str,
        #[case] expected: &str,
    ) {
        let path = Path::new("C:/photos/trip/img.jpg");
        let c = ctx(path, "orig img.jpg");
        assert_eq!(expand(template, input, &c), expected);
    }

    // Arithmetic past i64 (or widths past u16) used to panic in debug builds
    // and wrap in release ones; it is now a bad argument. Counter index is 4.
    #[rstest]
    #[case::start_overflows("<num:9223372036854775807:1>", "<num:9223372036854775807:1>")]
    #[case::step_overflows("<num:1:9223372036854775807>", "<num:1:9223372036854775807>")]
    #[case::alpha_start_overflows("<alpha:zzzzzzzzzzzzzzz>", "<alpha:zzzzzzzzzzzzzzz>")]
    #[case::largest_value_fits("<num:9223372036854775803:1>", "9223372036854775807")]
    #[case::huge_negative_clamps_to_zero("<num:-9223372036854775807:-9223372036854775807>", "000")]
    #[case::modifier_add_overflows("<num|+9223372036854775807>", "<num|+9223372036854775807>")]
    #[case::modifier_sub_overflows(
        "<num|-9223372036854775807|-9>",
        "<num|-9223372036854775807|-9>"
    )]
    #[case::modifier_mul_overflows("<num|*9223372036854775807>", "<num|*9223372036854775807>")]
    #[case::offset_multiply_overflows("<now::9223372036854775807d>", "<now::9223372036854775807d>")]
    #[case::offset_add_overflows("<now::9223372036854775807>", "<now::9223372036854775807>")]
    #[case::sub_length_to_usize_max("<name|sub:1,18446744073709551615>", "")]
    #[case::pad_wider_than_the_formatter("<name|pad:65536>", "<name|pad:65536>")]
    #[case::rands_longer_than_u16("<rands:65536>", "<rands:65536>")]
    #[gtest]
    fn bad_numeric_arguments_pass_through(#[case] template: &str, #[case] expected: &str) {
        let path = Path::new("/t/a.txt");
        expect_that!(expand(template, "a.txt", &ctx(path, "a.txt")), eq(expected));
    }

    #[gtest]
    fn edge_widths_and_ranges_still_work() {
        let path = Path::new("/t/a.txt");
        let x = |t: &str| expand(t, "a.txt", &ctx(path, "a.txt"));
        expect_that!(x("<name|pad:65535>").len(), eq(65535));
        expect_that!(x("<rands:65535>").len(), eq(65535));
        // The full i64 span used to overflow computing max - min + 1.
        expect_that!(
            x("<rand:-9223372036854775808:9223372036854775807>").parse::<i64>(),
            ok(anything())
        );
        // A counter start past i64::MAX can't be shown; the tag passes through.
        let big = Ctx {
            num: usize::MAX,
            ..ctx(path, "a.txt")
        };
        expect_that!(expand("<num>", "a.txt", &big), eq("<num>"));
    }

    #[rstest]
    #[case::number("<num>", "a.txt", "007")]
    #[case::start_step("<num:100:10>", "a.txt", "140")]
    #[case::negative_step("<num:10:-2>", "a.txt", "002")]
    #[case::hex("<hex:250:2>", "a.txt", "102")]
    #[case::alpha("<alpha>", "a.txt", "g")]
    #[case::alpha_start("<alpha:aa>", "a.txt", "ae")]
    #[case::roman("<roman:10:10>", "a.txt", "L")]
    #[case::folder_number("<dirnum>", "a.txt", "002")]
    #[case::folder_start_step("<dirnum:0:5>", "a.txt", "005")]
    #[case::index("<index>", "a.txt", "5")]
    fn counter_variants(#[case] template: &str, #[case] input: &str, #[case] expected: &str) {
        let path = Path::new("a.txt");
        let c = ctx(path, "a.txt");
        assert_eq!(expand(template, input, &c), expected);
    }

    #[test]
    fn date_formatting_and_offsets() {
        let (y, m, d, ..) = civil_utc(1_784_016_196); // 2026-07-14 UTC
        assert_eq!((y, m, d), (2026, 7, 14));
        assert_eq!(fmt_dt("yyyy-MM-dd", 1_784_016_196), "2026-07-14");
        assert_eq!(fmt_dt("yy.MM.dd HH-mm-ss", 0), "70.01.01 00-00-00");
        assert_eq!(parse_offset("+1d"), Some(86400));
        assert_eq!(parse_offset("-2h"), Some(-7200));
    }

    #[rstest]
    #[case::upper("<name|upper>", "img file.jpg", "IMG FILE")]
    #[case::title("<name|title>", "img file.jpg", "Img File")]
    #[case::substring("<name|sub:0,3>", "abcdef.jpg", "abc")]
    #[case::negative_substring("<name|sub:-2>", "abcdef.jpg", "ef")]
    #[case::padding("<index|pad:4>", "a.jpg", "0005")]
    #[case::trim("<name|trim:_>", "_ab_.jpg", "ab")]
    #[case::replace("<name|replace: ,_>", "a b.jpg", "a_b")]
    #[case::fallback("<ext|fallback:none>", "noext", "none")]
    #[case::addition("<num|+10>", "a.jpg", "017")]
    #[case::multiplication("<index|*3>", "a.jpg", "15")]
    #[case::unknown("<name|bogus>", "a.jpg", "<name|bogus>")]
    #[case::chain("<name|upper|sub:0,2>", "abcd.jpg", "AB")]
    fn modifiers(#[case] template: &str, #[case] input: &str, #[case] expected: &str) {
        let path = Path::new("a.txt");
        let c = ctx(path, "a.txt");
        assert_eq!(expand(template, input, &c), expected);
    }

    #[rstest]
    #[case::second("<name|split:-,2>", "a-b-c.jpg", "b")]
    #[case::last("<name|split:-,-1>", "a-b-c.jpg", "c")]
    #[case::whitespace("<name|split:,2>", "one  two three.jpg", "two")]
    #[case::out_of_range_fallback("<name|split:-,9|fallback:x>", "a-b.jpg", "x")]
    #[case::zero_index("<name|split:-,0>", "a-b.jpg", "<name|split:-,0>")]
    #[case::missing_index("<name|split:->", "a-b.jpg", "<name|split:->")]
    fn split_modifier(#[case] template: &str, #[case] input: &str, #[case] expected: &str) {
        let path = Path::new("a.txt");
        let c = ctx(path, "a.txt");
        assert_eq!(expand(template, input, &c), expected);
    }

    #[test]
    fn subfolder_and_total_and_dates() {
        let path = Path::new("C:/photos/2024/trip/img.jpg");
        let c = ctx(path, "img.jpg");
        assert_eq!(expand("<subfolder>", "img.jpg", &c), "trip");
        assert_eq!(expand("<subfolder:2>", "img.jpg", &c), "2024");
        assert_eq!(expand("<subfolder:3>", "img.jpg", &c), "photos");
        assert_eq!(expand("<subfolder:0>", "img.jpg", &c), "<subfolder:0>");
        // <total> is literal without batch context (ctx.total == 0)
        assert_eq!(expand("<total>", "img.jpg", &c), "<total>");
        let c42 = Ctx { total: 42, ..c };
        assert_eq!(expand("<total>", "img.jpg", &c42), "42");
        // unix format token
        let secs: i64 = expand("<now:unix>", "img.jpg", &c).parse().unwrap();
        assert!(secs > 1_700_000_000);
    }

    #[test]
    fn csv_tag_by_number_and_header() {
        let path = Path::new("a.txt");
        // no CSV loaded -> literal
        assert_eq!(expand("<csv:1>", "a.txt", &ctx(path, "a.txt")), "<csv:1>");
        let rows: Vec<Vec<String>> = (0..7)
            .map(|r| vec![format!("r{r}c1"), format!("r{r}c2")])
            .collect();
        let c = Ctx {
            csv: &rows,
            ..ctx(path, "a.txt")
        }; // index 4
        assert_eq!(expand("<csv:2>", "a.txt", &c), "r4c2");
        // header lookup: row 0 is the header, data rows shift by one
        let mut rows: Vec<Vec<String>> = vec![vec!["Old".into(), "Title".into()]];
        rows.extend((0..7).map(|r| vec![format!("o{r}"), format!("t{r}")]));
        let c = Ctx {
            csv: &rows,
            ..ctx(path, "a.txt")
        };
        assert_eq!(expand("<csv:title>", "a.txt", &c), "t4");
        // missing column/row gives "" so |fallback applies
        assert_eq!(expand("<csv:9|fallback:x>", "a.txt", &c), "x");
        assert_eq!(expand("<csv:nope>", "a.txt", &c), "<csv:nope>");
    }

    #[rstest]
    #[case::bytes(512, "512 B")]
    #[case::kilobytes(1536, "1.5 KB")]
    #[case::gigabytes(1_500_000_000, "1.4 GB")]
    fn human_sizes(#[case] input: u64, #[case] expected: &str) {
        assert_eq!(human_size(input), expected);
    }

    #[gtest]
    fn extracts_datetimes_from_text() {
        let (y, m, d, h, mi, s) = civil_utc(extract_datetime("IMG_20240501_123005.jpg").unwrap());
        assert_eq!((y, m, d, h, mi, s), (2024, 5, 1, 12, 30, 5));
        let (y, m, d, h, ..) = civil_utc(extract_datetime("trip 2024-05-01").unwrap());
        assert_eq!((y, m, d, h), (2024, 5, 1, 0));
        expect_that!(extract_datetime("no date here 123"), none());
        assert!(
            extract_datetime("9999 99 99").is_none(),
            "month/day ranges checked"
        );
        // epoch_from_civil is the inverse of civil_utc
        assert_eq!(
            civil_utc(epoch_from_civil(1999, 12, 31, 23, 59, 58)),
            (1999, 12, 31, 23, 59, 58)
        );
    }

    #[rstest]
    #[case::timestamp("2024:05:01 10:30:00", "2024-05-01 10-30-00")]
    #[case::invalid_characters("a<b>c?d*e", "abcde")]
    #[case::padding("  padded  ", "padded")]
    fn sanitizes_metadata_values(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn metadata_tags() {
        let dir = std::env::temp_dir().join(format!("iron_tagmeta_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("data.txt");
        fs::write(&p, "hello").unwrap();
        let c = ctx(&p, "data.txt");
        // FileType comes from the extension, so it works for any file.
        assert_eq!(expand("<exif:FileType>", "data.txt", &c), "TXT");
        // A tag the file lacks resolves to "" so |fallback applies.
        assert_eq!(
            expand("<artist|fallback:unknown>", "data.txt", &c),
            "unknown"
        );
    }

    #[gtest]
    fn file_tags() {
        let dir = std::env::temp_dir().join(format!("iron_tags_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("data.bin");
        fs::write(&p, b"hello world").unwrap();
        let c = ctx(&p, "data.bin");
        assert_eq!(expand("<size>", "data.bin", &c), "11");
        assert_eq!(expand("<size:kb>", "data.bin", &c), "0");
        // hashes of "hello world" are known constants
        assert_eq!(expand("<crc32>", "data.bin", &c), "0d4a1185");
        assert_eq!(
            expand("<md5>", "data.bin", &c),
            "5eb63bbbe01eeed093cb22bb8f5acdc3"
        );
        assert_eq!(
            expand("<sha1>", "data.bin", &c),
            "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"
        );
        assert_eq!(expand("<size:h>", "data.bin", &c), "11 B");
        let modified = expand("<modified>", "data.bin", &c);
        expect_that!(modified.len(), eq(10), "yyyy-MM-dd: {modified}");
        let r: i64 = expand("<rand:5:5>", "data.bin", &c).parse().unwrap();
        assert_eq!(r, 5);
        expect_that!(expand("<rands:6>", "data.bin", &c).len(), eq(6));
    }

    // -- mutation-testing gap fills (2026-08-30 baseline) --

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iron_tagx_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn original_extension_and_absolute_path_tags() {
        let path = Path::new("C:/photos/trip/img.jpg");
        let c = ctx(path, "orig scan.tiff");
        assert_eq!(expand("<oext>", "img.jpg", &c), "tiff");
        // <path> is the whole parent folder, <parent> only its last component.
        let full = expand("<path>", "img.jpg", &c);
        assert!(full.ends_with("trip"), "{full}");
        assert!(
            full.len() > expand("<parent>", "img.jpg", &c).len(),
            "{full}"
        );
    }

    #[test]
    fn size_units_shift_down_not_up() {
        let dir = scratch("size");
        let p = dir.join("blob.bin");
        fs::write(&p, vec![0u8; 3000]).unwrap();
        let c = ctx(&p, "blob.bin");
        assert_eq!(expand("<size>", "blob.bin", &c), "3000");
        // Each unit divides; a left shift would give a number far larger than
        // the file, and a deleted arm would leave the tag literal.
        assert_eq!(expand("<size:kb>", "blob.bin", &c), "2");
        assert_eq!(expand("<size:mb>", "blob.bin", &c), "0");
        assert_eq!(expand("<size:gb>", "blob.bin", &c), "0");
        assert_eq!(expand("<size:tb>", "blob.bin", &c), "0");
        assert_eq!(expand("<size:zz>", "blob.bin", &c), "<size:zz>");
    }

    #[rstest]
    #[case::bytes(512, "512 B")]
    #[case::kilobytes(1536, "1.5 KB")]
    #[case::megabytes(3 << 20, "3.0 MB")]
    #[case::gigabytes(5 << 30, "5.0 GB")]
    #[case::petabytes_clamp_to_terabytes(1 << 50, "1024.0 TB")]
    fn human_size_scales_and_stops_at_the_last_unit(#[case] input: u64, #[case] expected: &str) {
        assert_eq!(human_size(input), expected);
    }

    #[test]
    fn numeric_counters_reject_letter_starts() {
        let path = Path::new("a.txt");
        let c = ctx(path, "a.txt");
        // Letters are only a valid START for <alpha>.
        assert_eq!(expand("<num:abc>", "a.txt", &c), "<num:abc>");
        assert_ne!(expand("<alpha:abc>", "a.txt", &c), "<alpha:abc>");
    }

    #[test]
    fn alpha_clamps_values_below_one() {
        assert_eq!(alpha(0), "a");
        assert_eq!(alpha(-5), "a");
        assert_eq!(alpha(1), "a");
        assert_eq!(alpha(27), "aa");
    }

    #[test]
    fn parse_offset_reads_every_unit_suffix() {
        assert_eq!(parse_offset("+3d"), Some(3 * 86_400));
        assert_eq!(parse_offset("-12h"), Some(-12 * 3600));
        assert_eq!(parse_offset("90m"), Some(5400));
        assert_eq!(parse_offset("45s"), Some(45));
        assert_eq!(parse_offset("100"), Some(100));
        assert_eq!(parse_offset("3x"), None);
    }

    #[test]
    fn civil_utc_handles_the_era_boundary_leap_day() {
        // 2000-02-29 sits on doe == 146096, the last day of a 400-year era,
        // where the era-correction terms are the only thing keeping it right.
        assert_eq!(civil_utc(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(civil_utc(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_utc(951_868_799), (2000, 2, 29, 23, 59, 59));
        assert_eq!(civil_utc(951_868_800), (2000, 3, 1, 0, 0, 0));

        // 2100 is a century year that is NOT a leap year, and the day after
        // its February is where the era-correction terms actually change the
        // answer. The roundtrip proptest above stops at 2100-01-01, so without
        // this the term is only checked when a random draw happens to land on
        // one of the ~0.8% of days that discriminate.
        assert_eq!(civil_utc(4_107_456_000), (2100, 2, 28, 0, 0, 0));
        assert_eq!(civil_utc(4_107_542_400), (2100, 3, 1, 0, 0, 0));
        assert_eq!(epoch_from_civil(2100, 3, 1, 0, 0, 0), 4_107_542_400);
    }

    #[test]
    fn created_accessed_and_modified_are_separate_clocks() {
        use crate::batch::{apply_touch, parse_touch};
        let dir = scratch("times");
        let p = dir.join("dated.txt");
        fs::write(&p, "x").unwrap();
        // macOS can lower creation time with mtime; seed an earlier year first.
        for value in ["modified=2000-05-06 07:08", "modified=2001-05-06 07:08"] {
            let spec = parse_touch(value).unwrap();
            let (n, errs) = apply_touch(std::slice::from_ref(&p), &spec);
            assert_eq!((n, errs.len()), (1, 0), "{errs:?}");
        }

        let c = ctx(&p, "dated.txt");
        assert_eq!(
            expand("<modified:yyyy-MM-dd>", "dated.txt", &c),
            "2001-05-06"
        );
        // A positive offset moves forward, and by days.
        assert_eq!(
            expand("<modified:yyyy-MM-dd:+3d>", "dated.txt", &c),
            "2001-05-09"
        );
        for tag in ["created", "accessed", "now"] {
            let y = expand(&format!("<{tag}:yyyy>"), "dated.txt", &c);
            assert_ne!(y, "2001", "<{tag}> fell through to <modified>");
            assert!(y.starts_with("20") && y.len() == 4, "<{tag}> gave {y}");
        }
    }

    #[test]
    fn csv_columns_are_one_based_and_named() {
        let rows = vec![
            vec!["id".to_string(), "who".to_string()],
            vec!["r1c1".to_string(), "alice".to_string()],
            vec!["r2c1".to_string(), "bob".to_string()],
            vec!["r3c1".to_string(), "carol".to_string()],
        ];
        let path = Path::new("a.txt");
        let mut c = ctx(path, "a.txt");
        c.csv = &rows;
        c.index = 2;
        // Numeric columns index the row at ctx.index directly.
        assert_eq!(expand("<csv:1>", "a.txt", &c), "r2c1");
        assert_eq!(expand("<csv:2>", "a.txt", &c), "bob");
        // Column 0 is not a column; a 1-based index must never underflow.
        assert_eq!(expand("<csv:0>", "a.txt", &c), "<csv:0>");
        // Named columns skip the header row, so they land one row later.
        assert_eq!(expand("<csv:who>", "a.txt", &c), "carol");
        assert_eq!(expand("<csv:nope>", "a.txt", &c), "<csv:nope>");
    }

    #[test]
    fn rand_spans_its_whole_range_and_rejects_inverted_ones() {
        let path = Path::new("a.txt");
        let c = ctx(path, "a.txt");
        assert_eq!(expand("<rand:9:2>", "a.txt", &c), "<rand:9:2>");
        let mut seen_high = false;
        for _ in 0..200 {
            let v: i64 = expand("<rand:100:109>", "a.txt", &c).parse().unwrap();
            assert!((100..=109).contains(&v), "out of range: {v}");
            seen_high |= v > 101;
        }
        // A narrowed span (max/min instead of max-min) never reaches the top.
        assert!(seen_high, "never drew above 101 in 200 tries");
    }

    #[gtest]
    fn rands_is_not_a_constant_stream() {
        let path = Path::new("a.txt");
        let c = ctx(path, "a.txt");
        let s = expand("<rands:32>", "a.txt", &c);
        expect_that!(s.len(), eq(32));
        // A stuck or saturating generator repeats one character forever.
        let distinct: std::collections::HashSet<char> = s.chars().collect();
        assert!(
            distinct.len() >= 5,
            "only {} distinct in {s}",
            distinct.len()
        );
    }

    #[rstest]
    #[case::lower("<name|lower>", "MiXeD.TXT", "mixed")]
    #[case::upper("<name|upper>", "MiXeD.TXT", "MIXED")]
    #[case::add("<num|+3>", "a.txt", "010")]
    #[case::subtract("<num|-3>", "a.txt", "004")]
    #[case::multiply("<num|*3>", "a.txt", "021")]
    #[case::divide("<num|/3>", "a.txt", "002")]
    #[case::unknown("<num|q5>", "a.txt", "<num|q5>")]
    fn arithmetic_and_case_mods(
        #[case] template: &str,
        #[case] input: &str,
        #[case] expected: &str,
    ) {
        let path = Path::new("a.txt");
        let c = ctx(path, "a.txt");
        assert_eq!(expand(template, input, &c), expected);
    }

    #[gtest]
    fn metadata_aliases_read_values_and_fall_back() {
        let dir = scratch("meta");
        let p = dir.join("tiny.png");
        // Header-only PNG: imagesize reads IHDR without needing pixel data.
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&7u32.to_be_bytes()); // width
        png.extend_from_slice(&3u32.to_be_bytes()); // height
        png.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        fs::write(&p, &png).unwrap();
        let c = ctx(&p, "tiny.png");
        assert_eq!(expand("<width>x<height>", "tiny.png", &c), "7x3");

        // Every other alias resolves to "" on a file that lacks it, so
        // |fallback applies rather than the tag being left literal.
        for tag in [
            "datetaken",
            "artist",
            "album",
            "track",
            "title",
            "duration",
            "author",
            "lat",
            "lon",
        ] {
            let out = expand(&format!("<{tag}|fallback:none>"), "tiny.png", &c);
            expect_that!(out, eq("none"), "<{tag}> did not resolve");
        }
    }

    #[rstest]
    // A full stamp round-trips through the same civil-date maths.
    #[case::full_stamp("IMG_20240506_070809.jpg", Some(epoch_from_civil(2024, 5, 6, 7, 8, 9)))]
    #[case::separators("2024-05-06 07.08.09", Some(epoch_from_civil(2024, 5, 6, 7, 8, 9)))]
    // Date only defaults the clock to midnight.
    #[case::date_only("scan_20240506.pdf", Some(epoch_from_civil(2024, 5, 6, 0, 0, 0)))]
    // Every bound is inclusive on the low side and exclusive above it.
    #[case::last_legal(
        "20241231_235959.jpg",
        Some(epoch_from_civil(2024, 12, 31, 23, 59, 59))
    )]
    #[case::first_legal("20240101_000000.jpg", Some(epoch_from_civil(2024, 1, 1, 0, 0, 0)))]
    #[case::month_13("20241301_000000.jpg", None)]
    #[case::day_32("20240132_000000.jpg", None)]
    #[case::hour_24("20240506_240000.jpg", None)]
    #[case::minute_60("20240506_006000.jpg", None)]
    #[case::second_60("20240506_000060.jpg", None)]
    #[case::year_below_1900("18991231_000000.jpg", None)]
    #[case::year_above_2999("30000101_000000.jpg", None)]
    #[case::no_digits("no digits here", None)]
    // Non-ASCII digits used to match Unicode \d and then panic in parse.
    #[case::arabic_indic_digits(
        "IMG_\u{0662}\u{0660}\u{0662}\u{0664}\u{0660}\u{0665}\u{0660}\u{0661}.jpg",
        None
    )]
    #[case::ascii_after_other_digits(
        "\u{0662}\u{0660}\u{0662}\u{0664} 20240501.jpg",
        Some(epoch_from_civil(2024, 5, 1, 0, 0, 0))
    )]
    #[gtest]
    fn datetime_extraction_validates_every_field(#[case] text: &str, #[case] want: Option<i64>) {
        expect_that!(extract_datetime(text), eq(want));
    }

    #[test]
    fn dt_string_formats_a_filesystem_time() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(989_132_889);
        assert_eq!(dt_string(t), "2001-05-06 07:08");
        assert_eq!(dt_string(std::time::UNIX_EPOCH), "1970-01-01 00:00");
    }

    #[test]
    fn the_random_seed_is_never_zero() {
        // A zero seed freezes the generator for the whole thread, so every
        // clock reading has to come out non-zero -- even ones included.
        for nanos in [0, 1, 2, 3, 1024, u64::MAX - 1, u64::MAX] {
            assert_ne!(seed_from(nanos), 0, "seed from {nanos}");
        }
        // Odd readings pass through untouched; even ones gain the low bit and
        // nothing else, so no entropy above bit 0 is disturbed.
        assert_eq!(seed_from(0), 1);
        assert_eq!(seed_from(2), 3);
        assert_eq!(seed_from(7), 7);
        assert_eq!(seed_from(8), 9);
    }

    #[test]
    fn xorshift64_reproduces_the_published_sequence() {
        // Known-answer test for Marsaglia's xorshift64 with the 13/7/17
        // triple, from a seed of 1. Pins the shifts and the xors without
        // freezing anything this crate chose.
        let mut x = 1u64;
        let seq: Vec<u64> = (0..3)
            .map(|_| {
                x = xorshift64(x);
                x
            })
            .collect();
        assert_eq!(
            seq,
            [0x4082_2041, 0x1000_4106_0C01_1441, 0x9B1E_842F_6E86_2629]
        );
        // Zero is the one state the generator cannot leave -- the reason
        // seed_from exists at all.
        assert_eq!(xorshift64(0), 0);
    }
}
