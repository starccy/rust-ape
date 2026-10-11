//! patches/audit.toml: the constant families and the allowed gaps.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::ConstRow;

/// One [[family]] of patches/audit.toml.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Family {
    prefixes: Vec<String>,
    /// prefixes that take names back out
    #[serde(default)]
    except: Vec<String>,
    #[serde(rename = "functions")]
    pub(super) consumers: Vec<String>,
}

/// patches/audit.toml
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditFile {
    allow: AllowFile,
    pub(super) family: Vec<Family>,
}

/// name = "why"; a trailing `*` on the name matches a prefix
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowFile {
    #[serde(rename = "const", default)]
    pub(super) consts: BTreeMap<String, String>,
    #[serde(rename = "fn", default)]
    pub(super) fns: BTreeMap<String, String>,
}

pub(super) fn load_audit_file(path: &Path) -> Result<(Allow, Vec<Family>)> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: AuditFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let patterns = |kind: &str, m: BTreeMap<String, String>| -> Result<Patterns> {
        if let Some((name, _)) = m.iter().find(|(_, why)| why.trim().is_empty()) {
            bail!("{}: allow.{kind}.{name} needs a reason", path.display());
        }
        Ok(Patterns(
            m.into_keys().map(|k| (k, Default::default())).collect(),
        ))
    };
    for f in &file.family {
        if f.prefixes.is_empty() || f.consumers.is_empty() {
            bail!("{}: a family needs a prefix and a function", path.display());
        }
    }
    let allow = Allow {
        consts: patterns("const", file.allow.consts)?,
        fns: patterns("fn", file.allow.fns)?,
    };
    Ok((allow, file.family))
}

pub(super) struct Patterns(Vec<(String, std::cell::Cell<bool>)>);

impl Patterns {
    /// Also remembers which entries ever matched, for `unused`.
    pub(super) fn matches(&self, name: &str) -> bool {
        let mut any = false;
        for (p, hit) in &self.0 {
            let m = match p.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => p == name,
            };
            if m {
                hit.set(true);
                any = true;
            }
        }
        any
    }

    pub(super) fn unused(&self) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(_, hit)| !hit.get())
            .map(|(p, _)| p.as_str())
            .collect()
    }
}

pub(super) struct Allow {
    pub(super) consts: Patterns,
    pub(super) fns: Patterns,
}

// ----------------------------------------------------------------- consumers

/// Every function the families a constant belongs to list, or None when no
/// family claims it.
pub(super) fn consumers_of<'a>(families: &'a [Family], name: &str) -> Option<Vec<&'a String>> {
    let mut out = Vec::new();
    let mut claimed = false;
    for f in families {
        if in_family(f, name) {
            claimed = true;
            out.extend(&f.consumers);
        }
    }
    claimed.then_some(out)
}

pub(super) fn in_family(f: &Family, name: &str) -> bool {
    f.prefixes.iter().any(|p| name.starts_with(p.as_str()))
        && !f.except.iter().any(|p| name.starts_with(p.as_str()))
}

/// Families with at least one differing value, labelled by the names that
/// differ, with their consumer functions.
pub(super) fn hot_families<'a>(
    families: &'a [Family],
    rows: &[ConstRow],
) -> Vec<(String, &'a [String])> {
    let mut out = Vec::new();
    for family in families {
        let consumers = &family.consumers;
        let differing: Vec<&str> = rows
            .iter()
            .filter(|r| !r.diffs.is_empty())
            .filter(|r| in_family(family, &r.name))
            .map(|r| r.name.as_str())
            .collect();
        if differing.is_empty() {
            continue;
        }
        let mut label = differing
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join(",");
        if differing.len() > 3 {
            let _ = write!(label, ",+{}", differing.len() - 3);
        }
        out.push((label, consumers.as_slice()));
    }
    out
}
