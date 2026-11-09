//! Merge fbuild's size records and render the trusted, per-PR CI comment.
use crate::sizes::{BoardSizes, StageSizes};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf};

const MARKER: &str = "<!-- fstart-stage-sizes -->";
type Boards = BTreeMap<String, BTreeMap<String, StageSizes>>;

#[derive(Serialize, Deserialize)]
pub struct Report {
    pub commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_epoch",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_date_epoch: Option<u64>,
    #[serde(default)]
    pub boards: Boards,
}

// Old or malformed provenance must produce a non-comparable report, not a
// spurious diff or a failed workflow. Keep the actual representation typed.
fn deserialize_epoch<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_u64())
}

#[derive(Subcommand)]
pub enum Command {
    /// Combine per-board JSON lines into a CI artifact.
    Merge {
        lines: PathBuf,
        out: PathBuf,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        source_date_epoch: u64,
        #[arg(long)]
        pr: Option<u64>,
    },
    /// Compare two artifacts, printing Markdown for a pull request.
    Report { base: PathBuf, head: PathBuf },
}

impl Command {
    pub fn run(self) -> Result<(), String> {
        match self {
            Self::Merge {
                lines,
                out,
                commit,
                source_date_epoch,
                pr,
            } => {
                let lines = fs::read_to_string(lines).map_err(|e| e.to_string())?;
                let report = merge(&lines, commit, source_date_epoch, pr)?;
                fs::write(
                    out,
                    serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())
            }
            Self::Report { base, head } => {
                let read = |path| -> Result<Report, String> {
                    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())
                };
                print!("{}", render(&read(base)?, &read(head)?)?);
                Ok(())
            }
        }
    }
}

pub fn merge(lines: &str, commit: String, epoch: u64, pr: Option<u64>) -> Result<Report, String> {
    let mut boards = BTreeMap::new();
    for line in lines.lines().filter(|line| !line.trim().is_empty()) {
        let record: BoardSizes = serde_json::from_str(line).map_err(|e| e.to_string())?;
        boards.insert(record.board, record.stages);
    }
    Ok(Report {
        commit,
        pr,
        source_date_epoch: Some(epoch),
        boards,
    })
}

fn validate(report: &Report) -> Result<(), String> {
    if report.commit.len() != 40
        || !report
            .commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("unexpected commit".into());
    }
    for (board, stages) in &report.boards {
        for name in std::iter::once(board).chain(stages.keys()) {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return Err("unexpected board/stage name".into());
            }
        }
    }
    Ok(())
}

fn number(value: i128) -> String {
    let text = value.unsigned_abs().to_string();
    let mut out = String::new();
    if value < 0 {
        out.push('-');
    }
    for (i, c) in text.chars().enumerate() {
        if i > 0 && (text.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn signed(value: i128) -> String {
    format!(
        "{}{n}",
        if value >= 0 { "+" } else { "" },
        n = number(value)
    )
}

fn delta(new: u64, old: u64) -> String {
    let change = i128::from(new) - i128::from(old);
    if change == 0 {
        return "0".into();
    }
    let percent = if old == 0 {
        String::new()
    } else {
        format!(" ({:+.2}%)", change as f64 / old as f64 * 100.0)
    };
    format!("{}{percent}", signed(change))
}

pub fn render(base: &Report, head: &Report) -> Result<String, String> {
    validate(base)?;
    validate(head)?;
    let mut lines = vec![MARKER.into(), "### Stage sizes".into(), String::new()];
    lines.push(format!(
        "`{}` against base `{}`. *Image* is the loaded stage (`stage.bin`); the other columns are section bytes (bss without the heap and stack reservations).",
        &head.commit[..12], &base.commit[..12]
    ));
    lines.push(String::new());
    if base.source_date_epoch.is_none() || base.source_date_epoch != head.source_date_epoch {
        lines.push("Stage sizes are not comparable: the builds do not record the same `SOURCE_DATE_EPOCH`. A baseline built with the same fixed epoch is needed.".into());
        lines.push(String::new());
        return Ok(lines.join("\n"));
    }
    let flatten = |boards: &Boards| {
        boards
            .iter()
            .flat_map(|(b, stages)| {
                stages
                    .iter()
                    .map(move |(s, n)| ((b.clone(), s.clone()), n.clone()))
            })
            .collect::<BTreeMap<_, _>>()
    };
    let old = flatten(&base.boards);
    let new = flatten(&head.boards);
    let changed: Vec<_> = new
        .keys()
        .filter(|k| old.get(*k).is_some_and(|o| o != &new[*k]))
        .collect();
    let added: Vec<_> = new.keys().filter(|k| !old.contains_key(*k)).collect();
    let removed: Vec<_> = old.keys().filter(|k| !new.contains_key(*k)).collect();
    let total: i128 = changed
        .iter()
        .map(|k| i128::from(new[*k].flat) - i128::from(old[*k].flat))
        .sum();
    if changed.is_empty() && added.is_empty() && removed.is_empty() {
        lines.push("No stage size changed.".into());
    } else {
        let mut summary = format!(
            "{} stage(s) changed, images {} bytes in total",
            changed.len(),
            signed(total)
        );
        if !added.is_empty() {
            summary += &format!("; {} new", added.len());
        }
        if !removed.is_empty() {
            summary += &format!("; {} gone", removed.len());
        }
        lines.push(summary + ".");
    }
    if !changed.is_empty() {
        lines.extend([
            String::new(),
            "| Board | Stage | Image | Δ image | Δ text | Δ rodata | Δ data | Δ bss |".into(),
            "|---|---|---:|---:|---:|---:|---:|---:|".into(),
        ]);
        for (board, stage) in changed {
            let n = &new[&(board.clone(), stage.clone())];
            let o = &old[&(board.clone(), stage.clone())];
            lines.push(format!(
                "| {board} | {stage} | {} | {} | {} | {} | {} | {} |",
                number(n.flat.into()),
                delta(n.flat, o.flat),
                delta(n.text, o.text),
                delta(n.rodata, o.rodata),
                delta(n.data, o.data),
                delta(n.bss, o.bss)
            ));
        }
    }
    for (title, keys) in [("New", added), ("Gone", removed)] {
        if !keys.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "{title}: {}",
                keys.iter()
                    .map(|(b, s)| format!("{b}/{s}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    lines.extend([
        String::new(),
        "<details><summary>All stages</summary>".into(),
        String::new(),
        "| Board | Stage | Image | text | rodata | data | bss |".into(),
        "|---|---|---:|---:|---:|---:|---:|".into(),
    ]);
    for ((board, stage), n) in new {
        lines.push(format!(
            "| {board} | {stage} | {} | {} | {} | {} | {} |",
            number(n.flat.into()),
            number(n.text.into()),
            number(n.rodata.into()),
            number(n.data.into()),
            number(n.bss.into())
        ));
    }
    lines.extend([String::new(), "</details>".into(), String::new()]);
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(epoch: Option<u64>, flat: u64, text: u64) -> Report {
        serde_json::from_value(json!({"commit": "c".repeat(40), "source_date_epoch": epoch,
            "boards": {"q35": {"stage": {"flat": flat, "text": text, "rodata": 0, "data": 0, "bss": 0}}}})).unwrap()
    }

    #[test]
    fn merge_records_boards_epoch_and_pr() {
        let r = merge(
            "{\"board\":\"a\",\"stages\":{}}\n\n{\"board\":\"b\",\"stages\":{}}\n",
            "abc".into(),
            0,
            Some(7),
        )
        .unwrap();
        assert_eq!(r.commit, "abc");
        assert_eq!(r.source_date_epoch, Some(0));
        assert_eq!(r.pr, Some(7));
        assert_eq!(
            r.boards.keys().map(String::as_str).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(merge("not json", "abc".into(), 0, None).is_err());
    }

    #[test]
    fn reports_changes_new_and_gone_stages() {
        let mut base = report(Some(0), 5000, 4000);
        let mut head = report(Some(0), 5128, 4096);
        head.boards
            .get_mut("q35")
            .unwrap()
            .get_mut("stage")
            .unwrap()
            .rodata = 32;
        base.boards.insert("old".into(), base.boards["q35"].clone());
        head.boards.insert("new".into(), head.boards["q35"].clone());
        let text = render(&base, &head).unwrap();
        assert!(text.starts_with(MARKER));
        assert!(text.contains("1 stage(s) changed, images +128 bytes in total; 1 new; 1 gone."));
        assert!(
            text.contains("| q35 | stage | 5,128 | +128 (+2.56%) | +96 (+2.40%) | +32 | 0 | 0 |")
        );
        assert!(text.contains("New: new/stage"));
        assert!(text.contains("Gone: old/stage"));
    }

    #[test]
    fn only_matching_recorded_epochs_are_comparable() {
        for (a, b) in [
            (None, None),
            (None, Some(0)),
            (Some(0), None),
            (Some(0), Some(1)),
        ] {
            let text = render(&report(a, 1, 1), &report(b, 2, 2)).unwrap();
            assert!(text.contains("not comparable"));
            assert!(!text.contains("| Board |"));
            assert!(!text.contains("No stage size changed."));
        }
        for epoch in [0, 1735689600] {
            let r = report(Some(epoch), 1, 1);
            assert!(render(&r, &r).unwrap().contains("No stage size changed."));
        }
    }

    #[test]
    fn rejects_untrusted_names_and_commits_and_skips_malformed_epochs() {
        let good = report(Some(0), 1, 1);
        for name in ["x | <img>", "q35\n", "", &"a".repeat(65)] {
            let mut bad = report(Some(0), 1, 1);
            let stages = bad.boards.remove("q35").unwrap();
            bad.boards.insert(name.into(), stages);
            assert!(render(&good, &bad).is_err());
        }
        for commit in ["bad".to_string(), "c".repeat(39) + "`", "F".repeat(40)] {
            let mut bad = report(Some(0), 1, 1);
            bad.commit = commit;
            assert!(render(&good, &bad).is_err());
        }
        for epoch in [json!(-1), json!(false), json!("0"), json!(0.0)] {
            let mut value = serde_json::to_value(&good).unwrap();
            value["source_date_epoch"] = epoch;
            let bad: Report = serde_json::from_value(value).unwrap();
            assert!(render(&good, &bad).unwrap().contains("not comparable"));
        }
    }

    #[test]
    fn deltas_do_not_overflow_and_keep_zero_baselines() {
        assert_eq!(delta(0, u64::MAX), "-18,446,744,073,709,551,615 (-100.00%)");
        assert_eq!(delta(32, 0), "+32");
        assert_eq!(delta(0, 0), "0");
    }
}
