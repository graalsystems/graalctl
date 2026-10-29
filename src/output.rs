//! Rendering of API values as aligned tables or JSON.

use serde_json::Value;
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Aligned columns (default).
    Table,
    /// Pretty-printed JSON, as returned by the API.
    Json,
    /// One identifier per line, for scripting.
    Id,
}

/// A table column: header and field path. A path may contain dots for nested
/// fields (`options.type`) and `|` for alternatives (`key|id`).
pub type Column = (&'static str, &'static str);

pub fn lookup<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('|').find_map(|alt| {
        let mut v = value;
        for part in alt.split('.') {
            v = v.get(part)?;
        }
        (!v.is_null()).then_some(v)
    })
}

pub fn cell(value: Option<&Value>) -> String {
    let s = match value {
        None | Some(Value::Null) => return "-".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) if o.is_empty() => return "-".into(),
        Some(Value::Array(a)) if a.is_empty() => return "-".into(),
        Some(Value::Array(a)) if a.iter().all(Value::is_string) => a
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(","),
        Some(other) => other.to_string(),
    };
    // Keep one row per item.
    let s = s.replace(['\n', '\r', '\t'], " ");
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(59).collect::<String>())
    } else {
        s
    }
}

pub fn render_table(rows: &[Value], columns: &[Column]) -> String {
    let mut grid: Vec<Vec<String>> = vec![columns.iter().map(|(h, _)| h.to_string()).collect()];
    for row in rows {
        grid.push(columns.iter().map(|(_, p)| cell(lookup(row, p))).collect());
    }
    let widths: Vec<usize> = (0..columns.len())
        .map(|i| grid.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &grid {
        let mut line = String::new();
        for (i, c) in row.iter().enumerate() {
            if i + 1 == row.len() {
                line.push_str(c);
            } else {
                line.push_str(c);
                line.push_str(&" ".repeat(widths[i] - c.chars().count() + 3));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Prints a list of items.
pub fn list(rows: &[Value], columns: &[Column], format: Format) {
    let mut out = std::io::stdout().lock();
    let _ = match format {
        Format::Json => writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&Value::Array(rows.to_vec())).unwrap_or_default()
        ),
        Format::Id => {
            for r in rows {
                let _ = writeln!(
                    out,
                    "{}",
                    cell(lookup(r, columns.first().map(|c| c.1).unwrap_or("id")))
                );
            }
            Ok(())
        }
        Format::Table if rows.is_empty() => writeln!(std::io::stderr(), "No resources found."),
        Format::Table => write!(out, "{}", render_table(rows, columns)),
    };
}

/// Prints a single item: JSON for `json` and `table` (objects are rarely flat
/// enough for columns), the identifier for `id`.
pub fn single(value: &Value, format: Format) {
    match format {
        Format::Id => println!("{}", cell(lookup(value, "id"))),
        _ => println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_aligned_columns() {
        let rows = vec![
            json!({"id": "p1", "name": "Sales", "options": {"type": "spark"}}),
            json!({"id": "project-2", "name": null, "labels": ["a", "b"]}),
        ];
        let t = render_table(
            &rows,
            &[
                ("ID", "id"),
                ("NAME", "name"),
                ("TYPE", "options.type"),
                ("LABELS", "labels"),
            ],
        );
        assert_eq!(
            t,
            "ID          NAME    TYPE    LABELS\n\
             p1          Sales   spark   -\n\
             project-2   -       -       a,b\n"
        );
    }

    #[test]
    fn lookup_supports_alternatives() {
        let v = json!({"key": null, "id": "x"});
        assert_eq!(lookup(&v, "key|id"), Some(&json!("x")));
        assert_eq!(cell(lookup(&v, "missing")), "-");
    }
}
