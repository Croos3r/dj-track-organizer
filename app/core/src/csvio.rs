// SPDX-License-Identifier: GPL-3.0-only
//! CSV artifacts shared with the Python CLI tools: `rename_plan.csv`,
//! `rename_rollback.csv` and `duplicates.csv`. Byte-compatible with the
//! Python `csv` module output on Windows: UTF-8 BOM, CRLF line endings,
//! minimal quoting.

use std::io::Write;
use std::path::Path;

use crate::normalize::PlanRow;

const BOM: &[u8] = b"\xef\xbb\xbf";

fn writer_to(path: &Path) -> std::io::Result<csv::Writer<std::fs::File>> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(BOM)?;
    Ok(csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .quote_style(csv::QuoteStyle::Necessary)
        .from_writer(file))
}

/// Python normalizer's plan CSV: only rows that actually change, header
/// `OLD name,NEW name,from`.
pub fn write_plan_csv(path: &Path, rows: &[PlanRow]) -> std::io::Result<()> {
    let mut w = writer_to(path)?;
    w.write_record(["OLD name", "NEW name", "from"])?;
    for r in rows {
        if !r.new.is_empty() && r.new != r.old {
            w.write_record([&r.old, &r.new, &r.origin])?;
        }
    }
    w.flush()?;
    Ok(())
}

/// Python normalizer's rollback CSV: `current_name,restore_to` (new -> old).
pub fn write_rollback_csv(path: &Path, done: &[(String, String)]) -> std::io::Result<()> {
    let mut w = writer_to(path)?;
    w.write_record(["current_name", "restore_to"])?;
    for (old, new) in done {
        w.write_record([new, old])?;
    }
    w.flush()?;
    Ok(())
}

/// Load a rename mapping {old -> new} from either a plan CSV or a rollback
/// CSV, mirroring `rekordbox_sync.load_mapping` (which accepts both).
pub fn load_mapping(path: &Path) -> std::io::Result<Vec<(String, String)>> {
    let bytes = std::fs::read(path)?;
    let bytes = bytes.strip_prefix(BOM).unwrap_or(&bytes);
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(bytes);
    let headers: Vec<String> = rdr
        .headers()?
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();
    let find = |name: &str| headers.iter().position(|h| h == name);
    let mut mapping = Vec::new();
    if let (Some(oi), Some(ni)) = (find("old name"), find("new name")) {
        for rec in rdr.records() {
            let rec = rec?;
            if rec.len() > oi.max(ni) {
                mapping.push((rec[oi].trim().to_string(), rec[ni].trim().to_string()));
            }
        }
    } else if let (Some(ci), Some(ri)) = (find("current_name"), find("restore_to")) {
        for rec in rdr.records() {
            let rec = rec?;
            if rec.len() > ci.max(ri) {
                // rollback rows are (new, old): mapping is old -> new
                mapping.push((rec[ri].trim().to_string(), rec[ci].trim().to_string()));
            }
        }
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Unrecognised mapping CSV header: {headers:?}"),
        ));
    }
    Ok(mapping)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_rows() -> Vec<PlanRow> {
        [
            (
                "Insomniak, Drymk - 1312 (Drymk Remix).wav",
                "Drymk, Insomniak - 1312 (Drymk Remix).wav",
                "filename",
            ),
            (
                "01 Some Artist - Some Title.mp3",
                "Some Artist - Some Title.mp3",
                "filename",
            ),
            ("Artist - Ti\"tle.mp3", "Artist - Title.mp3", "tags"),
            (
                "Sétaou - Prière Païenne.mp3",
                "Sétaou - Prière Païenne (Über Edit).mp3",
                "mixed",
            ),
        ]
        .into_iter()
        .map(|(o, n, s)| PlanRow {
            old: o.into(),
            new: n.into(),
            origin: s.into(),
        })
        .collect()
    }

    #[test]
    fn plan_csv_bytes_match_python() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("plan.csv");
        write_plan_csv(&p, &plan_rows()).unwrap();
        let got = std::fs::read(&p).unwrap();
        let want = include_bytes!("../tests/fixtures/csv_plan_expected.csv");
        assert_eq!(got, want, "plan CSV bytes differ from Python oracle");
    }

    #[test]
    fn rollback_csv_bytes_match_python() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("rollback.csv");
        let done: Vec<(String, String)> = plan_rows().into_iter().map(|r| (r.old, r.new)).collect();
        write_rollback_csv(&p, &done).unwrap();
        let got = std::fs::read(&p).unwrap();
        let want = include_bytes!("../tests/fixtures/csv_rollback_expected.csv");
        assert_eq!(got, want, "rollback CSV bytes differ from Python oracle");
    }

    #[test]
    fn mapping_roundtrips_from_both_csv_kinds() {
        let td = tempfile::tempdir().unwrap();
        let rows = plan_rows();
        let plan = td.path().join("plan.csv");
        let rb = td.path().join("rollback.csv");
        write_plan_csv(&plan, &rows).unwrap();
        let done: Vec<(String, String)> = rows
            .iter()
            .map(|r| (r.old.clone(), r.new.clone()))
            .collect();
        write_rollback_csv(&rb, &done).unwrap();
        let want: Vec<(String, String)> = done;
        assert_eq!(load_mapping(&plan).unwrap(), want);
        assert_eq!(load_mapping(&rb).unwrap(), want);
    }
}
