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
// Location    <parent> <path>     folder name / full folder path
// File        <size[:kb|mb]> <crc32>
// Dates (UTC) <now|created|modified[:FMT[:OFFSET]]>
//             FMT tokens yyyy yy MM dd HH mm ss (default yyyy-MM-dd),
//             OFFSET like +3d -12h +30m
// Random      <rand[:MIN[:MAX]]> <rands[:LEN]>
// Modifiers   |upper |lower |title |sub:START[,LEN] |pad:N |trim[:CHARS]
//             |replace:OLD[,NEW] |fallback:TEXT |+N |-N |*N |/N

use crate::engine::{Ctx, change_case, split_ext, CaseMode};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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
        "num" => format!("{:0w$}", counter(&args, ctx.index, ctx.num, false)?, w = ctx.pad),
        "hex" => format!("{:0w$x}", counter(&args, ctx.index, ctx.num, false)?, w = ctx.pad),
        "roman" => roman(counter(&args, ctx.index, ctx.num, false)?),
        "alpha" => alpha(counter(&args, ctx.index, ctx.num, true)?),
        "dirnum" => {
            format!("{:0w$}", counter(&args, ctx.folder_num - 1, ctx.folder_num, false)?, w = ctx.pad)
        }
        "parent" => abs_parent(ctx)?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "path" => abs_parent(ctx)?.display().to_string(),
        "size" => {
            let bytes = fs::metadata(ctx.path).ok()?.len();
            match args.first().copied().unwrap_or("") {
                "" | "b" => bytes.to_string(),
                "kb" => (bytes / 1024).to_string(),
                "mb" => (bytes / (1024 * 1024)).to_string(),
                _ => return None,
            }
        }
        "crc32" => crc32_cached(ctx)?,
        "now" | "created" | "modified" => {
            let t = match tag.as_str() {
                "now" => SystemTime::now(),
                "created" => fs::metadata(ctx.path).ok()?.created().ok()?,
                _ => fs::metadata(ctx.path).ok()?.modified().ok()?,
            };
            let mut secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
            if let Some(off) = args.get(1) {
                secs += parse_offset(off)?;
            }
            let fmt = args.first().filter(|s| !s.is_empty()).copied().unwrap_or("yyyy-MM-dd");
            fmt_dt(fmt, secs)
        }
        "rand" => {
            let min: i64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let max: i64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(9999);
            if max < min {
                return None;
            }
            (min + (rng() % (max - min + 1) as u64) as i64).to_string()
        }
        "rands" => {
            let len: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(8);
            const CS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
            (0..len).map(|_| CS[(rng() % CS.len() as u64) as usize] as char).collect()
        }
        _ => return None,
    };
    for m in parts {
        val = modify(val, m)?;
    }
    Some(val)
}

// Counter value: no args -> the batch counter; with args -> START + STEP*i.
// For <alpha>, START may be letters ("aa" = 27).
fn counter(args: &[&str], i: usize, default: usize, letters: bool) -> Option<i64> {
    if args.is_empty() || args[0].is_empty() {
        return Some(default as i64);
    }
    let start: i64 = match args[0].parse() {
        Ok(n) => n,
        Err(_) if letters => alpha_to_num(args[0])?,
        Err(_) => return None,
    };
    let step: i64 = match args.get(1) {
        Some(s) => s.parse().ok()?,
        None => 1,
    };
    Some((start + step * i as i64).max(0))
}

fn alpha(mut n: i64) -> String {
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

fn alpha_to_num(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let mut n = 0i64;
    for c in s.chars() {
        let c = c.to_ascii_lowercase();
        if !c.is_ascii_lowercase() {
            return None;
        }
        n = n * 26 + (c as i64 - 'a' as i64 + 1);
    }
    Some(n)
}

fn roman(mut n: i64) -> String {
    if !(1..=3999).contains(&n) {
        return n.to_string();
    }
    const STEPS: [(i64, &str); 13] = [
        (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"),
        (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
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
    std::path::absolute(ctx.path).ok()?.parent().map(PathBuf::from)
}

fn crc32_cached(ctx: &Ctx) -> Option<String> {
    thread_local! {
        static CACHE: RefCell<HashMap<PathBuf, u32>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        if let Some(&v) = c.borrow().get(ctx.path) {
            return Some(format!("{v:08x}"));
        }
        let data = fs::read(ctx.path).ok()?;
        let mut crc = !0u32;
        for &b in &data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        let v = !crc;
        c.borrow_mut().insert(ctx.path.to_path_buf(), v);
        Some(format!("{v:08x}"))
    })
}

fn parse_offset(s: &str) -> Option<i64> {
    let (num, mult) = match s.chars().last()? {
        'd' => (&s[..s.len() - 1], 86400),
        'h' => (&s[..s.len() - 1], 3600),
        'm' => (&s[..s.len() - 1], 60),
        's' => (&s[..s.len() - 1], 1),
        _ => (s, 1),
    };
    num.parse::<i64>().ok().map(|n| n * mult)
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

/// "yyyy-MM-dd HH:mm" for a filesystem time; used by the item-details panel.
pub fn dt_string(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
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

// xorshift64, seeded once per thread; random tags regenerate on every preview.
fn rng() -> u64 {
    thread_local! { static STATE: Cell<u64> = const { Cell::new(0) }; }
    STATE.with(|st| {
        let mut x = st.get();
        if x == 0 {
            x = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9)
                | 1;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
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
            let to = match len {
                Some(l) => (from + l).min(chars.len()),
                None => chars.len(),
            };
            chars[from..to].iter().collect()
        }
        "pad" => {
            let w: usize = arg.parse().ok()?;
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
        "fallback" => {
            if val.is_empty() { arg.to_string() } else { val }
        }
        _ if matches!(op.as_bytes().first(), Some(b'+' | b'-' | b'*' | b'/')) => {
            let n: i64 = op[1..].parse().ok()?;
            let v: i64 = val.trim().parse().ok()?;
            let r = match op.as_bytes()[0] {
                b'+' => v + n,
                b'-' => v - n,
                b'*' => v * n,
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
    use std::path::Path;

    fn ctx<'a>(path: &'a Path, original: &'a str) -> Ctx<'a> {
        Ctx { index: 4, num: 7, pad: 3, folder_num: 2, path, original }
    }

    #[test]
    fn expands_tags_and_leaves_unknown() {
        let path = Path::new("C:/photos/trip/img.jpg");
        let c = ctx(path, "orig img.jpg");
        assert_eq!(expand("<name>_<num>.<ext>", "img.jpg", &c), "img_007.jpg");
        assert_eq!(expand("<oname>", "img.jpg", &c), "orig img");
        assert_eq!(expand("<parent>-<index>", "img.jpg", &c), "trip-5");
        assert_eq!(expand("<NAME>.<Ext>", "img.jpg", &c), "img.jpg");
        assert_eq!(expand("a<unknown>b", "img.jpg", &c), "a<unknown>b");
        assert_eq!(expand("2 < 3 > 1", "img.jpg", &c), "2 < 3 > 1");
        assert_eq!(expand("<name", "img.jpg", &c), "<name");
    }

    #[test]
    fn counter_variants() {
        let path = Path::new("a.txt");
        let c = ctx(path, "a.txt"); // index 4, num 7, pad 3, folder_num 2
        assert_eq!(expand("<num>", "a.txt", &c), "007");
        assert_eq!(expand("<num:100:10>", "a.txt", &c), "140");
        assert_eq!(expand("<num:10:-2>", "a.txt", &c), "002");
        assert_eq!(expand("<hex:250:2>", "a.txt", &c), "102"); // 258 = 0x102
        assert_eq!(expand("<alpha>", "a.txt", &c), "g"); // 7
        assert_eq!(expand("<alpha:aa>", "a.txt", &c), "ae"); // 27 + 4
        assert_eq!(expand("<roman:10:10>", "a.txt", &c), "L"); // 50
        assert_eq!(expand("<dirnum>", "a.txt", &c), "002");
        assert_eq!(expand("<dirnum:0:5>", "a.txt", &c), "005");
        assert_eq!(expand("<index>", "a.txt", &c), "5");
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

    #[test]
    fn modifiers() {
        let path = Path::new("a.txt");
        let c = ctx(path, "a.txt");
        assert_eq!(expand("<name|upper>", "img file.jpg", &c), "IMG FILE");
        assert_eq!(expand("<name|title>", "img file.jpg", &c), "Img File");
        assert_eq!(expand("<name|sub:0,3>", "abcdef.jpg", &c), "abc");
        assert_eq!(expand("<name|sub:-2>", "abcdef.jpg", &c), "ef");
        assert_eq!(expand("<index|pad:4>", "a.jpg", &c), "0005");
        assert_eq!(expand("<name|trim:_>", "_ab_.jpg", &c), "ab");
        assert_eq!(expand("<name|replace: ,_>", "a b.jpg", &c), "a_b");
        assert_eq!(expand("<ext|fallback:none>", "noext", &c), "none");
        assert_eq!(expand("<num|+10>", "a.jpg", &c), "017");
        assert_eq!(expand("<index|*3>", "a.jpg", &c), "15");
        // bad modifier leaves the whole tag literal
        assert_eq!(expand("<name|bogus>", "a.jpg", &c), "<name|bogus>");
        assert_eq!(expand("<name|upper|sub:0,2>", "abcd.jpg", &c), "AB");
    }

    #[test]
    fn file_tags() {
        let dir = std::env::temp_dir().join(format!("iron_tags_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("data.bin");
        fs::write(&p, b"hello world").unwrap();
        let c = ctx(&p, "data.bin");
        assert_eq!(expand("<size>", "data.bin", &c), "11");
        assert_eq!(expand("<size:kb>", "data.bin", &c), "0");
        // CRC32 of "hello world" is a known constant
        assert_eq!(expand("<crc32>", "data.bin", &c), "0d4a1185");
        let modified = expand("<modified>", "data.bin", &c);
        assert_eq!(modified.len(), 10, "yyyy-MM-dd: {modified}");
        let r: i64 = expand("<rand:5:5>", "data.bin", &c).parse().unwrap();
        assert_eq!(r, 5);
        assert_eq!(expand("<rands:6>", "data.bin", &c).len(), 6);
    }
}
