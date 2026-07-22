//! Plain column-aligned tables for human output. `--json` bypasses all of this and
//! prints the wire type verbatim.

/// Render rows as space-padded columns, two spaces between columns, no trailing
/// padding on the last column. Empty cells render as `-` so columns stay scannable.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let cols = headers.len();
    let cell = |row: &[String], i: usize| -> String {
        let raw = row.get(i).map(String::as_str).unwrap_or("");
        if raw.is_empty() {
            "-".to_string()
        } else {
            raw.to_string()
        }
    };
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, w) in widths.iter_mut().enumerate() {
            *w = (*w).max(cell(row, i).chars().count());
        }
    }
    let mut out = String::new();
    let render = |out: &mut String, cells: &[String]| {
        for (i, c) in cells.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            if i + 1 == cols {
                out.push_str(c);
            } else {
                out.push_str(c);
                for _ in c.chars().count()..widths[i] {
                    out.push(' ');
                }
            }
        }
        out.push('\n');
    };
    render(
        &mut out,
        &headers.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
    );
    for row in rows {
        let cells: Vec<String> = (0..cols).map(|i| cell(row, i)).collect();
        render(&mut out, &cells);
    }
    out
}

/// `34855082762` → `32.5 GiB`; keeps small numbers readable too.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Compact decimal token count for dense fleet tables: `128400` → `128K`.
pub fn token_count(tokens: u64) -> String {
    const UNITS: [&str; 5] = ["", "K", "M", "B", "T"];
    let mut value = tokens as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => tokens.to_string(),
        _ if value < 10.0 => format!("{value:.1}{}", UNITS[unit]),
        _ => format!("{value:.0}{}", UNITS[unit]),
    }
}

/// Shorten `sha256:8e6e9ec2c685…` to the familiar 12-hex-char id.
pub fn short_id(id: &str) -> String {
    id.strip_prefix("sha256:")
        .unwrap_or(id)
        .chars()
        .take(12)
        .collect()
}

/// A usage window as `pct%` (or `-` when absent).
pub fn pct(window: &Option<wire::ClaudeUsageWindow>) -> String {
    window
        .as_ref()
        .map(|w| format!("{:.0}%", w.pct))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_aligns_columns_and_dashes_empty_cells() {
        let out = table(
            &["ID", "STATE"],
            &[
                vec!["w-long-name".into(), "working".into()],
                vec!["w2".into(), "".into()],
            ],
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "ID           STATE");
        assert_eq!(lines[1], "w-long-name  working");
        assert_eq!(lines[2], "w2           -");
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(34855082762), "32.5 GiB");
    }

    #[test]
    fn token_counts_are_compact() {
        assert_eq!(token_count(999), "999");
        assert_eq!(token_count(1_200), "1.2K");
        assert_eq!(token_count(128_400), "128K");
        assert_eq!(token_count(9_500_000), "9.5M");
    }

    #[test]
    fn short_ids() {
        assert_eq!(short_id("sha256:8e6e9ec2c685ca1747a6"), "8e6e9ec2c685");
        assert_eq!(short_id("plain"), "plain");
    }
}
