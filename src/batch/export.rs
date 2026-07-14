use super::{ExecResult, Op, PlanItem};
use crate::engine::name_of;
use std::{fs, io, path::Path};

// ───────────────────────── preview export

pub(super) fn export_rows(
    path: &Path,
    headers: &[&str],
    rows: Vec<(Vec<String>, String)>,
) -> io::Result<()> {
    use crate::presets::{csv_field, json_str};
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let has_rows = !rows.is_empty();
    let body = match ext.as_str() {
        "csv" => {
            let mut out = headers.join(",") + "\n";
            for (values, _) in &rows {
                out.push_str(
                    &values
                        .iter()
                        .map(|v| csv_field(v))
                        .collect::<Vec<_>>()
                        .join(","),
                );
                out.push('\n');
            }
            out
        }
        "json" => {
            let objects: Vec<String> = rows
                .iter()
                .map(|(values, _)| {
                    let fields = headers
                        .iter()
                        .zip(values)
                        .map(|(key, value)| format!("\"{key}\": {}", json_str(value)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("  {{{fields}}}")
                })
                .collect();
            format!("[\n{}\n]\n", objects.join(",\n"))
        }
        _ => {
            rows.into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
                .join("\n")
                + if has_rows { "\n" } else { "" }
        }
    };
    fs::write(path, body)
}

/// Write the preview to `path`; the extension picks the format:
/// .csv, .json, or plain text ("old -> target") for anything else.
pub fn export_preview(items: &[PlanItem], path: &Path) -> io::Result<()> {
    let rows = items
        .iter()
        .map(|it| {
            let status = match (&it.issue, it.changed) {
                (Some(e), _) => e.clone(),
                (None, true) => "ok".to_string(),
                (None, false) => "unchanged".to_string(),
            };
            (
                vec![
                    name_of(&it.from),
                    it.new_name.clone(),
                    it.target.display().to_string(),
                    status.clone(),
                ],
                format!(
                    "{}  ->  {}   [{status}]",
                    it.from.display(),
                    it.target.display()
                ),
            )
        })
        .collect();
    export_rows(path, &["old", "new", "target", "status"], rows)
}

/// Write an execution result log to `path` (.csv, .json, or text).
pub fn export_results(res: &ExecResult, path: &Path) -> io::Result<()> {
    let row = |op: &Op, result: String| {
        (
            vec![
                op.from.display().to_string(),
                op.to.display().to_string(),
                result.clone(),
            ],
            format!(
                "{}  ->  {}   [{result}]",
                op.from.display(),
                op.to.display()
            ),
        )
    };
    let rows = res
        .renamed
        .iter()
        .map(|op| row(op, "done".to_string()))
        .chain(
            res.failed
                .iter()
                .map(|(op, e)| row(op, format!("failed: {e}"))),
        )
        .collect();
    export_rows(path, &["from", "to", "result"], rows)
}
