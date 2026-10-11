//! The full tables written under target/audit.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::Result;

use super::{ConstRow, FnRow};

pub(super) fn write_reports(
    work: &Path,
    rows: &[ConstRow],
    fn_rows: &[FnRow],
    imports: &BTreeMap<String, BTreeSet<String>>,
) -> Result<()> {
    let mut t = String::from("name\tverdict\tkind\tcosmo\tdiffers\tused by\n");
    for r in rows {
        let _ = writeln!(
            t,
            "{}\t{}\t{}\t{}\t{}\t{}",
            r.name,
            r.verdict,
            r.kind,
            r.cosmo_cell.trim(),
            r.diffs.join("; "),
            r.users
        );
    }
    fs::write(work.join("consts.tsv"), t)?;

    let flagged: HashMap<&str, &FnRow> = fn_rows.iter().map(|r| (r.name.as_str(), r)).collect();
    let mut t = String::from("function\tstatus\tfamilies\timported by\n");
    for (func, crates) in imports {
        let crates = crates.iter().cloned().collect::<Vec<_>>().join(" ");
        let (status, fam) = if func.starts_with("__ape_shim_") {
            ("shim", "")
        } else if let Some(r) = flagged.get(func.as_str()) {
            (r.verdict, r.families.as_str())
        } else {
            ("direct", "")
        };
        let _ = writeln!(t, "{func}\t{status}\t{fam}\t{crates}");
    }
    for r in fn_rows.iter().filter(|r| r.verdict.starts_with("unused")) {
        let _ = writeln!(t, "{}\t{}\t{}\t", r.name, r.verdict, r.families);
    }
    fs::write(work.join("functions.tsv"), t)?;
    Ok(())
}
