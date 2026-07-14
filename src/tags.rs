// One tag parser shared by every text rule (Pattern, Insert, Replace/Regex
// replacements) and, later, destination paths. Unknown tags pass through
// unchanged so plain '<' and '>' in names stay safe.
//
// Tags (case-insensitive):
//   <name>    stem of the current name
//   <ext>     extension without the dot
//   <num>     counter, zero-padded to the batch pad width
//   <index>   1-based position in the list
//   <parent>  name of the containing folder

use crate::engine::{Ctx, split_ext};

pub fn expand(template: &str, full_name: &str, ctx: &Ctx) -> String {
    let (stem, ext) = split_ext(full_name);
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
        let val = match after[..close].to_ascii_lowercase().as_str() {
            "name" => Some(stem.to_string()),
            "ext" => Some(ext.to_string()),
            "num" => Some(format!("{:0w$}", ctx.num, w = ctx.pad)),
            "index" => Some((ctx.index + 1).to_string()),
            // Absolutize so relative paths like "img.jpg" still have a parent.
            "parent" => Some(
                std::path::absolute(ctx.path)
                    .ok()
                    .as_deref()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            _ => None,
        };
        match val {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn expands_tags_and_leaves_unknown() {
        let path = Path::new("C:/photos/trip/img.jpg");
        let ctx = Ctx { index: 4, num: 7, pad: 3, path, original: "img.jpg" };
        assert_eq!(expand("<name>_<num>.<ext>", "img.jpg", &ctx), "img_007.jpg");
        assert_eq!(expand("<parent>-<index>", "img.jpg", &ctx), "trip-5");
        assert_eq!(expand("<NAME>.<Ext>", "img.jpg", &ctx), "img.jpg");
        assert_eq!(expand("a<unknown>b", "img.jpg", &ctx), "a<unknown>b");
        assert_eq!(expand("2 < 3 > 1", "img.jpg", &ctx), "2 < 3 > 1");
        assert_eq!(expand("<name", "img.jpg", &ctx), "<name");
    }
}
